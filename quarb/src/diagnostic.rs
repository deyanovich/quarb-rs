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
/// the message, and the site that raised it (a command, a step, a
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
        let code = code.into();
        // a code of the inventory is raised under a severity it has
        // a row for: a new pair is a row to ask the language
        // repository for, found here by any test that raises it
        debug_assert!(
            {
                let rows = rows_of(&code);
                rows.is_empty() || rows.iter().any(|r| r.severity == severity)
            },
            "{code} is raised as {severity}, and langspec/diagnostics.tsv has no such row"
        );
        Diagnostic {
            severity,
            code,
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

    /// The line a terminal host prints: `SEVERITY CODE (site): message`,
    /// and after it the tail of a code that has a remedy: `Narrow
    /// first: qua man SORT_NOT_PUSHED`.
    pub fn line(&self) -> String {
        let head = self.head();
        match self.tail() {
            Some(tail) if self.message.ends_with('.') => format!("{head} {tail}"),
            Some(tail) => format!("{head}. {tail}"),
            None => head,
        }
    }

    /// The line without its tail: `SEVERITY CODE (site): message`.
    pub fn head(&self) -> String {
        format!(
            "{} {} ({}): {}",
            self.severity, self.code, self.site, self.message
        )
    }

    /// Where the remedy is read: the lead of the code's row in the
    /// inventory and the door to its page (`Narrow first: qua man
    /// SORT_NOT_PUSHED`; ruling 2026-11-17). `None` for a code whose
    /// row has no lead, and for one the inventory does not hold.
    pub fn tail(&self) -> Option<String> {
        let row = row_of(&self.code, self.severity)?;
        (!row.lead.is_empty()).then(|| format!("{}: {} {}", row.lead, manual_door(), self.code))
    }
}

/// The inventory of the codes: `langspec/diagnostics.tsv` of the
/// language repository, vendored beside the command inventory
/// (`scripts/vendor-commands.sh`). One row a code and severity: its
/// family, the class of its remedy, when it is raised and what to
/// do about it, in English and in Russian. `man CODE` prints the
/// row, a line's tail names the door to it, and a test holds the
/// codes below and the rows equal both ways.
const INVENTORY: &str = include_str!("syntax/diagnostics.tsv");

/// One row of the inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub code: &'static str,
    pub severity: Severity,
    /// `spec` (the specification names it), `planner` (a part of a
    /// query a source was not handed), `bridge` (a translator's
    /// refusal) or `engine` (the implementation's own).
    pub family: &'static str,
    /// The class of the remedy: `respell`, `narrow`, `choose`,
    /// `index`, `lenient` or `none`.
    pub remedy: &'static str,
    /// The words of a line's tail before the door (`Narrow first`);
    /// empty where the code has no tail.
    pub lead: &'static str,
    /// When the code is raised.
    pub raised_en: &'static str,
    pub raised_ru: &'static str,
    /// What to do about it; empty where there is nothing to do.
    pub remedy_en: &'static str,
    pub remedy_ru: &'static str,
}

/// The rows of the inventory, in its order.
pub fn inventory() -> &'static [Row] {
    static ROWS: std::sync::OnceLock<Vec<Row>> = std::sync::OnceLock::new();
    ROWS.get_or_init(|| parse_inventory(INVENTORY))
}

/// The columns are read by the names of the header line (the
/// comment that opens `# code`), so the language repository may
/// add one or move one.
fn parse_inventory(text: &'static str) -> Vec<Row> {
    let header: Vec<&str> = text
        .lines()
        .find(|l| l.starts_with("# code\t"))
        .map(|l| l.trim_start_matches("# ").split('\t').collect())
        .unwrap_or_default();
    let at = |name: &str| header.iter().position(|h| *h == name);
    let (code, severity, family, remedy) = (at("code"), at("severity"), at("family"), at("remedy"));
    let (lead, raised_en, raised_ru) = (at("lead_en"), at("raised_en"), at("raised_ru"));
    let (remedy_en, remedy_ru) = (at("remedy_en"), at("remedy_ru"));
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let cells: Vec<&'static str> = l.split('\t').collect();
            let cell = |i: Option<usize>| i.and_then(|i| cells.get(i).copied()).unwrap_or("");
            Some(Row {
                code: cell(code),
                severity: cell(severity).parse().ok()?,
                family: cell(family),
                remedy: cell(remedy),
                lead: cell(lead),
                raised_en: cell(raised_en),
                raised_ru: cell(raised_ru),
                remedy_en: cell(remedy_en),
                remedy_ru: cell(remedy_ru),
            })
        })
        .collect()
}

