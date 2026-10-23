//! Position lists in brackets (spec: Index Predicates): `[2; 4; 5]`,
//! `[1; 3..4; -1]` on a hop, on the pipe, on the aggregate pipe and
//! glued to a value; brackets that look at no node (`[@(2; 4)]`,
//! `[$.pos]`, `[$.n]`) read as positions at run time; two positions
//! joined by `||` / `&&` are refused with the list spelling.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"rows":[
            {"name":"Iliad","pages":683},
            {"name":"Emma","pages":474},
            {"name":"Hamlet","pages":342},
            {"name":"Odyssey","pages":541}]}"#,
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

fn names(q: &str) -> Vec<String> {
    run(q).unwrap()
}

#[test]
fn a_position_list_on_a_hop_keeps_the_hops_order_each_once() {
    assert_eq!(names("/rows/*[1; 3] | ::name"), ["Iliad", "Hamlet"]);
    assert_eq!(names("/rows/*[3; 1] | ::name"), ["Iliad", "Hamlet"]);
    assert_eq!(
        names("/rows/*[1; 2..3; -1] | ::name"),
        ["Iliad", "Emma", "Hamlet", "Odyssey"]
    );
    assert_eq!(names("/rows/*[2; 2] | ::name"), ["Emma"]);
    assert_eq!(names("/rows/*[9; 2] | ::name"), ["Emma"]);
    // a list after the hop's filtering, as `[n]` is
    assert_eq!(
        names("/rows/*[::pages > 400][1; -1] | ::name"),
        ["Iliad", "Odyssey"]
    );
    for q in ["/rows/*[2 || 3] | ::name", "/rows/*[2 && 3] | ::name"] {
        let e = run(q).unwrap_err().to_string();
        assert!(e.contains("write '[2; 3]'"), "{q}: {e}");
    }
}

#[test]
fn brackets_that_look_at_no_node_are_positional_at_run_time() {
    assert_eq!(names("/rows/*[@(1; 3)] | ::name"), ["Iliad", "Hamlet"]);
    assert_eq!(names("/rows/*[@(3; 1)] | ::name"), ["Iliad", "Hamlet"]);
    assert_eq!(names("/rows/*[1 + 1] | ::name"), ["Emma"]);
    assert_eq!(
        names("= @(1; 3) | .pos | /rows/*[$.pos] | ::name"),
        ["Iliad", "Hamlet"]
    );
    assert_eq!(names("= 2 | .n | /rows/*[$.n] | ::name"), ["Emma"]);
    // an expression that reads the node stays a condition
    assert_eq!(names("/rows/*[::pages] | ::name").len(), 4);
}

#[test]
fn the_same_brackets_on_the_pipes_and_on_values() {
    assert_eq!(names("/rows/* | ::name @| [1; 3]"), ["Iliad", "Hamlet"]);
    assert_eq!(names("/rows/* | ::name @| [@(3; 1)]"), ["Iliad", "Hamlet"]);
    assert_eq!(
        names("= @(1; 3) | .p | /rows/* | ::name @| [$.p]"),
        ["Iliad", "Hamlet"]
    );
    // a list value on the pipe and glued
    assert_eq!(
        names("/rows/* | ::name @| @ | [1; 3]"),
        ["@(\"Iliad\"; \"Hamlet\")"]
    );
    assert_eq!(
        names("/rows/* | ::name @| @ | [@(3; 1)]"),
        ["@(\"Iliad\"; \"Hamlet\")"]
    );
    assert_eq!(
        names("/rows/* | ::name @| @ | $_[3; 1; 1]"),
        ["@(\"Iliad\"; \"Hamlet\")"]
    );
    assert_eq!(
        names("/rows/* | ::name @| @ | $_[1; 3]"),
        ["@(\"Iliad\"; \"Hamlet\")"]
    );
    assert_eq!(
        names("= @(1; 3) | .p | /rows/* | ::name @| @ | $_[$.p]"),
        ["@(\"Iliad\"; \"Hamlet\")"]
    );
    // a table: the listed rows on the pipe; glued, two coordinates
    // stay the cell, and a list form in either coordinate selects
    let t = "/rows/* | %(::name; ::pages) @| #";
    let two = "#(@(name; pages);\n  %(\"Iliad\"; 683);\n  %(\"Hamlet\"; 342))";
    assert_eq!(names(&format!("{t} | [1; 3]")), [two]);
    // three written coordinates on a table are refused: it has two
    let e = run(&format!("{t} | $_[1; 2; 3]")).unwrap_err().to_string();
    assert!(e.contains("two coordinates"), "{e}");
    assert_eq!(names(&format!("{t} | [@(3; 1)]")), [two]);
    assert_eq!(names(&format!("{t} | $_[@(1; 3)]")), [two]);
    assert_eq!(names(&format!("{t} | $_[2; 1]")), ["Emma"]);
    assert_eq!(names(&format!("{t} | $_[@(1; 3); 2]")), ["@(683; 342)"]);
    assert_eq!(
        names(&format!("{t} | $_[2; @(1; 2)]")),
        ["%(name = \"Emma\"; pages = 474)"]
    );
    assert_eq!(
        names(&format!("{t} | $_[..2; @(2)]")),
        ["#(@(pages);\n  %(683);\n  %(474))"]
    );
    // a register holding no position is the condition it always
    // was: null keeps no row
    assert_eq!(names(&format!("{t} | [$.nope]")), ["#(@(name; pages))"]);
}
