//! Steps read by key: a lazily opened database answers `-->`, `->`,
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
    // the planner agrees to prefilter beside steps for a keyed adapter…
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

/// A sweep of the reverse resolution is read as a sweep of the
/// incoming link step is: the referrers of the whole context in
/// statements of some hundred keys. A row at a time, a table's
/// keyed statements were spent after 64 rows and the table was then
/// read whole.
#[test]
fn a_sweep_of_the_reverse_resolution_reads_its_referrers_together() {
    let path = std::env::temp_dir().join(format!("quarb-keyed-{}-sweep.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE cantos (id INTEGER PRIMARY KEY, title TEXT);
             CREATE TABLE lines (id INTEGER PRIMARY KEY,
                 canto_id INTEGER REFERENCES cantos(id), text TEXT);",
        )
        .unwrap();
        for n in 1..=100 {
            conn.execute(
                "INSERT INTO cantos VALUES (?1, ?2)",
                (n, format!("canto {n}")),
            )
            .unwrap();
            conn.execute("INSERT INTO lines VALUES (?1, ?2, 'a line')", (n, n))
                .unwrap();
        }
        // lines of no canto: what a whole read would bring in
        for n in 101..=150 {
            conn.execute("INSERT INTO lines VALUES (?1, NULL, 'a stray line')", (n,))
                .unwrap();
        }
    }
    let read = "| %(n = $_; whole = (^/lines:::loaded); read = (^/lines:::\"n-loaded\"))";
    let a = SqliteAdapter::open(&path).unwrap();
    assert_eq!(
        show(&a, &format!("/cantos/*<--(::canto_id) @| count {read}")),
        [r#"%(n = 100; whole = false; read = 100)"#]
    );
    // the incoming link step: the same rows, read the same way
    let b = SqliteAdapter::open(&path).unwrap();
    assert_eq!(
        show(&b, &format!("/cantos/*<-canto @| count {read}")),
        [r#"%(n = 100; whole = false; read = 100)"#]
    );
    let _ = std::fs::remove_file(path);
}

/// A sweep over many rows that refer to many keys is read in time
/// linear in the rows. The adapter is handed one value a thread and
/// drops the repeats; it scanned its list of wanted keys for each
/// value, so a step glued to a path, `/items/*::order_id-->`, took
/// 146 s over 1,000,000 items of 50,000 orders where the piped
/// spelling, a thread at a time, took 6 (lang's finding of
/// 2026-11-17). Four times the rows and the keys must cost well
/// under the sixteen times a scan would; the faster of two runs
/// each, so a loaded machine does not fail it.
#[test]
fn a_sweep_over_many_keys_is_linear_in_its_rows() {
    let path = std::env::temp_dir().join(format!("quarb-keyed-{}-linear.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE cantos (id INTEGER PRIMARY KEY, title TEXT);
             CREATE TABLE lines (id INTEGER PRIMARY KEY,
                 canto_id INTEGER REFERENCES cantos(id), text TEXT);
             CREATE TABLE verses (id INTEGER PRIMARY KEY,
                 canto_id INTEGER REFERENCES cantos(id), text TEXT);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 40000)
                 INSERT INTO cantos SELECT i, 'canto ' || i FROM n;
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 20000)
                 INSERT INTO lines SELECT i, 1 + (i * 7919) % 10000, 'a line' FROM n;
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 80000)
                 INSERT INTO verses SELECT i, 1 + (i * 7919) % 40000, 'a verse' FROM n;",
        )
        .unwrap();
    }
    let sweep = |table: &str, rows: &str| {
        (0..2)
            .map(|_| {
                let a = SqliteAdapter::open(&path).unwrap();
                let asked = std::time::Instant::now();
                assert_eq!(
                    show(&a, &format!("/{table}/*::canto_id--> @| count")),
                    [rows]
                );
                asked.elapsed()
            })
            .min()
            .unwrap()
    };
    let small = sweep("lines", "20000");
    let large = sweep("verses", "80000");
    assert!(
        large < small * 9,
        "20,000 rows over 10,000 keys took {small:?}, 80,000 over 40,000 took {large:?}"
    );
    let _ = std::fs::remove_file(path);
}

