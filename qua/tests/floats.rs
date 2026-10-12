//! The float values nan, inf and -inf (spec: the float values):
//! IEEE 754 arithmetic yields them, they print as Python prints
//! them, the text reads back as the number, nan equals nothing and
//! orders after every number, and JSON spells them null.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"v":[{"x":"3"},{"x":"nan"},{"x":"1"},{"x":"inf"},{"x":"-inf"}]}"#)
        .unwrap()
}

fn run(q: &str) -> Vec<String> {
    let out = quarb::Query::parse(q, &doc())
        .unwrap()
        .run(&doc(), &quarb::Run::ambient())
        .unwrap();
    match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::display_form).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    }
}

#[test]
fn division_by_zero_and_the_three_spellings() {
    assert_eq!(run("= 1 div 0"), ["inf"]);
    assert_eq!(run("= -1 div 0"), ["-inf"]);
    assert_eq!(run("= 0 div 0"), ["nan"]);
    assert_eq!(run("= 7 idiv 0"), ["nan"]);
    assert_eq!(run("= 7 mod 0"), ["nan"]);
    assert_eq!(run("= 7.5 mod 0"), ["nan"]);
    // the text reads back as the number
    assert_eq!(run(r#"= "inf" + 1"#), ["inf"]);
    assert_eq!(run(r#"= "nan" * 2"#), ["nan"]);
    assert_eq!(run("/v/*[::x > 2]::x"), ["3", "inf"]);
}

#[test]
fn nan_equals_nothing_and_orders_last() {
    // `[::x = ::x]` drops the nan
    assert_eq!(run("/v/*[::x = ::x]::x"), ["3", "1", "inf", "-inf"]);
    assert_eq!(run("= 0 div 0 | [$_ = $_]"), Vec::<String>::new());
    assert_eq!(run("= 1 div 0 | [$_ = $_]"), ["inf"]);
    // the order: -inf first, nan after every number
    assert_eq!(run("/v/*::x @| sort"), ["-inf", "1", "3", "inf", "nan"]);
    // JSON has no spelling: null
    assert_eq!(run("= @(1; 1 div 0; 0 div 0) | json"), ["[1, null, null]"]);
}
