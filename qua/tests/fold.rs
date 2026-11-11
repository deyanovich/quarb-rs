//! The fold by a regula (spec: The Pipeline, the fold by a regula):
//! `@|{.name}` partitions the context by the regula's value and runs
//! the aggregation-pipe command per partition as `@|` runs it over the
//! whole context; `@|{.}` folds by the last push, `@|{.-1}` and
//! `@|{.2}` by position; a mark's node is the fold's position; null
//! is a group; `@|` is unchanged.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"books":[
            {"title":"Iliad","genre":"epic","year":-750,"tags":["war","gods"]},
            {"title":"Odyssey","genre":"epic","year":-725,"tags":["sea"]},
            {"title":"Emma","genre":"novel","year":1815,"tags":["love","wit","money"]},
            {"title":"Hamlet","genre":"tragedy","tags":[]}]}"#,
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
    let run = quarb::Run::ambient();
    match quarb::Query::parse(q, &doc()) {
        Err(e) => e.to_string(),
        Ok(parsed) => match parsed.run(&doc(), &run) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("{q}: ran"),
        },
    }
}

#[test]
fn a_fold_by_a_named_regula_runs_the_aggregator_per_group() {
    assert_eq!(
        rows("/books/* | .g(::genre) @|{.g} count"),
        vec!["2", "1", "1"]
    );
    assert_eq!(
        rows("/books/* | .g(::genre) @|{.g} count | %(g = $.g; n = $_)"),
        vec![
            "%(g = \"epic\"; n = 2)",
            "%(g = \"novel\"; n = 1)",
            "%(g = \"tragedy\"; n = 1)"
        ]
    );
    // order and selection commands keep threads, per group
    assert_eq!(
        rows("/books/* | .g(::genre) @|{.g} sort_by(::year; -d) @|{.g} first(1) | ::title"),
        vec!["Odyssey", "Emma", "Hamlet"]
    );
    // the global fold is unchanged
    assert_eq!(rows("/books/* | .g(::genre) @| count"), vec!["4"]);
}

#[test]
fn a_fold_by_a_mark_stands_on_the_node() {
    assert_eq!(
        rows("/books/* | .b | /tags/* @|{.b} count | %(::title; tags = $_)"),
        vec![
            "%(title = \"Iliad\"; tags = 2)",
            "%(title = \"Odyssey\"; tags = 1)",
            "%(title = \"Emma\"; tags = 3)"
        ]
    );
}

#[test]
fn a_fold_by_position_and_the_null_group() {
    assert_eq!(
        rows("/books/* | .g(::genre) | .y(::year) @|{.-2} count"),
        vec!["2", "1", "1"]
    );
    assert_eq!(
        rows("/books/* | .g(::genre) | .y(::year) @|{.1} count"),
        vec!["2", "1", "1"]
    );
    // a missing year is the null group, beside the dated ones
    assert_eq!(
        rows("/books/* | .d(::year) @|{.d} count | %(d = $.d; n = $_)"),
        vec![
            "%(d = -750; n = 1)",
            "%(d = -725; n = 1)",
            "%(d = 1815; n = 1)",
            "%(d = null; n = 1)"
        ]
    );
}

#[test]
fn the_fold_s_selector_is_refused_when_misspelled() {
    assert!(err("/books/* | .g(::genre) @| {.g} count").contains("glued to the pipe"));
    assert!(err("/books/* | .g(::genre) @|{g} count").contains("names a regula as the capsa does"));
    assert!(err("/books/* | .g(::genre) @|{.5} count").contains("no regula at position"));
    // the dot forms count the value side alone, as `$.` does, and a
    // position past the capsa refuses at parse time, the recall too
    assert!(err("/books/* | .b | /tags/* @|{.} count").contains("no regula at position '.'"));
    assert!(err("/books/* | .g(::genre) | $.3").contains("no regula at position '.3'"));
    assert!(err("/books/* | $.").contains("no regula at position '.'"));
    assert_eq!(
        rows("/books/* | .g(::genre) | .y(::year) | $.-2 @| unique"),
        vec!["epic", "novel", "tragedy"]
    );
}

