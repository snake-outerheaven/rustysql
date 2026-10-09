//! Planejador: `SelectStmt` → árvore de operadores.
//!
//! Decisões que ele toma:
//! * **caminho de acesso** por tabela: varredura completa ou índice (igualdade em
//!   índice único > igualdade > intervalo dos dois lados > intervalo de um lado);
//! * **pushdown** de predicados do WHERE (e de ON) para o scan da tabela que
//!   referenciam, exceto para o lado direito de um LEFT JOIN, onde mudaria o resultado;
//! * **algoritmo de join**: *index nested loop* se a coluna da direita tem índice,
//!   senão *hash join* para igualdades, senão *nested loop*;
//! * **agregação**: as chamadas `SUM(x)` etc. são extraídas para colunas sintéticas
//!   `#aggN` e o resto da consulta passa a referenciá-las.

use crate::ast::*;
use crate::catalog::{IndexInfo, TableInfo};
use crate::error::{Error, Result};
use crate::eval::{const_value, Scope, ScopeCol};
use crate::keyenc::{prefix_end, value_key, AFTER_NULL};
use crate::value::{DataType, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// Uma agregação extraída da consulta. O resultado dela vira a coluna sintética `name`
/// (`#agg0`, `#agg1`...), que o resto da consulta passa a referenciar.
pub struct AggSpec {
    /// A função.
    pub func: AggFunc,
    /// Argumento; `None` em `COUNT(*)`.
    pub arg: Option<Expr>,
    /// Nome da coluna sintética que recebe o resultado.
    pub name: String,
}

/// Um nó do plano de execução, junto com o escopo das colunas que ele produz.
pub struct Plan {
    /// O operador.
    pub node: Node,
    /// Colunas da linha produzida por este nó, em ordem.
    pub scope: Scope,
}

/// Operadores físicos do plano.
pub enum Node {
    /// Varredura completa do heap.
    SeqScan {
        /// Tabela lida.
        table: Arc<TableInfo>,
        /// Alias pelo qual as colunas são qualificadas.
        alias: String,
    },
    /// Varredura por intervalo de um índice, buscando cada linha no heap pelo `RowId`.
    IndexScan {
        /// Tabela lida.
        table: Arc<TableInfo>,
        /// Alias pelo qual as colunas são qualificadas.
        alias: String,
        /// Índice usado.
        index: Arc<IndexInfo>,
        /// Menor chave codificada (inclusive).
        start: Vec<u8>,
        /// Chave codificada onde para (exclusive); `None` = até o fim.
        end: Option<Vec<u8>>,
        /// Descrição legível das restrições, para o `EXPLAIN`.
        cond: String,
    },
    /// Descarta as linhas em que o predicado não é exatamente `TRUE`.
    Filter {
        /// Operador filho.
        input: Box<Plan>,
        /// Predicado.
        pred: Expr,
    },
    /// Junção por laço aninhado (a direita é materializada). Serve para qualquer condição.
    NestedLoop {
        /// Lado externo, lido em fluxo.
        left: Box<Plan>,
        /// Lado interno, materializado.
        right: Box<Plan>,
        /// Tipo de junção.
        kind: JoinKind,
        /// Condição; `None` = produto cartesiano.
        on: Option<Expr>,
    },
    /// Junção por hash numa igualdade `lkey = rkey`: constrói uma tabela hash com o lado
    /// direito e sonda com cada linha da esquerda.
    HashJoin {
        /// Lado de sondagem, lido em fluxo.
        left: Box<Plan>,
        /// Lado de construção, materializado numa tabela hash.
        right: Box<Plan>,
        /// Tipo de junção.
        kind: JoinKind,
        /// Chave calculada sobre as linhas da esquerda.
        lkey: Expr,
        /// Chave calculada sobre as linhas da direita.
        rkey: Expr,
        /// Outros predicados do `ON`, testados sobre a linha combinada.
        residual: Option<Expr>,
    },
    /// Junção por índice (*index nested loop*): para cada linha da esquerda, busca as da
    /// direita pelo índice, sem varrer a tabela.
    IndexJoin {
        /// Lado externo, lido em fluxo.
        left: Box<Plan>,
        /// Tabela sondada.
        table: Arc<TableInfo>,
        /// Alias da tabela sondada.
        alias: String,
        /// Índice usado na sondagem.
        index: Arc<IndexInfo>,
        /// Tipo de junção.
        kind: JoinKind,
        /// Expressão da esquerda cujo valor é procurado no índice.
        lkey: Expr,
        /// Outros predicados do `ON`, testados sobre a linha combinada.
        residual: Option<Expr>,
        /// Predicados só da tabela da direita, aplicados às linhas encontradas.
        right_filter: Option<Expr>,
    },
    /// Agrupamento por hash. A saída é a primeira linha de cada grupo seguida dos valores
    /// agregados.
    Aggregate {
        /// Operador filho.
        input: Box<Plan>,
        /// Expressões de agrupamento (vazio = um único grupo).
        group_by: Vec<Expr>,
        /// Agregações a calcular.
        aggs: Vec<AggSpec>,
    },
    /// Ordenação estável (materializa a entrada).
    Sort {
        /// Operador filho.
        input: Box<Plan>,
        /// Chaves como `(expressão, descendente?)`.
        keys: Vec<(Expr, bool)>,
    },
    /// Calcula as expressões da lista de projeção.
    Project {
        /// Operador filho.
        input: Box<Plan>,
        /// Expressões e nomes das colunas de saída.
        exprs: Vec<(Expr, String)>,
    },
    /// `LIMIT` e `OFFSET`.
    Limit {
        /// Operador filho.
        input: Box<Plan>,
        /// Máximo de linhas; `None` = sem limite.
        limit: Option<usize>,
        /// Linhas puladas antes de começar.
        offset: usize,
    },
}

impl Plan {
    /// Árvore do plano em texto, uma linha por operador e com indentação por nível. É o que o
    /// `EXPLAIN` mostra.
    pub fn explain(&self) -> String {
        let mut out = String::new();
        self.describe(0, &mut out);
        out.trim_end().to_string()
    }

    fn describe(&self, depth: usize, out: &mut String) {
        let pad = "  ".repeat(depth);
        let kind_name = |k: &JoinKind| if *k == JoinKind::Left { "Left" } else { "Inner" };
        let opt = |e: &Option<Expr>| e.as_ref().map(|e| format!(" residual {e}")).unwrap_or_default();
        let children: Vec<&Plan> = match &self.node {
            Node::SeqScan { table, alias } => {
                out.push_str(&format!("{pad}SeqScan {}{}\n", table.name, as_alias(&table.name, alias)));
                vec![]
            }
            Node::IndexScan { table, alias, index, cond, .. } => {
                out.push_str(&format!(
                    "{pad}IndexScan {}{} using {} ({cond})\n",
                    table.name,
                    as_alias(&table.name, alias),
                    index.name
                ));
                vec![]
            }
            Node::Filter { input, pred } => {
                out.push_str(&format!("{pad}Filter {pred}\n"));
                vec![input]
            }
            Node::NestedLoop { left, right, kind, on } => {
                let on = on.as_ref().map(|e| format!(" on {e}")).unwrap_or_default();
                out.push_str(&format!("{pad}NestedLoopJoin {}{on}\n", kind_name(kind)));
                vec![left, right]
            }
            Node::HashJoin { left, right, kind, lkey, rkey, residual } => {
                out.push_str(&format!(
                    "{pad}HashJoin {} on {lkey} = {rkey}{}\n",
                    kind_name(kind),
                    opt(residual)
                ));
                vec![left, right]
            }
            Node::IndexJoin { left, table, alias, index, kind, lkey, residual, right_filter } => {
                let rf = right_filter.as_ref().map(|e| format!(" filter {e}")).unwrap_or_default();
                out.push_str(&format!(
                    "{pad}IndexNestedLoopJoin {} probe {}{} using {} with {lkey}{rf}{}\n",
                    kind_name(kind),
                    table.name,
                    as_alias(&table.name, alias),
                    index.name,
                    opt(residual)
                ));
                vec![left]
            }
            Node::Aggregate { input, group_by, aggs } => {
                let g: Vec<String> = group_by.iter().map(|e| e.to_string()).collect();
                let a: Vec<String> = aggs
                    .iter()
                    .map(|s| match &s.arg {
                        Some(e) => format!("{}({e})", s.func),
                        None => format!("{}(*)", s.func),
                    })
                    .collect();
                out.push_str(&format!("{pad}Aggregate group by [{}] compute [{}]\n", g.join(", "), a.join(", ")));
                vec![input]
            }
            Node::Sort { input, keys } => {
                let k: Vec<String> = keys
                    .iter()
                    .map(|(e, d)| format!("{e}{}", if *d { " DESC" } else { "" }))
                    .collect();
                out.push_str(&format!("{pad}Sort [{}]\n", k.join(", ")));
                vec![input]
            }
            Node::Project { input, exprs } => {
                let e: Vec<String> = exprs.iter().map(|(e, _)| e.to_string()).collect();
                out.push_str(&format!("{pad}Project [{}]\n", e.join(", ")));
                vec![input]
            }
            Node::Limit { input, limit, offset } => {
                out.push_str(&format!("{pad}Limit {limit:?} offset {offset}\n"));
                vec![input]
            }
        };
        for c in children {
            c.describe(depth + 1, out);
        }
    }
}

fn as_alias(table: &str, alias: &str) -> String {
    if table == alias { String::new() } else { format!(" AS {alias}") }
}

// ---- caminho de acesso ------------------------------------------------

#[derive(Default)]
struct ColConstraint {
    eq: Option<Value>,
    lo: Option<(Value, bool)>, // (valor, inclusivo)
    hi: Option<(Value, bool)>,
}

fn coerce_literal(v: Value, ty: DataType) -> Option<Value> {
    match (v, ty) {
        (v @ Value::Int(_), DataType::Int)
        | (v @ Value::Float(_), DataType::Float)
        | (v @ Value::Text(_), DataType::Text)
        | (v @ Value::Bool(_), DataType::Bool) => Some(v),
        (Value::Int(i), DataType::Float) => Some(Value::Float(i as f64)),
        _ => None,
    }
}

fn flip(op: BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Gt,
        BinOp::Le => BinOp::Ge,
        BinOp::Gt => BinOp::Lt,
        BinOp::Ge => BinOp::Le,
        other => other,
    }
}

