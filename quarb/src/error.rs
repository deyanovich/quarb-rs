//! Engine error type.

use thiserror::Error;

/// Errors raised while lexing, parsing, or executing a query.
#[derive(Debug, Error)]
pub enum QuarbError {
    /// A character or token could not be lexed.
    #[error("lex error: {0}")]
    Lex(String),

    /// The token stream is not a valid query.
    #[error("parse error: {0}")]
    Parse(String),

    /// A construct that is valid Quarb but not yet implemented.
    #[error("not yet supported: {0}")]
    Unsupported(String),
}

/// Convenience alias for engine results.
pub type Result<T> = std::result::Result<T, QuarbError>;
