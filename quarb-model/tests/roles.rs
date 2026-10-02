//! One container, several roles: a container declared by more than
//! one `node` statement holds every statement's members under its
//! own role — `/tables/class` and `/tables/schema` share `/tables`,
//! hops label by the role they land on, and a ref targets a role.

use quarb::AstAdapter as _;
use quarb_json::JsonAdapter;
use quarb_model::{ModelAdapter, parse_model};

fn tables() -> ModelAdapter<JsonAdapter> {
    let base = JsonAdapter::parse(
        r#"{"rows": [
            {"w": "дом",  "class": "n1", "schema": "c"},
            {"w": "стол", "class": "n1", "schema": "b"},
            {"w": "нож",  "class": "n4", "schema": "b"}
        ]}"#,
    )
    .unwrap();
    let model = parse_model(
        r#"
        node /tables/class:  /rows/*::class  | unique;
        node /tables/schema: /rows/*::schema | unique;
        node /entries/entry: /rows/*;
        ref /rows/*::class  --> /tables/class[:: = $];
        ref /rows/*::schema --> /tables/schema[:: = $];
        edge /rows/*: ::class -- ::schema;
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
fn one_container_holds_both_roles() {
    let a = tables();
    let mut roots = values(&a, "/*");
    roots.sort();
    assert_eq!(roots, vec!["entries", "rows", "tables"]);
    assert_eq!(values(&a, "/tables/* @| count"), vec!["4"]);
    assert_eq!(values(&a, "/tables/class::"), vec!["n1", "n4"]);
    assert_eq!(values(&a, "/tables/schema::"), vec!["b", "c"]);
    // children answer to their own role, and are named for it
    assert_eq!(values(&a, "/tables/class @| count"), vec!["2"]);
    assert_eq!(
        values(&a, "/tables/*:::name @| unique"),
        vec!["class", "schema"]
    );
}

#[test]
fn refs_and_edges_resolve_within_the_role() {
    let a = tables();
    assert_eq!(values(&a, "/rows/*[::w = 'дом']::class-->::"), vec!["n1"]);
    assert_eq!(values(&a, "/rows/*[::w = 'дом']--schema::"), vec!["c"]);
    // backlinks land on the alias role
    assert_eq!(
        values(&a, "/tables/class[:: = 'n1']--entry::w"),
        vec!["дом", "стол"]
    );
    // the lattice: hops label by the role they land on
    assert_eq!(
        values(&a, "/tables/class[:: = 'n1']--schema::"),
        vec!["b", "c"]
    );
    assert_eq!(
        values(&a, "/tables/schema[:: = 'b']--class::"),
        vec!["n1", "n4"]
    );
    // a locator counts within the role
    let n = match quarb::run("/tables/schema[:: = 'b']", &a).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns[0],
        _ => panic!(),
    };
    assert_eq!(a.locator(n, |_| String::new()), "/tables/schema[2]");
}

#[test]
fn a_role_declared_twice_only_adds() {
    let base = JsonAdapter::parse(r#"{"rows": [{"c": "x"}, {"c": "y"}]}"#).unwrap();
    let model = parse_model(
        r#"
        node /t/c: /rows/*::c | unique;
        node /t/c: /rows/*::c | unique;
        node /t/d: /rows/*::c | unique;
        "#,
    )
    .unwrap();
    let a = ModelAdapter::new(base, model);
    assert_eq!(values(&a, "/t/c @| count"), vec!["2"]);
    assert_eq!(values(&a, "/t/* @| count"), vec!["4"]);
}
