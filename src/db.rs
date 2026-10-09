//! Banco de dados: catálogo + executor de comandos sobre o heap.
//!
//! O catálogo é uma tabela comum guardada na página 1 do arquivo, com linhas
//! `(nome TEXT, raiz INT, esquema TEXT)`.

use crate::ast::{BinOp, Expr, SelectItem, Statement};
use crate::btree;
use crate::error::{Error, Result};
use crate::eval::{eval, matches};
use crate::heap::{self, RowId};
use crate::pager::{PageId, Pager};
use crate::parser::parse;
use crate::schema::{Column, Schema};
use crate::value::{decode_row, encode_row, DataType, Row, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;

const CATALOG_ROOT: PageId = 1;

#[derive(Debug, Clone)]
pub struct TableMeta {
    pub name: String,
    pub root: PageId,
    /// raiz da B+tree da PRIMARY KEY (0 = tabela sem índice)
    pub index_root: PageId,
    pub schema: Schema,
}

#[derive(Debug)]
pub enum QueryResult {
    Rows { columns: Vec<String>, rows: Vec<Row> },
    Affected(usize),
    Done(String),
}

pub struct Database {
    pager: Pager,
    /// nome → (linha no catálogo, metadados)
    tables: HashMap<String, (RowId, TableMeta)>,
}

impl Database {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut pager = Pager::open(path)?;
        if pager.num_pages() == 1 {
            let id = heap::create(&mut pager)?;
            debug_assert_eq!(id, CATALOG_ROOT);
        }
        let mut tables = HashMap::new();
        for (rid, bytes) in heap::scan(&mut pager, CATALOG_ROOT)? {
            let bad = || Error::storage("linha de catálogo inválida");
            let row = decode_row(&bytes)?;
            let [Value::Text(name), Value::Int(root), Value::Text(schema), Value::Int(index_root)] =
                &row[..]
            else {
                return Err(Error::storage(
                    "catálogo em formato antigo ou inválido; apague o arquivo .db",
                ));
            };
            let meta = TableMeta {
                name: name.clone(),
                root: PageId::try_from(*root).map_err(|_| bad())?,
                index_root: PageId::try_from(*index_root).map_err(|_| bad())?,
                schema: Schema::deserialize(schema)?,
            };
            tables.insert(name.clone(), (rid, meta));
        }
        Ok(Database { pager, tables })
    }

    /// Executa um ou mais comandos SQL separados por `;`.
    pub fn run(&mut self, sql: &str) -> Result<Vec<QueryResult>> {
        parse(sql)?.iter().map(|s| self.execute(s)).collect()
    }

    pub fn table_names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.tables.keys().cloned().collect();
        names.sort();
        names
    }

    pub fn table_meta(&self, name: &str) -> Option<&TableMeta> {
        self.tables.get(name).map(|(_, m)| m)
    }

    pub fn execute(&mut self, stmt: &Statement) -> Result<QueryResult> {
        match stmt {
            Statement::CreateTable { name, columns } => self.create_table(name, columns),
            Statement::DropTable { name } => self.drop_table(name),
            Statement::Insert { table, columns, rows } => self.insert(table, columns, rows),
            Statement::Select { table, items, filter, order_by, limit } => {
                self.select(table, items, filter, order_by, *limit)
            }
            Statement::Update { table, sets, filter } => self.update(table, sets, filter),
            Statement::Delete { table, filter } => self.delete(table, filter),
        }
    }

    // ---- DDL ------------------------------------------------------------

    fn create_table(&mut self, name: &str, columns: &[Column]) -> Result<QueryResult> {
        if self.tables.contains_key(name) {
            return Err(Error::exec(format!("tabela '{name}' já existe")));
        }
        if columns.is_empty() {
            return Err(Error::exec("tabela sem colunas"));
        }
        let mut seen = HashSet::new();
        for c in columns {
            if !seen.insert(&c.name) {
                return Err(Error::exec(format!("coluna '{}' duplicada", c.name)));
            }
        }
        if columns.iter().filter(|c| c.primary_key).count() > 1 {
            return Err(Error::exec("só uma PRIMARY KEY é suportada"));
        }
        let schema = Schema { columns: columns.to_vec() };
        let root = heap::create(&mut self.pager)?;
        let index_root = match schema.primary_key() {
            Some(_) => btree::create(&mut self.pager)?,
            None => 0,
        };
        let entry = [
            Value::Text(name.to_string()),
            Value::Int(root as i64),
            Value::Text(schema.serialize()),
            Value::Int(index_root as i64),
        ];
        let rid = heap::insert(&mut self.pager, CATALOG_ROOT, &encode_row(&entry))?;
        let meta = TableMeta { name: name.to_string(), root, index_root, schema };
        self.tables.insert(name.to_string(), (rid, meta));
        Ok(QueryResult::Done(format!("tabela '{name}' criada")))
    }

    fn drop_table(&mut self, name: &str) -> Result<QueryResult> {
        let (rid, _) = self
            .tables
            .remove(name)
            .ok_or_else(|| Error::exec(format!("tabela '{name}' não existe")))?;
        // as páginas da tabela ficam órfãs: ainda não há lista de páginas livres
        heap::delete(&mut self.pager, rid)?;
        Ok(QueryResult::Done(format!("tabela '{name}' removida")))
    }

    // ---- DML ------------------------------------------------------------

    fn table(&self, name: &str) -> Result<TableMeta> {
        self.tables
            .get(name)
            .map(|(_, m)| m.clone())
            .ok_or_else(|| Error::exec(format!("tabela '{name}' não existe")))
    }

    fn load(&mut self, meta: &TableMeta) -> Result<Vec<(RowId, Row)>> {
        heap::scan(&mut self.pager, meta.root)?
            .into_iter()
            .map(|(rid, bytes)| Ok((rid, decode_row(&bytes)?)))
            .collect()
    }

    /// Linhas que podem satisfazer o filtro: um acesso pelo índice quando há
    /// `pk = literal` (possivelmente dentro de ANDs); senão, varredura completa.
    /// O chamador ainda reaplica o filtro inteiro.
    fn candidates(&mut self, meta: &TableMeta, filter: &Option<Expr>) -> Result<Vec<(RowId, Row)>> {
        let Some(key) = pk_lookup(filter, &meta.schema) else {
            return self.load(meta);
        };
        let Some(rid) = btree::search(&mut self.pager, meta.index_root, &key)? else {
            return Ok(Vec::new());
        };
        match heap::get(&mut self.pager, rid)? {
            Some(bytes) => Ok(vec![(rid, decode_row(&bytes)?)]),
            None => Err(Error::storage("índice aponta para um registro apagado")),
        }
    }

    fn insert(
        &mut self,
        table: &str,
        columns: &Option<Vec<String>>,
        rows: &[Vec<Expr>],
    ) -> Result<QueryResult> {
        let meta = self.table(table)?;
        let schema = &meta.schema;
        let n = schema.columns.len();

        let positions: Vec<usize> = match columns {
            None => (0..n).collect(),
            Some(names) => {
                let mut pos = Vec::new();
                for name in names {
                    let i = schema
                        .index(name)
                        .ok_or_else(|| Error::exec(format!("coluna desconhecida '{name}'")))?;
                    if pos.contains(&i) {
                        return Err(Error::exec(format!("coluna '{name}' repetida")));
                    }
                    pos.push(i);
                }
                pos
            }
        };

        let empty = Schema::default();
        let mut new_rows: Vec<Row> = Vec::new();
        for exprs in rows {
            if exprs.len() != positions.len() {
                return Err(Error::exec(format!(
                    "esperava {} valores, recebi {}",
                    positions.len(),
                    exprs.len()
                )));
            }
            let mut row = vec![Value::Null; n];
            for (&pos, e) in positions.iter().zip(exprs) {
                row[pos] = eval(e, &empty, &[])?;
            }
            for (i, col) in schema.columns.iter().enumerate() {
                row[i] = coerce(std::mem::replace(&mut row[i], Value::Null), col)?;
            }
            new_rows.push(row);
        }

        let pk = schema.primary_key();
        if let Some(pk) = pk {
            let mut seen = HashSet::new();
            for r in &new_rows {
                let dup = !seen.insert(key_of(r, pk))
                    || btree::search(&mut self.pager, meta.index_root, &r[pk])?.is_some();
                if dup {
                    return Err(Error::exec(format!("chave primária duplicada: {}", r[pk])));
                }
            }
        }

        for r in &new_rows {
            let rid = heap::insert(&mut self.pager, meta.root, &encode_row(r))?;
            if let Some(pk) = pk {
                btree::insert(&mut self.pager, meta.index_root, &r[pk], rid)?;
            }
        }
        Ok(QueryResult::Affected(new_rows.len()))
    }

    fn select(
        &mut self,
        table: &str,
        items: &[SelectItem],
        filter: &Option<Expr>,
        order_by: &Option<(String, bool)>,
        limit: Option<usize>,
    ) -> Result<QueryResult> {
        let meta = self.table(table)?;
        let schema = &meta.schema;

        let mut rows = Vec::new();
        for (_, row) in self.candidates(&meta, filter)? {
            if matches(filter, schema, &row)? {
                rows.push(row);
            }
        }

        if let Some((col, desc)) = order_by {
            let i = schema
                .index(col)
                .ok_or_else(|| Error::exec(format!("coluna desconhecida '{col}'")))?;
            rows.sort_by(|a, b| {
                let o = a[i].total_cmp(&b[i]);
                if *desc { o.reverse() } else { o }
            });
        }
        if let Some(n) = limit {
            rows.truncate(n);
        }

        let mut columns = Vec::new();
        for item in items {
            match item {
                SelectItem::Star => columns.extend(schema.columns.iter().map(|c| c.name.clone())),
                SelectItem::Expr(Expr::Col(name)) => columns.push(name.clone()),
                SelectItem::Expr(_) => columns.push("expr".to_string()),
            }
        }

        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut o = Vec::new();
            for item in items {
                match item {
                    SelectItem::Star => o.extend(row.iter().cloned()),
                    SelectItem::Expr(e) => o.push(eval(e, schema, row)?),
                }
            }
            out.push(o);
        }
        Ok(QueryResult::Rows { columns, rows: out })
    }

    fn update(
        &mut self,
        table: &str,
        sets: &[(String, Expr)],
        filter: &Option<Expr>,
    ) -> Result<QueryResult> {
        let meta = self.table(table)?;
        let schema = &meta.schema;

        let mut targets = Vec::new();
        for (name, e) in sets {
            let i = schema
                .index(name)
                .ok_or_else(|| Error::exec(format!("coluna desconhecida '{name}'")))?;
            targets.push((i, e));
        }

        let mut changes: Vec<(RowId, Row, Row)> = Vec::new(); // (rid, antiga, nova)
        for (rid, row) in self.candidates(&meta, filter)? {
            if matches(filter, schema, &row)? {
                let mut new = row.clone();
                for &(i, e) in &targets {
                    new[i] = coerce(eval(e, schema, &row)?, &schema.columns[i])?;
                }
                changes.push((rid, row, new));
            }
        }

        let pk = schema.primary_key();
        if let Some(pk) = pk {
            let changed: HashSet<RowId> = changes.iter().map(|(r, _, _)| *r).collect();
            let mut seen = HashSet::new();
            for (_, _, new) in &changes {
                let clash = match btree::search(&mut self.pager, meta.index_root, &new[pk])? {
                    Some(existing) => !changed.contains(&existing),
                    None => false,
                };
                if clash || !seen.insert(key_of(new, pk)) {
                    return Err(Error::exec(format!("chave primária duplicada: {}", new[pk])));
                }
            }
        }

        // remove todas as versões antigas antes de inserir as novas (permite trocar chaves)
        for (rid, old, _) in &changes {
            heap::delete(&mut self.pager, *rid)?;
            if let Some(pk) = pk {
                btree::delete(&mut self.pager, meta.index_root, &old[pk])?;
            }
        }
        for (_, _, new) in &changes {
            let rid = heap::insert(&mut self.pager, meta.root, &encode_row(new))?;
            if let Some(pk) = pk {
                btree::insert(&mut self.pager, meta.index_root, &new[pk], rid)?;
            }
        }
        Ok(QueryResult::Affected(changes.len()))
    }

    fn delete(&mut self, table: &str, filter: &Option<Expr>) -> Result<QueryResult> {
        let meta = self.table(table)?;
        let pk = meta.schema.primary_key();
        let mut count = 0;
        for (rid, row) in self.candidates(&meta, filter)? {
            if matches(filter, &meta.schema, &row)? {
                heap::delete(&mut self.pager, rid)?;
                if let Some(pk) = pk {
                    btree::delete(&mut self.pager, meta.index_root, &row[pk])?;
                }
                count += 1;
            }
        }
        Ok(QueryResult::Affected(count))
    }
}

