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
        "/tracks/*[::price < 1] @| sort_by(::title) | %(::title; ::price)"
    );
    assert_eq!(t("SELECT COUNT(*) FROM tracks"), "/tracks/* @| count");
    assert_eq!(
        t("SELECT customer, SUM(qty) AS total FROM invoices GROUP BY customer HAVING total > 1"),
        "/invoices/* | .customer(::customer) | ::qty @|{.customer} sum | .total | [$_ > 1] | %."
    );
    assert_eq!(
        t(
            "SELECT al.title, t.title FROM albums al JOIN tracks t ON t.album_id = al.id \
           WHERE t.secs > 400"
        ),
        "/albums/* <=>* /tracks/*[::album_id = $$0::id && ::secs > 400] \
         | %(::title; \"t.title\" = $$1::title)"
    );
    assert_eq!(
        t("SELECT DISTINCT country FROM artists"),
        "/artists/* | ::country @| unique"
    );
    assert_eq!(
        t("SELECT title FROM tracks WHERE title LIKE '%o%' AND secs >= 200"),
        "/tracks/*[::title == (/(?i)o/) && ::secs >= 200] | %(::title)"
    );
    // `IS [NOT] NULL` → `= null` / `!= null`: exact under Quarb's
    // value_eq (truthiness would also drop 0 and '').
    assert_eq!(
        t("SELECT name FROM artists WHERE country IS NOT NULL"),
        "/artists/*[::country != null] | %(::name)"
    );
    assert_eq!(
        t("SELECT name FROM artists WHERE country IS NULL"),
        "/artists/*[::country = null] | %(::name)"
    );
    // DISTINCT dedups before LIMIT (SQL's order of operations).
    assert_eq!(
        t("SELECT DISTINCT country FROM artists ORDER BY country LIMIT 3"),
        "/artists/* @| sort_by(::country) | ::country @| unique @| [..3]"
    );
    // `AS` table aliases.
    assert_eq!(
        t("SELECT t.title FROM tracks AS t WHERE t.secs > -10"),
        "/tracks/*[::secs > -10] | %(::title)"
    );
    // HAVING may name the aggregate by call, alias, or function.
    assert_eq!(
        t(
            "SELECT customer, SUM(qty) AS total FROM invoices GROUP BY customer \
           HAVING SUM(qty) > 1"
        ),
        "/invoices/* | .customer(::customer) | ::qty @|{.customer} sum | .total | [$_ > 1] | %."
    );
    // An aliased group key names the key field.
    assert_eq!(
        t("SELECT country AS c, COUNT(*) FROM artists GROUP BY country"),
        "/artists/* | .c(::country) @|{.c} count | .count | %."
    );
    // An outer join keeps the driver's rows with no pair, and its
    // WHERE reads the joined rows, so it stands after the join.
    assert_eq!(
        t(
            "SELECT a.name, t.title FROM artists a LEFT JOIN tracks t ON t.artist_id = a.id \
           WHERE t.secs IS NULL"
        ),
        "/artists/* <=>*? /tracks/*[::artist_id = $$0::id] | [$$1::secs = null] \
         | %(::name; \"t.title\" = $$1::title)"
    );
    // RIGHT is LEFT with the tables' places exchanged.
    assert_eq!(
        t("SELECT a.name, t.title FROM tracks t RIGHT JOIN artists a ON t.artist_id = a.id"),
        "/artists/* <=>*? /tracks/*[::artist_id = $$0::id] | %(::name; \"t.title\" = $$1::title)"
    );
    // Every pair, and a join on any condition.
    assert_eq!(
        t("SELECT a.name, t.title FROM artists a CROSS JOIN tracks t"),
        "/artists/* <=>* /tracks/* | %(::name; \"t.title\" = $$1::title)"
    );
    assert_eq!(
        t("SELECT a.name, t.title FROM artists a JOIN tracks t ON t.artist_id > a.id"),
        "/artists/* <=>* /tracks/*[::artist_id > $$0::id] | %(::name; \"t.title\" = $$1::title)"
    );
    // SQL's third truth value: a comparison with a NULL is neither
    // true nor false, so `<>` is guarded and a negation is carried
    // inward to the comparisons.
    assert_eq!(
        t("SELECT name FROM artists WHERE country <> 'GB'"),
        "/artists/*[::country != null && ::country != \"GB\"] | %(::name)"
    );
    assert_eq!(
        t("SELECT title FROM tracks WHERE NOT (artist_id > 1 AND secs > 100)"),
        "/tracks/*[(::artist_id <= 1 || ::secs <= 100)] | %(::title)"
    );
    assert_eq!(
        t("SELECT name FROM artists WHERE country = 'GB' OR country = 'HU'"),
        "/artists/*[(::country = \"GB\" || ::country = \"HU\")] | %(::name)"
    );
    assert_eq!(
        t("SELECT name FROM artists WHERE country NOT IN ('GB', 'HU')"),
        "/artists/*[::country != null && ::country != \"GB\" && ::country != \"HU\"] | %(::name)"
    );
    // Any LIKE pattern: anchored at both ends, `%` a run and `_` one
    // character; the simple forms keep their short spelling.
    assert_eq!(
        t("SELECT title FROM tracks WHERE title LIKE 'M_r%s'"),
        "/tracks/*[::title == (/(?is)^M.r.*s$/)] | %(::title)"
    );
    assert_eq!(
        t("SELECT title FROM tracks WHERE title NOT LIKE '%s'"),
        "/tracks/*[::title != null && !(::title == (/(?i)s$/))] | %(::title)"
    );
    // DISTINCT over several columns: the rows as records.
    assert_eq!(
        t("SELECT DISTINCT price, artist_id FROM tracks ORDER BY price LIMIT 3"),
        "/tracks/* @| sort_by(::price) | %(::price; ::artist_id) @| unique @| [..3]"
    );
    // UNION ALL of two tables is a branch union; UNION removes the
    // duplicate values.
    assert_eq!(
        t("SELECT name FROM artists UNION ALL SELECT title FROM tracks WHERE secs > 400"),
        "/artists/*::name || /tracks/*[::secs > 400]::title"
    );
    assert_eq!(
        t(
            "SELECT country FROM artists UNION SELECT title FROM tracks \
           ORDER BY country DESC LIMIT 2"
        ),
        "/artists/*::country || /tracks/*::title @| unique @| sort @| reverse @| [..2]"
    );
    // SQL strings escape into Quarb string syntax.
    assert_eq!(
        t("SELECT a FROM t WHERE a = 'say \"hi\" for $5'"),
        "/t/*[::a = \"say \\\"hi\\\" for \\$5\"] | %(::a)"
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
    assert!(err("SELECT a, COUNT(*) FROM t").contains("mixing aggregates"));
    // A FULL join keeps the unmatched rows of both tables: no
    // correlation does. A CROSS JOIN has no ON.
    assert!(err("SELECT t.a FROM t FULL JOIN u ON u.x = t.x").contains("FULL JOIN"));
    assert!(err("SELECT t.a FROM t FULL OUTER JOIN u ON u.x = t.x").contains("FULL JOIN"));
    assert!(err("SELECT t.a FROM t CROSS JOIN u ON u.x = t.x").contains("CROSS JOIN with an ON"));
    // A row node is one table's row: `*` over a join is refused.
    assert!(err("SELECT * FROM t JOIN u ON u.x = t.x").contains("SELECT * over a JOIN"));
    // A comparison with NULL is never true; a NOT IN over a NULL
    // never holds.
    assert!(err("SELECT a FROM t WHERE a = NULL").contains("comparison with NULL"));
    assert!(
        err("SELECT a FROM t WHERE a NOT IN (1, NULL)").contains("NOT IN over a list with NULL")
    );
    // UNION ALL over one table twice would lose the rows both take;
    // a UNION joins single columns.
    assert!(
        err("SELECT a FROM t WHERE a > 1 UNION ALL SELECT a FROM t WHERE a < 9")
            .contains("UNION ALL over the table 't' twice")
    );
    assert!(err("SELECT a, b FROM t UNION SELECT a, b FROM u").contains("single columns"));
    assert!(
        err("SELECT a FROM t UNION SELECT b FROM u ORDER BY b").contains("ORDER BY of a UNION")
    );
    // HAVING binds to a group.
    assert!(err("SELECT name FROM t HAVING name > 5").contains("HAVING without GROUP BY"));
    assert!(err("SELECT SUM(x) FROM t HAVING x > 5").contains("HAVING without GROUP BY"));
    // Every plain select item must be the GROUP BY key.
    assert!(err("SELECT name, COUNT(*) FROM emp GROUP BY dept").contains("not the GROUP BY key"));
    // HAVING may only name the select list's aggregate or the key.
    assert!(
        err("SELECT k, SUM(x) FROM t GROUP BY k HAVING COUNT(*) > 1")
            .contains("not in the select list")
    );
    assert!(err("SELECT k, SUM(x) FROM t GROUP BY k HAVING z > 1").contains("HAVING column"));
    // Aggregates belong in HAVING, not WHERE.
    assert!(err("SELECT a FROM t WHERE SUM(x) > 5").contains("aggregate in WHERE"));
    assert!(err("SELECT DISTINCT COUNT(*) FROM t").contains("DISTINCT"));
    assert!(err("SELECT \"a FROM t").contains("unterminated"));
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
        INSERT INTO artists VALUES
          (1,'Holst','GB'), (2,'Bartok','HU'), (3,'Satie',NULL), (4,'Anon','');
        INSERT INTO tracks VALUES
          (1,'Mars',430,1.29,1), (2,'Venus',480,0.99,1), (3,'Jupiter',470,1.29,1),
          (4,'Bourree',95,0.99,2), (5,'Gymnopedie',210,0.99,3), (6,'Sketch',60,0.5,NULL);
        "#,
    )
    .unwrap();
    let adapter = quarb_sqlite::SqliteAdapter::load(&conn).unwrap();

    // Scalar-column cases: compare flattened value lists. The ''
    // country row keeps the NULL translations exact — SQL's
    // IS [NOT] NULL distinguishes '' from NULL, and so must the
    // `= null` / `!= null` forms.
    let cases = [
        "SELECT title FROM tracks WHERE price < 1 ORDER BY title",
        "SELECT name FROM artists WHERE country IS NOT NULL ORDER BY name",
        "SELECT name FROM artists WHERE country IS NULL ORDER BY name",
        "SELECT name FROM artists WHERE country = '' ORDER BY name",
        "SELECT title FROM tracks WHERE title LIKE '%s%' ORDER BY title",
        "SELECT DISTINCT price FROM tracks ORDER BY price",
        "SELECT DISTINCT price FROM tracks ORDER BY price LIMIT 2",
        "SELECT title FROM tracks WHERE secs > 100 AND price >= 1 ORDER BY title",
        "SELECT title FROM tracks WHERE secs > -1 AND price < 1 ORDER BY title",
        "SELECT name FROM artists WHERE country IN ('GB','HU') ORDER BY name",
        "SELECT price, COUNT(*) AS n FROM tracks GROUP BY price ORDER BY price",
        // SQL's third truth value: Satie's country and Sketch's
        // artist are NULL, and neither passes a negated comparison
        "SELECT name FROM artists WHERE country <> 'GB' ORDER BY name",
        "SELECT name FROM artists WHERE NOT (country = 'GB') ORDER BY name",
        "SELECT name FROM artists WHERE country NOT IN ('GB', 'HU') ORDER BY name",
        "SELECT name FROM artists WHERE country IN ('GB', NULL) ORDER BY name",
        "SELECT title FROM tracks WHERE NOT (artist_id > 1) ORDER BY title",
        "SELECT title FROM tracks WHERE NOT (artist_id > 1 AND secs > 100) ORDER BY title",
        "SELECT title FROM tracks WHERE NOT (artist_id > 1 OR secs > 100) ORDER BY title",
        "SELECT title FROM tracks WHERE NOT (NOT (artist_id = 1)) ORDER BY title",
        "SELECT title FROM tracks WHERE artist_id = 1 OR price < 0.9 ORDER BY title",
        "SELECT title FROM tracks WHERE NOT (artist_id IS NULL OR secs < 100) ORDER BY title",
        "SELECT title FROM tracks WHERE id <> artist_id ORDER BY title",
        "SELECT title FROM tracks WHERE id = artist_id ORDER BY title",
        "SELECT title FROM tracks WHERE NOT (id = artist_id) ORDER BY title",
        // any LIKE pattern, and its negation
        "SELECT title FROM tracks WHERE title LIKE 'M_r%' ORDER BY title",
        "SELECT title FROM tracks WHERE title LIKE '%e%s' ORDER BY title",
        "SELECT title FROM tracks WHERE title LIKE '_e%' ORDER BY title",
        "SELECT title FROM tracks WHERE title NOT LIKE '%s' ORDER BY title",
        "SELECT name FROM artists WHERE country NOT LIKE 'g%' ORDER BY name",
        // DISTINCT over several columns
        "SELECT DISTINCT price, artist_id FROM tracks ORDER BY price",
        "SELECT DISTINCT price, artist_id FROM tracks ORDER BY price LIMIT 3",
        // aggregates skip the NULLs; COUNT(col) counts the values
        "SELECT COUNT(artist_id) FROM tracks",
        "SELECT COUNT(*) FROM tracks",
        "SELECT SUM(artist_id) FROM tracks",
        "SELECT MIN(artist_id) FROM tracks",
        "SELECT MAX(artist_id) FROM tracks",
        "SELECT price, COUNT(artist_id) AS n FROM tracks GROUP BY price ORDER BY price",
        "SELECT price, COUNT(artist_id) FROM tracks GROUP BY price ORDER BY price",
        "SELECT price, SUM(artist_id) AS s FROM tracks GROUP BY price ORDER BY price",
        "SELECT COUNT(*) FROM tracks WHERE secs > 9999",
        "SELECT COUNT(artist_id) FROM tracks WHERE secs > 9999",
        "SELECT AVG(secs) FROM tracks",
        "SELECT MAX(secs) FROM tracks WHERE price < 1",
        // a branch union of two tables; UNION removes duplicates
        "SELECT name FROM artists UNION ALL SELECT title FROM tracks WHERE secs > 400 ORDER BY name",
        "SELECT country FROM artists WHERE country IS NOT NULL UNION SELECT title FROM tracks ORDER BY 1 DESC LIMIT 3",
        "SELECT price FROM tracks WHERE secs > 400 UNION SELECT price FROM tracks WHERE price < 1 ORDER BY price",
    ];

    // Joins: SQL leaves the order of the rows to the engine where
    // no key decides it, so the two answers are compared as
    // multisets. Every pair of an inner join is a row (Holst has
    // three tracks); an outer join keeps the rows with no pair.
    let unordered = [
        "SELECT a.name, t.title FROM artists a JOIN tracks t ON t.artist_id = a.id",
        "SELECT a.name, t.title FROM artists a INNER JOIN tracks t ON t.artist_id = a.id WHERE t.secs > 400",
        "SELECT a.name, t.title FROM artists a LEFT JOIN tracks t ON t.artist_id = a.id",
        "SELECT a.name, t.title FROM artists a LEFT OUTER JOIN tracks t ON t.artist_id = a.id WHERE t.secs IS NULL",
        "SELECT a.name, t.title FROM artists a LEFT JOIN tracks t ON t.artist_id = a.id WHERE t.secs > 100",
        "SELECT a.name, t.title FROM artists a LEFT JOIN tracks t ON t.artist_id = a.id AND t.secs > 450",
        "SELECT t.title, a.name FROM artists a RIGHT JOIN tracks t ON t.artist_id = a.id",
        "SELECT a.name, COUNT(*) AS n FROM artists a JOIN tracks t ON t.artist_id = a.id GROUP BY a.name",
        "SELECT a.name, COUNT(t.id) AS n FROM artists a LEFT JOIN tracks t ON t.artist_id = a.id GROUP BY a.name",
        "SELECT COUNT(*) FROM artists a CROSS JOIN tracks t",
        "SELECT COUNT(*) FROM artists a JOIN tracks t ON t.artist_id > a.id",
        "SELECT COUNT(*) FROM artists a JOIN tracks t ON t.artist_id = a.id OR t.secs < 100",
    ];
    let all = cases
        .iter()
        .map(|c| (*c, true))
        .chain(unordered.iter().map(|c| (*c, false)));
    for (sql, ordered) in all {
        // SQL side.
        let mut stmt = conn.prepare(sql).unwrap();
        let ncols = stmt.column_count();
        let mut sql_rows: Vec<String> = stmt
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
        let ran = quarb::run(&quarb, &adapter)
            .unwrap_or_else(|e| panic!("{sql}\n  translated: {quarb}\n  {e}"));
        let got = match ran {
            quarb::QueryResult::Values(vs) => vs,
            quarb::QueryResult::Nodes(_) => panic!("expected values for {sql}"),
        };
        let mut quarb_rows: Vec<String> = got
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
        if !ordered {
            sql_rows.sort();
            quarb_rows.sort();
        }
        assert_eq!(
            quarb_rows, sql_rows,
            "differential mismatch for: {sql}\n  translated: {quarb}"
        );
    }
}

