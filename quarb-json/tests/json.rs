//! End-to-end tests: queries run through the engine against parsed
//! JSON documents.

use quarb_json::JsonAdapter;

const DOC: &str = r#"{
  "users": [
    { "name": "Alice", "age": 30, "admin": true },
    { "name": "Bob", "age": 17, "admin": false }
  ],
  "count": 2
}"#;

/// Run a query and return the node locations (JSON pointers), sorted.
fn nodes(query: &str) -> Vec<String> {
    let adapter = JsonAdapter::parse(DOC).unwrap();
    let mut got: Vec<String> = match quarb::run(query, &adapter).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns.into_iter().map(|n| adapter.pointer(n)).collect(),
        quarb::QueryResult::Values(_) => panic!("expected nodes"),
    };
    got.sort();
    got
}

/// Run a projection query and return the values as strings, sorted.
fn values(query: &str) -> Vec<String> {
    let adapter = JsonAdapter::parse(DOC).unwrap();
    let mut got: Vec<String> = match quarb::run(query, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    };
    got.sort();
    got
}

/// Value expressions with path operands over JSON numbers.
#[test]
fn value_expressions() {
    let doc = r#"{"items": [
        {"price": 4, "qty": 3},
        {"price": 10, "qty": 2},
        {"price": 7}
    ]}"#;
    let adapter = JsonAdapter::parse(doc).unwrap();
    let vals = |q: &str| match quarb::run(q, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    };
    // computed comparison over child values
    assert_eq!(vals("/items/*[/price:: * /qty:: > 15]/price::"), vec!["10"]);
    // a missing operand path is null, and null propagates
    assert_eq!(vals("/items/* | /price:: * /qty::"), vec!["12", "20", ""]);
    // div is always a float; idiv truncates
    assert_eq!(vals("/items/1 | /price:: idiv /qty::"), vec!["5"]);
    // a computed column, aggregated
    assert_eq!(
        vals("/items/* | .total(/price:: * /qty::) @| sum"),
        vec!["32"]
    );
}

#[test]
fn navigate_by_key_and_index() {
    assert_eq!(values("/users/0/name::"), vec!["Alice"]);
    assert_eq!(values("/count::"), vec!["2"]);
    assert_eq!(nodes("/users/1"), vec!["/users/1"]);
}

#[test]
fn descend_and_project() {
    // every user's name, anywhere in the document
    assert_eq!(values("//name::"), vec!["Alice", "Bob"]);
    assert_eq!(values("/users/*/age::"), vec!["17", "30"]);
}

#[test]
fn type_traits() {
    // string-valued nodes named "name"
    assert_eq!(
        nodes("//name<string>"),
        vec!["/users/0/name", "/users/1/name"]
    );
    // the array
    assert_eq!(nodes("/*<array>"), vec!["/users"]);
    // boolean values
    assert_eq!(
        nodes("//*<boolean>"),
        vec!["/users/0/admin", "/users/1/admin"]
    );
}

#[test]
fn predicates_over_json() {
    // users whose age >= 18, then their name (project the child value
    // with `::` inside the predicate)
    assert_eq!(values("/users/*[/age:: >= 18]/name::"), vec!["Alice"]);
    // admins only (bare `true` is a boolean literal)
    assert_eq!(values("/users/*[/admin:: = true]/name::"), vec!["Alice"]);
}

#[test]
fn resolve_json_ref() {
    const REF: &str = r##"{
      "definitions": { "address": { "city": "London", "zip": "SW1" } },
      "user": { "name": "Alice", "home": { "$ref": "#/definitions/address" } }
    }"##;
    let adapter = JsonAdapter::parse(REF).unwrap();

    let vals = |q: &str| match quarb::run(q, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|n| adapter.pointer(*n)).collect(),
    };

    // follow the $ref, then read a field of the target
    assert_eq!(vals("//home::'$ref'~>/city::"), vec!["London"]);
    assert_eq!(vals("//home::'$ref'~>/zip::"), vec!["SW1"]);
    // the resolution lands on the definitions/address node
    assert_eq!(vals("//home::'$ref'~>"), vec!["/definitions/address"]);
}

#[test]
fn metadata_and_aggregation() {
    assert_eq!(values("/users;;;length"), vec!["2"]);
    assert_eq!(values("/users/0/name;;;type"), vec!["string"]);
    // total age across users
    assert_eq!(values("/users/*/age;;;type"), vec!["number", "number"]);
    assert_eq!(values("/users/*/age:: @| sum"), vec!["47"]);
}

/// A correlated join over JSON, where the join keys are *children*
/// (fields), not properties — reachable only because `$*N` can now
/// navigate. Ada has limit 100, Bo limit 500; order-1 (amount 200)
/// exceeds its own user Ada's limit, order-2 (amount 300) does not
/// exceed Bo's 500. Both conditions must bind to the same user.
#[test]
fn correlated_join_navigates_from_context() {
    let doc = r#"{
      "users": [
        { "id": 1, "limit": 100 },
        { "id": 2, "limit": 500 }
      ],
      "orders": [
        { "uid": 1, "amount": 200 },
        { "uid": 2, "amount": 300 }
      ]
    }"#;
    let adapter = JsonAdapter::parse(doc).unwrap();
    let vals = |q: &str| match quarb::run(q, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    };
    let join = "/users/* <=> /orders/*\
        [/uid:: = $*1/id:: and /amount:: > $*1/limit::]/amount::";
    assert_eq!(vals(join), vec!["200"]);
    // stacked brackets share the binding, so identical result
    let stacked = "/users/* <=> /orders/*\
        [/uid:: = $*1/id::][/amount:: > $*1/limit::]/amount::";
    assert_eq!(vals(stacked), vec!["200"]);
}

/// A key that begins with `@` (kaiv canonicalizes array namespaces
/// this way) navigates as an ordinary hop name after `/` or `//`.
#[test]
fn at_leading_hop_names() {
    let doc = r#"{ "@servers": [ {"host": "web1"}, {"host": "web2"} ] }"#;
    let adapter = JsonAdapter::parse(doc).unwrap();
    let vals = |q: &str| match quarb::run(q, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    };
    assert_eq!(vals("/@servers/*/host::"), vec!["web1", "web2"]);
    assert_eq!(vals("//@servers/*/host::"), vec!["web1", "web2"]);
    // the `@|` aggregation sigil is unaffected
    assert_eq!(vals("/@servers/*/host:: @| join(\",\")"), vec!["web1,web2"]);
}

/// The correlation keeps its witness: `$*k` in pipeline stages
/// projects the joined side into output (`SELECT a.x, b.y`).
#[test]
fn join_projection() {
    let doc = r#"{"users": [{"id": 1, "name": "Ada"}, {"id": 2, "name": "Bo"}],
                  "orders": [{"uid": 1, "amt": 30}, {"uid": 2, "amt": 45}, {"uid": 1, "amt": 99}]}"#;
    let adapter = JsonAdapter::parse(doc).unwrap();
    let got = match quarb::run(
        "/users/* <=> /orders/*[/uid:: = $*1/id::] | rec(\"who\", $*1/name::, \"amt\", /amt::)",
        &adapter,
    )
    .unwrap()
    {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        _ => panic!("expected values"),
    };
    assert_eq!(
        got,
        vec![
            r#"{"who": "Ada", "amt": 30}"#,
            r#"{"who": "Bo", "amt": 45}"#,
            r#"{"who": "Ada", "amt": 99}"#
        ]
    );
}