/// Extrai `pk = literal` de um filtro (também dentro de conjunções AND).
fn pk_lookup(filter: &Option<Expr>, schema: &Schema) -> Option<Value> {
    fn find(e: &Expr, name: &str) -> Option<Value> {
        match e {
            Expr::Binary { op: BinOp::And, left, right } => {
                find(left, name).or_else(|| find(right, name))
            }
            Expr::Binary { op: BinOp::Eq, left, right } => match (&**left, &**right) {
                (Expr::Col(c), Expr::Lit(v)) | (Expr::Lit(v), Expr::Col(c)) if c == name => {
                    Some(v.clone())
                }
                _ => None,
            },
            _ => None,
        }
    }
    let col = &schema.columns[schema.primary_key()?];
    coerce(find(filter.as_ref()?, &col.name)?, col).ok()
}

fn key_of(row: &Row, pk: usize) -> Vec<u8> {
    encode_row(&row[pk..=pk])
}

/// Valida o valor contra a coluna (NOT NULL, tipo) e promove INT → FLOAT.
fn coerce(v: Value, col: &Column) -> Result<Value> {
    match (v, col.ty) {
        (Value::Null, _) if col.not_null => {
            Err(Error::exec(format!("coluna '{}' não aceita NULL", col.name)))
        }
        (Value::Null, _) => Ok(Value::Null),
        (v @ Value::Int(_), DataType::Int)
        | (v @ Value::Float(_), DataType::Float)
        | (v @ Value::Text(_), DataType::Text)
        | (v @ Value::Bool(_), DataType::Bool) => Ok(v),
        (Value::Int(i), DataType::Float) => Ok(Value::Float(i as f64)),
        (v, ty) => Err(Error::exec(format!(
            "coluna '{}' é {}, mas recebeu {v:?}",
            col.name,
            ty.name()
        ))),
    }
}
