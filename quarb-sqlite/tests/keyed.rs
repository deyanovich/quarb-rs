//! Hops read by key: a lazily opened database answers `-->`, `->`,
//! `<-`, `<--` and `/table/KEY` from the rows they land on, never
//! from a whole table it was not asked to list — and answers
//! exactly as the eagerly loaded one does.

use quarb::QueryResult;
use quarb_sqlite::SqliteAdapter;
use rusqlite::Connection;

// Keys are declared but not enforced: the fixture holds a book whose
// author does not exist, and a SQLite built with enforcement on by
// default would refuse to store it.
const SCHEMA: &str = r#"
    PRAGMA foreign_keys = OFF;
    CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT, code TEXT UNIQUE);
    CREATE TABLE books (
        id INTEGER PRIMARY KEY, title TEXT,
        author_id INTEGER REFERENCES authors(id),
        editor_id INTEGER REFERENCES authors(id),
        code TEXT REFERENCES authors(code));
    CREATE TABLE staff (id INTEGER PRIMARY KEY, name TEXT,
        boss INTEGER REFERENCES staff(id));
    CREATE TABLE notes (n TEXT, about INTEGER);
    INSERT INTO authors VALUES (1,'Austen','A'), (2,'Twain','T'), (3,'Homer','H'), (4,'Gibbon','G');
    INSERT INTO books VALUES
        (1,'Emma',1,2,'A'), (2,'Tom Sawyer',2,NULL,'T'), (3,'The Iliad',3,1,'H'),
        (4,'Persuasion',1,1,'A'), (5,'Huckleberry Finn',2,3,'T'), (6,'Anonymous',NULL,NULL,NULL),
        (7,'Orphan',99,NULL,'Z');
    INSERT INTO staff VALUES (1,'ada',NULL), (2,'bo',1), (3,'cy',1), (4,'di',2), (5,'ed',2), (6,'flo',4);
    INSERT INTO notes VALUES ('x', 1), ('y', 2);
"#;

fn file_db(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("quarb-keyed-{}-{tag}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    path
}

fn eager() -> SqliteAdapter {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    SqliteAdapter::load(&conn).unwrap()
}

fn show(a: &SqliteAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.into_iter().map(|n| a.locator(n)).collect(),
    }
}

const QUERIES: &[&str] = &[
    "/books/3::author_id-->::name",
    "/books/3->author",
    "/books/3->*",
    "/books/5::code-->::name",
    "/books/5::code-->authors",
    "/books/7::author_id-->",
    "/books/6::author_id-->",
    "/books/99::title",
    "/authors/1<-author",
    "/authors/1<-*",
    "/authors/1<--(::author_id)::title",
    "/authors/1<--(::editor_id)::title",
    "/authors/1<--::title",
    "/authors/1<--(/books/*::author_id)::title",
    "/authors/2<--(/books/*)::title",
    "/authors/*<--(::author_id) @| count",
    "/authors/* | %(::name; n = (<-author @| count))",
    "/books/* | %(::title; a = ::author_id-->::name)",
    "/books/*[::id < 4]->author",
    "/staff/1(<-boss){1;3}",
    "/staff/6(->boss)+",
    "/staff/*[<-boss]::name",
    "/authors/1--*",
    "/notes/*::n",
    "/authors/1<--authors(/notes/*::about)::n",
];

