//! Live integration tests, gated on a running server: set
//! `QUARB_PG` to a connection string for a database loaded with
//! the music-store fixture (see the SQL cookbook) and run with
//! `cargo test -p quarb-postgres -- --ignored`.

use quarb_postgres::PostgresAdapter;

/// The order a plan owes, as its driver takes it.
fn order_of(plan: &quarb_sql::Pushdown) -> quarb_relational::PlanOrder<'_> {
    quarb_relational::PlanOrder {
        table: plan.order_table.as_deref(),
        then: plan.order_then.as_deref(),
        then_as: plan.order_then_as.as_deref(),
        first: plan.order_first.as_deref(),
        desc: plan.order_desc,
        groups: plan.order_groups,
        limit: plan.limit,
    }
}

fn adapter() -> PostgresAdapter {
    let config = std::env::var("QUARB_PG").expect("QUARB_PG connection string");
    PostgresAdapter::connect(&config).unwrap()
}

fn values(query: &str) -> Vec<String> {
    let a = adapter();
    match quarb::run(query, &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    }
}

#[test]
#[ignore = "needs QUARB_PG pointing at a music-store database"]
fn catalog_and_rows() {
    assert_eq!(values("/tracks:::n-rows"), vec!["7"]);
    assert_eq!(values("/artists/2::name"), vec!["Bartok"]);
    assert_eq!(values("/tracks/*[::price < 1]::title @| count"), vec!["3"]);
}

#[test]
#[ignore = "needs QUARB_PG pointing at a music-store database"]
fn fk_machinery() {
    // the three-way join as a path, over information_schema FKs
    assert_eq!(
        values("/invoices/1::track_id-->::album_id-->::artist_id-->::name"),
        vec!["Holst"]
    );
    // reverse resolution
    assert_eq!(
        values("/artists/2<--(::artist_id)::title"),
        vec!["Mikrokosmos", "Quartets"]
    );
}

#[test]
#[ignore = "needs QUARB_PG pointing at a music-store database"]
fn sql_shapes() {
    // (the two queries in today's spelling: the fold names its key,
    // the driver is `$$0`, the join binds every witness)
    assert_eq!(
        values(
            "/tracks/* | ::price | .artist(::album_id-->::artist_id-->::name) @|{.artist} sum \
             | .rev | %."
        ),
        // (the fixture's `price REAL` is a four-byte float here, read
        // as the nearest double: 0.99 is 0.9900000095367432)
        vec![
            r#"%(artist = "Holst"; rev = 3.569999933242798)"#,
            r#"%(artist = "Bartok"; rev = 3.569999933242798)"#,
            r#"%(artist = "Satie"; rev = 0.9900000095367432)"#
        ]
    );
    assert_eq!(
        values(
            "/albums/* <=>* /tracks/*[::album_id = $$0::id && ::secs > 400] \
             | %(album = ::title; title = $$1::title)"
        ),
        vec![
            r#"%(album = "The Planets"; title = "Mars")"#,
            r#"%(album = "The Planets"; title = "Venus")"#,
            r#"%(album = "The Planets"; title = "Jupiter")"#,
            r#"%(album = "Quartets"; title = "Quartet No.4")"#
        ]
    );
}

/// An every-witness plan (`<=>*`, `<=>*?`) is pushed here from 0.69:
/// the rows of one driver in the joined table's own order, after the
/// driver's, both by their primary keys. The plan answers what the
/// engine's own evaluation answers, row for row.
#[test]
#[ignore = "needs QUARB_PG pointing at a music-store database"]
fn an_every_witness_join_is_pushed_in_the_scans_order() {
    let config = std::env::var("QUARB_PG").expect("QUARB_PG connection string");
    for q in [
        "/artists/* <=>* /albums/*[::artist_id = $$0::id] | %(a = ::name; t = $$1::title)",
        "/albums/* <=>* /tracks/*[::album_id = $$0::id && ::secs > 200] \
         | %(a = ::title; t = $$1::title)",
        "/artists/* <=>*? /albums/*[::artist_id = $$0::id && ::year > 1930] \
         | %(a = ::name; t = $$1::title)",
    ] {
        let plan = quarb_sql::pushdown(q, Some(quarb_sql::Dialect::Postgres)).expect("pushes");
        assert!(plan.order_then.is_some(), "{q}");
        let (_, rows) =
            quarb_postgres::raw_query(&config, &plan.sql, &order_of(&plan), None).unwrap();
        let pushed: Vec<String> = rows
            .iter()
            .map(|r| {
                r.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" | ")
            })
            .collect();
        let a = adapter();
        let scanned: Vec<String> = match quarb::run(q, &a).unwrap() {
            quarb::QueryResult::Values(vs) => vs
                .iter()
                .map(|v| match v {
                    quarb::Value::Record(fields) => fields
                        .iter()
                        .map(|(_, v)| v.to_string())
                        .collect::<Vec<_>>()
                        .join(" | "),
                    other => other.to_string(),
                })
                .collect(),
            quarb::QueryResult::Nodes(_) => panic!("expected values"),
        };
        assert!(pushed.len() >= 3, "{q}: {pushed:?}");
        assert_eq!(pushed, scanned, "{q}");
    }
}

