//! A positional read past the pushes of its side is refused at parse
//! (spec: The Capsa; ruling 2026-11-11): `$$.2` with one mark as
//! `$.2` with one value. Each side's count is an upper bound: a dot
//! after a pipe files whichever face the topic has, so it counts
//! toward both. A side stands unchecked where its count is not the
//! parser's to know: a push that may run more than once, and on the
//! node side a query that moves references as values.

use quarb::{QueryResult, Value};

fn org() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"teams":[
            {"name":"web","floor":3,"members":[{"name":"ada"},{"name":"bo"}]},
            {"name":"data","floor":4,"members":[{"name":"cy"}]}]}"#,
    )
    .unwrap()
}

fn nest() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"v":0,"a":{"v":1,"a":{"v":2,"a":{"v":3}}}}"#).unwrap()
}

fn answer(q: &str, doc: &quarb_json::JsonAdapter) -> Result<Vec<String>, String> {
    let run = quarb::Run::ambient();
    let out = quarb::Query::parse(q, doc)
        .map_err(|e| e.to_string())?
        .run(doc, &run)
        .map_err(|e| e.to_string())?;
    Ok(match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::display_form).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    })
}

fn rows(q: &str) -> Vec<String> {
    answer(q, &org()).unwrap_or_else(|e| panic!("{q}: {e}"))
}

fn err(q: &str) -> String {
    match answer(q, &org()) {
        Err(e) => e,
        Ok(rows) => panic!("{q}: ran, {rows:?}"),
    }
}

#[test]
fn a_mark_position_past_the_marks_is_refused_as_a_value_position_is() {
    // one value pushed, one node marked: the second of either is not there
    let e = err("/teams/* . | .(::floor) | /members/* | %(a = $.1; b = $.2)");
    assert!(
        e.contains("no regula at position '.2'; the query pushes 1 value"),
        "{e}"
    );
    let e = err("/teams/* . | .(::floor) | /members/* | %(a = $$.1::name; b = $$.2::name)");
    assert!(
        e.contains("no mark at position '.2'; the query marks 1 node"),
        "{e}"
    );
    // read bare, as a record's item, as a path's start, as the thread's seat
    for q in [
        "/teams/* . | /members/* | $$.2",
        "/teams/* . | /members/* | %(a = $$.1; b = $$.2)",
        "/teams/* . | /members/* | $$.2/members/*::name",
        "/teams/* . | /members/* | $$.2 | ::name",
    ] {
        let e = err(q);
        assert!(
            e.contains("no mark at position '.2'; the query marks 1 node"),
            "{q}: {e}"
        );
    }
    // the latest and the counted-back positions, and a query with no mark
    let e = err("/teams/* . | /members/* | $$.-2::name");
    assert!(
        e.contains("no mark at position '.-2'; the query marks 1 node"),
        "{e}"
    );
    let e = err("/teams/* | /members/* | $$.");
    assert!(
        e.contains("no mark at position '.'; the query marks 0 nodes"),
        "{e}"
    );
    let e = err("/teams/* . | /members/* . | $$.3::name");
    assert!(
        e.contains("no mark at position '.3'; the query marks 2 nodes"),
        "{e}"
    );
}

#[test]
fn positions_within_the_marks_answer() {
    assert_eq!(
        rows("/teams/* . | .(::floor) | /members/* | %(a = $$.1::name; c = $.1)"),
        [
            "%(a = \"web\"; c = 3)",
            "%(a = \"web\"; c = 3)",
            "%(a = \"data\"; c = 4)"
        ]
    );
    assert_eq!(
        rows(
            "/teams/*[1] .t | /members/*[1] . \
             | %(a = $$.1::name; b = $$.2::name; c = $$.t::name; d = $$.::name; e = $$.-2::name)"
        ),
        ["%(a = \"web\"; b = \"ada\"; c = \"web\"; d = \"ada\"; e = \"web\")"]
    );
    // read bare as the thread's seat: the node itself
    assert_eq!(
        rows("/teams/* . | /members/* | $$.1 | ::name"),
        ["web", "web", "data"]
    );
}

