//! Navigation stages: round-trips and the mode discipline.
//!
//! `| /path` is a branch in stage position — the pipeline spelling
//! of a path continuation. Legal only in navigation mode; a push
//! returns a scalar-mode thread to navigation mode (spec:
//! Execution Modes).

use quarb::Defs;
use quarb::expand;

fn canon(q: &str) -> String {
    expand(q, &Defs::default()).unwrap()
}

fn refuse(q: &str) -> String {
    expand(q, &Defs::default()).unwrap_err().to_string()
}

#[test]
fn nav_stages_round_trip() {
    for q in [
        // plain hop, descent, projection endings
        "/teams/* | /members/*",
        "/teams/* | /members/*::name",
        // collect along the way — the Data Model page's hero
        "/teams/* | .team(::name) | .floor(::floor) \
         | /members/* | .who(::name) | .role(::role) | %.",
        // anchors: root, mark; arrows; a quantified walk in
        // stage position
        "/a/b | ^/c",
        "/a .m | /b | (m)/c",
        "/x | ->ref",
        "/e | (->manager_id)+::name",
        // file-first: push reopens navigation
        "/a | ::name | . | /kids/*",
    ] {
        assert_eq!(canon(q), q, "not a fixpoint: {q}");
    }
}

#[test]
fn arithmetic_paths_stay_expressions() {
    // A dangling operator after the path means the stage was a
    // value expression all along — the operand reading survives
    // (null-propagating arithmetic over child values).
    assert_eq!(canon("/items/* | /price:: * /qty::"), "/items/* | (/price:: * /qty::)");
}

#[test]
fn parenthesized_paths_stay_expressions() {
    // A parenthesized path is a value expression (existence /
    // first-value operand semantics), not navigation — and it
    // must round-trip WITH its parentheses, or the reparse would
    // read it as a hop.
    assert_eq!(canon("/a | (/kids/*::name)"), "/a | (/kids/*::name)");
}

#[test]
fn scalar_mode_refuses_hops() {
    let e = refuse("/a::name | /kids/*");
    assert!(e.contains("scalar mode"), "unexpected message: {e}");
    assert!(e.contains("| ."), "the fix should be named: {e}");
    let e = refuse("/a | ::name | /kids/*");
    assert!(e.contains("scalar mode"), "unexpected message: {e}");
}

#[test]
fn context_pipes_refuse_hops() {
    let e = refuse("/a @| /kids/*");
    assert!(e.contains("per-thread"), "unexpected message: {e}");
    let e = refuse("/a | ... $| /kids/*");
    assert!(e.contains("map pipe"), "unexpected message: {e}");
}