// ---------------------------------------------------------------
// The exporter: Quarb → SQL, and its own differential rig.
// ---------------------------------------------------------------

/// The plan's answer beside the scan's over NULLs, for the
/// comparisons where SQL's three-valued logic and Quarb's two-valued
/// one part: `!=`, a negation, and an equality of two columns (two
/// NULLs are equal in Quarb). The first two were refused; the third
/// was pushed as the bare `a = b` and lost the row where both are
/// NULL (to 0.68.0).
#[test]
fn the_plan_follows_the_scan_over_nulls() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a INTEGER, b INTEGER, s TEXT);
         INSERT INTO t VALUES (1,1,1,'x'), (2,1,2,'y'), (3,NULL,NULL,'z'), (4,NULL,2,NULL),
           (5,3,NULL,'x');",
    )
    .unwrap();
    let adapter = quarb_sqlite::SqliteAdapter::load(&conn).unwrap();
    for q in [
        "/t/*[::a = ::b]::id",
        "/t/*[::a != ::b]::id",
        "/t/*[::a != 1]::id",
        "/t/*[::s != \"x\"]::id",
        "/t/*[!(::a = 1)]::id",
        "/t/*[!(::a > 1)]::id",
        "/t/*[!(::a = ::b)]::id",
        "/t/*[!(::a != ::b)]::id",
        "/t/*[!(::a = 1 && ::b = 1)]::id",
        "/t/*[!(::a = 1 || ::b = 2)]::id",
        "/t/*[!(!(::a = 1))]::id",
        "/t/*[::a != 1 && ::b != 2]::id",
        "/t/*[::a < ::b]::id",
        "/t/*[::a <= ::b]::id",
        "/t/*[!(::a < ::b)] @| count",
        // a substring test, by bytes and by case: SQLite's instr
        "/t/*[::s *= \"x\"]::id",
        "/t/*[::s *= \"X\"] @| count",
        "/t/*[!(::s *= \"x\")]::id",
        "/t/*[::s *= \"\"]::id",
        // a count of the distinct values: a null is one of them
        "/t/* | ::a @| unique @| count",
        "/t/*::b @| unique @| count",
        "/t/*[::a = 1] | ::b @| unique @| count",
        "/t/*[::id > 99] | ::a @| unique @| count",
    ] {
        let scan: Vec<String> = match quarb::run(q, &adapter).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
            quarb::QueryResult::Nodes(_) => panic!("values expected for {q}"),
        };
        let plan = quarb_sql::pushdown_explained(q, Some(quarb_sql::Dialect::Sqlite))
            .unwrap_or_else(|e| panic!("{q} is not pushed: {e}"));
        let sql = match &plan.order_table {
            Some(t) => format!("{} ORDER BY {t}.id", plan.sql),
            None => plan.sql.clone(),
        };
        let mut stmt = conn.prepare(&sql).unwrap();
        let pushed: Vec<String> = stmt
            .query_map([], |r| r.get::<_, i64>(0).map(|v| v.to_string()))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(pushed, scan, "{q}\n  plan: {sql}");
    }
}

