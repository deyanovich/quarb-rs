//! The static plan through the SQLite adapter: `ANALYZE`'s row
//! counts as the estimate, unknown without them; the equality on
//! the child hop claimed by the catalog.

use quarb::{Confidence, Latency};
use quarb_sqlite::SqliteAdapter;
use rusqlite::Connection;

fn db(analyzed: bool) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "quarb-estimate-{}-{analyzed}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT, author TEXT);
         INSERT INTO books VALUES (1,'Emma','Austen'), (2,'Persuasion','Austen'), (3,'The Iliad','Homer');
         CREATE INDEX books_author ON books (author);
         CREATE INDEX books_title ON books (title);",
    )
    .unwrap();
    if analyzed {
        conn.execute_batch("ANALYZE;").unwrap();
    }
    path
}

#[test]
fn analyzed_statistics_estimate_the_table_and_the_catalog_claims_the_equality() {
    let a = SqliteAdapter::open(&db(true)).unwrap();
    let plan = quarb::Query::parse("/books/*[::author = \"Austen\"] | ::title", &a)
        .unwrap()
        .plan(&a);
    let step = &plan.branches[0].steps[1];
    assert_eq!(step.scope, Some(vec!["books".into()]));
    assert_eq!(step.pushed, Some(vec![true]));
    // The author index's statistics: three rows over two values,
    // two per value as SQLite rounds.
    let e = step.estimate.clone().unwrap();
    assert_eq!(e.nodes, (0, 2));
    assert_eq!(e.latency, Latency::Local);
    assert_eq!(e.confidence, Confidence::Guess);
    let whole = quarb::Query::parse("/books/*", &a).unwrap().plan(&a);
    assert_eq!(whole.estimate().unwrap().nodes, (3, 3));
}

#[test]
fn without_statistics_the_estimate_is_unknown() {
    let a = SqliteAdapter::open(&db(false)).unwrap();
    let plan = quarb::Query::parse("/books/*", &a).unwrap().plan(&a);
    assert_eq!(plan.branches[0].steps[1].estimate, None);
    assert_eq!(plan.estimate(), None);
}
