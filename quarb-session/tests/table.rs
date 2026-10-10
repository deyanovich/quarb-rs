//! A session hands the browser hosts the grid of a result closing in
//! `@| table`, and text for everything else.
use quarb_session::doc::Doc;
use quarb_session::{LocalExecutor, MemStore, Session};

fn session(doc: Doc) -> Session {
    Session::new(
        Box::new(LocalExecutor::new(doc, (1_753_500_000, 0), false)),
        Box::new(MemStore),
    )
}

#[test]
fn a_table_result_carries_the_grid_and_the_text() {
    let s = session(
        Doc::parse(
            r#"{"books": [{"title": "Emma", "price": 25}, {"title": "Iliad", "price": 18}]}"#,
            "json",
        )
        .unwrap(),
    );
    let (cells, html) = s
        .eval_table("/books/* | %(t = /title::; p = /price::) @| table")
        .unwrap();
    let html = html.expect("a closing @| table renders the grid");
    assert!(html.starts_with("<table class=\"quarb-table\">"), "{html}");
    assert!(
        html.contains("<td>Emma</td><td class=\"num\">25</td>"),
        "{html}"
    );
    assert_eq!(cells.len(), 1);
    let text = cells[0].display();
    assert!(text.starts_with("t      p\n-----  --\nEmma   25"), "{text}");
    // Everything else stays text.
    let (cells, html) = s.eval_table("/books/*/title::").unwrap();
    assert!(html.is_none());
    assert_eq!(cells.len(), 2);
    let (_, html) = s.eval_table("/books/* @| csv").unwrap();
    assert!(html.is_none());
}

#[test]
fn a_documents_own_table_renders_through_the_same_path() {
    let md = "# Shelf\n\n| title | price |\n|---|---|\n| Emma | 25 |\n| Iliad | 18 |\n";
    let s = session(
        Doc::parse(md, "text-markdown")
            .or_else(|_| Doc::parse(md, "markdown"))
            .unwrap(),
    );
    let (cells, html) = s.eval_table("//*<table> @| table").unwrap();
    let html = html.expect("a koine table node renders as the grid");
    assert!(html.contains("<th>title</th>"), "{html}");
    assert!(html.contains("<td>Emma</td>"), "{html}");
    assert!(
        cells[0].display().contains("Emma"),
        "{}",
        cells[0].display()
    );
}