/// A pushed reducer answers as the scan does under its options. The
/// plan read the reducer's name and none of its arguments:
/// `count(--values)` and `count(--nulls)` over a column with a NULL
/// were both answered as COUNT(*), six where the scan counts five
/// and one (in every release that pushed a count, to 0.68.0).
#[test]
fn a_pushed_reducer_reads_its_options() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE tracks (id INTEGER PRIMARY KEY, title TEXT, price REAL,
                              artist_id INTEGER);
         INSERT INTO tracks VALUES
           (1,'Mars',1.29,1), (2,'Venus',0.99,1), (3,'Jupiter',1.29,1),
           (4,'Bourree',0.99,2), (5,'Gymnopedie',0.99,3), (6,'Sketch',0.5,NULL);",
    )
    .unwrap();
    let adapter = quarb_sqlite::SqliteAdapter::load(&conn).unwrap();
    let scan = |q: &str| match quarb::run(q, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(_) => panic!("values expected for {q}"),
    };
    let pushed = |q: &str| -> Option<Vec<String>> {
        let plan = quarb_sql::pushdown(q, Some(quarb_sql::Dialect::Sqlite))?;
        let mut stmt = conn.prepare(&plan.sql).unwrap();
        let n = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                Ok((0..n)
                    .map(|i| match r.get_ref(i).unwrap() {
                        rusqlite::types::ValueRef::Integer(v) => v.to_string(),
                        rusqlite::types::ValueRef::Real(v) => v.to_string(),
                        rusqlite::types::ValueRef::Null => "null".to_string(),
                        other => format!("{other:?}"),
                    })
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        Some(rows)
    };
    // the bare count is every row, a column projected or not
    assert_eq!(scan("/tracks/* | ::artist_id @| count"), ["6"]);
    assert_eq!(pushed("/tracks/* | ::artist_id @| count").unwrap(), ["6"]);
    // the values other than null: the plan says COUNT(col)
    assert_eq!(scan("/tracks/* | ::artist_id @| count(--values)"), ["5"]);
    assert_eq!(
        pushed("/tracks/* | ::artist_id @| count(--values)").unwrap(),
        ["5"]
    );
    assert_eq!(scan("/tracks/*::artist_id @| count(--values)"), ["5"]);
    assert_eq!(
        pushed("/tracks/*::artist_id @| count(--values)").unwrap(),
        ["5"]
    );
    // a row node is no value: that count is the engine's to say
    assert_eq!(scan("/tracks/* @| count(--values)"), ["0"]);
    assert_eq!(pushed("/tracks/* @| count(--values)"), None);
    // the nulls, and any other argument, are the engine's to count
    assert_eq!(scan("/tracks/* | ::artist_id @| count(--nulls)"), ["1"]);
    assert_eq!(pushed("/tracks/* | ::artist_id @| count(--nulls)"), None);
    assert_eq!(pushed("/tracks/* | ::price @| sum(--of=::artist_id)"), None);
}

use quarb_sql::export;

fn x(quarb: &str) -> String {
    export(quarb).unwrap().query
}

#[test]
fn export_translations() {
    assert_eq!(
        x("/tracks/*[::price < 1] | %(::title; ::secs) @| sort_by(::title)"),
        "SELECT title, secs FROM tracks WHERE price < 1 ORDER BY title"
    );
    assert_eq!(x("/tracks/* @| count"), "SELECT COUNT(*) FROM tracks");
    // a reducer's own arguments decide what is reduced: the bare
    // count is every row, a projected column or none; `--values`
    // counts the values other than null, which is COUNT(col)
    assert_eq!(
        x("/tracks/* | ::artist_id @| count"),
        "SELECT COUNT(*) FROM tracks"
    );
    assert_eq!(
        x("/tracks/* | ::artist_id @| count(--values)"),
        "SELECT COUNT(artist_id) FROM tracks"
    );
    assert_eq!(
        x("/tracks/* | .price(::price) | ::artist_id @|{.price} count(--values) | .n | %."),
        "SELECT price, COUNT(artist_id) AS n FROM tracks GROUP BY price"
    );
    // the fold's answer unrecorded: one value per group
    assert_eq!(
        x("/tracks/* | .price(::price) @|{.price} count"),
        "SELECT COUNT(*) FROM tracks GROUP BY price"
    );
    assert_eq!(
        x("/invoices/* | .customer(::customer) | ::qty @|{.customer} sum | .total | [$_ > 1] | %."),
        "SELECT customer, SUM(qty) AS total FROM invoices GROUP BY customer \
         HAVING SUM(qty) > 1"
    );
    assert_eq!(
        x(
            "/albums/* <=> /tracks/*[::album_id = $$0::id && ::secs > 400] \
           | %(album = ::title; $$1::title)"
        ),
        "SELECT albums.title AS album, tracks.title FROM albums \
         JOIN tracks ON tracks.album_id = albums.id WHERE tracks.secs > 400"
    );
    assert_eq!(
        x("/tracks/* | ::price @| unique"),
        "SELECT DISTINCT price FROM tracks"
    );
    assert_eq!(
        x("/tracks/* @| top(2, ::price) | %(::title; ::price)"),
        "SELECT title, price FROM tracks ORDER BY price DESC LIMIT 2"
    );
}

