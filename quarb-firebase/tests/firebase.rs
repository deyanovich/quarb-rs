//! Live tests against the public Hacker News RTDB (no auth, no
//! billing — but network). Gated: run with `cargo test -p
//! quarb-firebase -- --ignored`, optionally overriding the target
//! via `QUARB_FIREBASE`.

use quarb_firebase::FirebaseAdapter;

fn target() -> String {
    std::env::var("QUARB_FIREBASE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "firebase://hacker-news.firebaseio.com/v0".to_string())
}

fn values(a: &FirebaseAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    }
}

#[test]
#[ignore = "needs network"]
fn opaque_navigation_and_properties() {
    let a = FirebaseAdapter::connect(&target()).unwrap();
    // /v0 and /v0/item refuse enumeration; named hops pass through.
    assert_eq!(
        values(&a, "/item/1 | rec(::title, ::by)"),
        [r#"{"title": "Y Combinator", "by": "pg"}"#]
    );
    // Item 1's kids enumerate (bounded container).
    assert_eq!(values(&a, "/item/1/kids/* @| count"), ["4"]);
    assert_eq!(values(&a, "/item/1/kids/0::"), ["15"]);
    // The opaque container yields nothing to a wildcard — honest.
    assert_eq!(values(&a, "/item/* @| count"), ["0"]);
}

#[test]
#[ignore = "needs network"]
fn hint_resolution() {
    let a = FirebaseAdapter::connect(&target()).unwrap();
    // A comment's parent field resolves into /item by hint.
    assert_eq!(
        values(&a, "/item/15::parent~>item::title"),
        ["Y Combinator"]
    );
    assert_eq!(values(&a, "/item/15;;;path"), ["/item/15"]);
}

#[test]
#[ignore = "needs network"]
fn declared_refs() {
    let refs = quarb_firebase::parse_refs(
        r#"{"refs": {"parent": "item", "by": "user", "kids/*": "item"}}"#,
    )
    .unwrap();
    let a = FirebaseAdapter::connect_with_refs(&target(), refs).unwrap();
    // Bare ~> uses the declared target; a hint still overrides.
    assert_eq!(values(&a, "/item/15::parent~>::title"), ["Y Combinator"]);
    assert_eq!(
        values(&a, "/item/15::parent~>item::title"),
        ["Y Combinator"]
    );
    // -> crosslinks: the declared fields become labeled edges,
    // array fields one edge per element.
    assert_eq!(values(&a, "/item/15->by"), ["/user/sama"]);
    assert_eq!(values(&a, "/item/1->kids @| count"), ["4"]);
}

#[test]
fn refs_parsing() {
    let refs = quarb_firebase::parse_refs(r#"{"refs": {"a": "t", "b/*": "u"}}"#).unwrap();
    assert_eq!(refs.get("a").map(String::as_str), Some("t"));
    assert_eq!(refs.get("b/*").map(String::as_str), Some("u"));
    assert!(quarb_firebase::parse_refs("{}").is_err());
    assert!(quarb_firebase::parse_refs(r#"{"refs": {"a": 1}}"#).is_err());
}