/// A step that reads a table by a column no index leads says so,
/// once a run, as an info beside the answer (the user's word of
/// 2026-11-17 on the reverse resolution); with an index on the column,
/// or where the column is a key, it says nothing.
#[test]
fn a_step_over_a_column_without_an_index_says_so() {
    let path = std::env::temp_dir().join(format!("quarb-keyed-{}-index.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE cantos (id INTEGER PRIMARY KEY, title TEXT);
         CREATE TABLE lines (id INTEGER PRIMARY KEY,
             canto_id INTEGER REFERENCES cantos(id), text TEXT);
         INSERT INTO cantos VALUES (1, 'canto 1'), (2, 'canto 2'), (3, 'canto 3');
         INSERT INTO lines VALUES (1, 1, 'a'), (2, 1, 'b'), (3, 2, 'c'), (4, 3, 'd');",
    )
    .unwrap();
    let said = |q: &str| -> (Vec<String>, Vec<String>) {
        let a = SqliteAdapter::open(&path).unwrap();
        let out = quarb::Query::parse(q, &a)
            .unwrap()
            .run(&a, &quarb::Run::ambient())
            .unwrap();
        let values = match out.result {
            QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
            QueryResult::Nodes(ns) => ns.into_iter().map(|n| a.locator(n)).collect(),
        };
        let infos = out
            .diagnostics
            .iter()
            .filter(|d| d.code == "UNINDEXED")
            .map(|d| format!("{} ({}): {}", d.code, d.site, d.message))
            .collect();
        (values, infos)
    };
    const TOLD: &str = "UNINDEXED (lines.canto_id): lines is read by canto_id, which has no \
                        index, so each statement of the step scans the table; CREATE INDEX \
                        lines_canto_id ON lines(canto_id) lets the database answer by key, or \
                        mount with ?crosslinks=canto to hold the reference in memory";
    // the reverse resolution and the incoming link step, each
    // once for the whole sweep
    let (values, infos) = said("/cantos/*<--(::canto_id) @| count");
    assert_eq!(values, ["4"]);
    assert_eq!(infos, [TOLD]);
    let (values, infos) = said("/cantos/1<-canto @| count");
    assert_eq!(values, ["2"]);
    assert_eq!(infos, [TOLD]);
    // forward, by the key: nothing to say
    let (values, infos) = said("/lines/*->canto @| unique @| count");
    assert_eq!(values, ["3"]);
    assert!(infos.is_empty(), "{infos:?}");
    // with an index on the column the step says nothing
    conn.execute_batch("CREATE INDEX lines_by_canto ON lines(canto_id);")
        .unwrap();
    let (values, infos) = said("/cantos/*<--(::canto_id) @| count");
    assert_eq!(values, ["4"]);
    assert!(infos.is_empty(), "{infos:?}");
    let _ = std::fs::remove_file(path);
}

/// A step by one label reads the one reference the label names
/// (the user's choice of 2026-11-17, on lang's finding). Two tables
/// refer to `poets`: the cantos by `poet_id`, the letters by
/// `sender_id`. `<-poet` read both, and so did `<--(::poet_id)`:
/// the referrers under every key into the table, with a notice of a
/// column without an index for each. It reads the cantos, and the
/// letters are not touched; forward, `->poet` reads the row that key
/// points at and not the patron's.
#[test]
fn a_step_by_one_label_reads_its_own_reference() {
    let path = std::env::temp_dir().join(format!("quarb-keyed-{}-onelabel.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE poets (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE cantos (id INTEGER PRIMARY KEY, title TEXT,
                 poet_id INTEGER REFERENCES poets(id),
                 patron_id INTEGER REFERENCES poets(id));
             CREATE TABLE letters (id INTEGER PRIMARY KEY, body TEXT,
                 sender_id INTEGER REFERENCES poets(id));
             INSERT INTO poets VALUES (1, 'Homer'), (2, 'Virgil'), (3, 'Dante'), (4, 'Horace');
             INSERT INTO cantos VALUES (1, 'Inferno I', 3, 2), (2, 'Inferno II', 3, 2),
                 (3, 'Aeneid I', 2, 4), (4, 'Iliad I', 1, NULL);
             INSERT INTO letters VALUES (1, 'to Maecenas', 4), (2, 'to Augustus', 4),
                 (3, 'to Cangrande', 3);",
        )
        .unwrap();
    }
    // the answer, the rows read of each table, and the columns a
    // notice of a missing index names
    let read = "| %(n = $_; poets = (^/poets:::\"n-loaded\"); cantos = (^/cantos:::\"n-loaded\"); \
                letters = (^/letters:::\"n-loaded\"))";
    let said = |q: &str| -> (Vec<String>, Vec<String>) {
        let a = SqliteAdapter::open(&path).unwrap();
        let out = quarb::Query::parse(&format!("{q} {read}"), &a)
            .unwrap()
            .run(&a, &quarb::Run::ambient())
            .unwrap();
        let values = match out.result {
            QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
            QueryResult::Nodes(ns) => ns.into_iter().map(|n| a.locator(n)).collect(),
        };
        let unindexed = out
            .diagnostics
            .iter()
            .filter(|d| d.code == "UNINDEXED")
            .map(|d| d.site.clone())
            .collect();
        (values, unindexed)
    };
    // one node, by the incoming link step and by the reverse
    // resolution: Dante's cantos, and no letter read
    for q in ["/poets/3<-poet @| count", "/poets/3<--(::poet_id) @| count"] {
        let (values, unindexed) = said(q);
        assert_eq!(
            values,
            ["%(n = 2; poets = 1; cantos = 2; letters = 0)"],
            "{q}"
        );
        assert_eq!(unindexed, ["cantos.poet_id"], "{q}");
    }
    // the other reference of the same table, by its own label
    let (values, unindexed) = said("/poets/2<-patron @| count");
    assert_eq!(values, ["%(n = 2; poets = 1; cantos = 2; letters = 0)"]);
    assert_eq!(unindexed, ["cantos.patron_id"]);
    let (values, unindexed) = said("/poets/4<-sender @| count");
    assert_eq!(values, ["%(n = 2; poets = 1; cantos = 0; letters = 2)"]);
    assert_eq!(unindexed, ["letters.sender_id"]);
    // a step that names no label follows every reference, as before
    let (values, unindexed) = said("/poets/3<-* @| count");
    assert_eq!(values, ["%(n = 3; poets = 1; cantos = 2; letters = 1)"]);
    assert_eq!(
        unindexed,
        ["cantos.patron_id", "cantos.poet_id", "letters.sender_id"]
    );
    // a sweep, by the label and by the property
    for q in ["/poets/*<-poet @| count", "/poets/*<--(::poet_id) @| count"] {
        let (values, unindexed) = said(q);
        assert_eq!(
            values,
            ["%(n = 4; poets = 4; cantos = 4; letters = 0)"],
            "{q}"
        );
        assert_eq!(unindexed, ["cantos.poet_id"], "{q}");
    }
    // forward: the poet's row is read by its key, the patron's is not
    let (values, _) = said("/cantos/3->poet @| count");
    assert_eq!(values, ["%(n = 1; poets = 1; cantos = 1; letters = 0)"]);
    let (values, _) = said("/cantos/3->* @| count");
    assert_eq!(values, ["%(n = 2; poets = 2; cantos = 1; letters = 0)"]);
    // a forward sweep by one label, and by the property: the two
    // cantos' one poet, and their patron not read (the listing reads
    // the cantos whole)
    for q in [
        "/cantos/*[::id < 3]->poet @| count",
        "/cantos/*[::id < 3]::poet_id--> @| count",
    ] {
        let (values, _) = said(q);
        assert_eq!(
            values,
            ["%(n = 2; poets = 1; cantos = 4; letters = 0)"],
            "{q}"
        );
    }
    let (values, _) = said("/cantos/*[::id < 3]->* @| count");
    assert_eq!(values, ["%(n = 4; poets = 2; cantos = 4; letters = 0)"]);
    let _ = std::fs::remove_file(path);
}

