//! The pushdown scanning round (2026-11-06): a prefix after a plain
//! hop over a relational mount reads k rows, directly and through a
//! model's `mount`; the diagnostics print what they printed.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-prefix-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("lib.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT, pages INTEGER);
         INSERT INTO books VALUES (1,'Iliad',300),(2,'Emma',400),(3,'Odyssey',350),(4,'Persuasion',250);
         CREATE TABLE notes (n TEXT, about INTEGER);
         INSERT INTO notes VALUES ('x',1),('y',2),('z',3);",
    )
    .unwrap();
    std::fs::write(
        dir.join("lib.model"),
        format!("mount lib: {};\n", dir.join("lib.db").display()),
    )
    .unwrap();
    dir
}

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
fn a_prefix_over_a_relational_mount_answers_the_whole_reads_first_rows() {
    let d = fixture("direct");
    let (out, err) = qua(
        &d,
        &[
            "--diagnostics=info",
            "/books/* @| [..2] | ::title",
            "lib.db",
        ],
    );
    assert_eq!(out, "Iliad\nEmma");
    // the whole-query translation still declines the cut: the same
    // info lines as before the round, nothing new printed
    assert!(err.contains("NOT_PUSHED (query)"), "{err}");
    assert!(!err.contains("PUSHED (prefix)"), "{err}");
    assert_eq!(qua(&d, &["/books/* @| [2] | ::title", "lib.db"]).0, "Emma");
    assert_eq!(
        qua(&d, &["/books/* @| first(3) | ::title", "lib.db"]).0,
        "Iliad\nEmma\nOdyssey"
    );
    assert_eq!(qua(&d, &["/notes/* @| [..2] | ::n", "lib.db"]).0, "x\ny");
    assert_eq!(qua(&d, &["/books/* @| count", "lib.db"]).0, "4");
    // the prefix read k rows, not the table
    assert_eq!(
        qua(
            &d,
            &[
                "/books/* @| [..2] @| count | %(n = $_; loaded = (^/books:::\"n-loaded\"))",
                "lib.db"
            ]
        )
        .0,
        "%(n = 2; loaded = 2)"
    );
}

#[test]
fn a_prefix_through_a_models_mount_reads_the_same() {
    let d = fixture("model");
    assert_eq!(
        qua(
            &d,
            &["--model", "lib.model", "/lib/books/* @| [..2] | ::title"]
        )
        .0,
        "Iliad\nEmma"
    );
    assert_eq!(
        qua(&d, &["--model", "lib.model", "/lib/books/* @| count"]).0,
        "4"
    );
    // the prefix read k rows, not the table
    assert_eq!(
        qua(
            &d,
            &[
                "--model",
                "lib.model",
                "/lib/books/* @| [..2] @| count | %(n = $_; loaded = (^/lib/books:::\"n-loaded\"))"
            ]
        )
        .0,
        "%(n = 2; loaded = 2)"
    );
}
