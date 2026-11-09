//! The rendering rule (2026-11-09): a session hands the browser hosts
//! the rich form of every line under its `--pretty` setting — a table
//! value as its grid (one per table, stacked), a record stream closed
//! by `@| pretty` as the grid, a drawing as its HTML; `off` keeps the
//! text; `always` renders a bare record and a list of scalars too. A
//! table value under `@| pretty` draws its rows, not one cell holding
//! the literal.
use quarb_session::doc::Doc;
use quarb_session::{LocalExecutor, MemStore, Session};

const PORTS: &str = "<!doctype html><table><thead><tr><th>Port</th><th>Draught, m</th></tr></thead>\
<tbody><tr><td>Piraeus</td><td>18.5</td></tr><tr><td>Koper</td><td>14</td></tr></tbody></table>";

fn session(doc: Doc) -> Session {
    Session::new(
        Box::new(LocalExecutor::new(doc, (1_753_500_000, 0), false)),
        Box::new(MemStore),
    )
}

#[test]
fn a_table_value_renders_as_its_grid_under_the_setting() {
    let s = session(Doc::parse(PORTS, "html").unwrap());
    // off: the text alone
    assert_eq!(s.pretty_mode(), quarb::PrettyMode::Off);
    let (cells, html) = s.eval_rich("//table[1] | #(--autounits)").unwrap();
    assert_eq!(cells.len(), 1);
    assert!(html.is_none());
    // structures: the grid from the table itself — the index marked,
    // the type word under the title
    s.set_pretty(quarb::PrettyMode::Structures);
    let (cells, html) = s
        .eval_rich("//table[1] | #(--autounits) | #(-i=:Port)")
        .unwrap();
    assert_eq!(cells.len(), 1);
    let html = html.expect("a table value renders");
    assert!(html.starts_with("<table class=\"quarb-table\">"), "{html}");
    assert!(
        html.contains("<th class=\"index\"><span class=\"title\">Port</span> <span class=\"type\">text</span></th>"),
        "{html}"
    );
    assert!(
        html.contains("<span class=\"title\">Draught</span> <span class=\"type\">_m</span>"),
        "{html}"
    );
    assert!(
        html.contains("<td class=\"index\">Piraeus</td><td class=\"num\">18.5_m</td>"),
        "{html}"
    );
    // the literal stays in the lines
    assert!(
        cells[0].display().starts_with("#(@(Port; Draught);"),
        "{}",
        cells[0].display()
    );
    // two tables in a stream: two grids
    let (_, html) = s
        .eval_rich("= @((//table[1] | #); (//table[1] | #))")
        .unwrap();
    let html = html.expect("tables in a list render");
    assert_eq!(
        html.matches("<table class=\"quarb-table\">").count(),
        2,
        "{html}"
    );
    // the session command sets it from a line
    s.eval("session(--pretty=off)").unwrap();
    assert_eq!(s.pretty_mode(), quarb::PrettyMode::Off);
    let (_, html) = s.eval_rich("//table[1] | #").unwrap();
    assert!(html.is_none());
}

#[test]
fn a_pretty_closing_over_a_table_value_draws_its_rows() {
    let s = session(Doc::parse(PORTS, "html").unwrap());
    s.set_pretty(quarb::PrettyMode::Structures);
    let (cells, html) = s.eval_rich("//table[1] | # @| pretty").unwrap();
    let html = html.expect("the grid");
    assert!(html.contains("<th>Port</th>"), "{html}");
    assert!(html.contains("<td>Piraeus</td>"), "{html}");
    assert!(!html.contains("#(@("), "the one-cell bug: {html}");
    let text = cells[0].display();
    assert!(text.starts_with("Port"), "{text}");
    assert!(!text.contains("#(@("), "{text}");
    // a record stream closed by @| pretty, as before
    let (_, html) = s
        .eval_rich("//tr[2]/td | %(cell = ::text) @| pretty")
        .unwrap();
    assert!(html.expect("grid").contains("<th>cell</th>"));
}

#[test]
fn always_renders_a_record_and_a_scalar_list_and_a_drawing_renders_rich() {
    let s = session(Doc::parse(PORTS, "html").unwrap());
    s.set_pretty(quarb::PrettyMode::Structures);
    let (_, html) = s.eval_rich("= %(a = 1; b = 2)").unwrap();
    assert!(html.is_none(), "a bare record is text under structures");
    s.set_pretty(quarb::PrettyMode::Always);
    let (cells, html) = s.eval_rich("= %(a = 1; b = 2)").unwrap();
    let html = html.expect("key/value grid");
    assert!(
        html.contains("<th>field</th>") && html.contains("<td>a</td><td class=\"num\">1</td>"),
        "{html}"
    );
    assert_eq!(cells.len(), 1);
    let (_, html) = s.eval_rich("= @(1; 2; 3)").unwrap();
    let html = html.expect("one row across");
    assert_eq!(html.matches("<tr>").count(), 1, "{html}");
    // a drawing: its HTML under the setting, none when off
    let (_, html) = s.eval_rich("//table[1] @| tree(-d=1)").unwrap();
    assert!(html.is_some(), "a drawing renders rich");
    s.set_pretty(quarb::PrettyMode::Off);
    let (_, html) = s.eval_rich("//table[1] @| tree(-d=1)").unwrap();
    assert!(html.is_none(), "off keeps the text of a drawing too");
    // the export of a line: the document form
    s.set_pretty(quarb::PrettyMode::Structures);
    let doc = s.export_rich("//table[1] | #").unwrap();
    assert!(
        doc.starts_with("<style>") && doc.contains("<table class=\"quarb-table\">"),
        "{doc}"
    );
    let doc = s.export_rich("= \"Piraeus\"").unwrap();
    assert!(doc.contains("<pre class=\"quarb-text\">Piraeus"), "{doc}");
}
