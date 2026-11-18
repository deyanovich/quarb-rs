//! A plan answers as the scan does where it compares and computes
//! (0.71). The engine goes by values: two texts that read as numbers
//! compare as numbers, a text that reads as none computes to null,
//! `div` is the true division, and two texts compare by their
//! characters. SQL goes by the column's type and collation. So a
//! plan that compares two columns or computes with one assumes
//! numeric columns, which the catalog bears out or not; `div` is
//! written as a real division by a number that is not zero; and a
//! text comparison is the plan's only where the database compares
//! the column exactly. Until 0.71 these were pushed unchecked, and
//! over text columns a plan answered other rows than the scan.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-typed-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("t.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT, b TEXT, n INTEGER, m INTEGER, r REAL);
         INSERT INTO t VALUES
           (1,'1.0','1',1,1,1.0), (2,'10','9',10,9,9.5), (3,'abc','abd',3,4,3.0),
           (4,'300','300.0',300,300,300.0), (5,'x',NULL,5,NULL,NULL),
           (6,NULL,NULL,NULL,NULL,NULL), (7,'7','abc',7,8,7.0), (8,'B','a',2,2,2.5),
           (9,'12','12',12,13,12.0);
         CREATE TABLE u (id INTEGER PRIMARY KEY, code TEXT, t_n INTEGER);
         INSERT INTO u VALUES (1,'1',1), (2,'9.0',10), (3,'abc',3), (4,'300',300), (5,NULL,7);
         CREATE TABLE folded (id INTEGER PRIMARY KEY, name TEXT COLLATE NOCASE, other TEXT);
         INSERT INTO folded VALUES (1,'Holst','Holst'), (2,'holst','holst'), (3,'HOLST ','x');
         CREATE TABLE plain (id INTEGER PRIMARY KEY, name TEXT);
         INSERT INTO plain VALUES (1,'Holst'), (2,'holst'), (3,'HOLST ');",
    )
    .unwrap();
    dir
}

/// Whether the run succeeded, stdout, and the info lines of stderr.
fn run(dir: &Path, args: &[&str]) -> (bool, String, Vec<String>) {
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
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .map(str::to_string)
            .collect(),
    )
}

/// The query's answer with the planner on, held to its answer with
/// the planner off; the planner's lines are returned.
fn same(dir: &Path, q: &str) -> Vec<String> {
    let (ok, plan, lines) = run(dir, &["--diagnostics=info", q, "t.db"]);
    assert!(ok, "{q}: {lines:?}");
    let (ok, scan, _) = run(dir, &["--no-pushdown", q, "t.db"]);
    assert!(ok, "{q}");
    assert_eq!(plan, scan, "{q}");
    lines
}

#[test]
fn two_columns_compared_answer_as_the_scan() {
    let dir = fixture("columns");
    // over text columns the engine's comparison is no SQL comparison
    for q in [
        "/t/*[::a = ::b]::id",
        "/t/*[::a != ::b]::id",
        "/t/*[::a < ::b]::id",
        "/t/*[::a > ::b]::id",
        "/t/*[::a <= ::b]::id",
        // a text column against a numeric one
        "/t/*[::a = ::n]::id",
        "/t/*[::a > ::n]::id",
        "/t/*[::b >= ::n]::id",
        // a join by a text key, and by a text key against a number
        "/t/* <=>* /u/*[::code = $$0::a] | %(t = ::id; u = $$1::id)",
        "/t/* <=>* /u/*[::code = $$0::n] | %(t = ::id; u = $$1::id)",
    ] {
        let lines = same(&dir, q);
        assert!(
            lines
                .first()
                .is_some_and(|l| l.starts_with("info TYPE_NOT_PUSHED (query): the comparison")),
            "{q}: {lines:?}"
        );
    }
    // told for the whole plan and for the partial one, which is
    // held to the same catalog
    assert_eq!(
        same(&dir, "/t/*[::a = ::b]::id"),
        [
            "info TYPE_NOT_PUSHED (query): the comparison on a, b would follow the \
             column's type where the scan follows the value; scanning",
            "info TYPE_NOT_PUSHED (prefilter): the comparison on a, b would follow the \
             column's type where the scan follows the value; scanning"
        ]
    );
    // two numeric columns are the plan's, as before
    for (q, sql) in [
        (
            "/t/*[::n = ::m]::id",
            "SELECT id FROM t WHERE (n = m OR (n IS NULL AND m IS NULL)) ORDER BY t.id",
        ),
        (
            "/t/*[::n < ::r]::id",
            "SELECT id FROM t WHERE n < r ORDER BY t.id",
        ),
        (
            "/t/* <=>* /u/*[::t_n = $$0::n] | %(t = ::id; u = $$1::id)",
            "SELECT t.id AS t, u.id AS u FROM t JOIN u ON u.t_n = t.n ORDER BY t.id, u.id",
        ),
    ] {
        assert_eq!(
            same(&dir, q),
            [format!("info PUSHED (query): pushdown: {sql}")],
            "{q}"
        );
    }
    // a condition the partial plan would send ahead is held to the
    // same: two text columns are not compared in a prefilter either
    let lines = same(&dir, "/t/*[::a < ::b] @| [2] | ::id");
    assert!(
        !lines.iter().any(|l| l.contains("partial pushdown")),
        "{lines:?}"
    );
}

