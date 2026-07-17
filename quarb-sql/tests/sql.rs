//! Translation assertions, plus the differential rig: the SQL runs
//! through SQLite and the translated Quarb through the engine over
//! the same database — the outputs must agree.

use quarb_sql::translate;
use rusqlite::Connection;

fn t(sql: &str) -> String {
    translate(sql).unwrap().query
}

#[test]
fn translations() {
    assert_eq!(
        t("SELECT title, price FROM tracks WHERE price < 1 ORDER BY title"),
        "/tracks/*[::price < 1] @| sort_by(::title) | rec(::title, ::price)"
    );
    assert_eq!(t("SELECT COUNT(*) FROM tracks"), "/tracks/* @| count");
    assert_eq!(
        t("SELECT customer, SUM(qty) AS total FROM invoices GROUP BY customer HAVING total > 1"),
        "/invoices/* | ::qty @| group(::customer) | sum | .total | [$_ > 1] | %."
    );
    assert_eq!(
        t(
            "SELECT al.title, t.title FROM albums al JOIN tracks t ON t.album_id = al.id \
           WHERE t.secs > 400"
        ),
        "/albums/* <=> /tracks/*[::album_id = $*1::id and ::secs > 400] \
         | rec(\"al.title\", $*1::title, ::title)"
    );
    assert_eq!(
        t("SELECT DISTINCT country FROM artists"),
        "/artists/* | ::country @| unique"
    );
    assert_eq!(
        t("SELECT title FROM tracks WHERE title LIKE '%o%' AND secs >= 200"),
        "/tracks/*[::title =~ /(?i)o/ and ::secs >= 200] | rec(::title)"
    );
    assert_eq!(
        t("SELECT name FROM artists WHERE country IS NOT NULL"),
        "/artists/*[::country] | rec(::name)"
    );
}

#[test]
fn refusals() {
    let err = |sql: &str| translate(sql).unwrap_err().to_string();
    assert!(err("DELETE FROM tracks").contains("only SELECT"));
    assert!(
        err("SELECT a FROM t1 JOIN t2 ON t2.x = t1.x JOIN t3 ON t3.y = t2.y")
            .contains("more than one JOIN")
    );
    assert!(err("SELECT title FROM tracks WHERE title LIKE 'a%b'").contains("LIKE pattern"));
    assert!(err("SELECT a, COUNT(*) FROM t").contains("mixing aggregates"));
}