/// A recall of a name no push in the query made is a parse-time
/// refusal naming the regulae that exist, for `$.name` and `@|{.name}`
/// alike (spec: The Capsa, ruling 2026-11-04): order-blind, a body
/// seeing the enclosing names, a quarq literal's body exempt.
#[test]
fn a_recall_of_a_name_no_push_made_is_refused() {
    let e = err("/books/* | .g(::genre) | $.nothere");
    assert!(e.contains("no push '.nothere' before this point"), "{e}");
    assert!(e.contains("the regulae are .g"), "{e}");
    let e = err("/books/* | .g(::genre) | 5 | .n | $.nothere");
    assert!(e.contains("the regulae are .g, .n"), "{e}");
    let e = err("/books/* | .g(::genre) @|{.nothere} count");
    assert!(e.contains("no push '.nothere'"), "{e}");
    let e = err("/books/* | $.x");
    assert!(e.contains("nothing is pushed"), "{e}");
    // a recall before its push, a body reading the enclosing name,
    // and a quarq's own names
    assert_eq!(rows("= 5 | .f(q($_ * $.k)) | .k(2) | &$.f"), vec!["10"]);
    assert_eq!(rows("= 5 | .k(2) | (3 | $_ * $.k)"), vec!["6"]);
    assert_eq!(
        rows("= 5 | .f(q($_ * $.later)) | 2 | .later(3) | &$.f"),
        vec!["6"]
    );
}

/// Several references fold by the tuple of their values (ruling
/// 2026-11-04): the regulae stay on the folded thread where they
/// were pushed (ruling 2026-11-10: neither is pushed again), a mark
/// among them is the answer's position, and the refusals are the
/// single form's.
#[test]
fn a_fold_by_several_regulae_partitions_by_the_tuple() {
    assert_eq!(
        rows(
            "/books/* | .g(::genre) | .old(::year < 1800) @|{.g; .old} count \
             | %($.g; $.old; n = $_)"
        ),
        [
            "%(g = \"epic\"; old = true; n = 2)",
            "%(g = \"novel\"; old = false; n = 1)",
            "%(g = \"tragedy\"; old = false; n = 1)"
        ]
    );
    // one reference merges what the tuple keeps apart
    assert_eq!(
        rows("/books/* | .g(::genre) | .old(::year < 1800) @|{.old} count"),
        ["2", "2"]
    );
    // the top is the regula pushed last in the text, whatever order
    // the selector names them in (the last reference was pushed
    // again on top, so the selector's order decided it)
    assert_eq!(
        rows("/books/* | .g(::genre) | .old(::year < 1800) @|{.old; .g} count | $."),
        ["true", "false", "false"]
    );
    assert_eq!(
        rows("/books/* | .g(::genre) | .old(::year < 1800) @|{.g; .old} count | $."),
        ["true", "false", "false"]
    );
    assert_eq!(
        rows("/books/* | .g(::genre) | .old(::year < 1800) @|{.old; .g} count | @."),
        [
            "@(\"epic\"; true)",
            "@(\"novel\"; false)",
            "@(\"tragedy\"; false)"
        ]
    );
    // the members as one list per tuple; no space needed after `;`
    assert_eq!(
        rows("/books/* | .g(::genre) | .old(::year < 1800) @|{.g;.old} (@@_::title)"),
        ["@(\"Iliad\"; \"Odyssey\")", "@(\"Emma\")", "@(\"Hamlet\")"]
    );
    // a mark among the references: the answer stands on its node
    assert_eq!(
        rows("/books/* | .b | .g(::genre) @|{.b; .g} first | :name"),
        ["0", "1", "2", "3"]
    );
    assert!(err("/books/* | .g(::genre) @|{.g; .nothere} count").contains("no push '.nothere'"));
    assert!(err("/books/* | .g(::genre) @|{.g;} count").contains("the selector names a regula"));
}

/// The selector reflects as written, the references joined by `; `.
#[test]
fn the_tuple_selector_reflects_as_written() {
    let arbor =
        quarb::reflect::QueryArbor::parse("/books/* | .g(::genre) | .h(::year) @|{.g;.h} count")
            .unwrap();
    match quarb::run("/query/pipeline/agg::group", &arbor).unwrap() {
        QueryResult::Values(vs) => assert_eq!(vs, [Value::Str(".g; .h".into())]),
        other => panic!("{other:?}"),
    }
}

/// The rounded selector (ruling 2026-11-04): the references in
/// parentheses glued to either pipe spelling say what the braces
/// say; spaced, the selector is refused naming the glued form.
#[test]
fn the_rounded_selector_is_the_braces_selector() {
    let braces = rows("/books/* | .g(::genre) | .old(::year < 1800) @|{.g; .old} count | $.");
    for q in [
        "/books/* | .g(::genre) | .old(::year < 1800) *__(.g; .old) count | $.",
        "/books/* | .g(::genre) | .old(::year < 1800) @|(.g; .old) count | $.",
        "/books/* | .g(::genre) | .old(::year < 1800) *__{.g; .old} count | $.",
    ] {
        assert_eq!(rows(q), braces, "{q}");
    }
    for q in [
        "/books/* | .g(::genre) @| (.g) count",
        "/books/* | .g(::genre) *__ (.g) count",
        "/books/* | .g(::genre) *__ {.g} count",
    ] {
        assert!(
            err(q).contains("glued to the pipe; write @|{.g} or *__(.g)"),
            "{q}"
        );
    }
}
