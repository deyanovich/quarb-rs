//! `--of=expr` / `-o=expr` on every reducing aggregate (spec:
//! Aggregating Pipe, draft 164): the expression is read on each
//! item of the input in turn, as a key is, and the reduction runs
//! over the results; a key written as an operand is refused with
//! the synopsis (`sum(:n)` inside an expression stays `(:n | sum)`).

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"rows":[{"k":"a","x":1,"p":2,"q":3},{"k":"b","x":3,"p":1,"q":1},{"k":"a","x":5,"p":2,"q":2}]}"#,
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
fn the_option_reads_an_expression_on_each_item_over_the_context() {
    assert_eq!(one("/rows/* @| sum(--of=::x)"), "9");
    assert_eq!(one("/rows/* @| sum(-o=::x)"), "9");
    assert_eq!(one("/rows/* @| count(--of=::x)"), "3");
    assert_eq!(one("/rows/* @| max(--of=::x)"), "5");
    assert_eq!(one("/rows/* @| mean(--of=::x * ::p)"), "5");
    // an ordering reduction keeps the context, the results its topics
    assert_eq!(
        run("/rows/* @| sort(--of=::x * 2)").unwrap(),
        vec!["2", "6", "10"]
    );
    // with the listed operands, the options last
    assert_eq!(one("/rows/* @| percentile(50; --of=::x)"), "3");
    assert_eq!(run("/rows/* @| first(2; --of=::x)").unwrap(), vec!["1", "3"]);
    assert_eq!(one("/rows/* @| variance(--sample --of=::x)"), "4");
    // the position stays in reach: a node's own property
    assert_eq!(one("/rows/* | ::k @| join(\", \"; --of=$$_::x)"), "1, 3, 5");
    // a null result is a missing value
    assert_eq!(one("/rows/* @| sum(--of=::missing)"), "");
}

#[test]
fn after_group_the_option_reads_each_member() {
    assert_eq!(
        run("/rows/* | %(::k; ::x) @| group(::k) | sum(--of=:x)").unwrap(),
        vec!["6", "3"]
    );
    assert_eq!(
        run("/rows/* | %(::k; ::x; ::p) @| group(::k) | mean(-o=:x * :p)").unwrap(),
        vec!["6", "3"]
    );
    // the spec's own example
    assert_eq!(
        run("/rows/* | %(::k; n = ::x) @| group(::k) | sum(--of=:n) | %($.k; total = $_)")
            .unwrap(),
        vec!["%(k = \"a\"; total = 6)", "%(k = \"b\"; total = 3)"]
    );
    // a list topic without members: each item as the topic
    assert_eq!(
        one("= @(%(x = 1); %(x = 2)) | sum(--of=:x * 10)"),
        "30"
    );
}

#[test]
fn a_numeric_reducer_over_lists_says_so() {
    let e = refuse("= @(@(1; 2; 3); @(10; 20)) | ... @| sum");
    assert!(
        e.contains("sum: the input holds lists, not numbers") && e.contains("'| ... @| sum'"),
        "{e}"
    );
    let e = refuse("= @(@(1; 2); @(3)) | ... @| mean");
    assert!(e.contains("mean: the input holds lists"), "{e}");
    // the two spellings it points at
    assert_eq!(
        run("= @(@(1; 2; 3); @(10; 20)) | ... | sum").unwrap(),
        vec!["6", "30"]
    );
    assert_eq!(one("= @(@(1; 2; 3); @(10; 20)) | ... | ... @| sum"), "36");
    // a list among numbers still reduces the numbers
    assert_eq!(one("= @(1; @(2); 3) | ... @| sum"), "4");
}

#[test]
fn a_reducer_in_a_body_over_a_group_takes_the_members_together() {
    assert_eq!(
        run("/rows/* | %(::k; ::x) @| group(::k) | %(k = $.k; n = count; s = ($_ | sum(--of=:x)))").unwrap(),
        [r#"%(k = "a"; n = 2; s = 6)"#, r#"%(k = "b"; n = 1; s = 3)"#]
    );
    assert_eq!(
        run("/rows/* | %(::k; ::x) @| group(::k) | %(n = ($_ | count))").unwrap(),
        ["%(n = 2)", "%(n = 1)"]
    );
    // a navigation body still runs per member
    assert_eq!(
        run("/rows/* | %(::k; ::x) @| group(::k) | %(xs = ($_ | :x @| join(\",\")))").unwrap(),
        [r#"%(xs = "1,5")"#, r#"%(xs = "3")"#]
    );
}

#[test]
fn a_key_written_as_an_operand_is_refused_with_the_synopsis() {
    let e = refuse("/rows/* @| sum(::x)");
    assert!(e.contains("'sum' takes no operand, 1 given") && e.contains("synopsis"), "{e}");
    let e = refuse("/rows/* | %(::k; ::x) @| group(::k) | sum(:x)");
    assert!(e.contains("'sum' takes no operand, 1 given"), "{e}");
    let e = refuse("/rows/* @| variance(::x; --sample)");
    assert!(e.contains("variance takes no operand"), "{e}");
    // the positional word is retired with the pointer to the switch
    let e = refuse("/rows/* @| variance(sample)");
    assert!(e.contains("'sample' is an option: write variance(--sample)"), "{e}");
    let e = refuse("/rows/* | ::x @| zscore(sample; ::k)");
    assert!(e.contains("'sample' is an option: write zscore(--sample)"), "{e}");
    // nested, the first argument is the input: `(:x | sum)`
    assert_eq!(
        run("/rows/* | %(::k; ::x) @| group(::k) | %($.k; total = sum(:x))").unwrap(),
        vec!["%(k = \"a\"; total = 6)", "%(k = \"b\"; total = 3)"]
    );
    // the option needs its expression; an unknown option refuses
    let e = refuse("/rows/* @| sum(--of)");
    assert!(e.contains("takes the expression to reduce"), "{e}");
    let e = refuse("/rows/* @| sum(--foo=1)");
    assert!(e.contains("'sum' takes '--of=expr' ('-o=expr')") && e.contains("'--foo' given"), "{e}");
    // a keyed aggregate keeps its key operand
    assert_eq!(
        run("/rows/* | ::x @| sort_by($_) | ::x").unwrap_or_default().len(),
        3
    );
}

#[test]
fn the_option_round_trips_through_unparse() {
    for q in [
        "/rows/* @| sum(--of=::x)",
        "/rows/* @| mean(--of=::x * ::p)",
        "/rows/* | %(::k; ::x) @| group(::k) | variance(--sample --of=:x)",
    ] {
        let printed = quarb::Query::parse(q, &doc()).unwrap().canonical();
        let again = quarb::Query::parse(&printed, &doc()).unwrap().canonical();
        assert_eq!(printed, again, "{q}");
    }
}