/// The differential rig: SQL through SQLite, the translation
/// through the engine, same database, same answers.
#[test]
fn differential_against_sqlite() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE artists (id INTEGER PRIMARY KEY, name TEXT, country TEXT);
        CREATE TABLE tracks (id INTEGER PRIMARY KEY, title TEXT, secs INTEGER,
                             price REAL, artist_id INTEGER REFERENCES artists(id));
        INSERT INTO artists VALUES (1,'Holst','GB'), (2,'Bartok','HU'), (3,'Satie',NULL);
        INSERT INTO tracks VALUES
          (1,'Mars',430,1.29,1), (2,'Venus',480,0.99,1), (3,'Jupiter',470,1.29,1),
          (4,'Bourree',95,0.99,2), (5,'Gymnopedie',210,0.99,3), (6,'Sketch',60,0.5,NULL);
        "#,
    )
    .unwrap();
    let adapter = quarb_sqlite::SqliteAdapter::load(&conn).unwrap();

    // Scalar-column cases: compare flattened value lists.
    let cases = [
        "SELECT title FROM tracks WHERE price < 1 ORDER BY title",
        "SELECT name FROM artists WHERE country IS NOT NULL ORDER BY name",
        "SELECT title FROM tracks WHERE title LIKE '%s%' ORDER BY title",
        "SELECT DISTINCT price FROM tracks ORDER BY price",
        "SELECT title FROM tracks WHERE secs > 100 AND price >= 1 ORDER BY title",
        "SELECT name FROM artists WHERE country IN ('GB','HU') ORDER BY name",
    ];
    for sql in cases {
        // SQL side.
        let mut stmt = conn.prepare(sql).unwrap();
        let ncols = stmt.column_count();
        let sql_rows: Vec<String> = stmt
            .query_map([], |r| {
                Ok((0..ncols)
                    .map(|i| {
                        r.get_ref(i)
                            .map(|v| match v {
                                rusqlite::types::ValueRef::Null => String::new(),
                                rusqlite::types::ValueRef::Integer(n) => n.to_string(),
                                rusqlite::types::ValueRef::Real(f) => f.to_string(),
                                rusqlite::types::ValueRef::Text(t) => {
                                    String::from_utf8_lossy(t).into_owned()
                                }
                                _ => "<blob>".into(),
                            })
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        // Quarb side: single-column rec streams as {"col": v} — use
        // the projected value form for comparison.
        let quarb = translate(sql).unwrap().query;
        let got = match quarb::run(&quarb, &adapter).unwrap() {
            quarb::QueryResult::Values(vs) => vs,
            quarb::QueryResult::Nodes(_) => panic!("expected values for {sql}"),
        };
        let quarb_rows: Vec<String> = got
            .iter()
            .map(|v| match v {
                quarb::Value::Record(fields) => fields
                    .iter()
                    .map(|(_, v)| v.to_string())
                    .collect::<Vec<_>>()
                    .join("|"),
                other => other.to_string(),
            })
            .collect();
        assert_eq!(quarb_rows, sql_rows, "differential mismatch for: {sql}");
    }
}

// ---------------------------------------------------------------
// The exporter: Quarb → SQL, and its own differential rig.
// ---------------------------------------------------------------

use quarb_sql::export;

fn x(quarb: &str) -> String {
    export(quarb).unwrap().query
}

#[test]
fn export_translations() {
    assert_eq!(
        x("/tracks/*[::price < 1] | rec(::title, ::secs) @| sort_by(::title)"),
        "SELECT title, secs FROM tracks WHERE price < 1 ORDER BY title"
    );
    assert_eq!(x("/tracks/* @| count"), "SELECT COUNT(*) FROM tracks");
    assert_eq!(
        x("/invoices/* | ::qty @| group(::customer) | sum | .total | [$_ > 1] | %."),
        "SELECT customer, SUM(qty) AS total FROM invoices GROUP BY customer \
         HAVING SUM(qty) > 1"
    );
    assert_eq!(
        x(
            "/albums/* <=> /tracks/*[::album_id = $*1::id and ::secs > 400] \
           | rec(\"album\", $*1::title, ::title)"
        ),
        "SELECT albums.title AS album, tracks.title FROM albums \
         JOIN tracks ON tracks.album_id = albums.id WHERE tracks.secs > 400"
    );
    assert_eq!(
        x("/tracks/* | ::price @| unique"),
        "SELECT DISTINCT price FROM tracks"
    );
    assert_eq!(
        x("/tracks/* @| top(2, ::price) | rec(::title, ::price)"),
        "SELECT title, price FROM tracks ORDER BY price DESC LIMIT 2"
    );
}

#[test]
fn export_refusals() {
    let err = |q: &str| export(q).unwrap_err().to_string();
    assert!(err("/tracks/* | rec(::album_id~>::title)").contains("schema"));
    assert!(err("/tracks/* | ::price @| window(3) | mean").contains("aggregate"));
    assert!(err("/a/*[::x =~ /y/]").contains("regex"));
    assert!(err("//tracks/*::title").contains("/table/*"));
}

/// Round-trip differential: the Quarb query through the engine, its
/// exported SQL through SQLite, same database, same answers.
#[test]
fn export_differential_against_sqlite() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE tracks (id INTEGER PRIMARY KEY, title TEXT, secs INTEGER, price REAL);
        INSERT INTO tracks VALUES
          (1,'Mars',430,1.29), (2,'Venus',480,0.99), (3,'Jupiter',470,1.29),
          (4,'Bourree',95,0.99), (5,'Gymnopedie',210,0.5);
        "#,
    )
    .unwrap();
    let adapter = quarb_sqlite::SqliteAdapter::load(&conn).unwrap();

    let cases = [
        "/tracks/*[::price < 1] | ::title @| sort_by(::title)",
        "/tracks/* @| count",
        "/tracks/*[::secs > 100 and ::price >= 1] | ::title @| sort_by(::title)",
        "/tracks/* | ::price @| sum",
        "/tracks/* @| top(2, ::secs) | rec(::title)",
    ];
    for quarb in cases {
        let sql = export(quarb).unwrap().query;
        let mut stmt = conn.prepare(&sql).unwrap();
        let ncols = stmt.column_count();
        let sql_rows: Vec<String> = stmt
            .query_map([], |r| {
                Ok((0..ncols)
                    .map(|i| {
                        r.get_ref(i)
                            .map(|v| match v {
                                rusqlite::types::ValueRef::Null => String::new(),
                                rusqlite::types::ValueRef::Integer(n) => n.to_string(),
                                rusqlite::types::ValueRef::Real(f) => f.to_string(),
                                rusqlite::types::ValueRef::Text(t) => {
                                    String::from_utf8_lossy(t).into_owned()
                                }
                                _ => "<blob>".into(),
                            })
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let got = match quarb::run(quarb, &adapter).unwrap() {
            quarb::QueryResult::Values(vs) => vs,
            quarb::QueryResult::Nodes(_) => panic!("expected values for {quarb}"),
        };
        let quarb_rows: Vec<String> = got
            .iter()
            .map(|v| match v {
                quarb::Value::Record(fields) => fields
                    .iter()
                    .map(|(_, v)| v.to_string())
                    .collect::<Vec<_>>()
                    .join("|"),
                other => other.to_string(),
            })
            .collect();
        assert_eq!(
            quarb_rows, sql_rows,
            "export differential mismatch for: {quarb}"
        );
    }
}

#[test]
fn pushdown_gate() {
    use quarb_sql::pushdown;
    // In: aggregates, filtered selections, witness joins.
    assert!(pushdown("/tracks/* @| count").is_some());
    assert!(pushdown("/tracks/*[::price < 1] | rec(::title)").is_some());
    assert!(
        pushdown("/albums/* <=> /tracks/*[::album_id = $*1::id] | rec($*1::title, ::title)")
            .is_some()
    );
    // Aggregates carry no order table; row selections order by the
    // result context's table.
    assert!(
        pushdown("/tracks/* @| count")
            .unwrap()
            .order_table
            .is_none()
    );
    assert_eq!(
        pushdown("/tracks/*[::price < 1] | rec(::title)")
            .unwrap()
            .order_table
            .as_deref(),
        Some("tracks")
    );
    // Out: every construct whose SQL semantics are not provably
    // identical.
    assert!(pushdown("/tracks/*[::title *= \"o\"] | ::title").is_none()); // LIKE folding
    assert!(pushdown("/tracks/*[::album_id] | ::title").is_none()); // truthiness
    assert!(pushdown("/t/* | ::p @| group(::k) | sum | .s | %.").is_none()); // group order
    assert!(pushdown("/t/* | ::p @| unique").is_none()); // distinct order
    assert!(pushdown("/t/* @| sort_by(::a) | ::a").is_none()); // collation
    assert!(pushdown("/t/* @| [..3]").is_none()); // unordered LIMIT
    assert!(pushdown("/t/* | ::p @| window(3) | mean").is_none()); // quarb-side
}

#[test]
fn partial_pushdown_gate() {
    use quarb_sql::partial_pushdown;
    // In: a strict leading predicate with an unpushable pipeline.
    let p = partial_pushdown("/events/*[::kind = \"rare\"] | ::amount @| group(\"b\", ::amount idiv 100) | count | .n | %.").unwrap();
    assert_eq!(p.table, "events");
    assert_eq!(p.where_sql, "kind = 'rare'");
    // Only the LEADING expression run pushes; positional first = out.
    assert!(
        partial_pushdown("/t/*[2][::a = 1] | ::a @| group(\"g\", ::a) | count | .n | %.").is_none()
    );
    // Reaching the table twice (a ^-anchored subcontext) = out.
    assert!(partial_pushdown("/t/*[::a = 1] | .n(^/t/* @| count) | $.n").is_none());
    // Crosslink/resolution axes = out.
    assert!(partial_pushdown("/t/*[::a = 1]::b~>::c").is_none());
    // Metadata = out (a filtered ::;n-rows would lie).
    assert!(partial_pushdown("/t/*[::a = 1] | ::;table").is_none());
    // Non-strict predicates (LIKE folding) = out.
    assert!(
        partial_pushdown("/t/*[::a *= \"x\"] | ::a @| group(\"g\", ::a) | count | .n | %.")
            .is_none()
    );
}

#[test]
fn pushdown_join_soundness_metadata() {
    use quarb_sql::pushdown;
    // A witness join carries its left-binding columns so the
    // driver can verify uniqueness (SQL JOIN multiplies rows when
    // the ON binds the left side by a non-key column; Quarb's
    // existential binding never does).
    let p = pushdown("/albums/* <=> /tracks/*[::album_id = $*1::id] | rec(\"a\", $*1::title)")
        .expect("canonical witness join pushes down");
    let (table, cols) = p.join_left.expect("join carries its obligation");
    assert_eq!(table, "albums");
    assert_eq!(cols, vec!["id".to_string()]);
    // No join → no obligation.
    assert!(
        pushdown("/tracks/* @| count")
            .expect("plain aggregate pushes down")
            .join_left
            .is_none()
    );
}

#[test]
fn pushdown_refuses_keyword_aliases() {
    use quarb_sql::pushdown_explained;
    // `AS order` is an SQL syntax error; quoting portably differs
    // by dialect, so strict pushdown refuses outright.
    let Err(err) = pushdown_explained(
        "/albums/* <=> /tracks/*[::album_id = $*1::id] | rec(\"order\", $*1::title)",
    ) else {
        panic!("keyword alias must refuse")
    };
    assert!(format!("{err}").contains("needs SQL quoting"), "{err}");
}

#[test]
fn refuses_the_internal_left_marker() {
    use quarb_sql::{export, pushdown, pushdown_explained};
    // "__LEFT__" is the exporter's internal placeholder for the
    // join's left table: query text containing it would be
    // rewritten inside its own literals and could spoof the join
    // obligation, so it stays on the scan path.
    let q = "/albums/* <=> /tracks/*[::note = '__LEFT__.id'] | rec($*1::title)";
    assert!(pushdown(q).is_none());
    let Err(err) = pushdown_explained(q) else {
        panic!("marker in a literal must refuse")
    };
    assert!(format!("{err}").contains("__LEFT__"), "{err}");
    assert!(export(q).is_err(), "export substitutes too; must refuse");
}

#[test]
fn join_projections_qualify_by_operand_index() {
    use quarb_sql::{pushdown, pushdown_explained};
    // `$*1` projects the left/FROM table, `$*2` the joined one —
    // found by the seams article: every context used to render as
    // the left table, so `$*2::col` compiled to invalid SQL that
    // only the driver's runtime refusal caught.
    let p = pushdown(
        "/albums/* <=> /tracks/*[::album_id = $*1::id] \
         | rec(\"a\", $*1::title, \"t\", $*2::title)",
    )
    .expect("two-sided projection pushes down");
    assert!(p.sql.contains("albums.title AS a"), "{}", p.sql);
    assert!(p.sql.contains("tracks.title AS t"), "{}", p.sql);
    // Beyond the two operands there is no verified mapping.
    assert!(
        pushdown_explained(
            "/albums/* <=> /tracks/*[::album_id = $*1::id] | rec(\"x\", $*3::title)"
        )
        .is_err()
    );
}
