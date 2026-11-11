//! `| #` on a node that carries the `tabula` trait (spec: The
//! Table): the node read as a table. The text model's lowered table
//! (rows of cells whose lemma is the column) gives a frame keyed by
//! the lemmas; a grid (HTML `tr`/`th`/`td`, TEI `row`/`cell`) gives a
//! frame where the format declares a header (a first row of `th`, a
//! `thead`, cells with `role="label"`) and an array otherwise, a
//! spanned cell repeated across its span; footer rows (`tfoot`) kept
//! out of the body, a `thead`/`tbody`/`tfoot` node readable as its
//! own rows. Cells are handed to the typed build as text, so they
//! read as CSV cells do — a number where the text reads as one, null
//! where empty or a null word — and a column that stays text keeps
//! every cell as written; a footnote mark in a `sup` is dropped, a
//! break is a space. A nested table is refused by its place; the
//! caption stays the node's lemma (the document-tables round, 2026-11-08).

use crate::NodeId;
use crate::graph::adapter::AstAdapter;
use crate::value::{Table, Value};

fn has_trait(adapter: &impl AstAdapter, node: NodeId, t: &str) -> bool {
    adapter.traits(node).iter().any(|x| x == t)
}

fn local_name(adapter: &impl AstAdapter, node: NodeId) -> String {
    adapter
        .name(node)
        .map(|n| n.rsplit(':').next().unwrap_or("").to_ascii_lowercase())
        .unwrap_or_default()
}

fn is_row(adapter: &impl AstAdapter, node: NodeId) -> bool {
    has_trait(adapter, node, "row") || matches!(local_name(adapter, node).as_str(), "tr" | "row")
}

fn is_cell(adapter: &impl AstAdapter, node: NodeId) -> bool {
    has_trait(adapter, node, "cell")
        || matches!(local_name(adapter, node).as_str(), "th" | "td" | "cell")
}

/// The widest column span and the tallest row span a cell is read
/// with, the clamps of the HTML standard that browsers apply. A
/// span is data: `colspan="30000000"` pushed that many cells and
/// aborted the process on the allocation.
const MAX_COLSPAN: usize = 1000;
const MAX_ROWSPAN: usize = 65534;

fn span_of(adapter: &impl AstAdapter, node: NodeId, names: &[&str], max: usize) -> usize {
    names
        .iter()
        .find_map(|n| adapter.property(node, n))
        .and_then(|v| v.to_string().trim().parse::<usize>().ok())
        .filter(|n| *n >= 1)
        .map_or(1, |n| n.min(max))
}

/// How a document's table is read (spec: The Table, `| #` on a
/// node; the document-tables round, rulings 2026-11-08): the cell
/// texts read as missing values, the locale of the number reading,
/// and whether a column of row headers becomes the index.
#[derive(Debug, Clone)]
pub struct ReadOptions {
    /// `#(--null=@("—"; "n/a"))`: the cell texts read as null beside
    /// the empty cell. A document table's default vocabulary is
    /// [`DEFAULT_NULLS`]; a stream table has none unless given.
    pub nulls: Vec<String>,
    /// `#(--locale=de)`: the convention the numbers are written in;
    /// the current rule (grouping commas, a point) when none.
    pub locale: Option<crate::value::number::Decimal>,
    /// When every body row opens with a header cell (`th`), that
    /// column is the index, named by its header label; `-i`
    /// overrides, `-i=` keeps it a column.
    pub th_index: bool,
    /// `#(--autounits)` (every column) or `#(--autounit=:col…)` (the
    /// named ones): a unit the title names at its end read as the
    /// column's unit, the title losing it (ruling 2026-11-09). `None`
    /// off; an empty list every column.
    pub autounits: Option<Vec<String>>,
    /// `#(--slugs)` / `#(--slugs="-")`: the titles slugged, with the
    /// separator.
    pub slugs: Option<String>,
    /// `#(--sci)`, or `?sci` on the mount: the scientific reading of
    /// the cells (ruling 2026-11-10). A bare digit set in superscript
    /// right after a plain number is an exponent, where it is a
    /// footnote mark without it.
    pub sci: bool,
    /// The column-scoped options and the word dictionary (the
    /// header-units round, ruling 2026-11-09).
    pub scopes: ReadScopes,
}

/// The options scoped to a column by name (`--null:Price=…`,
/// `--locale:Draught=de`, `--unit:Weight=_lb`, `--unit:Weight=%(pounds
/// = _lb)`) and the dictionary for every title (`--units=%(…)`): the
/// unscoped option stays the default, a scoped one overrides it for
/// its column.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReadScopes {
    /// Per column title, its null words.
    pub nulls_by: Vec<(String, Vec<String>)>,
    /// Per column title, its number convention.
    pub locale_by: Vec<(String, crate::value::number::Decimal)>,
    /// Word → unit, for every title.
    pub units: Vec<(String, String)>,
    /// Per column title, word → unit for that title alone.
    pub units_by: Vec<(String, Vec<(String, String)>)>,
    /// Per column title, the unit the column is declared in.
    pub declare_by: Vec<(String, String)>,
}