#[test]
fn export_refusals() {
    let err = |q: &str| export(q).unwrap_err().to_string();
    assert!(err("/tracks/* | %(::album_id-->::title)").contains("schema"));
    assert!(err("/tracks/* | ::price @| window(3) | mean").contains("aggregate"));
    assert!(err("/a/*[::x == (/y/)]").contains("regex"));
    assert!(err("//tracks/*::title").contains("/table/*"));
    // SUM(*) is not SQL: bare-row aggregates beyond count refuse.
    assert!(err("/t/* @| sum").contains("project a column"));
    // a reducer's arguments change what is reduced: all but
    // `count(--values)` stay with the engine
    assert!(err("/t/* | ::c @| count(--nulls)").contains("the '--nulls' option of 'count'"));
    assert!(err("/t/* | ::c @| sum(--of=::d)").contains("option of 'sum'"));
    assert!(
        err("/t/* | .k(::k) | ::c @|{.k} count(--nulls) | .n | %.")
            .contains("the '--nulls' option of 'count'")
    );
    // The fixed SELECT shape orders before LIMIT and dedups before
    // LIMIT; command orders it cannot render refuse rather than
    // silently reordering.
    assert!(err("/t/* @| [..5] @| sort_by(::c) | ::c").contains("before LIMIT"));
    assert!(err("/t/* @| [..5] | ::c @| unique").contains("DISTINCT before LIMIT"));
    assert!(err("/t/* @| sort_by(::a) @| sort_by(::b) | ::a").contains("second sort"));
    // the keyed sort's options are its own: the direction is said, a
    // named collation and a second key are not SQL's to say
    assert_eq!(
        x("/t/* @| sort_by(::c; --desc) | ::c"),
        "SELECT c FROM t ORDER BY c DESC"
    );
    assert!(
        err("/t/* @| sort_by(::c; --locale=de) | ::c").contains("names the engine's collation")
    );
    assert!(err("/t/* @| sort_by(::a; ::b) | ::a").contains("a sort by several keys"));
    // a case order is the engine's collation's as well (ruling
    // 2026-11-12), and `top` says the same of its options: they were
    // dropped, and the SQL ordered as the backend pleased
    for flag in [
        "-u",
        "--upper-first",
        "-l",
        "--lower-first",
        "-i",
        "--ignore-case",
    ] {
        let e = err(&format!("/t/* @| sort_by(::c; {flag}) | ::c"));
        assert!(
            e.contains("case order is the engine's collation's"),
            "{flag}: {e}"
        );
    }
    assert_eq!(
        x("/t/* @| top(3; ::c) | ::c"),
        "SELECT c FROM t ORDER BY c DESC LIMIT 3"
    );
    for option in ["--locale=de", "-i", "--upper-first"] {
        let e = err(&format!("/t/* @| top(3; ::c; {option}) | ::c"));
        assert!(e.contains("names the engine's collation"), "{option}: {e}");
    }
    // A '*=' pattern must be a text literal — a column operand
    // holds a value, not a pattern.
    assert!(err("/t/*[::a *= ::b] | ::a").contains("non-literal"));
    // `$*k` belongs on a joined expression; the driver cannot
    // reference the join (a parse-level placement error).
    assert!(err("/t/*[$$1::x = 5] | ::a").contains("drives"));
}

#[test]
fn export_details() {
    // A terminal projection on the joined branch selects from the
    // result context's table (it used to be silently dropped).
    assert_eq!(
        x("/albums/* <=> /tracks/*[::album_id = $$0::id] | %($$1::title)"),
        "SELECT tracks.title FROM albums JOIN tracks ON tracks.album_id = albums.id"
    );
    // `<=>?` keeps the unmatched driver rows: a LEFT JOIN, and the
    // joined expression's own predicates choose the witness, so they
    // stand in the ON (in a WHERE they would discard the unmatched
    // rows again).
    assert_eq!(
        x("/tracks/* <=>? /albums/*[::id = $$0::album_id] | %(::title; a = $$1::title)"),
        "SELECT tracks.title, albums.title AS a FROM tracks LEFT JOIN albums \
         ON albums.id = tracks.album_id"
    );
    assert_eq!(
        x(
            "/tracks/* <=>? /albums/*[::id = $$0::album_id && ::year > 1900] \
             | %(::title; a = $$1::title)"
        ),
        "SELECT tracks.title, albums.title AS a FROM tracks LEFT JOIN albums \
         ON albums.id = tracks.album_id AND albums.year > 1900"
    );
    // Grouped '| count' counts every member like Quarb does —
    // COUNT(*), never the NULL-skipping COUNT(col).
    assert_eq!(
        x("/t/* | ::c | .k(::k) @|{.k} count | .n | %."),
        "SELECT k, COUNT(*) AS n FROM t GROUP BY k"
    );
    // ORDER BY then LIMIT renders (that order matches Quarb's).
    assert_eq!(
        x("/t/* @| sort_by(::c) @| [..5] | ::c"),
        "SELECT c FROM t ORDER BY c LIMIT 5"
    );
    // LIKE metacharacters escape, with an explicit ESCAPE (SQLite,
    // MSSQL, and Oracle have no default escape character).
    assert_eq!(
        x("/t/*[::a *= \"50%\"] | ::a"),
        "SELECT a FROM t WHERE a LIKE '%50\\%%' ESCAPE '\\'"
    );
}

#[test]
fn identifiers_are_gated() {
    let pushdown = |q: &str| quarb_sql::pushdown(q, None);
    // Strict mode refuses any table or column name that is not a
    // plain SQL identifier: rendered bare it would rewrite the
    // SQL's meaning ("a OR b", "t; DROP TABLE u"), and quoting
    // dialects disagree.
    assert!(pushdown("/t/*[::\"a OR b\" = 5] | ::x").is_none());
    assert!(pushdown("/\"t; DROP TABLE u\"/*[::a = 5] | ::x").is_none());
    assert!(
        pushdown("/albums/* <=> /tracks/*[::aid = $$0::\"album-id\"] | %(t = $$1::title)")
            .is_none()
    );
    // The display translation double-quotes (ANSI) with a note.
    let t = export("/t/*[::\"a OR b\" = 5] | ::x").unwrap();
    assert_eq!(t.query, "SELECT x FROM t WHERE \"a OR b\" = 5");
    assert!(t.notes.iter().any(|n| n.contains("double-quoted")));
}

#[test]
fn refuses_the_internal_agg_marker() {
    // "__AGG__" is the exporter's placeholder for the HAVING
    // aggregate — same spoofing hazard as "__LEFT__".
    let q = "/t/* | ::q | .k(::k) @|{.k} sum | .s | [$_ = \"__AGG__\"] | %.";
    assert!(export(q).unwrap_err().to_string().contains("__AGG__"));
    assert!(quarb_sql::pushdown(q, None).is_none());
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
        "/tracks/*[::secs > 100 && ::price >= 1] | ::title @| sort_by(::title)",
        "/tracks/* | ::price @| sum",
        "/tracks/* @| top(2, ::secs) | %(::title)",
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
    let pushdown = |q: &str| quarb_sql::pushdown(q, None);
    // In: aggregates, filtered selections, witness joins.
    assert!(pushdown("/tracks/* @| count").is_some());
    assert!(pushdown("/tracks/*[::price < 1] | %(::title)").is_some());
    assert!(
        pushdown("/albums/* <=> /tracks/*[::album_id = $$0::id] | %(::title; track = $$1::title)")
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
        pushdown("/tracks/*[::price < 1] | %(::title)")
            .unwrap()
            .order_table
            .as_deref(),
        Some("tracks")
    );
    // Out: every construct whose SQL semantics are not provably
    // identical.
    assert!(pushdown("/tracks/*[::title *= \"o\"] | ::title").is_none()); // LIKE folding
    assert!(pushdown("/tracks/*[::album_id] | ::title").is_none()); // truthiness
    assert!(pushdown("/t/* | ::p @| unique").is_none()); // distinct order
    assert!(pushdown("/t/* | ::p @| window(3) | mean").is_none()); // quarb-side
    // a sort by an expression, by two keys or under a collation is
    // the engine's; so is a fold over a join, and a reducer over the
    // rows a selection kept
    assert!(pushdown("/t/* @| sort_by(::a + 1) | ::a").is_none());
    assert!(pushdown("/t/* @| sort_by(::a; ::b) | ::a").is_none());
    assert!(pushdown("/t/* @| sort_by(::a; --locale=de) | ::a").is_none());
    assert!(pushdown("/t/* @| [..3] | ::a @| sum").is_none());
    assert!(pushdown("/t/* @| [..3] @| sort_by(::a) | ::a").is_none());
    assert!(pushdown("/t/* @| [2..3] | ::a").is_none());
    assert!(pushdown("/t/* @| top(2; ::a; --locale=de) | ::a").is_none());
    assert!(pushdown("/t/* @| sort_by(::a) @| top(2; ::b) | ::a").is_none());
    // a reducer over a cut would count the table's rows
    assert!(pushdown("/t/* @| top(2; ::a) @| count").is_none());
    // row nodes, cut or sorted: the scan answers the nodes, a plan
    // their rows
    assert!(pushdown("/t/* @| [..3]").is_none());
    assert!(pushdown("/t/* @| sort_by(::a)").is_none());
    // a record the query built is told as one, of one field too
    assert!(pushdown("/t/* | %(::a)").unwrap().record);
    assert!(!pushdown("/t/* | ::a").unwrap().record);
}

