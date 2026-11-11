//! The history suffixes (spec: .quai; quai's `:help`): `&N#` replays
//! line N's frozen footprint, `&N!` re-runs line N against a freshly
//! read source — in the session and in the `.quai` runner alike, and
//! a session goes on (and ends) cleanly after a frozen replay.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn scratch() -> PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("quai-history-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("books.json"),
        r#"{"books": [{"title": "Emma", "price": 25}, {"title": "Iliad", "price": 18}]}"#,
    )
    .unwrap();
    dir
}

fn quai(dir: &Path, args: &[&str], lines: &str) -> (String, String, bool) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_quai"))
        .args(args)
        .arg("books.json")
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(lines.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

#[test]
fn a_session_goes_on_after_a_frozen_replay() {
    let dir = scratch();
    let (out, err, ok) = quai(&dir, &[], "/books/*/title::\n&1#\n&1!\n:quit\n");
    assert!(ok, "{err}");
    assert!(!err.contains("panicked"), "{err}");
    assert_eq!(out.matches("Emma").count(), 3, "{out}");
}

#[test]
fn the_runner_reads_the_history_suffixes() {
    let dir = scratch();
    std::fs::write(
        dir.join("cells.quai"),
        "    /books/*/title::\n\n    &1#\n\n    &1!\n",
    )
    .unwrap();
    let (out, err, ok) = quai(&dir, &["--run", "cells.quai"], "");
    assert!(ok, "{out}\n{err}");
    assert!(out.contains("&2  &1#"), "{out}");
    assert!(out.contains("&3  &1!"), "{out}");
    assert_eq!(out.matches("    Emma").count(), 3, "{out}");
    assert!(!out.contains("error:"), "{out}");
}

#[test]
fn the_runner_refuses_a_replay_of_a_line_that_has_not_run() {
    let dir = scratch();
    std::fs::write(dir.join("cells.quai"), "    &9#\n").unwrap();
    let (out, _, ok) = quai(&dir, &["--run", "cells.quai"], "");
    assert!(!ok);
    assert!(out.contains("&9# has no captured result"), "{out}");
}

#[test]
fn the_runner_reads_a_hash_inside_a_cell_as_the_query_s_own() {
    let dir = scratch();
    std::fs::write(
        dir.join("cells.quai"),
        "    = #(@(A; B); %(1; \"x\"))\n\n    = 5\n\n    &1#\n",
    )
    .unwrap();
    let (out, err, ok) = quai(&dir, &["--run", "cells.quai"], "");
    assert!(ok, "{out}\n{err}");
    assert!(!out.contains("error:"), "{out}");
    assert!(out.contains("&2  = 5"), "{out}");
    assert!(out.contains("&3  &1#"), "{out}");
}

/// A history reference heads a pipeline (finding 2026-11-05): `&N#`
/// hands the captured result to the tail, `&N!` the live re-run, as
/// the bare `&N | …` re-runs the line through the macro table; a
/// session recall in parentheses opens a query.
#[test]
fn the_runner_reads_a_history_reference_heading_a_pipeline() {
    let dir = scratch();
    std::fs::write(
        dir.join("cells.quai"),
        "    /books/*/price::\n\n    &1# | ($_ * 2)\n\n    &1! @| sum\n\n    &1 @| count\n\n    \
         /books/* @| # | ,t\n\n    ($,t | count)\n\n    &1# @| sum\n",
    )
    .unwrap();
    let (out, err, ok) = quai(&dir, &["--run", "cells.quai"], "");
    assert!(ok, "{out}\n{err}");
    assert!(!out.contains("error:"), "{out}");
    assert!(
        out.contains("&2  &1# | ($_ * 2)\n    50\n    36\n"),
        "{out}"
    );
    assert!(out.contains("&3  &1! @| sum\n    43\n"), "{out}");
    assert!(out.contains("&4  &1 @| count\n    2\n"), "{out}");
    assert!(out.contains("&6  ($,t | count)\n    2\n"), "{out}");
    assert!(out.contains("&7  &1# @| sum\n    43\n"), "{out}");
}

/// A value-headed cell — a constructor, a literal — is recalled like
/// any other: `&N | ,df` files the cell's value, `&N#` and `&N!`
/// stand too (finding 2026-11-05: a `@(…)` head read as the
/// aggregation pipe, so the line never became a macro).
#[test]
fn a_constructor_headed_cell_is_recalled() {
    let dir = scratch();
    std::fs::write(
        dir.join("cells.quai"),
        "    @(%(Name = \"a\"; Age = 1); %(Name = \"b\"; Age = 2)) | #\n\n    &1 | ,df\n\n    \
         ($,df | count)\n\n    &1# | count\n\n    &1! | count\n\n    @%(@(a); %(1); %(2))\n\n    &6 | count\n",
    )
    .unwrap();
    let (out, err, ok) = quai(&dir, &["--run", "cells.quai"], "");
    assert!(ok, "{out}\n{err}");
    assert!(!out.contains("error:"), "{out}");
    assert!(out.contains("&3  ($,df | count)\n    2\n"), "{out}");
    assert!(out.contains("&4  &1# | count\n    2\n"), "{out}");
    assert!(out.contains("&5  &1! | count\n    2\n"), "{out}");
    assert!(out.contains("&7  &6 | count\n    2\n"), "{out}");
}

/// A headed frozen reference to a line with no result (`&9# | …`) in
/// a notebook reports its error and the runner goes on: matched in
/// place, the session's borrow lived across the arms and the error
/// arm panicked ("RefCell already borrowed"), losing every cell
/// after it (the code review of 2026-11-10).
#[test]
fn the_runner_survives_a_headed_reference_to_an_empty_line() {
    let dir = scratch();
    std::fs::write(
        dir.join("nb.quai"),
        "    &9# @| count\n\n    /books/*::title\n",
    )
    .unwrap();
    let (out, err, ok) = quai(&dir, &["--run", "nb.quai"], "");
    assert!(!err.contains("panicked"), "{err}");
    assert!(out.contains("&9# has no captured result"), "{out}");
    // the cell after it ran
    assert!(out.contains("Emma") && out.contains("Iliad"), "{out}");
    // and the run reports the failed cell by its exit
    assert!(!ok);
}
