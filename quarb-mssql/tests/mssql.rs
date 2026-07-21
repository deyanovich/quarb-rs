//! Live integration tests, gated on a running server: set
//! `QUARB_MSSQL` to a connection string for a database loaded
//! with the music-store fixture and run with
//! `cargo test -p quarb-mssql -- --ignored`.
//!
//! Verified against SQL Server 2022 with the `music` database
//! from the overnight adapter round.

use quarb_mssql::MssqlAdapter;

fn adapter() -> MssqlAdapter {
    let target = std::env::var("QUARB_MSSQL").expect("QUARB_MSSQL connection string");
    MssqlAdapter::connect(&target).unwrap()
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
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
        quarb::QueryResult::Values(_) => panic!("expected nodes"),
    }
}

#[test]
#[ignore = "needs QUARB_MSSQL pointing at a music database"]
fn catalog_and_rows() {
    assert_eq!(nodes("/*"), ["/albums", "/artists", "/tracks"]);
    assert_eq!(values("/tracks/1::title"), ["Mars"]);
    assert_eq!(
        values("/tracks/*[::price < 1]::title @| sort"),
        ["Bourree", "Gymnopedie No.1", "Venus"]
    );
}

#[test]
#[ignore = "needs QUARB_MSSQL pointing at a music database"]
fn fk_machinery() {
    // the two-hop join as a path, over sys.foreign_key_columns
    assert_eq!(
        values("/tracks/1::album_id~>::artist_id~>::name"),
        ["Holst"]
    );
}
