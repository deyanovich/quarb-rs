//! Live integration tests, gated on a running server: set
//! `QUARB_MYSQL` to a `mysql://` URL for a database loaded with
//! the music-store fixture (see the SQL cookbook) and run with
//! `cargo test -p quarb-mysql -- --ignored`.

use quarb_mysql::MysqlAdapter;

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

fn values(query: &str) -> Vec<String> {
    let config = std::env::var("QUARB_MYSQL").expect("QUARB_MYSQL connection URL");
    let a = MysqlAdapter::connect(&config).unwrap();
    match quarb::run(query, &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    }
}

#[test]
#[ignore = "needs QUARB_MYSQL pointing at a music-store database"]
fn catalog_rows_and_lazy() {
    assert_eq!(values("/tracks:::n-rows"), vec!["7"]);
    assert_eq!(values("/artists/2::name"), vec!["Bartok"]);
    assert_eq!(values("/tracks/*[::price < 1]::title @| count"), vec!["3"]);
}

#[test]
#[ignore = "needs QUARB_MYSQL pointing at a music-store database"]
fn fk_machinery() {
    assert_eq!(
        values("/invoices/1::track_id-->::album_id-->::artist_id-->::name"),
        vec!["Holst"]
    );
    assert_eq!(
        values("/artists/2<--(::artist_id)::title"),
        vec!["Mikrokosmos", "Quartets"]
    );
}

#[test]
#[ignore = "needs QUARB_MYSQL pointing at a music-store database"]
fn sql_shapes() {
    assert_eq!(
        values(
            "/tracks/* | ::price | .artist(::album_id-->::artist_id-->::name) @|{.artist} \
             | sum | .rev | %($.artist; rev = $.rev)"
        ),
        vec![
            r#"{"artist": "Holst", "rev": 3.5700000000000003}"#,
            r#"{"artist": "Bartok", "rev": 3.5700000000000003}"#,
            r#"{"artist": "Satie", "rev": 0.99}"#
        ]
    );
    assert_eq!(
        values(
            "/albums/* <=> /tracks/*[::album_id = $$1::id && ::secs > 400] \
             | %(album = $$1::title; ::title)"
        ),
        vec![
            r#"{"album": "The Planets", "title": "Mars"}"#,
            r#"{"album": "The Planets", "title": "Venus"}"#,
            r#"{"album": "The Planets", "title": "Jupiter"}"#,
            r#"{"album": "Quartets", "title": "Quartet No.4"}"#
        ]
    );
}

/// An every-witness plan (`<=>*`, `<=>*?`) is pushed here from 0.69:
/// the rows of one driver in the joined table's own order, after the
/// driver's, both by their primary keys. The plan answers what the
/// engine's own evaluation answers, row for row.
#[test]
#[ignore = "needs QUARB_MYSQL pointing at a music-store database"]
fn an_every_witness_join_is_pushed_in_the_scans_order() {
    let config = std::env::var("QUARB_MYSQL").expect("QUARB_MYSQL connection URL");
    for q in [
        "/artists/* <=>* /albums/*[::artist_id = $$0::id] | %(a = ::name; t = $$1::title)",
        "/albums/* <=>* /tracks/*[::album_id = $$0::id && ::secs > 200] \
         | %(a = ::title; t = $$1::title)",
        "/artists/* <=>*? /albums/*[::artist_id = $$0::id && ::year > 1930] \
         | %(a = ::name; t = $$1::title)",
    ] {
        let plan = quarb_sql::pushdown(q, Some(quarb_sql::Dialect::MySql)).expect("pushes");
        assert!(plan.order_then.is_some(), "{q}");
        let (_, rows) = quarb_mysql::raw_query(&config, &plan.sql, &order_of(&plan), None).unwrap();
        let pushed: Vec<String> = rows
            .iter()
            .map(|r| {
                r.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" | ")
            })
            .collect();
        let a = MysqlAdapter::connect(&config).unwrap();
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
