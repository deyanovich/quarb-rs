//! Subcontext bodies (ruling 2026-10-10): every parenthesis in a
//! pipeline that does not follow a command name is a body — a
//! pipeline fed by the enclosing control operator. Its first
//! command may be headless (a built-in applied to the seed), a
//! bare word in it names a command (never text), a command's own
//! parentheses hold options where a bare word is a string. The
//! aggregate push `@| .name(body)` files once on every capsa and
//! passes the context on; `@| %(…)` is the aggregate record; `@|
//! %.` and `@| $.name` read the shared regulae once; every
//! reduction keeps the regulae its inputs share.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"w":["tom","tom","sid","sid","sid","huck"],
            "p":[{"x":1,"k":"a"},{"x":2,"k":"a"},{"x":3,"k":"b"}],
            "n":[7]}"#,
    )
    .unwrap()
}

fn values(q: &str) -> Vec<String> {
    match quarb::run(q, &doc()).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect(),
        _ => panic!("expected values"),
    }
}

fn refuse(q: &str) -> String {
    quarb::run(q, &doc()).unwrap_err().to_string()
}

#[test]
fn a_headless_first_command_applies_to_the_seed() {
    // `.n(cc)` pushes the word's length: `cc` applied to `$_`.
    assert_eq!(
        values("/w/* | :: | .n(cc) | %."),
        vec![
            "%(n = 3)", "%(n = 3)", "%(n = 3)", "%(n = 3)", "%(n = 3)", "%(n = 4)"
        ]
    );
    // ... and in a record field.
    assert_eq!(
        values("/w/* | :: @| [1] | %(w = $_; n = cc)"),
        vec!["%(w = \"tom\"; n = 3)"]
    );
    // A headless command with a tail: `cc` then arithmetic.
    assert_eq!(
        values("/w/* | :: @| [-1] | .n(cc | ($_ * 2)) | $.n"),
        vec!["8"]
    );
    // In a list item.
    assert_eq!(values("/w/* | :: @| [1] | @(cc; upper)"), vec!["3, TOM"]);
}

#[test]
fn a_bare_word_in_a_body_names_a_command() {
    // An unknown word is refused by its name, never read as text.
    let err = refuse("/w/*[:: = tom]");
    assert!(err.contains("'tom' is not a command"), "{err}");
    let err = refuse("/w/* | :: | %(u = tome)");
    assert!(err.contains("'tome' is not a command"), "{err}");
    let err = refuse("/w/* | :: | .n(tom)");
    assert!(err.contains("'tom' is not a command"), "{err}");
    let err = refuse("/w/* | :: | @(tom)");
    assert!(err.contains("'tom' is not a command"), "{err}");
    // Quoted, it is the string.
    assert_eq!(values("/w/*[:: = \"tom\"] | :: @| count"), vec!["2"]);
    // A known name is that command over the seed: `count` of one
    // word is 1, `upper` its uppercase.
    assert_eq!(
        values("/w/* | :: @| [1] | %(u = count; v = upper)"),
        vec!["%(u = 1; v = \"TOM\")"]
    );
}

#[test]
fn a_commands_options_keep_bare_words_as_strings() {
    // `sort(atergo)`: the option is the string "atergo".
    assert_eq!(
        values("/w/* | :: @| unique @| sort(atergo) @| join(\",\")"),
        vec!["sid,huck,tom"]
    );
    // The grouping call takes fields, which are bodies.
    assert_eq!(
        values("/w/* | :: @| group(n = cc) | count @| join(\",\")"),
        vec!["5,1"]
    );
}

#[test]
fn the_aggregate_push_files_once_on_every_capsa_and_passes_the_context_on() {
    // Every word carries the total; the stream is unchanged.
    assert_eq!(
        values("/w/* | :: @| .total(count) | %(w = $_; total = $.total) @| [1..2]"),
        vec!["%(w = \"tom\"; total = 6)", "%(w = \"tom\"; total = 6)"]
    );
    // A second body sees the same stream and the regula just filed.
    assert_eq!(
        values("/w/* | :: @| .total(count) @| .unique(unique @| count) @| %."),
        vec!["%(total = 6; unique = 3)"]
    );
    assert_eq!(
        values(
            "/w/* | :: @| .total(count) @| .share(unique @| count | ($_ div $.total)) @| $.share"
        ),
        vec!["0.5"]
    );
    // The body's seed is the list of the peers' topics: a node
    // context stands by its names.
    assert_eq!(values("/w/* @| .n(count) @| $.n"), vec!["6"]);
    // Nothing to file on an empty context.
    assert_eq!(
        values("/w/*[:: = \"none\"] | :: @| .n(count) @| count"),
        vec!["0"]
    );
}

#[test]
fn the_aggregate_record_builds_from_the_context_as_a_whole() {
    assert_eq!(
        values("/w/* | :: @| %(total = count; unique = unique @| count)"),
        vec!["%(total = 6; unique = 3)"]
    );
    // Parentheses around a field body are optional.
    assert_eq!(
        values("/w/* | :: @| %(total = count; unique = (unique @| count))"),
        vec!["%(total = 6; unique = 3)"]
    );
    // The enriched form opens with the shared named regulae.
    assert_eq!(
        values("/w/* | :: @| .total(count) @| %%(unique = unique @| count)"),
        vec!["%(total = 6; unique = 3)"]
    );
    // The field bodies may read the register every capsa shares.
    assert_eq!(
        values("/w/* | :: @| .total(count) @| %(share = unique @| count | ($_ div $.total))"),
        vec!["%(share = 0.5)"]
    );
}

#[test]
fn register_accessors_after_the_aggregation_pipe_read_once() {
    assert_eq!(values("/w/* | :: @| .total(count) @| $.total"), vec!["6"]);
    assert_eq!(
        values("/w/* | :: @| .total(count) @| %."),
        vec!["%(total = 6)"]
    );
    // After `|` they read per capsa, as before.
    assert_eq!(
        values("/w/* | :: @| .total(count) | $.total @| count"),
        vec!["6"]
    );
    // Only what every capsa shares: a per-capsa push is not shared.
    assert_eq!(
        values("/w/* | :: | .w @| .total(count) @| %."),
        vec!["%(total = 6)"]
    );
    // An expression over the accessor belongs to `|` or a body.
    let err = refuse("/w/* | :: @| .total(count) @| $.total * 2");
    assert!(err.contains("reads once"), "{err}");
}

#[test]
fn a_reduction_keeps_the_regulae_its_inputs_share() {
    // The filed total survives the reduction.
    assert_eq!(
        values("/w/* | :: @| .total(count) @| unique @| count | %(unique = $_; total = $.total)"),
        vec!["%(unique = 3; total = 6)"]
    );
    // A single capsa keeps its whole register across an aggregate.
    assert_eq!(
        values("/n/* | :: | .seven @| count | %."),
        vec!["%(seven = 7)"]
    );
    assert_eq!(values("/n/* | :: | .seven @| sum | $.seven"), vec!["7"]);
    // Rows that filed different values share nothing under the name.
    assert_eq!(
        values("/p/* | ::x | .x | ::k | .k @| count | %."),
        vec!["%()"]
    );
    assert_eq!(
        values("/p/*[::k = \"a\"] | ::k | .k | ::x @| sum | %."),
        vec!["%(k = \"a\")"]
    );
}

#[test]
fn the_aggregate_push_stays_out_of_expression_pipes() {
    let err = refuse("/w/* | :: | .n(($_ @| .t(count)))");
    assert!(err.contains("pushes belong to real capsae"), "{err}");
}
