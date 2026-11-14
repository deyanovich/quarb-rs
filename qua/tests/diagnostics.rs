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
    // --diagnostics=info shows the infos as before, now as diagnostics
    let (_, err) = qua(&dir, &["--diagnostics=info", "/books/* @| count", "lib.db"]);
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
            "/rows/1::n",
            "rows.json",
        ],
    );
    assert_eq!(out, "1,5");
    assert!(
        err.contains("info LOCALE (--locale): locale: de (from --locale)"),
        "{err}"
    );
    let (out, err) = qua(&dir, &["--locale", "xx", "/rows/1::n", "rows.json"]);
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
            "/rows/1::n",
            "rows.json",
        ],
    );
    assert_eq!(out, "1.5");
    assert!(err.trim().is_empty(), "{err}");
}

/// Raw stdout, stderr and status of a run, whatever its outcome.
fn raw(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn colour_paints_the_severity_word_only_when_asked_or_on_a_terminal() {
    let dir = fixture("colour");
    // A pipe is no terminal: the automatic rule leaves the line
    // plain, so a captured stderr reads the same whatever the
    // terminal would have shown.
    let (ok, err) = raw(
        &dir,
        &["--diagnostics=info", "/books/* | ::pages", "lib.db"],
    );
    assert!(ok, "{err}");
    assert!(err.contains("info "), "{err}");
    assert!(!err.contains('\x1b'), "{err}");
    // `--color=always` paints the severity word and nothing else.
    let (ok, err) = raw(
        &dir,
        &[
            "--color=always",
            "--diagnostics=info",
            "/books/* | ::pages",
            "lib.db",
        ],
    );
    assert!(ok, "{err}");
    assert!(err.contains("\x1b[32minfo\x1b[0m "), "{err}");
    // The refusal that ends a run: `Error` painted, the text as before.
    let (ok, err) = raw(&dir, &["--color=always", "/books/* | nope", "lib.db"]);
    assert!(!ok);
    assert!(err.starts_with("\x1b[1;31mError\x1b[0m: "), "{err}");
    let (ok, plain) = raw(&dir, &["/books/* | nope", "lib.db"]);
    assert!(!ok);
    assert!(plain.starts_with("Error: "), "{plain}");
    assert_eq!(plain, err.replace("\x1b[1;31m", "").replace("\x1b[0m", ""));
    // `--color=never` under a forcing environment stays plain.
    let (_, err) = raw(&dir, &["--color=never", "/books/* | nope", "lib.db"]);
    assert!(!err.contains('\x1b'), "{err}");
}

#[test]
fn info_and_debug_are_the_levels_shorter() {
    let dir = fixture("shorthand");
    let (_, long) = qua(
        &dir,
        &["--diagnostics=info", "/books/* | ::pages", "lib.db"],
    );
    let (_, short) = qua(&dir, &["--info", "/books/* | ::pages", "lib.db"]);
    assert!(long.contains("info "), "{long}");
    assert_eq!(long, short);
    let (_, debug) = qua(&dir, &["--debug", "/books/* | ::pages", "lib.db"]);
    assert!(debug.contains("info "), "{debug}");
    // One level at a time: the shorthand and the switch conflict.
    let (ok, err) = raw(
        &dir,
        &["--info", "--diagnostics=error", "/books/*", "lib.db"],
    );
    assert!(!ok && err.contains("cannot be used with"), "{err}");
}
