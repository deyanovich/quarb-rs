//! The pushdown scanning round (2026-11-06): under a model, a count
//! and a prefix on a source's own table are the source's to answer
//! — nothing loaded for the count, k rows for the prefix.

use quarb_model::{ModelAdapter, parse_model};
use quarb_sqlite::SqliteAdapter;
use rusqlite::Connection;

fn lazy(tag: &str, model: &str) -> ModelAdapter<SqliteAdapter> {
    let path = std::env::temp_dir().join(format!(
        "quarb-model-pushdown-{}-{tag}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT, pages INTEGER);
         INSERT INTO books VALUES (1,'Iliad',300),(2,'Emma',400),(3,'Odyssey',350),(4,'Persuasion',250);",
    )
    .unwrap();
    let base = SqliteAdapter::open(&path).unwrap();
    ModelAdapter::new(base, parse_model(model).unwrap())
}

fn values(a: &ModelAdapter<SqliteAdapter>, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap_or_else(|e| panic!("{q}: {e}")) {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => vec![ns.len().to_string()],
    }
}

#[test]
fn a_count_and_a_prefix_under_a_model_are_the_sources() {
    let a = lazy("plain", "");
    assert_eq!(values(&a, "/books/* @| count"), ["4"]);
    assert_eq!(values(&a, "/books:::\"n-loaded\""), ["0"]);
    assert_eq!(values(&a, "/books/* @| [..2] | ::title"), ["Iliad", "Emma"]);
    assert_eq!(values(&a, "/books:::\"n-loaded\""), ["2"]);
    assert_eq!(
        values(&a, "/books/* | ::title"),
        ["Iliad", "Emma", "Odyssey", "Persuasion"]
    );
}
