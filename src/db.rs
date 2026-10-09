//! O motor: junta pool, WAL, locks, catálogo, planejador e executor.
//!
//! * `Database` é um handle barato (`Clone`, `Send + Sync`) para o banco; várias threads
//!   podem usá-lo ao mesmo tempo, cada uma com a sua `Session`.
//! * Uma `Session` é uma "conexão": executa SQL em autocommit ou dentro de
//!   `BEGIN ... COMMIT`.
//!
//! Ciclo de vida de uma transação: `begin` (segura o portão de checkpoint) → comandos
//! (pegam locks de tabela 2PL, mexem no heap/índices em memória, acumulam WAL e undo) →
//! `commit` (grava o WAL de forma atômica e durável, solta os locks) ou `rollback`
//! (aplica o undo em memória).

use crate::ast::{Expr, SelectStmt, Statement};
use crate::btree::BTree;
use crate::bufferpool::BufferPool;
use crate::catalog::{Catalog, IndexInfo, TableInfo};
use crate::error::{Error, Result};
use crate::eval::{bind, Scope};
use crate::exec::{collect_rids, execute};
use crate::heap::{self, HeapFile, RowId};
use crate::keyenc::value_key;
use crate::lockmgr::{LockManager, LockMode, TableId};
use crate::parser::parse;
use crate::sched::{Admit, Priority, SchedConfig, SchedStats, Scheduler};
use crate::plan::{plan_select, plan_table_scan};
use crate::schema::{Column, Schema};
use crate::txn::{Txn, Undo};
use crate::value::{decode_row, encode_row, DataType, Row, Value};
use crate::wal::Wal;
use parking_lot::{Mutex, RwLock};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const CATALOG_ROOT: u32 = 1;

/// Opções de abertura do banco. `Options::default()` serve para uso geral; os testes
/// tipicamente desligam `sync_commits` para rodar mais rápido.
///
/// # Exemplo
/// ```
/// use rustysql::{Options, SchedConfig};
/// use std::time::Duration;
///
/// let opts = Options {
///     pool_pages: 4096,
///     sync_commits: true,
///     sched: SchedConfig { max_running: 16, ..SchedConfig::default() },
///     ..Options::default()
/// };
/// assert_eq!(opts.lock_timeout, Duration::from_secs(5));
/// ```
#[derive(Debug, Clone)]
pub struct Options {
    /// tamanho do buffer pool, em páginas de 4 KB
    pub pool_pages: usize,
    /// `fsync` do WAL a cada commit (desligue só em testes/benchmarks)
    pub sync_commits: bool,
    /// Quanto uma transação espera por um lock de tabela antes de falhar com
    /// [`Error::LockTimeout`].
    pub lock_timeout: Duration,
    /// fração de páginas sujas que dispara um checkpoint automático
    pub checkpoint_ratio: f64,
    /// controle de admissão e prioridades (veja `sched`)
    pub sched: SchedConfig,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            pool_pages: 8192,
            sync_commits: true,
            lock_timeout: Duration::from_secs(5),
            checkpoint_ratio: 0.5,
            sched: SchedConfig::default(),
        }
    }
}

/// Resultado de um comando.
#[derive(Debug)]
pub enum QueryResult {
    /// Resultado de um `SELECT`.
    Rows {
        /// Nomes das colunas de saída.
        columns: Vec<String>,
        /// Linhas, na ordem pedida.
        rows: Vec<Row>,
    },
    /// Número de linhas afetadas por `INSERT`, `UPDATE` ou `DELETE`.
    Affected(usize),
    /// Mensagem de um comando sem linhas (`CREATE`, `BEGIN`, `EXPLAIN`...).
    Done(String),
}

pub(crate) struct Inner {
    pool: Arc<BufferPool>,
    wal: Wal,
    locks: LockManager,
    sched: Arc<Scheduler>,
    catalog: Arc<Catalog>,
    catalog_heap: HeapFile,
    /// toda transação segura o lado "leitor"; o checkpoint pega o lado "escritor"
    gate: Arc<RwLock<()>>,
    ddl: Arc<Mutex<()>>,
    next_txn: AtomicU64,
    opts: Options,
    crashed: AtomicBool,
}