/// The order a plan owes beyond the FROM table's key is the driver's
/// to write, and the plan says what it is: a sort by one numeric
/// column, a reverse, the first rows, the groups of a fold in the
/// order they first appear. Each needs the column numeric, which
/// the catalog confirms (`keyed_by`).
#[test]
fn a_plan_says_the_order_it_owes() {
    let pushdown = |q: &str| quarb_sql::pushdown(q, None).unwrap_or_else(|| panic!("{q}"));
    // a sort: the null key last, the ties to the key after it
    let p = pushdown("/t/* @| sort_by(::a) | ::a");
    assert_eq!(p.sql, "SELECT a FROM t");
    assert_eq!(p.order_first.as_deref(), Some("(a IS NULL), a"));
    assert_eq!(p.order_table.as_deref(), Some("t"));
    assert!(!p.order_desc && p.limit.is_none());
    assert_eq!(
        p.keyed_by,
        Some((quarb_sql::Refusal::Sort, "a".to_string()))
    );
    assert_eq!(p.column_types.numeric, ["a"]);
    // descending values keep a null last; a reverse turns every term
    let p = pushdown("/t/* @| sort_by(::a; --desc) @| [..5] | %(::a; ::b)");
    assert_eq!(p.sql, "SELECT a, b FROM t");
    assert_eq!(p.order_first.as_deref(), Some("(a IS NULL), a DESC"));
    assert_eq!((p.order_desc, p.limit), (false, Some(5)));
    let p = pushdown("/t/* @| sort_by(::a) @| reverse | ::a");
    assert_eq!(p.order_first.as_deref(), Some("(a IS NULL) DESC, a DESC"));
    assert!(p.order_desc);
    // the first rows, and the last ones reversed: the key alone
    let p = pushdown("/t/*[::a > 1] @| [..3] | ::b");
    assert_eq!(p.sql, "SELECT b FROM t WHERE a > 1");
    assert_eq!((p.order_first.as_deref(), p.limit), (None, Some(3)));
    assert!(p.keyed_by.is_none());
    let p = pushdown("/t/* @| reverse @| [..2] | ::a");
    assert_eq!((p.order_desc, p.limit), (true, Some(2)));
    // a fold by a numeric column: the groups as they first appear
    let p = pushdown("/t/* | ::p | .k(::k) @|{.k} sum | .s | %.");
    assert_eq!(p.sql, "SELECT k, SUM(p) AS s FROM t GROUP BY k");
    assert!(p.order_groups);
    assert_eq!(
        p.keyed_by,
        Some((quarb_sql::Refusal::Fold, "k".to_string()))
    );
    assert_eq!(p.order_table.as_deref(), Some("t"));
    assert_eq!(p.column_types.numeric, ["k", "p"]);
    // top and bottom: a sort and a cut over the keys that are not null
    let p = pushdown("/t/* @| top(3; ::a) | ::b");
    assert_eq!(p.sql, "SELECT b FROM t WHERE a IS NOT NULL");
    assert_eq!(p.order_first.as_deref(), Some("(a IS NULL), a DESC"));
    assert_eq!(p.limit, Some(3));
    let p = pushdown("/t/*[::b > 1] @| bottom(2; ::a) | ::b");
    assert_eq!(p.sql, "SELECT b FROM t WHERE b > 1 AND a IS NOT NULL");
    assert_eq!(p.order_first.as_deref(), Some("(a IS NULL), a"));
    // a count of the distinct values asks for no order
    let p = pushdown("/t/* | ::a @| unique @| count");
    assert!(p.order_table.is_none());
    assert_eq!(
        p.keyed_by,
        Some((quarb_sql::Refusal::Unique, "a".to_string()))
    );
}

#[test]
fn partial_pushdown_gate() {
    use quarb_sql::partial_pushdown;
    // In: a strict leading predicate with an unpushable pipeline.
    let p = partial_pushdown(
        "/events/*[::kind = \"rare\"] | ::amount | .b(::amount idiv 100) @|{.b} count | .n | %.",
        None,
    )
    .unwrap();
    assert_eq!(p.table, "events");
    assert_eq!(p.where_sql, "kind = 'rare'");
    // Only the LEADING expression run pushes; positional first = out.
    assert!(
        partial_pushdown(
            "/t/*[2][::a = 1] | ::a | .g(::a) @|{.g} count | .n | %.",
            None
        )
        .is_none()
    );
    // Reaching the table twice (a ^-anchored subcontext) = out.
    assert!(partial_pushdown("/t/*[::a = 1] | .n(^/t/* @| count) | $.n", None).is_none());
    // Crosslink/resolution axes = out.
    assert!(partial_pushdown("/t/*[::a = 1]::b-->::c", None).is_none());
    // Metadata = out (a filtered :::n-rows would lie).
    assert!(partial_pushdown("/t/*[::a = 1] | :::table", None).is_none());
    // Non-strict predicates (LIKE folding) = out.
    assert!(
        partial_pushdown(
            "/t/*[::a *= \"x\"] | ::a | .g(::a) @|{.g} count | .n | %.",
            None
        )
        .is_none()
    );
}

#[test]
fn pushdown_join_soundness_metadata() {
    let pushdown = |q: &str| quarb_sql::pushdown(q, None);
    // A witness join carries the joined table's ON-bound columns
    // so the driver can verify uniqueness (SQL JOIN multiplies
    // rows when several joined rows match one FROM row; Quarb's
    // existential binding never does).
    let p = pushdown("/albums/* <=> /tracks/*[::album_id = $$0::id] | %(a = ::title)")
        .expect("canonical witness join pushes down");
    let (table, cols) = p.join_left.expect("join carries its obligation");
    assert_eq!(table, "tracks");
    assert_eq!(cols, vec!["album_id".to_string()]);
    // No join → no obligation.
    assert!(
        pushdown("/tracks/* @| count")
            .expect("plain aggregate pushes down")
            .join_left
            .is_none()
    );
}

/// An every-witness join (`<=>*`, ruling 2026-11-11) is the JOIN
/// as SQL means it: no uniqueness to verify, and the joined table's
/// order owed after the driver's.
/// A comparison with the literal null inside a join condition is the
/// presence test, true or false as everywhere else (ruling
/// 2026-11-18; in 0.69 it was a key comparison, never held, and the
/// plan was refused): the scan admits by it, and the plan says IS
/// [NOT] NULL. Two values compared stay three-valued.
#[test]
fn a_null_test_inside_a_join_condition_is_the_presence_test() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE artists (id INTEGER PRIMARY KEY, name TEXT);
         CREATE TABLE tracks (id INTEGER PRIMARY KEY, title TEXT, artist_id INTEGER);
         INSERT INTO artists VALUES (1,'Holst'), (2,'Bartok');
         INSERT INTO tracks VALUES (1,'Mars',1), (2,'Venus',1), (3,'Bourree',2), (4,'Sketch',NULL);",
    )
    .unwrap();
    let adapter = quarb_sqlite::SqliteAdapter::load(&conn).unwrap();
    let count = |q: &str| match quarb::run(q, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs[0].to_string(),
        quarb::QueryResult::Nodes(ns) => ns.len().to_string(),
    };
    let on_driver = "/tracks/* <=>* /artists/*[$$0::artist_id != null && ::id = $$0::artist_id] \
                     @| count";
    // the three tracks that have an artist, each with its one
    assert_eq!(count(on_driver), "3");
    let plan = quarb_sql::pushdown(on_driver, Some(quarb_sql::Dialect::Sqlite))
        .expect("the presence test is pushed");
    assert!(
        plan.sql.contains("tracks.artist_id IS NOT NULL"),
        "{}",
        plan.sql
    );
    // the plan's own answer, where the planner has one (a join with
    // no key equality has no JOIN to write)
    let planned = |q: &str| {
        quarb_sql::pushdown(q, Some(quarb_sql::Dialect::Sqlite)).map(|plan| {
            let n: i64 = conn.query_row(&plan.sql, [], |r| r.get(0)).unwrap();
            n.to_string()
        })
    };
    assert_eq!(planned(on_driver).as_deref(), Some("3"));
    // `= null`: the driver that has no value. No artist's id equals
    // a null key, so with the key comparison beside it nothing is
    // admitted; alone it admits every candidate for that driver.
    let absent = "/tracks/* <=>* /artists/*[$$0::artist_id = null] @| count";
    assert_eq!(count(absent), "2");
    assert!(planned(absent).is_none_or(|n| n == "2"));
    let both = "/tracks/* <=>* /artists/*[$$0::artist_id = null && ::id = $$0::artist_id] \
                @| count";
    assert_eq!(count(both), "0");
    assert_eq!(planned(both).as_deref(), Some("0"));
    // two values compared stay three-valued: a null key meets no key
    let keys = "/tracks/* <=>* /artists/*[::id != $$0::artist_id] @| count";
    assert_eq!(count(keys), "3");
    assert!(planned(keys).is_none_or(|n| n == "3"));
    // on the candidate the test is a predicate like any other, and
    // on the driver's own step it stands before the join
    let on_candidate = "/tracks/* <=>* /artists/*[::id != null && ::id = $$0::artist_id] @| count";
    assert_eq!(count(on_candidate), "3");
    assert!(quarb_sql::pushdown(on_candidate, Some(quarb_sql::Dialect::Sqlite)).is_some());
    let before = "/tracks/*[::artist_id != null] <=>* /artists/*[::id = $$0::artist_id] @| count";
    assert_eq!(count(before), "3");
}

