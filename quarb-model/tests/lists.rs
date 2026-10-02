//! A list-valued property as a reference source: a row's marks
//! resolve per element — `-->` lands on the first, the hop forks
//! one per element, every element backlinks, and an edge over a
//! list field connects the whole product.

use quarb::AstAdapter as _;
use quarb_json::JsonAdapter;
use quarb_model::{ModelAdapter, parse_model};

fn marks() -> ModelAdapter<JsonAdapter> {
    let base = JsonAdapter::parse(
        r#"{"rows": [
            {"w": "дом",  "marks": ["①", "②"]},
            {"w": "стол", "marks": ["②"]},
            {"w": "еда",  "marks": []}
        ]}"#,
    )
    .unwrap();
    let model = parse_model(
        r#"
        node /entries/entry: /rows/*;
        node /words/word:    /rows/*::w | unique;
        node /marks/mark:    /rows/*::marks | ... | unique;
        ref /rows/*::w     --> words;
        ref /rows/*::marks --> marks;
        edge /rows/*: ::w -- ::marks;
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
fn a_list_property_elevates_through_the_spread() {
    let a = marks();
    assert_eq!(values(&a, "/marks/* @| count"), vec!["2"]);
    assert_eq!(values(&a, "/marks/mark::"), vec!["①", "②"]);
}

#[test]
fn a_list_property_resolves_per_element() {
    let a = marks();
    // `-->` lands on the first element
    assert_eq!(values(&a, "/rows/*[::w = 'дом']::marks-->::"), vec!["①"]);
    // the hop forks, one landing per element
    assert_eq!(values(&a, "/rows/*[::w = 'дом']--mark::"), vec!["①", "②"]);
    // every element backlinks, on the alias role
    assert_eq!(
        values(&a, "/marks/mark[:: = '②']--entry::w"),
        vec!["дом", "стол"]
    );
    assert_eq!(values(&a, "/marks/mark[:: = '①']--entry::w"), vec!["дом"]);
    // an empty list refers to nothing
    assert_eq!(values(&a, "/rows/*[::w = 'еда']--mark @| count"), vec!["0"]);
}

#[test]
fn an_edge_over_a_list_field_connects_the_product() {
    let a = marks();
    assert_eq!(
        values(&a, "/words/word[:: = 'дом']--mark::"),
        vec!["①", "②"]
    );
    assert_eq!(
        values(&a, "/marks/mark[:: = '②']--word::"),
        vec!["дом", "стол"]
    );
    assert_eq!(
        values(&a, "/words/word[:: = 'еда']--mark @| count"),
        vec!["0"]
    );
}

/// A list operand reads as a set: `=` is membership, `!=` its
/// complement — so a join condition against a list-valued field
/// finds the partners the list names.
#[test]
fn equality_against_a_list_is_membership() {
    let a = marks();
    assert_eq!(values(&a, "/rows/*[::marks = '②']::w"), vec!["дом", "стол"]);
    assert_eq!(values(&a, "/rows/*[::marks != '②']::w"), vec!["еда"]);
    assert_eq!(values(&a, "/rows/*['①' = ::marks]::w"), vec!["дом"]);
    // a join keyed on a list-valued field: a row meets every mark
    // node its list names, and the outer join's misses are the rows
    // whose list names none
    assert_eq!(
        values(&a, "/rows/* <=> /marks/mark[:: = _::marks] | ::w"),
        vec!["дом", "стол"]
    );
    assert_eq!(
        values(
            &a,
            "/rows/* <=>? /marks/mark[:: = _::marks] | [!$$1::] | ::w"
        ),
        vec!["еда"]
    );
}