/// A whole plan answers as the scan does where it compares text,
/// compares two columns or computes (0.71): the engine compares the
/// characters of two texts and the values of two columns, and
/// divides truly, where the database compares under a collation and
/// by its types. Each plan here is one the catalog bears out; its
/// rows are the scan's.
#[test]
#[ignore = "needs QUARB_PG pointing at a music-store database"]
fn text_comparisons_columns_and_arithmetic_answer_as_the_scan() {
    let config = std::env::var("QUARB_PG").expect("QUARB_PG connection string");
    for q in [
        // no artist is spelled in small letters: a collation that
        // folds case answered Holst
        "/artists/*[::name = 'holst']::id",
        "/artists/*[::name = 'Holst']::id",
        "/artists/*[::name != 'holst']::id",
        "/artists/*[::name >= 'a']::id",
        "/artists/*[::name < 'M']::id",
        "/artists/*['M' > ::name]::id",
        "/artists/*[::country = 'uk'] @| count",
        "/tracks/*[::title = 'mars']::id",
        "/tracks/*[::title != 'Mars' && ::secs > 100]::id",
        // two numeric columns, and a computed value
        "/tracks/*[::id = ::album_id]::id",
        "/tracks/*[::secs > ::id]::id",
        "/tracks/* | ::secs div 7",
        "/tracks/*[::secs div 60 > 7]::id",
        "/tracks/*[::secs + 1 > 400]::id",
        "/tracks/* | %(::id; m = ::secs div 60)",
    ] {
        let plan = quarb_sql::pushdown(q, Some(quarb_sql::Dialect::Postgres))
            .unwrap_or_else(|| panic!("{q}"));
        let tables: Vec<&str> = plan.tables.iter().map(String::as_str).collect();
        let catalog = quarb_postgres::column_catalog(&config, &tables).unwrap();
        assert_eq!(
            plan.column_types.unmet(&catalog),
            Vec::<String>::new(),
            "{q}"
        );
        let (_, rows) = quarb_postgres::raw_query(&config, &plan.sql, &order_of(&plan), None)
            .unwrap_or_else(|e| panic!("{q}: {e}: {}", plan.sql));
        let pushed: Vec<String> = rows
            .iter()
            .map(|r| {
                r.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" | ")
            })
            .collect();
        let a = PostgresAdapter::connect(&config).unwrap();
        let scanned: Vec<String> = match quarb::run(q, &a).unwrap() {
            quarb::QueryResult::Values(vs) => vs
                .iter()
                .map(|v| match v {
                    quarb::Value::Record(fields) => fields
                        .iter()
                        .map(|(_, v)| v.to_string())
                        .collect::<Vec<_>>()
                        .join(" | "),
                    other => other.to_string(),
                })
                .collect(),
            quarb::QueryResult::Nodes(_) => panic!("expected values"),
        };
        assert_eq!(pushed, scanned, "{q}: {}", plan.sql);
    }
    // two text columns compared, a text column computed with: the
    // catalog does not bear the plan out, and the tool scans
    for q in [
        "/tracks/*[::title = ::title]::id",
        "/artists/*[::name < ::country]::id",
        "/tracks/* | ::title + 1",
    ] {
        let plan = quarb_sql::pushdown(q, Some(quarb_sql::Dialect::Postgres))
            .unwrap_or_else(|| panic!("{q}"));
        let tables: Vec<&str> = plan.tables.iter().map(String::as_str).collect();
        let catalog = quarb_postgres::column_catalog(&config, &tables).unwrap();
        assert!(!plan.column_types.unmet(&catalog).is_empty(), "{q}");
    }
}

/// A plan's float sums in the ordered form (0.71). PostgreSQL adds
/// a float column plainly, under a parallel plan in whatever order
/// its workers return, and the last digits of a sum move from run
/// to run; `sum(x ORDER BY key)` adds in the order the scan reads
/// the rows. A four-byte column (the fixture's `price REAL`) is
/// widened first: PostgreSQL would add it in four bytes.
#[test]
#[ignore = "needs QUARB_PG pointing at a music-store database"]
fn a_float_sum_is_asked_in_key_order() {
    let config = std::env::var("QUARB_PG").expect("QUARB_PG connection string");
    let ask = |mean, column| quarb_relational::FloatSum {
        mean,
        table: "tracks",
        column,
        column_sql: column,
    };
    assert_eq!(
        quarb_postgres::float_sum_forms(
            &config,
            &[ask(false, "price"), ask(true, "price"), ask(false, "secs")]
        )
        .unwrap(),
        [
            Some(r#"sum((price)::float8 ORDER BY "tracks"."id")"#.to_string()),
            Some(r#"avg((price)::float8 ORDER BY "tracks"."id")"#.to_string()),
            None,
        ]
    );
    // the ordered sum is the cells added one after another in key
    // order, each read as a double, and the same on every run
    let plan = quarb_sql::pushdown(
        "/tracks/*::price @| sum",
        Some(quarb_sql::Dialect::Postgres),
    )
    .unwrap();
    assert_eq!(plan.sums.len(), 1);
    let sql = plan.sql.replace(
        &plan.sums[0].text,
        r#"sum((price)::float8 ORDER BY "tracks"."id")"#,
    );
    let none = quarb_relational::PlanOrder::default();
    let (_, cells) = quarb_postgres::raw_query(
        &config,
        "SELECT price::float8 FROM tracks ORDER BY id",
        &none,
        None,
    )
    .unwrap();
    let plain: f64 = cells
        .iter()
        .filter_map(|r| match &r[0] {
            quarb::Value::Float(x) => Some(*x),
            _ => None,
        })
        .sum();
    for _ in 0..3 {
        let (_, rows) = quarb_postgres::raw_query(&config, &sql, &order_of(&plan), None).unwrap();
        assert_eq!(rows[0][0], quarb::Value::Float(plain), "{sql}");
    }
}