#[test]
fn a_dot_after_a_pipe_counts_on_both_sides() {
    // on a node it files a mark, yet the parser cannot know the
    // topic's face: it counts toward the values and toward the marks
    assert_eq!(
        rows("/teams/* | . | .(::floor) | /members/* | %(a = $$.1::name; c = $.1; d = $.2)"),
        [
            "%(a = \"web\"; c = 3; d = null)",
            "%(a = \"web\"; c = 3; d = null)",
            "%(a = \"data\"; c = 4; d = null)"
        ]
    );
    let e = err("/teams/* | . | .(::floor) | /members/* | $$.2::name");
    assert!(
        e.contains("no mark at position '.2'; the query marks 1 node"),
        "{e}"
    );
    let e = err("/teams/* | . | .(::floor) | /members/* | $.3");
    assert!(
        e.contains("no regula at position '.3'; the query pushes 2 values"),
        "{e}"
    );
}

#[test]
fn a_push_of_one_reference_is_a_mark() {
    // `.t` is `.t($$_)`: the written form counts as the mark it files
    assert_eq!(
        rows("/teams/* | .t($$_) | /members/* | $$.1::name"),
        ["web", "web", "data"]
    );
    let e = err("/teams/* | .t($$_) | /members/* | $$.2::name");
    assert!(
        e.contains("no mark at position '.2'; the query marks 1 node"),
        "{e}"
    );
    // and so does the push of a recalled mark, which files it again
    assert_eq!(
        rows("/teams/* | .t($$_) | /members/* | .u($.t) | $$.2::name"),
        ["web", "web", "data"]
    );
    let e = err("/teams/* | .t($$_) | /members/* | .u($.t) | $$.3::name");
    assert!(
        e.contains("no mark at position '.3'; the query marks 2 nodes"),
        "{e}"
    );
}

#[test]
fn the_marks_stand_uncounted_where_references_move_as_values() {
    // a reference made the topic (`$$_` as a value) and filed by a
    // dot: one more mark, which the parser does not count
    assert_eq!(
        rows("/teams/* . | /members/* | $$_ | . | $$.2::name"),
        ["ada", "bo", "cy"]
    );
    // a list of references filed by one push: two more marks
    assert_eq!(
        rows("/teams/* | .t($$_) | .u(@($$.t; $$.t)) | /members/* | $$.3::name"),
        ["web", "web", "data"]
    );
    // `collapse` gathers the folded threads' marks into lists
    assert_eq!(
        rows("/teams/* . | /members/* . @| collapse | $$.2::name"),
        ["ada", "bo", "cy"]
    );
    // nothing is refused there: a position past the marks reads null
    assert_eq!(
        rows("/teams/*[1] . | /members/*[1] | $$_ | . | %(x = $$.9::name)"),
        ["%(x = null)"]
    );
    // a mark stood on again and marked by a dot is navigation, not
    // a value: counted as written
    assert_eq!(
        rows("/teams/* . | /members/* | $$.1 | . | $$.2::name"),
        ["web", "web", "data"]
    );
    let e = err("/teams/* . | /members/* | $$.1 | . | $$.3::name");
    assert!(
        e.contains("no mark at position '.3'; the query marks 2 nodes"),
        "{e}"
    );
}

