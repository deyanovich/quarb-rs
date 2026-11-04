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
