//! Environment commands (spec: Environment Commands, Environment
//! Mounts): `ALIAS:name(operands; options)` run by the environment
//! the host mounted under the alias — `exec:` a directory of
//! programs, `shell:` the user's shell — with the text framing of
//! the shell command. The mount is the opt-in: an unmounted alias
//! refuses before the run.

use quarb::{EnvKind, Environment, QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"w":["pear","apple","fig"],
            "p":[{"n":"a","x":2},{"n":"b","x":1}]}"#,
    )
    .unwrap()
}

fn run_with(q: &str, envs: Vec<Environment>) -> quarb::Result<Vec<String>> {
    let run = quarb::Run {
        environments: envs,
        ..quarb::Run::ambient()
    };
    let out = quarb::Query::parse(q, &doc())?.run(&doc(), &run)?;
    Ok(match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    })
}

fn bin() -> Environment {
    Environment::parse("u", "exec:/usr/bin").unwrap()
}

fn shell() -> Environment {
    Environment::parse("sh", "shell:").unwrap()
}

#[test]
fn the_mount_grammar_names_the_kinds() {
    assert_eq!(bin().kind, EnvKind::Exec);
    assert_eq!(shell().kind, EnvKind::Shell);
    // Not an environment: a source.
    assert!(Environment::parse_mount("ga=events.json").is_none());
    assert!(Environment::parse_mount("t=text:book.txt").is_none());
    // An environment, malformed or not yet built.
    let e = Environment::parse_mount("go=exec:/no/such/dir")
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(e.contains("no such directory or program"), "{e}");
    let e = Environment::parse_mount("sh=shell:bash")
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(e.contains("takes no target"), "{e}");
    let e = Environment::parse_mount("py=python:./helpers.py")
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(e.contains("plugin round"), "{e}");
    let e = Environment::parse_mount("9x=exec:/usr/bin")
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(e.contains("alias is a word"), "{e}");
}

#[test]
fn an_unmounted_alias_refuses_before_the_run() {
    let e = run_with("/w/* | :: | sh:tr(a-z; A-Z)", vec![])
        .unwrap_err()
        .to_string();
    assert!(e.contains("environment 'sh' is not mounted"), "{e}");
    // The prefix is required: a bare name never falls through.
    let e = run_with("/w/* | :: | tr(a-z; A-Z)", vec![shell()])
        .unwrap_err()
        .to_string();
    assert!(e.contains("pipeline command 'tr'"), "{e}");
}

#[test]
fn after_the_plain_pipe_once_per_capsa_on_the_topics_text() {
    assert_eq!(
        run_with("/w/* | :: | u:tr(a-z; A-Z)", vec![bin()]).unwrap(),
        vec!["PEAR", "APPLE", "FIG"]
    );
    // Operands are words in order; an expression operand evaluates
    // per capsa.
    assert_eq!(
        run_with("/p/* | ::n | u:printf(\"%s-%s\"; $_; ::x)", vec![bin()]).unwrap(),
        vec!["a-2", "b-1"]
    );
    // A node stands by its name on the input.
    assert_eq!(
        run_with("/w/* | u:tr(a-z; A-Z)", vec![bin()]).unwrap(),
        vec!["0", "1", "2"]
    );
}

#[test]
fn after_the_aggregation_pipe_once_over_the_context_as_lines() {
    let sorted = run_with("/w/* | :: @| u:sort", vec![bin()]).unwrap();
    assert_eq!(sorted, vec!["apple", "fig", "pear"]);
    // Options go first, in shell form.
    assert_eq!(
        run_with("/w/* | :: @| u:sort(-r)", vec![bin()]).unwrap(),
        vec!["pear", "fig", "apple"]
    );
    assert_eq!(
        run_with("/w/* | :: @| u:wc(-l) | trim", vec![bin()]).unwrap(),
        vec!["3"]
    );
    // Records cross as their JSON line.
    assert_eq!(
        run_with("/p/* | %(n = ::n; x = ::x) @| u:head(-n=1)", vec![bin()]).unwrap(),
        vec!["{\"n\": \"a\", \"x\": 2}"]
    );
    // The shared regulae ride every returned capsa.
    assert_eq!(
        run_with(
            "/w/* | :: @| .total(count) @| u:sort @| [1] | %(w = $_; total = $.total)",
            vec![bin()]
        )
        .unwrap(),
        vec!["%(w = \"apple\"; total = 3)"]
    );
}

#[test]
fn the_shell_kind_reaches_the_users_shell() {
    assert_eq!(
        run_with("/w/* | :: @| [1] | sh:echo(hi; there)", vec![shell()]).unwrap(),
        vec!["hi there"]
    );
    // A `shell:` mount opens the shell gate: `sh(...)` runs, its
    // command read raw to the balancing paren; shell quotes shield
    // a paren and the backtick spelling is the same lexeme.
    assert_eq!(
        run_with("/w/* | :: @| [1] | sh(tr a-z A-Z)", vec![shell()]).unwrap(),
        vec!["PEAR"]
    );
    assert_eq!(
        run_with(
            "/w/* | :: @| [1] | sh(tr 'a-z' \"A-Z\" | sed 's/^/(/; s/$/)/')",
            vec![shell()]
        )
        .unwrap(),
        vec!["(PEAR)"]
    );
    assert_eq!(
        run_with("/w/* | :: @| [1] | `tr a-z A-Z`", vec![shell()]).unwrap(),
        vec!["PEAR"]
    );
    // The quoted form is retired: its quotes would reach the shell.
    let e = run_with("/w/* | :: @| [1] | sh('tr a-z A-Z')", vec![shell()]).unwrap_err();
    assert!(
        e.to_string().contains("sh reads its command raw: sh(cmd)"),
        "{e}"
    );
}

#[test]
fn failures_are_null_per_capsa_and_a_refusal_over_the_context() {
    // A failed call (nonzero exit) after `|` is null.
    assert_eq!(
        run_with("/w/* | :: @| [1] | u:false | default(\"nil\")", vec![bin()]).unwrap(),
        vec!["nil"]
    );
    // ... and refuses after `@|`.
    let e = run_with("/w/* | :: @| u:false", vec![bin()])
        .unwrap_err()
        .to_string();
    assert!(e.contains("'false' failed"), "{e}");
    // An unknown name refuses on either pipe, naming the alias.
    let e = run_with("/w/* | :: | u:no_such_program_here", vec![bin()])
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("environment 'u' has no command 'no_such_program_here'"),
        "{e}"
    );
    let e = run_with("/w/* | :: @| [1] | sh:no_such_program_here", vec![shell()])
        .unwrap_err()
        .to_string();
    assert!(e.contains("environment 'sh' has no command"), "{e}");
}