/// The rows of one code: one, or two where a code is raised under
/// two severities (`OMITTED`).
pub fn rows_of(code: &str) -> Vec<&'static Row> {
    inventory().iter().filter(|r| r.code == code).collect()
}

/// The row of a code under a severity, or its first row.
pub fn row_of(code: &str, severity: Severity) -> Option<&'static Row> {
    let rows = rows_of(code);
    rows.iter()
        .find(|r| r.severity == severity)
        .or(rows.first())
        .copied()
}

/// A translator's refusal as the line a tool prints for it: `error
/// SORT_NOT_TRANSLATED (jq): …`, the bridge the site. The tail of a
/// row whose remedy is the flag (`Or pass --lenient: …`) is printed
/// only where the flag helps: where the lenient translator takes
/// what was refused (`lenient_takes`) and is not the one that
/// refused it (`lenient_on`).
pub fn translator_refusal(
    bridge: &str,
    code: &str,
    reason: &str,
    lenient_takes: bool,
    lenient_on: bool,
) -> String {
    let d = Diagnostic::error(code, bridge, reason);
    let by_flag = row_of(code, Severity::Error).is_some_and(|r| r.remedy == "lenient");
    if by_flag && (!lenient_takes || lenient_on) {
        d.head()
    } else {
        d.line()
    }
}

static MANUAL_DOOR: std::sync::RwLock<&'static str> = std::sync::RwLock::new("qua man");

/// The words a line's tail opens the manual with: `qua man` from
/// the command line (the default), `man` where the manual is a line
/// of the session (quai, the browser hosts).
pub fn set_manual_door(door: &'static str) {
    if let Ok(mut d) = MANUAL_DOOR.write() {
        *d = door;
    }
}

fn manual_door() -> &'static str {
    MANUAL_DOOR.read().map(|d| *d).unwrap_or("qua man")
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.line())
    }
}

