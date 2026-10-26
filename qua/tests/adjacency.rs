//! The adjacency rule (spec: Pipelines, "Stdin"; draft 163): a
//! built-in after a pipe reads its input from the pipe and never
//! names it; a surplus operand is refused with the synopsis, a
//! surplus `$_` with the pointer to the pipe; inside an expression
//! the first argument is the input and the rest is held to the
//! same bound.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"rows":[{"k":"a","x":1},{"k":"b","x":3},{"k":"a","x":5}]}"#)
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
fn a_command_after_a_pipe_reads_its_input_from_the_pipe() {
    assert_eq!(one("= \"abc\" | upper"), "abc".to_uppercase());
    assert_eq!(one("= 3.14159 | round(2)"), "3.14");
    assert_eq!(one("= @(1; 2) | sum"), "3");
    assert_eq!(one("= 5 | + 1"), "6");
    // a surplus `$_` gets the pointer, as written
    let e = refuse("= 3.14159 | round($_; 2)");
    assert!(
        e.contains("round reads its input from the pipe: write | round(2)"),
        "{e}"
    );
    // a command that takes no operand, written with a parenthesis
    // after a pipe, is the call operand it is anywhere else (draft
    // 166): `upper($_)` is `($_ | upper)`, `upper("b")` is `("b" | upper)`
    assert_eq!(one("= \"a\" | upper($_)"), "A");
    assert_eq!(one("= \"a\" | upper(\"b\")"), "B");
    assert_eq!(one("= 1 | date(2013-01-01) + 1_d"), "2013-01-02");
    // a reducer keeps its refusal: the key goes through --of
    let e = refuse("= @(1; 2) | sum($_)");
    assert!(
        e.contains("sum reads its input from the pipe: write | sum"),
        "{e}"
    );
    let e = refuse("/rows/* | ::x @| count($_)");
    assert!(
        e.contains("count reads its input from the pipe: write @| count"),
        "{e}"
    );
    // any other surplus is refused with the synopsis
    let e = refuse("= 3.14159 | round(2; 3)");
    assert!(
        e.contains("'round' takes 1 operand at most, 2 given; the synopsis: round / round(d)"),
        "{e}"
    );
}

#[test]
fn inside_an_expression_the_first_argument_is_the_input() {
    assert_eq!(one("= upper(\"abc\")"), "ABC");
    assert_eq!(one("= round(3.14159; 2)"), "3.14");
    assert_eq!(one("= sum(@(1; 2))"), "3");
    assert_eq!(one("= @(1; 2) | (0 + sum($_))"), "3");
    let e = refuse("= sum(1; 2)");
    assert!(
        e.contains("'sum' inside an expression takes its input and no operand after it, 1 given"),
        "{e}"
    );
    let e = refuse("= count(@(1); 2)");
    assert!(
        e.contains("'count' inside an expression takes its input"),
        "{e}"
    );
}

#[test]
fn a_reduction_reads_its_field_through_of_and_a_keyed_aggregate_takes_the_topic_as_a_key() {
    // draft 164: the field to reduce is the `--of` option; a key
    // written as an operand is a surplus operand
    assert_eq!(
        run("/rows/* | %(k = ::k; x = ::x) @| group(k = $_:k) | sum(--of=:x)").unwrap(),
        vec!["6", "3"]
    );
    assert_eq!(
        run("/rows/* | %(k = ::k; x = ::x) @| group(k = $_:k) | variance(--sample -o=:x)").unwrap(),
        vec!["8", "null"]
    );
    let e = run("/rows/* | %(k = ::k; x = ::x) @| group(k = $_:k) | sum(:x)").unwrap_err();
    assert!(
        e.to_string().contains("'sum' takes no operand, 1 given"),
        "{e}"
    );
    // `$_` is a key of a keyed aggregate, so a surplus there is a
    // count, never the misplaced input
    assert_eq!(
        run("/rows/* | %(k = ::k; x = ::x) @| unique_by($_:k; $_:x) @| count").unwrap(),
        vec!["3"]
    );
    let e = refuse("/rows/* | ::x @| top(1; $_; ::x)");
    assert!(e.contains("'top' takes 2 operands at most, 3 given"), "{e}");
}
