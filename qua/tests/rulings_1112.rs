//! Two rulings of 2026-11-12. A reducer's grouping option `--by`
//! after the aggregation pipe is refused, the refusal naming both
//! right spellings: it groups a table's rows on the plain pipe, and
//! after `@|` the reduction runs over the threads, where the fold
//! groups. `| reverse` on a record answers its fields in reverse
//! order, as `sort` orders them.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"novels": [
            {"author": "Austen", "title": "Emma", "year": 1815},
            {"author": "Dickens", "title": "Bleak House", "year": 1853},
            {"author": "Austen", "title": "Persuasion", "year": 1817},
            {"author": "Dickens", "title": "Hard Times", "year": 1854},
            {"author": "Austen", "title": "Mansfield Park", "year": 1814}
        ]}"#,
    )
    .unwrap()
}

fn values(q: &str) -> Vec<String> {
    match quarb::run(q, &doc()).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryResult::Values(vs) => vs.iter().map(Value::to_quarb).collect(),
        other => panic!("{q}: expected values, got {other:?}"),
    }
}

fn refused(q: &str) -> String {
    match quarb::run(q, &doc()) {
        Ok(v) => panic!("{q}: expected a refusal, got {v:?}"),
        Err(e) => e.to_string(),
    }
}

const WHY: &str = "--by groups a table's rows, and after '@|' the reduction runs over the threads";

/// The option was read by nothing after `@|`: the threads were
/// counted, or the one thread that holds the table.
#[test]
fn a_reducer_s_by_after_the_aggregation_pipe_is_refused() {
    let e = refused("/novels/* @| count(--by=::author)");
    assert!(
        e.contains(&format!(
            "{WHY}: write '| count(--by=:author)' on the table, or group the threads with \
             the fold, '| .author(::author) @|{{.author}} count'"
        )),
        "{e}"
    );
    // the short spelling, a table's column, the table held by one
    // thread
    for q in [
        "/novels/* @| count(-b=::author)",
        "/novels/* @| # @| count(--by=:author)",
        "/novels/* @| # @| mean(--by=:author)",
    ] {
        let e = refused(q);
        assert!(e.contains(WHY), "{q}: {e}");
        assert!(e.contains("'| .author("), "{q}: {e}");
    }
    // the reduction's other arguments stay with it in the fold
    let e = refused("/novels/* @| sum(--of=::year --by=::author)");
    assert!(
        e.contains(
            "write '| sum(… --by=:author)' on the table, or group the threads with the fold, \
             '| .author(::author) @|{.author} sum(--of=::year)'"
        ),
        "{e}"
    );
    let e = refused("/novels/* @| percentile(50; --of=::year --by=::author)");
    assert!(
        e.contains("'| .author(::author) @|{.author} percentile(50; --of=::year)'"),
        "{e}"
    );
    // several keys, or a key that is no one name: the spellings in
    // general
    let e = refused("/novels/* @| count(--by=(::author; ::year))");
    assert!(
        e.contains(
            "write '| count(--by=:col)' on the table, or group the threads with the fold, \
             '| .k(KEY) @|{.k} count'"
        ),
        "{e}"
    );
    // every reducer that takes the option, and under a fold as well
    for reducer in [
        "count", "sum", "product", "min", "max", "mean", "avg", "median", "std", "stddev", "var",
        "variance",
    ] {
        let e = refused(&format!("/novels/* @| {reducer}(--by=::author)"));
        assert!(e.contains(WHY), "{reducer}: {e}");
    }
    let e = refused("/novels/* | .a(::author) @|{.a} count(--by=::year)");
    assert!(e.contains(WHY), "{e}");
}

/// The two spellings the refusal names answer the grouped
/// reduction, and agree.
#[test]
fn the_two_spellings_group() {
    assert_eq!(
        values("/novels/* @| # | count(--by=:author)"),
        [r#"#(@(author; count); %("Austen"; 3); %("Dickens"; 2))"#]
    );
    assert_eq!(
        values("/novels/* | .author(::author) @|{.author} count | %($.author; n = $_)"),
        [
            r#"%(author = "Austen"; n = 3)"#,
            r#"%(author = "Dickens"; n = 2)"#
        ]
    );
    assert_eq!(
        values("/novels/* | .author(::author) @|{.author} sum(--of=::year)"),
        ["5446", "3707"]
    );
    // the reduction over the threads, without the option, as ever
    assert_eq!(values("/novels/* @| count"), ["5"]);
    assert_eq!(values("/novels/* @| sum(--of=::year)"), ["9153"]);
}

/// `reverse` on a record: its fields in reverse order, keys kept.
/// It answered the record unchanged.
#[test]
fn reverse_reverses_a_record_s_fields() {
    assert_eq!(values("= %(b = 2; a = 1) | reverse"), ["%(a = 1; b = 2)"]);
    assert_eq!(
        values("= %(b = 2; a = 1; c = 3) | reverse | keys"),
        [r#"@("c"; "a"; "b")"#]
    );
    assert_eq!(values("= %() | reverse"), ["%()"]);
    assert_eq!(values("= %(only = 1) | reverse"), ["%(only = 1)"]);
    // twice is the record again; after `sort` it is the descending
    // order of the keys
    assert_eq!(
        values("= %(b = 2; a = 1; c = 3) | reverse | reverse"),
        ["%(b = 2; a = 1; c = 3)"]
    );
    assert_eq!(
        values("= %(b = 2; a = 1; c = 3) | sort | reverse"),
        ["%(c = 3; b = 2; a = 1)"]
    );
    // a list of records reverses its items, each record as it was;
    // `map` reaches the records
    assert_eq!(
        values("= @(%(b = 2; a = 1); %(c = 3)) | reverse"),
        ["@(%(c = 3); %(b = 2; a = 1))"]
    );
    assert_eq!(
        values("= @(%(b = 2; a = 1); %(c = 3)) | map(reverse)"),
        ["@(%(a = 1; b = 2); %(c = 3))"]
    );
    // a list, a table's rows and a scalar as before
    assert_eq!(values("= @(1; 2; 3) | reverse"), ["@(3; 2; 1)"]);
    assert_eq!(
        values("= @(%(n = 1); %(n = 2)) | # | reverse | :n"),
        ["@(2; 1)"]
    );
    assert_eq!(values(r#"= "abc" | reverse"#), [r#""abc""#]);
}
