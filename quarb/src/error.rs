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

    /// A query the engine refuses to answer because the result
    /// would be silently wrong or incomplete — the honest
    /// alternative to returning a truncated answer (the same
    /// doctrine as pushdown's refuse-or-fall-back, but with
    /// nothing to fall back to).
    #[error("refused: {0}")]
    Refused(String),
}

/// Convenience alias for engine results.
pub type Result<T> = std::result::Result<T, QuarbError>;
