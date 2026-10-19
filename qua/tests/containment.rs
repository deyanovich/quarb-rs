//! The containment comparison `<<=` / `>>=`, the period's reading
//! as its first instant, the retired set reading of `=` over a
//! list, and comparison chains (spec: Containment, Chains in
//! Predicates).
use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"rows":[
            {"id":1,"n":12,"tags":["a","b"],"when":"2025-10-06T14:30:00Z"},
            {"id":2,"n":25,"tags":["c"],"when":"2025-10-31"},
            {"id":3,"n":7,"tags":[],"when":"2025-11-02"},
            {"id":4,"n":20,"tags":["a"],"when":"2024"}]}"#,
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

fn ids(pred: &str) -> String {
    run(&format!("/rows/*[{pred}]::id")).unwrap().join(" ")
}

fn one(q: &str) -> String {
    let v = run(q).unwrap();
    assert_eq!(v.len(), 1, "{q}: {v:?}");
    v.into_iter().next().unwrap()
}

#[test]
fn a_value_lies_within_a_list_when_it_equals_an_item() {
    assert_eq!(ids(r#""a" <<= ::tags"#), "1 4");
    assert_eq!(ids(r#"::tags >>= "c""#), "2");
    assert_eq!(ids(r#"!("a" <<= ::tags)"#), "2 3");
    assert_eq!(ids("::n <<= @(7; 25)"), "2 3");
    // a list within a list is an item, never a sub-sequence
    assert_eq!(ids(r#"::tags >>= @("a")"#), "");
    // a value that is no container contains itself: equality
    assert_eq!(ids(r#""a" <<= "abc""#), "");
    assert_eq!(one(r#"= ("e5152" <<= "e5152")"#), "true");
    assert_eq!(one(r#"= ("e515" <<= "e5152")"#), "false");
    assert_eq!(one("= (3 <<= 3)"), "true");
    assert_eq!(one("= (3 >>= 3)"), "true");
    assert_eq!(ids("::n <<= 12"), "1");
    assert_eq!(ids("::n <<= 100"), "");
    // null holds nothing and lies in nothing
    assert_eq!(ids("::n <<= null"), "");
    assert_eq!(ids("null <<= ::tags"), "");
    assert_eq!(one("= (null <<= null)"), "false");
}

#[test]
fn a_range_is_the_set_of_the_values_it_yields() {
    assert_eq!(ids("::n <<= 10..20"), "1 4");
    assert_eq!(ids("10..20 >>= ::n"), "1 4");
    assert_eq!(one("= (12 <<= 10..20)"), "true");
    assert_eq!(one("= (12.5 <<= 10..20)"), "false");
    assert_eq!(one("= (12 <<= 10..20..5)"), "false");
    assert_eq!(one("= (15 <<= 10..20..5)"), "true");
}

#[test]
fn a_period_holds_what_its_span_covers() {
    assert_eq!(ids("::when <<= 2025-10"), "1 2");
    assert_eq!(ids("2025-10 >>= ::when"), "1 2");
    assert_eq!(ids("::when <<= (2025 | period)"), "1 2 3");
    assert_eq!(ids("::when <<= \"2025\""), "1 2 3");
    assert_eq!(ids("(::when | period) <<= 2025-10"), "1 2");
    assert_eq!(one("= (2025-10-06 <<= ('2025-10' | period))"), "true");
    assert_eq!(
        one("= (('2025-10' | period) <<= ('2025-10' | period))"),
        "true"
    );
    assert_eq!(one("= (('2025-10' | period) <<= 2025-10-06)"), "false");
    assert_eq!(one("= (2025-10-06T14:30:00Z <<= 2025-10-06)"), "true");
    assert_eq!(one("= (2025-11-01 <<= ('2025-10' | period))"), "false");
}

#[test]
fn a_period_reads_as_its_first_instant() {
    assert_eq!(one("= (('2025-10' | period) = 2025-10-01)"), "true");
    assert_eq!(one("= (('2025-10' | period) = 2025-10-15)"), "false");
    assert_eq!(one("= (('2025-10' | period) < 2025-10-15)"), "true");
    assert_eq!(one("= (('2025-10' | period) <= 2025-10-01)"), "true");
    assert_eq!(one("= (('2025' | period) = 2025-01-01T00:00:00Z)"), "true");
}

#[test]
fn the_set_reading_of_equality_over_a_list_is_retired() {
    let err = run(r#"/rows/*[::tags = "a"]::id"#).unwrap_err().to_string();
    assert!(
        err.contains("retired") && err.contains("VALUE <<= LIST"),
        "{err}"
    );
    let err = run(r#"/rows/*[::tags != "a"]::id"#)
        .unwrap_err()
        .to_string();
    assert!(err.contains("retired"), "{err}");
    // a list equals a list, element by element
    assert_eq!(ids(r#"::tags = @("a"; "b")"#), "1");
    assert_eq!(ids("::tags = @()"), "3");
    // null beside a list is unequal
    assert_eq!(ids("::tags = null"), "");
    assert_eq!(ids("::tags != null"), "1 2 3 4");
    // the ordering, pattern and substring comparisons still read any item
    assert_eq!(ids(r#"::tags == (/^a$/)"#), "1 4");
    assert_eq!(ids(r#"::tags *= "c""#), "2");
}

#[test]
fn comparisons_chain_with_the_middle_evaluated_once() {
    assert_eq!(ids("10 <= ::n <= 20"), "1 4");
    assert_eq!(ids("10 < ::n < 20"), "1");
    assert_eq!(ids("7 <= ::n <= 12 <= 20"), "1 3");
    assert_eq!(ids(r#""a" <<= ::tags >>= "b""#), "1");
    assert_eq!(ids("::when <<= 2025-10 <<= (2025 | period)"), "1 2");
    assert_eq!(ids("::n = 12 = 12"), "1");
    assert_eq!(ids("::n != 12 != 7"), "2 3 4");
    assert_eq!(one("= (1 < 2 < 3)"), "true");
    assert_eq!(one("= (1 < 3 < 2)"), "false");
    assert_eq!(one("= (1 < 2 >= 2 = 2)"), "true");
    // a chain sits where one comparison sits
    assert_eq!(one("= (1 < 2 < 3 && 3 < 4)"), "true");
    assert_eq!(one("= ((1 < 2 < 3) || 5 < 4)"), "true");
    // the middle operand runs once: a counter would show twice
    assert_eq!(one("= (0 <= (1 + 1) <= 2)"), "true");
    // a pattern comparison chains like any other: the middle operand
    // is the pattern's subject
    assert_eq!(ids(r#""a" <<= ::tags == (/^b$/)"#), "1");
    assert_eq!(ids(r#""a" <<= ::tags == (/x/)"#), "");
}

#[test]
fn a_chain_reflects_as_one_node_and_prints_back() {
    let q = r#"/rows/*[10 <= ::n <= 20]::id"#;
    let doc = doc();
    let parsed = quarb::Query::parse(q, &doc).unwrap();
    assert_eq!(parsed.canonical(), q);
    let out = run("/rows/*[10 <= ::n <= 20 && ::n != 15]::id").unwrap();
    assert_eq!(out, vec!["1", "4"]);
}