/// Handle para um banco aberto. É barato de clonar (`Arc` por dentro) e `Send + Sync`: passe
/// um clone para cada thread e abra uma [`Session`] em cada uma.
///
/// O banco é fechado, com um checkpoint final, quando o **último** handle e a última sessão
/// são descartados.
///
/// # Exemplo
/// ```
/// use rustysql::{Database, QueryResult, Value};
///
/// let path = std::env::temp_dir().join(format!("rustysql_doc_db_{}.db", std::process::id()));
/// let _ = std::fs::remove_file(&path);
/// let db = Database::open(&path)?;
/// db.run("CREATE TABLE t (id INT PRIMARY KEY, nome TEXT)")?;
/// db.run("INSERT INTO t VALUES (1, 'ana'), (2, 'bia')")?;
///
/// let mut results = db.run("SELECT nome FROM t WHERE id = 2")?;
/// let Some(QueryResult::Rows { rows, .. }) = results.pop() else { unreachable!() };
/// assert_eq!(rows, vec![vec![Value::Text("bia".into())]]);
/// # Ok::<(), rustysql::Error>(())
/// ```
#[derive(Clone)]
pub struct Database {
    inner: Arc<Inner>,
}

impl Database {
    /// Abre (ou cria) o banco em `path` com as opções padrão. O log fica em `<path>.wal`.
    ///
    /// Se o banco não foi fechado limpamente, refaz o WAL e reconstrói os índices antes de
    /// devolver (veja [`Database::open_with`]).
    ///
    /// # Errors
    /// Falha se o arquivo não é um banco rustysql (ou está em formato antigo) ou se a
    /// recuperação encontra dados inconsistentes.
    pub fn open(path: impl AsRef<Path>) -> Result<Database> {
        Database::open_with(path, Options::default())
    }

    /// Como [`Database::open`], com opções explícitas.
    ///
    /// # Recuperação
    /// 1. Lê o WAL e descarta um registro final incompleto ou com CRC errado.
    /// 2. Reaplica, em ordem, as operações de heap dos registros restantes (idempotentes).
    /// 3. Carrega o catálogo e **reconstrói todos os índices** a partir do heap.
    /// 4. Faz um checkpoint, que grava as páginas e esvazia o WAL.
    pub fn open_with(path: impl AsRef<Path>, opts: Options) -> Result<Database> {
        let path = path.as_ref();
        let pool = Arc::new(BufferPool::open(path, opts.pool_pages)?);
        let (wal, batches) = Wal::open(Path::new(&format!("{}.wal", path.display())), opts.sync_commits)?;
        let fresh = pool.num_pages() == 1;

        let catalog_heap = if fresh {
            let heap = HeapFile::create(&pool, &mut Vec::new())?;
            assert_eq!(heap.root, CATALOG_ROOT);
            heap
        } else {
            // recuperação: refaz, em ordem, tudo que foi commitado desde o último checkpoint
            for batch in &batches {
                for op in &batch.ops {
                    heap::apply_op(&pool, op)?;
                }
            }
            HeapFile::open(pool.clone(), CATALOG_ROOT)
        };

        let inner = Arc::new(Inner {
            locks: LockManager::new(opts.lock_timeout),
            sched: Scheduler::new(opts.sched.clone()),
            catalog: Arc::new(Catalog::default()),
            gate: Arc::new(RwLock::new(())),
            ddl: Arc::new(Mutex::new(())),
            next_txn: AtomicU64::new(1),
            crashed: AtomicBool::new(false),
            pool,
            wal,
            catalog_heap,
            opts,
        });
        inner.load_catalog()?;
        let recovered = !fresh && !batches.is_empty();
        if recovered {
            inner.rebuild_indexes()?;
        }
        if fresh || recovered {
            inner.checkpoint()?;
        }
        Ok(Database { inner })
    }

    /// Abre uma nova sessão ("conexão") neste banco. Cada thread deve usar a sua.
    pub fn session(&self) -> Session {
        Session { db: self.inner.clone(), txn: None, priority: Priority::Normal }
    }

    /// Atalho: executa em autocommit numa sessão descartável.
    pub fn run(&self, sql: &str) -> Result<Vec<QueryResult>> {
        self.session().execute(sql)
    }

    /// Grava todas as páginas sujas no arquivo e esvazia o WAL.
    ///
    /// Espera todas as transações em andamento terminarem e bloqueia as novas enquanto roda.
    /// O banco também faz isso sozinho quando a fração de páginas sujas passa de
    /// [`Options::checkpoint_ratio`].
    pub fn checkpoint(&self) -> Result<()> {
        self.inner.checkpoint()
    }

    /// Nomes das tabelas, em ordem alfabética.
    pub fn table_names(&self) -> Vec<String> {
        self.inner.catalog.all().iter().map(|t| t.name.clone()).collect()
    }

    /// Colunas e índices de uma tabela, ou `None` se ela não existe.
    pub fn describe(&self, table: &str) -> Option<TableDescription> {
        let t = self.inner.catalog.get(table)?;
        Some(TableDescription {
            columns: t.schema.columns.clone(),
            indexes: t
                .indexes()
                .iter()
                .map(|i| (i.name.clone(), t.schema.columns[i.column].name.clone(), i.unique))
                .collect(),
        })
    }

