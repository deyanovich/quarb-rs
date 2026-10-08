//! A table without a single-column primary key names its rows by
//! position, and a filtered listing's positions are not the
//! table's: before the decline, a `WITHOUT ROWID` table read under
//! a pushed-down filter and then whole met the wrong rows under the
//! same keys. Now such a table is read whole (the engine re-applies
//! the predicate), and the rows stay distinct.

use quarb::{AstAdapter, QueryResult};
use quarb_sqlite::SqliteAdapter;
use rusqlite::Connection;

fn values<A: AstAdapter>(a: &A, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| a.name(*n).unwrap_or_default()).collect(),
    }
}

#[test]
fn a_filtered_listing_of_a_pkless_table_keeps_the_rows_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pairs.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE pair(a INTEGER NOT NULL, b TEXT NOT NULL, PRIMARY KEY(a, b)) WITHOUT ROWID;
         INSERT INTO pair VALUES (1, 'one'), (2, 'two'), (3, 'three'), (4, 'four');
         CREATE TABLE \"odd\"\"name\"(id INTEGER PRIMARY KEY, \"co\"\"l\" TEXT);
         INSERT INTO \"odd\"\"name\" VALUES (1, 'quoted');",
    )
    .unwrap();
    drop(conn);
    let filtered = SqliteAdapter::open_filtered(&path, "pair", "a > 2").unwrap();
    // The filtered question answers as the whole-table one does…
    assert_eq!(values(&filtered, "/pair/*[::a > 2]::b"), ["three", "four"]);
    // …and a whole read afterwards meets four distinct rows, not
    // the two filtered ones twice under positional keys.
    assert_eq!(values(&filtered, "/pair/* @| count"), ["4"]);
    assert_eq!(values(&filtered, "/pair/*::b @| unique @| count"), ["4"]);
    assert_eq!(
        values(&filtered, "/pair/*::b"),
        ["one", "two", "three", "four"]
    );
    // A name with a quote inside is quoted, not interpolated.
    let whole = SqliteAdapter::open(&path).unwrap();
    assert_eq!(values(&whole, r#"/'odd"name'/*::'co"l'"#), ["quoted"]);
}
