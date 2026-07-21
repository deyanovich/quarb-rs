//! Live integration tests, gated on a running server: set
//! `QUARB_ORACLE` to a connection string for a schema loaded
//! with the music-store fixture and run with
//! `cargo test -p quarb-oracle -- --ignored`. Needs an Oracle
//! Instant Client on the loader path (libclntsh).
//!
//! Verified against Oracle Free 23ai with the `music` schema
//! from the overnight adapter round (uppercase dictionary
//! names, a DATE column decoded to instants).

use quarb_oracle::OracleAdapter;

fn adapter() -> OracleAdapter {
    let target = std::env::var("QUARB_ORACLE").expect("QUARB_ORACLE connection string");
    OracleAdapter::connect(&target).unwrap()
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
#[ignore = "needs QUARB_ORACLE pointing at a music schema"]
fn catalog_and_rows() {
    assert_eq!(nodes("/*"), ["/ALBUMS", "/ARTISTS", "/TRACKS"]);
    assert_eq!(values("/TRACKS/1::TITLE"), ["Mars"]);
    assert_eq!(values("/TRACKS/*::PRICE @| sum"), ["3.2700000000000005"]);
}

#[test]
#[ignore = "needs QUARB_ORACLE pointing at a music schema"]
fn fk_machinery() {
    // the two-hop join as a path, over USER_CONSTRAINTS FKs
    assert_eq!(
        values("/TRACKS/1::ALBUM_ID~>::ARTIST_ID~>::NAME"),
        ["Holst"]
    );
}

#[test]
#[ignore = "needs QUARB_ORACLE pointing at a music schema"]
fn dates_are_instants() {
    // DATE decodes through the ISO TO_CHAR into an instant, so a
    // bare-date predicate compares on the calendar.
    assert_eq!(
        values("/TRACKS/*[::ADDED > 2026-03-15]::TITLE @| sort"),
        ["Bourree"]
    );
}
