//! The append pipe `+|` (spec: The Append Pipe): the body's values
//! appended to the topic by the shape rule.

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
fn a_list_appends_elements_and_concatenates_lists() {
    assert_eq!(one("= @(1; 2) +| 3"), "@(1; 2; 3)");
    assert_eq!(one("= @(1; 2) +| @(3; 4)"), "@(1; 2; 3; 4)");
    assert_eq!(one("= @(1; 2) +| @(@(3; 4))"), "@(1; 2; @(3; 4))");
    // a path appends as many items as it reaches
    assert_eq!(one("= @(0) +| (/rows/*::x)"), "@(0; 1; 3; 5)");
    // the body sees the topic
    assert_eq!(one("= @(1; 2) +| ($_ @| sum)"), "@(1; 2; 3)");
    // a record array concatenates like any list
    assert_eq!(
        one("= @%(@(a); %(1)) +| @%(@(a); %(2))"),
        "@%(@(a); %(1); %(2))"
    );
    // the rounded spelling
    assert_eq!(one("= @(1) +__ 2"), "@(1; 2)");
    // `+|!` is for records
    assert!(refuse("= @(1) +|! 2").contains("overwrites a record's fields"));
}

#[test]
fn a_table_appends_rows_and_retypes() {
    assert_eq!(
        one("= #(@(a; b); %(1; 2)) +| %(a = 3; b = 4) | dim"),
        "@(2; 2)"
    );
    assert_eq!(one("= #(@(a; b); %(1; 2)) +| %(b = 5) | :a"), "@(1; null)");
    // a name the header lacks extends it, null in the rows before
    assert_eq!(
        one("= #(@(a; b); %(1; 2)) +| %(a = 1; c = 2; b = 3; d = 4) | header"),
        r#"@("a"; "b"; "c"; "d")"#
    );
    assert_eq!(
        one("= #(@(a; b); %(1; 2)) +| %(a = 1; c = 2; b = 3; d = 4) | @"),
        "@%(@(a; b; c; d); %(1; 2; null; null); %(1; 3; 2; 4))"
    );
    assert_eq!(
        one("= #(@(a; b); %(1; 2)) +| #(@(b; c); %(3; 4)) | @"),
        "@%(@(a; b; c); %(1; 2; null); %(null; 3; 4))"
    );
    // a table row by row, columns by name on a frame
    assert_eq!(
        one("= #(@(a; b); %(1; 2)) +| #(@(b; a); %(9; 8)) | :a"),
        "@(1; 8)"
    );
    // by position on an array
    assert_eq!(one("= #(%(1; 2)) +| #(%(3; 4)) | dim"), "@(2; 2)");
    assert!(refuse("= #(%(1; 2)) +| #(%(3; 4; 5))").contains("3 values"));
    // the columns re-type after the append
    assert_eq!(one("= #(@(a); %(1)) +| %(a = \"x\") | type"), "text");
    assert_eq!(one("= #(@(a); %(\"1\")) +| %(a = 2) | type"), "int");
    // a cell holds a scalar or null
    assert!(refuse("= #(@(a); %(1)) +| %(a = @(1; 2))").contains("flat"));
    // not a list
    assert!(refuse("= #(@(a); %(1)) +| @(2)").contains("a record as a row"));
}

#[test]
fn a_row_on_the_right_follows_the_literal_rules() {
    // positional by the header, named by name, mixed
    assert_eq!(
        one(r#"= #(@(title; year); %("Hamlet"; 1603)) +| %("Emma"; 1815) | :year"#),
        "@(1603; 1815)"
    );
    assert_eq!(
        one(r#"= #(@(title; year); %("Hamlet"; 1603)) +| %("Emma"; year = 1815) | :title"#),
        r#"@("Hamlet"; "Emma")"#
    );
    // a short row is null-padded, a long row refused
    assert_eq!(one(r#"= #(@(a; b); %(1; 2)) +| %(3) | :b"#), "@(2; null)");
    assert_eq!(
        one(r#"= #(@(a; b); %(1; 2)) +| %(3; c = 9) | header"#),
        r#"@("a"; "b"; "c")"#
    );
    assert!(refuse(r#"= #(@(a; b); %(1; 2)) +| %(3; 4; 5)"#).contains("more values"));
    // the array face takes positions, not names
    assert_eq!(one(r#"= #(%(1; 2)) +| %(3; 4) | dim"#), "@(2; 2)");
    assert!(refuse(r#"= #(%(1; 2)) +| %(a = 3)"#).contains("positions"));
    // the items are evaluated per capsa
    assert_eq!(
        one(r#"/rows/*[1] | #(@(book; x); %("z"; 0)) +| %(::book; ::x) | :x"#),
        "@(0; 1)"
    );
    // on a record or a list a named row is a record; positional is refused by kind
    assert_eq!(one(r#"= %(a = 1) +| %(b = 2)"#), "%(a = 1; b = 2)");
    assert_eq!(one(r#"= @(1) +| %(b = 2)"#), "@(1; %(b = 2))");
    assert!(refuse(r#"= %(a = 1) +| %(2)"#).contains("positional row appends to a table"));
    assert_eq!(one(r#"= %(a = 1) +|! %(a = 2)"#), "%(a = 2)");
}

#[test]
fn a_record_appends_fields_and_never_overwrites() {
    assert_eq!(one("= %(a = 1) +| %(b = 2)"), "%(a = 1; b = 2)");
    assert!(refuse("= %(a = 1) +| %(a = 3)").contains("already a field"));
    assert_eq!(one("= %(a = 1; b = 2) +|! %(a = 3)"), "%(a = 3; b = 2)");
    assert!(refuse("= %(a = 1) +| 2").contains("has no name"));
}

#[test]
fn any_other_topic_is_refused_by_kind() {
    assert!(refuse("= null +| 1").contains("takes no append"));
    assert!(refuse("= 1 +| 2").contains("takes no append"));
    // per capsa: each topic takes its own append
    assert_eq!(
        run("/rows/* | @(::x) +| ::y").unwrap(),
        vec!["@(1; 2)", "@(3; 4)", "@(5; 6)"]
    );
}