    /// O gerenciador de prioridade: dá para pedir uma vaga direto (`admit`) e consultar estatísticas.
    pub fn scheduler(&self) -> &Arc<Scheduler> {
        &self.inner.sched
    }

    /// Estatísticas do gerenciador de prioridade (fila, admissões por prioridade, esperas).
    pub fn scheduler_stats(&self) -> SchedStats {
        self.inner.sched.stats()
    }

    /// Diagnóstico: quem segura e quem espera cada lock de tabela.
    pub fn lock_snapshot(&self) -> String {
        self.inner.locks.snapshot()
    }

    /// Páginas sujas no buffer pool (ainda não gravadas no arquivo de dados).
    pub fn dirty_pages(&self) -> usize {
        self.inner.pool.dirty_pages()
    }

    /// Tamanho atual do WAL em bytes (0 logo depois de um checkpoint).
    pub fn wal_bytes(&self) -> u64 {
        self.inner.wal.len().unwrap_or(0)
    }

    /// Para testes de recuperação: ao soltar o banco, não faz o checkpoint final,
    /// como se o processo tivesse morrido (páginas sujas e WAL ficam como estão).
    pub fn simulate_crash(&self) {
        self.inner.crashed.store(true, Ordering::SeqCst);
    }
}

/// Descrição de uma tabela, devolvida por [`Database::describe`].
#[derive(Debug)]
pub struct TableDescription {
    /// Colunas, na ordem do esquema.
    pub columns: Vec<Column>,
    /// (nome, coluna, único?)
    pub indexes: Vec<(String, String, bool)>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if !self.crashed.load(Ordering::SeqCst) {
            let _ = self.checkpoint();
        }
    }
}

// ---- sessão ---------------------------------------------------------------

/// Uma "conexão": executa SQL em autocommit ou dentro de `BEGIN ... COMMIT`.
///
/// Uma sessão não é compartilhada entre threads ao mesmo tempo (os métodos pedem `&mut self`),
/// mas pode ser movida entre threads. Se for descartada com uma transação aberta, ela sofre
/// *rollback*.
///
/// # Isolamento
/// Os comandos pegam locks de tabela (compartilhado para leitura, exclusivo para escrita) e só
/// os soltam no fim da transação (*strict two-phase locking*), o que dá isolamento
/// serializável por tabela.
///
/// # Exemplo
/// ```
/// use rustysql::Database;
///
/// let path = std::env::temp_dir().join(format!("rustysql_doc_session_{}.db", std::process::id()));
/// let _ = std::fs::remove_file(&path);
/// let db = Database::open(&path)?;
/// let mut s = db.session();
/// s.execute("CREATE TABLE conta (id INT PRIMARY KEY, saldo INT)")?;
/// s.execute("INSERT INTO conta VALUES (1, 100), (2, 0)")?;
///
/// s.execute("BEGIN")?;
/// s.execute("UPDATE conta SET saldo = saldo - 30 WHERE id = 1")?;
/// s.execute("UPDATE conta SET saldo = saldo + 30 WHERE id = 2")?;
/// s.execute("ROLLBACK")?; // nada disso aconteceu
/// # Ok::<(), rustysql::Error>(())
/// ```
pub struct Session {
    db: Arc<Inner>,
    txn: Option<Txn>,
    priority: Priority,
}

impl Session {
    /// Executa um ou mais comandos separados por `;` e devolve um resultado por comando.
    ///
    /// Para no primeiro erro. Em autocommit, cada comando é uma transação; dentro de `BEGIN`, um
    /// comando que falha desfaz só a si mesmo e mantém a transação aberta (exceto deadlock e
    /// timeout de lock, que abortam a transação inteira).
    ///
    /// # Errors
    /// Erro de sintaxe, de execução (tabela inexistente, violação de restrição...), deadlock,
    /// timeout de lock ou de fila, ou falha de armazenamento.
    pub fn execute(&mut self, sql: &str) -> Result<Vec<QueryResult>> {
        parse(sql)?.iter().map(|s| self.execute_stmt(s)).collect()
    }

    /// Há uma transação explícita aberta?
    pub fn in_transaction(&self) -> bool {
        self.txn.is_some()
    }

    /// Prioridade de admissão dos próximos comandos desta sessão.
    pub fn set_priority(&mut self, p: Priority) {
        self.priority = p;
    }

    /// Prioridade de admissão atual desta sessão.
    pub fn priority(&self) -> Priority {
        self.priority
    }

