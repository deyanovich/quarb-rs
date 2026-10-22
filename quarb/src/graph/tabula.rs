//! `| #` on a node that carries the `tabula` trait (spec: The
//! Table): the node read as a table. The text model's lowered table
//! (rows of cells whose lemma is the column) gives a frame keyed by
//! the lemmas; a grid (HTML `tr`/`th`/`td`, TEI `row`/`cell`) gives a
//! frame where the format declares a header (a first row of `th`, a
//! `thead`, cells with `role="label"`) and an array otherwise, a
//! spanned cell repeated across its span. Cells read as CSV cells: a
//! number where the text reads as one, null where empty. A nested
//! table is refused; the caption stays the node's lemma.

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

/// A cell's text as a CSV cell: null where empty, a number where
/// the text reads as one, the text otherwise.
fn cell_value(text: &str) -> Value {
    let t = text.trim();
    if t.is_empty() {
        return Value::Null;
    }
    crate::value::number::read(t, crate::value::number::Decimal::Point)
        .unwrap_or_else(|| Value::Str(t.to_string()))
}

fn span_of(adapter: &impl AstAdapter, node: NodeId, names: &[&str]) -> usize {
    names
        .iter()
        .find_map(|n| adapter.property(node, n))
        .and_then(|v| v.to_string().trim().parse::<usize>().ok())
        .filter(|n| *n >= 1)
        .unwrap_or(1)
}

struct Cell {
    label: Option<String>,
    head: bool,
    value: Value,
    colspan: usize,
    rowspan: usize,
}

fn rows_under(
    adapter: &impl AstAdapter,
    node: NodeId,
    root: NodeId,
    rows: &mut Vec<(NodeId, bool)>,
    in_head: bool,
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
            rows.push((c, in_head || labelled));
        } else {
            let head = in_head || local_name(adapter, c) == "thead";
            rows_under(adapter, c, root, rows, head)?;
        }
    }
    Ok(())
}

fn cells_under(adapter: &impl AstAdapter, node: NodeId, out: &mut Vec<Cell>) {
    for c in adapter.children(node) {
        if is_cell(adapter, c) {
            let name = local_name(adapter, c);
            let label = adapter
                .property(c, "lemma")
                .map(|v| v.to_string())
                .filter(|s| !s.is_empty());
            let role_label = adapter
                .property(c, "role")
                .is_some_and(|v| v.to_string() == "label");
            // The lowered text table keeps a cell's value as `::body`
            // (its text carries the label too: "Author: Mark Twain");
            // a grid's cell is its text.
            let mut text = adapter
                .property(c, "body")
                .or_else(|| adapter.default_value(c))
                .map(|v| v.to_string())
                .unwrap_or_default();
            if let Some(l) = &label
                && let Some(rest) = text.strip_prefix(&format!("{l}: "))
            {
                text = rest.to_string();
            }
            out.push(Cell {
                label,
                head: name == "th" || role_label,
                value: cell_value(&text),
                colspan: span_of(adapter, c, &["colspan", "cols"]),
                rowspan: span_of(adapter, c, &["rowspan", "rows"]),
            });
        } else if !is_row(adapter, c) {
            cells_under(adapter, c, out);
        }
    }
}

/// The table a `tabula` node holds.
pub(crate) fn node_table(adapter: &impl AstAdapter, node: NodeId) -> Result<Table, String> {
    if !has_trait(adapter, node, "tabula") {
        return Err(format!(
            "'{}' is not a table: '//*<tabula>' finds the tables of a document, '@| #' reads a stream of rows as one",
            adapter.name(node).unwrap_or_else(|| "the node".into())
        ));
    }
    let mut rows: Vec<(NodeId, bool)> = Vec::new();
    rows_under(adapter, node, node, &mut rows, false)?;
    let rows: Vec<(Vec<Cell>, bool)> = rows
        .into_iter()
        .map(|(r, head)| {
            let mut cells = Vec::new();
            cells_under(adapter, r, &mut cells);
            (cells, head)
        })
        .collect();
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
        let body = rows
            .iter()
            .map(|(cells, _)| {
                let mut row = vec![Value::Null; header.len()];
                for c in cells {
                    if let Some(l) = &c.label
                        && let Some(j) = header.iter().position(|h| h == l)
                    {
                        row[j] = c.value.clone();
                    }
                }
                row
            })
            .collect();
        return Ok(Table::new(Some(header), body));
    }
    // A grid: spans repeated, the header where the format declares
    // one (a first row of head cells, or one under `thead`).
    let mut grid: Vec<Vec<Value>> = Vec::new();
    let mut carried: Vec<(usize, Value)> = Vec::new(); // per column: rows left, value
    let mut header_rows = 0usize;
    for (i, (cells, in_head)) in rows.iter().enumerate() {
        let mut row: Vec<Value> = Vec::new();
        let mut spans: Vec<(usize, Value)> = Vec::new();
        let mut it = cells.iter();
        let mut next = it.next();
        loop {
            let j = row.len();
            if j < carried.len() && carried[j].0 > 0 {
                carried[j].0 -= 1;
                row.push(carried[j].1.clone());
                spans.push((0, Value::Null));
                continue;
            }
            let Some(c) = next else { break };
            for _ in 0..c.colspan {
                row.push(c.value.clone());
                spans.push((c.rowspan - 1, c.value.clone()));
            }
            next = it.next();
        }
        // columns a row span still fills past the row's own cells
        while row.len() < carried.len() && carried[row.len()].0 > 0 {
            let j = row.len();
            carried[j].0 -= 1;
            row.push(carried[j].1.clone());
            spans.push((0, Value::Null));
        }
        for (j, (left, v)) in spans.into_iter().enumerate() {
            if j >= carried.len() {
                carried.push((0, Value::Null));
            }
            if left > 0 {
                carried[j] = (left, v);
            }
        }
        let all_head = !cells.is_empty() && cells.iter().all(|c| c.head);
        if i == header_rows && (all_head || *in_head) {
            header_rows += 1;
        }
        grid.push(row);
    }
    let width = grid.iter().map(Vec::len).max().unwrap_or(0);
    for r in &mut grid {
        r.resize(width, Value::Null);
    }
    if header_rows > 0 {
        let header: Vec<String> = grid[header_rows - 1].iter().map(Value::to_string).collect();
        return Ok(Table::new(Some(header), grid.split_off(header_rows)));
    }
    Ok(Table::new(None, grid))
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
            records.push(Value::Record(fields));
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