/// The codes, each a constant of its own name, and [`CODES`], the
/// list of them all: what the test holds against the inventory.
macro_rules! codes {
    ($($(#[$doc:meta])* $name:ident),* $(,)?) => {
        $($(#[$doc])* pub const $name: &str = stringify!($name);)*
        /// Every code the engine, its tools and its bridges raise.
        pub const CODES: &[&str] = &[$($name),*];
    };
}

codes! {
    /// The code of a refusal no rule has named yet.
    REFUSED,
    /// Data the source withholds from the caller (spec: omitted data): a
    /// property, or whole nodes; a warning once per query where data was
    /// omitted, an error where the query keyed on an omitted property.
    OMITTED,
    /// A query text the lexer or the parser could not read.
    SYNTAX,
    /// An open walk or loop that reached the quantifier bound with
    /// rounds still to take, or an explicit count beyond it.
    QUANTIFIER_BOUND,
    /// An exact walk whose expansion paths in flight exceeded the
    /// frontier bound.
    FRONTIER_BOUND,
    /// Debug: the walk engine's account of one round.
    WALK,
    /// Lone values of a kind a numeric command does not take (text with
    /// no numeric reading under `sqrt`, a boolean under `round`): each
    /// answered null; one warning per command, counting them.
    NOT_NUMERIC,
    /// Warning: a step names a label that the surface its source
    /// declares lacks (a table's keys); the labels there are named. A
    /// source that declares nothing raises nothing (ruling 2026-11-10).
    UNKNOWN_LABEL,
    /// Warning: a projection or a predicate names a property that the
    /// surface its source declares lacks (a table's columns, a CSV's
    /// header); the properties there are named.
    UNKNOWN_PROPERTY,
    /// Warning: the query read a field by name on records, and no record
    /// it read under that name has it (ruling 2026-11-11): the answer is
    /// null as ever, and the fields the records have are named. Quiet
    /// where one record read under the name has the field. One warning
    /// per name, when the run ends.
    UNKNOWN_FIELD,
    /// Warning: a worksheet of a workbook could not be read and is left
    /// out of the mount; the sheet and the reader's error are named
    /// (ruling 2026-11-13: a table missing from a file the user mounted
    /// is told, as a record's missing field is). Raised once per sheet,
    /// at the mount, and told with the first run. Sheets that hold no
    /// table (a chart sheet, a macro sheet) are left out without it.
    UNREADABLE_SHEET,
    /// Warning: a plain correlation (`<=>`, `<=>?`) bound the first of
    /// several satisfying candidates for some driver, and the query
    /// reads that witness: the answer depends on which one was bound.
    /// Quiet where the join only filters, and under `<=>*`, which binds
    /// every candidate (ruling 2026-11-11).
    SEVERAL_WITNESSES,
    /// Info: two indexes met and the labels of a side were cast to the
    /// type they agreed on (a declared index won, two undeclared were
    /// upcast to their common type, an integer met a float).
    INDEX_CAST,
    /// Info: the run drew its random seed (none was pinned), named so
    /// that the run can be repeated.
    RANDOM_SEED,
    /// Info: a transposition brought the cells of columns of several
    /// kinds to their common type.
    TRANSPOSE_CAST,
    /// An induced subgraph a graph library command was asked about
    /// that exceeded the graph bound.
    GRAPH_BOUND,
    /// Warning: a path after an aggregation pipe went on from the
    /// root, the context standing on no node (ruling 2026-11-06).
    NAVIGATION_RESTARTED,
    /// The error type's own: a construct the grammar reads and the
    /// engine does not run yet.
    UNSUPPORTED,
    /// The error type's own: a macro or a template that failed while
    /// it expanded, wrapping the error inside it.
    EXPANSION,
    /// Info: a part of the query a source answered in its own
    /// language, or a library command answered from a declared graph.
    PUSHED,
    /// Info: a part of the query a source answered natively.
    DELEGATED,
    /// Info: the locale in force.
    LOCALE,
    /// Warning: a locale tag the implementation has no rules for.
    LOCALE_UNKNOWN,
    /// Info: the units a header's suffixes gave a table's columns.
    AUTOUNITS,
    /// Info: what a correlation of two tables did with its keys.
    CORRELATE,
    /// Warning: a column of a table fell to text.
    TABLE_TEXT,
    /// Info: a step read a table by a column no index leads
    /// (quarb-relational).
    UNINDEXED,
    /// Warning: under `--lenient` a translator took the nearest
    /// Quarb construct for one with no exact counterpart.
    LENIENT_TRANSLATION,
    /// Info: an external reference the run left unresolved (qua).
    UNRESOLVED,
    /// Info: a file under a folder or an archive that a reading
    /// could not read; its fields answer null (qua).
    READING,
    /// Info or warning: what `--copy` put on the clipboard (qua).
    COPIED,
    /// Debug: the plan a source was handed (qua).
    PLAN,
    /// Debug: the adapter a query ran over (qua).
    MOUNT,
    /// The browser builds' code of a failure no rule has named.
    ERROR,

    // The planner's words (ruling 2026-11-17: one word per reason,
    // where `NOT_PUSHED` was the one kind): the part of a query a
    // source was not handed, named by what the user wrote. Info.
    /// A sort: a database orders text by its own collation.
    SORT_NOT_PUSHED,
    /// A fold, or a command over its groups.
    FOLD_NOT_PUSHED,
    /// `unique`: DISTINCT returns rows in no stated order.
    UNIQUE_NOT_PUSHED,
    /// A selection by position.
    TAKE_NOT_PUSHED,
    /// A negation: SQL drops the rows whose column is empty.
    NEGATION_NOT_PUSHED,
    /// A regular expression or a substring test.
    PATTERN_NOT_PUSHED,
    /// A condition that follows a reference or navigates.
    RESOLUTION_NOT_PUSHED,
    /// Navigation beyond a table's rows.
    WALK_NOT_PUSHED,
    /// A read of the query's own state: a register, the topic, a
    /// capture, a pushed value.
    STATE_NOT_PUSHED,
    /// A correlation in a form with no JOIN.
    JOIN_NOT_PUSHED,
    /// A reducer over row nodes, or one SQL has no aggregate for.
    REDUCER_NOT_PUSHED,
    /// A comparison or a reducer that would follow the column's
    /// declared type where the engine follows each value.
    TYPE_NOT_PUSHED,
    /// A union of branches.
    UNION_NOT_PUSHED,
    /// An operator, a command or a projection with no SQL form.
    OPERATOR_NOT_PUSHED,
    /// A table or a column name that is no plain SQL identifier.
    NAME_NOT_PUSHED,
    /// A condition on a JSON column written by children, under the
    /// property reading of the mount's grafts.
    GRAFT_NOT_PUSHED,
    /// Pushdown is off for the run: a flag, a model, a plan, a
    /// budget, a standing session.
    OPTION_NOT_PUSHED,
    /// The database refused the statement the planner sent.
    STATEMENT_NOT_PUSHED,
    /// A step of a declared graph its store did not answer.
    GRAPH_NOT_PUSHED,
    /// A leading condition the database could take that was not
    /// sent ahead of the scan: what follows would see the difference
    /// (a step that could reach the filtered table, metadata that
    /// would observe the filtering, a table reached twice).
    CONDITION_NOT_PUSHED,

    // The bridges' words: a construct of jq, XPath or SQL with no
    // Quarb translation. Errors, the bridge as the site.
    /// jq: a construction, an assignment, an interpolation, a
    /// variable; SQL: an OFFSET, a subquery, a window function.
    CONSTRUCT_NOT_TRANSLATED,
    /// jq: a recursive descent.
    DESCENT_NOT_TRANSLATED,
    /// jq: a slice.
    SLICE_NOT_TRANSLATED,
    /// jq: values built or iterated in a form with no translation.
    VALUES_NOT_TRANSLATED,
    /// jq: a `map`.
    MAP_NOT_TRANSLATED,
    /// jq, XPath: a function.
    FUNCTION_NOT_TRANSLATED,
    /// jq, SQL: a sort (`--lenient` translates jq's by-key ones).
    SORT_NOT_TRANSLATED,
    /// jq, XPath: a literal.
    LITERAL_NOT_TRANSLATED,
    /// XPath: a path (`--lenient` reads a relative one from the root).
    PATH_NOT_TRANSLATED,
    /// XPath: an axis.
    AXIS_NOT_TRANSLATED,
    /// XPath: a node test (`--lenient` reads `node()` as the elements).
    NODE_TEST_NOT_TRANSLATED,
    /// XPath: an attribute form.
    ATTRIBUTE_NOT_TRANSLATED,
    /// XPath: a position.
    POSITION_NOT_TRANSLATED,
    /// SQL: a statement that is no translatable SELECT.
    STATEMENT_NOT_TRANSLATED,
    /// SQL: a JOIN.
    JOIN_NOT_TRANSLATED,
    /// SQL: a DISTINCT.
    DISTINCT_NOT_TRANSLATED,
    /// SQL: a GROUP BY.
    GROUP_NOT_TRANSLATED,
    /// SQL: a HAVING.
    HAVING_NOT_TRANSLATED,
    /// SQL: a UNION outside two single columns.
    UNION_NOT_TRANSLATED,
    /// SQL: a comparison with the NULL literal, which is never true.
    NULL_NOT_TRANSLATED,
    /// SQL: an identifier or an alias that is no plain name.
    NAME_NOT_TRANSLATED,
    /// SQL: an aggregate.
    AGGREGATE_NOT_TRANSLATED,
}

/// The one wording of [`OMITTED`], error and warning, naming no
/// field, no node and no count.
pub const OMITTED_TEXT: &str = "some data is omitted due to insufficient permissions";

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
        let d = Diagnostic::error(SYNTAX, "query", "an open bracket");
        assert_eq!(d.to_string(), "error SYNTAX (query): an open bracket");
    }

    /// A code whose row has a lead ends its line with the door to
    /// its page; the door is the host's.
    #[test]
    fn a_line_ends_with_the_door_to_its_remedy() {
        let d = Diagnostic::info(
            SORT_NOT_PUSHED,
            "query",
            "ORDER BY collations differ per engine; scanning",
        );
        assert_eq!(
            d.to_string(),
            "info SORT_NOT_PUSHED (query): ORDER BY collations differ per engine; scanning. \
             Narrow first: qua man SORT_NOT_PUSHED"
        );
        // a row with no lead has no tail, and a code with no row none
        assert_eq!(Diagnostic::info(LOCALE, "run", "C").tail(), None);
        assert_eq!(Diagnostic::info("NO_SUCH_CODE", "run", "x").tail(), None);
    }

    /// The codes and the inventory's rows are the same set: a code
    /// with no row fails here, and a row no code raises.
    #[test]
    fn every_code_has_a_row_and_every_row_a_code() {
        let rows: std::collections::BTreeSet<&str> = inventory().iter().map(|r| r.code).collect();
        let codes: std::collections::BTreeSet<&str> = CODES.iter().copied().collect();
        assert_eq!(codes.len(), CODES.len(), "a code listed twice");
        let unlisted: Vec<&&str> = codes.difference(&rows).collect();
        let unraised: Vec<&&str> = rows.difference(&codes).collect();
        assert!(
            unlisted.is_empty() && unraised.is_empty(),
            "codes with no row in langspec/diagnostics.tsv: {unlisted:?}; rows no code raises: \
             {unraised:?}"
        );
        // every row is whole: a severity, a family, a sentence
        for r in inventory() {
            assert!(
                !r.family.is_empty() && !r.raised_en.is_empty() && !r.raised_ru.is_empty(),
                "{r:?}"
            );
            assert_eq!(
                r.lead.is_empty(),
                r.remedy == "none",
                "a lead goes with a remedy: {r:?}"
            );
        }
    }
}
