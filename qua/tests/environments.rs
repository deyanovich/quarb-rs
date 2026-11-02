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
    // The python kind: a module file that exists, or an import
    // path; an interpreter alone is no target.
    let e = Environment::parse_mount("py=python:./no-such-helpers.py")
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(e.contains("no such module file"), "{e}");
    let e = Environment::parse_mount("py=python:python3")
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(e.contains("an interpreter alone is no target"), "{e}");
    let e = Environment::parse_mount("py=python:/usr/bin/python")
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(e.contains("an interpreter alone is no target"), "{e}");
    let e = Environment::parse_mount("py=python:")
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(e.contains("names no module"), "{e}");
    let py = Environment::parse_mount("py=python:pkg.helpers")
        .unwrap()
        .unwrap();
    assert_eq!(py.kind, EnvKind::Python);
    assert_eq!(py.target, "pkg.helpers");
    let e = Environment::parse_mount("9x=exec:/usr/bin")
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(e.contains("alias is a word"), "{e}");
}

#[test]
fn a_python_mount_a_host_did_not_bind_refuses_naming_the_feature() {
    let py = Environment::parse("py", "python:textwrap").unwrap();
    let e = run_with("/w/* | :: | py:dedent", vec![py])
        .unwrap_err()
        .to_string();
    assert!(e.contains("python feature"), "{e}");
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
    assert_eq!(
        run_with("/w/* | :: @| [1] | sh:tr(a-z; A-Z)", vec![shell()]).unwrap(),
        vec!["PEAR"]
    );
    // The shell joins a query only as the mount (ruling 2026-11-02):
    // the literal spellings refuse with the pointer, mounted or not.
    for q in [
        "/w/* | :: @| [1] | sh(tr a-z A-Z)",
        "/w/* | :: @| [1] | sh('tr a-z A-Z')",
        "/w/* | :: @| [1] | `tr a-z A-Z`",
        "/w/* | :: @| [1] | (;tr a-z A-Z)",
    ] {
        let e = run_with(q, vec![shell()]).unwrap_err().to_string();
        assert!(
            e.contains("is retired") && e.contains("sh=shell:"),
            "{q}: {e}"
        );
    }
}

#[test]
fn an_option_takes_a_comma_after_its_equals_as_its_value() {
    // `cut -d ,` in Quarb's spellings: the comma after the option's
    // `=` is the value; the quoted getopt form passes verbatim. A
    // comma glued to the option's NAME is the separator sugar, so
    // `-d, -f2` hands cut `-d -f2` and cut refuses.
    for q in [
        "/w/* | :: @| [1] | u:echo(\"a,b,c\") | u:cut(--delimiter=, --fields=2)",
        "/w/* | :: @| [1] | u:echo(\"a,b,c\") | u:cut(-d=, -f=2)",
        "/w/* | :: @| [1] | u:echo(\"a,b,c\") | u:cut(\"-d,\"; -f2)",
    ] {
        assert_eq!(run_with(q, vec![bin()]).unwrap(), vec!["b"], "{q}");
    }
    assert!(
        run_with(
            "/w/* | :: @| [1] | u:echo(\"a,b,c\") | u:cut(-d, -f2)",
            vec![bin()]
        )
        .is_err()
    );
}

#[test]
fn headless_a_text_callee_reads_its_first_operand_on_stdin() {
    // At the head of an expression the first operand is the stdin
    // slot (spec: The stdin slot): `u:tr(abc; a-z; A-Z)` feeds abc.
    assert_eq!(
        run_with("= u:tr(abc; a-z; A-Z)", vec![bin()]).unwrap(),
        vec!["ABC"]
    );
    // after a pipe the pipe is the slot, the operands the words
    assert_eq!(
        run_with("= \"abc\" | u:tr(a-z; A-Z)", vec![bin()]).unwrap(),
        vec!["ABC"]
    );
    // the alias `sh`, once a command word, is an alias like any
    // other at the head
    assert_eq!(
        run_with("= sh:tr(abc; a-z; A-Z)", vec![shell()]).unwrap(),
        vec!["ABC"]
    );
}

#[test]
fn the_allow_shell_switch_is_retired_naming_the_mount() {
    let dir = std::env::temp_dir().join(format!("quarb-allow-shell-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("w.json"), r#"{"w":["pear"]}"#).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .arg("--allow-shell")
        .arg("/w/*::")
        .arg(dir.join("w.json"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--allow-shell is retired") && err.contains("sh=shell:"),
        "{err}"
    );
    std::fs::remove_dir_all(&dir).ok();
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
