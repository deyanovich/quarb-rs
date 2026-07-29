//! End-to-end tests: queries run through the engine against a
//! materialized SQLite database. The schema's declared foreign keys
//! drive `~>` resolution, `->`/`<-` crosslinks, and `<~` reverse
//! resolution.

use quarb::AstAdapter as _;
use quarb_sqlite::SqliteAdapter;
use rusqlite::Connection;

fn adapter() -> SqliteAdapter {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE artists (
          id INTEGER PRIMARY KEY,
          name TEXT
        );
        CREATE TABLE albums (
          id INTEGER PRIMARY KEY,
          title TEXT,
          artist_id INTEGER REFERENCES artists(id)
        );
        CREATE TABLE tracks (
          id INTEGER PRIMARY KEY,
          title TEXT,
          album_id INTEGER REFERENCES albums(id),
          price REAL,
          secs INTEGER
        );
        INSERT INTO artists VALUES (1, 'Holst'), (2, 'Bartok'), (3, 'Satie');
        INSERT INTO albums VALUES
          (1, 'The Planets', 1),
          (2, 'Mikrokosmos', 2),
          (3, 'Gymnopedies', 3),
          (4, 'Quartets', 2);
        INSERT INTO tracks VALUES
          (1, 'Mars', 1, 1.29, 430),
          (2, 'Venus', 1, 0.99, 480),
          (3, 'Jupiter', 1, 1.29, 470),
          (4, 'Bourree', 2, 0.99, 95),
          (5, 'Ostinato', 2, 1.29, 140),
          (6, 'Gymnopedie No.1', 3, 0.99, 210),
          (7, 'Quartet No.4', 4, 1.29, 1500),
          (8, 'Unfiled Sketch', NULL, 0.50, 60);
        "#,
    )
    .unwrap();
    SqliteAdapter::load(&conn).unwrap()
}

fn values(query: &str) -> Vec<String> {
    let a = adapter();
    match quarb::run(query, &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    }
}

fn nodes(query: &str) -> Vec<String> {
    let a = adapter();
    match quarb::run(query, &a).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns.into_iter().map(|n| a.locator(n)).collect(),
        quarb::QueryResult::Values(_) => panic!("expected nodes"),
    }
}

#[test]
fn tables_and_rows() {
    // tables are the root's children; rows are named by primary key
    assert_eq!(nodes("/*"), vec!["/albums", "/artists", "/tracks"]);
    assert_eq!(values("/tracks;;;n-rows"), vec!["8"]);
    assert_eq!(nodes("/tracks/7"), vec!["/tracks/7"]);
    assert_eq!(values("/artists/2::name"), vec!["Bartok"]);
    // columns are properties; SQL NULL is null (dropna filters)
    assert_eq!(values("/tracks/*[::price > 1]::title @| count"), vec!["4"]);
    assert_eq!(values("/tracks/*[::album_id] @| count"), vec!["7"]);
    assert_eq!(values("/tracks/8;;;table"), vec!["tracks"]);
}

#[test]
fn fk_resolution() {
    // ~> follows a declared foreign key: no join spelled
    assert_eq!(values("/tracks/1::album_id~>::title"), vec!["The Planets"]);
    // ... and chains: track -> album -> artist
    assert_eq!(
        values("/tracks/*[::title = \"Bourree\"]::album_id~>::artist_id~>::name"),
        vec!["Bartok"]
    );
    // a NULL FK resolves to nothing, not an error
    assert_eq!(values("/tracks/8::album_id~>::title @| count"), vec!["0"]);
    // the hint names a table for undeclared references
    assert_eq!(
        values("/tracks/1::album_id~>albums::title"),
        vec!["The Planets"]
    );
}

#[test]
fn fk_links() {
    // each FK column is an outgoing crosslink labeled by column name
    assert_eq!(nodes("/tracks/4->album_id"), vec!["/albums/2"]);
    assert_eq!(nodes("/tracks/4->album_id->artist_id"), vec!["/artists/2"]);
    // backlinks: which rows point here?
    assert_eq!(
        nodes("/albums/1<-album_id"),
        vec!["/tracks/1", "/tracks/2", "/tracks/3"]
    );
    // reverse resolution: rows whose artist_id points here
    assert_eq!(
        values("/artists/2::artist_id<~::title"),
        vec!["Mikrokosmos", "Quartets"]
    );
}

