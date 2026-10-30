use std::{fmt, io, sync::Arc};

#[derive(Debug, Clone)]
pub enum Error {
    Io(Arc<io::Error>),
    Corruption(String),
    InvalidArgument(String),
    /// first-committer-wins conflict on a transaction commit
    Conflict,
    /// another process (or handle) holds the db directory lock
    Locked,
    /// the engine hit a fatal background/WAL error and is now read-only
    Poisoned(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub(crate) fn corruption(msg: impl Into<String>) -> Self {
        Error::Corruption(msg.into())
    }

    pub(crate) fn invalid(msg: impl Into<String>) -> Self {
        Error::InvalidArgument(msg.into())
    }
}

impl Error {
    pub fn is_conflict(&self) -> bool {
        matches!(self, Error::Conflict)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::Corruption(m) => write!(f, "corruption: {m}"),
            Error::InvalidArgument(m) => write!(f, "invalid argument: {m}"),
            Error::Conflict => write!(f, "transaction conflict"),
            Error::Locked => write!(f, "database is locked"),
            Error::Poisoned(m) => write!(f, "database is read-only after fatal error: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(Arc::new(e))
    }
}