    /// Executa um comando já interpretado (veja [`parse`]). É o que
    /// [`Session::execute`] chama para cada comando do texto.
    pub fn execute_stmt(&mut self, stmt: &Statement) -> Result<QueryResult> {
        match stmt {
            Statement::Begin => {
                if self.txn.is_some() {
                    return Err(Error::exec("já existe uma transação aberta"));
                }
                self.txn = Some(self.db.begin(true, self.priority)?);
                Ok(QueryResult::Done("BEGIN".into()))
            }
            Statement::Commit => {
                let txn = self.txn.take().ok_or_else(|| Error::exec("não há transação aberta"))?;
                self.db.commit(txn)?;
                Ok(QueryResult::Done("COMMIT".into()))
            }
            Statement::Rollback => {
                let txn = self.txn.take().ok_or_else(|| Error::exec("não há transação aberta"))?;
                self.db.rollback(txn)?;
                Ok(QueryResult::Done("ROLLBACK".into()))
            }
            Statement::SetPriority(p) => {
                self.priority = *p;
                Ok(QueryResult::Done(format!("PRIORITY {}", p.name())))
            }
            Statement::Checkpoint => {
                if self.txn.is_some() {
                    return Err(Error::exec("CHECKPOINT não pode rodar dentro de uma transação"));
                }
                self.db.checkpoint()?;
                Ok(QueryResult::Done("CHECKPOINT".into()))
            }
            other => self.run_in_txn(other),
        }
    }