/// `?crosslinks=LABEL` (the user's ruling of 2026-11-17: a name is
/// the link's label, as in `<-canto`): the reference of that label
/// is held in memory. The first step along it reads the referring
/// table whole, the steps after send no statement, and nothing is
/// said of a scan; a label the database does not carry is refused
/// by name.
#[test]
fn a_held_crosslink_reads_its_table_whole_once() {
    let path = std::env::temp_dir().join(format!("quarb-keyed-{}-held.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE cantos (id INTEGER PRIMARY KEY, title TEXT);
             CREATE TABLE lines (id INTEGER PRIMARY KEY,
                 canto_id INTEGER REFERENCES cantos(id), text TEXT);
             INSERT INTO cantos VALUES (1, 'canto 1'), (2, 'canto 2');
             INSERT INTO lines VALUES (1, 1, 'a'), (2, 1, 'b'), (3, 2, 'c'), (4, NULL, 'd');",
        )
        .unwrap();
    }
    let read = "| %(n = $_; whole = (^/lines:::loaded); read = (^/lines:::\"n-loaded\"))";
    let held = || {
        let mut a = SqliteAdapter::open(&path).unwrap();
        a.hold_crosslinks(&["canto".to_string()]).unwrap();
        a
    };
    // unheld: one canto's lines by key, the table not whole
    let a = SqliteAdapter::open(&path).unwrap();
    assert_eq!(
        show(&a, &format!("/cantos/1<-canto @| count {read}")),
        [r#"%(n = 2; whole = false; read = 2)"#]
    );
    // held: the table whole at the first step, by either arrow
    assert_eq!(
        show(&held(), &format!("/cantos/1<-canto @| count {read}")),
        [r#"%(n = 2; whole = true; read = 4)"#]
    );
    assert_eq!(
        show(
            &held(),
            &format!("/cantos/2<--(::canto_id) @| count {read}")
        ),
        [r#"%(n = 1; whole = true; read = 4)"#]
    );
    // and nothing is said of a scan: the reference is in memory
    let a = held();
    let out = quarb::Query::parse("/cantos/*<-canto @| count", &a)
        .unwrap()
        .run(&a, &quarb::Run::ambient())
        .unwrap();
    assert!(
        out.diagnostics.iter().all(|d| d.code != "UNINDEXED"),
        "{:?}",
        out.diagnostics
    );
    // a label the database does not carry
    let mut a = SqliteAdapter::open(&path).unwrap();
    assert_eq!(
        a.hold_crosslinks(&["verse".to_string()]).unwrap_err(),
        "crosslinks=\"verse\": no reference of this database is labelled \"verse\" (its \
         labels: canto)"
    );
    let _ = std::fs::remove_file(path);
}
