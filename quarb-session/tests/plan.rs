//! A session plans a line and holds its lines to a budget.
use quarb_session::doc::Doc;
use quarb_session::{LocalExecutor, MemStore, Session};

fn session() -> Session {
    let doc = Doc::parse(
        r#"{"books": [{"title": "Emma", "price": 25}, {"title": "Iliad", "price": 18}]}"#,
        "json",
    )
    .unwrap();
    Session::new(
        Box::new(LocalExecutor::new(doc, (1_753_500_000, 0), false)),
        Box::new(MemStore),
    )
}

#[test]
fn a_line_is_planned_as_kaiv() {
    let s = session();
    let text = s.plan_text("/books/*/title::").unwrap();
    assert!(text.starts_with(".!kaiv\n.!verbatim\n"), "{text}");
    assert!(text.contains("contract=quarb/plan"), "{text}");
    assert!(text.contains("query=/books/*/title::"), "{text}");
    assert!(text.contains("nodes-min="), "{text}");
    let plan = s.plan("/books/*").unwrap();
    let e = plan.estimate().expect("a parsed arbor prices its steps");
    assert_eq!(e.nodes, (2, 2));
}

#[test]
fn a_budget_refuses_a_line_before_it_runs_and_admits_within() {
    let mut s = session();
    s.set_budget(Some(quarb::plan::Budget::parse("nodes=1").unwrap()));
    let err = s.eval("/books/*/title::").unwrap_err().to_string();
    assert!(err.contains("the budget allows 1"), "{err}");
    s.set_budget(Some(
        quarb::plan::Budget::parse("nodes=100,bytes=1MiB").unwrap(),
    ));
    let cells = s.eval("/books/*/title::").unwrap();
    assert_eq!(cells.len(), 2, "{cells:?}");
    s.set_budget(None);
    assert_eq!(s.eval("/books/*/title::").unwrap().len(), 2);
}

#[test]
fn the_budget_grammar() {
    let b = quarb::plan::Budget::parse("nodes=50k, bytes=2GiB, requests=3, strict").unwrap();
    assert_eq!(b.nodes, Some(50_000));
    assert_eq!(b.bytes, Some(2 << 30));
    assert_eq!(b.requests, Some(3));
    assert!(b.strict);
    assert_eq!(quarb::plan::Budget::parse("rows=7").unwrap().nodes, Some(7));
    let err = quarb::plan::Budget::parse("rows").unwrap_err();
    assert!(err.starts_with("budget takes nodes=N"), "{err}");
    let err = quarb::plan::Budget::parse("nodes=1x").unwrap_err();
    assert!(err.starts_with("budget nodes:"), "{err}");
}
