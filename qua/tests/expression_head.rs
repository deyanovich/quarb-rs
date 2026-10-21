//! The expression head (spec: Value Expressions, "Literals and
//! constructors"; draft 164): a literal or a constructor right
//! after a pipe opens an expression where a command name would
//! stand, and after `@|` one expression over the context; the
//! constructors are expressions, their items an alternative to
//! stdin; `#(--width=n)` / `#(--header=list)` lay the list topic in
//! rows, `#(n)` and a header alone being retired.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"rows":[{"k":"a","x":1},{"k":"b","x":3}]}"#).unwrap()
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
    let out = run(q).unwrap_or_else(|e| panic!("{q}: {e}"));
    assert_eq!(out.len(), 1, "{q}: {out:?}");
    out.into_iter().next().unwrap()
}

fn refuse(q: &str) -> String {
    match run(q) {
        Err(e) => e.to_string(),
        Ok(out) => panic!("{q} passed: {out:?}"),
    }
}

#[test]
fn a_literal_after_a_pipe_opens_an_expression() {
    assert_eq!(one("= 3 | 0"), "0");
    assert_eq!(one("= 3 | 10 + $_"), "13");
    assert_eq!(one(r#"= 3 | "x""#), "x");
    assert_eq!(one("= 3 | null | type"), "null");
    assert_eq!(one("= 3 | true | type"), "bool");
    assert_eq!(one("= 3 | -1 * $_"), "-3");
    assert_eq!(one("= 3 | 2_km | type"), "quantity");
    assert_eq!(one("= 3 | 1.5 * 2"), "3");
    // per thread, the thread's own topic in reach
    assert_eq!(
        run("/rows/* | ::x | 10 + $_").unwrap(),
        vec!["11", "13"]
    );
    // a bare word is still a command
    let e = refuse("= 3 | nosuchcommand");
    assert!(e.contains("nosuchcommand"), "{e}");
    // a literal head continues only with an operator: the pointers
    for q in ["= 12 h", "= 3 | 12 km | type", "= 3 | 12 h"] {
        let e = refuse(q);
        assert!(e.contains("continues only with an operator") && e.contains("_"), "{q}: {e}");
    }
    assert!(refuse("= 12 h").contains("12_h"));
    let e = refuse("= 3 | 12h");
    assert!(e.contains("12_h") && e.contains("\"12h\""), "{e}");
    let e = refuse("= 3 | 12 x");
    assert!(e.contains("takes a right side"), "{e}");
}

#[test]
fn a_constructor_after_a_pipe_heads_an_expression() {
    assert_eq!(one("= 3 | @(0; $_)"), "@(0; 3)");
    assert_eq!(one("= 3 | @(0; $_) ++ @(1)"), "@(0; 3; 1)");
    assert_eq!(one("= 3 | %(v = $_) ++ %(w = 1)"), "%(v = 3; w = 1)");
    assert_eq!(one("= 3 | #(%(1; 2)) | dim"), "@(1; 2)");
    assert_eq!(one("= 3 | #(%(1; 2)) ++ %(3; 4) | dim"), "@(2; 2)");
    // the items are an alternative to stdin, never a first operand
    assert_eq!(one("= 3 | %(v = $_; n = 1)"), "%(v = 3; n = 1)");
    // the bare sigil reads stdin whole
    assert_eq!(one("= %(a = @(1; 2)) | @ | type"), "list");
    assert_eq!(one("= @(1; 2) | # | dim"), "@(2; 1)");
}

#[test]
fn after_the_aggregation_pipe_the_expression_runs_once_over_the_context() {
    assert_eq!(one("/rows/* | ::x @| @(0; $_)"), "@(0; @(1; 3))");
    assert_eq!(one("/rows/* | ::x @| 0"), "0");
    assert_eq!(one(r#"/rows/* | ::x @| "x""#), "x");
    assert_eq!(one("/rows/* | ::x @| 10 + count"), "12");
    assert_eq!(
        one("/rows/* | ::x @| %(xs = $_) ++ %(n = 1)"),
        "%(xs = @(1; 3); n = 1)"
    );
    assert_eq!(one("/rows/* | ::x @| #(%(1; 2)) | dim"), "@(1; 2)");
    // the plain aggregate record keeps its bodies over the context
    assert_eq!(one("/rows/* | ::x @| %(xs = $_; n = count)"), "%(xs = @(1; 3); n = 2)");
    // the stream read as rows is one list: a stream of records the
    // record array, as `@| # | @` gives
    assert_eq!(
        one("/rows/* | %(::k; ::x) @| @"),
        one("/rows/* | %(::k; ::x) @| # | @")
    );
    assert_eq!(one("/rows/* | %(::k; ::x) @| @ | type"), "list");
    assert_eq!(one("/rows/* | ::x @| @"), "@(1; 3)");
}

#[test]
fn the_table_constructor_takes_its_option_alone() {
    assert_eq!(one("= @(1..12) | #(--width=3) | dim"), "@(4; 3)");
    assert_eq!(one("= @(1..12) | #(-w=3) | dim"), "@(4; 3)");
    assert_eq!(
        one("= @(1..6) | #(--header=@(a; b; c)) | header"),
        r#"@("a"; "b"; "c")"#
    );
    assert_eq!(one("= @(1..6) | #(-h=@(a; b; c)) | :b"), "@(2; 5)");
    assert_eq!(one("= 1..6 @| #(-w=2) | dim"), "@(3; 2)");
    assert_eq!(one("= 1..6 @| #(--header=@(a; b)) | header"), r#"@("a"; "b")"#);
    // inside an expression the option form reads the topic where it stands
    assert_eq!(one("= @(1; 2) | (#(-w=1) | dim)"), "@(2; 1)");
    // a width that does not divide the length names both numbers
    let e = refuse("= @(1..5) | #(-w=3)");
    assert!(e.contains("5 items do not divide into rows of 3"), "{e}");
    let e = refuse("= @(1..5) | #(-h=@(a; b))");
    assert!(e.contains("5 items do not divide into rows of 2"), "{e}");
    // an item beside an option refuses, naming both
    let e = refuse("= @(1..6) | #(-w=2; %(1; 2))");
    assert!(e.contains("takes its option alone") && e.contains("'-w'"), "{e}");
    let e = refuse("= @(1..6) | #(%(1; 2); --width=2)");
    assert!(e.contains("takes its option alone") && e.contains("'--width'"), "{e}");
    // one option at a time; an unknown one refuses
    let e = refuse("= @(1..6) | #(-w=2 -h=@(a; b))");
    assert!(e.contains("both given"), "{e}");
    let e = refuse("= @(1..6) | #(--foo=3)");
    assert!(e.contains("'--foo' given"), "{e}");
    // the other constructors take no option yet
    let e = refuse("= @(1; --x=1)");
    assert!(e.contains("'@(…)' takes no option ('--x' given)"), "{e}");
    let e = refuse("= %(a = 1; --x=1)");
    assert!(e.contains("'%(…)' takes no option ('--x' given)"), "{e}");
}

#[test]
fn the_count_and_the_header_alone_are_retired() {
    for q in ["= @(1..6) | #(3)", "= 1..6 @| #(3)", "= #(3)"] {
        let e = refuse(q);
        assert!(e.contains("'#(3)' is retired") && e.contains("#(--width=3)"), "{q}: {e}");
    }
    let e = refuse("= @(1..6) | #(@(a; b; c))");
    assert!(
        e.contains("'#(@(a; b; c))' alone is retired") && e.contains("#(--header=@(a; b; c))"),
        "{e}"
    );
    // a header-only literal in an expression is still the empty frame
    assert_eq!(one("= #(@(a; b)) | header"), r#"@("a"; "b")"#);
}

#[test]
fn the_expression_head_round_trips() {
    for q in [
        "= 3 | 0 + $_ | @(0; $_) ++ @(1) | %(v = $_) ++ %(w = 1)",
        "/rows/* | ::x @| @(0; $_) ++ @(1)",
        "/rows/* | ::x @| 0",
        "= @(1..12) | #(--width=3) | #(--header=@(a; b; c))",
    ] {
        let first = quarb::Query::parse(q, &doc())
            .unwrap_or_else(|e| panic!("{q}: {e}"))
            .canonical();
        let again = quarb::Query::parse(&first, &doc())
            .unwrap_or_else(|e| panic!("{first}: {e}"))
            .canonical();
        assert_eq!(first, again, "{q}");
    }
}