impl ReadScopes {
    pub fn is_empty(&self) -> bool {
        self.nulls_by.is_empty()
            && self.locale_by.is_empty()
            && self.units.is_empty()
            && self.units_by.is_empty()
            && self.declare_by.is_empty()
    }

    /// Every column title the scopes name.
    pub fn columns(&self) -> impl Iterator<Item = &str> {
        self.nulls_by
            .iter()
            .map(|(c, _)| c.as_str())
            .chain(self.locale_by.iter().map(|(c, _)| c.as_str()))
            .chain(self.units_by.iter().map(|(c, _)| c.as_str()))
            .chain(self.declare_by.iter().map(|(c, _)| c.as_str()))
    }

    /// The unit a word maps to for a column: the column's own
    /// dictionary first, then the call's.
    pub fn resolve(&self, column: &str, word: &str) -> Option<String> {
        let word = word.to_lowercase();
        self.units_by
            .iter()
            .filter(|(c, _)| c == column)
            .flat_map(|(_, m)| m.iter())
            .chain(self.units.iter())
            .find(|(w, _)| *w == word)
            .map(|(_, u)| u.clone())
    }

    /// The columns the dictionaries ask to read when `--autounits`
    /// is not written: every column under `--units=%(…)`, the scoped
    /// columns under `--unit:col=%(…)`; `None` when neither stands.
    pub fn implied_autounits(&self) -> Option<Vec<String>> {
        if !self.units.is_empty() {
            return Some(Vec::new());
        }
        if !self.units_by.is_empty() {
            return Some(self.units_by.iter().map(|(c, _)| c.clone()).collect());
        }
        None
    }

    /// The unit a column is declared in, if any.
    pub fn declared(&self, column: &str) -> Option<String> {
        self.declare_by
            .iter()
            .find(|(c, _)| c == column)
            .map(|(_, u)| u.clone())
    }

    /// A text cell of the named column under its scoped null words
    /// and locale: a null word the empty cell, a number respelled
    /// canonical; `None` where the scopes say nothing.
    pub fn normalize(&self, column: &str, text: &str) -> Option<String> {
        if let Some((_, words)) = self.nulls_by.iter().find(|(c, _)| c == column)
            && words.iter().any(|w| w == text)
        {
            return Some(String::new());
        }
        if let Some((_, d)) = self.locale_by.iter().find(|(c, _)| c == column)
            && crate::value::number::read(text, *d).is_some()
            && let Some(c) = crate::value::number::canonical(text, *d)
        {
            return Some(c);
        }
        None
    }

    /// The scopes that name no column of the header, for the refusal.
    pub fn unknown_columns<'a>(&'a self, header: &'a [String]) -> Vec<&'a str> {
        let mut out: Vec<&str> = Vec::new();
        for c in self.columns() {
            if !header.iter().any(|h| h == c) && !out.contains(&c) {
                out.push(c);
            }
        }
        out
    }
}

/// The cell texts a document table reads as missing values by
/// default: the dashes, and the "not available" spellings.
pub const DEFAULT_NULLS: &[&str] = &["—", "–", "-", "n/a", "N/A", "NA"];

impl Default for ReadOptions {
    fn default() -> Self {
        ReadOptions {
            nulls: DEFAULT_NULLS.iter().map(|s| s.to_string()).collect(),
            locale: None,
            th_index: true,
            autounits: None,
            slugs: None,
            sci: false,
            scopes: ReadScopes::default(),
        }
    }
}

/// The adapter of a mount opened under the scientific reading
/// (`?sci`, ruling 2026-11-10): it forwards everything and answers
/// `:::sci` true on every node, so each table the mount builds is
/// read as `#(--sci)` reads it and `clean` over its nodes as
/// `clean(--sci)` does.
pub struct Scientific<A: AstAdapter> {
    pub inner: A,
}

impl<A: AstAdapter> AstAdapter for Scientific<A> {
    crate::forward_ast_adapter!(inner; except metadata);
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        if key == "sci" {
            return Some(Value::Bool(true));
        }
        self.inner.metadata(node, key)
    }
}

/// Whether `node` stands in a mount opened under the scientific
/// reading.
pub(crate) fn scientific_at(adapter: &impl AstAdapter, node: NodeId) -> bool {
    matches!(adapter.metadata(node, "sci"), Some(Value::Bool(true)))
}

/// What a superscript says (ruling 2026-11-10).
#[derive(Debug, PartialEq)]
pub(crate) enum Raised {
    /// A footnote mark: dropped.
    Footnote,
    /// An exponent: its digits, signed where a minus was written.
    Exponent(String),
    /// Neither (`5<sup>th</sup>`): the text as written.
    Other,
}

