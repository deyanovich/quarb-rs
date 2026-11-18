//! An in-test database: catalog, keys, lazy rows, pushdown.

use quarb_duckdb::DuckdbAdapter;

/// The order a plan owes, as its driver takes it.
fn order_of(plan: &quarb_sql::Pushdown) -> quarb_relational::PlanOrder<'_> {
    quarb_relational::PlanOrder {
        table: plan.order_table.as_deref(),
        then: plan.order_then.as_deref(),
        first: plan.order_first.as_deref(),
        desc: plan.order_desc,
        groups: plan.order_groups,
        limit: plan.limit,
    }
}

fn fixture() -> std::path::PathBuf {
    // One database per call: the tests run in parallel, and two of
    // them writing one file left the third a half-made database.
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("quarb-duckdb-test-{}-{n}", std::process::id()));
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
    assert_eq!(v("/tracks/1::artist_id-->::name"), ["Holst"]);
    assert_eq!(v("/artists/2<--(::artist_id) @| count"), ["1"]);
    // The pushdown path answers identically to the scan.
    let plan = quarb_sql::pushdown("/tracks/*[::secs > 100] | %(::title)", None).unwrap();
    let (cols, rows) = quarb_duckdb::raw_query(
        &path,
        &plan.sql,
        &order_of(&plan),
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

// A step reads the rows it lands on (`WHERE key IN (…)`), not the
// table; the answers are those of a table read whole.
#[test]
fn hops_read_by_key() {
    let path = fixture();
    let v = |q: &str| {
        let a = DuckdbAdapter::open(&path).unwrap();
        match quarb::run(q, &a).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
            quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
        }
    };
    assert_eq!(
        v(
            "/tracks/3::artist_id--> | %(::name; t = (^/tracks:::loaded); \
           a = (^/artists:::loaded); n = (^/artists:::\"n-loaded\"))"
        ),
        [r#"%(name = "Bartok"; t = false; a = false; n = 1)"#]
    );
    assert_eq!(
        v("/artists/1<-artist | %(::title; t = (^/tracks:::loaded))"),
        [
            r#"%(title = "Mars"; t = false)"#,
            r#"%(title = "Venus"; t = false)"#
        ]
    );
    assert_eq!(v("/tracks/2::title"), ["Venus"]);
    assert_eq!(v("/tracks/99::title"), Vec::<String>::new());
    // a row read by key knows its place once asked (the table is
    // then read to find it)
    assert_eq!(v("/tracks/3:::rowid"), ["3"]);
    // a listing still reads the table whole
    assert_eq!(
        v("/tracks/* @| count | %(n = $_; t = (^/tracks:::loaded))"),
        [r#"%(n = 3; t = true)"#]
    );
}

#[test]
fn the_catalog_estimates_a_table_before_any_scan() {
    let path = fixture();
    let a = DuckdbAdapter::open(&path).unwrap();
    let plan = quarb::Query::parse("/tracks/*[::title = \"Mars\"]", &a)
        .unwrap()
        .plan(&a);
    let step = &plan.branches[0].steps[1];
    assert_eq!(step.pushed, Some(vec![true]));
    let e = step.estimate.clone().unwrap();
    assert_eq!(e.nodes, (0, 3));
    assert_eq!(e.latency, quarb::Latency::Local);
    let whole = quarb::Query::parse("/tracks/*", &a).unwrap().plan(&a);
    assert_eq!(whole.estimate().unwrap().nodes, (0, 3));
    assert_eq!(
        whole.estimate().unwrap().confidence,
        quarb::Confidence::Bound
    );
}

/// An every-witness plan (`<=>*`) is pushed: the rows of one driver
/// in the joined table's own order, after the driver's, both by
/// their primary keys, which is what the engine's own evaluation
/// answers (0.69: pushed on SQLite alone before). With a key missing
/// the plan could not keep that order, and declines.
#[test]
fn raw_query_orders_an_every_witness_join_by_both_keys() {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("quarb-duckdb-every-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.duckdb");
    let _ = std::fs::remove_file(&path);
    {
        // the readings are stored out of their sensors' order
        let conn = duckdb::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE owners (id INTEGER PRIMARY KEY, sensor TEXT, team TEXT);
             CREATE TABLE readings (id INTEGER PRIMARY KEY, sensor TEXT, val DOUBLE);
             CREATE TABLE loose (sensor TEXT, note TEXT);
             INSERT INTO owners VALUES (3, 'c', 'lab'), (1, 'a', 'core'), (2, 'b', 'edge');
             INSERT INTO readings VALUES (4, 'a', 1.5), (1, 'b', 7.5), (3, 'b', 3.0), (2, 'a', 5.0);
             INSERT INTO loose VALUES ('a', 'x'), ('b', 'y');",
        )
        .unwrap();
    }
    let pushed = |q: &str| -> Result<Vec<String>, String> {
        let plan = quarb_sql::pushdown(q, None).expect("pushes");
        assert!(plan.order_then.is_some() && plan.join_left.is_none());
        quarb_duckdb::raw_query(&path, &plan.sql, &order_of(&plan), None)
            .map(|(_, rows)| {
                rows.iter()
                    .map(|r| {
                        r.iter()
                            .map(|v| v.to_string())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .collect()
            })
            .map_err(|e| e.to_string())
    };
    let scanned = |q: &str| -> Vec<String> {
        let a = DuckdbAdapter::open(&path).unwrap();
        match quarb::run(q, &a).unwrap() {
            quarb::QueryResult::Values(vs) => vs
                .iter()
                .map(|v| match v {
                    quarb::Value::Record(fields) => fields
                        .iter()
                        .map(|(_, v)| v.to_string())
                        .collect::<Vec<_>>()
                        .join(" "),
                    other => other.to_string(),
                })
                .collect(),
            other => panic!("{other:?}"),
        }
    };
    let inner =
        "/owners/* <=>* /readings/*[::sensor = $$0::sensor] | %(team = ::team; val = $$1::val)";
    let want = ["core 5.0", "core 1.5", "edge 7.5", "edge 3.0"];
    assert_eq!(scanned(inner), want);
    assert_eq!(pushed(inner).unwrap(), want);
    // the outer form keeps the driver with no pair
    let outer =
        "/owners/* <=>*? /readings/*[::sensor = $$0::sensor] | %(team = ::team; val = $$1::val)";
    // (a null prints as nothing here)
    let want = ["core 5.0", "core 1.5", "edge 7.5", "edge 3.0", "lab "];
    assert_eq!(scanned(outer), want);
    assert_eq!(pushed(outer).unwrap(), want);
    // a joined table with no primary key has no order to keep
    let keyless =
        "/owners/* <=>* /loose/*[::sensor = $$0::sensor] | %(team = ::team; n = $$1::note)";
    let refused = pushed(keyless).unwrap_err();
    assert!(
        refused.contains("by the primary key of loose, and loose has none"),
        "{refused}"
    );
}

/// The order a plan owes, written by this driver, against the
/// engine's own evaluation (as quarb-sqlite's `a_plans_own_order_is_
/// the_scans`): a numeric sort with NULLs and ties, descending,
/// reversed, the first rows, the groups of a fold as they first
/// appear, a distinct count; and the catalog's word on a column's
/// type, which decides whether such a plan is run here at all.
#[test]
fn a_plans_own_order_is_the_scans() {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("quarb-duckdb-order-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.duckdb");
    let _ = std::fs::remove_file(&path);
    {
        let conn = duckdb::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT, pages INTEGER,
                                 price DECIMAL(8,2), year BIGINT);
             INSERT INTO books VALUES
               (7, 'Emma', 320, 9.50, 1815), (2, 'Iliad', NULL, 12.00, -750),
               (5, 'Persuasion', 250, 9.50, 1817), (1, 'Odyssey', 320, NULL, -725),
               (9, 'Aeneid', 250, 7.25, -19), (4, 'Tom Sawyer', NULL, 7.25, 1876),
               (3, 'Histories', 700, 15.00, -430);",
        )
        .unwrap();
    }
    // the catalog: the integer and decimal columns numeric, the text
    // one not
    let catalog = quarb_duckdb::column_catalog(&path, &["books"]).unwrap();
    let numeric: Vec<&str> = catalog
        .iter()
        .filter(|(_, numeric, _)| *numeric)
        .map(|(name, _, _)| name.as_str())
        .collect();
    assert_eq!(numeric, ["id", "pages", "price", "year"]);
    for q in [
        "/books/* @| sort_by(::pages) | ::id",
        "/books/* @| sort_by(::pages; --desc) @| [..3] | ::title",
        "/books/* @| sort_by(::price) @| reverse | ::id",
        "/books/*[::year > 0] @| sort_by(::pages) | ::id",
        "/books/* @| [..3] | ::id",
        "/books/* @| reverse @| [..2] | %(::id; ::title)",
        "/books/* | .p(::pages) @|{.p} count | .n | %.",
        "/books/* | ::year | .p(::price) @|{.p} max | .latest | %.",
        "/books/*::pages @| unique @| count",
        "/books/* @| top(3; ::pages) | ::id",
        "/books/* @| bottom(2; ::price) | ::title",
        "/books/*[::pages != 320]::id",
        "/books/*[!(::pages > 300)] @| count",
    ] {
        let plan = quarb_sql::pushdown(q, None).unwrap_or_else(|| panic!("{q} is not pushed"));
        let (_, rows) = quarb_duckdb::raw_query(&path, &plan.sql, &order_of(&plan), None).unwrap();
        let pushed: Vec<String> = rows
            .iter()
            .map(|r| {
                r.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect();
        let a = DuckdbAdapter::open(&path).unwrap();
        let scanned: Vec<String> = match quarb::run(q, &a).unwrap() {
            quarb::QueryResult::Values(vs) => vs
                .iter()
                .map(|v| match v {
                    quarb::Value::Record(fields) => fields
                        .iter()
                        .map(|(_, v)| v.to_string())
                        .collect::<Vec<_>>()
                        .join("|"),
                    other => other.to_string(),
                })
                .collect(),
            other => panic!("{other:?}"),
        };
        assert!(!scanned.is_empty(), "{q}");
        assert_eq!(pushed, scanned, "{q}");
    }
}
