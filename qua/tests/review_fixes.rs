//! The engine rows of the external spec review (lang's triage of
//! 2026-11-06): structures equal item by item, the empty record,
//! the boolean scrutinee, the ambiguous kilobyte, a regex construct
//! the engine has no reading of, the window span, captures from a
//! navigation predicate, and the refusal's "value mode".

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-review-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("books.json"),
        r#"{"books": [{"title": "The Histories", "year": -430}, {"title": "Dracula", "year": 1897},
                     {"title": "The Divine Comedy", "year": 1320}]}"#,
    )
    .unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap().trim().to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let (success, out, err) = qua(dir, args);
    assert!(success, "{args:?}: {err}");
    out
}

fn refused(dir: &Path, args: &[&str]) -> String {
    let (success, out, err) = qua(dir, args);
    assert!(!success, "{args:?} answered: {out}");
    err
}

#[test]
fn structures_are_equal_item_by_item() {
    let d = fixture("eq");
    assert_eq!(ok(&d, &["= @(1; @(2; 3)) = @(1; @(2; 3))"]), "true");
    assert_eq!(ok(&d, &["= @(1; @(2; 4)) = @(1; @(2; 3))"]), "false");
    assert_eq!(ok(&d, &["= %(a = @(1; 2)) = %(a = @(1; 2))"]), "true");
    assert_eq!(ok(&d, &["= @(@(1); @(2)) | sort"]), "@(@(1); @(2))");
}

#[test]
fn the_empty_record_reads_back() {
    let d = fixture("rec");
    assert_eq!(ok(&d, &["= %()"]), "%()");
    assert_eq!(ok(&d, &["= %() | type"]), "record");
    assert_eq!(ok(&d, &["= %() | count"]), "0");
}

#[test]
fn a_boolean_scrutinee_is_the_plain_conditional() {
    let d = fixture("match");
    let e = refused(&d, &["= (true ?= true ? 1 : 2)"]);
    assert!(
        e.contains("the plain conditional is '(cond ? a : b)'"),
        "{e}"
    );
    assert_eq!(ok(&d, &["= (1 ?= 1 ? \"a\" : \"b\")"]), "a");
}

#[test]
fn the_ambiguous_kilobyte_is_refused_at_the_literal() {
    let d = fixture("kb");
    let e = refused(&d, &["= 1_KB"]);
    assert!(
        e.contains("no unit 'KB' (kB is 1000 B, KiB is 1024 B)"),
        "{e}"
    );
    assert_eq!(ok(&d, &["= 1_kB | unit"]), "kB");
    assert_eq!(ok(&d, &["= 1_KiB | unit"]), "KiB");
}

#[test]
fn a_regex_construct_without_a_reading_refuses_by_name() {
    let d = fixture("regex");
    let e = refused(
        &d,
        &["/books/*[::title == (/(?=D)une/)]::title", "books.json"],
    );
    assert!(e.contains("bad regex '/(?=D)une/': look-around"), "{e}");
    assert!(e.contains("(at character"), "{e}");
    let e = refused(
        &d,
        &["/books/*[::title == (/(a)\\1/)]::title", "books.json"],
    );
    assert!(e.contains("backreferences are not supported"), "{e}");
    assert_eq!(
        ok(
            &d,
            &["/books/*[::title == (/Dracula/)]::title", "books.json"]
        ),
        "Dracula"
    );
}

#[test]
fn a_window_span_may_lie_on_one_side_of_the_row() {
    // the predecessors and the successors are windows too; a span
    // reaching nothing is the empty window, never a refusal
    let d = fixture("window");
    assert_eq!(ok(&d, &["= 5 @| window(5..)"]), "@()");
    assert_eq!(ok(&d, &["= 5 @| window(-1..1)"]), "@(5)");
    let e = refused(&d, &["= 5 @| window(3..1)"]);
    assert!(e.contains("start <= end"), "{e}");
}

#[test]
fn a_navigation_predicate_binds_its_captures() {
    let d = fixture("captures");
    assert_eq!(
        ok(
            &d,
            &["/books/*[::title == (/^The (\\w+)/)] | $1", "books.json"]
        ),
        "Histories\nDivine"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "/books/*[::title == (/^The (?<w>\\w+)/)] | %+",
                "books.json"
            ]
        ),
        "%(w = \"Histories\")\n%(w = \"Divine\")"
    );
    // the pipeline predicate binds as before
    assert_eq!(
        ok(
            &d,
            &["/books/* | [::title == (/^The (\\w+)/)] | $1", "books.json"]
        ),
        "Histories\nDivine"
    );
}

#[test]
fn the_refusal_says_value_mode() {
    let d = fixture("mode");
    let e = refused(&d, &["/books/*:title | /x", "books.json"]);
    assert!(e.contains("cannot navigate in value mode"), "{e}");
    // push is the one term (ruling 2026-11-10): the pointer says it
    assert!(e.contains("push it first with '| .'"), "{e}");
    assert!(!e.contains("file it"), "{e}");
}