#[test]
fn sql_shapes() {
    // SELECT title, price FROM tracks WHERE price < 1 ORDER BY title
    assert_eq!(
        values("/tracks/*[::price < 1] @| sort_by(::title) | rec(::title, ::price)"),
        vec![
            r#"{"title": "Bourree", "price": 0.99}"#,
            r#"{"title": "Gymnopedie No.1", "price": 0.99}"#,
            r#"{"title": "Unfiled Sketch", "price": 0.5}"#,
            r#"{"title": "Venus", "price": 0.99}"#
        ]
    );
    // GROUP BY album HAVING count > 1 — via the FK chain in the key
    assert_eq!(
        values(
            "/tracks/*[::album_id] @| group(\"album\", ::album_id~>::title) \
             | count | .n | [$_ > 1] | %."
        ),
        vec![
            r#"{"album": "The Planets", "n": 3}"#,
            r#"{"album": "Mikrokosmos", "n": 2}"#
        ]
    );
    // JOIN with projection of both sides (the witness)
    assert_eq!(
        values(
            "/tracks/*[::secs > 400] <=> /albums/*[::id = $$::album_id] \
             | rec(\"album\", $*1::title, ::title)"
        ),
        vec![
            r#"{"album": "The Planets", "title": "Mars"}"#,
            r#"{"album": "The Planets", "title": "Venus"}"#,
            r#"{"album": "The Planets", "title": "Jupiter"}"#,
            r#"{"album": "Quartets", "title": "Quartet No.4"}"#
        ]
    );
    // correlated subquery: albums with revenue over 2 (SUM per row)
    assert_eq!(
        values(
            "/albums/* | .id(::id) \
             | .rev(^/tracks/*[::album_id = $$.id]::price @| sum) \
             | [$.rev > 2] | rec(::title, \"revenue\", $.rev)"
        ),
        vec![
            r#"{"title": "The Planets", "revenue": 3.5700000000000003}"#,
            r#"{"title": "Mikrokosmos", "revenue": 2.2800000000000002}"#
        ]
    );
}

/// Reserved-word / special-character table names must be quoted, and
/// WITHOUT ROWID tables (which have no rowid column) must still load.
/// Before the fix, `specs()`/`fetch_rows_where` interpolated the name
/// unquoted and unconditionally selected `rowid`, so these tables
/// failed to load (silently empty under the lazy model).
#[test]
fn tricky_names_and_without_rowid() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE "Order" (id INTEGER PRIMARY KEY, note TEXT);
        CREATE TABLE "my table" (id INTEGER PRIMARY KEY, note TEXT);
        CREATE TABLE kv (k TEXT PRIMARY KEY, v TEXT) WITHOUT ROWID;
        INSERT INTO "Order" VALUES (1, 'a'), (2, 'b');
        INSERT INTO "my table" VALUES (1, 'x');
        INSERT INTO kv VALUES ('one', '1'), ('two', '2'), ('three', '3');
        "#,
    )
    .unwrap();
    // Pre-fix this errored (the eager path propagates the failure).
    let a = SqliteAdapter::load(&conn).unwrap();

    let node = |table: &str| {
        a.children(a.root())
            .into_iter()
            .find(|n| a.name(*n).as_deref() == Some(table))
            .unwrap_or_else(|| panic!("table {table} missing"))
    };
    assert_eq!(a.children(node("Order")).len(), 2);
    assert_eq!(a.children(node("my table")).len(), 1);
    // WITHOUT ROWID: rows load, keyed by the single-column pk (`k`),
    // ordered by that key (ordinals stand in for the absent rowid).
    let kv_keys: Vec<String> = a
        .children(node("kv"))
        .iter()
        .filter_map(|n| a.name(*n))
        .collect();
    assert_eq!(kv_keys, vec!["one", "three", "two"]);
}

