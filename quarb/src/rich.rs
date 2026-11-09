//! The rich form of a result (the user's ruling, 2026-11-09): one
//! renderer for every browser host — the notebook cell, the
//! playground, the extension's panel — of what a table value, a record
//! stream under `@| pretty` and a drawing look like as HTML, and one
//! rule for WHEN: the session's `--pretty` setting
//! ([`PrettyMode`](crate::PrettyMode)). `off` keeps the text; under
//! `structures` a table value renders as the grid (one grid per table,
//! stacked in thread order), a record stream closed by `@| pretty` as
//! the grid, a drawing (`@| tree`, `@| histogram`) as its HTML; under
//! `always` a bare record renders too, as a two-column key/value grid,
//! and a list of scalars as one row across. The literal always stays
//! in the text lines for copy and export; a table inside a cell shows
//! its literal text. The hosts differ in nothing but the setting they
//! open with: the kernel and the extension start at `structures`, the
//! playground at `off` with a button that runs `session(--pretty=…)`.
//!
//! The grid of a TABLE value is drawn from the table itself: the index
//! levels first, marked (`class="index"`), the header a title with the
//! column's type word under it (`<span class="type">_m</span>`: the
//! declared unit or the inferred word), cells through the pretty
//! float; the stylesheet is [`css`] ([`TABLE_CSS`](crate::TABLE_CSS)
//! with the grid's rules), the diagnostics block [`diagnostics_html`],
//! and [`document`] puts the three together for an export or a saved
//! notebook.

use crate::diagnostic::{Diagnostic, Severity};
use crate::pipeline::PrettyMode;
use crate::value::table::{escape, table_html};
use crate::value::{Table, Value};

/// The stylesheet every rich form is dressed with, once per host.
pub fn css() -> &'static str {
    crate::TABLE_CSS
}

