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

fn span_of(adapter: &impl AstAdapter, node: NodeId, names: &[&str]) -> usize {
    names
        .iter()
        .find_map(|n| adapter.property(node, n))
        .and_then(|v| v.to_string().trim().parse::<usize>().ok())
        .filter(|n| *n >= 1)
        .unwrap_or(1)
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
        }
    }
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

/// Whether `text` is a footnote mark as a `sup` carries it: `[1]`,
/// `1`, `[a]`, `*`, `†`.
fn is_footnote_mark(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    let inner = t
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .unwrap_or(t);
    inner.chars().all(|c| c.is_ascii_digit())
        || (inner.chars().count() == 1 && inner.chars().all(|c| c.is_ascii_lowercase()))
        || inner
            .chars()
            .all(|c| matches!(c, '*' | '†' | '‡' | '§' | '¶'))
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
/// child that is a footnote mark removed, a trailing `[n]` removed,
/// trimmed. A break already reads as a space in the text.
fn cell_text(adapter: &impl AstAdapter, cell: NodeId, label: Option<&str>) -> String {
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
    for c in adapter.children(cell) {
        if local_name(adapter, c) == "sup"
            && let Some(mark) = adapter.default_value(c).map(|v| v.to_string())
            && is_footnote_mark(&mark)
            && let Some(at) = text.rfind(mark.as_str())
        {
            text.replace_range(at..at + mark.len(), "");
        }
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
            let text = cell_text(adapter, c, label.as_deref());
            out.push(Cell {
                label,
                head: name == "th" || role_label,
                text,
                colspan: span_of(adapter, c, &["colspan", "cols"]),
                rowspan: span_of(adapter, c, &["rowspan", "rows"]),
            });
        } else if !is_row(adapter, c) {
            cells_under(adapter, c, out, nested);
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

/// The table a `tabula` node holds, read under the default options.
pub(crate) fn node_table(adapter: &impl AstAdapter, node: NodeId) -> Result<Table, String> {
    node_table_with(adapter, node, &ReadOptions::default())
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
    let mut rows: Vec<(NodeId, Place)> = Vec::new();
    rows_under(adapter, table, table, &mut rows, Place::Body)?;
    let mut nested: Option<(usize, usize)> = None;
    let rows: Vec<(Vec<Cell>, Place)> = rows
        .into_iter()
        .enumerate()
        .map(|(i, (r, place))| {
            let mut cells = Vec::new();
            let mut at = None;
            cells_under(adapter, r, &mut cells, &mut at);
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
    let mut b = crate::TableBuilder::shaped(header.clone(), Vec::new());
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
        && matches!(suffix.trim(), "k" | "K" | "M" | "G" | "m" | "bn" | "mn")
        && read(num.trim(), Decimal::Point).is_some()
    {
        return Some("a scale suffix");
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
        if near
            .iter()
            .any(|(_, _, w)| matches!(*w, "a percent" | "a scale suffix" | "a quantity"))
        {
            cleanup.push(format!(
                "map(number; --to=:{}) after the suffix is split off",
                t.column_name(j)
            ));
        }
        let msg = format!(
            "column '{}' falls to text: {}{more}; cleanup: {}",
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
        crate::QueryResult::Values(values) => values,
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
