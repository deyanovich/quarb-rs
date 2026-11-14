//! The diagnostics channel (spec: Execution Model, Diagnostics): a
//! query's evaluation yields its answer and its diagnostics, each a
//! severity, a code, a message and the site that raised it. An error
//! withholds the answer; a warning and an info accompany it. A
//! diagnostic belongs to the query that raised it, as the answer
//! does: every host delivers a query's diagnostics with that query's
//! answer, in raising order. The level a host shows (error, warning,
//! info) changes what is shown, never what is raised.

use std::fmt;

/// The logging triple. A refusal is not a severity but the class of
/// error whose code names the rule under which a well-formed query
/// is declined, as against `SYNTAX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Shown at every level.
    Error,
    /// Shown at `warning` and `info`.
    Warning,
    /// Shown at `info` and `debug`.
    Info,
    /// Shown at `debug` only: what the evaluation did on the way,
    /// round by round or mount by mount, for whoever works on the
    /// implementation or on a query that puzzles them; its content
    /// is the implementation's and need not keep still between
    /// releases. Raised only when the level asks for it.
    Debug,
}

impl Severity {
    /// The word a host prints and a switch takes.
    pub fn word(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Debug => "debug",
        }
    }

    /// Whether a diagnostic of this severity is shown at `level`:
    /// `error` shows errors, `warning` errors and warnings, `info`
    /// the infos too, `debug` everything.
    pub fn shown_at(self, level: Severity) -> bool {
        self <= level
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())
    }
}

impl std::str::FromStr for Severity {
    type Err = String;

    fn from_str(s: &str) -> Result<Severity, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "error" | "errors" => Ok(Severity::Error),
            "warning" | "warnings" | "warn" => Ok(Severity::Warning),
            "info" | "infos" => Ok(Severity::Info),
            "debug" | "all" => Ok(Severity::Debug),
            other => Err(format!(
                "'{other}' is no diagnostics level: error, warning, info or debug"
            )),
        }
    }
}

/// One diagnostic: its severity, a short code in capitals naming
/// the rule or the event (`QUANTIFIER_BOUND`, `SYNTAX`, `PUSHED`),
/// the message, and the site that raised it (a command, a hop, a
/// round, a mount, the query).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    pub site: String,
}

impl Diagnostic {
    pub fn new(
        severity: Severity,
        code: impl Into<String>,
        site: impl Into<String>,
        message: impl Into<String>,
    ) -> Diagnostic {
        Diagnostic {
            severity,
            code: code.into(),
            message: message.into(),
            site: site.into(),
        }
    }

    pub fn error(
        code: impl Into<String>,
        site: impl Into<String>,
        message: impl Into<String>,
    ) -> Diagnostic {
        Diagnostic::new(Severity::Error, code, site, message)
    }

    pub fn warning(
        code: impl Into<String>,
        site: impl Into<String>,
        message: impl Into<String>,
    ) -> Diagnostic {
        Diagnostic::new(Severity::Warning, code, site, message)
    }

    pub fn info(
        code: impl Into<String>,
        site: impl Into<String>,
        message: impl Into<String>,
    ) -> Diagnostic {
        Diagnostic::new(Severity::Info, code, site, message)
    }

    /// The line a terminal host prints: `SEVERITY CODE (site): message`.
    pub fn line(&self) -> String {
        format!(
            "{} {} ({}): {}",
            self.severity, self.code, self.site, self.message
        )
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.line())
    }
}

/// The code of a refusal no rule has named yet.
pub const REFUSED: &str = "REFUSED";
/// Data the source withholds from the caller (spec: omitted data): a
/// property, or whole nodes; a warning once per query where data was
/// omitted, an error where the query keyed on an omitted property.
pub const OMITTED: &str = "OMITTED";
/// The one wording of both, naming no field, no node and no count.
pub const OMITTED_TEXT: &str = "some data is omitted due to insufficient permissions";
/// A query text the lexer or the parser could not read.
pub const SYNTAX: &str = "SYNTAX";
/// An open walk or loop that reached the quantifier bound with
/// rounds still to take, or an explicit count beyond it.
pub const QUANTIFIER_BOUND: &str = "QUANTIFIER_BOUND";
/// An exact walk whose expansion paths in flight exceeded the
/// frontier bound.
pub const FRONTIER_BOUND: &str = "FRONTIER_BOUND";
/// Debug: the walk engine's account of one round.
pub const WALK: &str = "WALK";
/// Lone values of a kind a numeric command does not take (text with
/// no numeric reading under `sqrt`, a boolean under `round`): each
/// answered null; one warning per command, counting them.
pub const NOT_NUMERIC: &str = "NOT_NUMERIC";
/// Warning: a hop names a label that the surface its source
/// declares lacks (a table's keys); the labels there are named. A
/// source that declares nothing raises nothing (ruling 2026-11-10).
pub const UNKNOWN_LABEL: &str = "UNKNOWN_LABEL";
/// Warning: a projection or a predicate names a property that the
/// surface its source declares lacks (a table's columns, a CSV's
/// header); the properties there are named.
pub const UNKNOWN_PROPERTY: &str = "UNKNOWN_PROPERTY";
/// Warning: the query read a field by name on records, and no record
/// it read under that name has it (ruling 2026-11-11): the answer is
/// null as ever, and the fields the records have are named. Quiet
/// where one record read under the name has the field. One warning
/// per name, when the run ends.
pub const UNKNOWN_FIELD: &str = "UNKNOWN_FIELD";
/// Warning: a worksheet of a workbook could not be read and is left
/// out of the mount; the sheet and the reader's error are named
/// (ruling 2026-11-13: a table missing from a file the user mounted
/// is told, as a record's missing field is). Raised once per sheet,
/// at the mount, and told with the first run. Sheets that hold no
/// table (a chart sheet, a macro sheet) are left out without it.
pub const UNREADABLE_SHEET: &str = "UNREADABLE_SHEET";
/// Warning: a plain correlation (`<=>`, `<=>?`) bound the first of
/// several satisfying candidates for some driver, and the query
/// reads that witness: the answer depends on which one was bound.
/// Quiet where the join only filters, and under `<=>*`, which binds
/// every candidate (ruling 2026-11-11).
pub const SEVERAL_WITNESSES: &str = "SEVERAL_WITNESSES";
/// Info: two indexes met and the labels of a side were cast to the
/// type they agreed on (a declared index won, two undeclared were
/// upcast to their common type, an integer met a float).
pub const INDEX_CAST: &str = "INDEX_CAST";
/// Info: a transposition brought the cells of columns of several
/// kinds to their common type.
pub const TRANSPOSE_CAST: &str = "TRANSPOSE_CAST";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_and_lines() {
        assert!(Severity::Error.shown_at(Severity::Error));
        assert!(!Severity::Warning.shown_at(Severity::Error));
        assert!(Severity::Warning.shown_at(Severity::Warning));
        assert!(Severity::Info.shown_at(Severity::Info));
        assert!(!Severity::Info.shown_at(Severity::Warning));
        assert!(Severity::Debug.shown_at(Severity::Debug));
        assert!(!Severity::Debug.shown_at(Severity::Info));
        assert_eq!("debug".parse::<Severity>().unwrap(), Severity::Debug);
        assert_eq!("warn".parse::<Severity>().unwrap(), Severity::Warning);
        assert!("loud".parse::<Severity>().is_err());
        let d = Diagnostic::error(QUANTIFIER_BOUND, "round 32", "edges still to take");
        assert_eq!(
            d.to_string(),
            "error QUANTIFIER_BOUND (round 32): edges still to take"
        );
    }
}
