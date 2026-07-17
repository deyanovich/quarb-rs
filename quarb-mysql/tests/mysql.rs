//! Live integration tests, gated on a running server: set
//! `QUARB_MYSQL` to a `mysql://` URL for a database loaded with
//! the music-store fixture (see the SQL cookbook) and run with
//! `cargo test -p quarb-mysql -- --ignored`.

use quarb_mysql::MysqlAdapter;

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
    assert_eq!(values("/tracks::;n-rows"), vec!["7"]);
    assert_eq!(values("/artists/2::name"), vec!["Bartok"]);
    assert_eq!(values("/tracks/*[::price < 1]::title @| count"), vec!["3"]);
}

#[test]
#[ignore = "needs QUARB_MYSQL pointing at a music-store database"]
fn fk_machinery() {
    assert_eq!(
        values("/invoices/1::track_id~>::album_id~>::artist_id~>::name"),
        vec!["Holst"]
    );
    assert_eq!(
        values("/artists/2::artist_id<~::title"),
        vec!["Mikrokosmos", "Quartets"]
    );
}

#[test]
#[ignore = "needs QUARB_MYSQL pointing at a music-store database"]
fn sql_shapes() {
    assert_eq!(
        values(
            "/tracks/* | ::price @| group(\"artist\", ::album_id~>::artist_id~>::name) \
             | sum | .rev | rec($.artist, \"rev\", $.rev)"
        ),
        vec![
            r#"{"artist": "Holst", "rev": 3.5700000000000003}"#,
            r#"{"artist": "Bartok", "rev": 3.5700000000000003}"#,
            r#"{"artist": "Satie", "rev": 0.99}"#
        ]
    );
    assert_eq!(
        values(
            "/albums/* <=> /tracks/*[::album_id = $*1::id and ::secs > 400] \
             | rec(\"album\", $*1::title, ::title)"
        ),
        vec![
            r#"{"album": "The Planets", "title": "Mars"}"#,
            r#"{"album": "The Planets", "title": "Venus"}"#,
            r#"{"album": "The Planets", "title": "Jupiter"}"#,
            r#"{"album": "Quartets", "title": "Quartet No.4"}"#
        ]
    );
}