#[test]
fn pushdown_of_the_every_witness_join() {
    let pushdown = |q: &str| quarb_sql::pushdown(q, None);
    let p =
        pushdown("/albums/* <=>* /tracks/*[::album_id = $$0::id] | %(a = ::title; t = $$1::title)")
            .expect("the every-witness join pushes down");
    assert_eq!(
        p.sql,
        "SELECT albums.title AS a, tracks.title AS t FROM albums JOIN tracks \
         ON tracks.album_id = albums.id"
    );
    assert!(p.join_left.is_none(), "no uniqueness obligation");
    assert_eq!(p.order_table.as_deref(), Some("albums"));
    assert_eq!(p.order_then.as_deref(), Some("tracks"));
    // the outer form is the LEFT JOIN
    let p = pushdown(
        "/albums/* <=>*? /tracks/*[::album_id = $$0::id] | %(a = ::title; t = $$1::title)",
    )
    .expect("the outer every-witness join pushes down");
    assert!(p.sql.contains(" LEFT JOIN tracks ON "), "{}", p.sql);
    assert_eq!(p.order_then.as_deref(), Some("tracks"));
    // an aggregate over it has no rows to order
    let p = pushdown("/albums/* <=>* /tracks/*[::album_id = $$0::id] @| count")
        .expect("the count pushes down");
    assert!(p.order_then.is_none() && p.join_left.is_none());
    // the plain witness join keeps its obligation and owes no second order
    let p = pushdown("/albums/* <=> /tracks/*[::album_id = $$0::id] | %(a = ::title)").unwrap();
    assert!(p.join_left.is_some() && p.order_then.is_none());
}

#[test]
fn pushdown_refuses_keyword_aliases() {
    let pushdown_explained = |q: &str| quarb_sql::pushdown_explained(q, None);
    // `AS order` is an SQL syntax error; quoting portably differs
    // by dialect, so strict pushdown refuses outright.
    let Err(err) =
        pushdown_explained("/albums/* <=> /tracks/*[::album_id = $$0::id] | %(order = $$1::title)")
    else {
        panic!("keyword alias must refuse")
    };
    assert!(format!("{err}").contains("needs SQL quoting"), "{err}");
}

#[test]
fn refuses_the_internal_left_marker() {
    use quarb_sql::export;
    let pushdown = |q: &str| quarb_sql::pushdown(q, None);
    let pushdown_explained = |q: &str| quarb_sql::pushdown_explained(q, None);
    // "__LEFT__" is the exporter's internal placeholder for the
    // join's left table: query text containing it would be
    // rewritten inside its own literals and could spoof the join
    // obligation, so it stays on the scan path.
    let q =
        "/albums/* <=> /tracks/*[::note = '__LEFT__.id' && ::album_id = $$0::id] | %($$1::title)";
    assert!(pushdown(q).is_none());
    let Err(err) = pushdown_explained(q) else {
        panic!("marker in a literal must refuse")
    };
    assert!(format!("{err}").contains("__LEFT__"), "{err}");
    assert!(export(q).is_err(), "export substitutes too; must refuse");
}

#[test]
fn join_projections_qualify_by_operand_index() {
    let pushdown = |q: &str| quarb_sql::pushdown(q, None);
    let pushdown_explained = |q: &str| quarb_sql::pushdown_explained(q, None);
    // Bare columns project the driver/FROM table, `$$1` the
    // joined one — found by the seams article: every context used
    // to render as one table, compiling invalid SQL that only the
    // driver's runtime refusal caught.
    let p = pushdown(
        "/albums/* <=> /tracks/*[::album_id = $$0::id] \
         | %(a = ::title; t = $$1::title)",
    )
    .expect("two-sided projection pushes down");
    assert!(p.sql.contains("albums.title AS a"), "{}", p.sql);
    assert!(p.sql.contains("tracks.title AS t"), "{}", p.sql);
    // Beyond the joined operand there is no verified mapping.
    assert!(
        pushdown_explained("/albums/* <=> /tracks/*[::album_id = $$0::id] | %(x = $$2::title)")
            .is_err()
    );
}

/// Quarb compares by value, SQL by the column's type: a plan
/// records what it assumes of the columns it compares, and a
/// numeric-looking text literal does not push at all.
#[test]
fn a_plan_records_the_column_types_it_assumes() {
    let plan = quarb_sql::pushdown(
        "/books/*[::pages > 300 && ::genre = 'epic'] | ::title",
        None,
    )
    .unwrap();
    assert_eq!(plan.column_types.numeric, ["pages"]);
    assert_eq!(plan.column_types.text, ["genre"]);
    assert_eq!(plan.tables, ["books"]);
    let catalog = |pages: bool, genre: bool| {
        vec![
            ("title".to_string(), false, false, true),
            ("Pages".to_string(), pages, false, true),
            ("genre".to_string(), genre, false, true),
        ]
    };
    assert!(plan.column_types.unmet(&catalog(true, false)).is_empty());
    assert_eq!(plan.column_types.unmet(&catalog(false, false)), ["pages"]);
    assert_eq!(plan.column_types.unmet(&catalog(true, true)), ["genre"]);
    // a text column the database does not compare exactly (a
    // `char(n)`, a column under a collation) is no text column to
    // compare with
    let inexact = vec![
        ("pages".to_string(), true, false, true),
        ("genre".to_string(), false, false, false),
    ];
    assert_eq!(plan.column_types.unmet(&inexact), ["genre"]);
    // a column the catalog does not list is unmet
    assert_eq!(plan.column_types.unmet(&[]), ["pages", "genre"]);
    // the literal on the left, and the prefilter, record the same
    let plan = quarb_sql::pushdown("/books/*[300 < ::pages] | ::title", None).unwrap();
    assert_eq!(plan.column_types.numeric, ["pages"]);
    // a numeric reducer over a column assumes the column numeric:
    // the scan lifts unit text to quantities, SQL sums its digits
    let plan = quarb_sql::pushdown("/books/* | ::pages @| sum", None).unwrap();
    assert_eq!(plan.sql, "SELECT SUM(pages) FROM books");
    assert_eq!(plan.column_types.numeric, ["pages"]);
    let plan = quarb_sql::pushdown("/books/* | ::title @| max", None).unwrap();
    assert_eq!(plan.column_types.numeric, ["title"]);
    // count assumes nothing of a column
    let plan = quarb_sql::pushdown("/books/* @| count", None).unwrap();
    assert!(plan.column_types.is_empty());
    // the glued projection plans as the piped one (the planner's
    // first exact edge, 2026-11-17): the same statement, the same
    // assumption; it was refused as a reducer "over row nodes"
    for (glued, piped) in [
        ("/books/*::pages @| sum", "/books/* | ::pages @| sum"),
        ("/books/*::pages @| mean", "/books/* | ::pages @| mean"),
        ("/books/*::pages @| min", "/books/* | ::pages @| min"),
        ("/books/*::pages @| max", "/books/* | ::pages @| max"),
        ("/books/*::pages @| count", "/books/* | ::pages @| count"),
        (
            "/books/*[::genre = 'epic']::pages @| max",
            "/books/*[::genre = 'epic'] | ::pages @| max",
        ),
    ] {
        let g = quarb_sql::pushdown(glued, None).unwrap_or_else(|| panic!("{glued}"));
        let p = quarb_sql::pushdown(piped, None).unwrap_or_else(|| panic!("{piped}"));
        assert_eq!(g.sql, p.sql, "{glued}");
        assert_eq!(g.column_types, p.column_types, "{glued}");
    }
    let plan = quarb_sql::pushdown("/books/*::pages @| max", None).unwrap();
    assert_eq!(plan.sql, "SELECT MAX(pages) FROM books");
    assert_eq!(plan.column_types.numeric, ["pages"]);
    let partial = quarb_sql::partial_pushdown(
        "/books/*[::pages > 300] | ::title | .title(::title) @|{.title} count",
        None,
    )
    .unwrap();
    assert_eq!(partial.column_types.numeric, ["pages"]);
    // '150' meets 150 and '150.0' in Quarb, and only '150' in SQL
    assert!(quarb_sql::pushdown("/books/*[::code = '150'] | ::title", None).is_none());
    assert!(
        quarb_sql::partial_pushdown(
            "/books/*[::code = '150'] | ::title | .title(::title) @|{.title} count",
            None
        )
        .is_none()
    );
    // a column against a column assumes both numeric: the engine
    // compares two values by what they read as, SQL two text
    // columns as texts (it assumed nothing, and was pushed so)
    let plan = quarb_sql::pushdown("/books/*[::pages = ::leaves] | ::title", None).unwrap();
    assert_eq!(plan.column_types.numeric, ["pages", "leaves"]);
    assert!(plan.column_types.columns_compared);
}

