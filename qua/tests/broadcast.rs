//! Arithmetic over structures (spec: Arithmetic over structures;
//! The matrix): broadcasting, the operator in command position,
//! and the matrix product `x`.

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

#[test]
fn a_scalar_broadcasts_over_a_list_a_record_and_a_table() {
    assert_eq!(one("= @(1; 2; 3) * 2"), "@(2; 4; 6)");
    assert_eq!(one("= 2 * @(1; 2; 3)"), "@(2; 4; 6)");
    assert_eq!(one("= 10 - @(1; 2)"), "@(9; 8)");
    // cells whose type does not admit the operation stay as they are; null stays null
    assert_eq!(
        one(r#"= @(1; "a"; null; true) * 2"#),
        r#"@(2; "a"; null; true)"#
    );
    assert_eq!(one("= %(a = 1; b = 2) * 10"), "%(a = 10; b = 20)");
    assert_eq!(
        one(r#"= #(@(title; price); %("Emma"; 10); %("Hamlet"; 20)) * 2 | :price"#),
        "@(20; 40)"
    );
    assert_eq!(
        one(r#"= #(@(title; price); %("Emma"; 10); %("Hamlet"; 20)) * 2 | :title"#),
        r#"@("Emma"; "Hamlet")"#
    );
    // the columns re-type after the operation
    assert_eq!(one(r#"= #(@(a); %(1); %(2)) div 2 | type"#), "float");
    // a nested structure takes the scalar all the way down
    assert_eq!(one("= @(@(1; 2); @(3)) + 1"), "@(@(2; 3); @(4))");
}

#[test]
fn two_structures_of_one_shape_combine_cell_by_cell() {
    assert_eq!(one("= @(1; 2) + @(10; 20)"), "@(11; 22)");
    assert_eq!(
        one("= @(@(1; 2); @(3; 4)) + @(10; 20)"),
        "@(@(11; 22); @(13; 24))"
    );
    assert_eq!(
        one("= %(a = 1; b = 2) + %(b = 1; a = 1)"),
        "%(a = 2; b = 3)"
    );
    assert_eq!(
        one("= #(@(a); %(1); %(2)) + #(@(a); %(10); %(20)) | :a"),
        "@(11; 22)"
    );
    // a list against a table goes coordinate by column, against a record by field
    assert_eq!(
        one("= #(@(a; b); %(1; 2); %(3; 4)) + @(10; 20) | :b"),
        "@(22; 24)"
    );
    assert_eq!(
        one("= @(10; 20) + #(@(a; b); %(1; 2); %(3; 4)) | :a"),
        "@(11; 13)"
    );
    assert_eq!(one("= %(a = 1; b = 2) + @(10; 20)"), "%(a = 11; b = 22)");
    // a null coordinate leaves its column as it is
    assert_eq!(
        one(r#"= #(@(title; price); %("Emma"; 10)) * @(null; 3) | :price"#),
        "@(30)"
    );
}

#[test]
fn a_shape_mismatch_is_refused_naming_the_dimensions() {
    assert!(refuse("= @(1; 2) + @(1; 2; 3)").contains("2 and 3"));
    let e = refuse("= %(a = 1) + %(c = 1)");
    assert!(e.contains("fields a and c"), "{e}");
    let e = refuse("= #(@(a); %(1)) + #(@(a); %(1); %(2))");
    assert!(e.contains("1×1 and 2×1"), "{e}");
    let e = refuse("= #(@(a; b); %(1; 2)) + @(1; 2; 3)");
    assert!(
        e.contains("a list of 3 against a table of 2 columns"),
        "{e}"
    );
    let e = refuse(r#"= #(@(title; price); %("Emma"; 10)) * @(2; 2)"#);
    assert!(e.contains("column 'title' is text"), "{e}");
    let e = refuse("= %(a = 1) + @(1; 2)");
    assert!(
        e.contains("a list of 2 against a record of 1 fields"),
        "{e}"
    );
    let e = refuse("= #(@(a); %(1)) + %(a = 1)");
    assert!(e.contains("no rule"), "{e}");
}

#[test]
fn an_operator_in_command_position_takes_the_topic() {
    assert_eq!(one("= @(1; 2) | * 2"), "@(2; 4)");
    assert_eq!(one("= 5 | + 1 * 2"), "7");
    assert_eq!(one("= 7 | mod 3"), "1");
    assert_eq!(
        one(r#"= #(@(title; price); %("Emma"; 10)) | * 2 | :price"#),
        "@(20)"
    );
    assert_eq!(one("= #(%(1; 2); %(3; 4)) | x @(1; 1)"), "@(3; 7)");
}

#[test]
fn the_matrix_product() {
    assert_eq!(one("= #(%(1; 2); %(3; 4)) x @(1; 1)"), "@(3; 7)");
    assert_eq!(one("= @(1; 1) x #(%(1; 2); %(3; 4))"), "@(4; 6)");
    assert_eq!(
        one("= #(%(1; 2); %(3; 4)) x #(%(1; 0); %(0; 1))"),
        "#(%(1; 2);\n  %(3; 4))"
    );
    assert_eq!(one("= @(1; 2; 3) x @(4; 5; 6)"), "32");
    assert_eq!(one("= #(%(1.5; 2)) x @(2; 1)"), "@(5)");
    let e = refuse("= #(%(1; 2)) x @(1; 2; 3)");
    assert!(e.contains("inner dimensions differ (2 and 3)"), "{e}");
    let e = refuse("= @(1; 2) x #(%(1; 2))");
    assert!(e.contains("inner dimensions differ (2 and 1)"), "{e}");
    let e = refuse("= 2 x 3");
    assert!(e.contains("x is the matrix product"), "{e}");
    let e = refuse(r#"= #(@(a); %(1)) x @(1)"#);
    assert!(e.contains("x is the matrix product"), "{e}");
    // the reserved word: the string is written quoted, names behind a sigil are untouched
    let e = refuse("= x");
    assert!(e.contains("matrix product") && e.contains("quoted"), "{e}");
    assert_eq!(one(r#"= "x""#), "x");
    assert_eq!(one("= %(x = 1) | :x"), "1");
}
