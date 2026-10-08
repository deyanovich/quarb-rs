//! `index SCOPE::prop;` — a declared index serves the searches by
//! value whose scope is the declared one, and changes nothing else:
//! every query answers the same with and without the declaration.

use quarb::AstAdapter as _;
use quarb_json::JsonAdapter;
use quarb_model::{ModelAdapter, parse_model};

const DATA: &str = r#"{"orders": [
      {"id": 1, "customer": "ada",  "total": 90},
      {"id": 2, "customer": "bo",   "total": 40},
      {"id": 3, "customer": "ada",  "total": 50},
      {"id": 4, "customer": "cy",   "total": 1},
      {"id": 5, "customer": "ada",  "total": 1.0}],
    "customers": [
      {"id": "ada", "name": "Ada"},
      {"id": "bo",  "name": "Bo"},
      {"id": "cy",  "name": "Cy"}]}"#;

fn shop(statements: &str) -> ModelAdapter<JsonAdapter> {
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
fn the_index_statement_parses_and_refuses_a_duplicate() {
    let m = parse_model("index /orders/*::customer;\nindex /orders/*::total;").unwrap();
    assert_eq!(m.indexes.len(), 2);
    assert_eq!(m.indexes[0].scope, "/orders/*");
    assert_eq!(m.indexes[0].property, "customer");
    assert!(parse_model("index /orders/*::customer;\nindex /orders/*::customer;").is_err());
    assert!(parse_model("index ::customer;").is_err());
}

#[test]
fn the_value_match_answers_the_same_with_and_without_the_index() {
    let plain = shop("");
    let indexed = shop("index /orders/*::customer;");
    for q in [
        "/customers/*[::id = \"ada\"]::id=>(/orders/*::customer)::total",
        "/customers/* | %(::name; n = (::id=>(/orders/*::customer) @| count))",
        "/customers/*[::id = \"bo\"]::id=>(/orders/*::customer)::id",
    ] {
        assert_eq!(values(&plain, q), values(&indexed, q), "{q}");
    }
    assert_eq!(
        values(
            &indexed,
            "/customers/*[::id = \"ada\"]::id=>(/orders/*::customer)::total"
        ),
        ["90", "50", "1"]
    );
    // The index is consulted: the adapter answers the lookup.
    assert_eq!(
        indexed
            .index_lookup("/orders/*", "customer", &quarb::Value::Str("ada".into()))
            .map(|v| v.len()),
        Some(3)
    );
    assert!(
        plain
            .index_lookup("/orders/*", "customer", &quarb::Value::Str("ada".into()))
            .is_none()
    );
}

#[test]
fn numbers_share_a_key_across_int_and_float() {
    let indexed = shop("index /orders/*::total;");
    // 1 and 1.0 compare equal, so the index names both orders.
    assert_eq!(
        values(&indexed, "/orders/4::total=>(/orders/*::total)::id"),
        ["4", "5"]
    );
    let plain = shop("");
    assert_eq!(
        values(&plain, "/orders/4::total=>(/orders/*::total)::id"),
        ["4", "5"]
    );
}

#[test]
fn the_reverse_arrow_over_the_indexed_scope_answers_the_same() {
    let refs = "node /clients/client: /customers/*;\n\
                ref /orders/*::customer --> /clients/client[::id = $];\n";
    let plain = shop(refs);
    let indexed = shop(&format!("{refs}index /orders/*::customer;"));
    for q in [
        "//client[::id = \"ada\"]<--(/orders/*::customer)::total",
        "//client | %(::name; n = (<--(/orders/*::customer) @| count))",
        "/customers/*[::id = \"ada\"]<--(/orders/*::customer)::total",
        "//client[::id = \"cy\"]<--clients(/orders/*::customer)::id",
    ] {
        assert_eq!(values(&plain, q), values(&indexed, q), "{q}");
    }
    assert_eq!(
        values(
            &indexed,
            "//client[::id = \"ada\"]<--(/orders/*::customer)::total"
        ),
        ["90", "50", "1"]
    );
    assert!(
        indexed
            .index_referrers("/orders/*", "customer", None, indexed.root())
            .is_some()
    );
}

#[test]
fn an_index_does_not_admit_an_unscoped_search() {
    // The declaration changes cost, never which queries are accepted:
    // an unscoped match stays on the adapter's own terms (json walks).
    let indexed = shop("index /orders/*::customer;");
    assert_eq!(
        values(
            &indexed,
            "/customers/*[::id = \"bo\"]::id=>(::customer)::total"
        ),
        ["40"]
    );
}
