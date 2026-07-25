//! Live battery, gated on QUARB_AGE_TEST (a reachable AGE
//! Postgres seeded as below). Fixture: docker `apache/age` on
//! localhost:15432, password `quarbtest`, graph `office`:
//!   City: Oslo/NO, Novi Sad/RS
//!   Person: Ada (engineer), Bo (analyst)
//!   Ada -LIVES_IN{since:2019}-> Oslo,
//!   Bo -LIVES_IN{since:2021}-> Novi Sad,
//!   Ada -MENTORS{hours:3}-> Bo

use quarb_age::AgeAdapter;

fn gate() -> Option<String> {
    std::env::var("QUARB_AGE_TEST").ok().map(|v| {
        if v == "1" {
            "age://postgres:quarbtest@localhost:15432/postgres/office?key=name".to_string()
        } else {
            v
        }
    })
}

fn values(a: &AgeAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    }
}

#[test]
#[ignore = "needs a seeded local AGE Postgres (QUARB_AGE_TEST=1)"]
fn live_age() {
    let Some(target) = gate() else { return };
    let a = AgeAdapter::connect(&target).unwrap();

    // Vertex labels at the root (internal `_…` labels hidden).
    assert_eq!(values(&a, "/*:::name"), ["City", "Person"]);
    assert_eq!(values(&a, "/Person;;;n-rows"), ["2"]);

    // ?key= names vertices; properties project.
    assert_eq!(values(&a, "/Person/*:::name"), ["Ada", "Bo"]);
    assert_eq!(values(&a, "/Person/Ada::role"), ["engineer"]);
    assert_eq!(values(&a, "/City/*[::country = \"RS\"]::name"), ["Novi Sad"]);

    // Edge labels are typed crosslinks, both directions.
    assert_eq!(values(&a, "/Person/Ada->LIVES_IN::name"), ["Oslo"]);
    assert_eq!(values(&a, "/Person/Bo<-MENTORS::name"), ["Ada"]);
    assert_eq!(values(&a, "/City/Oslo<-LIVES_IN::role"), ["engineer"]);

    // Edge properties answer the $- accessor.
    assert_eq!(
        values(&a, "/Person/*->LIVES_IN[$-::since > 2020]::name"),
        ["Novi Sad"]
    );

    // Resolution against the ?key= property.
    assert_eq!(values(&a, "/Person/Ada->MENTORS @| count"), ["1"]);
}
