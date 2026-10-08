//! A `ref` target: a declared container, or a root-level container
//! of the source — and nothing else. A name that is neither is
//! refused when the model is checked, never left to resolve nothing.

use quarb::AstAdapter as _;
use quarb_json::JsonAdapter;
use quarb_model::{ModelAdapter, parse_model};

const DATA: &str = r#"{"books": [
      {"title": "Tom Sawyer",       "author": "twain"},
      {"title": "Huckleberry Finn", "author": "twain"},
      {"title": "Emma",             "author": "austen"}],
    "authors": {
      "twain":  {"name": "Mark Twain"},
      "austen": {"name": "Jane Austen"}}}"#;

fn shelf(statements: &str) -> ModelAdapter<JsonAdapter> {
    let base = JsonAdapter::parse(DATA).unwrap();
    ModelAdapter::new(base, parse_model(statements).unwrap())
}

fn values(a: &ModelAdapter<JsonAdapter>, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns
            .into_iter()
            .map(|n| a.name(n).unwrap_or_default())
            .collect(),
    }
}

#[test]
fn the_bare_target_names_a_root_level_container_of_the_source() {
    // No `node` statement declares `authors`: the bare form reads
    // as the source's own container, its children keyed by name.
    let a = shelf("ref /books/*::author --> authors;");
    a.check().unwrap();
    assert_eq!(values(&a, "/books/0::author-->::name"), ["Mark Twain"]);
    assert_eq!(
        values(&a, "/authors/twain<--(::author)::title"),
        ["Tom Sawyer", "Huckleberry Finn"]
    );
    // The path form says the same thing.
    let b = shelf("ref /books/*::author --> /authors/*;");
    b.check().unwrap();
    assert_eq!(values(&b, "/books/2::author-->::name"), ["Jane Austen"]);
}

#[test]
fn a_declared_container_still_wins_the_name() {
    let a = shelf(
        "node /authors/writer: /books/*::author | unique;\nref /books/*::author --> authors;",
    );
    a.check().unwrap();
    assert_eq!(values(&a, "/books/0->writer"), ["writer"]);
}

#[test]
fn a_target_that_names_nothing_is_refused() {
    let a = shelf("ref /books/*::author --> writers;");
    let err = a.check().unwrap_err();
    assert!(err.contains("'writers' names nothing"), "{err}");
    let b = shelf("ref /books/*::author --> /nosuch/*[::id = $];");
    assert!(b.check().is_err());
}

#[test]
fn any_property_is_searched_where_a_ref_is_declared_into_the_container() {
    // The declaration, not the data, lets `::*` search: books carry
    // a ref into authors, so an empty answer is a true empty.
    let a = shelf("ref /books/*::author --> authors;");
    assert_eq!(
        values(&a, "/authors/twain<--(/books/*::*)::title"),
        ["Tom Sawyer", "Huckleberry Finn"]
    );
    assert!(
        values(
            &a,
            "/authors/twain<--(/books/*[::title = \"Emma\"]::*)::title"
        )
        .is_empty()
    );
    assert_eq!(
        a.can_refer_to(
            quarb::run("/books/0", &a)
                .ok()
                .and_then(first_node)
                .unwrap(),
            quarb::run("/authors/twain", &a)
                .ok()
                .and_then(first_node)
                .unwrap()
        ),
        Some(true)
    );
}

fn first_node(r: quarb::QueryResult) -> Option<quarb::NodeId> {
    match r {
        quarb::QueryResult::Nodes(ns) => ns.into_iter().next(),
        _ => None,
    }
}
