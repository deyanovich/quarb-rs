//! Fragments with parameters, run (spec: Fragments, call by name):
//! a form parameter stands where the form it binds would stand — as
//! a literal count the command reads, as a slice bound, as a group's
//! key, as a bare record field — and the run answers as the written
//! query does.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"rows":[
            {"name":"Iliad","pages":683},
            {"name":"Emma","pages":474},
            {"name":"Odyssey","pages":541}]}"#,
    )
    .unwrap()
}

fn rows(q: &str) -> Vec<String> {
    let run = quarb::Run::ambient();
    let out = quarb::Query::parse(q, &doc())
        .unwrap_or_else(|e| panic!("{q}: {e}"))
        .run(&doc(), &run)
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::display_form).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    }
}

fn one(q: &str) -> String {
    let v = rows(q);
    assert_eq!(v.len(), 1, "{q}: {v:?}");
    v.into_iter().next().unwrap()
}

#[test]
fn a_bound_literal_is_the_count_the_command_reads() {
    assert_eq!(
        one("def &top($n): @| first($n) @| count; /rows/* @| &top(2)"),
        "2"
    );
    assert_eq!(
        one("def &tail($n): @| last($n) @| count; /rows/* @| &tail(2)"),
        "2"
    );
    assert_eq!(
        rows("def &band($r): @| first($r) | ::name; /rows/* @| &band(2..3)"),
        vec!["Emma", "Odyssey"]
    );
}

#[test]
fn a_bound_integer_is_the_slice_bound() {
    assert_eq!(one("def &f($n): @| [..$n] @| count; /rows/* @| &f(2)"), "2");
    assert_eq!(one("def &f($n): @| [$n..] @| count; /rows/* @| &f(3)"), "1");
    assert_eq!(one("def &g($n): /rows/*[..$n] @| count; &g(1)"), "1");
}

#[test]
fn a_bound_form_names_a_key_and_a_field() {
    assert_eq!(
        one("def &per($key): @| group($key) @| count; /rows/* @| &per(::pages)"),
        "3"
    );
    assert_eq!(
        rows("def &card($extra): | %(::name; $extra) | :pages; /rows/* | &card(::pages)"),
        vec!["683", "474", "541"]
    );
}
