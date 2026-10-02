//! Seams met in the corpus-recipes round (lang/TODO.md, "Recipes:
//! the corpus round" and "The corpus reading"), closed on dev: a
//! nested path in a stage operand forks the capsa (its predicates
//! read the register and `_`), an aggregate in a pipe tail keeps the
//! nodes it permutes, and glued filters chain on the pipe.

use quarb::{AstAdapter, QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"a":[1,2,3,4],"b":[{"x":2},{"x":3}]}"#).unwrap()
}

fn values(q: &str) -> Vec<String> {
    match quarb::run(q, &doc()).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect(),
        _ => panic!("expected values"),
    }
}

#[test]
fn nested_path_predicate_reads_the_register() {
    // The spec: a body's capsae fork from the invoking capsa — so a
    // path in an operand sees the push, as the filter stage did.
    let in_predicate = values("/a/* | .v(::) | %(v = $.v; n = (^/a/*[:: < $.v] @| count))");
    let in_filter = values("/a/* | .v(::) | %(v = $.v; n = (^/a/* | [:: < $.v] @| count))");
    assert_eq!(in_predicate, in_filter);
    assert_eq!(in_predicate[3], "%(v = 4; n = 3)");
    // `_` one scope out is the capsa's node.
    assert_eq!(
        values("/b/* | %(n = (^/a/*[:: < _::x] @| count))"),
        ["%(n = 1)", "%(n = 2)"]
    );
    // A body subcontext forks as before.
    assert_eq!(
        values("/a/* | .v(::) | .n(^/a/*[:: < $.v] @| count) | %(v = $.v; n = $.n)")[2],
        "%(v = 3; n = 2)"
    );
}

#[test]
fn pipe_tail_aggregates_keep_their_nodes() {
    // `@| reverse` permutes the reached nodes; `::` after it reads
    // each of them, never the operand's own node.
    assert_eq!(
        values(r#"/a/*[:: = 4] | %(prev = ((<*){1;3} @| reverse | :: @| join(" ")))"#),
        [r#"%(prev = "1 2 3")"#]
    );
    assert_eq!(
        values("/a/*[:: = 4] | %(t = ((<*){1;3} @| top(1; ::) | ::))"),
        ["%(t = 3)"]
    );
    // Selection was already right; it stays so.
    assert_eq!(
        values(r#"/a/*[:: = 4] | %(s = ((<*){1;3} @| [1..2] | :: @| join(" ")))"#),
        [r#"%(s = "3 2")"#]
    );
}

#[test]
fn glued_filters_chain_on_the_pipe() {
    assert_eq!(values("/a/* | :: | [$_ > 1][$_ < 4] @| count"), ["2"]);
    assert_eq!(
        values("/a/* | :: | [$_ > 1][$_ < 4] @| count"),
        values("/a/* | :: | [$_ > 1] | [$_ < 4] @| count")
    );
    // Positional selection stays whole-context.
    let e = quarb::run("/a/* | :: | [$_ > 1][2]", &doc()).unwrap_err();
    assert!(e.to_string().contains("whole-context"), "{e}");
}

#[test]
fn expression_arguments_evaluate_per_capsa() {
    // `levenshtein(::alt)` reads the capsa's node; the expression
    // form hands the first argument in as the topic.
    let d = quarb_json::JsonAdapter::parse(
        r#"{"w":[{"name":"kitten","alt":"sitting"},{"name":"flaw","alt":"lawn"}],"n":[1,2,3,4,5,6,7,8,9,10]}"#,
    )
    .unwrap();
    let values = |q: &str| match quarb::run(q, &d).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect::<Vec<_>>(),
        _ => panic!("expected values"),
    };
    assert_eq!(
        values("/w/* | %(d = (::name | levenshtein(::alt)))"),
        ["%(d = 3)", "%(d = 2)"]
    );
    assert_eq!(
        values("/w/* | %(d = levenshtein(::name; ::alt))"),
        ["%(d = 3)", "%(d = 2)"]
    );
    assert_eq!(values(r#"/w/*::name | levenshtein("sitting")"#), ["3", "7"]);
    assert_eq!(values("/n/* | :: @| percentile(90)"), ["9.1"]);
    assert_eq!(
        values("/n/* | :: @| percentile(50)"),
        values("/n/* | :: @| median")
    );
    assert_eq!(values("/n/* | :: @| percentile(100)"), ["10"]);
}
