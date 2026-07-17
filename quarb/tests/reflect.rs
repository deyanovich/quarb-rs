//! Reflection tests: Quarb queries run against the arbor of a parsed
//! Quarb query (spec: Reflection — The Query Arbor).

use quarb::reflect::QueryArbor;
use quarb::{QueryResult, Value};

const QUERY: &str = "/row[::Age > 30 or /orders] @| group(::Pclass) \
                     | mean | .mean-age | %. @| sort_by($.mean-age)";

fn arbor() -> QueryArbor {
    QueryArbor::parse(QUERY).unwrap()
}

fn values(q: &str) -> Vec<Value> {
    match quarb::run(q, &arbor()).unwrap() {
        QueryResult::Values(vs) => vs,
        QueryResult::Nodes(_) => panic!("expected values"),
    }
}

fn locators(q: &str) -> Vec<String> {
    let a = arbor();
    match quarb::run(q, &a).unwrap() {
        QueryResult::Nodes(ns) => ns.into_iter().map(|n| a.locator(n)).collect(),
        QueryResult::Values(_) => panic!("expected nodes"),
    }
}

#[test]
fn structure_and_locators() {
    assert_eq!(locators("/query/branch/step"), vec!["/query/branch/step"]);
    // two pipelines-of-stages: the pipeline node groups them
    assert_eq!(
        locators("/query/pipeline/*").len(),
        5 // agg(group), func(mean), push, recall, agg(sort_by)
    );
    // same-kind siblings disambiguate positionally
    assert_eq!(
        locators("//agg"),
        vec!["/query/pipeline/agg[1]", "/query/pipeline/agg[2]"]
    );
}

#[test]
fn spellings_are_query_syntax() {
    // the step's axis and matcher read as written
    assert_eq!(values("//step[1]::axis"), vec![Value::Str("/".to_string())]);
    assert_eq!(
        values("//step[1]::matcher"),
        vec![Value::Str("row".to_string())]
    );
    // comparison operators keep their spelling
    assert_eq!(values("//compare::op"), vec![Value::Str(">".to_string())]);
    // register references too
    assert_eq!(
        values("//recall::ref @| sort"),
        vec![
            Value::Str("$.mean-age".to_string()),
            Value::Str("%.".to_string()),
        ]
    );
}

#[test]
fn default_projections() {
    // bare `::` on a func/agg is its name, on a literal its value
    assert_eq!(
        values("//func:: || //agg:: @| sort"),
        vec![
            Value::Str("group".to_string()),
            Value::Str("mean".to_string()),
            Value::Str("sort_by".to_string()),
        ]
    );
    assert_eq!(values("//literal::"), vec![Value::Int(30)]);
}

#[test]
fn predicates_reflect() {
    // the or-branch: a compare and a bare truthy path
    assert_eq!(values("//or @| count"), vec![Value::Int(1)]);
    assert_eq!(values("//or/compare @| count"), vec![Value::Int(1)]);
    assert_eq!(
        values("//or/path/step::matcher"),
        vec![Value::Str("orders".to_string())]
    );
}

#[test]
fn source_metadata_and_introspection_aggregates() {
    assert_eq!(values("^::;source"), vec![Value::Str(QUERY.to_string())]);
    // "how many stages does this query run?"
    assert_eq!(values("/query/pipeline/* @| count"), vec![Value::Int(5)]);
    // "does it use any descendant hops?" — no
    assert_eq!(
        values("//step[::axis = \"//\"] @| count"),
        vec![Value::Int(0)]
    );
}

#[test]
fn malformed_query_is_an_error() {
    assert!(QueryArbor::parse("/row[").is_err());
    assert!(QueryArbor::parse("").is_err());
}