#[test]
fn arithmetic_answers_as_the_scan() {
    let dir = fixture("arith");
    // a text column computed with: 'abc' is no number to the engine
    for q in [
        "/t/*[::a + 1 = 1]::id",
        "/t/* | ::a + 1",
        "/t/* | %(::id; x = ::a * 2)",
    ] {
        let lines = same(&dir, q);
        assert!(
            lines
                .first()
                .is_some_and(|l| l.starts_with("info TYPE_NOT_PUSHED (query): ")),
            "{q}: {lines:?}"
        );
    }
    // numeric columns
    for q in [
        "/t/*[::n + 1 > 5]::id",
        "/t/*[::n * ::r > 100]::id",
        "/t/*[::n - ::m = 0]::id",
        "/t/* | ::n * 2",
    ] {
        let lines = same(&dir, q);
        assert!(
            lines.len() == 1 && lines[0].starts_with("info PUSHED (query): pushdown: SELECT "),
            "{q}: {lines:?}"
        );
    }
    // `div` is the true division: 10 div 4 is 2.5 (the plan answered 2)
    assert_eq!(
        same(&dir, "/t/* | ::n div 4"),
        ["info PUSHED (query): pushdown: SELECT (CAST(n AS REAL) / 4) FROM t ORDER BY t.id"]
    );
    assert_eq!(
        run(&dir, &["/t/*[::id < 3] | ::n div 4", "t.db"]).1,
        "0.25\n2.5"
    );
    same(&dir, "/t/*[::n div 2 = 0.5]::id");
    same(&dir, "/t/* | %(::id; x = ::r div 3)");
    // by a column, or by zero, it stays with the engine (inf, nan)
    for q in [
        "/t/* | ::n div ::m",
        "/t/* | ::n div 0",
        "/t/*[::n div ::m > 1.1]::id",
    ] {
        let lines = same(&dir, q);
        assert!(
            lines
                .first()
                .is_some_and(|l| l.starts_with("info OPERATOR_NOT_PUSHED (query): 'div' by")),
            "{q}: {lines:?}"
        );
    }
    // `mod` keeps a fraction and stays with the engine
    for q in ["/t/*[::n mod 3 = 1]::id", "/t/*[::r mod 3 = 0.5]::id"] {
        let lines = same(&dir, q);
        assert!(
            lines
                .first()
                .is_some_and(|l| l.starts_with("info OPERATOR_NOT_PUSHED (query): 'mod' ")),
            "{q}: {lines:?}"
        );
    }
}

