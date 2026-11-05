//! The aggregate push on the plain pipe (ruling 2026-11-04): `|
//! .name@(body)` runs the body over the whole context and files its
//! value on every thread, the threads passing on; `| .name@{.x}(body)`
//! runs it per partition of the regulae in the braces (the fold's
//! selector, tuples included); `@| .name(body)` stays the aggregation
//! pipe's command: the context collapses to the one thread the pipe
//! answers, the regula filed on it (one per partition under a fold).

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"books":[
            {"title":"Iliad","genre":"epic","year":-750},
            {"title":"Odyssey","genre":"epic","year":-725},
            {"title":"Emma","genre":"novel","year":1815},
            {"title":"Hamlet","genre":"tragedy","year":1603}]}"#,
    )
    .unwrap()
}

fn rows(q: &str) -> Vec<String> {
    let run = quarb::Run::ambient();
    let out = quarb::Query::parse(q, &doc())
        .unwrap_or_else(|e| panic!("{q}: {e}"))
        .run(&doc(), &run)
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::display_form).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    }
}

fn err(q: &str) -> String {
    match quarb::Query::parse(q, &doc()) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("{q}: parsed"),
    }
}

#[test]
fn the_body_runs_over_the_whole_context_and_files_on_every_thread() {
    assert_eq!(
        rows("/books/* | ::year | .n@(count) | %(y = $_; n = $.n)"),
        [
            "%(y = -750; n = 4)",
            "%(y = -725; n = 4)",
            "%(y = 1815; n = 4)",
            "%(y = 1603; n = 4)"
        ]
    );
    // the body's stream is the context: a call with arguments reads
    // it (no stdin-slot question on the aggregate push)
    assert_eq!(
        rows("/books/* | ::title | .all@(join(\", \")) | $.all @| first"),
        ["Iliad, Odyssey, Emma, Hamlet"]
    );
    // the nameless push files under the dot
    assert_eq!(
        rows("/books/* | ::year | .@(sum) | $."),
        ["1943", "1943", "1943", "1943"]
    );
}

#[test]
fn per_partition_the_body_runs_over_each_group_in_the_threads_order() {
    assert_eq!(
        rows("/books/* | .g(::genre) | ::year | .n@{.g}(count) | %(g = $.g; y = $_; n = $.n)"),
        [
            "%(g = \"epic\"; y = -750; n = 2)",
            "%(g = \"epic\"; y = -725; n = 2)",
            "%(g = \"novel\"; y = 1815; n = 1)",
            "%(g = \"tragedy\"; y = 1603; n = 1)"
        ]
    );
    // a tuple selector, as the fold's
    assert_eq!(
        rows(
            "/books/* | .g(::genre) | .old(::year < 0) | ::year \
             | .n@{.g; .old}(count) | $.n"
        ),
        ["2", "2", "1", "1"]
    );
    // the body sees the partition's stream
    assert_eq!(
        rows("/books/* | .g(::genre) | ::title | .all@{.g}(join(\"+\")) | $.all @| unique"),
        ["Iliad+Odyssey", "Emma", "Hamlet"]
    );
}

#[test]
fn the_aggregation_pipe_push_collapses_to_the_one_thread_the_pipe_answers() {
    // standing nowhere, its topic the list it was handed, the regula on it
    assert_eq!(
        rows("/books/* | ::year @| .n(count) | %(n = $.n; t = $_)"),
        ["%(n = 4; t = @(-750; -725; 1815; 1603))"]
    );
    // under a fold, one thread per partition
    assert_eq!(
        rows("/books/* | .g(::genre) | ::year @|{.g} .n(count) | %(g = $.g; n = $.n; t = $_)"),
        [
            "%(g = \"epic\"; n = 2; t = @(-750; -725))",
            "%(g = \"novel\"; n = 1; t = @(1815))",
            "%(g = \"tragedy\"; n = 1; t = @(1603))"
        ]
    );
    // the spec's example: one thread after the first push, the second
    // push on the plain pipe over the list topic
    assert_eq!(
        rows("/books/* | ::genre @| .total(count) | .unique(unique @| count) | %."),
        ["%(total = 4; unique = 3)"]
    );
    // a selector of a name no push made, as the fold refuses it
    let e = err("/books/* | .n@{.nothere}(count)");
    assert!(e.contains("no push '.nothere'"), "{e}");
    // the sigil needs its body
    let e = err("/books/* | .n@");
    assert!(e.contains("takes a body"), "{e}");
}

