//! The calculator's range, `flatten`, and the reshaping constructor
//! (spec draft.150).

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"rows":[
            {"book":"a","x":1,"y":2},
            {"book":"b","x":3,"y":4},
            {"book":"c","x":5,"y":6}],
          "long":[
            {"book":"a","measure":"mean","v":1},
            {"book":"a","measure":"semi","v":2},
            {"book":"b","measure":"mean","v":3},
            {"book":"b","measure":"semi","v":4}],
          "m":[[1,2,3],[4,5,6]]}"#,
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

fn refuse(q: &str) -> String {
    run(q).unwrap_err().to_string()
}

/// The substitution's modifiers come from the set g, i, m, s, x:
/// `g` reaches every match, the other four are the pattern
/// literal's flags, and a letter outside the set is refused at
/// lexing with a hint naming the set.
#[test]
fn substitution_modifiers_are_gimsx() {
    assert_eq!(one("= 'foo' | s/o/0/"), "f0o");
    assert_eq!(one("= 'foo' | s/o/0/g"), "f00");
    assert_eq!(one("= 'Foo' | s/f/b/i"), "boo");
    // x: whitespace in the pattern is not part of it.
    assert_eq!(one("= 'a b' | s/a b/x/x"), "a b");
    assert_eq!(one("= 'ab' | s/a b/x/x"), "x");
    // s: the dot crosses a newline.
    assert_eq!(one("= 'a\nb' | s/a.b/x/s"), "x");
    assert_eq!(one("= 'a\nb' | s/a.b/x/"), "a\nb");
    // m: anchors match at line boundaries.
    assert_eq!(one("= 'a\nb' | s/^b/x/m"), "a\nx");
    assert_eq!(one("= 'a\nb' | s/^b/x/"), "a\nb");
    // The modifiers compose, in any order.
    assert_eq!(one("= 'A\nb A' | s/^ a/x/gxim"), "x\nb A");
    let err = refuse("= 'foo' | s/o/0/gz");
    assert!(err.contains("'z' is not a substitution modifier"), "{err}");
    assert!(err.contains("g, i, m, s and x"), "{err}");
}

