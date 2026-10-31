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
    /// The diagnostic code and site, when a rule named them —
    /// boxed, so the error stays small on the parser's deep stacks
    /// ([`QuarbError::code`], [`QuarbError::site`]).
    diag: Option<Box<DiagInfo>>,
}

/// The code a rule named and the site that raised it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct DiagInfo {
    code: Option<String>,
    site: Option<String>,
}

impl QuarbError {
    fn new(kind: Kind, message: String) -> QuarbError {
        QuarbError {
            kind,
            message,
            hint: None,
            at: None,
            source: None,
            diag: None,
        }
    }

    /// With the diagnostic code a rule names (`QUANTIFIER_BOUND`).
    pub fn with_code(mut self, code: impl Into<String>) -> QuarbError {
        self.diag.get_or_insert_with(Default::default).code = Some(code.into());
        self
    }

    /// With the site that raised it (a command, a hop, a round, a
    /// mount, the query).
    pub fn with_site(mut self, site: impl Into<String>) -> QuarbError {
        self.diag.get_or_insert_with(Default::default).site = Some(site.into());
        self
    }

    /// The diagnostic code (spec: Diagnostics): the one the rule
    /// named, else the kind's — `SYNTAX` for a lex or parse error,
    /// `UNSUPPORTED`, `EXPANSION`, `REFUSED` for a refusal no rule
    /// has named yet.
    pub fn code(&self) -> &str {
        match self.diag.as_ref().and_then(|d| d.code.as_deref()) {
            Some(c) => c,
            None => match self.kind {
                Kind::Lex | Kind::Parse => crate::diagnostic::SYNTAX,
                Kind::Unsupported => "UNSUPPORTED",
                Kind::Expansion => "EXPANSION",
                Kind::Refused => crate::diagnostic::REFUSED,
            },
        }
    }

    /// The site that raised it: the one recorded, else `query`.
    pub fn site(&self) -> &str {
        self.diag
            .as_ref()
            .and_then(|d| d.site.as_deref())
            .unwrap_or("query")
    }

    /// The error as a diagnostic of severity `error`: its code, its
    /// site and the displayed message (banner, hint and position
    /// included, as a host prints it).
    pub fn diagnostic(&self) -> crate::diagnostic::Diagnostic {
        crate::diagnostic::Diagnostic::error(self.code(), self.site(), self.to_string())
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
    /// No chain: the display already carries the error inside (an
    /// expansion's), so a host printing the chain (`{:#}` on an
    /// `anyhow::Error` holding this one) does not say it twice;
    /// [`QuarbError::inner`] reaches it.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
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