/// The rich form of a result's values under the session's setting:
/// `None` where the text form is the only one (`off`, or nothing in
/// the result that renders). `drawing` is the HTML a closing drawing
/// left ([`Outcome::html`](crate::Outcome::html)); `closing_grid` is
/// the grid a `@| pretty` closing drew over the record stream (the
/// host's `run_table` road) — both take precedence over the values.
pub fn html_of(
    values: &[Value],
    closing_grid: Option<&str>,
    drawing: Option<&str>,
    mode: PrettyMode,
) -> Option<String> {
    if mode == PrettyMode::Off {
        return None;
    }
    if let Some(d) = drawing {
        return Some(d.to_string());
    }
    if let Some(g) = closing_grid {
        return Some(g.to_string());
    }
    // the table values: one grid each, stacked in order — the
    // threads' tables, or the items of a list of tables (`@($,t1;
    // $,t2)`)
    let tables: Vec<&Table> = values
        .iter()
        .flat_map(|v| match v {
            Value::List(items) => items.iter().collect::<Vec<_>>(),
            other => vec![other],
        })
        .filter_map(|v| match v {
            Value::Table(t) => Some(t.as_ref()),
            _ => None,
        })
        .collect();
    if !tables.is_empty() {
        return Some(
            tables
                .iter()
                .map(|t| table_grid(t))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    if mode != PrettyMode::Always {
        return None;
    }
    match values {
        [] => None,
        // a bare record: the two-column key/value grid
        [Value::Record(fields)] => {
            let rows: Vec<Value> = fields
                .iter()
                .map(|(k, v)| {
                    Value::record(vec![
                        ("field".to_string(), Value::Str(k.clone())),
                        ("value".to_string(), v.clone()),
                    ])
                })
                .collect();
            Some(table_html(
                &rows,
                Some(&["field".to_string(), "value".to_string()]),
                &cell_text,
            ))
        }
        // a list of records: the record grid
        [Value::List(items)] if items.iter().all(|i| matches!(i, Value::Record(_))) => {
            Some(table_html(items, None, &cell_text))
        }
        // a list of scalars, or a stream of them: one row across
        [Value::List(inner)] if inner.iter().all(is_scalar) => {
            let scalars: Vec<&Value> = inner.iter().collect();
            (scalars.len() >= 2).then(|| row_grid(&scalars))
        }
        items if items.iter().all(is_scalar) => {
            let scalars: Vec<&Value> = items.iter().collect();
            (scalars.len() >= 2).then(|| row_grid(&scalars))
        }
        _ => None,
    }
}

fn is_scalar(v: &Value) -> bool {
    !matches!(v, Value::List(_) | Value::Record(_) | Value::Table(_))
}

/// A cell's text in a grid: a string as itself, a float through the
/// pretty float, anything else in its Quarb form.
fn cell_text(v: &Value) -> String {
    match v {
        Value::Str(s) => s.clone(),
        Value::Float(f) => crate::value::pretty_float(*f, 6),
        other => other.display_form(),
    }
}

fn cell_class(v: &Value) -> &'static str {
    match v {
        Value::Instant { .. } => "time",
        Value::Quantity { base, .. } if base == "s" => "time",
        Value::Int(_) | Value::Float(_) | Value::Quantity { .. } => "num",
        Value::Bool(_) => "bool",
        Value::List(_) | Value::Record(_) | Value::Table(_) => "nested",
        _ => "",
    }
}

/// The grid of a TABLE value: the index levels first, marked; the
/// header a title with the type word beneath; one body row per row.
pub fn table_grid(t: &Table) -> String {
    let width = t.width();
    let types = t.types();
    let mut out = String::with_capacity(128 + t.n_rows() * width * 24);
    out.push_str("<table class=\"quarb-table\">\n<thead>\n<tr>");
    for j in 0..width {
        let numeric = matches!(
            types.get(j),
            Some(
                crate::value::ColumnType::Integer
                    | crate::value::ColumnType::Float
                    | crate::value::ColumnType::Quantity
                    | crate::value::ColumnType::Unit(_)
            )
        );
        let mut classes: Vec<&str> = Vec::new();
        if j < t.index {
            classes.push("index");
        }
        if numeric {
            classes.push("num");
        }
        if classes.is_empty() {
            out.push_str("<th>");
        } else {
            out.push_str(&format!("<th class=\"{}\">", classes.join(" ")));
        }
        out.push_str(&format!(
            "<span class=\"title\">{}</span>",
            escape(&t.column_name(j))
        ));
        let word = match t.declaration(j) {
            Some(d) => d.word(),
            None => types.get(j).map(|ty| ty.word()).unwrap_or_default(),
        };
        if !word.is_empty() {
            out.push_str(&format!(" <span class=\"type\">{}</span>", escape(&word)));
        }
        out.push_str("</th>");
    }
    out.push_str("</tr>\n</thead>\n<tbody>\n");
    for i in 0..t.n_rows() {
        out.push_str("<tr>");
        for j in 0..width {
            let v = t.cell(i, j);
            let mut classes: Vec<&str> = Vec::new();
            if j < t.index {
                classes.push("index");
            }
            match &v {
                Value::Null => {
                    classes.push("null");
                    out.push_str(&format!("<td class=\"{}\"></td>", classes.join(" ")));
                }
                other => {
                    let c = cell_class(other);
                    if !c.is_empty() {
                        classes.push(c);
                    }
                    if classes.is_empty() {
                        out.push_str("<td>");
                    } else {
                        out.push_str(&format!("<td class=\"{}\">", classes.join(" ")));
                    }
                    out.push_str(&escape(&cell_text(other)));
                    out.push_str("</td>");
                }
            }
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</tbody>\n</table>");
    out
}

/// A list of scalars as one row across (the `always` setting).
fn row_grid(values: &[&Value]) -> String {
    let mut out = String::from("<table class=\"quarb-table\">\n<tbody>\n<tr>");
    for v in values {
        match v {
            Value::Null => out.push_str("<td class=\"null\"></td>"),
            other => {
                let c = cell_class(other);
                if c.is_empty() {
                    out.push_str("<td>");
                } else {
                    out.push_str(&format!("<td class=\"{c}\">"));
                }
                out.push_str(&escape(&cell_text(other)));
                out.push_str("</td>");
            }
        }
    }
    out.push_str("</tr>\n</tbody>\n</table>");
    out
}

/// The diagnostics under an answer, down to the host's level (spec:
/// Diagnostics): the warnings as lines, the infos and debug notes
/// behind a `<details>` with their count; the errors are the answer's
/// own, never here.
pub fn diagnostics_html(diagnostics: &[Diagnostic], level: Severity) -> String {
    let shown: Vec<&Diagnostic> = diagnostics
        .iter()
        .filter(|d| d.severity != Severity::Error && d.severity <= level)
        .collect();
    if shown.is_empty() {
        return String::new();
    }
    let line = |d: &Diagnostic| {
        format!(
            "<div class=\"quarb-diag {}\"><span class=\"diag-code\">{}</span> <span class=\"diag-site\">({})</span> {}</div>",
            d.severity.word(),
            escape(&d.code),
            escape(&d.site),
            escape(&d.message)
        )
    };
    let mut out = String::new();
    for d in shown.iter().filter(|d| d.severity == Severity::Warning) {
        out.push_str(&line(d));
    }
    let infos: Vec<&&Diagnostic> = shown
        .iter()
        .filter(|d| matches!(d.severity, Severity::Info | Severity::Debug))
        .collect();
    if !infos.is_empty() {
        out.push_str(&format!(
            "<details class=\"quarb-diag-infos\"><summary>ⓘ {}</summary>",
            infos.len()
        ));
        for d in infos {
            out.push_str(&line(d));
        }
        out.push_str("</details>");
    }
    out
}

/// A rich form as a document fragment a host saves or exports: the
/// stylesheet, the form, the diagnostics block.
pub fn document(html: &str, diagnostics: &[Diagnostic], level: Severity) -> String {
    format!(
        "<style>{}</style>\n{}{}",
        css(),
        html,
        diagnostics_html(diagnostics, level)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Table {
        Table::typed(
            Some(vec!["Port".into(), "Draught".into()]),
            vec![
                vec![Value::Str("Piraeus".into()), Value::Float(18.5)],
                vec![Value::Str("Koper".into()), Value::Null],
            ],
            vec![None, Some(crate::value::ColumnType::Unit("m".into()))],
        )
        .unwrap()
        .indexed_by("Port", None)
        .unwrap()
    }

    #[test]
    fn a_table_value_renders_as_its_grid_under_structures() {
        let t = Value::table(table());
        let html = html_of(std::slice::from_ref(&t), None, None, PrettyMode::Structures).unwrap();
        assert!(html.contains("<th class=\"index\"><span class=\"title\">Port</span> <span class=\"type\">text</span></th>"), "{html}");
        assert!(
            html.contains("<span class=\"title\">Draught</span> <span class=\"type\">_m</span>"),
            "{html}"
        );
        assert!(
            html.contains("<td class=\"index\">Piraeus</td><td class=\"num\">18.5_m</td>"),
            "{html}"
        );
        assert!(html.contains("<td class=\"null\"></td>"), "{html}");
        // two tables stack
        let two = html_of(&[t.clone(), t.clone()], None, None, PrettyMode::Structures).unwrap();
        assert_eq!(two.matches("<table class=\"quarb-table\">").count(), 2);
        // off: text only
        assert_eq!(html_of(&[t], None, None, PrettyMode::Off), None);
    }

    #[test]
    fn always_renders_a_record_and_a_scalar_list_too() {
        let rec = Value::record(vec![
            ("a".into(), Value::Int(1)),
            ("b".into(), Value::Str("x".into())),
        ]);
        assert_eq!(
            html_of(
                std::slice::from_ref(&rec),
                None,
                None,
                PrettyMode::Structures
            ),
            None
        );
        let html = html_of(std::slice::from_ref(&rec), None, None, PrettyMode::Always).unwrap();
        assert!(html.contains("<th>field</th><th>value</th>"), "{html}");
        let list = Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let html = html_of(std::slice::from_ref(&list), None, None, PrettyMode::Always).unwrap();
        assert_eq!(html.matches("<tr>").count(), 1, "{html}");
        assert_eq!(html.matches("<td class=\"num\">").count(), 3, "{html}");
        assert_eq!(
            html_of(&[Value::Int(1)], None, None, PrettyMode::Always),
            None
        );
    }

    #[test]
    fn a_drawing_and_a_closing_grid_come_first() {
        assert_eq!(
            html_of(&[], None, Some("<svg/>"), PrettyMode::Structures).as_deref(),
            Some("<svg/>")
        );
        assert_eq!(
            html_of(&[], Some("<table/>"), None, PrettyMode::Structures).as_deref(),
            Some("<table/>")
        );
        assert_eq!(html_of(&[], Some("<table/>"), None, PrettyMode::Off), None);
    }

    #[test]
    fn the_diagnostics_block_follows_the_level() {
        let ds = vec![
            Diagnostic::new(
                Severity::Warning,
                "TABLE_TEXT",
                "t",
                "column 'x' falls to text",
            ),
            Diagnostic::new(Severity::Info, "AUTOUNITS", "t", "autounits on t: …"),
        ];
        let html = diagnostics_html(&ds, Severity::Warning);
        assert!(
            html.contains("quarb-diag warning") && !html.contains("<details"),
            "{html}"
        );
        let html = diagnostics_html(&ds, Severity::Info);
        assert!(
            html.contains("<details class=\"quarb-diag-infos\"><summary>ⓘ 1</summary>"),
            "{html}"
        );
        assert!(document("<p/>", &ds, Severity::Warning).starts_with("<style>"));
    }
}