/// Pushdown may only ever change speed: the pushed path and the
/// scan path must produce identical output.
#[test]
fn pushdown_matches_scan() {
    use std::io::Write as _;
    // A file-backed DB (raw_query opens by path).
    let dir = std::env::temp_dir().join("quarb-sqlite-pushdown-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("music.db");
    let _ = std::fs::remove_file(&path);
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE tracks (id INTEGER PRIMARY KEY, title TEXT, secs INTEGER, price REAL);
             INSERT INTO tracks VALUES (1,'Mars',430,1.29), (2,'Venus',480,0.99),
               (3,'Jupiter',470,1.29), (4,'Bourree',95,0.99);",
        )
        .unwrap();
        std::io::stdout().flush().unwrap();
    }
    let cases = [
        "/tracks/* @| count",
        "/tracks/* | ::price @| sum",
        "/tracks/*[::price < 1] | rec(::title, ::secs)",
        "/tracks/*[::secs > 100 and ::price >= 1] | ::title",
    ];
    for q in cases {
        let plan = quarb_sql::pushdown(q, Some(quarb_sql::Dialect::Sqlite))
            .unwrap_or_else(|| panic!("expected pushdown for {q}"));
        let (cols, rows) = quarb_sqlite::raw_query(
            &path,
            &plan.sql,
            plan.order_table.as_deref(),
            plan.join_left
                .as_ref()
                .map(|(t, c)| (t.as_str(), c.as_slice())),
        )
        .unwrap();
        let pushed: Vec<String> = rows
            .into_iter()
            .map(|row| {
                if cols.len() <= 1 {
                    row[0].to_string()
                } else {
                    quarb::Value::Record(cols.iter().cloned().zip(row).collect()).to_string()
                }
            })
            .collect();
        let adapter = SqliteAdapter::open(&path).unwrap();
        let scanned: Vec<String> = match quarb::run(q, &adapter).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
            quarb::QueryResult::Nodes(_) => panic!("expected values for {q}"),
        };
        assert_eq!(pushed, scanned, "pushdown/scan divergence for: {q}");
    }
}

/// Partial pushdown identity: the filtered-fetch adapter running
/// the ORIGINAL query equals the plain scan.
#[test]
fn filtered_open_matches_scan() {
    let dir = std::env::temp_dir().join("quarb-sqlite-partial-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("events.db");
    let _ = std::fs::remove_file(&path);
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE events (id INTEGER PRIMARY KEY, kind TEXT, amount REAL);
             INSERT INTO events VALUES
               (1,'a',10.0),(2,'b',20.0),(3,'a',30.0),(4,'b',40.0),(5,'a',50.0);",
        )
        .unwrap();
    }
    let q = "/events/*[::kind = \"a\"] | ::amount @| group(\"half\", ::amount idiv 25) \
             | count | .n | %.";
    let plan = quarb_sql::partial_pushdown(q).expect("partial plan");
    let run = |a: &SqliteAdapter| -> Vec<String> {
        match quarb::run(q, a).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
            _ => panic!("expected values"),
        }
    };
    let filtered = SqliteAdapter::open_filtered(&path, &plan.table, &plan.where_sql).unwrap();
    let plain = SqliteAdapter::open(&path).unwrap();
    assert_eq!(run(&filtered), run(&plain));
}

#[test]
fn raw_query_join_uniqueness_gate() {
    // The witness-JOIN soundness gate: a plan whose ON binds the
    // left table by its primary key executes; one bound by a
    // non-key column is refused (the SQL JOIN would multiply rows
    // where the engine's existential binding does not).
    let path = std::env::temp_dir().join(format!(
        "quarb-sqlite-unique-gate-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE a (id INTEGER PRIMARY KEY, name TEXT);
        CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id));
        INSERT INTO a VALUES (1, 'one'), (2, 'two');
        INSERT INTO b VALUES (10, 1), (11, 1), (12, 2);
        "#,
    )
    .unwrap();
    drop(conn);

    let sql = "SELECT a.name, b.id FROM a JOIN b ON b.a_id = a.id";
    let ok = quarb_sqlite::raw_query(&path, sql, Some("b"), Some(("a", &["id".to_string()][..])));
    assert_eq!(ok.expect("PK-bound join executes").1.len(), 3);

    let refused = quarb_sqlite::raw_query(
        &path,
        "SELECT b.id FROM b JOIN a ON a.a_id = b.a_id",
        Some("a"),
        Some(("b", &["a_id".to_string()][..])),
    );
    assert!(
        refused.is_err(),
        "non-key binding must refuse: the JOIN multiplies rows"
    );

    let _ = std::fs::remove_file(&path);
}