/// Restrições `coluna <op> constante` encontradas nas conjunções.
fn constraints(table: &TableInfo, alias: &str, conjs: &[Expr]) -> HashMap<usize, ColConstraint> {
    let mut out: HashMap<usize, ColConstraint> = HashMap::new();
    for c in conjs {
        let Expr::Binary { op, left, right } = c else { continue };
        let (col, value, op) = match (&**left, &**right) {
            (col @ Expr::Col { .. }, other) if const_value(other).is_some() => {
                (col, const_value(other).unwrap(), *op)
            }
            (other, col @ Expr::Col { .. }) if const_value(other).is_some() => {
                (col, const_value(other).unwrap(), flip(*op))
            }
            _ => continue,
        };
        let Expr::Col { table: qualifier, name } = col else { continue };
        if qualifier.as_deref().is_some_and(|q| q != alias) {
            continue;
        }
        let Some(idx) = table.schema.index(name) else { continue };
        let Some(value) = coerce_literal(value, table.schema.columns[idx].ty) else { continue };
        let slot = out.entry(idx).or_default();
        match op {
            BinOp::Eq => slot.eq = slot.eq.take().or(Some(value)),
            BinOp::Lt => slot.hi = slot.hi.take().or(Some((value, false))),
            BinOp::Le => slot.hi = slot.hi.take().or(Some((value, true))),
            BinOp::Gt => slot.lo = slot.lo.take().or(Some((value, false))),
            BinOp::Ge => slot.lo = slot.lo.take().or(Some((value, true))),
            _ => {}
        }
    }
    out
}

