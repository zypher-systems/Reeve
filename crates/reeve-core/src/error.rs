//! Crate-wide error type.

/// Everything that can go wrong in Reeve's core.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Bad or missing configuration.
    #[error("config: {0}")]
    Config(String),
    /// The model provider failed or said no.
    #[error("{0}")]
    Provider(String),
    /// Filesystem or serialization trouble.
    #[error("io: {0}")]
    Io(String),
    /// A spending cap was reached. The work stopped.
    #[error("budget: {0}")]
    Budget(String),
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// Result with [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
