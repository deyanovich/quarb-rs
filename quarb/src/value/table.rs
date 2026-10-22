//! The HTML table: the grid a result closing in `@| pretty` renders as
//! in every browser host — the notebook's output cell, the
//! playground's result pane, the extension's side panel,
//! quarb.org/search — one renderer beside the text form
//! ([`tabulate`](crate::tabulate)) and one stylesheet ([`TABLE_CSS`])
//! shipped with the engine, so every host draws the same grid. The
//! rule: a result whose closing command is `@| pretty` renders as the
//! grid, everything else stays text; no toggle.
//!
//! Both forms read the same record stream: the header from the
//! record keys (or the named columns, in their order), one row per
//! record, a scalar in a `value` column, a field a record lacks an
//! empty cell. The HTML form adds what text cannot: numbers
//! right-aligned (`class="num"`, the header too when every cell of
//! the column is one), nulls marked (`class="null"`), lists and
//! records set in a monospace face (`class="nested"`). A koine
//! `table` node — the text-level lowering of a document's table,
//! rows as items, each cell carrying its column's `::lemma` and its
//! `::body` — renders through the same path once
//! [`table_node_records`] has read it into records.
use crate::value::Value;

/// The one stylesheet, scoped under `.quarb-table`: a host includes
/// it once (a `<style>` element, a notebook's output cell) and
/// every table it renders is dressed; the host's `--quarb-*`
/// variables set the colours where it has a theme.
pub const TABLE_CSS: &str = include_str!("table.css");

/// The columns a record stream tabulates under: the named columns in
/// their order, else the union of the records' fields in first-seen
/// order; a scalar rides a `value` column (so does an empty stream).
pub fn table_columns(values: &[Value], named: Option<&[String]>) -> Vec<String> {
    match named {
        Some(names) => names.to_vec(),
        None => {
            let mut columns: Vec<String> = Vec::new();
            let mut has_scalar = false;
            for v in values {
                match v {
                    Value::Record(fields) => {
                        for (k, _) in fields {
                            if !columns.contains(k) {
                                columns.push(k.clone());
                            }
                        }
                    }
                    _ => has_scalar = true,
                }
            }
            if has_scalar || columns.is_empty() {
                columns.push("value".to_string());
            }
            columns
        }
    }
}

/// The grid's cells, one row per value, one cell per column: a
/// record's field under its column (null where the record lacks
/// it), a scalar under `value`.
pub fn table_cells(values: &[Value], columns: &[String]) -> Vec<Vec<Value>> {
    values
        .iter()
        .map(|v| match v {
            Value::Record(fields) => columns
                .iter()
                .map(|c| {
                    fields
                        .iter()
                        .find(|(k, _)| k == c)
                        .map(|(_, v)| v.clone())
                        .unwrap_or(Value::Null)
                })
                .collect(),
            other => columns
                .iter()
                .map(|c| {
                    if c == "value" {
                        other.clone()
                    } else {
                        Value::Null
                    }
                })
                .collect(),
        })
        .collect()
}

/// The grid as HTML: `<table class="quarb-table">` with a header
/// row and one body row per value, every text escaped, the cell's
/// text from `cell` (the host's rendering: the output locale's, or
/// [`Value::display_form`] with a bare string as itself). The
/// element alone, no stylesheet — the host includes [`TABLE_CSS`]
/// once.
pub fn table_html(
    values: &[Value],
    named: Option<&[String]>,
    cell: &dyn Fn(&Value) -> String,
) -> String {
    let columns = table_columns(values, named);
    let rows = table_cells(values, &columns);
    let mut out = String::with_capacity(64 + rows.len() * columns.len() * 24);
    out.push_str("<table class=\"quarb-table\">\n<thead>\n<tr>");
    for (i, c) in columns.iter().enumerate() {
        // A header is right-aligned over a column of numbers.
        let numeric = rows.iter().filter_map(|r| r.get(i)).any(is_number)
            && rows
                .iter()
                .filter_map(|r| r.get(i))
                .all(|v| is_number(v) || matches!(v, Value::Null));
        if numeric {
            out.push_str("<th class=\"num\">");
        } else {
            out.push_str("<th>");
        }
        out.push_str(&escape(c));
        out.push_str("</th>");
    }
    out.push_str("</tr>\n</thead>\n<tbody>\n");
    for row in &rows {
        out.push_str("<tr>");
        for v in row {
            match v {
                Value::Null => out.push_str("<td class=\"null\"></td>"),
                _ => {
                    let class = cell_class(v);
                    if class.is_empty() {
                        out.push_str("<td>");
                    } else {
                        out.push_str("<td class=\"");
                        out.push_str(class);
                        out.push_str("\">");
                    }
                    out.push_str(&escape(&cell(v)));
                    out.push_str("</td>");
                }
            }
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</tbody>\n</table>");
    out
}

/// [`table_html`] with the default cell text: a string as itself,
/// anything else in its Quarb form.
pub fn table_html_plain(values: &[Value], named: Option<&[String]>) -> String {
    table_html(values, named, &|v| match v {
        Value::Str(s) => s.clone(),
        other => other.display_form(),
    })
}

fn is_number(v: &Value) -> bool {
    matches!(v, Value::Int(_) | Value::Float(_) | Value::Quantity { .. })
}

fn cell_class(v: &Value) -> &'static str {
    match v {
        Value::Instant { .. } => "time",
        Value::Quantity { base, .. } if base == "s" => "time",
        Value::Int(_) | Value::Float(_) | Value::Quantity { .. } => "num",
        Value::Bool(_) => "bool",
        Value::List(_) | Value::Record(_) => "nested",
        _ => "",
    }
}

/// HTML text escaping: the five characters with a meaning.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}