    fn run_in_txn(&mut self, stmt: &Statement) -> Result<QueryResult> {
        let auto = self.txn.is_none();
        if auto {
            // begin antes da fila: ele pode esperar um checkpoint, e quem espera na fila
            // de admissão não pode estar segurando uma vaga que o checkpoint precise
            self.txn = Some(self.db.begin(false, self.priority)?);
        }
        // quem já segura locks entra na frente: segurar lock esperando vaga atrasa todo mundo
        let why = if self.txn.as_ref().is_some_and(|t| t.locked) {
            Admit::InTransaction
        } else {
            Admit::New(self.priority)
        };
        let permit = match self.db.sched.admit(why) {
            Ok(p) => p,
            Err(e) => {
                if auto {
                    let txn = self.txn.take().unwrap();
                    let _ = self.db.rollback(txn);
                }
                return Err(e);
            }
        };
        let txn = self.txn.as_mut().unwrap();
        txn.permit = Some(permit);
        let mark = txn.mark();
        match self.db.exec(txn, stmt) {
            Ok(result) => {
                if auto {
                    let txn = self.txn.take().unwrap();
                    self.db.commit(txn)?; // a vaga só volta depois do commit durável
                } else {
                    txn.permit = None;
                }
                Ok(result)
            }
            Err(e) => {
                // deadlock/timeout: a transação inteira tem que sair para destravar os outros
                let fatal = matches!(e, Error::Deadlock | Error::LockTimeout);
                if auto || fatal {
                    let txn = self.txn.take().unwrap();
                    let _ = self.db.rollback(txn);
                } else {
                    // erro num comando: desfaz só ele e mantém a transação aberta
                    let txn = self.txn.as_mut().unwrap();
                    let _ = txn.rollback_to(mark);
                    txn.permit = None;
                }
                Err(e)
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(txn) = self.txn.take() {
            let _ = self.db.rollback(txn);
        }
    }
}

// ---- motor ------------------------------------------------------------------

impl Inner {
    fn load_catalog(&self) -> Result<()> {
        let bad = || Error::storage("linha de catálogo inválida");
        let mut table_rows = Vec::new();
        let mut index_rows = Vec::new();
        for item in self.catalog_heap.scan() {
            let (rid, bytes) = item?;
            let row = decode_row(&bytes)?;
            match &row[..] {
                [Value::Text(k), Value::Text(name), Value::Int(root), Value::Text(schema)] if k == "table" => {
                    table_rows.push((rid, name.clone(), *root as u32, Schema::deserialize(schema)?));
                }
                [Value::Text(k), Value::Text(name), Value::Text(table), Value::Int(col), Value::Bool(unique), Value::Int(root)]
                    if k == "index" =>
                {
                    index_rows.push((rid, name.clone(), table.clone(), *col as usize, *unique, *root as u32));
                }
                _ => return Err(bad()),
            }
        }
        let mut tables: HashMap<String, Arc<TableInfo>> = HashMap::new();
        for (rid, name, root, schema) in table_rows {
            let table = Arc::new(TableInfo {
                name: name.clone(),
                schema,
                heap: HeapFile::open(self.pool.clone(), root),
                indexes: RwLock::new(Vec::new()),
                catalog_rid: rid,
                dropped: AtomicBool::new(false),
            });
            tables.insert(name, table);
        }
        for (rid, name, table, column, unique, root) in index_rows {
            let t = tables.get(&table).ok_or_else(bad)?;
            t.indexes.write().push(Arc::new(IndexInfo {
                name,
                table,
                column,
                unique,
                tree: BTree::open(self.pool.clone(), root),
                catalog_rid: rid,
            }));
        }
        for t in tables.into_values() {
            self.catalog.publish(t);
        }
        Ok(())
    }

    /// Índices não passam pelo WAL: depois de uma recuperação são refeitos a partir do heap.
    fn rebuild_indexes(&self) -> Result<()> {
        for table in self.catalog.all() {
            let indexes = table.indexes();
            for idx in &indexes {
                idx.tree.reset()?;
            }
            if indexes.is_empty() {
                continue;
            }
            for item in table.heap.scan() {
                let (rid, bytes) = item?;
                let row = decode_row(&bytes)?;
                for idx in &indexes {
                    if !idx.tree.insert(&idx.key_for(&row, rid), rid)? {
                        return Err(Error::storage(format!("chave duplicada ao reconstruir '{}'", idx.name)));
                    }
                }
            }
        }
        Ok(())
    }

    /// Checkpoint "sharp": espera todas as transações terminarem, grava as páginas
    /// sujas e só então esvazia o WAL.
    fn checkpoint(&self) -> Result<()> {
        let _exclusive = self.gate.write();
        self.pool.flush_all()?;
        self.wal.reset()
    }

    fn begin(&self, explicit: bool, priority: Priority) -> Result<Txn> {
        let limit = self.opts.checkpoint_ratio * self.pool.capacity() as f64;
        if self.pool.dirty_pages() as f64 > limit {
            self.checkpoint()?;
        }
        Ok(Txn {
            id: self.next_txn.fetch_add(1, Ordering::SeqCst),
            explicit,
            priority,
            wal: Vec::new(),
            undo: Vec::new(),
            on_commit: Vec::new(),
            _gate: self.gate.read_arc(),
            ddl_guard: None,
            permit: None,
            locked: false,
        })
    }

    fn commit(&self, mut txn: Txn) -> Result<()> {
        if !txn.wal.is_empty()
            && let Err(e) = self.wal.append_commit(txn.id, &txn.wal)
        {
            let _ = self.rollback(txn);
            return Err(e);
        }
        for publish in txn.on_commit.drain(..) {
            publish();
        }
        self.locks.release_all(txn.id);
        Ok(()) // `txn` sai de escopo: solta o portão e o guard de DDL
    }

    fn rollback(&self, mut txn: Txn) -> Result<()> {
        let mark = crate::txn::Mark::start();
        let result = txn.rollback_to(mark);
        // o que sobrou de `wal` são só operações estruturais: precisam ser duráveis
        // antes de soltar os locks, porque a próxima transação vai escrever nessas páginas
        let logged = if txn.wal.is_empty() { Ok(()) } else { self.wal.append_commit(txn.id, &txn.wal) };
        self.locks.release_all(txn.id);
        result.and(logged)
    }

    // ---- execução de comandos -------------------------------------------

    fn exec(&self, txn: &mut Txn, stmt: &Statement) -> Result<QueryResult> {
        match stmt {
            Statement::CreateTable { name, columns } => self.create_table(txn, name, columns),
            Statement::DropTable { name } => self.drop_table(txn, name),
            Statement::CreateIndex { name, table, column, unique } => {
                self.create_index(txn, name, table, column, *unique)
            }
            Statement::DropIndex { name } => self.drop_index(txn, name),
            Statement::Insert { table, columns, rows } => self.insert(txn, table, columns, rows),
            Statement::Select(sel) => self.select(txn, sel),
            Statement::Update { table, sets, filter } => self.update(txn, table, sets, filter),
            Statement::Delete { table, filter } => self.delete(txn, table, filter),
            Statement::Explain(inner) => self.explain(inner),
            Statement::Begin
            | Statement::Commit
            | Statement::Rollback
            | Statement::Checkpoint
            | Statement::SetPriority(_) => {
                Err(Error::exec("comando de transação inesperado"))
            }
        }
    }

    /// Pega um lock de tabela. Se for preciso esperar, devolve a vaga de execução enquanto
    /// o comando está parado e a retoma (com prioridade máxima) quando o lock sai.
    fn lock(&self, txn: &mut Txn, table: TableId, mode: LockMode) -> Result<()> {
        let id = txn.id;
        let mut yielded = false;
        let permit = &mut txn.permit;
        let rank = txn.priority as u8;
        let result = self.locks.acquire_with(id, table, mode, rank, &mut || {
            if let Some(p) = permit.as_mut() {
                p.yield_slot();
                yielded = true;
            }
        });
        if result.is_ok() {
            txn.locked = true;
            if yielded && let Some(p) = txn.permit.as_mut() {
                p.resume()?;
            }
        }
        result
    }

    fn table(&self, name: &str) -> Result<Arc<TableInfo>> {
        self.catalog.get(name).ok_or_else(|| Error::exec(format!("tabela '{name}' não existe")))
    }

    /// Resolve, trava (strict 2PL: o lock só cai no fim da transação) e revalida.
    fn lock_table(&self, txn: &mut Txn, name: &str, mode: LockMode) -> Result<Arc<TableInfo>> {
        let table = self.table(name)?;
        self.lock(txn, table.id(), mode)?;
        if table.dropped.load(Ordering::SeqCst) {
            return Err(Error::exec(format!("tabela '{name}' não existe")));
        }
        Ok(table)
    }

    fn select_tables(&self, txn: Option<&mut Txn>, sel: &SelectStmt) -> Result<HashMap<String, Arc<TableInfo>>> {
        let mut names = vec![sel.from.name.clone()];
        names.extend(sel.joins.iter().map(|j| j.table.name.clone()));
        let mut found: Vec<Arc<TableInfo>> = Vec::new();
        for n in &names {
            let t = self.table(n)?;
            if !found.iter().any(|f| f.id() == t.id()) {
                found.push(t);
            }
        }
        if let Some(txn) = txn {
            // ordem global (por id) reduz deadlocks entre comandos que usam as mesmas tabelas
            found.sort_by_key(|t| t.id());
            for t in &found {
                self.lock(txn, t.id(), LockMode::Shared)?;
                if t.dropped.load(Ordering::SeqCst) {
                    return Err(Error::exec(format!("tabela '{}' não existe", t.name)));
                }
            }
        }
        Ok(found.into_iter().map(|t| (t.name.clone(), t)).collect())
    }

    fn select(&self, txn: &mut Txn, sel: &SelectStmt) -> Result<QueryResult> {
        let tables = self.select_tables(Some(txn), sel)?;
        let plan = plan_select(sel, &tables)?;
        let columns = plan.scope.cols.iter().map(|c| c.name.clone()).collect();
        let rows = execute(&plan)?.collect::<Result<Vec<_>>>()?;
        Ok(QueryResult::Rows { columns, rows })
    }

    fn explain(&self, stmt: &Statement) -> Result<QueryResult> {
        let text = match stmt {
            Statement::Select(sel) => {
                let tables = self.select_tables(None, sel)?;
                plan_select(sel, &tables)?.explain()
            }
            Statement::Update { table, filter, .. } | Statement::Delete { table, filter } => {
                let t = self.table(table)?;
                let verb = if matches!(stmt, Statement::Update { .. }) { "Update" } else { "Delete" };
                format!("{verb} {table}\n  {}", plan_table_scan(&t, filter).explain().replace('\n', "\n  "))
            }
            _ => return Err(Error::exec("EXPLAIN só vale para SELECT, UPDATE e DELETE")),
        };
        Ok(QueryResult::Done(text))
    }

    // ---- DML ---------------------------------------------------------------

    fn insert_row(&self, txn: &mut Txn, table: &Arc<TableInfo>, row: &Row) -> Result<RowId> {
        let indexes = table.indexes();
        for idx in indexes.iter().filter(|i| i.unique && row[i.column] != Value::Null) {
            if idx.tree.search(&value_key(&row[idx.column]))?.is_some() {
                return Err(dup_error(idx, &row[idx.column]));
            }
        }
        let rid = table.heap.insert(&encode_row(row), &mut txn.wal)?;
        txn.undo.push(Undo::HeapInsert { heap: table.heap.clone(), rid });
        for idx in &indexes {
            let key = idx.key_for(row, rid);
            if !idx.tree.insert(&key, rid)? {
                return Err(dup_error(idx, &row[idx.column]));
            }
            txn.undo.push(Undo::IndexInsert { tree: idx.tree.clone(), key });
        }
        Ok(rid)
    }

    fn delete_row(&self, txn: &mut Txn, table: &Arc<TableInfo>, rid: RowId, row: &Row) -> Result<()> {
        for idx in table.indexes() {
            let key = idx.key_for(row, rid);
            idx.tree.delete(&key)?;
            txn.undo.push(Undo::IndexDelete { tree: idx.tree.clone(), key, rid });
        }
        let data = table.heap.delete(rid, &mut txn.wal)?;
        txn.undo.push(Undo::HeapDelete { heap: table.heap.clone(), rid, data });
        Ok(())
    }

    fn insert(
        &self,
        txn: &mut Txn,
        name: &str,
        columns: &Option<Vec<String>>,
        rows: &[Vec<Expr>],
    ) -> Result<QueryResult> {
        let table = self.lock_table(txn, name, LockMode::Exclusive)?;
        let schema = &table.schema;
        let n = schema.columns.len();

        let positions: Vec<usize> = match columns {
            None => (0..n).collect(),
            Some(names) => {
                let mut pos = Vec::new();
                for c in names {
                    let i = schema
                        .index(c)
                        .ok_or_else(|| Error::exec(format!("coluna desconhecida '{c}'")))?;
                    if pos.contains(&i) {
                        return Err(Error::exec(format!("coluna '{c}' repetida")));
                    }
                    pos.push(i);
                }
                pos
            }
        };

        let empty = Scope::default();
        let mut new_rows: Vec<Row> = Vec::with_capacity(rows.len());
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
                row[pos] = bind(e, &empty)?.eval(&[])?;
            }
            for (i, col) in schema.columns.iter().enumerate() {
                row[i] = coerce(std::mem::replace(&mut row[i], Value::Null), col)?;
            }
            new_rows.push(row);
        }
        for row in &new_rows {
            self.insert_row(txn, &table, row)?;
        }
        Ok(QueryResult::Affected(new_rows.len()))
    }

