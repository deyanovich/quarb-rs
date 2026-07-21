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
    assert_eq!(values("^;;;source"), vec![Value::Str(QUERY.to_string())]);
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

#[test]
fn recall_resolution_is_scope_aware() {
    let vals = |src: &str, q: &str| -> Vec<Value> {
        let a = QueryArbor::parse(src).unwrap();
        match quarb::run(q, &a).unwrap() {
            QueryResult::Values(vs) => vs,
            QueryResult::Nodes(_) => panic!("expected values"),
        }
    };
    // A site buried in a subcontext body binds the *inner* scope:
    // the outer `$.2` numbers past it to `.b`, while the body's own
    // `$.tmp` still resolves inside.
    let src = r#"/row | .s(/y | .tmp(::z * 1) | $.tmp) | .b(::w * 1) | rec("n", $.2)"#;
    assert_eq!(
        vals(src, "//recall[::ref = '$.2']::ref~>::name"),
        vec![Value::Str("b".to_string())]
    );
    assert_eq!(
        vals(src, "//recall[::ref = '$.tmp']::ref~>::name"),
        vec![Value::Str("tmp".to_string())]
    );
    // `$.` means the latest preceding site *in scope* — the
    // subcontext itself, not the last site inside its body.
    let src = r#"/row | .s(/y | .tmp(::z * 1) | $.tmp) | rec("z", $.)"#;
    assert_eq!(
        vals(src, "//recall[::ref = '$.']::ref~>::name @| last"),
        vec![Value::Str("s".to_string())]
    );
    // `$$.d` climbs one register scope out of the body.
    let src = r#"/row | .d(::dept * 1) | .m(^/row[::dept = $$.d] @| count) | $.m"#;
    assert_eq!(
        vals(src, "//outer/recall::ref~>::name"),
        vec![Value::Str("d".to_string())]
    );
    // `$*1` inside a subcontext body of a correlated query means
    // the outer correlation's first operand, not the body's own
    // branches: both context refs resolve to the /a/* branch (one
    // deduped node — before the fix the inner one resolved to
    // /c/*, and this printed ["a", "c"]).
    let src = r#"/a/* <=> /b/*[::x = $*1::x] | .s(/c/*[::y = $*1::y] @| count) | $.s"#;
    assert_eq!(
        vals(src, "//context::index~>/step[1]::matcher"),
        vec![Value::Str("a".to_string())]
    );
}

#[test]
fn recalls_resolve_to_expr_push_sites() {
    // Expression pushes bind a regula at runtime exactly as plain
    // pushes and named subcontexts do, so they must count as
    // definition sites — both for name lookup and `$.N` numbering.
    let a = QueryArbor::parse(r#"/row | .total(::price * ::qty) | rec("t", $.total)"#).unwrap();
    let resolved = match quarb::run("//recall::ref~>::name", &a).unwrap() {
        QueryResult::Values(vs) => vs,
        QueryResult::Nodes(_) => panic!("expected values"),
    };
    assert_eq!(resolved, vec![Value::Str("total".to_string())]);

    let a = QueryArbor::parse(r#"/row | .a(::x * 1) | .b(::y) | rec("n", $.1)"#).unwrap();
    let resolved = match quarb::run("//recall::ref~>::name", &a).unwrap() {
        QueryResult::Values(vs) => vs,
        QueryResult::Nodes(_) => panic!("expected values"),
    };
    assert_eq!(resolved, vec![Value::Str("a".to_string())]);
}