/// A superscript read against the text it follows. Under either
/// reading: a letter, a symbol and a bracketed number are footnote
/// marks; a signed number, a number after a unit's letter (`5 m²`,
/// `9.8 m/s²`, a title's `(km²)`) and a number after `× 10` are
/// exponents. The one case the text does not settle is a bare number
/// right after a plain number (`10³`): a footnote mark, and an
/// exponent under the scientific reading. A bare number after
/// anything else (a name, a year in a phrase) is a footnote mark.
/// `title` says the text is a column's title, where a unit stands
/// alone or after a bracket, a comma or a slash.
pub(crate) fn raised(before: &str, mark: &str, sci: bool, title: bool) -> Raised {
    let m = mark.trim();
    if m.is_empty() {
        return Raised::Other;
    }
    let inner = m.strip_prefix('[').and_then(|r| r.strip_suffix(']'));
    if let Some(i) = inner {
        return if !i.is_empty()
            && (i.chars().all(|c| c.is_ascii_digit())
                || (i.chars().count() == 1 && i.chars().all(|c| c.is_ascii_lowercase())))
        {
            Raised::Footnote
        } else {
            Raised::Other
        };
    }
    if (m.chars().count() == 1 && m.chars().all(|c| c.is_ascii_lowercase()))
        || m.chars().all(|c| matches!(c, '*' | '†' | '‡' | '§' | '¶'))
    {
        return Raised::Footnote;
    }
    // a signed number is an exponent wherever it stands
    let (sign, digits) = match m.chars().next() {
        Some('-' | '−' | '–') => ("-", &m[m.chars().next().map_or(0, char::len_utf8)..]),
        Some('+') => ("+", &m[1..]),
        _ => ("", m),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Raised::Other;
    }
    if !sign.is_empty() {
        let sign = if sign == "-" { "-" } else { "" };
        return Raised::Exponent(format!("{sign}{digits}"));
    }
    let exponent = || Raised::Exponent(digits.to_string());
    // right after: glued to the text before it
    let Some(last) = before.chars().last() else {
        return Raised::Footnote;
    };
    if last.is_alphabetic() {
        // the unit's letter: the word the superscript is glued to
        // names a unit, in a quantity (`5 m`, `9.8 m/s`) or in a
        // title's unit (`Area (km`, `Density, kg/m`, `km`)
        let start = before
            .char_indices()
            .rev()
            .find(|(_, c)| !c.is_alphabetic())
            .map_or(0, |(i, c)| i + c.len_utf8());
        let (head, word) = before.split_at(start);
        let is_unit = crate::value::quantity::scale_expr(word).is_some();
        // an exponent of one is never written: a 1 is a mark
        if !is_unit || digits == "1" {
            return Raised::Footnote;
        }
        let quantity = crate::value::quantity::parse_unit_text(before.trim()).is_some();
        let titled = title
            && matches!(
                head.trim_end().chars().last(),
                None | Some('(' | '[' | ',' | '/' | '·' | '⋅')
            );
        return if quantity || titled {
            exponent()
        } else {
            Raised::Footnote
        };
    }
    if last.is_ascii_digit() {
        // after `× 10`
        if let Some(rest) = before.strip_suffix("10")
            && matches!(
                rest.trim_end().chars().last(),
                Some('×' | 'x' | 'X' | '·' | '⋅' | '*')
            )
        {
            return exponent();
        }
        // right after a plain number: the scientific reading's case
        let plain = crate::value::number::read(before.trim(), crate::value::number::Decimal::Point)
            .or_else(|| {
                crate::value::number::read(before.trim(), crate::value::number::Decimal::Comma)
            })
            .is_some();
        return if plain && sci {
            exponent()
        } else {
            Raised::Footnote
        };
    }
    Raised::Footnote
}

struct Cell {
    label: Option<String>,
    head: bool,
    /// The cell's text as the table reads it: footnote marks dropped,
    /// a break a space, trimmed.
    text: String,
    colspan: usize,
    rowspan: usize,
}

/// Which rows of a table a node asks for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    /// The table: its body rows under its header.
    Whole,
    /// `thead`: the header rows as rows, no header of their own.
    Head,
    /// `tbody`: the body rows under the table's header.
    Body,
    /// `tfoot`: the footer rows under the table's header.
    Foot,
}

/// A row's place: a header row, a footer row, or a body row.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    Head,
    Body,
    Foot,
}

fn rows_under(
    adapter: &impl AstAdapter,
    node: NodeId,
    root: NodeId,
    rows: &mut Vec<(NodeId, Place)>,
    place: Place,
) -> Result<(), String> {
    for c in adapter.children(node) {
        if c != root && has_trait(adapter, c, "tabula") {
            return Err(
                "a nested table is not read with its outer one: address it on its own ('//*<tabula>')".into(),
            );
        }
        if is_row(adapter, c) {
            // TEI marks the header row itself: `<row role="label">`.
            let labelled = adapter
                .property(c, "role")
                .is_some_and(|v| v.to_string() == "label");
            let p = if place == Place::Body && labelled {
                Place::Head
            } else {
                place
            };
            rows.push((c, p));
        } else {
            let p = match local_name(adapter, c).as_str() {
                "thead" => Place::Head,
                "tfoot" => Place::Foot,
                _ => place,
            };
            rows_under(adapter, c, root, rows, p)?;
        }
    }
    Ok(())
}

