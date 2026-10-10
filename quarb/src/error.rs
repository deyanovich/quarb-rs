//! Engine error type.

use std::fmt;

/// What went wrong, by phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A character or token could not be lexed.
    Lex,
    /// The token stream is not a valid query.
    Parse,
    /// A construct that is valid Quarb but not yet implemented.
    Unsupported,
    /// A failure in the macro-expansion phase — the body ran or
    /// its output re-parsed and something went wrong there, as
    /// distinct from reading the query text itself. The inner
    /// error keeps its own banner ([`QuarbError::source`]); this
    /// kind names the phase.
    Expansion,
    /// A query the engine refuses to answer because the result
    /// would be silently wrong or incomplete — the alternative to
    /// returning a truncated answer (the same doctrine as
    /// pushdown's refuse-or-fall-back, but with nothing to fall
    /// back to).
    Refused,
}

impl Kind {
    /// The banner the message prints under.
    fn banner(self) -> &'static str {
        match self {
            Kind::Lex => "lex error",
            Kind::Parse => "parse error",
            Kind::Unsupported => "not yet supported",
            Kind::Expansion => "expansion error",
            Kind::Refused => "refused",
        }
    }
}

/// An error raised while lexing, parsing, or executing a query:
/// its kind, the message, and — when the engine knows them — the
/// way out, where in the query text it was found, and the error
/// inside (an expansion's).
///
/// Displayed as `BANNER: MESSAGE[HINT][: SOURCE][ (at character N)]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarbError {
    kind: Kind,
    message: String,
    hint: Option<String>,
    at: Option<usize>,
    source: Option<Box<QuarbError>>,
}

impl QuarbError {
    fn new(kind: Kind, message: String) -> QuarbError {
        QuarbError {
            kind,
            message,
            hint: None,
            at: None,
            source: None,
        }
    }

    /// A lexer error.
    pub fn lex(message: impl Into<String>) -> QuarbError {
        QuarbError::new(Kind::Lex, message.into())
    }

    /// A parse error.
    pub fn parse(message: impl Into<String>) -> QuarbError {
        QuarbError::new(Kind::Parse, message.into())
    }

    /// A construct the engine does not implement yet.
    pub fn unsupported(message: impl Into<String>) -> QuarbError {
        QuarbError::new(Kind::Unsupported, message.into())
    }

    /// An expansion-phase error; the error inside goes on with
    /// [`QuarbError::wrapping`].
    pub fn expansion(message: impl Into<String>) -> QuarbError {
        QuarbError::new(Kind::Expansion, message.into())
    }

    /// A refusal.
    pub fn refused(message: impl Into<String>) -> QuarbError {
        QuarbError::new(Kind::Refused, message.into())
    }

    /// With the way out, printed right after the message.
    pub fn with_hint(mut self, hint: impl Into<String>) -> QuarbError {
        self.hint = Some(hint.into());
        self
    }

    /// With the character offset (zero-based) in the query text
    /// where the engine stopped.
    pub fn at(mut self, offset: usize) -> QuarbError {
        self.at = Some(offset);
        self
    }

    /// With the error inside (an expansion's re-parse or run).
    pub fn wrapping(mut self, inner: QuarbError) -> QuarbError {
        self.source = Some(Box::new(inner));
        self
    }

    /// The phase.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The message, without the banner, hint, position or source.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The way out, if the engine named one.
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    /// The character offset (zero-based) in the query text where
    /// the engine stopped, when it knows.
    pub fn offset(&self) -> Option<usize> {
        self.at
    }

    /// The error inside, for an expansion.
    pub fn inner(&self) -> Option<&QuarbError> {
        self.source.as_deref()
    }
}

impl fmt::Display for QuarbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.banner(), self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, "{hint}")?;
        }
        if let Some(inner) = &self.source {
            write!(f, ": {inner}")?;
        }
        if let Some(at) = self.at {
            write!(f, " (at character {})", at + 1)?;
        }
        Ok(())
    }
}

impl std::error::Error for QuarbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|e| e as _)
    }
}

/// Convenience alias for engine results.
pub type Result<T> = std::result::Result<T, QuarbError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_reproduces_the_banners() {
        assert_eq!(QuarbError::parse("x").to_string(), "parse error: x");
        assert_eq!(
            QuarbError::parse("x").at(4).to_string(),
            "parse error: x (at character 5)"
        );
        assert_eq!(
            QuarbError::unsupported("pipeline command 'f'")
                .with_hint(" ('f' uses '@|')")
                .to_string(),
            "not yet supported: pipeline command 'f' ('f' uses '@|')"
        );
        let e =
            QuarbError::expansion("in expansion of '&m' ('/a')").wrapping(QuarbError::lex("bad"));
        assert_eq!(
            e.to_string(),
            "expansion error: in expansion of '&m' ('/a'): lex error: bad"
        );
        assert_eq!(e.inner().map(|i| i.kind()), Some(Kind::Lex));
        assert_eq!(QuarbError::refused("r").to_string(), "refused: r");
    }
}
