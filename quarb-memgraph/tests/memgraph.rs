//! Live battery, gated on QUARB_MEMGRAPH_TEST (a reachable
//! Memgraph seeded as below). Fixture: docker
//! `memgraph/memgraph` on localhost:17687 (Bolt, no auth):
//!   City: Oslo/NO, Novi Sad/RS
//!   Person: Ada (engineer), Bo (analyst)
//!   Ada -LIVES_IN{since:2019}-> Oslo,
//!   Bo -LIVES_IN{since:2021}-> Novi Sad,
//!   Ada -MENTORS{hours:3}-> Bo

use quarb_memgraph::MemgraphAdapter;

fn gate() -> Option<String> {
    std::env::var("QUARB_MEMGRAPH_TEST").ok().map(|v| {
        if v == "1" {
            "memgraph://localhost:17687?key=name".to_string()
        } else {
            v
        }
    })
}

fn values(a: &MemgraphAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    }
}

#[test]
#[ignore = "needs a seeded local Memgraph (QUARB_MEMGRAPH_TEST=1)"]
fn live_memgraph() {
    let Some(target) = gate() else { return };
    let a = MemgraphAdapter::connect(&target).unwrap();

    assert_eq!(values(&a, "/*:::name"), ["City", "Person"]);
    assert_eq!(values(&a, "/Person;;;n-rows"), ["2"]);
    assert_eq!(values(&a, "/Person/*:::name"), ["Ada", "Bo"]);
    assert_eq!(values(&a, "/Person/Ada::role"), ["engineer"]);
    assert_eq!(values(&a, "/City/*[::country = \"RS\"]::name"), ["Novi Sad"]);

    assert_eq!(values(&a, "/Person/Ada->LIVES_IN::name"), ["Oslo"]);
    assert_eq!(values(&a, "/Person/Bo<-MENTORS::name"), ["Ada"]);
    assert_eq!(
        values(&a, "/Person/*->LIVES_IN[$-::since > 2020]::name"),
        ["Novi Sad"]
    );
    assert_eq!(values(&a, "/Person/Bo::name~>Person::role"), ["analyst"]);
}
