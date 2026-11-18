//! The pushdown scanning round (2026-11-06): a prefix after a plain
//! step over a relational mount reads k rows, directly and through a
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
    // the whole plan takes the cut since 0.69 (the driver writes the
    // key's order and the LIMIT after it); a selection that is no
    // head of the context is still read by the prefix
    assert!(
        err.contains(
            "info PUSHED (query): pushdown: SELECT title FROM books ORDER BY books.id LIMIT 2"
        ),
        "{err}"
    );
    let (out, err) = qua(
        &d,
        &["--diagnostics=info", "/books/* @| [2] | ::title", "lib.db"],
    );
    assert_eq!(out, "Emma");
    assert!(err.contains("info TAKE_NOT_PUSHED (query): "), "{err}");
    assert!(!err.contains("PUSHED (prefix)"), "{err}");
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

/// A regex is never pushed (each database has a REGEXP of its own),
/// but the literals it cannot match without narrow what is fetched,
/// and the engine runs the regex over that. Each query answers as
/// the scan does, over cells chosen to tell the two apart: a NULL,
/// case, a text that reads as a number, a quote, a number column.
#[test]
fn a_regex_narrows_the_fetch_by_its_literals_and_answers_as_the_scan() {
    let dir = std::env::temp_dir().join(format!("quarb-prefix-{}-regex", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("w.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE words (id INTEGER PRIMARY KEY, w TEXT, n INTEGER, r REAL);
         INSERT INTO words VALUES
           (1,'Mars',240,1.5),(2,'mars',210,2.0),(3,'MARS',NULL,NULL),
           (4,NULL,7,0.25),(5,'Marsyas',12,100.0),(6,'It''s Mars',240,1e20),
           (7,'1e2',100,1.0),(8,'100',100,3.5),(9,'na' || char(239) || 've',1,1.0),
           (10,'50%_off',2,2.5),(11,'',3,3.0),(12,'null',4,4.0);",
    )
    .unwrap();
    let narrowed = [
        (
            "/words/*[::w == (/^Ma.*s$/)]::id",
            "instr(w, 'Ma') > 0 AND instr(w, 's') > 0",
        ),
        ("/words/*[::w == (/Mars/)]::id", "instr(w, 'Mars') > 0"),
        (
            "/words/*[::n > 100 && ::w == (/ars/)]::id",
            "(n > 100 AND instr(w, 'ars') > 0)",
        ),
        (
            "/words/*[::w == (/^M/) || ::w == (/^m/)]::id",
            "(instr(w, 'M') > 0 OR instr(w, 'm') > 0)",
        ),
        ("/words/*[::w == (/100/)]::id", "instr(w, '100') > 0"),
        ("/words/*[::w == (/1e2/)]::id", "instr(w, '1e2') > 0"),
        ("/words/*[::w == (/50%_/)]::id", "instr(w, '50%_') > 0"),
        ("/words/*[::w == (/null/)]::id", "instr(w, 'null') > 0"),
        (
            "/words/*[::w == (/na.ve/)]::w",
            "instr(w, 'na') > 0 AND instr(w, 've') > 0",
        ),
        (
            "/words/*[::w == (/(ars)+y/)] | %(::id; ::w)",
            "instr(w, 'ars') > 0 AND instr(w, 'y') > 0",
        ),
    ];
    for (q, filter) in narrowed {
        let (out, err) = qua(&dir, &["--diagnostics=info", q, "w.db"]);
        let (scan, _) = qua(&dir, &["--no-pushdown", q, "w.db"]);
        assert_eq!(out, scan, "{q}");
        assert!(
            err.contains(&format!("partial pushdown: WHERE {filter} on words")),
            "{q}: {err}"
        );
    }
    // not narrowed, and the same answer: no literal, a negation, a
    // quote, a number column (the database would search its own
    // rendering of a number)
    for q in [
        "/words/*[::w == (/(?i)mars/)]::id",
        "/words/*[::w == (/Mars|mars/)]::id",
        "/words/*[::w !== (/Mars/)]::id",
        "/words/*[!(::w == (/Mars/))]::id",
        "/words/*[::w == (/t's/)]::id",
        "/words/*[::w == (/^$/)]::id",
        "/words/*[::n == (/^2/)]::id",
        "/words/*[::r == (/e/)]::id",
        "/words/*[::r == (/^1/)]::id",
    ] {
        let (out, err) = qua(&dir, &["--diagnostics=info", q, "w.db"]);
        let (scan, _) = qua(&dir, &["--no-pushdown", q, "w.db"]);
        assert_eq!(out, scan, "{q}");
        assert!(!err.contains("partial pushdown"), "{q}: {err}");
    }
}
