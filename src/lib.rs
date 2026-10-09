//! # rustysql
//!
//! Um banco de dados relacional embutido, escrito em Rust para **estudar como um banco funciona
//! por dentro**: armazenamento em páginas, B+tree, WAL, transações, concorrência e um
//! otimizador de consultas, tudo em poucas milhares de linhas legíveis.
//!
//! ## Início rápido
//!
//! ```
//! use rustysql::{Database, QueryResult, Value};
//!
//! # let path = std::env::temp_dir().join(format!("rustysql_quickstart_{}.db", std::process::id()));
//! # let _ = std::fs::remove_file(&path);
//! // abre (ou cria) o banco; o log de transações fica em "<arquivo>.wal"
//! let db = Database::open(&path)?;
//!
//! db.run("CREATE TABLE emp (id INT PRIMARY KEY, nome TEXT, salario FLOAT)")?;
//! db.run("INSERT INTO emp VALUES (1, 'ana', 100), (2, 'bia', 250), (3, 'caio', 180)")?;
//!
//! // uma consulta devolve um `QueryResult` por comando
//! let mut out = db.run("SELECT nome FROM emp WHERE salario > 150 ORDER BY salario DESC")?;
//! let Some(QueryResult::Rows { columns, rows }) = out.pop() else { unreachable!() };
//! assert_eq!(columns, ["nome"]);
//! assert_eq!(rows, vec![
//!     vec![Value::Text("bia".into())],
//!     vec![Value::Text("caio".into())],
//! ]);
//! # Ok::<(), rustysql::Error>(())
//! ```
//!
//! ## Transações e concorrência
//!
//! [`Database`] é um handle barato de clonar e compartilhável entre threads. Cada thread abre a sua
//! [`Session`]; dentro dela, `BEGIN ... COMMIT` agrupa comandos de forma atômica e isolada:
//!
//! ```
//! use rustysql::{Database, Error};
//! use std::thread;
//!
//! # let path = std::env::temp_dir().join(format!("rustysql_conc_{}.db", std::process::id()));
//! # let _ = std::fs::remove_file(&path);
//! let db = Database::open(&path)?;
//! db.run("CREATE TABLE conta (id INT PRIMARY KEY, saldo INT)")?;
//! db.run("INSERT INTO conta VALUES (1, 1000), (2, 0)")?;
//!
//! let workers: Vec<_> = (0..4).map(|_| {
//!     let db = db.clone();
//!     thread::spawn(move || -> Result<(), Error> {
//!         let mut s = db.session();
//!         loop {
//!             let tentativa = (|| {
//!                 s.execute("BEGIN")?;
//!                 s.execute("UPDATE conta SET saldo = saldo - 10 WHERE id = 1")?;
//!                 s.execute("UPDATE conta SET saldo = saldo + 10 WHERE id = 2")?;
//!                 s.execute("COMMIT")
//!             })();
//!             match tentativa {
//!                 Ok(_) => return Ok(()),
//!                 // falhas transitórias: a transação já foi desfeita, é só repetir
//!                 Err(Error::Deadlock | Error::LockTimeout) => continue,
//!                 Err(e) => return Err(e),
//!             }
//!         }
//!     })
//! }).collect();
//! for w in workers { w.join().unwrap()?; }
//! # Ok::<(), Error>(())
//! ```
//!
//! ## Mapa dos módulos
//!
//! As camadas, de baixo para cima (cada uma só conhece a de baixo):
//!
//! | Camada | Módulos | Responsabilidade |
//! |---|---|---|
//! | Disco | [`disk`], [`bufferpool`] | páginas de 4 KB; cache com latches, pin por `Arc`, política *no-steal* |
//! | Registros e índices | [`heap`], [`btree`], [`keyenc`] | *slotted pages*; B+tree com *latch crabbing*; chaves comparáveis por `memcmp` |
//! | Durabilidade | [`wal`] (+ checkpoint em [`db`]) | log de redo com commit atômico e CRC |
//! | Transações | [`txn`], [`lockmgr`] | undo em memória; locks de tabela 2PL; deadlock |
//! | Admissão | [`sched`] | fila de prioridade com *aging* |
//! | SQL | [`lexer`], [`parser`], [`ast`], [`eval`], [`plan`], [`exec`] | do texto ao resultado |
//! | Motor | [`catalog`], [`schema`], [`value`], [`db`] | metadados e a API pública |
//!
//! Para a visão de arquitetura, decisões de projeto (ADRs), formatos binários e guias, veja a
//! pasta `docs/` do repositório.
//!
//! ## Garantias
//!
//! * **Atomicidade e durabilidade**: um commit só retorna depois do registro do WAL estar em
//!   disco (`fsync`, se `Options::sync_commits`); uma queda no meio da gravação descarta apenas
//!   aquele commit.
//! * **Isolamento**: *strict two-phase locking* por tabela, o que dá isolamento serializável por
//!   tabela. Não há MVCC: leitores e escritores da mesma tabela se excluem.
//! * **Integridade das estruturas**: a B+tree e o buffer pool são seguros para uso concorrente
//!   por construção (latches de página, sempre adquiridos de cima para baixo).
//!
//! ## Limitações
//!
//! Uma transação precisa caber no buffer pool; locks são só de tabela; não há `ALTER TABLE`,
//! subconsultas nem `DISTINCT`. A lista completa está em `docs/architecture/arc42.md`.

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

pub mod ast;
pub mod btree;
pub mod bufferpool;
pub mod catalog;
pub mod db;
pub mod disk;
pub mod error;
pub mod eval;
pub mod exec;
pub mod heap;
pub mod keyenc;
pub mod lexer;
pub mod lockmgr;
pub mod parser;
pub mod plan;
pub mod sched;
pub mod schema;
pub mod txn;
pub mod value;
pub mod wal;

pub use db::{Database, Options, QueryResult, Session};
pub use sched::{Admit, Priority, SchedConfig, SchedStats, Scheduler};
pub use error::{Error, Result};
pub use value::Value;
