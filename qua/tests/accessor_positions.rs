//! An accessor is built one way in every position (ruling
//! 2026-11-05): `@_` is the list of the threads' `$_`, null for a
//! thread that never projected, and a reduction over the context
//! reads the same list; `%.` is an operand wherever `$.name` is; a
//! row's positional item is named by its accessor as a record's is;
//! `:key` glued to a reference refuses an unknown core field as the
//! path form does.
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    let test = std::thread::current()
        .name()
        .unwrap_or("t")
        .replace(':', "-");
    let dir = std::env::temp_dir().join(format!("quarb-accessors-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("rows.json"),
        r#"{"row":[{"Sex":"male","Age":22},{"Sex":"female","Age":38},{"Sex":"male","Age":26}]}"#,
    )
    .unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_qua"))
        .current_dir(dir)
        .env("QUARB_LOCALE", "C")
        .env("HOME", dir)
        .args(args)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
        out.status.success(),
    )
}

fn ok(dir: &Path, query: &str) -> String {
    let (out, err, good) = qua(dir, &[query, "rows.json"]);
    assert!(good, "{query}: {err}");
    out
}

fn err(dir: &Path, query: &str) -> String {
    let (out, err, good) = qua(dir, &[query, "rows.json"]);
    assert!(!good, "{query} succeeded: {out}");
    err
}

/// A thread that never projected has a null topic: a node's name is
/// not its value. `@_`, the seed after `@|` and the reductions read
/// the threads' `$_`, one list.
#[test]
fn the_context_topics_are_the_threads_topics() {
    let d = fixture();
    assert_eq!(
        ok(&d, "/row/* @| [..2] @| %(t = @_)"),
        "%(t = @(null; null))"
    );
    assert_eq!(
        ok(&d, "/row/* @| [..2] @| %(t = $_)"),
        "%(t = @(null; null))"
    );
    assert_eq!(
        ok(&d, "/row/* @| [..2] | %(t = $_)"),
        "%(t = null)\n%(t = null)"
    );
    assert_eq!(ok(&d, "/row/* @| sum"), "null");
    assert_eq!(ok(&d, "/row/* @| max"), "null");
    // projected, the values — and the positions stay the nodes
    assert_eq!(
        ok(&d, "/row/* | ::Age @| %(t = @_)"),
        "%(t = @(22; 38; 26))"
    );
    assert_eq!(ok(&d, "/row/* | ::Age @| sum"), "86");
    assert_eq!(
        ok(&d, "/row/* @| %(p = @@_)"),
        "%(p = @(/row/0; /row/1; /row/2))"
    );
    assert_eq!(ok(&d, "/row/* | @@_::Age @| first"), "@(22; 38; 26)");
    // the thread commands keep the nodes
    assert_eq!(ok(&d, "/row/* @| first"), "/row/0");
    assert_eq!(ok(&d, "/row/* @| count"), "3");
    // a reduction that combines values says what to project
    let e = err(&d, "/row/* @| pairs");
    assert!(e.contains("a bare node reads as null"), "{e}");
}

/// `%.` is an operand wherever `$.name` is: a field, a list item, a
/// push body, a template hole, a side of `++`; as a stage it heads
/// an expression.
#[test]
fn the_named_view_is_an_operand() {
    let d = fixture();
    let fold = "/row/* | ::Sex | .s | ::Age @|{.s} mean | ";
    assert_eq!(
        ok(&d, &format!("{fold}%.")),
        "%(s = \"male\")\n%(s = \"female\")"
    );
    assert_eq!(
        ok(&d, &format!("{fold}%(k = %.)")),
        "%(k = %(s = \"male\"))\n%(k = %(s = \"female\"))"
    );
    assert_eq!(
        ok(&d, &format!("{fold}(%.)")),
        "%(s = \"male\")\n%(s = \"female\")"
    );
    assert_eq!(
        ok(&d, &format!("{fold}@(%.)")),
        "@(%(s = \"male\"))\n@(%(s = \"female\"))"
    );
    assert_eq!(
        ok(&d, &format!("{fold}.k(%.) | $.k")),
        "%(s = \"male\")\n%(s = \"female\")"
    );
    assert_eq!(
        ok(&d, &format!("{fold}\"${{%.}}\"")),
        "%(s = \"male\")\n%(s = \"female\")"
    );
    // the fold's keys, then its answer: the row of any number of keys
    assert_eq!(
        ok(&d, &format!("{fold}%. ++ %(m = $_)")),
        "%(s = \"male\"; m = 24.0)\n%(s = \"female\"; m = 38.0)"
    );
    assert_eq!(
        ok(&d, &format!("{fold}%.* ++ %(m = $_)")),
        "%(s = \"male\"; m = 24.0)\n%(s = \"female\"; m = 38.0)"
    );
}

/// A row's positional item that is an accessor is named by it, as
/// the record constructor names it; a literal stays positional.
#[test]
fn a_row_names_its_accessors() {
    let d = fixture();
    let fold = "/row/* | ::Sex | .s | ::Age @|{.s} mean | %(m = $_) | ";
    assert_eq!(
        ok(&d, &format!("{fold}++ %($.s)")),
        "%(m = 24.0; s = \"male\")\n%(m = 38.0; s = \"female\")"
    );
    assert_eq!(
        ok(&d, &format!("{fold}%($.s) ++ $_")),
        "%(s = \"male\"; m = 24.0)\n%(s = \"female\"; m = 38.0)"
    );
    let e = err(&d, "= %(a = 1) ++ %(2)");
    assert!(e.contains("a positional row appends to a table"), "{e}");
}

/// `:key` glued to a reference is the core-field read, refused
/// naming the core fields when the key is none — as on a path.
#[test]
fn a_reference_refuses_an_unknown_core_field() {
    let d = fixture();
    for q in [
        "/row/* | %(a = $$_:%(:Age))",
        "/row/* | $$_:Age",
        "/row/* | @@_:%(:Age; :Sex)",
        "/row/*:%(:Age)",
    ] {
        let e = err(&d, q);
        assert!(e.contains("':Age' is no core field"), "{q}: {e}");
    }
    assert_eq!(
        ok(&d, "/row/* @| [..2] | $$_:%(:name; ::Age)"),
        "%(name = \"0\"; Age = 22)\n%(name = \"1\"; Age = 38)"
    );
    assert_eq!(
        ok(&d, "/row/* @| [..2] | @@_:%(::Age)"),
        "%(Age = 22)\n%(Age = 38)"
    );
}