/// A database that never touched a filesystem: serialize the test
/// db to bytes, reopen with from_bytes (the browser-upload path),
/// and query — FK resolution included.
#[test]
fn from_bytes_roundtrip() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE artists (id INTEGER PRIMARY KEY, name TEXT);
         CREATE TABLE tracks (id INTEGER PRIMARY KEY, title TEXT,
                              artist_id INTEGER REFERENCES artists(id));
         INSERT INTO artists VALUES (1, 'Holst');
         INSERT INTO tracks VALUES (1, 'Jupiter', 1), (2, 'Mars', 1);",
    )
    .unwrap();
    let bytes: Vec<u8> = conn.serialize(rusqlite::MAIN_DB).unwrap().to_vec();
    drop(conn);
    let a = SqliteAdapter::from_bytes(&bytes).unwrap();
    let got = match quarb::run("/tracks/*[::artist_id~>::name = 'Holst']::title", &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        _ => panic!("expected values"),
    };
    assert_eq!(got, vec!["Jupiter", "Mars"]);
}

/// The declared-references fixture: one flat table whose two TEXT
/// columns are identities, plus the SELECT DISTINCT dimension views
/// over them — the ad-hoc bipartite graph a schema without foreign
/// keys only implies.
fn forum() -> SqliteAdapter {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE posts (
          id INTEGER PRIMARY KEY,
          ip TEXT NOT NULL,
          cookie TEXT NOT NULL
        );
        INSERT INTO posts VALUES
          (1, 'ip-a', 'ck-1'),
          (2, 'ip-a', 'ck-2'),
          (3, 'ip-b', 'ck-2'),
          (4, 'ip-b', 'ck-3'),
          (5, 'ip-c', 'ck-9');
        CREATE VIEW cookies AS SELECT DISTINCT cookie AS id FROM posts;
        CREATE VIEW ips     AS SELECT DISTINCT ip     AS id FROM posts;
        "#,
    )
    .unwrap();
    let refs = [
        ("cookie".to_string(), "cookies".to_string()),
        ("ip".to_string(), "ips".to_string()),
    ];
    SqliteAdapter::load_with_refs(&conn, &refs).unwrap()
}

fn forum_nodes(query: &str) -> Vec<String> {
    let a = forum();
    match quarb::run(query, &a).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns.into_iter().map(|n| a.locator(n)).collect(),
        quarb::QueryResult::Values(_) => panic!("expected nodes"),
    }
}

fn forum_values(query: &str) -> Vec<String> {
    let a = forum();
    match quarb::run(query, &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    }
}

#[test]
fn views_are_containers() {
    // views join the catalog beside the tables
    assert_eq!(forum_nodes("/*"), vec!["/cookies", "/ips", "/posts"]);
    // a single-column view's rows are named by that column, the way
    // a single-column primary key names a table's rows
    assert_eq!(forum_nodes("/cookies/ck-2"), vec!["/cookies/ck-2"]);
    assert_eq!(forum_values("/ips/* @| count"), vec!["3"]);
}

#[test]
fn declared_refs_are_crosslinks() {
    // the refs document supplies what the schema never declared:
    // forward, reverse, and resolution — the full fabric
    assert_eq!(forum_nodes("/posts/1->cookie"), vec!["/cookies/ck-1"]);
    assert_eq!(forum_values("/posts/1::ip~>::id"), vec!["ip-a"]);
    assert_eq!(
        forum_nodes("/cookies/ck-2<-cookie"),
        vec!["/posts/2", "/posts/3"]
    );
}

