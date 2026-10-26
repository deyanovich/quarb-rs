//! The projection operator (spec: The Projection Operator, draft
//! 166): a constructor after a glued colon applied to the value on
//! its left; the spread item `%(:props...)`; `project` retired.

use quarb::{Defs, QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"rows":[
            {"name":"Iliad","pages":683},
            {"name":"Emma","pages":474}]}"#,
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

fn one(q: &str) -> String {
    let v = run(q).unwrap();
    assert_eq!(v.len(), 1, "{q}: {v:?}");
    v.into_iter().next().unwrap()
}

/// The query unparses to text that parses back to the same text.
fn fixpoint(q: &str) -> String {
    let once = quarb::expand(q, &Defs::default()).unwrap();
    let twice = quarb::expand(&once, &Defs::default()).unwrap();
    assert_eq!(once, twice, "{q}");
    once
}

const T: &str = "= #(@(A; B); %(1; null); %(3; 4))";

#[test]
fn the_shape_after_the_colon_reads_on_the_value() {
    assert_eq!(one(&format!("{T} | $_:@(:A; 1)")), "@(@(1; 3); 1)");
    assert_eq!(
        one(&format!("{T} | $_:%(n = :A; k = :B; fixed = 1)")),
        "%(n = @(1; 3); k = @(null; 4); fixed = 1)"
    );
    // a bare projection keys its own field
    assert_eq!(
        one(&format!("{T} | $_:%(:A; :B)")),
        "%(A = @(1; 3); B = @(null; 4))"
    );
    // the `#` shape: the table of the named columns, still a table
    assert_eq!(one(&format!("{T} | $_:#(:A)")), "#(@(A);\n  %(1);\n  %(3))");
    assert_eq!(
        one(&format!("{T} | $_[..1]:#(:A; s = :B)")),
        "#(@(A; s);\n  %(1; null))"
    );
    assert_eq!(
        one(&format!("{T} | $_[2]:#(:A; :B)")),
        "#(@(A; B);\n  %(3; 4))"
    );
    // on a record and on nodes: one row of the readings
    assert_eq!(one("= %(A = 1; B = 2) | $_:#(:A)"), "#(@(A);\n  %(1))");
    assert_eq!(
        run("/rows/*:#(:index; ::name)").unwrap(),
        [
            "#(@(index; name);\n  %(1; \"Iliad\"))",
            "#(@(index; name);\n  %(2; \"Emma\"))"
        ]
    );
    // (a JSON array names its items by position)
    assert_eq!(
        one("/rows/*:%(:name; :index) @| #"),
        "#(@(name; index);\n  %(0; 1);\n  %(1; 2))"
    );
}

#[test]
fn a_register_or_an_expression_supplies_the_structure() {
    // a structure built where its projections had nothing to read
    // keeps them as projectors, read at the operator
    assert_eq!(one("= %(:A; :B)"), "%(A = :A; B = :B)");
    assert_eq!(one("= @(:A; :B)"), "@(:A; :B)");
    assert_eq!(one("= :name"), ":name");
    assert_eq!(
        one("= %(:A; :B) | .p | %(A = 1; B = 2) | $_:$.p"),
        "%(A = 1; B = 2)"
    );
    assert_eq!(
        one("= %(:A; :B) | .p | %(A = 1; B = 2) | $_:($.p)"),
        "%(A = 1; B = 2)"
    );
    assert_eq!(
        one("= %(A = 1; B = 2) | .t | %(:A; :B) | .p | $.t:$.p"),
        "%(A = 1; B = 2)"
    );
    assert_eq!(
        one(&format!("= %(:A; :B) | .p | {} | .t | $.t[2]:$.p", &T[2..])),
        "%(A = 3; B = 4)"
    );
    // every rung survives at the head (it has no cursor)
    assert_eq!(one("= @(:index; ::name; :::k)"), "@(:index; ::name; :::k)");
    assert_eq!(
        one("= @(:index; ::name) | .cols | /rows/*:$.cols @| #"),
        "#(@(1; \"Iliad\");\n  @(2; \"Emma\"))"
    );
    assert_eq!(
        one("= %(::name; ::pages) | .cols | /rows/*:$.cols @| #"),
        "#(@(name; pages);\n  %(\"Iliad\"; 683);\n  %(\"Emma\"; 474))"
    );
    // a null read later in a pipeline stays null (the survival is
    // the head's alone)
    assert_eq!(run("= null | :zzz").unwrap(), ["null"]);
    // a node still refuses an unknown core field; a scalar misses
    let e = run("/rows/*[1] | :zzz | count").unwrap_err().to_string();
    assert!(e.contains("no core field"), "{e}");
    assert_eq!(run("= 3 | :zzz").unwrap(), ["null"]);
}

#[test]
fn the_spread_item_keys_by_the_bare_keys() {
    assert_eq!(
        one("/rows/*[1] | %(:props...)"),
        "%(name = \"Iliad\"; pages = 683)"
    );
    assert_eq!(one("/rows/*[1] | %(:props...)"), one("/rows/*[1] | %"));
    assert_eq!(
        one("/rows/*[1]:%(:props...; n = 1)"),
        "%(name = \"Iliad\"; pages = 683; n = 1)"
    );
    assert_eq!(
        one("= %(a = 1; b = 2) | $_:%((fields)...)"),
        "%(a = 1; b = 2)"
    );
    let e = run("/rows/*[1] | %(::name; :props...)")
        .unwrap_err()
        .to_string();
    assert!(e.contains("share the bare key 'name'"), "{e}");
    // `project` still works through 0.41.x
    // `project` is retired at 0.42.0; `| %` is the node's record
    let e = run("/rows/*[1] | project(:props; -r)")
        .unwrap_err()
        .to_string();
    assert!(e.contains("'project(…)' is retired"), "{e}");
}

#[test]
fn the_operator_unparses_to_a_fixpoint() {
    assert_eq!(fixpoint("= 5 | $_:$.p"), "= 5 | $_:$.p");
    assert_eq!(fixpoint("= 5 | $_:($.p)"), "= 5 | $_:$.p");
    assert_eq!(fixpoint("= 5 | $_:#(:A; s = :B)"), "= 5 | $_:#(:A; s = :B)");
    assert_eq!(fixpoint("= 5 | $_:@(:A; 1)"), "= 5 | $_:@(:A; 1)");
    fixpoint("= 5 | $_:%(n = :A)");
    fixpoint("//x:%(:props...)");
    fixpoint("= 5 | %(:props...; n = 1)");
    fixpoint("//x:$.cols @| #");
}

#[test]
fn equality_inside_map_and_after_a_pipe() {
    assert_eq!(
        one(&format!("{T} | map($_ = null)")),
        "#(@(A; B);\n  %(false; true);\n  %(false; false))"
    );
    assert_eq!(
        one(&format!("{T} | map(= null)")),
        one(&format!("{T} | map($_ = null)"))
    );
    // the comparison does not broadcast
    assert_eq!(one(&format!("{T} | $_ = null")), "false");
    assert_eq!(one("= 5 | $_ = 5 && $_ > 1"), "true");
    assert_eq!(one("= 5 | $_ > 3 ? $_ : null"), "5");
}
