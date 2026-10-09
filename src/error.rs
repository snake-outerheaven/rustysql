//! Tipo de erro único da biblioteca.
//!
//! Um só `enum` com variantes por categoria (sintaxe, execução, armazenamento, E/S) mais três
//! falhas *transitórias* de concorrência (`Deadlock`, `LockTimeout`, `QueueTimeout`), que o
//! chamador pode resolver repetindo a transação.

use std::fmt;

/// Erros do rustysql. Todas as operações falíveis devolvem [`Result`].
///
/// As variantes `Deadlock`, `LockTimeout` e `QueueTimeout` indicam falhas transitórias: o
/// cliente pode tentar de novo a transação (idealmente com recuo exponencial).
#[derive(Debug)]
pub enum Error {
    /// Falha de E/S do sistema operacional.
    Io(std::io::Error),
    /// Erro de sintaxe SQL.
    Parse(String),
    /// Erro de execução: tabela inexistente, tipo errado, violação de restrição...
    Exec(String),
    /// Inconsistência ou falha na camada de armazenamento (arquivo corrompido, pool cheio).
    Storage(String),
    /// a transação foi escolhida como vítima de um deadlock
    Deadlock,
    /// Esperou um lock de tabela por mais que [`Options::lock_timeout`](crate::Options::lock_timeout).
    LockTimeout,
    /// esperou demais na fila de admissão do gerenciador de prioridade
    QueueTimeout,
}

/// `Result` com o [`Error`] do rustysql.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Cria um [`Error::Parse`].
    pub fn parse(msg: impl Into<String>) -> Self {
        Error::Parse(msg.into())
    }
    /// Cria um [`Error::Exec`].
    pub fn exec(msg: impl Into<String>) -> Self {
        Error::Exec(msg.into())
    }
    /// Cria um [`Error::Storage`].
    pub fn storage(msg: impl Into<String>) -> Self {
        Error::Storage(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::Parse(m) => write!(f, "syntax error: {m}"),
            Error::Exec(m) => write!(f, "error: {m}"),
            Error::Storage(m) => write!(f, "storage error: {m}"),
            Error::Deadlock => write!(f, "deadlock detectado: transação abortada"),
            Error::LockTimeout => write!(f, "tempo esgotado esperando lock: transação abortada"),
            Error::QueueTimeout => write!(f, "tempo esgotado na fila de admissão: servidor sobrecarregado"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