#[test]
fn a_push_that_may_run_more_than_once_stands_uncounted() {
    let nest = nest();
    let rows = |q: &str| answer(q, &nest).unwrap_or_else(|e| panic!("{q}: {e}"));
    // a mark in a repeated group files once per round
    assert_eq!(
        rows("(/a .){2} | %(x = $$.1::v; y = $$.2::v)"),
        ["%(x = 1; y = 2)"]
    );
    assert_eq!(
        rows("(/a .)+ | %(x = $$.1::v; y = $$.2::v; z = $$.3::v)"),
        [
            "%(x = 1; y = null; z = null)",
            "%(x = 1; y = 2; z = null)",
            "%(x = 1; y = 2; z = 3)"
        ]
    );
    // and so does a value push there (it was refused as one value)
    assert_eq!(
        rows("(/a .(::v)){2} | %(x = $.1; y = $.2)"),
        ["%(x = 1; y = 2)"]
    );
    assert_eq!(
        rows("(/a .(::v))+ | %(x = $.1; y = $.2; z = $.3)"),
        [
            "%(x = 1; y = null; z = null)",
            "%(x = 1; y = 2; z = null)",
            "%(x = 1; y = 2; z = 3)"
        ]
    );
    // the quantified push files one regula per round
    assert_eq!(
        rows("/a | .v(::v){2} | %(x = $.1; y = $.2)"),
        ["%(x = 1; y = 1)"]
    );
    // a group walked at most once is counted as written
    let e = answer("(/a .)? | $$.2::v", &nest).unwrap_err();
    assert!(
        e.contains("no mark at position '.2'; the query marks 1 node"),
        "{e}"
    );
    let e = answer("(/a .(::v))? | $.2", &nest).unwrap_err();
    assert!(
        e.contains("no regula at position '.2'; the query pushes 1 value"),
        "{e}"
    );
}

/// A name that holds a value, read as a mark, is an error (ruling
/// 2026-11-11), where it answered nothing: refused at parse where
/// every push of the name files a value. A mark's name read as a
/// value stays what it was, the node's reference.
#[test]
fn a_value_read_as_a_mark_is_refused() {
    for q in [
        "/teams/* | .t(::name) | /members/* | $$.t::name",
        "/teams/* | .t(::name) | /members/* | $$.t",
        "/teams/* | .t(::name) | /members/* | %(a = $$.t::name)",
        "/teams/* | .t(::name) | /members/* | ((@t))::name",
        "/teams/* | .t(/members/* @| count) | $$.t",
        "/teams/* | .%(t = ::name) | $$.t::name",
    ] {
        let e = err(q);
        assert!(
            e.contains("'.t' holds a value, not a node: read it as '$.t'"),
            "{q}: {e}"
        );
    }
    // a name no push made is refused under `$$` as under `$`
    let e = err("/teams/* | /members/* | $$.zz::name");
    assert!(e.contains("no push '.zz' before this point"), "{e}");
    let e = err("/teams/* | /members/* | ((@zz))::name");
    assert!(e.contains("no push '.zz' before this point"), "{e}");
    let e = err("/teams/* | /members/* | $.zz");
    assert!(e.contains("no push '.zz' before this point"), "{e}");
    // a mark, a push of one reference, and a name a later mark took
    // over all read as marks
    assert_eq!(
        rows("/teams/* .t | /members/* | $$.t::name"),
        ["web", "web", "data"]
    );
    assert_eq!(
        rows("/teams/* | .t($$_) | /members/* | $$.t::name"),
        ["web", "web", "data"]
    );
    assert_eq!(
        rows("/teams/* | .t(::name) | /members/* .t | $$.t::name"),
        ["ada", "bo", "cy"]
    );
    // where the parser cannot tell, the run refuses (ruling
    // 2026-11-11, "refuse at runtime"): a dot after a pipe files
    // whichever face the topic has, and a name may be a mark first
    // and a value later; the name's latest regula decides
    for q in [
        "/teams/* | ::name | .t | $$.t::name",
        "/teams/* .t | /members/* | .t(::name) | $$.t::name",
        "/teams/* .t | /members/* | .t(::name) | ((@t))::name",
    ] {
        let e = err(q);
        assert!(
            e.contains("refused") && e.contains("'.t' holds a value, not a node"),
            "{q}: {e}"
        );
    }
    // a dot after a pipe on a node is a mark, and reads as one
    assert_eq!(
        rows("/teams/* | .t | /members/* | $$.t::name"),
        ["web", "web", "data"]
    );
    // a thread on which the name is not filed at all is a miss, as
    // it was: the name is some other thread's
    assert_eq!(
        rows("/teams/* .t || /teams/*/members/* | %(n = ::name; t = $$.t::name)"),
        [
            "%(n = \"web\"; t = \"web\")",
            "%(n = \"data\"; t = \"data\")",
            "%(n = \"ada\"; t = null)",
            "%(n = \"bo\"; t = null)",
            "%(n = \"cy\"; t = null)"
        ]
    );
}