#[test]
fn the_canonical_text_and_the_reflection() {
    for q in [
        "/books/* | .n@(count)",
        "/books/* | .g(::genre) | .n@{.g}(count)",
        "/books/* | .g(::genre) | .h(::year) | .n@{.g; .h}(count)",
        "/books/* @| .n(count)",
        "/books/* | .g(::genre) @|{.g} .n(count)",
    ] {
        let once = quarb::expand(q, &quarb::Defs::default()).unwrap();
        assert_eq!(once, q);
    }
    let arbor =
        quarb::reflect::QueryArbor::parse("/books/* | .g(::genre) | .n@{.g}(count)").unwrap();
    match quarb::run("/query/pipeline/agg-push::group", &arbor).unwrap() {
        QueryResult::Values(vs) => assert_eq!(vs, [Value::Str(".g".into())]),
        other => panic!("{other:?}"),
    }
}

/// In the thread tree the aggregate push is a plain step: every line
/// continues with its regula, no paragraph break.
#[test]
fn threads_prints_the_aggregate_push_as_a_step() {
    let text = rows("/books/* | ::year | .n@(count) @| threads").remove(0);
    assert!(text.contains("└─ .n@(count)  (…; 1603)"), "{text}");
    assert!(!text.contains("\n.n@(count)"), "{text}");
}

/// The rounded spelling (ruling 2026-11-04): `*` for `@`, the selector
/// in parentheses, all glued; the canonical text keeps the symbols.
#[test]
fn the_rounded_spelling_is_the_sigil_spelling() {
    assert_eq!(
        rows("/books/* | ::year | .n*(count) | $.n"),
        rows("/books/* | ::year | .n@(count) | $.n")
    );
    assert_eq!(
        rows("/books/* | .g(::genre) | ::year | .n*(.g)(count) | $.n"),
        rows("/books/* | .g(::genre) | ::year | .n@{.g}(count) | $.n")
    );
    assert_eq!(
        quarb::expand(
            "/books/* | .g(::genre) | .n*(.g)(count)",
            &quarb::Defs::default()
        )
        .unwrap(),
        "/books/* | .g(::genre) | .n@{.g}(count)"
    );
    // a body that happens to start with a dot is a body: nothing follows it
    assert_eq!(
        rows("/books/* | ::year | .h*(.5 + 1) | $.h @| first"),
        ["1.5"]
    );
}

/// A body's head call whose operands overflow its parameters takes
/// its first operand as the input, the stdin slot's reading (`sum(:n)`
/// is `(:n | sum)`); one with room for them reads the stream.
#[test]
fn a_body_call_with_a_surplus_operand_reads_it_as_the_input() {
    assert_eq!(
        rows("/books/* | %(n = ::year) | .tot@(sum(:n)) | $.tot @| first"),
        ["1943"]
    );
    assert_eq!(
        rows("/books/* | .g(::genre) | %(n = ::year) | .tot@{.g}(sum(:n)) | $.tot @| unique"),
        ["-1475", "1815", "1603"]
    );
    assert_eq!(
        rows("/books/* | %(n = ::year) @| .tot(sum(:n)) | $.tot"),
        ["1943"]
    );
    // room for the operand: the stream is the input
    assert_eq!(
        rows("/books/* | ::title | .two@(first(2)) | $.two @| first"),
        ["@(\"Iliad\"; \"Odyssey\")"]
    );
}

/// A nested record constructor builds its fields on the scope it
/// stands in: the context's peers stay in reach, as in the flat
/// record (finding 2026-11-04).
#[test]
fn a_nested_record_keeps_the_context_in_reach() {
    assert_eq!(
        rows(
            "/books/* @| %(y = %(min = (@@_::year @| min); max = (@@_::year @| max)); n = (@@_ @| count))"
        ),
        ["%(y = %(min = -750; max = 1815); n = 4)"]
    );
}
