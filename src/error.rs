//! The one error type of the crate.

use std::fmt;
use std::io;

/// Why a file could not be read.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Reading from the source failed.
    Io(io::Error),
    /// The bytes are not a valid AlphaQuick file: a bad frame header, a
    /// corrupt LZ4 block, a record whose size does not fit its block, a META
    /// record without its magic.
    Format(String),
    /// The file ends in the middle of a frame, for example after an
    /// interrupted download. Every block before the cut was read correctly.
    Truncated(String),
}

/// `Result` with this crate's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub(crate) fn format(message: impl Into<String>) -> Self {
        Error::Format(message.into())
    }

    pub(crate) fn truncated(message: impl Into<String>) -> Self {
        Error::Truncated(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(error) => write!(f, "read error: {error}"),
            Error::Format(message) => write!(f, "invalid file: {message}"),
            Error::Truncated(message) => write!(f, "truncated file: {message}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::Io(error)
    }
}