fn base_scan(table: &Arc<TableInfo>, alias: &str, conjs: &[Expr]) -> Plan {
    let scope = Scope::for_table(alias, &table.schema);
    let cons = constraints(table, alias, conjs);

    let mut best: Option<(i32, Arc<IndexInfo>)> = None;
    for index in table.indexes() {
        let Some(c) = cons.get(&index.column) else { continue };
        let score = if c.eq.is_some() {
            if index.unique { 100 } else { 80 }
        } else if c.lo.is_some() && c.hi.is_some() {
            60
        } else if c.lo.is_some() || c.hi.is_some() {
            40
        } else {
            continue;
        };
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, index));
        }
    }

    let node = match best {
        None => Node::SeqScan { table: table.clone(), alias: alias.to_string() },
        Some((_, index)) => {
            let c = &cons[&index.column];
            let col = &table.schema.columns[index.column].name;
            let (start, end, cond);
            if let Some(v) = &c.eq {
                let k = value_key(v);
                end = Some(prefix_end(&k));
                start = k;
                cond = format!("{col} = {v}");
            } else {
                start = match &c.lo {
                    Some((v, true)) => value_key(v),
                    Some((v, false)) => prefix_end(&value_key(v)),
                    None => AFTER_NULL.to_vec(),
                };
                end = match &c.hi {
                    Some((v, true)) => Some(prefix_end(&value_key(v))),
                    Some((v, false)) => Some(value_key(v)),
                    None => None,
                };
                let mut parts = Vec::new();
                if let Some((v, inc)) = &c.lo {
                    parts.push(format!("{col} {} {v}", if *inc { ">=" } else { ">" }));
                }
                if let Some((v, inc)) = &c.hi {
                    parts.push(format!("{col} {} {v}", if *inc { "<=" } else { "<" }));
                }
                cond = parts.join(" AND ");
            }
            Node::IndexScan { table: table.clone(), alias: alias.to_string(), index, start, end, cond }
        }
    };
    Plan { node, scope }
}

