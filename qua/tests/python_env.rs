//! The `python:` environment kind (spec: Environment Mounts,
//! Environment Commands, The stdin slot): `py=python:./helpers.py`
//! mounts a module, its functions the commands under the alias with
//! the native framing — the input the first positional parameter
//! unless `$_` places it, operands positional, options keyword
//! arguments, a raised exception null after `|` and a refusal after
//! `@|`, one thread per returned value after `@|`. Built only with
//! `--features python`.
#![cfg(feature = "python")]

use quarb::{QueryResult, Value};
use std::sync::Arc;

const HELPERS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/helpers.py");

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"w":["pear","apple","fig"],
            "p":[{"n":"a","x":2},{"n":"b","x":1}]}"#,
    )
    .unwrap()
}

fn mounted() -> Arc<dyn quarb::CommandLibrary> {
    Arc::new(quarb_pyvalue::ModuleEnvironment::mount("py", HELPERS).unwrap())
}

fn run(q: &str) -> quarb::Result<Vec<String>> {
    let run = quarb::Run {
        libraries: vec![mounted()],
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

#[test]
fn the_mount_imports_the_module_or_refuses() {
    let e = quarb_pyvalue::ModuleEnvironment::mount("py", "/no/such/helpers.py").unwrap_err();
    assert!(e.contains("'py=python:/no/such/helpers.py'"), "{e}");
    let e = quarb_pyvalue::ModuleEnvironment::mount("py", "no_such_module_xyz").unwrap_err();
    assert!(e.contains("No module named"), "{e}");
    // An import path: a module of the standard library imports.
    let m = quarb_pyvalue::ModuleEnvironment::mount("txt", "textwrap").unwrap();
    assert_eq!(quarb::CommandLibrary::prefix(&m), "txt");
}

#[test]
fn after_the_plain_pipe_the_input_is_the_first_positional() {
    assert_eq!(
        run("/w/* | :: | py:shout").unwrap(),
        vec!["PEAR", "APPLE", "FIG"]
    );
    // `py:round_to(1)` is `round_to(x, 1)`: the operand fills the
    // next parameter.
    assert_eq!(run("= 0.666 | py:round_to(1)").unwrap(), vec!["0.7"]);
    // An option is a keyword argument.
    assert_eq!(run("= 0.666 | py:round_to(--n=2)").unwrap(), vec!["0.67"]);
}

#[test]
fn the_topic_variable_places_the_input() {
    // `py:sub("^p"; "P"; $_)` is `sub("^p", "P", x)`.
    assert_eq!(
        run(r#"/w/* | :: | py:sub("^p"; "P"; $_)"#).unwrap(),
        vec!["Pear", "apple", "fig"]
    );
    // `py:tag(--data=$_)` is `tag(data=x)`.
    assert_eq!(
        run(r#"/w/* | :: | py:tag(--data=$_ --k="w")"#).unwrap(),
        vec!["%(w = \"pear\")", "%(w = \"apple\")", "%(w = \"fig\")"]
    );
}

#[test]
fn values_cross_as_the_hosts_own() {
    assert_eq!(
        run("/w/* | :: | py:kind").unwrap(),
        vec!["str", "str", "str"]
    );
    assert_eq!(run("/p/* | ::x | py:kind").unwrap(), vec!["int", "int"]);
    assert_eq!(
        run("/p/* | %(n = ::n; x = ::x) | py:kind").unwrap(),
        vec!["dict", "dict"]
    );
    assert_eq!(run("/p/1 | @(::n; ::x) | py:kind").unwrap(), vec!["list"]);
    // A node crosses as the record of its name, text and properties.
    assert_eq!(run("/p/* | py:kind").unwrap(), vec!["dict", "dict"]);
}

#[test]
fn after_the_aggregation_pipe_one_call_over_the_stream() {
    assert_eq!(run("/p/* | ::x @| py:total").unwrap(), vec!["3"]);
    // One thread per returned value, each carrying the shared regulae.
    assert_eq!(run("/p/* | ::x @| py:spread").unwrap(), vec!["4", "2"]);
    assert_eq!(
        run("/p/* | ::x | .n@(count) @| py:spread | %(v = $_; n = $.n)").unwrap(),
        vec!["%(v = 4; n = 2)", "%(v = 2; n = 2)"]
    );
}

#[test]
fn a_raised_exception_is_null_per_capsa_and_a_refusal_over_the_context() {
    assert_eq!(
        run("/w/* | :: | py:fail | default(\"nil\")").unwrap(),
        vec!["nil", "nil", "nil"]
    );
    let e = refuse("/w/* | :: @| py:fail");
    assert!(e.contains("py:fail") && e.contains("no"), "{e}");
}

#[test]
fn an_unknown_name_refuses_naming_the_environment() {
    let e = refuse("/w/* | :: | py:nope");
    assert!(
        e.contains("environment 'py'") && e.contains("has no 'nope'") && e.contains("helpers.py"),
        "{e}"
    );
}

#[test]
fn an_environment_is_not_pure_so_nothing_is_withheld() {
    let text = run(&format!("= \"{HELPERS}\" | py:read_file")).unwrap();
    assert!(text[0].contains("def shout"), "{}", text[0]);
}

#[test]
fn the_cli_mounts_a_module_and_refuses_an_interpreter() {
    let dir = std::env::temp_dir().join(format!("quarb-python-env-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let json = dir.join("w.json");
    std::fs::write(&json, r#"{"w":["pear","fig"]}"#).unwrap();
    // The mount stands among the inputs, after the query.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .arg("/w/* | :: | py:shout")
        .arg(format!("py=python:{HELPERS}"))
        .arg(&json)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "PEAR\nFIG\n");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .arg("/w/* | :: | py:shout")
        .arg("py=python:python3")
        .arg(&json)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("an interpreter alone is no target"), "{err}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn headless_the_first_operand_fills_the_slot_when_the_operands_reach() {
    // At the head of an expression or in a subcontext body no pipe
    // fills the stdin slot (spec: The stdin slot, ruling 2026-11-02):
    // the first operand does when the operands reach the parameters,
    // the seed otherwise; after a pipe the pipe fills it and a
    // surplus operand refuses.
    let math: Arc<dyn quarb::CommandLibrary> =
        Arc::new(quarb_pyvalue::ModuleEnvironment::mount("m", "math").unwrap());
    let run_math = |q: &str| -> quarb::Result<Vec<String>> {
        let run = quarb::Run {
            libraries: vec![math.clone()],
            ..quarb::Run::ambient()
        };
        let out = quarb::Query::parse_with(q, &quarb::Defs::default(), &doc(), &run)?
            .run(&doc(), &run)?;
        Ok(match out.result {
            QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect(),
            QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
        })
    };
    assert_eq!(run_math("= m:pow(2; 3)").unwrap(), vec!["8.0"]);
    assert_eq!(run_math("= 2 | m:pow(3)").unwrap(), vec!["8.0"]);
    assert_eq!(
        run_math("= 2 | %(p = m:pow(3))").unwrap(),
        vec!["%(p = 8.0)"]
    );
    assert_eq!(
        run_math("= 2 | %(p = m:pow(2; 3))").unwrap(),
        vec!["%(p = 8.0)"]
    );
    let e = run_math("= 2 | m:pow(2; 3)").unwrap_err().to_string();
    assert!(e.contains("takes 1 operand, 2 given"), "{e}");
    // the module fixture: `round_to(x, n=0)` headless with one
    // operand takes the seed as x, with two takes neither
    assert_eq!(
        run("/p/* | ::x | %(r = py:round_to(1))").unwrap(),
        vec!["%(r = 2)", "%(r = 1)"]
    );
    assert_eq!(
        run("/p/1 | %(r = py:round_to(2.567; 1))").unwrap(),
        vec!["%(r = 2.6)"]
    );
}