    fn update(
        &self,
        txn: &mut Txn,
        name: &str,
        sets: &[(String, Expr)],
        filter: &Option<Expr>,
    ) -> Result<QueryResult> {
        let table = self.lock_table(txn, name, LockMode::Exclusive)?;
        let schema = &table.schema;
        let scope = Scope::for_table(&table.name, schema);

        let mut targets = Vec::new();
        for (col, e) in sets {
            let i = schema.index(col).ok_or_else(|| Error::exec(format!("coluna desconhecida '{col}'")))?;
            targets.push((i, bind(e, &scope)?));
        }

        let matched = collect_rids(&plan_table_scan(&table, filter))?;
        let mut changes: Vec<(RowId, Row, Row)> = Vec::new();
        for (rid, old) in matched {
            let mut new = old.clone();
            for (i, e) in &targets {
                new[*i] = coerce(e.eval(&old)?, &schema.columns[*i])?;
            }
            changes.push((rid, old, new));
        }

        // apaga todas as versões antigas antes de inserir as novas: permite trocar
        // chaves em lote (`SET id = id + 1`) sem falsas colisões
        let affected = changes.len();
        let changes: Vec<_> = changes.into_iter().filter(|(_, old, new)| old != new).collect();
        for (rid, old, _) in &changes {
            self.delete_row(txn, &table, *rid, old)?;
        }
        for (_, _, new) in &changes {
            self.insert_row(txn, &table, new)?;
        }
        Ok(QueryResult::Affected(affected))
    }

