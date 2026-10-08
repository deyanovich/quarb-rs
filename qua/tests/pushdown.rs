//! A pushed plan answers as the scan does. Quarb compares by value
//! (numeric-looking text meets a number as a number); SQLite by the
//! column's type, so a comparison pushes only where the catalog
//! says the two agree, and the query scans otherwise.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-pushdown-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("lib.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT, code TEXT, born INTEGER);
         INSERT INTO authors VALUES
           (1, 'Homer', '150', -800), (2, 'Austen', '150.0', 1775),
           (3, 'Twain', ' 150', 1835), (4, 'Gibbon', '151', 1737),
           (5, 'Herodotus', 'abc', 0);
         CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT, pages);
         INSERT INTO books VALUES
           (1, 'Iliad', 300), (2, 'Emma', '300'), (3, 'Tom Sawyer', 300.0),
           (4, 'Decline and Fall', 3000);",
    )
    .unwrap();
    dir
}

/// stdout, and what `--explain` said on stderr.
fn qua(dir: &Path, query: &str) -> (String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(["--explain", query, "lib.db"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{query}: {err}");
    (
        String::from_utf8(out.stdout).unwrap().trim().to_string(),
        err,
    )
}

#[test]
fn a_typed_column_pushes() {
    let dir = fixture("typed");
    let (out, err) = qua(&dir, "/authors/*[::born > 1700]::name");
    assert_eq!(out, "Austen\nTwain\nGibbon");
    assert!(
        err.contains("pushdown: SELECT name FROM authors WHERE born > 1700"),
        "{err}"
    );
    let (out, err) = qua(&dir, "/authors/*[::name = 'Homer']::born");
    assert_eq!(out, "-800");
    assert!(
        err.contains("pushdown: SELECT born FROM authors WHERE name = 'Homer'"),
        "{err}"
    );
}

#[test]
fn a_comparison_the_types_would_change_scans() {
    let dir = fixture("loose");
    // a number against a TEXT column: the scan reads every
    // numeric-looking cell as its number; SQLite's `code = 150`
    // would keep '150' alone
    let (out, err) = qua(&dir, "/authors/*[::code = 150]::name");
    assert_eq!(out, "Homer\nAusten\nTwain");
    assert!(
        err.contains("pushdown: not used") && err.contains("code"),
        "{err}"
    );
    assert!(err.contains("partial pushdown: not used"), "{err}");
    // an untyped column holding 300, '300' and 300.0
    let (out, err) = qua(&dir, "/books/*[::pages = 300] @| count");
    assert_eq!(out, "3");
    assert!(err.contains("pushdown: not used"), "{err}");
    // numeric-looking text against anything
    let (out, err) = qua(&dir, "/authors/*[::code = '150']::name");
    assert_eq!(out, "Homer\nAusten\nTwain");
    assert!(err.contains("pushdown refused"), "{err}");
    // text against a numeric column; the unmet column alone is named
    let (out, err) = qua(&dir, "/authors/*[::born > 1700 && ::code = 150]::name");
    assert_eq!(out, "Austen\nTwain");
    assert!(err.contains("the comparison on code would"), "{err}");
    let (out, err) = qua(&dir, "/authors/*[::born = 'abc'] @| count");
    assert_eq!(out, "0");
    assert!(err.contains("pushdown: not used"), "{err}");
}