#[test]
fn declared_refs_quantify() {
    // the bipartite closure: cookie -> its posts -> their ips ->
    // other posts there -> their cookies, transitively. The edge is
    // entered by the matched column and left by the other, so the
    // undirected value graph needs no alternation.
    assert_eq!(
        forum_values("/cookies/ck-1(<-cookie->ip<-ip->cookie)+::id"),
        vec!["ck-2", "ck-3"]
    );
    // ck-9 shares no ip: a different component
    assert_eq!(
        forum_values("/cookies/ck-9(<-cookie->ip<-ip->cookie)+::id @| count"),
        vec!["0"]
    );
}

#[test]
fn scoped_refs_and_precedence() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE users (id TEXT PRIMARY KEY, boss TEXT);
        CREATE TABLE rooms (id TEXT PRIMARY KEY, owner TEXT REFERENCES users(id));
        INSERT INTO users VALUES ('u1', NULL), ('u2', 'u1');
        INSERT INTO rooms VALUES ('r1', 'u2');
        "#,
    )
    .unwrap();
    // a scoped field binds only in its table; a declared FK wins
    // over a refs entry on the same column; a container's key
    // pointing at the container itself is the identity, not an edge
    let refs = [
        ("users.boss".to_string(), "users".to_string()),
        ("owner".to_string(), "rooms".to_string()),
        ("id".to_string(), "users".to_string()),
    ];
    let a = SqliteAdapter::load_with_refs(&conn, &refs).unwrap();
    let run_nodes = |q: &str| match quarb::run(q, &a).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns
            .into_iter()
            .map(|n| a.locator(n))
            .collect::<Vec<String>>(),
        _ => panic!("expected nodes"),
    };
    // users.boss: scoped declaration works (self-referencing table)
    assert_eq!(run_nodes("/users/u2->boss"), vec!["/users/u1"]);
    // rooms.owner: the declared FK to users still wins over the
    // refs entry pointing at rooms
    assert_eq!(run_nodes("/rooms/r1->owner"), vec!["/users/u2"]);
    // users.id: no self-loop was synthesized
    assert!(run_nodes("/users/u1->id").is_empty());
}

#[test]
fn headless_arrow_walks_both_directions() {
    // `--` follows the crosslink whichever way it exists here: from
    // a value node it is the backlink fan, from an edge row the
    // outgoing reference — so the bipartite closure spells without
    // direction claims, and the wildcard form walks the whole
    // component (both node kinds).
    assert_eq!(
        forum_nodes("/posts/1--cookie"),
        vec!["/cookies/ck-1"]
    );
    assert_eq!(
        forum_nodes("/cookies/ck-2--cookie"),
        vec!["/posts/2", "/posts/3"]
    );
    assert_eq!(
        forum_values("/cookies/ck-1(--cookie--ip--ip--cookie)+::id"),
        vec!["ck-2", "ck-3"]
    );
    // ck-1 .. ck-3 chain over ip-a/ip-b: the component holds the
    // other two cookies and both ips.
    assert_eq!(
        forum_values("/cookies/ck-1(--*--*)+::id @| count"),
        vec!["4"]
    );
}

#[test]
fn headless_arrow_union_on_self_reference() {
    // On a self-referencing FK the headless arrow is the union of
    // both directions: manager and reports under one matcher.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE employees (
          id INTEGER PRIMARY KEY,
          name TEXT,
          manager_id INTEGER REFERENCES employees(id)
        );
        INSERT INTO employees VALUES
          (1, 'Ada', NULL),
          (2, 'Bo',  1),
          (3, 'Cy',  1),
          (4, 'Dee', 2);
        "#,
    )
    .unwrap();
    let a = SqliteAdapter::load(&conn).unwrap();
    let vals = |q: &str| match quarb::run(q, &a).unwrap() {
        quarb::QueryResult::Values(vs) => {
            vs.iter().map(|v| v.to_string()).collect::<Vec<_>>()
        }
        _ => panic!("expected values"),
    };
    // Bo's neighbors under manager_id: his manager and his report.
    assert_eq!(vals("/employees/2--manager_id::name"), vec!["Ada", "Dee"]);
    // The whole org is one undirected component from any seat.
    assert_eq!(vals("/employees/4(--manager_id)+::name @| count"), vec!["3"]);
}
