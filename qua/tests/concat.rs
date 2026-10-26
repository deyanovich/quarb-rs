//! Concatenation (spec: Concatenation): `++`, `++!` and `+@+`, the
//! binary operators at the additive level that took over from the
//! append pipe (draft 163); the left side gives the shape.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"rows":[
            {"book":"a","x":1,"y":2},
            {"book":"b","x":3,"y":4},
            {"book":"c","x":5,"y":6}],
          "names":[{"first":"Jane","last":"Austen"}],
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
fn a_list_concatenates_a_list_and_takes_anything_else_as_one_element() {
    assert_eq!(one("= @(1; 2) ++ 3"), "@(1; 2; 3)");
    assert_eq!(one("= @(1; 2) ++ @(3; 4)"), "@(1; 2; 3; 4)");
    assert_eq!(one("= @(1; 2) ++ @(@(3; 4))"), "@(1; 2; @(3; 4))");
    assert_eq!(one("= @(1; 2) ++ \"a\""), "@(1; 2; \"a\")");
    assert_eq!(one("= @(1; 2) ++ %(k = 1)"), "@(1; 2; %(k = 1))");
    // a path on the right contributes its first value, as every
    // binary operand does; gathered, every value
    // a forking right operand forks the expression (draft 164)
    assert_eq!(
        run("= @(0) ++ (/rows/*::x)").unwrap(),
        vec!["@(0; 1)", "@(0; 3)", "@(0; 5)"]
    );
    assert_eq!(one("= @(0) ++ @(/rows/*::x)"), "@(0; 1; 3; 5)");
    // the operator sits at the additive level, left-associative
    assert_eq!(one("= @(1) ++ 2 * 3"), "@(1; 6)");
    assert_eq!(one("= @(1) ++ @(2) ++ 3"), "@(1; 2; 3)");
    assert_eq!(one("= @(1) ++ @(2) + 1"), "@(2; 3)");
    // a record array concatenates like any list
    assert_eq!(
        one("= @%(@(a); %(1)) ++ @%(@(a); %(2))"),
        "@%(@(a); %(1); %(2))"
    );
}

#[test]
fn the_element_append_takes_the_right_side_as_one_item() {
    assert_eq!(one("= @(1; 2) +@+ @(3; 4)"), "@(1; 2; @(3; 4))");
    assert_eq!(one("= @(1; 2) +@+ 3"), "@(1; 2; 3)");
    assert_eq!(one("= @(1; 2) +@+ %(k = 1)"), "@(1; 2; %(k = 1))");
    assert!(refuse("= \"a\" +@+ 1").contains("'+@+' appends one item to a list"));
    assert!(refuse("= %(a = 1) +@+ 1").contains("'+@+' appends one item to a list"));
}

#[test]
fn a_text_takes_the_right_sides_text() {
    assert_eq!(one("= \"n = \" ++ 5"), "n = 5");
    assert_eq!(one("= \"a\" ++ @(1; 2)"), "a@(1; 2)");
    assert_eq!(one("= \"a\" ++ %(k = 1)"), "a%(k = 1)");
    assert_eq!(one("= \"a\" ++ \"b\" ++ \"c\""), "abc");
    assert_eq!(one("/names/* | ::first ++ \" \" ++ ::last"), "Jane Austen");
    // non-commutative: the left side drives
    assert_eq!(one("= @(1; 2) ++ \"a\""), "@(1; 2; \"a\")");
}

#[test]
fn null_on_either_side_is_null_and_other_kinds_are_refused() {
    assert_eq!(run("= null ++ 1").unwrap(), vec!["null"]);
    assert_eq!(run("= @(1) ++ null").unwrap(), vec!["null"]);
    assert_eq!(run("= null ++! %(a = 1)").unwrap(), vec!["null"]);
    assert!(refuse("= 1 ++ 2").contains("takes no concatenation"));
    assert!(refuse("= true ++ @(1)").contains("takes no concatenation"));
}

#[test]
fn a_table_takes_rows_and_retypes() {
    assert_eq!(
        one("= #(@(a; b); %(1; 2)) ++ %(a = 3; b = 4) | dim"),
        "@(2; 2)"
    );
    assert_eq!(one("= #(@(a; b); %(1; 2)) ++ %(b = 5) | :a"), "@(1; null)");
    // a name the header lacks extends it, null in the rows before
    assert_eq!(
        one("= #(@(a; b); %(1; 2)) ++ %(a = 1; c = 2; b = 3; d = 4) | keys"),
        r#"@("a"; "b"; "c"; "d")"#
    );
    assert_eq!(
        one("= #(@(a; b); %(1; 2)) ++ %(a = 1; c = 2; b = 3; d = 4) | @"),
        "@%(@(a; b; c; d); %(1; 2; null; null); %(1; 3; 2; 4))"
    );
    assert_eq!(
        one("= #(@(a; b); %(1; 2)) ++ #(@(b; c); %(3; 4)) | @"),
        "@%(@(a; b; c); %(1; 2; null); %(null; 3; 4))"
    );
    // a table row by row, columns by name on a frame
    assert_eq!(
        one("= #(@(a; b); %(1; 2)) ++ #(@(b; a); %(9; 8)) | :a"),
        "@(1; 8)"
    );
    // by position on an array
    assert_eq!(one("= #(%(1; 2)) ++ #(%(3; 4)) | dim"), "@(2; 2)");
    assert!(refuse("= #(%(1; 2)) ++ #(%(3; 4; 5))").contains("3 values"));
    // the columns re-type after
    assert_eq!(one("= #(@(a); %(1)) ++ %(a = \"x\") | type"), "text");
    assert_eq!(one("= #(@(a); %(\"1\")) ++ %(a = 2) | type"), "int");
    // a cell holds a scalar or null
    assert!(refuse("= #(@(a); %(1)) ++ %(a = @(1; 2))").contains("flat"));
    // not a list
    assert!(refuse("= #(@(a); %(1)) ++ @(2)").contains("a record as a row"));
}

