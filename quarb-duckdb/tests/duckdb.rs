//! An in-test database: catalog, keys, lazy rows, pushdown.

use quarb_duckdb::DuckdbAdapter;

fn fixture() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("quarb-duckdb-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.duckdb");
    let _ = std::fs::remove_file(&path);
    let conn = duckdb::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE artists (id INTEGER PRIMARY KEY, name TEXT);
         CREATE TABLE tracks (id INTEGER PRIMARY KEY, title TEXT, secs INTEGER,
                              artist_id INTEGER REFERENCES artists(id));
         INSERT INTO artists VALUES (1,'Holst'), (2,'Bartok');
         INSERT INTO tracks VALUES (1,'Mars',430,1),(2,'Venus',480,1),(3,'Bourree',95,2);",
    )
    .unwrap();
    path
}

#[test]
fn catalog_keys_and_pushdown() {
    let path = fixture();
    let a = DuckdbAdapter::open(&path).unwrap();
    let v = |q: &str| match quarb::run(q, &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    };
    assert_eq!(v("/tracks/* @| count"), ["3"]);
    // The FK from duckdb_constraints() drives resolution.
    assert_eq!(v("/tracks/1::artist_id~>::name"), ["Holst"]);
    assert_eq!(v("/artists/2::artist_id<~ @| count"), ["1"]);
    // The pushdown path answers identically to the scan.
    let plan = quarb_sql::pushdown("/tracks/*[::secs > 100] | rec(::title)").unwrap();
    let (cols, rows) = quarb_duckdb::raw_query(
        &path,
        &plan.sql,
        plan.order_table.as_deref(),
        plan.join_left
            .as_ref()
            .map(|(t, c)| (t.as_str(), c.as_slice())),
    )
    .unwrap();
    assert_eq!(cols, ["title"]);
    let pushed: Vec<String> = rows.into_iter().map(|r| r[0].to_string()).collect();
    assert_eq!(pushed, ["Mars", "Venus"]);
}

// Columns outside the numeric/text families used to decode to null for
// every row (the catch-all leaned on ValueRef::as_str(), which only
// succeeds for Text). Each of these types must now read as a value.
#[test]
fn non_text_columns_decode() {
    let dir = std::env::temp_dir().join("quarb-duckdb-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("typed.duckdb");
    let _ = std::fs::remove_file(&path);
    let conn = duckdb::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE events (id INTEGER PRIMARY KEY, ts TIMESTAMP, day DATE,
                              amount DECIMAL(10,2), big HUGEINT);
         INSERT INTO events VALUES
           (1, TIMESTAMP '2021-06-15 12:30:00', DATE '2021-06-15', 19.99,
            100000000000000000000),
           (2, TIMESTAMP '2020-01-01 00:00:00', DATE '2020-01-01', 5.00, 42);",
    )
    .unwrap();
    let a = DuckdbAdapter::open(&path).unwrap();
    let v = |q: &str| match quarb::run(q, &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    };
    // TIMESTAMP lands on the timeline; a DATE prints as a bare date.
    assert_eq!(v("/events/1::ts"), ["2021-06-15T12:30:00"]);
    assert_eq!(v("/events/1::day"), ["2021-06-15"]);
    // DECIMAL joins the numeric family as a float.
    assert_eq!(v("/events/1::amount"), ["19.99"]);
    // HUGEINT narrows when it fits, keeps its decimal text when it can't.
    assert_eq!(v("/events/2::big"), ["42"]);
    assert_eq!(v("/events/1::big"), ["100000000000000000000"]);
}
