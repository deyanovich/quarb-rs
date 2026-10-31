//! The diagnostics channel at the tool (spec: Diagnostics): the
//! planner's account and the locale layer are infos and warnings
//! beside the answer, shown down to the level in force, each line
//! `severity CODE (site): message`.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-diag-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("lib.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT, pages INTEGER);
         INSERT INTO books VALUES (1, 'Iliad', 300), (2, 'Emma', 400);",
    )
    .unwrap();
    std::fs::write(dir.join("rows.json"), r#"{"rows":[{"n":1.5}]}"#).unwrap();
    dir
}

/// stdout and stderr of a run under the given switches.
fn qua(dir: &Path, args: &[&str]) -> (String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (
        String::from_utf8(out.stdout).unwrap().trim().to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn the_planner_reports_as_infos() {
    let dir = fixture("planner");
    // a plan the source answered: PUSHED, naming the statement, at
    // the info level — silent at the default level
    let (out, err) = qua(&dir, &["--diagnostics=info", "/books/* @| count", "lib.db"]);
    assert_eq!(out, "2");
    assert!(
        err.contains("info PUSHED (query): pushdown: SELECT COUNT(*) FROM books"),
        "{err}"
    );
    let (out, err) = qua(&dir, &["/books/* @| count", "lib.db"]);
    assert_eq!(out, "2");
    assert!(err.trim().is_empty(), "{err}");
    // --explain shows the infos as before, now as diagnostics
    let (_, err) = qua(&dir, &["--explain", "/books/* @| count", "lib.db"]);
    assert!(
        err.contains("info PUSHED (query): pushdown: SELECT COUNT(*) FROM books"),
        "{err}"
    );
    // what stayed with the engine says why (a text column under a
    // numeric reducer: the scan's answer, whatever it is, not
    // SQLite's)
    let (_, err) = qua(
        &dir,
        &["--diagnostics=info", "/books/* | ::title @| max", "lib.db"],
    );
    assert!(
        err.contains("info NOT_PUSHED (pushdown): pushdown: not used"),
        "{err}"
    );
    // the debug level names the plan the source was handed, and the
    // mount a scanned query ran over
    let (_, err) = qua(
        &dir,
        &[
            "--diagnostics=debug",
            "/books/*[::pages > 350]::title",
            "lib.db",
        ],
    );
    assert!(
        err.contains("debug PLAN (query): pushdown plan: SELECT"),
        "{err}"
    );
    let (_, err) = qua(
        &dir,
        &["--diagnostics=debug", "/books/* | ::title @| max", "lib.db"],
    );
    assert!(err.contains("debug MOUNT ("), "{err}");
}

#[test]
fn a_locale_without_rules_warns_and_falls_back() {
    let dir = fixture("locale");
    // the locale in force is an info; one without rules a warning,
    // the canonical form kept, shown at the default level
    let (out, err) = qua(
        &dir,
        &[
            "--diagnostics=info",
            "--locale",
            "de",
            "/rows/0::n",
            "rows.json",
        ],
    );
    assert_eq!(out, "1,5");
    assert!(
        err.contains("info LOCALE (--locale): locale: de (from --locale)"),
        "{err}"
    );
    let (out, err) = qua(&dir, &["--locale", "xx", "/rows/0::n", "rows.json"]);
    assert_eq!(out, "1.5");
    assert!(
        err.contains("warning LOCALE_UNKNOWN (--locale): locale: xx (from --locale) has no rendering rules; canonical"),
        "{err}"
    );
    // the error level hides the warning; the answer stands
    let (out, err) = qua(
        &dir,
        &[
            "--diagnostics=error",
            "--locale",
            "xx",
            "/rows/0::n",
            "rows.json",
        ],
    );
    assert_eq!(out, "1.5");
    assert!(err.trim().is_empty(), "{err}");
}
