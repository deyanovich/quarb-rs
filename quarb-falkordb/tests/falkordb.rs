//! Live battery, gated on QUARB_FALKOR_TEST (a reachable
//! FalkorDB seeded as below). Fixture: docker
//! `falkordb/falkordb` on localhost:16380, graph `office`:
//!   City: Oslo/NO, Novi Sad/RS
//!   Person: Ada (engineer), Bo (analyst)
//!   Ada -LIVES_IN{since:2019}-> Oslo,
//!   Bo -LIVES_IN{since:2021}-> Novi Sad,
//!   Ada -MENTORS{hours:3}-> Bo

use quarb_falkordb::FalkorAdapter;

fn gate() -> Option<String> {
    std::env::var("QUARB_FALKOR_TEST").ok().map(|v| {
        if v == "1" {
            "falkor://localhost:16380/office?key=name".to_string()
        } else {
            v
        }
    })
}

fn values(a: &FalkorAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    }
}

#[test]
#[ignore = "needs a seeded local FalkorDB (QUARB_FALKOR_TEST=1)"]
fn live_falkordb() {
    let Some(target) = gate() else { return };
    let a = FalkorAdapter::connect(&target).unwrap();

    // Labels at the root, nodes named by ?key=.
    assert_eq!(values(&a, "/*:::name"), ["City", "Person"]);
    assert_eq!(values(&a, "/Person;;;n-rows"), ["2"]);
    assert_eq!(values(&a, "/Person/*:::name"), ["Ada", "Bo"]);
    assert_eq!(values(&a, "/Person/Ada::role"), ["engineer"]);
    assert_eq!(values(&a, "/City/*[::country = \"RS\"]::name"), ["Novi Sad"]);

    // Relationships as typed crosslinks, both directions.
    assert_eq!(values(&a, "/Person/Ada->LIVES_IN::name"), ["Oslo"]);
    assert_eq!(values(&a, "/Person/Bo<-MENTORS::name"), ["Ada"]);
    assert_eq!(values(&a, "/City/Oslo<-LIVES_IN::role"), ["engineer"]);

    // Edge properties answer the $- accessor.
    assert_eq!(
        values(&a, "/Person/*->LIVES_IN[$-::since > 2020]::name"),
        ["Novi Sad"]
    );

    // Hinted resolution against the ?key= property.
    assert_eq!(values(&a, "/Person/Ada->MENTORS @| count"), ["1"]);
}
