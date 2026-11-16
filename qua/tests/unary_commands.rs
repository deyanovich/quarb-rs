//! The unary operators as commands, and the negation at the head of
//! a pipe item (ruling 2026-11-15).
//!
//! `neg` and `not` are the unary minus and `!` to pipe into:
//! `| ::n | neg`, `| ::ok | not`, with the calls `neg(::n)` and
//! `not(::ok)`. `!` may head a pipe item and an expression
//! (`| !::ok`, `= !true`). The minus may not: an operator of two
//! sides standing there is the topic's own (`| - 3` is `$_ - 3`).

fn run(args: &[&str]) -> (bool, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout)
            .unwrap()
            .trim_end()
            .to_string(),
        String::from_utf8(out.stderr)
            .unwrap()
            .trim_end()
            .to_string(),
    )
}

fn fixture(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("qua-unary-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("d.json");
    std::fs::write(
        &path,
        r#"{"items": [{"n": 3, "ok": false}, {"n": -2, "ok": true}]}"#,
    )
    .unwrap();
    path.to_string_lossy().into_owned()
}

/// The answer's lines; a failure is the test's.
fn lines(query: &str, file: &str) -> Vec<String> {
    let (success, out, err) = run(&[query, file]);
    assert!(success, "{query}: {err}");
    out.lines().map(str::to_string).collect()
}

/// The refusal a query answers.
fn refusal(query: &str, file: &str) -> String {
    let (success, _, err) = run(&[query, file]);
    assert!(!success, "{query} was taken");
    err
}

#[test]
fn neg_and_not_are_piped_into() {
    let f = fixture("piped");
    assert_eq!(lines("/items/* | ::n | neg", &f), ["-3", "2"]);
    assert_eq!(lines("/items/* | ::ok | not", &f), ["true", "false"]);
    // a property the node lacks is null, and null is false
    assert_eq!(lines("/items/* | ::absent | not", &f), ["true", "true"]);
    assert_eq!(lines("/items/* | ::absent | neg", &f), ["null", "null"]);
    // they chain as any command does
    assert_eq!(lines("/items/* | ::n | neg | abs", &f), ["3", "2"]);
    assert_eq!(lines("/items/* | ::ok | not | not", &f), ["false", "true"]);
}

#[test]
fn the_calls_ride_their_operand() {
    let f = fixture("calls");
    assert_eq!(lines("= neg(3)", &f), ["-3"]);
    assert_eq!(lines("= not(false)", &f), ["true"]);
    assert_eq!(lines("= not(null)", &f), ["true"]);
    assert_eq!(
        lines("/items/* | %(a = not(::ok); b = neg(::n))", &f),
        ["%(a = true; b = -3)", "%(a = false; b = 2)"]
    );
    // in a predicate `not(x)` is the call; the bare word before an
    // operand stays retired
    assert_eq!(lines("/items/*[not(::ok)]::n", &f), ["3"]);
    assert_eq!(lines("/items/*[neg(::n) > 0]::n", &f), ["-2"]);
    let err = refusal("/items/*[not ::ok]::n", &f);
    assert!(err.contains("the bare word 'not' is retired"), "{err}");
    // after a pipe the pipe fills the one slot: an operand is one
    // too many
    let err = refusal("/items/* | not(::ok)", &f);
    assert!(err.contains("'not' takes no operand"), "{err}");
    let err = refusal("/items/* | neg(::n)", &f);
    assert!(err.contains("'neg' takes no operand"), "{err}");
}

#[test]
fn the_commands_agree_with_the_operators_on_one_value() {
    let f = fixture("agree");
    for v in ["5", "2.5", "\"7\"", "5_km", "3_h", "null"] {
        assert_eq!(
            lines(&format!("= {v} | neg"), &f),
            lines(&format!("= {v} | (-$_)"), &f),
            "{v}"
        );
    }
    for v in ["true", "false", "0", "3", "\"\"", "\"x\"", "null", "0_km"] {
        assert_eq!(
            lines(&format!("= {v} | not"), &f),
            lines(&format!("= {v} | (!$_)"), &f),
            "{v}"
        );
    }
    // the unary minus keeps a quantity's unit (it answered null)
    assert_eq!(lines("= 5_km | (-$_)", &f), ["-5_km"]);
    assert_eq!(lines("= 5_km | neg", &f), ["-5_km"]);
}

#[test]
fn a_list_is_read_item_by_item() {
    let f = fixture("lists");
    assert_eq!(lines("= @(1; -2) | neg", &f), ["@(-1; 2)"]);
    assert_eq!(lines("= @(22; 38) > 35 | not", &f), ["@(true; false)"]);
    assert_eq!(
        lines("= %(a = 1; b = 0) | not", &f),
        ["%(a = false; b = true)"]
    );
    // the operator before a list reads the list's own truth
    assert_eq!(lines("= @() | not", &f), ["@()"]);
    assert_eq!(lines("= @() | (!$_)", &f), ["true"]);
}

#[test]
fn a_value_neg_does_not_take_warns_once() {
    let f = fixture("warns");
    let (success, out, err) = run(&["= \"x\" | neg", &f]);
    assert!(success, "{err}");
    assert_eq!(out, "null");
    assert!(
        err.contains("NOT_NUMERIC") && err.contains("neg takes a number or a quantity"),
        "{err}"
    );
}

#[test]
fn a_negation_heads_a_pipe_item_and_an_expression() {
    let f = fixture("head");
    assert_eq!(lines("/items/* | !::ok", &f), ["true", "false"]);
    assert_eq!(lines("= !true", &f), ["false"]);
    assert_eq!(lines("= 3 | !$_", &f), ["false"]);
    assert_eq!(lines("= 0 | !$_", &f), ["true"]);
    // a structural test: no child of that name
    assert_eq!(lines("^ | !/items", &f), ["false"]);
    assert_eq!(lines("^ | !/nothing", &f), ["true"]);
    // the connectives, the null default and the conditional follow
    // it as after any first operand
    assert_eq!(lines("/items/* | !::ok && ::n > 2", &f), ["true", "false"]);
    assert_eq!(lines("= !false && true", &f), ["true"]);
    assert_eq!(lines("/items/* | !::ok ? \"a\" : \"b\"", &f), ["a", "b"]);
    // the same reading as the parenthesized form, which is how it
    // is written back
    assert_eq!(
        lines("/items/* | !::ok && ::n > 2", &f),
        lines("/items/* | (!::ok && ::n > 2)", &f)
    );
    let (success, out, err) = run(&["--expand", "/items/* | !::ok && ::n > 2", &f]);
    assert!(success, "{err}");
    assert_eq!(out, "/items/* | (!::ok && ::n > 2)");
    let (_, out, _) = run(&["--expand", "= !true", &f]);
    assert_eq!(out, "= (!true)");
}

#[test]
fn a_minus_heading_a_pipe_item_is_still_the_topics() {
    let f = fixture("minus");
    // `| - 3` subtracts from the topic, `| -3` is the literal
    assert_eq!(lines("= 5 | - 3", &f), ["2"]);
    assert_eq!(lines("= 5 | -3", &f), ["-3"]);
    // so a glued minus on a projection is no negation there
    let err = refusal("/items/* | -::n", &f);
    assert!(err.contains("expected a command name after '|'"), "{err}");
    // the parenthesized form and the command are
    assert_eq!(lines("/items/* | (-::n)", &f), ["-3", "2"]);
}

#[test]
fn man_knows_both() {
    let (success, out, err) = run(&["man", "neg"]);
    assert!(success, "{err}");
    assert!(out.contains("neg{number}()"), "{out}");
    let (success, out, err) = run(&["man", "not"]);
    assert!(success, "{err}");
    assert!(out.contains("not{value}()"), "{out}");
}