#[test]
fn a_row_on_the_right_follows_the_literal_rules() {
    // positional by the header, named by name, mixed
    assert_eq!(
        one(r#"= #(@(title; year); %("Hamlet"; 1603)) ++ %("Emma"; 1815) | :year"#),
        "@(1603; 1815)"
    );
    assert_eq!(
        one(r#"= #(@(title; year); %("Hamlet"; 1603)) ++ %("Emma"; year = 1815) | :title"#),
        r#"@("Hamlet"; "Emma")"#
    );
    // a short row is null-padded, a long row refused
    assert_eq!(one(r#"= #(@(a; b); %(1; 2)) ++ %(3) | :b"#), "@(2; null)");
    assert_eq!(
        one(r#"= #(@(a; b); %(1; 2)) ++ %(3; c = 9) | keys"#),
        r#"@("a"; "b"; "c")"#
    );
    assert!(refuse(r#"= #(@(a; b); %(1; 2)) ++ %(3; 4; 5)"#).contains("more values"));
    // the array face takes positions, not names
    assert_eq!(one(r#"= #(%(1; 2)) ++ %(3; 4) | dim"#), "@(2; 2)");
    assert!(refuse(r#"= #(%(1; 2)) ++ %(a = 3)"#).contains("positions"));
    // the items are evaluated per capsa
    assert_eq!(
        one(r#"/rows/*[1] | #(@(book; x); %("z"; 0)) ++ %(::book; ::x) | :x"#),
        "@(0; 1)"
    );
    // on a record or a list a named row is a record; positional is refused by kind
    assert_eq!(one(r#"= %(a = 1) ++ %(b = 2)"#), "%(a = 1; b = 2)");
    assert_eq!(one(r#"= @(1) ++ %(b = 2)"#), "@(1; %(b = 2))");
    assert!(refuse(r#"= %(a = 1) ++ %(2)"#).contains("positional row appends to a table"));
    assert_eq!(one(r#"= %(a = 1) ++! %(a = 2)"#), "%(a = 2)");
    // a row under `+@+` is the record, one item
    assert_eq!(one(r#"= @(1) +@+ %(b = 2)"#), "@(1; %(b = 2))");
}

#[test]
fn a_record_appends_fields_and_overwrites_only_with_the_bang() {
    assert_eq!(one("= %(a = 1) ++ %(b = 2)"), "%(a = 1; b = 2)");
    assert!(refuse("= %(a = 1) ++ %(a = 2)").contains("already a field"));
    assert_eq!(one("= %(a = 1; b = 2) ++! %(a = 3)"), "%(a = 3; b = 2)");
    assert_eq!(one("= %(a = 1) ++! %(a = 2)"), "%(a = 2)");
    assert!(refuse("= %(a = 1) ++ 2").contains("has no name"));
    assert!(refuse("= @(1) ++! 2").contains("'++!' overwrites a record's fields"));
    assert!(refuse("= \"a\" ++! %(a = 1)").contains("'++!' overwrites a record's fields"));
}

#[test]
fn after_a_pipe_the_topic_is_the_left_side() {
    assert_eq!(
        run("/rows/* | @(::x) | ++ ::y").unwrap(),
        vec!["@(1; 2)", "@(3; 4)", "@(5; 6)"]
    );
    assert_eq!(
        run("/rows/* | @(::x) ++ ::y").unwrap(),
        vec!["@(1; 2)", "@(3; 4)", "@(5; 6)"]
    );
    assert_eq!(one("= %(a = 1) | ++! %(a = 2)"), "%(a = 2)");
    assert_eq!(one("= @(1) | +@+ @(2; 3)"), "@(1; @(2; 3))");
    assert_eq!(one("= @(1) | ++ 2 | ++ @(3; 4) | count"), "4");
    // the topic of a body too
    assert_eq!(one("= @(1; 2) | ++ ($_ | count)"), "@(1; 2; 2)");
}

#[test]
fn the_append_pipe_is_retired_with_a_pointer() {
    for q in [
        "= @(1) +| 2",
        "= %(a = 1) +|! %(a = 2)",
        "= @(1) +__ 2",
        "= @(1) +__! 2",
    ] {
        let e = refuse(q);
        assert!(e.contains("'+|' is retired: write '| ++ x'"), "{q}: {e}");
    }
}

#[test]
fn concatenation_round_trips_through_unparse() {
    for q in [
        "= @(1; 2) ++ 3",
        "= %(a = 1) ++! %(a = 2)",
        "= @(1; 2) +@+ @(3; 4)",
        "= #(@(a; b); %(1; 2)) ++ %(3; c = 9)",
        "= @(1) ++ (2 + 3)",
        "= @(1) ++ 2 + 3",
        "= (1 + 2) ++ 3",
        "= \"a\" ++ \"b\" ++ \"c\"",
        "/rows/* | @(::x) | ++ ::y",
    ] {
        let printed = quarb::Query::parse(q, &doc()).unwrap().canonical();
        let again = quarb::Query::parse(&printed, &doc()).unwrap().canonical();
        assert_eq!(printed, again, "{q}");
    }
}
