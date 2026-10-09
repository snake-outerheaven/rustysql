//! rustysql: um banco de dados SQL pequeno, escrito para aprender.
//!
//! Camadas, de baixo para cima:
//! `pager` (páginas no disco) → `heap` (registros em slotted pages) →
//! `db` (catálogo + executor) ← `parser`/`lexer` (SQL → `ast`) + `eval` (expressões).

pub mod ast;
pub mod btree;
pub mod db;
pub mod error;
pub mod eval;
pub mod heap;
pub mod lexer;
pub mod pager;
pub mod parser;
pub mod schema;
pub mod value;

pub use db::{Database, QueryResult};
pub use error::{Error, Result};
pub use value::Value;
