use std::{fmt, io};

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Json(serde_json::Error),
    Index(String),
    Parse(String),
    Graph(String),
    Ledger(String),
    InvalidProject(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Json(error) => write!(f, "index serialization error: {error}"),
            Self::Index(message) => write!(f, "index error: {message}"),
            Self::Parse(message) => write!(f, "parse error: {message}"),
            Self::Graph(message) => write!(f, "invalid graph: {message}"),
            Self::Ledger(message) => write!(f, "ledger error: {message}"),
            Self::InvalidProject(message) => write!(f, "invalid project: {message}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