#[test]
fn a_range_yields_its_integers_one_at_a_time() {
    assert_eq!(run("= 1..5").unwrap(), vec!["1", "2", "3", "4", "5"]);
    assert_eq!(one("= @(1..5)"), "@(1; 2; 3; 4; 5)");
    assert_eq!(one("= @(0; 1..3; 9)"), "@(0; 1; 2; 3; 9)");
    assert_eq!(one("= @(5..1)"), "@(5; 4; 3; 2; 1)");
    assert_eq!(one("= @(0..8..2)"), "@(0; 2; 4; 6; 8)");
    assert_eq!(one("= @(0..7..2)"), "@(0; 2; 4; 6)");
    // the step's sign is the direction; a step that points away
    // from the second bound yields nothing (Python's range)
    assert_eq!(one("= @(10..0..-5)"), "@(10; 5; 0)");
    assert_eq!(one("= @(7..0..-2)"), "@(7; 5; 3; 1)");
    assert_eq!(one("= @(0..8..-2)"), "@()");
    assert_eq!(one("= @(0..7..-2)"), "@()");
    assert_eq!(one("= @(8..0..2)"), "@()");
    assert_eq!(one("= @(10..0..5)"), "@()");
    assert_eq!(one("= @(3..3..-1)"), "@(3)");
    // the invariant: a non-empty range always begins at a
    for (q, first) in [
        ("0..8..2", "0"),
        ("8..0..-2", "8"),
        ("10..0..-5", "10"),
        ("5..1", "5"),
        ("-2..2", "-2"),
        ("3..3", "3"),
        ("0..7..3", "0"),
        ("7..0..-3", "7"),
    ] {
        let listed = one(&format!("= @({q})"));
        assert!(
            listed.starts_with(&format!("@({first}")),
            "{q} lists {listed}, not from {first}"
        );
    }
    for q in ["0..7..-2", "7..0..2", "0..8..-1", "1..5..-3"] {
        assert_eq!(one(&format!("= @({q})")), "@()", "{q}");
    }
    assert_eq!(one("= @(3..3)"), "@(3)");
    assert_eq!(one("= @(-2..2)"), "@(-2; -1; 0; 1; 2)");
    // bounds as parenthesized expressions, no precedence of their own
    assert_eq!(one("= @(1..(2 + 1))"), "@(1; 2; 3)");
    assert_eq!(one("= @((1 + 1)..4)"), "@(2; 3; 4)");
    // (a parenthesized integer alone is the rounded register `(N)`:
    // a computed bound is an expression)
    assert_eq!(one("= @((0 + 1)..(2 + 1)..(1 + 0))"), "@(1; 2; 3)");
    assert_eq!(one("= @(1..(4 + 1)..2)"), "@(1; 3; 5)");
    assert_eq!(one("= 1..5 @| sum"), "15");
    // inside index brackets a range is still the slice
    assert_eq!(one("= @(10; 20; 30; 40) | [2..3]"), "@(20; 30)");
    // the refusals: a zero step, a bound that is no integer; the string is written quoted
    assert!(refuse("= @(1..5..0)").contains("not zero"));
    assert!(refuse(r#"= @(1..("a"))"#).contains("integer"));
    assert_eq!(one(r#"= "1..5""#), "1..5");
}

#[test]
fn keys_and_values_split_a_record_and_a_frame() {
    assert_eq!(one("= %(a = 1; b = 2) | keys"), r#"@("a"; "b")"#);
    assert_eq!(one("= %(a = 1; b = 2) | values"), "@(1; 2)");
    assert_eq!(one("= #(@(a; b); %(1; 2)) | keys"), r#"@("a"; "b")"#);
    assert_eq!(one("= #(@(a; b); %(1; 2)) | values"), "#(%(1; 2))");
    assert_eq!(
        one(r#"= #(@(a; b); %("1"; "x")) | values | type"#),
        r#"@("int"; "text")"#
    );
    // on a list of records: the header is the collection's union,
    // keys and values answer per record
    let recs = "@(%(a = 1; b = 2); %(b = 3; a = 4; c = 5))";
    assert_eq!(one(&format!("= {recs} | header")), r#"@("a"; "b"; "c")"#);
    assert_eq!(
        one(&format!("= {recs} | keys")),
        r#"@(@("a"; "b"); @("b"; "a"; "c"))"#
    );
    assert_eq!(one(&format!("= {recs} | values")), "@(@(1; 2); @(3; 4; 5))");
    assert_eq!(
        one("= @(%(a = @(1; 2)); %(b = 3)) | header"),
        r#"@("a"; "b")"#
    );
    assert!(refuse("= @(%(a = 1); 2) | keys").contains("not all records"));
    assert!(refuse("= @(1; 2) | keys").contains("not all records"));
    // (a stream of values crosses as the list it is)
    assert!(refuse("= 7 | values").contains("not all records"));
}

#[test]
fn the_pairwise_record_constructor() {
    assert_eq!(one("= %(@(a; b) = @(1; 2))"), "%(a = 1; b = 2)");
    assert_eq!(
        one(r#"= %(@("first name"; b) = @(1; 2)) | keys"#),
        r#"@("first name"; "b")"#
    );
    assert_eq!(
        one("= %(id = 7; @(a; b) = @(1; 2); z = 9)"),
        "%(id = 7; a = 1; b = 2; z = 9)"
    );
    // two saved lists, and the topic under the keys
    assert_eq!(
        one(r#"= 1 | .k(@("a"; "b")) | .v(@(1; 2)) | %($.k = $.v)"#),
        "%(a = 1; b = 2)"
    );
    // (the keys filed first: a push sets the topic to what it pushes)
    assert_eq!(
        one(r#"= @("a"; "b") | .k | @(1; 2) | %($.k = $_)"#),
        "%(a = 1; b = 2)"
    );
    // the round trip: the record filed first, since a push sets the
    // topic to what it pushes (`.k(keys)` would leave the keys)
    assert_eq!(
        one("= %(a = 1; b = 2) | .r | keys | .k | $.r | values | %($.k = $_)"),
        "%(a = 1; b = 2)"
    );
    // the refusals: differing lengths, a repeated name, no list
    assert!(refuse("= %(@(a; b) = @(1))").contains("differ in number, 2 and 1"));
    // a repeated name: a literal list is refused at parse time, a
    // computed one at run time
    assert!(refuse("= %(a = 1; @(a) = @(2))").contains("'a' twice"));
    assert!(refuse(r#"= @("a") | .k | %(a = 1; $.k = @(2))"#).contains("'a' is already a field"));
    assert!(refuse("= %(@(a) = 2)").contains("list of values"));
}

#[test]
fn transpose_yields_a_matrix_of_one_type() {
    // every cell coerced together first, then the exchange
    assert_eq!(
        one("= @(@(1; 2.5); @(3; 4)) | transpose"),
        "#(%(1; 3);\n  %(2.5; 4))"
    );
    assert_eq!(
        one("= @(@(1; 2.5); @(3; 4)) | transpose | type"),
        r#"@("float"; "float")"#
    );
    assert_eq!(
        one(r#"= @(@(1; "a"); @(3; 4)) | transpose | type"#),
        r#"@("text"; "text")"#
    );
    assert_eq!(
        one("= @(1..6) | #(-w=3) | transpose"),
        "#(%(1; 4);\n  %(2; 5);\n  %(3; 6))"
    );
    assert_eq!(
        one("= @(1..6) | #(-w=3) | transpose | transpose"),
        "#(%(1; 2; 3);\n  %(4; 5; 6))"
    );
    // a frame is refused, pointing at values; a ragged grid by position; a list of records by kind
    let e = refuse("= #(@(a; b); %(1; 2)) | transpose");
    assert!(e.contains("'| values'"), "{e}");
    assert_eq!(
        one("= #(@(a; b); %(1; 2)) | values | transpose"),
        "#(%(1);\n  %(2))"
    );
    assert!(refuse("= @(@(1); @(2; 3)) | transpose").contains("row 2 holds 2"));
    assert!(refuse("= @(%(a = 1); %(a = 2)) | transpose").contains("list of records"));
}

#[test]
fn flatten_opens_nested_lists_and_a_tables_cells() {
    assert_eq!(one("= @(1; @(2; @(3))) | flatten"), "@(1; 2; 3)");
    assert_eq!(one("= @(1; @(2; @(3))) | flatten(1)"), "@(1; 2; @(3))");
    assert_eq!(one("= @(1; null; @(@()); 2) | flatten"), "@(1; null; 2)");
    assert_eq!(one("= #(%(1; 2); %(3; 4)) | flatten"), "@(1; 2; 3; 4)");
    assert_eq!(one(r#"= #(@(a; b); %("x"; 1)) | flatten"#), r#"@("x"; 1)"#);
    assert_eq!(one("= 7 | flatten"), "7");
    assert!(refuse("= %(a = 1) | flatten").contains("fields, not items"));
    // over the stream: the topics' items, one row each
    assert_eq!(
        run("= @(@(1; 2); @(3)) | ... @| flatten").unwrap(),
        vec!["1", "2", "3"]
    );
}

#[test]
fn the_reshaping_constructor_lays_a_list_into_columns() {
    assert_eq!(one("= @(1..12) | #(--width=3) | dim"), "@(4; 3)");
    assert_eq!(one("= @(1..6) | #(-w=3)"), "#(%(1; 2; 3);\n  %(4; 5; 6))");
    assert_eq!(
        one("= @(1..6) | #(--header=@(a; b; c)) | header"),
        r#"@("a"; "b"; "c")"#
    );
    assert_eq!(one("= @(1..6) | #(-h=@(a; b; c)) | :b"), "@(2; 5)");
    // the columns type by the rule
    assert_eq!(
        one(r#"= @("1"; "x"; "2"; "y") | #(-w=2) | type"#),
        r#"@("int"; "text")"#
    );
    // round trips
    assert_eq!(
        one("= #(%(1; 2); %(3; 4)) | flatten | #(-w=2)"),
        "#(%(1; 2);\n  %(3; 4))"
    );
    assert_eq!(one("= @(1..12) | #(-w=3) x @(1; 1; 1)"), "@(6; 15; 24; 33)");
    // over the stream
    assert_eq!(one("= 1..6 @| #(-w=2) | dim"), "@(3; 2)");
    // the refusals: a count that does not divide, no list topic, a nested item
    assert!(refuse("= @(1..5) | #(-w=3)").contains("5 items do not divide into rows of 3"));
    // a stream of values is the list it lays out: one value, one item
    assert!(refuse("= 7 | #(-w=3)").contains("1 items do not divide into rows of 3"));
    assert_eq!(one("= 7 | #(-w=1) | dim"), "@(1; 1)");
    assert!(refuse("= %(a = 1) | #(-w=1)").contains("is not a list"));
    assert!(refuse("= @(1; @(2)) | #(-w=2)").contains("flat"));
    // a literal with rows after a pipe is still the value
    assert_eq!(one("= 1 | #(@(a); %(1)) | dim"), "@(1; 1)");
    assert_eq!(one("= #(@(a; b)) | dim"), "@(0; 2)");
}