/// SQLite compares text by its bytes unless the column is declared
/// under a collation; the catalog reports a table that names one,
/// and a text comparison over it stays with the engine.
#[test]
fn a_text_comparison_under_a_collation_stays_with_the_engine() {
    let dir = fixture("collation");
    assert_eq!(
        run(&dir, &["/folded/*[::name = \"holst\"]::id", "t.db"]).1,
        "2"
    );
    for q in [
        "/folded/*[::name = \"holst\"]::id",
        "/folded/*[::name != \"holst\"]::id",
        "/folded/*[::name < \"a\"]::id",
        "/folded/*[::other = \"holst\"]::id",
    ] {
        let lines = same(&dir, q);
        assert!(
            lines
                .first()
                .is_some_and(|l| l.starts_with("info TYPE_NOT_PUSHED (query): ")),
            "{q}: {lines:?}"
        );
    }
    // a table with no collation is compared by bytes, and pushed
    assert_eq!(
        same(&dir, "/plain/*[::name = \"holst\"]::id"),
        [
            "info PUSHED (query): pushdown: SELECT id FROM plain WHERE name = 'holst' ORDER BY \
          plain.id"
        ]
    );
    same(&dir, "/plain/*[::name < \"a\"]::id");
    same(&dir, "/plain/*[::name != \"Holst\"]::id");
}

/// The careful sum (ruling 2026-11-18): the engine adds floats by the
/// compensated sum SQLite's SUM and AVG use, step for step, so a
/// float sum is the same number whether the engine adds it or
/// SQLite adds it for a plan, to the last digit. Until 0.71 the
/// engine kept a plain running total and a pushed float sum could
/// differ from the scan's from the 13th digit on.
#[test]
fn a_float_sum_is_sqlites_to_the_last_digit() {
    let dir = fixture("careful");
    let db = rusqlite::Connection::open(dir.join("t.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE prices (id INTEGER PRIMARY KEY, g INTEGER, x REAL, wide REAL, mixed NUMERIC);
         CREATE INDEX prices_g ON prices(g);
         CREATE INDEX prices_x ON prices(x);",
    )
    .unwrap();
    // a fixed generator: prices of two decimals, values of very
    // different sizes and both signs, and a column that holds whole
    // numbers and floats side by side
    let mut s: u64 = 88172645463325252;
    let mut next = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    db.execute_batch("BEGIN").unwrap();
    for id in 1..=6000 {
        let x = (200 + next() % 39_801) as f64 / 100.0;
        let wide = (next() % 2_000_001) as f64 / 1e3
            * 10f64.powi((next() % 31) as i32 - 15)
            * if next() % 2 == 0 { 1.0 } else { -1.0 };
        let mixed: rusqlite::types::Value = if next() % 3 == 0 {
            rusqlite::types::Value::Integer((next() % 100_000) as i64)
        } else {
            rusqlite::types::Value::Real((next() % 100_000) as f64 / 7.0)
        };
        db.execute(
            "INSERT INTO prices VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, (next() % 5) as i64, x, wide, mixed],
        )
        .unwrap();
    }
    db.execute_batch("COMMIT").unwrap();
    drop(db);
    for q in [
        "/prices/*::x @| sum",
        "/prices/*::x @| mean",
        "/prices/*::wide @| sum",
        "/prices/*::wide @| mean",
        "/prices/*::mixed @| sum",
        "/prices/*::mixed @| mean",
        // SQLite answers these through an index, in another order
        "/prices/*[::g = 3]::x @| sum",
        "/prices/*[::x > 100]::x @| sum",
        "/prices/*[::g = 1]::wide @| mean",
    ] {
        let lines = same(&dir, q);
        assert!(
            lines.len() == 1 && lines[0].starts_with("info PUSHED (query): pushdown: SELECT "),
            "{q}: {lines:?}"
        );
    }
    // and the sum a plain total gets wrong in its last digits
    let db = rusqlite::Connection::open(dir.join("t.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE tenths (id INTEGER PRIMARY KEY, x REAL);
         INSERT INTO tenths (x) VALUES (0.1),(0.1),(0.1),(0.1),(0.1),(0.1),(0.1),(0.1),(0.1),(0.1);",
    )
    .unwrap();
    drop(db);
    same(&dir, "/tenths/*::x @| sum");
    assert_eq!(
        run(&dir, &["--no-pushdown", "/tenths/*::x @| sum", "t.db"]).1,
        "1.0"
    );
}
