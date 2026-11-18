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
    // (a JSON array names its items by position, from one: the
    // name and the index agree)
    assert_eq!(
        one("/rows/*:%(:name; :index) @| #"),
        "#(@(name; index);\n  %(1; 1);\n  %(2; 2))"
    );
}

#[test]
fn a_register_or_an_expression_supplies_the_structure() {
    // a structure built where its projections had nothing to read
    // keeps them as projectors, read at the operator
    assert_eq!(one("= %(:A; :B)"), "%(A = :A; B = :B)");
    assert_eq!(one("= @(:A; :B)"), "@(:A; :B)");
    // a bare read at the head has nothing to project from (ruling 2026-11-06)
    assert!(
        run("= :name")
            .unwrap_err()
            .to_string()
            .contains("nothing to project from")
    );
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
    assert_eq!(
        fixpoint("= 5 | .p(@(1; 2)) | $_:$.p"),
        "= 5 | .p(@(1; 2)) | $_:$.p"
    );
    assert_eq!(
        fixpoint("= 5 | .p(@(1; 2)) | $_:($.p)"),
        "= 5 | .p(@(1; 2)) | $_:$.p"
    );
    assert_eq!(fixpoint("= 5 | $_:#(:A; s = :B)"), "= 5 | $_:#(:A; s = :B)");
    assert_eq!(fixpoint("= 5 | $_:@(:A; 1)"), "= 5 | $_:@(:A; 1)");
    fixpoint("= 5 | $_:%(n = :A)");
    fixpoint("//x:%(:props...)");
    fixpoint("= 5 | %(:props...; n = 1)");
    fixpoint("= 5 | .cols(@(1)) | //x:$.cols @| #");
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

/// A table shape nests in a record or a list, and at the head (spec:
/// Projection by the Glued Colon, the structure at any depth): one row
/// of the projections under their names, read where it stands.
#[test]
fn a_table_shape_nests_in_a_record_or_a_list() {
    assert_eq!(
        one("/rows/*[1]:%(t = #(::name; ::pages); n = ::name) | :t | type"),
        "@(\"text\"; \"int\")"
    );
    assert_eq!(
        one("/rows/*[1]:%(t = #(::name; ::pages)) | :t | :pages"),
        "@(683)"
    );
    assert_eq!(one("/rows/*[1]:@(#(::name)) | [1] | type"), "text");
    assert_eq!(
        one("= %(t = #(::name; p = ::pages)) | .p | /rows/*[1]:$.p | :t | :p"),
        "@(683)"
    );
}

/// `| %` answers the record of a node however the node arrives
/// (ruling 2026-11-18): the thread's own node, a mark, a witness by
/// number, a field that holds a node. The last two arrived as
/// values and were refused ("the rows have no field names").
#[test]
fn the_record_of_a_node_however_it_arrives() {
    let iliad = "%(name = \"Iliad\"; pages = 683)";
    assert_eq!(one("/rows/*[1] | %"), iliad);
    assert_eq!(one("/rows/*[1] .R | $$.R | %"), iliad);
    // a witness by number
    assert_eq!(
        one("/rows/*[2] <=> /rows/*[::pages = 683] | $$1 | %"),
        iliad
    );
    // a field that holds a node
    assert_eq!(one("/rows/*[1] .R | %(t = $$.R) | :t | %"), iliad);
    // the node itself is still what a keyed field holds
    assert!(one("/rows/*[1] .R | %(t = $$.R) | :t").starts_with('/'));
    // a value that is no node reads as it did
    assert_eq!(one("= %(a = 1) | %"), "%(a = 1)");
    let e = run("= @(1; 2) | %").unwrap_err().to_string();
    assert!(e.contains("no field names"), "{e}");
}

/// A record constructor whose items are self-labelled projections
/// heads an expression after a pipe, as one with named items does:
/// `| %(::name) ++ %(n = 2)`. It ended the command at its closing
/// parenthesis, and the rest was refused as trailing input.
#[test]
fn a_projector_after_a_pipe_heads_an_expression() {
    assert_eq!(
        one("/rows/*[1] | %(::name) ++ %(n = 2)"),
        "%(name = \"Iliad\"; n = 2)"
    );
    assert_eq!(
        one("/rows/*[1] | %(::name; p = ::pages) ++ %(n = 2) ++ %(m = 3)"),
        "%(name = \"Iliad\"; p = 683; n = 2; m = 3)"
    );
    // the same value as the spellings that parsed
    assert_eq!(
        one("/rows/*[1] | %(::name) ++ %(n = 2)"),
        one("/rows/*[1] | %(name = ::name) ++ %(n = 2)")
    );
    assert_eq!(
        one("/rows/*[1] | %(::name) ++ %(n = 2)"),
        one("/rows/*[1] | (%(::name)) ++ %(n = 2)")
    );
    assert_eq!(
        one("/rows/*[1] | %(::pages) ++! %(pages = 1)"),
        "%(pages = 1)"
    );
    // alone it is what it was, and the text it unparses to parses
    assert_eq!(one("/rows/*[1] | %(::name)"), "%(name = \"Iliad\")");
    fixpoint("/rows/*[1] | %(::name) ++ %(n = 2)");
}

/// The record constructor takes a node as an item (ruling
/// 2026-11-18): as it takes a projection `::proj` and keys it
/// `proj`, it takes a node and reads all of its property
/// projections, each naming itself. With a key the item holds the
/// node, as before.
#[test]
fn a_node_as_a_record_item_is_its_property_projections() {
    let iliad = "%(name = \"Iliad\"; pages = 683)";
    // a mark, an absolute path, a witness by number
    assert_eq!(one("/rows/*[1] .R | %($$.R)"), iliad);
    assert_eq!(one("= %(^/rows/*[1])"), iliad);
    assert_eq!(one("/rows/*[2] | %(^/rows/*[1])"), iliad);
    assert_eq!(one("/rows/*[2] <=> /rows/*[::pages = 683] | %($$1)"), iliad);
    // the alternate form of `$$.R | %`
    assert_eq!(
        one("/rows/*[1] .R | %($$.R)"),
        one("/rows/*[1] .R | $$.R | %")
    );
    // among other items, where it stands
    assert_eq!(
        one("/rows/*[2] | %(n = 1; ^/rows/*[1]; m = 2)"),
        "%(n = 1; name = \"Iliad\"; pages = 683; m = 2)"
    );
    // its use is beside another record, not in pipe position
    assert_eq!(
        one("/rows/*[1] .R | %($$.R) ++? %(name = \"x\"; n = 1)"),
        "%(name = \"Iliad\"; pages = 683; n = 1)"
    );
    // several nodes: one record of lists, aligned by node
    assert_eq!(
        one("= %(^/rows/*)"),
        "%(name = @(\"Iliad\"; \"Emma\"); pages = @(683; 474))"
    );
    // no node: no field
    assert_eq!(one("= %(^/none/*)"), "%()");
    // with a key the item is what it was: the node, or a path's test
    assert!(one("/rows/*[1] .R | %(t = $$.R) | :t").starts_with('/'));
    assert_eq!(one("= %(t = ^/rows/*[1])"), "%(t = true)");

    // a name met twice is refused when the record is built
    for q in [
        "/rows/*[1] | %(name = 1; ^/rows/*[2])",
        "/rows/*[1] | %(^/rows/*[2]; name = 1)",
        "/rows/*[1] | %(::name; ^/rows/*[2])",
        "/rows/*[1] .R | %($$.R; ^/rows/*[2])",
    ] {
        let e = run(q).unwrap_err().to_string();
        assert!(
            e.contains("'name' is already a field of the record"),
            "{q}: {e}"
        );
    }
    // an item with no key that answers no node is still refused: by
    // its form where it can answer none, by its value otherwise
    let e = run("= %(1 + 2)").unwrap_err().to_string();
    assert!(e.contains("a record field needs a name"), "{e}");
    let e = run("/rows/*[1] | ::name | %($_)").unwrap_err().to_string();
    assert!(e.contains("which is no node"), "{e}");
    // and it unparses as written
    fixpoint("/rows/*[1] .R | %($$.R; n = 1)");
    fixpoint("= %(^/rows/*[1])");
}

/// `| %` on a list of nodes is the column view of the nodes'
/// records (ruling 2026-11-18): one record of lists, an entry per
/// node, the value `%(/x)` gives over the same nodes.
#[test]
fn the_record_of_a_list_of_nodes_is_their_column_view() {
    let columns = "%(name = @(\"Iliad\"; \"Emma\"); pages = @(683; 474))";
    assert_eq!(one("/rows/* @| (@@_) | %"), columns);
    assert_eq!(one("/rows/* @| (@@_) | %"), one("= %(^/rows/*)"));
    assert_eq!(one("/rows/* @| (@@_) | %"), one("/rows/* | % @| %"));
    // a node among records is read as its record
    assert_eq!(
        one("/rows/*[1] .R | @($$.R; %(name = \"x\"; n = 1)) | %"),
        "%(name = @(\"Iliad\"; \"x\"); pages = @(683; null); n = @(null; 1))"
    );
    // a member that is neither is refused as before
    let e = run("/rows/*[1] .R | @($$.R; 5) | %")
        .unwrap_err()
        .to_string();
    assert!(e.contains("no field names"), "{e}");
}