/// A joined expression's mark (ruling 2026-11-14) and the driver's
/// are rows of the two tables: `$$.al::col` is the joined table's
/// column, `$$.t::col` the FROM table's, in the ON and after it.
#[test]
fn pushdown_reads_the_marks_of_a_join() {
    let pushdown = |q: &str| quarb_sql::pushdown(q, None);
    let named = pushdown(
        "/tracks/* .t <=> /albums/*[::id = $$.t::album_id] .al \
         | %(t = $$.t::title; al = $$.al::title)",
    )
    .expect("the named join pushes down");
    assert_eq!(
        named.sql,
        "SELECT tracks.title AS t, albums.title AS al FROM tracks JOIN albums \
         ON albums.id = tracks.album_id"
    );
    // the same plan as the numbered spelling, obligation included
    let numbered =
        pushdown("/tracks/* <=> /albums/*[::id = $$0::album_id] | %(t = ::title; al = $$1::title)")
            .unwrap();
    assert_eq!(named.sql, numbered.sql);
    assert_eq!(named.join_left, numbered.join_left);
    assert_eq!(
        named.join_left,
        Some(("albums".to_string(), vec!["id".to_string()]))
    );
    // the outer join and the every-witness join take the names too
    let p = pushdown(
        "/tracks/* .t <=>? /albums/*[::id = $$.t::album_id] .al \
         | %(t = $$.t::title; al = $$.al::title)",
    )
    .unwrap();
    assert!(
        p.sql
            .contains(" LEFT JOIN albums ON albums.id = tracks.album_id"),
        "{}",
        p.sql
    );
    let p = pushdown(
        "/albums/* .al <=>* /tracks/*[::album_id = $$.al::id] .t \
         | %(al = $$.al::title; t = $$.t::title)",
    )
    .unwrap();
    assert_eq!(
        p.sql,
        "SELECT albums.title AS al, tracks.title AS t FROM albums JOIN tracks \
         ON tracks.album_id = albums.id"
    );
    // a mark read the translation has no row for stays on the scan:
    // by position, of a mark on the table, of a mark with no name
    for q in [
        "/tracks/* .t <=> /albums/*[::id = $$.t::album_id] .al | %(n = $$.2::title)",
        "/tracks/* .t <=> /albums/*[::id = $$.t::album_id] . | %(n = $$.::title)",
        "/tracks .x /* | %(x = $$.x:name; t = ::title)",
        "/tracks/* .t <=> /albums .x /*[::id = $$.t::album_id] | %(t = ::title)",
    ] {
        assert!(pushdown(q).is_none(), "{q}");
    }
    // a driver's mark alone still pushes
    assert_eq!(
        pushdown("/tracks/* .t | %(x = $$.t::title; y = ::secs)")
            .unwrap()
            .sql,
        "SELECT title AS x, secs AS y FROM tracks"
    );
}

/// A whole plan answers values. A query that ends on row nodes is
/// the scan's (ruling 2026-11-18: locators), its conditions the
/// partial plan's, and the refusal carries no code.
#[test]
fn a_whole_plan_never_answers_row_nodes() {
    for q in ["/t/*", "/t/*[::a > 1]", "/t/* <=>* /u/*[::t_id = $$0::id]"] {
        let Err(refused) = quarb_sql::pushdown_explained(q, Some(quarb_sql::Dialect::Sqlite))
        else {
            panic!("{q} was planned whole");
        };
        let (word, _) = refused.not_pushed().expect("a refusal with a word");
        assert_eq!(word.code(), None, "{q}");
    }
    let partial = quarb_sql::partial_pushdown("/t/*[::a > 1]", Some(quarb_sql::Dialect::Sqlite))
        .expect("the condition goes ahead");
    assert_eq!(partial.where_sql, "a > 1");
    // with a column or a record after it, the plan is whole
    for q in [
        "/t/*[::a > 1]::b",
        "/t/*[::a > 1] | %(::b)",
        "/t/* @| count",
    ] {
        assert!(
            quarb_sql::pushdown(q, Some(quarb_sql::Dialect::Sqlite)).is_some(),
            "{q}"
        );
    }
}

/// `| %` over the rows of one table is the SELECT * plan: the record
/// of each row's properties, its NULL cells left out by the printer
/// (`properties`).
#[test]
fn the_whole_row_record_is_the_select_star_plan() {
    let pushdown = |q: &str| quarb_sql::pushdown(q, Some(quarb_sql::Dialect::Sqlite));
    let p = pushdown("/t/*[::a > 1] | %").expect("pushed");
    assert_eq!(p.sql, "SELECT * FROM t WHERE a > 1");
    assert!(p.record && p.properties);
    let p = pushdown("/t/* @| sort_by(::a) @| [..3] | %").expect("pushed");
    assert_eq!(p.sql, "SELECT * FROM t");
    assert_eq!(p.limit, Some(3));
    assert!(p.properties);
    // a record the query built keeps its nulls
    let p = pushdown("/t/* | %(::a; ::b)").expect("pushed");
    assert!(p.record && !p.properties);
    // not the rows themselves, not last, over a join: the engine's
    for q in [
        "/t/*::a | %",
        "/t/* | % @| count",
        "/t/* | % | ::a",
        "/t/* <=>* /u/*[::t_id = $$0::id] | %",
    ] {
        assert!(pushdown(q).is_none(), "{q}");
    }
}

/// Two values of a row compared with each other, and a column
/// computed with: the engine goes by what each value reads as, SQL
/// by the column's type, so the plan assumes numeric columns and
/// says that it compares two of them (ruling of the contract: a
/// whole plan answers as the scan does; the 0.70.0 planner pushed
/// these unchecked).
#[test]
fn compared_and_computed_columns_are_assumed_numeric() {
    let plan = |q: &str| quarb_sql::pushdown(q, Some(quarb_sql::Dialect::Sqlite)).expect(q);
    let p = plan("/t/*[::a = ::b]::id");
    assert_eq!(p.column_types.numeric, ["a", "b"]);
    assert!(p.column_types.columns_compared);
    let p = plan("/t/*[::a < ::b && ::c = 'x']::id");
    assert_eq!(p.column_types.numeric, ["a", "b"]);
    assert_eq!(p.column_types.text, ["c"]);
    // a join's key equality is such a comparison
    let p = plan("/t/* <=>* /u/*[::t_id = $$0::id] | %(a = ::id; b = $$1::id)");
    assert_eq!(p.column_types.numeric, ["t_id", "id"]);
    assert!(p.column_types.columns_compared);
    // a column against a literal is not: a typed engine holds it
    let p = plan("/t/*[::a > 3]::id");
    assert_eq!(p.column_types.numeric, ["a"]);
    assert!(!p.column_types.columns_compared);
    // arithmetic: in a condition and in what is read
    let p = plan("/t/*[::a + 1 > 5]::id");
    assert_eq!(p.sql, "SELECT id FROM t WHERE (a + 1) > 5");
    assert_eq!(p.column_types.numeric, ["a"]);
    assert!(!p.column_types.columns_compared);
    let p = plan("/t/* | ::a * ::b");
    assert_eq!(p.column_types.numeric, ["a", "b"]);
    let p = plan("/t/*[::a + 1 > ::b]::id");
    assert_eq!(p.column_types.numeric, ["b", "a"]);
    assert!(p.column_types.columns_compared);
}

/// `div` is the true division and answers inf or nan for a zero
/// divisor; `mod` keeps a fraction. SQL's `/` divides integers as
/// integers on most engines, and both answer no value or an error
/// for a zero divisor.
#[test]
fn division_is_real_and_by_a_number_that_is_not_zero() {
    use quarb_sql::Dialect::*;
    let sql = |q: &str, d: Option<quarb_sql::Dialect>| quarb_sql::pushdown(q, d).map(|p| p.sql);
    let q = "/t/* | ::n div 4";
    assert_eq!(
        sql(q, Some(Sqlite)).unwrap(),
        "SELECT (CAST(n AS REAL) / 4) FROM t"
    );
    assert_eq!(
        sql(q, Some(Postgres)).unwrap(),
        "SELECT (CAST(n AS DOUBLE PRECISION) / 4) FROM t"
    );
    assert_eq!(
        sql(q, Some(MySql)).unwrap(),
        "SELECT (CAST(n AS DOUBLE) / 4) FROM t"
    );
    assert_eq!(
        sql(q, Some(Mssql)).unwrap(),
        "SELECT (CAST(n AS FLOAT) / 4) FROM t"
    );
    assert_eq!(
        sql(q, None).unwrap(),
        "SELECT (CAST(n AS DOUBLE) / 4) FROM t"
    );
    assert_eq!(
        sql("/t/*[::n div 2.5 > 1]::id", Some(Sqlite)).unwrap(),
        "SELECT id FROM t WHERE (CAST(n AS REAL) / 2.5) > 1"
    );
    for q in [
        "/t/* | ::n div ::m",
        "/t/* | ::n div 0",
        "/t/* | ::n div 0.0",
        "/t/*[::n div ::m > 1]::id",
        "/t/* | ::n mod 3",
        "/t/*[::n mod 2 = 0]::id",
        // a text literal is not computed with, nor compared with what
        // is computed
        "/t/* | ::n + '1'",
        "/t/*[::n + 1 = 'x']::id",
    ] {
        assert!(sql(q, Some(Sqlite)).is_none(), "{q}");
    }
    let Err(refused) = quarb_sql::pushdown_explained("/t/* | ::n mod 3", Some(Sqlite)) else {
        panic!("mod was planned");
    };
    assert_eq!(
        refused.not_pushed().unwrap().0.code(),
        Some(quarb::diagnostic::OPERATOR_NOT_PUSHED)
    );
}