/// A trailing `[n]` of a cell's text, as a Markdown cell carries a
/// footnote mark: the text without it.
fn strip_trailing_mark(text: &str) -> &str {
    let t = text.trim_end();
    if let Some(open) = t.rfind('[')
        && t.ends_with(']')
    {
        let inner = &t[open + 1..t.len() - 1];
        if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
            return t[..open].trim_end();
        }
    }
    t
}

/// A cell's text as the table reads it: the lowered table's `body`
/// or the element's text, the lemma prefix dropped, every `sup`
/// child read as what it says ([`raised`]: a footnote mark removed,
/// an exponent written with its caret, `m^2`, `10^-3`), a trailing
/// `[n]` removed, trimmed. A break already reads as a space in the
/// text. (Every superscript that was a number was dropped as a mark:
/// `5 m<sup>2</sup>` read five metres.)
fn cell_text(
    adapter: &impl AstAdapter,
    cell: NodeId,
    label: Option<&str>,
    sci: bool,
    title: bool,
) -> String {
    let mut text = adapter
        .property(cell, "body")
        .or_else(|| adapter.default_value(cell))
        .map(|v| v.to_string())
        .unwrap_or_default();
    if let Some(l) = label
        && let Some(rest) = text.strip_prefix(&format!("{l}: "))
    {
        text = rest.to_string();
    }
    // The superscripts from the last to the first, each found as the
    // last occurrence of its text before the one after it: the cell
    // hands its text whole, and a mark stands at the end of what it
    // marks (`12<sup>2</sup>` is "122", the mark its last 2).
    let sups: Vec<String> = adapter
        .children(cell)
        .into_iter()
        .filter(|&c| local_name(adapter, c) == "sup")
        .filter_map(|c| adapter.default_value(c).map(|v| v.to_string()))
        .filter(|m| !m.is_empty())
        .collect();
    let mut limit = text.len();
    for mark in sups.iter().rev() {
        let Some(at) = text[..limit].rfind(mark.as_str()) else {
            continue;
        };
        match raised(&text[..at], mark, sci, title) {
            Raised::Footnote => text.replace_range(at..at + mark.len(), ""),
            Raised::Exponent(n) => text.replace_range(at..at + mark.len(), &format!("^{n}")),
            Raised::Other => {}
        }
        limit = at;
    }
    strip_trailing_mark(&text).trim().to_string()
}

/// Whether a table stands anywhere below `node` (a nested table in
/// a cell).
fn holds_table(adapter: &impl AstAdapter, node: NodeId) -> bool {
    let mut stack = adapter.children(node);
    while let Some(n) = stack.pop() {
        if has_trait(adapter, n, "tabula") {
            return true;
        }
        stack.extend(adapter.children(n));
    }
    false
}

fn cells_under(
    adapter: &impl AstAdapter,
    node: NodeId,
    out: &mut Vec<Cell>,
    nested: &mut Option<usize>,
    // the scientific reading, and whether the row is a header row
    reading: (bool, bool),
) {
    for c in adapter.children(node) {
        if is_cell(adapter, c) {
            if nested.is_none() && holds_table(adapter, c) {
                *nested = Some(out.len() + 1);
            }
            let name = local_name(adapter, c);
            let label = adapter
                .property(c, "lemma")
                .map(|v| v.to_string())
                .filter(|s| !s.is_empty());
            let role_label = adapter
                .property(c, "role")
                .is_some_and(|v| v.to_string() == "label");
            let head = name == "th" || role_label;
            let (sci, head_row) = reading;
            // a title is a header row's cell, or a head cell
            let text = cell_text(adapter, c, label.as_deref(), sci, head_row || head);
            out.push(Cell {
                label,
                head,
                text,
                colspan: span_of(adapter, c, &["colspan", "cols"], MAX_COLSPAN),
                rowspan: span_of(adapter, c, &["rowspan", "rows"], MAX_ROWSPAN),
            });
        } else if !is_row(adapter, c) {
            cells_under(adapter, c, out, nested, reading);
        }
    }
}

/// A cell's text under the options: a null word is the empty cell;
/// under a locale a number is respelled canonical, as a CSV's
/// `?decimal=` respells it; any other text is itself.
fn normalize(text: &str, opts: &ReadOptions) -> String {
    if opts.nulls.iter().any(|n| n == text) {
        return String::new();
    }
    if let Some(d) = opts.locale
        && crate::value::number::read(text, d).is_some()
        && let Some(c) = crate::value::number::canonical(text, d)
    {
        return c;
    }
    text.to_string()
}