    fn delete(&self, txn: &mut Txn, name: &str, filter: &Option<Expr>) -> Result<QueryResult> {
        let table = self.lock_table(txn, name, LockMode::Exclusive)?;
        let matched = collect_rids(&plan_table_scan(&table, filter))?;
        for (rid, row) in &matched {
            self.delete_row(txn, &table, *rid, row)?;
        }
        Ok(QueryResult::Affected(matched.len()))
    }

    // ---- DDL ---------------------------------------------------------------

    fn begin_ddl(&self, txn: &mut Txn) -> Result<()> {
        if txn.explicit {
            return Err(Error::exec("DDL não é permitido dentro de uma transação explícita"));
        }
        txn.ddl_guard = Some(self.ddl.lock_arc());
        Ok(())
    }

    fn catalog_insert(&self, txn: &mut Txn, row: &[Value]) -> Result<RowId> {
        let rid = self.catalog_heap.insert(&encode_row(row), &mut txn.wal)?;
        txn.undo.push(Undo::HeapInsert { heap: self.catalog_heap.clone(), rid });
        Ok(rid)
    }

    fn catalog_delete(&self, txn: &mut Txn, rid: RowId) -> Result<()> {
        let data = self.catalog_heap.delete(rid, &mut txn.wal)?;
        txn.undo.push(Undo::HeapDelete { heap: self.catalog_heap.clone(), rid, data });
        Ok(())
    }

    fn index_row(name: &str, table: &str, column: usize, unique: bool, root: u32) -> Vec<Value> {
        vec![
            Value::Text("index".into()),
            Value::Text(name.into()),
            Value::Text(table.into()),
            Value::Int(column as i64),
            Value::Bool(unique),
            Value::Int(root as i64),
        ]
    }

