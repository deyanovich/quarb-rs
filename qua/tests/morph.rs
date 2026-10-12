//! The `morph:` command library (pymorphy3 through an embedded
//! Python), the pilot of the command-library contract: the synopsis
//! checked before the run, the prefix, the input slot, options as
//! keyword arguments, the stream after `@|`, the failure mapping
//! and the run-time requirement. Built only with `--features morph`;
//! the morphology tests run where pymorphy3 and its Russian
//! dictionaries import (PYTHONPATH may point at them), and the
//! refusal for a missing package is checked where they do not.
#![cfg(feature = "morph")]

use quarb::library::CommandLibrary;
use quarb::{QueryResult, Value};
use std::sync::Arc;

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"w":["книги","стали","книга"],"p":[{"n":"книга"}]}"#)
        .unwrap()
}

fn run(q: &str) -> quarb::Result<Vec<String>> {
    let run = quarb::Run {
        libraries: vec![Arc::new(quarb_lib_morph::Morph)],
        ..quarb::Run::ambient()
    };
    let out = quarb::Query::parse(q, &doc())?.run(&doc(), &run)?;
    Ok(match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    })
}

fn refuse(q: &str) -> String {
    run(q).unwrap_err().to_string()
}

fn installed() -> bool {
    quarb_lib_morph::Morph.available().is_ok()
}

#[test]
fn the_synopsis_is_checked_before_the_run() {
    // These refusals need no Python: the gate reads the synopsis.
    let e = refuse("/w/* | :: | morph:lemma(--foo=1)");
    assert!(
        e.contains("'morph:lemma' has no option '--foo'") && e.contains("--lang="),
        "{e}"
    );
    let e = refuse("/w/* | :: | morph:lemma(extra)");
    assert!(e.contains("takes 0 operands, 1 given"), "{e}");
    let e = refuse("/w/* | :: | morph:parse(-n)");
    assert!(e.contains("takes a value"), "{e}");
    let e = refuse("/w/* | :: | morph:nope");
    assert!(
        e.contains(
            "library 'morph' has no command 'nope'; its commands: lemma, tag, pos, parse, inflect"
        ),
        "{e}"
    );
    // The prefix is required: the bare name is no command.
    let e = refuse("/w/* | :: | lemma");
    assert!(e.contains("pipeline command 'lemma'"), "{e}");
}

#[test]
fn a_library_that_is_not_linked_is_refused_by_name() {
    let plain = quarb::Run::ambient();
    let e = quarb::Query::parse("/w/* | :: | morph:lemma", &doc())
        .unwrap()
        .run(&doc(), &plain)
        .unwrap_err()
        .to_string();
    assert!(e.contains("environment 'morph' is not mounted"), "{e}");
}

#[test]
fn the_requirement_is_refused_by_name_where_it_is_missing() {
    if installed() {
        return;
    }
    let e = refuse("/w/* | :: | morph:lemma");
    assert!(
        e.contains("library 'morph' is not usable here") && e.contains("pymorphy3"),
        "{e}"
    );
}

#[test]
fn lemma_parse_and_inflect_over_russian_forms() {
    if !installed() {
        eprintln!("pymorphy3 not importable; skipping");
        return;
    }
    assert_eq!(
        run("/w/* | :: | morph:lemma").unwrap(),
        vec!["книга", "стать", "книга"]
    );
    // `$_` places the input itself; an option is a keyword argument.
    assert_eq!(
        run("/w/0 | :: | morph:lemma($_; --lang=ru)").unwrap(),
        vec!["книга"]
    );
    // The best parse is a record with its score.
    let best = run("/w/1 | :: | morph:parse(-n=1) | ... | :lemma").unwrap();
    assert_eq!(best, vec!["стать"]);
    let all = run("/w/1 | :: | morph:parse @| count").unwrap();
    assert_eq!(all, vec!["1"]);
    let n = run("/w/1 | :: | morph:parse | ... @| count").unwrap();
    assert!(n[0].parse::<i64>().unwrap() >= 2, "{n:?}");
    assert_eq!(
        run("/w/2 | :: | morph:inflect(--case=gent --number=plur)").unwrap(),
        vec!["книг"]
    );
    assert_eq!(run("/w/0 | :: | morph:pos").unwrap(), vec!["NOUN"]);
}

#[test]
fn after_the_aggregation_pipe_the_stream_crosses_once() {
    if !installed() {
        return;
    }
    // One call over the list, one capsa back per element, the
    // shared regulae carried.
    assert_eq!(
        run("/w/* | :: @| .n(count) @| morph:lemma | %(w = $_; n = $.n) @| [1]").unwrap(),
        vec!["%(w = \"книга\"; n = 3)"]
    );
    assert_eq!(
        run("/w/* | :: @| morph:lemma @| unique @| count").unwrap(),
        vec!["2"]
    );
}

#[test]
fn a_node_crosses_as_its_record() {
    if !installed() {
        return;
    }
    // A node without a topic is handed over as the record of its
    // name, text and properties; the library reads the text.
    assert_eq!(run("/p/0/n | morph:lemma").unwrap(), vec!["книга"]);
}