/// The node named for a message: the names from the root, an
/// ordinal where siblings share the name.
fn locate(adapter: &impl AstAdapter, node: NodeId) -> String {
    let root = adapter.root();
    let mut parts = Vec::new();
    let mut cur = Some(node);
    while let Some(n) = cur {
        if n == root || parts.len() > 64 {
            break;
        }
        let name = adapter.name(n).unwrap_or_else(|| "*".to_string());
        let parent = adapter.parent(n);
        let ordinal = parent
            .map(|p| adapter.children_named(p, &name))
            .filter(|sib| sib.len() > 1)
            .and_then(|sib| sib.iter().position(|&s| s == n))
            .map(|k| format!("[{}]", k + 1))
            .unwrap_or_default();
        parts.push(format!("{name}{ordinal}"));
        cur = parent;
    }
    parts.reverse();
    format!("/{}", parts.join("/"))
}

/// Whether `| #` reads `node` as a document's table: a tabula node,
/// or a `thead`/`tbody`/`tfoot` whose parent is one.
pub(crate) fn is_table_or_section(adapter: &impl AstAdapter, node: NodeId) -> bool {
    has_trait(adapter, node, "tabula")
        || (matches!(
            local_name(adapter, node).as_str(),
            "thead" | "tbody" | "tfoot"
        ) && adapter
            .parent(node)
            .is_some_and(|p| has_trait(adapter, p, "tabula")))
}

/// The table a `tabula` node holds — or the rows of its `thead`,
/// `tbody` or `tfoot` — under `opts`. The body rows stand under the
/// header the format declares (a first row of head cells, a
/// `thead`); footer rows are kept out of the body; a nested table
/// refuses by its place; every cell is handed to the typed build as
/// text, so a column of numbers types and a column that stays text
/// keeps each cell as written. A column that falls to text while
/// some cells nearly read as numbers is told as a warning.
pub(crate) fn node_table_with(
    adapter: &impl AstAdapter,
    node: NodeId,
    opts: &ReadOptions,
) -> Result<Table, String> {
    let (table, part) = if has_trait(adapter, node, "tabula") {
        (node, Part::Whole)
    } else {
        let part = match local_name(adapter, node).as_str() {
            "thead" => Some(Part::Head),
            "tbody" => Some(Part::Body),
            "tfoot" => Some(Part::Foot),
            _ => None,
        };
        let holder = adapter
            .parent(node)
            .filter(|&p| has_trait(adapter, p, "tabula"));
        match (part, holder) {
            (Some(part), Some(t)) => (t, part),
            _ => {
                return Err(format!(
                    "'{}' is not a table: '//*<tabula>' finds the tables of a document, '@| #' reads a stream of rows as one",
                    adapter.name(node).unwrap_or_else(|| "the node".into())
                ));
            }
        }
    };
    // the scientific reading: the constructor's option, or the
    // mount's (`?sci`)
    let sci = opts.sci || scientific_at(adapter, table);
    let scientific;
    let opts = if sci && !opts.sci {
        scientific = ReadOptions {
            sci: true,
            ..opts.clone()
        };
        &scientific
    } else {
        opts
    };
    let mut rows: Vec<(NodeId, Place)> = Vec::new();
    rows_under(adapter, table, table, &mut rows, Place::Body)?;
    let mut nested: Option<(usize, usize)> = None;
    let rows: Vec<(Vec<Cell>, Place)> = rows
        .into_iter()
        .enumerate()
        .map(|(i, (r, place))| {
            let mut cells = Vec::new();
            let mut at = None;
            cells_under(adapter, r, &mut cells, &mut at, (sci, place == Place::Head));
            if let Some(col) = at
                && nested.is_none()
            {
                nested = Some((i + 1, col));
            }
            (cells, place)
        })
        .collect();
    if let Some((row, col)) = nested {
        return Err(format!(
            "a nested table at row {row}, column {col} is not read with its outer one: address \
             it on its own ('//*<tabula>')"
        ));
    }
    let site = locate(adapter, table);
    // The lowered text table: every cell carries its column as its
    // lemma, so the frame is keyed by the lemmas in order of first
    // appearance and a missing lemma is a hole.
    if rows
        .iter()
        .any(|(cells, _)| cells.iter().any(|c| c.label.is_some()))
    {
        let mut header: Vec<String> = Vec::new();
        for (cells, _) in &rows {
            for c in cells {
                if let Some(l) = &c.label
                    && !header.contains(l)
                {
                    header.push(l.clone());
                }
            }
        }
        let body: Vec<Vec<String>> = rows
            .iter()
            .filter(|(_, place)| *place != Place::Foot)
            .map(|(cells, _)| {
                let mut row = vec![String::new(); header.len()];
                for c in cells {
                    if let Some(l) = &c.label
                        && let Some(j) = header.iter().position(|h| h == l)
                    {
                        row[j] = normalize(&c.text, opts);
                    }
                }
                row
            })
            .collect();
        return build(Some(header), body, opts, &site, false);
    }
    // A grid: spans repeated, the header where the format declares
    // one (a first row of head cells, or one under `thead`).
    let mut grid: Vec<(Vec<String>, Vec<bool>, Place)> = Vec::new();
    let mut carried: Vec<(usize, String)> = Vec::new(); // per column: rows left, text
    let mut header_rows = 0usize;
    for (i, (cells, place)) in rows.iter().enumerate() {
        let mut row: Vec<String> = Vec::new();
        let mut heads: Vec<bool> = Vec::new();
        let mut spans: Vec<(usize, String)> = Vec::new();
        let mut it = cells.iter();
        let mut next = it.next();
        loop {
            let j = row.len();
            if j < carried.len() && carried[j].0 > 0 {
                carried[j].0 -= 1;
                row.push(carried[j].1.clone());
                heads.push(false);
                spans.push((0, String::new()));
                continue;
            }
            let Some(c) = next else { break };
            for _ in 0..c.colspan {
                row.push(c.text.clone());
                heads.push(c.head);
                spans.push((c.rowspan - 1, c.text.clone()));
            }
            next = it.next();
        }
        // columns a row span still fills past the row's own cells
        while row.len() < carried.len() && carried[row.len()].0 > 0 {
            let j = row.len();
            carried[j].0 -= 1;
            row.push(carried[j].1.clone());
            heads.push(false);
            spans.push((0, String::new()));
        }
        for (j, (left, v)) in spans.into_iter().enumerate() {
            if j >= carried.len() {
                carried.push((0, String::new()));
            }
            if left > 0 {
                carried[j] = (left, v);
            }
        }
        let all_head = !cells.is_empty() && cells.iter().all(|c| c.head);
        if i == header_rows && (all_head || *place == Place::Head) {
            header_rows += 1;
        }
        grid.push((row, heads, *place));
    }
    let width = grid.iter().map(|(r, _, _)| r.len()).max().unwrap_or(0);
    for (r, h, _) in &mut grid {
        r.resize(width, String::new());
        h.resize(width, false);
    }
    let header: Option<Vec<String>> = (header_rows > 0).then(|| grid[header_rows - 1].0.clone());
    let (rows, th_index): (Vec<Vec<String>>, bool) = match part {
        Part::Head => (
            grid.iter()
                .take(header_rows)
                .map(|(r, _, _)| r.clone())
                .collect(),
            false,
        ),
        Part::Whole | Part::Body | Part::Foot => {
            let want = if part == Part::Foot {
                Place::Foot
            } else {
                Place::Body
            };
            let picked: Vec<&(Vec<String>, Vec<bool>, Place)> = grid
                .iter()
                .skip(header_rows)
                .filter(|(_, _, place)| *place == want)
                .collect();
            // every body row opening with a head cell: the row headers
            let th_index = opts.th_index
                && header.is_some()
                && width > 1
                && !picked.is_empty()
                && picked
                    .iter()
                    .all(|(_, h, _)| h.first().copied().unwrap_or(false));
            (
                picked.into_iter().map(|(r, _, _)| r.clone()).collect(),
                th_index,
            )
        }
    };
    let rows: Vec<Vec<String>> = rows
        .into_iter()
        .map(|r| r.into_iter().map(|c| normalize(&c, opts)).collect())
        .collect();
    let header = if part == Part::Head { None } else { header };
    build(header, rows, opts, &site, th_index)
}

