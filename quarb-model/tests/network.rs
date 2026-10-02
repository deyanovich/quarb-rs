//! An edge whose two ends land in one role: a table of pairs
//! `a`, `b` over one container of characters is an undirected
//! network, and the storage order of a pair never shows in a walk.
//! The refs resolve by a key property, and the edge connects the
//! very nodes they resolved to.

use quarb::AstAdapter as _;
use quarb_json::JsonAdapter;
use quarb_model::{ModelAdapter, parse_model};

fn network() -> ModelAdapter<JsonAdapter> {
    let base = JsonAdapter::parse(
        r#"{"cast": [
              {"id": "anna",    "name": "Anna Karenina"},
              {"id": "vronsky", "name": "Count Vronsky"},
              {"id": "levin",   "name": "Konstantin Levin"},
              {"id": "kitty",   "name": "Kitty"}],
            "pairs": [
              {"a": "anna",  "b": "vronsky", "n": 9},
              {"a": "kitty", "b": "levin",   "n": 7},
              {"a": "kitty", "b": "vronsky", "n": 3},
              {"a": "anna",  "b": "nobody",  "n": 1}]}"#,
    )
    .unwrap();
    let model = parse_model(
        r#"
        node /people/person: /cast/*;
        ref /pairs/*::a --> /people/person[::id = $];
        ref /pairs/*::b --> /people/person[::id = $];
        edge /pairs/*: ::a -- ::b;
        "#,
    )
    .unwrap();
    ModelAdapter::new(base, model)
}

fn values(a: &ModelAdapter<JsonAdapter>, q: &str) -> Vec<String> {
    let mut got: Vec<String> = match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns
            .into_iter()
            .map(|n| a.name(n).unwrap_or_default())
            .collect(),
    };
    got.sort();
    got
}

#[test]
fn a_pair_is_walked_from_either_end() {
    let a = network();
    // vronsky stands in the b column twice; kitty in the a column twice
    assert_eq!(
        values(&a, r#"/people/person[::id = "vronsky"]--person::id"#),
        ["anna", "kitty"]
    );
    assert_eq!(
        values(&a, r#"/people/person[::id = "kitty"]--person::id"#),
        ["levin", "vronsky"]
    );
    // a pair whose other end names no one connects nothing
    assert_eq!(
        values(&a, r#"/people/person[::id = "anna"]--person::id"#),
        ["vronsky"]
    );
}

#[test]
fn degree_is_a_count_over_the_edge() {
    let a = network();
    assert_eq!(
        values(
            &a,
            "/people/person | %(::id; degree = (--person @| count)) | \"${:id} ${:degree}\""
        ),
        ["anna 1", "kitty 2", "levin 1", "vronsky 2"]
    );
}