/// Scan com os predicados já empurrados para baixo (reaplicados como Filter: o índice
/// só estreita o conjunto, nunca decide sozinho).
fn scan_plan(table: &Arc<TableInfo>, alias: &str, conjs: &[Expr]) -> Plan {
    let scan = base_scan(table, alias, conjs);
    match Expr::and_all(conjs.to_vec()) {
        None => scan,
        Some(pred) => {
            let scope = scan.scope.clone();
            Plan { node: Node::Filter { input: Box::new(scan), pred }, scope }
        }
    }
}

/// Plano de acesso de uma tabela só (usado por UPDATE e DELETE).
pub fn plan_table_scan(table: &Arc<TableInfo>, filter: &Option<Expr>) -> Plan {
    let conjs = filter.clone().map(Expr::conjuncts).unwrap_or_default();
    scan_plan(table, &table.name, &conjs)
}

// ---- SELECT -------------------------------------------------------------

struct Rel {
    alias: String,
    table: Arc<TableInfo>,
    kind: JoinKind,
    on: Vec<Expr>,
}

fn rewrite_aggs(e: &Expr, specs: &mut Vec<AggSpec>) -> Expr {
    match e {
        Expr::Agg { func, arg } => {
            let key = format!("{func}({})", arg.as_ref().map_or("*".to_string(), |a| a.to_string()));
            let pos = specs.iter().position(|s| {
                format!("{}({})", s.func, s.arg.as_ref().map_or("*".to_string(), |a| a.to_string())) == key
            });
            let i = pos.unwrap_or_else(|| {
                specs.push(AggSpec {
                    func: *func,
                    arg: arg.as_deref().cloned(),
                    name: format!("#agg{}", specs.len()),
                });
                specs.len() - 1
            });
            Expr::col(&specs[i].name)
        }
        Expr::Lit(_) | Expr::Col { .. } => e.clone(),
        Expr::Unary { op, expr } => Expr::Unary { op: *op, expr: Box::new(rewrite_aggs(expr, specs)) },
        Expr::Binary { op, left, right } => Expr::Binary {
            op: *op,
            left: Box::new(rewrite_aggs(left, specs)),
            right: Box::new(rewrite_aggs(right, specs)),
        },
        Expr::IsNull { expr, negated } => {
            Expr::IsNull { expr: Box::new(rewrite_aggs(expr, specs)), negated: *negated }
        }
        Expr::Like { expr, pattern, negated } => Expr::Like {
            expr: Box::new(rewrite_aggs(expr, specs)),
            pattern: Box::new(rewrite_aggs(pattern, specs)),
            negated: *negated,
        },
    }
}

