//! The pushdown scanning round (2026-11-06): a child hop that keeps
//! a prefix reads k rows (`SELECT … ORDER BY rowid LIMIT k`), a
//! count on a table not read is the server's `count(*)`, and the
//! rows a prefix read are the rows the whole read finds again.

use quarb::{NodeId, QueryResult};
use quarb_sqlite::SqliteAdapter;
use rusqlite::Connection;

const SCHEMA: &str = r#"
    CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT, pages INTEGER);
    INSERT INTO books VALUES (1,'Iliad',300),(2,'Emma',400),(3,'Odyssey',350),(4,'Persuasion',250);
    CREATE TABLE notes (n TEXT, about INTEGER);
    INSERT INTO notes VALUES ('x',1),('y',2),('z',3);
"#;

fn file_db(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("quarb-prefix-{}-{tag}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    path
}

fn values(a: &SqliteAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.into_iter().map(|n| a.locator(n)).collect(),
    }
}

fn nodes(a: &SqliteAdapter, q: &str) -> Vec<NodeId> {
    match quarb::run(q, a).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryResult::Nodes(ns) => ns,
        other => panic!("{q}: not nodes: {other:?}"),
    }
}

#[test]
fn a_prefix_hop_reads_k_rows_and_the_whole_read_finds_them() {
    let a = SqliteAdapter::open(&file_db("prefix")).unwrap();
    assert_eq!(values(&a, "/books/* @| [..2] | ::title"), ["Iliad", "Emma"]);
    assert_eq!(values(&a, "/books:::n-loaded"), ["2"]);
    assert_eq!(values(&a, "/books:::loaded"), ["false"]);
    // a longer prefix reads again, a shorter one answers from the cache
    assert_eq!(
        values(&a, "/books/* @| [..3] | ::title"),
        ["Iliad", "Emma", "Odyssey"]
    );
    assert_eq!(values(&a, "/books:::n-loaded"), ["3"]);
    assert_eq!(values(&a, "/books/* @| first(1) | ::title"), ["Iliad"]);
    assert_eq!(values(&a, "/books/* @| [2] | ::title"), ["Emma"]);
    assert_eq!(values(&a, "/books:::n-loaded"), ["3"]);
    // the whole read finds the prefix's rows again: the same nodes
    let first = nodes(&a, "/books/* @| [..2]");
    let all = nodes(&a, "/books/*");
    assert_eq!(all.len(), 4);
    assert_eq!(&all[..2], &first[..]);
    assert_eq!(values(&a, "/books:::loaded"), ["true"]);
    assert_eq!(values(&a, "/books:::n-loaded"), ["4"]);
}

#[test]
fn a_table_without_a_key_takes_the_prefix_by_rowid() {
    let a = SqliteAdapter::open(&file_db("rowid")).unwrap();
    assert_eq!(values(&a, "/notes/* @| [..2] | ::n"), ["x", "y"]);
    assert_eq!(values(&a, "/notes:::n-loaded"), ["2"]);
    assert_eq!(values(&a, "/notes/* | ::n"), ["x", "y", "z"]);
}

#[test]
fn a_count_on_a_table_not_read_is_the_servers() {
    let a = SqliteAdapter::open(&file_db("count")).unwrap();
    assert_eq!(values(&a, "/books/* @| count"), ["4"]);
    assert_eq!(values(&a, "/notes/* @| count"), ["3"]);
    assert_eq!(values(&a, "/books:::loaded"), ["false"]);
    assert_eq!(values(&a, "/books:::n-loaded"), ["0"]);
}

#[test]
fn a_prefix_after_a_keyed_hop_keeps_the_row() {
    let a = SqliteAdapter::open(&file_db("keyed")).unwrap();
    let emma = nodes(&a, "/books/2");
    assert_eq!(values(&a, "/books:::n-loaded"), ["1"]);
    let first = nodes(&a, "/books/* @| [..3]");
    assert_eq!(first.len(), 3);
    assert_eq!(first[1], emma[0]);
    assert_eq!(values(&a, "/books:::n-loaded"), ["3"]);
}
