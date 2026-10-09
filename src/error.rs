use std::fmt;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Parse(String),
    Exec(String),
    Storage(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn parse(msg: impl Into<String>) -> Self {
        Error::Parse(msg.into())
    }
    pub fn exec(msg: impl Into<String>) -> Self {
        Error::Exec(msg.into())
    }
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
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