/// The typed build over text cells, the row-header index when the
/// rule holds, and the warning per column that fell to text while
/// some of its cells nearly read.
fn build(
    header: Option<Vec<String>>,
    rows: Vec<Vec<String>>,
    opts: &ReadOptions,
    site: &str,
    th_index: bool,
) -> Result<Table, String> {
    // the title rules (the unit at the end of a title, the slug) on
    // the header, before the typed build: a unit column is declared
    // in its unit (the percent one included), the titles are renamed
    // the column-scoped null words and locale first (the header-units
    // round): a scope naming no column refuses
    let rows: Vec<Vec<String>> = match &header {
        Some(h) if !opts.scopes.is_empty() => {
            let unknown = opts.scopes.unknown_columns(h);
            if !unknown.is_empty() {
                return Err(format!(
                    "{} no column: the table's columns are {}",
                    unknown
                        .iter()
                        .map(|c| format!("'{c}' names"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    h.join(", ")
                ));
            }
            rows.into_iter()
                .map(|r| {
                    r.into_iter()
                        .enumerate()
                        .map(|(j, c)| {
                            match h.get(j).and_then(|col| opts.scopes.normalize(col, &c)) {
                                Some(n) => n,
                                None => c,
                            }
                        })
                        .collect()
                })
                .collect()
        }
        _ => rows,
    };
    let autounits: Option<Vec<String>> = opts
        .autounits
        .clone()
        .or_else(|| opts.scopes.implied_autounits());
    let title_rules =
        autounits.is_some() || opts.slugs.is_some() || !opts.scopes.declare_by.is_empty();
    let (header, rows, declared) = match (&header, title_rules) {
        (Some(h), true) => {
            let scopes = &opts.scopes;
            let plan = crate::graph::autounits::title_plan_with(
                h,
                autounits.as_deref(),
                opts.slugs.as_deref(),
                &|j| {
                    rows.iter()
                        .find_map(|r| r.get(j).filter(|c| !c.trim().is_empty()).cloned())
                },
                site,
                &|column, word| scopes.resolve(column, word),
                &|column| scopes.declared(column),
            )?;
            let rows: Vec<Vec<String>> = if plan.factors.iter().all(|f| *f == 1.0) {
                rows
            } else {
                rows.into_iter()
                    .map(|r| {
                        r.into_iter()
                            .enumerate()
                            .map(|(j, c)| match plan.scale_cell(j, Value::Str(c)) {
                                Value::Str(t) => t,
                                other => other.to_string(),
                            })
                            .collect()
                    })
                    .collect()
            };
            (Some(plan.header), rows, plan.declared)
        }
        _ => (header, rows, Vec::new()),
    };
    let mut b = crate::TableBuilder::shaped(header.clone(), declared);
    for r in &rows {
        let refs: Vec<&str> = r.iter().map(String::as_str).collect();
        b.push_text_row(&refs);
    }
    let t = b.finish()?;
    warn_text_columns(&t, &rows, opts, site);
    if th_index && let Some(h) = &header {
        return t.indexed_by(&h[0], None);
    }
    Ok(t)
}

/// Why a cell nearly reads as a number, when it does: the cleanup
/// that would make it one.
fn nearly(text: &str, opts: &ReadOptions) -> Option<&'static str> {
    use crate::value::number::{Decimal, read};
    let t = text.trim();
    if t.is_empty() || read(t, Decimal::Point).is_some() {
        return None;
    }
    if DEFAULT_NULLS.contains(&t) && !opts.nulls.iter().any(|n| n == t) {
        return Some("a null word: --null");
    }
    if strip_trailing_mark(t) != t && read(strip_trailing_mark(t), Decimal::Point).is_some() {
        return Some("a footnote mark");
    }
    if let Some(p) = t.strip_suffix('%')
        && read(p.trim(), Decimal::Point).is_some()
    {
        return Some("a percent");
    }
    if let Some((num, suffix)) = t
        .rsplit_once(|c: char| c.is_ascii_digit())
        .map(|(_, s)| (t[..t.len() - s.len()].to_string(), s))
        && (matches!(suffix.trim(), "k" | "M" | "G" | "m" | "bn" | "mn")
            || (suffix.trim() == "K" && !opts.sci))
        && read(num.trim(), Decimal::Point).is_some()
    {
        return Some("a scale suffix");
    }
    if crate::stdlib::power(t, Decimal::Point).is_some() {
        return Some(if t.contains("10^") {
            "a power of ten"
        } else {
            "a power"
        });
    }
    if read(t, Decimal::Comma).is_some()
        || t.contains(' ') && read(&t.replace(' ', ""), Decimal::Comma).is_some()
    {
        return Some("a number under another locale: --locale");
    }
    if crate::value::quantity::parse_unit_text(t).is_some() {
        return Some("a quantity");
    }
    None
}

/// One warning per column that fell to text while a cell of it is a
/// number or nearly one: the column, up to three cells with their
/// rows and the reason, and the cleanup.
fn warn_text_columns(t: &Table, rows: &[Vec<String>], opts: &ReadOptions, site: &str) {
    use crate::value::ColumnType;
    let types = t.types();
    for (j, ty) in types.iter().enumerate() {
        if *ty != ColumnType::Text {
            continue;
        }
        let mut numeric = 0usize;
        let mut near: Vec<(usize, String, &'static str)> = Vec::new();
        for (i, r) in rows.iter().enumerate() {
            let Some(c) = r.get(j) else { continue };
            if c.trim().is_empty() {
                continue;
            }
            if crate::value::number::read(c, crate::value::number::Decimal::Point).is_some() {
                numeric += 1;
            } else if let Some(why) = nearly(c, opts) {
                near.push((i + 1, c.clone(), why));
            }
        }
        if near.is_empty() || numeric + near.len() < 2 {
            continue;
        }
        let shown: Vec<String> = near
            .iter()
            .take(3)
            .map(|(row, text, why)| format!("row {row} holds \"{text}\" ({why})"))
            .collect();
        let more = if near.len() > 3 {
            format!(" and {} more", near.len() - 3)
        } else {
            String::new()
        };
        let mut cleanup: Vec<String> = Vec::new();
        if near.iter().any(|(_, _, w)| w.starts_with("a null word")) {
            let words: Vec<String> = near
                .iter()
                .filter(|(_, _, w)| w.starts_with("a null word"))
                .map(|(_, c, _)| format!("\"{}\"", c.trim()))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            cleanup.push(format!("#(--null=@({}))", words.join("; ")));
        }
        if near.iter().any(|(_, _, w)| w.contains("--locale")) {
            cleanup.push("#(--locale=…)".to_string());
        }
        if near.iter().any(|(_, _, w)| *w == "a footnote mark") {
            cleanup.push(format!("map(s/\\[\\d+\\]$//; --to=:{})", t.column_name(j)));
        }
        if near.iter().any(|(_, _, w)| {
            matches!(
                *w,
                "a percent" | "a scale suffix" | "a quantity" | "a power of ten" | "a power"
            )
        }) {
            cleanup.push(format!(
                "map(number; --to=:{}) after the suffix is split off",
                t.column_name(j)
            ));
        }
        let msg = format!(
            "column '{}' falls to text: {}{more}; cleanup: | clean (or {})",
            t.column_name(j),
            shown.join(", "),
            cleanup.join(", or ")
        );
        crate::value::ambient::record_diagnostic(crate::Diagnostic::warning(
            "TABLE_TEXT",
            site.to_string(),
            msg,
        ));
    }
}

/// The rows a result tabulates — what a host hands [`table_html`]
/// and [`tabulate`](crate::tabulate) after a run under
/// [`Query::without_closing`](crate::Query::without_closing): values
/// as they are; a node that is a document's own table as its rows'
/// records ([`table_node_records`]); any other node as the record
/// of the columns asked of it (`@| pretty(title; price)` over nodes
/// reads each node's properties), else as its locator from
/// `render`.
pub fn table_rows_of(
    adapter: &dyn AstAdapter,
    result: crate::QueryResult,
    columns: &[String],
    render: &dyn Fn(NodeId) -> String,
) -> Vec<Value> {
    match result {
        // a TABLE value among the values is its rows (the one-cell
        // bug of the browser hosts: `//table[1] | # | clean @| pretty`
        // drew one cell holding the whole literal; 2026-11-09)
        crate::QueryResult::Values(values) => values
            .into_iter()
            .flat_map(|v| match v {
                Value::Table(t) => table_value_records(&t),
                other => vec![other],
            })
            .collect(),
        crate::QueryResult::Nodes(nodes) => {
            let mut rows = Vec::with_capacity(nodes.len());
            for n in nodes {
                if let Some(records) = table_node_records(adapter, n) {
                    rows.extend(records);
                } else if !columns.is_empty() {
                    rows.push(Value::Record(
                        columns
                            .iter()
                            .map(|c| (c.clone(), adapter.property(n, c).unwrap_or(Value::Null)))
                            .collect(),
                    ));
                } else {
                    rows.push(Value::Str(render(n)));
                }
            }
            rows
        }
    }
}

/// A table value as the records of its rows: every column a field,
/// the index levels among them, in the table's order.
pub fn table_value_records(t: &crate::Table) -> Vec<Value> {
    let names: Vec<String> = (0..t.width()).map(|j| t.column_name(j)).collect();
    (0..t.n_rows())
        .map(|i| {
            Value::record(
                names
                    .iter()
                    .enumerate()
                    .map(|(j, n)| (n.clone(), t.cell(i, j)))
                    .collect(),
            )
        })
        .collect()
}

/// A koine `table` node read into records — the text-level lowering
/// of a document's table (litogramma, HTML, Word, Markdown through
/// the koine route): its rows are the children carrying the `row`
/// trait, a row's cells the children carrying `cell`, and each cell
/// knows its column (`::lemma`, the header's text) and its value
/// (`::body`). The header row itself yields no record. None when the
/// node is not a table.
pub fn table_node_records(adapter: &dyn AstAdapter, node: NodeId) -> Option<Vec<Value>> {
    if !adapter.has_trait(node, "tabula") {
        return None;
    }
    let text = |n: NodeId, key: &str| -> Value {
        adapter
            .property(n, key)
            .or_else(|| adapter.default_value(n))
            .unwrap_or(Value::Null)
    };
    let mut records = Vec::new();
    for row in adapter.children(node) {
        if !adapter.has_trait(row, "row") || adapter.has_trait(row, "header-row") {
            continue;
        }
        let mut fields: Vec<(String, Value)> = Vec::new();
        for (i, cell) in cells_of(adapter, row).into_iter().enumerate() {
            let key = match adapter.property(cell, "lemma") {
                Some(Value::Str(s)) if !s.is_empty() => s,
                _ => (i + 1).to_string(),
            };
            fields.push((key, text(cell, "body")));
        }
        if !fields.is_empty() {
            records.push(Value::Record(fields.into()));
        }
    }
    Some(records)
}

/// A row's cells in order: the descendants carrying the `cell`
/// trait, found below whatever list node the lowering put between
/// (a Markdown row is a list of cells).
fn cells_of(adapter: &dyn AstAdapter, row: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack: Vec<NodeId> = adapter.children(row);
    stack.reverse();
    while let Some(n) = stack.pop() {
        if adapter.has_trait(n, "cell") {
            out.push(n);
            continue;
        }
        let kids = adapter.children(n);
        stack.extend(kids.into_iter().rev());
    }
    out
}
