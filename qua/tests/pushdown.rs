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

/// stdout, and what `--diagnostics=info` said on stderr.
fn qua(dir: &Path, query: &str) -> (String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(["--diagnostics=info", query, "lib.db"])
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
    assert!(
        err.contains("the comparison or reducer on code would"),
        "{err}"
    );
    let (out, err) = qua(&dir, "/authors/*[::born = 'abc'] @| count");
    assert_eq!(out, "0");
    assert!(err.contains("pushdown: not used"), "{err}");
    // a numeric reducer over a text column: SQL would sum the
    // leading digits and order by collation; the scan reads values
    let (_, err) = qua(&dir, "/authors/* | ::name @| max");
    assert!(
        err.contains("the comparison or reducer on name would"),
        "{err}"
    );
    let (out, err) = qua(&dir, "/authors/* | ::born @| sum");
    assert!(err.contains("pushdown: SELECT SUM(born)"), "{err}");
    assert!(!out.is_empty());
}

/// A JSON path under a column pushes down only as the scan grafts:
/// a TEXT column holding JSON text is a leaf on both roads (no
/// rows), a column declared JSON grafts on both, and `<json>` on
/// the step declares the graft on both whatever the column's type.
#[test]
fn a_json_path_pushes_only_as_declared() {
    let dir = fixture("json-declared");
    let db = rusqlite::Connection::open(dir.join("lib.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE jrows (id INTEGER PRIMARY KEY, j TEXT);
         INSERT INTO jrows VALUES
           (1, '{\"tag\":\"gold\"}'), (2, '{\"tag\":\"lead\"}'), (3, '{\"tag\":\"gold\"}');
         CREATE TABLE t (id INTEGER PRIMARY KEY, doc JSON);
         INSERT INTO t VALUES (1, '{\"tag\":\"gold\"}'), (2, '{\"tag\":\"lead\"}');",
    )
    .unwrap();
    let scan = |q: &str| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
            .env("QUARB_LOCALE", "C")
            .current_dir(&dir)
            .args(["--no-pushdown", q, "lib.db"])
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    // TEXT, undeclared: the leaf is a leaf on both roads.
    let q = "/jrows/*[/j/tag:: = \"gold\"]::id";
    let (out, err) = qua(&dir, q);
    assert_eq!(out, "");
    assert_eq!(scan(q), "");
    assert!(
        !err.contains("json_extract") || err.contains("scann"),
        "{err}"
    );
    // Declared JSON: both roads graft and agree.
    let q = "/t/*[/doc/tag:: = \"gold\"]::id";
    let (out, err) = qua(&dir, q);
    assert_eq!(out, "1");
    assert_eq!(scan(q), "1");
    assert!(err.contains("json_extract"), "{err}");
    // `<json>` on the step declares the graft on both roads.
    let q = "/jrows/*[/j<json>/tag:: = \"gold\"]::id";
    let (out, _) = qua(&dir, q);
    assert_eq!(out, "1\n3");
    assert_eq!(scan(q), "1\n3");
}

/// The property form of a JSON path, `[/col/a::b OP v]`, answers
/// pushed as it answers scanned, over every shape a value takes. It
/// is not the child form's question: an array of scalars is a list
/// (any item matches), and `!=` keeps a row whose key is absent.
/// And the child form's whole push no longer takes an array or an
/// object for the text literal that spells it.
#[test]
fn a_json_property_pushes_as_the_scan_answers() {
    let dir = fixture("json-property");
    let db = rusqlite::Connection::open(dir.join("lib.db")).unwrap();
    db.execute_batch(
        r#"CREATE TABLE orders (id INTEGER PRIMARY KEY, data JSON);
           INSERT INTO orders VALUES
             (1, '{"a":[1,2]}'), (2, '{"a":"[1,2]"}'), (3, '{"a":"x"}'),
             (4, '{"a":{"k":1}}'), (5, '{"a":"{\"k\":1}"}'), (6, '{"a":["x","y"]}'),
             (7, '{"a":["x",{"k":1}]}'), (8, '{}'), (9, '{"a":null}'), (10, '{"a":[]}'),
             (11, '{"a":7}'), (12, '{"a":"7"}'), (13, '{"a":true}'), (14, '{"a":["7"]}'),
             (15, '{"b":{"a":"x"}}'), (16, '{"a":[null,"x"]}'), (17, 'not json'),
             (18, '["x"]');
           CREATE TABLE named (id INTEGER PRIMARY KEY, value JSON);
           INSERT INTO named VALUES (1, '{"a":"x"}'), (2, '{"a":["x"]}'), (3, '{"a":"y"}');
           CREATE TABLE plain (id INTEGER PRIMARY KEY, j TEXT);
           INSERT INTO plain VALUES (1, '{"a":"x"}'), (2, '{"a":["x"]}'), (3, '{"a":"y"}');"#,
    )
    .unwrap();
    let scan = |q: &str| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
            .env("QUARB_LOCALE", "C")
            .current_dir(&dir)
            .args(["--no-pushdown", q, "lib.db"])
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        String::from_utf8(out.stdout)
            .unwrap()
            .trim()
            .replace('\n', " ")
    };
    let pushed = |q: &str| {
        let (out, err) = qua(&dir, q);
        (out.replace('\n', " "), err)
    };
    // what each answers, and that the two roads agree
    for (q, answer) in [
        ("/orders/*[/data::a = 'x']::id", "3 6 16"),
        ("/orders/*['x' = /data::a]::id", "3 6 16"),
        ("/orders/*[/data::a = '[1,2]']::id", "2"),
        (
            "/orders/*[/data::a != 'x']::id",
            "1 2 4 5 6 7 8 9 11 12 13 14 15 16 17 18",
        ),
        ("/orders/*[/data::a > 6]::id", "11 12 14"),
        ("/orders/*[/data::a = 7]::id", "11 12 14"),
        ("/orders/*[/data::a = '7']::id", "11 12 14"),
        ("/orders/*[/data::a < 'y']::id", "2 3 6 12 14 16"),
        ("/orders/*[/data::a]::id", "1 2 3 5 6 11 12 13 14 16"),
        ("/orders/*[/data/b::a = 'x']::id", "15"),
        ("/orders/*[/data/b::a != 'q']::id", "15"),
        ("/orders/*[/data::a = 'x'] @| count", "3"),
        // the child form, beside it: an array has no value
        ("/orders/*[/data/a:: = 'x']::id", "3"),
        ("/orders/*[/data/a:: = '[1,2]']::id", "2"),
        ("/orders/*[/data/a:: = '{\"k\":1}']::id", "5"),
        // a column of one of json_each's own names
        ("/named/*[/value::a = 'x']::id", "1 2"),
        // a TEXT column is a leaf on both roads, and a graft where
        // the step declares it
        ("/plain/*[/j::a = 'x']::id", ""),
        ("/plain/*[/j<json>::a = 'x']::id", "1 2"),
    ] {
        let (out, _) = pushed(q);
        assert_eq!(out, answer, "{q}");
        assert_eq!(scan(q), answer, "{q} (scanned)");
    }
    // the string equality is pushed whole, its array branch with it
    let (_, err) = pushed("/orders/*[/data::a = 'x']::id");
    assert!(
        err.contains("PUSHED (query)") && err.contains("json_each(data, '$.a')"),
        "{err}"
    );
    // a comparison narrows, the arrays let through
    let (_, err) = pushed("/orders/*[/data::a > 6]::id");
    assert!(
        err.contains("PUSHED (prefilter)") && err.contains("IN ('text', 'array')"),
        "{err}"
    );
    // the container-looking literal narrows too; it was pushed whole
    let (_, err) = pushed("/orders/*[/data/a:: = '[1,2]']::id");
    assert!(err.contains("PUSHED (prefilter)"), "{err}");
}