/// Planeja um `SELECT`: escolhe os caminhos de acesso, empurra predicados para os scans,
/// escolhe o algoritmo de cada junção e monta agregação, ordenação e projeção.
///
/// # Errors
/// `Error::Exec` para tabela inexistente, alias repetido, coluna desconhecida ou ambígua, e
/// agregação em lugar proibido (`WHERE`, `ON`).
pub fn plan_select(stmt: &SelectStmt, tables: &HashMap<String, Arc<TableInfo>>) -> Result<Plan> {
    let lookup = |name: &str| {
        tables.get(name).cloned().ok_or_else(|| Error::exec(format!("tabela '{name}' não existe")))
    };

    // 1. relações
    let mut rels = vec![Rel {
        alias: stmt.from.alias().to_string(),
        table: lookup(&stmt.from.name)?,
        kind: JoinKind::Inner,
        on: vec![],
    }];
    for j in &stmt.joins {
        rels.push(Rel {
            alias: j.table.alias().to_string(),
            table: lookup(&j.table.name)?,
            kind: j.kind,
            on: j.on.clone().conjuncts(),
        });
    }
    for (i, r) in rels.iter().enumerate() {
        if rels[..i].iter().any(|o| o.alias == r.alias) {
            return Err(Error::exec(format!("alias '{}' repetido; use AS para diferenciar", r.alias)));
        }
    }

    let scopes: Vec<Scope> = rels.iter().map(|r| Scope::for_table(&r.alias, &r.table.schema)).collect();
    let full = scopes.iter().fold(Scope::default(), |acc, s| acc.concat(s));
    let mut offsets = Vec::new();
    let mut acc = 0;
    for s in &scopes {
        offsets.push(acc);
        acc += s.cols.len();
    }
    let rel_of = |col: usize| offsets.iter().rposition(|&o| o <= col).unwrap();

    // quais relações uma expressão referencia?
    let rel_set = |e: &Expr| -> Result<Vec<usize>> {
        if e.contains_agg() {
            return Err(Error::exec("função de agregação não é permitida em WHERE/ON"));
        }
        let mut cols = Vec::new();
        e.columns(&mut cols);
        let mut set = Vec::new();
        for (t, n) in cols {
            let i = rel_of(full.resolve(t.as_deref(), &n)?);
            if !set.contains(&i) {
                set.push(i);
            }
        }
        set.sort();
        Ok(set)
    };

    // 2. WHERE: empurra o que dá; o resto vira filtro no menor nível possível
    let mut pushed: Vec<Vec<Expr>> = vec![vec![]; rels.len()];
    let mut rest: Vec<(usize, Expr)> = Vec::new();
    for c in stmt.filter.clone().map(Expr::conjuncts).unwrap_or_default() {
        let set = rel_set(&c)?;
        match set.as_slice() {
            [i] if *i == 0 || rels[*i].kind == JoinKind::Inner => pushed[*i].push(c),
            [] => rest.push((0, c)),
            _ => rest.push((*set.last().unwrap(), c)),
        }
    }

    let attach = |plan: Plan, level: usize, rest: &mut Vec<(usize, Expr)>| -> Plan {
        let (now, later): (Vec<_>, Vec<_>) = std::mem::take(rest).into_iter().partition(|(l, _)| *l == level);
        *rest = later;
        match Expr::and_all(now.into_iter().map(|(_, e)| e).collect()) {
            None => plan,
            Some(pred) => {
                let scope = plan.scope.clone();
                Plan { node: Node::Filter { input: Box::new(plan), pred }, scope }
            }
        }
    };

    // 3. junções, da esquerda para a direita
    let mut plan = scan_plan(&rels[0].table, &rels[0].alias, &pushed[0]);
    plan = attach(plan, 0, &mut rest);

    for k in 1..rels.len() {
        let rel = &rels[k];
        let mut right_push = std::mem::take(&mut pushed[k]);
        let mut join_conjs = Vec::new();
        for c in &rel.on {
            let set = rel_set(c)?;
            if set.last().is_some_and(|&m| m > k) {
                return Err(Error::exec("ON referencia uma tabela que ainda não foi ligada"));
            }
            if set == [k] {
                right_push.push(c.clone());
            } else {
                join_conjs.push(c.clone());
            }
        }

        let col_type = |e: &Expr| -> Option<DataType> {
            let Expr::Col { table, name } = e else { return None };
            let i = full.resolve(table.as_deref(), name).ok()?;
            let r = rel_of(i);
            Some(rels[r].table.schema.columns[i - offsets[r]].ty)
        };

        // procura uma igualdade `esquerda = direita`
        let mut equi = None;
        for (ci, c) in join_conjs.iter().enumerate() {
            let Expr::Binary { op: BinOp::Eq, left, right } = c else { continue };
            for (l, r) in [(left, right), (right, left)] {
                let (ls, rs) = (rel_set(l)?, rel_set(r)?);
                let l_ok = !ls.is_empty() && ls.iter().all(|&i| i < k);
                if l_ok && rs == [k] && col_type(l).zip(col_type(r)).is_none_or(|(a, b)| a == b) {
                    equi = Some((ci, (**l).clone(), (**r).clone()));
                    break;
                }
            }
            if equi.is_some() {
                break;
            }
        }

        let right_scope = scopes[k].clone();
        let out_scope = plan.scope.concat(&right_scope);
        let node = match equi {
            Some((ci, lkey, rkey)) => {
                let mut others = join_conjs;
                others.remove(ci);
                let residual = Expr::and_all(others);
                let indexed = match &rkey {
                    Expr::Col { name, .. } => rel
                        .table
                        .schema
                        .index(name)
                        .and_then(|c| rel.table.index_on(c).into_iter().max_by_key(|i| i.unique as u8)),
                    _ => None,
                };
                match indexed {
                    Some(index) => Node::IndexJoin {
                        left: Box::new(plan),
                        table: rel.table.clone(),
                        alias: rel.alias.clone(),
                        index,
                        kind: rel.kind,
                        lkey,
                        residual,
                        right_filter: Expr::and_all(right_push),
                    },
                    None => Node::HashJoin {
                        left: Box::new(plan),
                        right: Box::new(scan_plan(&rel.table, &rel.alias, &right_push)),
                        kind: rel.kind,
                        lkey,
                        rkey,
                        residual,
                    },
                }
            }
            None => Node::NestedLoop {
                left: Box::new(plan),
                right: Box::new(scan_plan(&rel.table, &rel.alias, &right_push)),
                kind: rel.kind,
                on: Expr::and_all(join_conjs),
            },
        };
        plan = Plan { node, scope: out_scope };
        plan = attach(plan, k, &mut rest);
    }

    // 4. agregação
    let has_agg = !stmt.group_by.is_empty()
        || stmt.having.is_some()
        || stmt.items.iter().any(|i| matches!(i, SelectItem::Expr { expr, .. } if expr.contains_agg()))
        || stmt.order_by.iter().any(|(e, _)| e.contains_agg());

    let base_scope = plan.scope.clone();
    let mut specs: Vec<AggSpec> = Vec::new();
    let fix = |e: &Expr, specs: &mut Vec<AggSpec>| if has_agg { rewrite_aggs(e, specs) } else { e.clone() };

    // itens do SELECT
    let mut items: Vec<(Expr, String)> = Vec::new();
    for item in &stmt.items {
        match item {
            SelectItem::Star | SelectItem::QualifiedStar(_) => {
                let only = if let SelectItem::QualifiedStar(t) = item { Some(t.as_str()) } else { None };
                let before = items.len();
                for c in &base_scope.cols {
                    if only.is_none_or(|t| c.table == t) {
                        items.push((Expr::Col { table: Some(c.table.clone()), name: c.name.clone() }, c.name.clone()));
                    }
                }
                if items.len() == before {
                    return Err(Error::exec(format!("'{}.*' não casa com nenhuma tabela", only.unwrap_or("*"))));
                }
            }
            SelectItem::Expr { expr, alias } => {
                let name = match (alias, expr) {
                    (Some(a), _) => a.clone(),
                    (None, Expr::Col { name, .. }) => name.clone(),
                    (None, e) => e.to_string(),
                };
                items.push((fix(expr, &mut specs), name));
            }
        }
    }
    let group_by = stmt.group_by.clone();
    let having = stmt.having.as_ref().map(|h| fix(h, &mut specs));

    // ORDER BY: posição, alias de saída ou expressão
    let mut order: Vec<(Expr, bool)> = Vec::new();
    for (e, desc) in &stmt.order_by {
        let resolved = match e {
            Expr::Lit(Value::Int(n)) if *n >= 1 && (*n as usize) <= items.len() => items[*n as usize - 1].0.clone(),
            Expr::Col { table: None, name } if items.iter().any(|(_, n)| n == name) => {
                items.iter().find(|(_, n)| n == name).unwrap().0.clone()
            }
            other => fix(other, &mut specs),
        };
        order.push((resolved, *desc));
    }

    if has_agg {
        let mut scope = base_scope.clone();
        scope.cols.extend(specs.iter().map(|s| ScopeCol { table: String::new(), name: s.name.clone() }));
        plan = Plan { node: Node::Aggregate { input: Box::new(plan), group_by, aggs: specs }, scope: scope.clone() };
        if let Some(pred) = having {
            plan = Plan { node: Node::Filter { input: Box::new(plan), pred }, scope };
        }
    }

    if !order.is_empty() {
        let scope = plan.scope.clone();
        plan = Plan { node: Node::Sort { input: Box::new(plan), keys: order }, scope };
    }

    let out_scope = Scope {
        cols: items.iter().map(|(_, n)| ScopeCol { table: String::new(), name: n.clone() }).collect(),
    };
    plan = Plan { node: Node::Project { input: Box::new(plan), exprs: items }, scope: out_scope.clone() };

    if stmt.limit.is_some() || stmt.offset > 0 {
        plan = Plan {
            node: Node::Limit { input: Box::new(plan), limit: stmt.limit, offset: stmt.offset },
            scope: out_scope,
        };
    }
    Ok(plan)
}