/// A text column against a text literal: the engine compares the
/// characters, a database compares under its collation. Each dialect
/// gets the form that is the engine's comparison there.
#[test]
fn a_text_comparison_is_written_for_the_dialects_collation() {
    use quarb_sql::Dialect::*;
    let sql = |q: &str, d: quarb_sql::Dialect| quarb_sql::pushdown(q, Some(d)).map(|p| p.sql);
    // SQLite compares bytes (a column under a collation is the
    // catalog's to report)
    assert_eq!(
        sql("/t/*[::name = 'holst']::id", Sqlite).unwrap(),
        "SELECT id FROM t WHERE name = 'holst'"
    );
    // MySQL: over the UTF-8 bytes of both sides; the collation's own
    // equality stays beside an `=`, for the index
    assert_eq!(
        sql("/t/*[::name = 'holst']::id", MySql).unwrap(),
        "SELECT id FROM t WHERE (name = 'holst' AND BINARY CONVERT(name USING utf8mb4) = \
         BINARY CONVERT('holst' USING utf8mb4))"
    );
    assert_eq!(
        sql("/t/*[::name != 'holst']::id", MySql).unwrap(),
        "SELECT id FROM t WHERE (BINARY CONVERT(name USING utf8mb4) <> BINARY \
         CONVERT('holst' USING utf8mb4) OR name IS NULL)"
    );
    assert_eq!(
        sql("/t/*['M' > ::name]::id", MySql).unwrap(),
        "SELECT id FROM t WHERE BINARY CONVERT('M' USING utf8mb4) > BINARY CONVERT(name \
         USING utf8mb4)"
    );
    // a number is compared as before
    assert_eq!(
        sql("/t/*[::n = 5]::id", MySql).unwrap(),
        "SELECT id FROM t WHERE n = 5"
    );
    // PostgreSQL: `=` is exact, an order is the collation's unless
    // it is written under "C"
    assert_eq!(
        sql("/t/*[::name = 'holst']::id", Postgres).unwrap(),
        "SELECT id FROM t WHERE name = 'holst'"
    );
    assert_eq!(
        sql("/t/*[::name < 'M']::id", Postgres).unwrap(),
        "SELECT id FROM t WHERE name < 'M' COLLATE \"C\""
    );
    assert_eq!(
        sql("/t/*[::n < 5]::id", Postgres).unwrap(),
        "SELECT id FROM t WHERE n < 5"
    );
    // SQL Server: the comparison stays with the engine, and an `=`
    // still narrows the fetch (what the collation calls equal holds
    // every row the engine does)
    assert!(sql("/t/*[::name = 'holst']::id", Mssql).is_none());
    assert!(sql("/t/*[::name < 'M']::id", Mssql).is_none());
    let partial = |q: &str| quarb_sql::partial_pushdown(q, Some(Mssql)).map(|p| p.where_sql);
    assert_eq!(
        partial("/t/*[::name = 'holst']::id").as_deref(),
        Some("name = 'holst'")
    );
    assert_eq!(
        partial("/t/*[::n > 2 && ::name = 'holst']::id").as_deref(),
        Some("(n > 2 AND name = 'holst')")
    );
    // an order, an inequality and a literal that reads as a number
    // narrow nothing there
    for q in [
        "/t/*[::name < 'M']::id",
        "/t/*[::name != 'holst']::id",
        "/t/*[::name = '10']::id",
    ] {
        assert_eq!(partial(q), None, "{q}");
    }
}

/// A table joined to itself is aliased on the joined side, and the
/// plan says under which name the driver writes that side's key.
#[test]
fn a_self_join_names_its_joined_side() {
    let plan = |q: &str| quarb_sql::pushdown(q, Some(quarb_sql::Dialect::Sqlite)).expect(q);
    let p = plan(
        "/employees/* <=>* /employees/*[::manager_id = $$0::id && ::salary > 100] \
         | %(boss = ::name; report = $$1::name)",
    );
    assert_eq!(
        p.sql,
        "SELECT employees.name AS boss, employees_2.name AS report FROM employees JOIN \
         employees employees_2 ON employees_2.manager_id = employees.id WHERE \
         employees_2.salary > 100"
    );
    assert_eq!(p.order_table.as_deref(), Some("employees"));
    assert_eq!(p.order_then.as_deref(), Some("employees"));
    assert_eq!(p.order_then_as.as_deref(), Some("employees_2"));
    assert_eq!(p.tables, ["employees", "employees"]);
    // two tables: no alias, as before
    let p = plan("/albums/* <=>* /tracks/*[::album_id = $$0::id] | %(a = ::title; t = $$1::title)");
    assert!(
        p.sql
            .contains(" FROM albums JOIN tracks ON tracks.album_id = albums.id")
    );
    assert_eq!(p.order_then.as_deref(), Some("tracks"));
    assert_eq!(p.order_then_as, None);
}

/// A plan names the sums and means it writes over a column
/// (`FloatSum`, 0.71): a source that adds floats in its own order
/// rewrites them in its ordered form. A plan that joins names none,
/// and a count is no sum.
#[test]
fn a_plan_names_its_sums_over_a_column() {
    let sums = |q: &str| -> Vec<(String, bool, String, String)> {
        quarb_sql::pushdown(q, Some(quarb_sql::Dialect::Postgres))
            .unwrap_or_else(|| panic!("expected pushdown for {q}"))
            .sums
            .into_iter()
            .map(|s| (s.text, s.mean, s.table, s.column))
            .collect()
    };
    let one = |text: &str, mean, column: &str| {
        vec![(text.to_string(), mean, "t".to_string(), column.to_string())]
    };
    assert_eq!(sums("/t/*::x @| sum"), one("SUM(x)", false, "x"));
    assert_eq!(sums("/t/*[::g = 3]::x @| mean"), one("AVG(x)", true, "x"));
    assert_eq!(sums("/t/*::x @| count"), vec![]);
    assert_eq!(sums("/t/*::x @| max"), vec![]);
    assert_eq!(sums("/t/* @| count"), vec![]);
    // the text stands in the statement, to be rewritten there
    let plan = quarb_sql::pushdown("/t/*::x @| sum", Some(quarb_sql::Dialect::Postgres)).unwrap();
    assert!(plan.sql.contains(&plan.sums[0].text), "{}", plan.sql);
    // the order the scan adds in: the driver's key; after it the
    // joined table's where every witness is bound, under its alias
    // where the table is joined to itself; a first witness is one
    // row a driver
    let order = |q: &str| -> (String, Vec<(String, String)>) {
        let s = quarb_sql::pushdown(q, Some(quarb_sql::Dialect::Postgres))
            .unwrap_or_else(|| panic!("expected pushdown for {q}"))
            .sums
            .remove(0);
        (s.table, s.order)
    };
    let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
    assert_eq!(
        order("/t/*::x @| sum"),
        ("t".to_string(), vec![pair("t", "t")])
    );
    assert_eq!(
        order("/t/* <=>* /u/*[::t_id = $$0::id] | $$1::x @| sum"),
        ("u".to_string(), vec![pair("t", "t"), pair("u", "u")])
    );
    assert_eq!(
        order("/t/* <=>* /u/*[::t_id = $$0::id] | ::x @| sum"),
        ("t".to_string(), vec![pair("t", "t"), pair("u", "u")])
    );
    assert_eq!(
        order("/u/* <=> /t/*[::id = $$0::t_id] | $$1::x @| mean"),
        ("t".to_string(), vec![pair("u", "u")])
    );
    assert_eq!(
        order("/t/* <=>* /t/*[::boss = $$0::id] | $$1::x @| sum"),
        ("t".to_string(), vec![pair("t", "t"), pair("t", "t_2")])
    );
}