    fn create_table(&self, txn: &mut Txn, name: &str, columns: &[Column]) -> Result<QueryResult> {
        self.begin_ddl(txn)?;
        if self.catalog.get(name).is_some() {
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
        let heap = HeapFile::create(&self.pool, &mut txn.wal)?;
        let table_row = [
            Value::Text("table".into()),
            Value::Text(name.into()),
            Value::Int(heap.root as i64),
            Value::Text(schema.serialize()),
        ];
        let catalog_rid = self.catalog_insert(txn, &table_row)?;

        let mut indexes = Vec::new();
        if let Some(pk) = schema.primary_key() {
            let tree = BTree::create(&self.pool)?;
            let idx_name = format!("pk_{name}");
            let rid = self.catalog_insert(txn, &Self::index_row(&idx_name, name, pk, true, tree.root()))?;
            indexes.push(Arc::new(IndexInfo {
                name: idx_name,
                table: name.into(),
                column: pk,
                unique: true,
                tree,
                catalog_rid: rid,
            }));
        }
        let table = Arc::new(TableInfo {
            name: name.into(),
            schema,
            heap,
            indexes: RwLock::new(indexes),
            catalog_rid,
            dropped: AtomicBool::new(false),
        });
        self.lock(txn, table.id(), LockMode::Exclusive)?;
        let catalog = self.catalog.clone();
        txn.on_commit.push(Box::new(move || catalog.publish(table)));
        Ok(QueryResult::Done(format!("tabela '{name}' criada")))
    }

    fn drop_table(&self, txn: &mut Txn, name: &str) -> Result<QueryResult> {
        self.begin_ddl(txn)?;
        let table = self.lock_table(txn, name, LockMode::Exclusive)?;
        for idx in table.indexes() {
            self.catalog_delete(txn, idx.catalog_rid)?;
        }
        self.catalog_delete(txn, table.catalog_rid)?;
        let catalog = self.catalog.clone();
        txn.on_commit.push(Box::new(move || {
            table.dropped.store(true, Ordering::SeqCst);
            catalog.remove(&table.name);
        }));
        Ok(QueryResult::Done(format!("tabela '{name}' removida")))
    }

    fn create_index(
        &self,
        txn: &mut Txn,
        name: &str,
        table_name: &str,
        column: &str,
        unique: bool,
    ) -> Result<QueryResult> {
        self.begin_ddl(txn)?;
        if self.catalog.find_index(name).is_some() {
            return Err(Error::exec(format!("índice '{name}' já existe")));
        }
        let table = self.lock_table(txn, table_name, LockMode::Exclusive)?;
        let col = table
            .schema
            .index(column)
            .ok_or_else(|| Error::exec(format!("coluna desconhecida '{column}'")))?;

        let tree = BTree::create(&self.pool)?;
        let rid = self.catalog_insert(txn, &Self::index_row(name, table_name, col, unique, tree.root()))?;
        let idx = Arc::new(IndexInfo {
            name: name.into(),
            table: table_name.into(),
            column: col,
            unique,
            tree,
            catalog_rid: rid,
        });
        // carrega o índice com o conteúdo atual da tabela
        for item in table.heap.scan() {
            let (rid, bytes) = item?;
            let row = decode_row(&bytes)?;
            if !idx.tree.insert(&idx.key_for(&row, rid), rid)? {
                return Err(Error::exec(format!(
                    "não dá para criar o índice único '{name}': há valores repetidos em '{column}'"
                )));
            }
        }
        txn.on_commit.push(Box::new(move || table.indexes.write().push(idx)));
        Ok(QueryResult::Done(format!("índice '{name}' criado")))
    }

    fn drop_index(&self, txn: &mut Txn, name: &str) -> Result<QueryResult> {
        self.begin_ddl(txn)?;
        let (table, idx) = self
            .catalog
            .find_index(name)
            .ok_or_else(|| Error::exec(format!("índice '{name}' não existe")))?;
        if table.schema.primary_key() == Some(idx.column) && idx.name == format!("pk_{}", table.name) {
            return Err(Error::exec("o índice da chave primária não pode ser removido"));
        }
        self.lock(txn, table.id(), LockMode::Exclusive)?;
        self.catalog_delete(txn, idx.catalog_rid)?;
        let name = name.to_string();
        let reply = format!("índice '{name}' removido");
        txn.on_commit.push(Box::new(move || table.indexes.write().retain(|i| i.name != name)));
        Ok(QueryResult::Done(reply))
    }
}

fn dup_error(idx: &IndexInfo, v: &Value) -> Error {
    if idx.name.starts_with("pk_") {
        Error::exec(format!("chave primária duplicada: {v}"))
    } else {
        Error::exec(format!("valor duplicado no índice único '{}': {v}", idx.name))
    }
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