#[test]
fn a_lazy_database_answers_as_the_eager_one() {
    let e = eager();
    let path = file_db("same");
    for q in QUERIES {
        // a fresh adapter per query: nothing read by an earlier one
        let l = SqliteAdapter::open(&path).unwrap();
        assert_eq!(show(&l, q), show(&e, q), "{q}");
    }
    // …and one adapter across all of them, where rows read by key
    // and tables read whole meet in every order.
    let l = SqliteAdapter::open(&path).unwrap();
    for q in QUERIES.iter().chain(QUERIES.iter().rev()) {
        assert_eq!(show(&l, q), show(&e, q), "{q} (shared adapter)");
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_hop_reads_the_rows_it_lands_on() {
    let path = file_db("rows");
    let loaded = |q: &str| {
        let l = SqliteAdapter::open(&path).unwrap();
        show(&l, q)
    };
    // forward: one book, one author; neither table whole
    assert_eq!(
        loaded(
            "/books/3::author_id--> | %(::name; b = (^/books:::loaded); \
             a = (^/authors:::loaded); nb = (^/books:::\"n-loaded\"); \
             na = (^/authors:::\"n-loaded\"))"
        ),
        [r#"%(name = "Homer"; b = false; a = false; nb = 1; na = 1)"#]
    );
    // backward: the author by key, the referring rows by their key
    // columns (the adapter is asked for a node's backlinks under
    // every key into it, so rows referring through another column
    // are read too — but rows, never the table)
    assert_eq!(
        loaded(
            "/authors/1<-author @| count | %(n = $_; b = (^/books:::loaded); \
             few = (^/books:::\"n-loaded\") < 7)"
        ),
        [r#"%(n = 2; b = false; few = true)"#]
    );
    // a scoped reverse arrow over one table's rows goes by key too
    assert_eq!(
        loaded(
            "/authors/2<--(/books/*::author_id) @| count \
             | %(n = $_; b = (^/books:::loaded))"
        ),
        [r#"%(n = 2; b = false)"#]
    );
    // a listing is a listing: the table is read whole
    assert_eq!(
        loaded("/books/* @| count | %(n = $_; b = (^/books:::loaded))"),
        [r#"%(n = 7; b = true)"#]
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_pushed_down_filter_lists_only_and_hops_stay_exact() {
    let e = eager();
    let path = file_db("filter");
    for (q, table, where_sql) in [
        ("/staff/*[::id < 3]<-boss", "staff", "id < 3"),
        ("/staff/*[::id > 3]->boss", "staff", "id > 3"),
        ("/staff/*[::id > 3]::boss-->::name", "staff", "id > 3"),
        ("/staff/*[::id < 3]<-boss<-boss", "staff", "id < 3"),
        ("/books/*[::id < 4]->author", "books", "id < 4"),
        (
            "/staff/*[::id = 2] | %(::name; all = (:::table))",
            "staff",
            "id = 2",
        ),
    ] {
        let l = SqliteAdapter::open_filtered(&path, table, where_sql).unwrap();
        assert_eq!(show(&l, q), show(&e, q), "{q}");
    }
    // the planner agrees to prefilter beside hops for a keyed adapter…
    let keyed = |q: &str| quarb_sql::partial_pushdown_keyed(q, Some(quarb_sql::Dialect::Sqlite));
    assert!(keyed("/staff/*[::id < 3]<-boss").is_ok());
    assert!(keyed("/books/*[::id < 4]::author_id-->::name").is_ok());
    assert!(keyed("/authors/*[::id < 3]<--(/books/*::author_id)").is_ok());
    // …but not where the table would be listed a second time
    assert!(keyed("/books/*[::id < 4]::author_id=>(/books/*::author_id)").is_err());
    assert!(keyed("/authors/*[::id < 3]::id=>(::author_id)").is_err());
    assert!(keyed("/books/*[::id < 4] | .n((^/books/* @| count))").is_err());
    // …and never for an adapter that filters every read of the table
    assert!(
        quarb_sql::partial_pushdown("/staff/*[::id < 3]<-boss", Some(quarb_sql::Dialect::Sqlite))
            .is_none()
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_column_with_no_key_resolves_only_under_a_label() {
    let path = file_db("label");
    let l = SqliteAdapter::open(&path).unwrap();
    // notes.about declares no key: the label names the table
    assert_eq!(show(&l, "/notes/1::about-->authors::name"), ["Austen"]);
    let err = quarb::run("/notes/1::about-->::name", &l).unwrap_err();
    assert!(err.to_string().contains("needs a reference label"), "{err}");
    // a declared key needs none, and a missing property is simply empty
    assert_eq!(show(&l, "/books/1::author_id-->::name"), ["Austen"]);
    assert!(show(&l, "/books/1::nosuch-->").is_empty());
    let _ = std::fs::remove_file(path);
}
