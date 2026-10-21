//! Ruling 2 (spec draft.164): no first-value rule anywhere. An
//! operand of a binary operator that reaches several values forks
//! the expression (two forking operands give the direct product,
//! the left changing slowest; an operand that reaches nothing is
//! null); constructors gather; a nested call's argument reaches the
//! command as its input would (a reducer gathers, a scalar command
//! forks); the numeric reducers reduce a record's values and a
//! table's columns, `count` counts fields and rows.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"[{"k":"a","n":1,"x":1,"y":10},{"k":"b","n":2,"x":2,"y":20},{"k":"a","n":3,"x":3,"y":30}]"#,
    )
    .unwrap()
}

fn run(q: &str) -> quarb::Result<Vec<String>> {
    let run = quarb::Run::ambient();
    let out = quarb::Query::parse(q, &doc())?.run(&doc(), &run)?;
    Ok(match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::display_form).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    })
}

fn rows(q: &str) -> Vec<String> {
    run(q).unwrap_or_else(|e| panic!("{q}: {e}"))
}

fn one(q: &str) -> String {
    let out = rows(q);
    assert_eq!(out.len(), 1, "{q}: {out:?}");
    out.into_iter().next().unwrap()
}

fn refuse(q: &str) -> String {
    match run(q) {
        Err(e) => e.to_string(),
        Ok(out) => panic!("{q} passed: {out:?}"),
    }
}

#[test]
fn a_forking_operand_forks_the_expression() {
    assert_eq!(rows("= 3 | (10 + /*::n)"), ["11", "12", "13"]);
    assert_eq!(rows("= 3 | (/*::n * 2)"), ["2", "4", "6"]);
    assert_eq!(rows("= 3 | (- /*::n)"), ["-1", "-2", "-3"]);
    assert_eq!(rows(r#"= 3 | ("a" ++ /*::k)"#), ["aa", "ab", "aa"]);
    // the direct product, the left operand changing slowest
    assert_eq!(
        rows("= 3 | (/*::x + /*::y)"),
        ["11", "21", "31", "12", "22", "32", "13", "23", "33"]
    );
    // forked once at the pipe, the pairs in step
    assert_eq!(rows("/* | (::x + ::y)"), ["11", "22", "33"]);
    // an operand that reaches nothing is null, one value
    assert_eq!(rows("= 3 | (10 + /zzz::n)"), [""]);
    assert_eq!(one("= 3 | (/zzz::n ?? 0)"), "0");
    // comparisons keep their existential reading
    assert_eq!(one("= 3 | (/*::n = 2)"), "true");
}

#[test]
fn constructors_gather() {
    assert_eq!(one("= 3 | @(/*::n)"), "@(1; 2; 3)");
    assert_eq!(one("= 3 | @(/*::n) * 2"), "@(2; 4; 6)");
    assert_eq!(one("= 3 | @(/*::n + 1)"), "@(2; 3; 4)");
    assert_eq!(one("= 3 | %(n = /*::n)"), "%(n = @(1; 2; 3))");
    assert_eq!(one("= 3 | %(n = /*::n * 10)"), "%(n = @(10; 20; 30))");
}

#[test]
fn a_nested_call_reads_its_argument_by_input_type() {
    // a reducer gathers the values into one list
    assert_eq!(one("= 3 | (sum(/*::n))"), "6");
    assert_eq!(one("= 3 | (count(/*))"), "3");
    assert_eq!(one("= 3 | (10 + sum(/*::n))"), "16");
    assert_eq!(one("= 3 | (sort(/*::n))"), "@(1; 2; 3)");
    assert_eq!(one("= 3 | (count(/zzz))"), "0");
    assert_eq!(one("= 3 | (sum(@(1; 2)))"), "3");
    // on a node thread the root is `^/`: a bare `/*` is the node's children
    assert_eq!(
        rows("/* | %(::k; total = sum(^/*::n))"),
        [r#"%(k = "a"; total = 6)"#, r#"%(k = "b"; total = 6)"#, r#"%(k = "a"; total = 6)"#]
    );
    // a scalar command takes them one at a time and forks
    assert_eq!(rows("= 3 | (upper(/*::k))"), ["A", "B", "A"]);
    assert_eq!(rows("= 3 | (substr(/*::k; 1))"), ["a", "b", "a"]);
    // the sub-pipeline and the aggregate form are untouched
    assert_eq!(rows("= 3 | (/*::n | sum)"), ["1", "2", "3"]);
    assert_eq!(one("= 3 | (/*::n @| sum)"), "6");
    // round trip
    let printed = quarb::Query::parse("= 3 | (10 + sum(/*::n))", &doc()).unwrap().canonical();
    let again = quarb::Query::parse(&printed, &doc()).unwrap().canonical();
    assert_eq!(printed, again);
}

#[test]
fn reducers_over_a_record_and_a_table() {
    assert_eq!(one("= %(a = 1; b = 2) | sum"), "3");
    assert_eq!(one("= %(a = 1; b = 2) | max"), "2");
    assert_eq!(one("= %(a = 1; b = 2) | count"), "2");
    assert_eq!(
        one("= #(%(a = 1; b = 2); %(a = 3; b = 4)) | sum"),
        "%(a = 4; b = 6)"
    );
    assert_eq!(
        one("= #(%(a = 1; b = 2); %(a = 3; b = 4)) | mean"),
        "%(a = 2; b = 3)"
    );
    assert_eq!(
        one(r#"= #(%(a = 1; b = "x"); %(a = 3; b = "y")) | sum"#),
        "%(a = 4; b = null)"
    );
    assert_eq!(one("= #(%(a = 1; b = 2); %(a = 3; b = 4)) | count"), "2");
    assert_eq!(one("= 7 | count"), "1");
    assert_eq!(one("= @(1; 2) | count"), "2");
    // with --of the reducers reduce per row, per value
    assert_eq!(one("= #(%(a = 1; b = 2); %(a = 3; b = 4)) | sum(--of=:a * :b)"), "14");
    assert_eq!(one("= %(a = 1; b = 2) | sum(--of=$_ * 2)"), "6");
    // the other readings of a table under a reducer stay
    let e = refuse("= #(%(a = 1; b = 2)) | join");
    assert!(e.contains("is a table"), "{e}");
}
