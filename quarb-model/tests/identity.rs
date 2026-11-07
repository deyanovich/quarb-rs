//! Identity is intrinsic (ruling 2026-11-06): a role's node answers
//! the source's `:id`, `:name` and `:path`, `collapse` sees one node
//! for the two, and the role stays visible to navigation alone.

use quarb_model::{ModelAdapter, parse_model};
use quarb_sqlite::SqliteAdapter;
use rusqlite::Connection;

fn model(text: &str) -> ModelAdapter<SqliteAdapter> {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT, author TEXT);
         CREATE TABLE authors (id TEXT PRIMARY KEY, name TEXT);
         INSERT INTO authors VALUES ('homer','Homer'),('austen','Jane Austen');
         INSERT INTO books VALUES (1,'The Iliad','homer'),(2,'The Odyssey','homer'),(3,'Emma','austen');",
    )
    .unwrap();
    let base = SqliteAdapter::load(&conn).unwrap();
    ModelAdapter::new(base, parse_model(text).unwrap())
}

fn values(a: &ModelAdapter<SqliteAdapter>, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap_or_else(|e| panic!("{q}: {e}")) {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => vec![ns.len().to_string()],
    }
}

const MODEL: &str =
    "node /writers/author: /authors/*;\nref /books/*::author --> /writers/author[::id = $];\n";

#[test]
fn a_roles_node_answers_the_sources_identity() {
    let a = model(MODEL);
    let source = values(
        &a,
        "/authors/homer | %(id = :id; name = :name; path = :path)",
    );
    let role = values(
        &a,
        "/writers/author[::id = \"homer\"] | %(id = :id; name = :name; path = :path)",
    );
    assert_eq!(role, source);
    // the role stays visible to navigation alone
    assert_eq!(
        values(&a, "/writers/author[::id = \"homer\"] | \\ | :name"),
        ["writers"]
    );
    assert_eq!(values(&a, "/authors/homer | \\ | :name"), ["authors"]);
    assert_eq!(
        values(&a, "/books/1 | ->author | :name"),
        values(&a, "/authors/homer | :name")
    );
}

#[test]
fn collapse_sees_one_node_for_a_role_and_its_source() {
    let a = model(MODEL);
    // three books, two by homer: the hop lands on homer's role node
    // twice and austen's once — collapse keeps two
    assert_eq!(
        values(&a, "/books/* | ->author @| collapse @| count"),
        ["2"]
    );
    assert_eq!(values(&a, "/books/* | ->author @| unique @| count"), ["2"]);
}
