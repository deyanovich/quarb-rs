//! Live battery, gated on QUARB_ARANGO_TEST (a reachable
//! ArangoDB seeded as below). Fixture: docker
//! `arangodb/arangodb` on localhost:18529, root/quarbtest,
//! database `shop`: cities (oslo, novisad), people (ada, bo),
//! edge collections lives_in {since} and mentors {hours}.

use quarb_arangodb::ArangoAdapter;

fn gate() -> Option<String> {
    std::env::var("QUARB_ARANGO_TEST").ok().map(|v| {
        if v == "1" {
            "arango://root:quarbtest@localhost:18529/shop".to_string()
        } else {
            v
        }
    })
}

fn values(a: &ArangoAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    }
}

#[test]
#[ignore = "needs a seeded local ArangoDB (QUARB_ARANGO_TEST=1)"]
fn live_arangodb() {
    let Some(target) = gate() else { return };
    let a = ArangoAdapter::connect(&target).unwrap();

    // Document collections at the root; edge collections are
    // link fabric, not tables.
    assert_eq!(values(&a, "/*:::name"), ["cities", "people"]);
    assert_eq!(values(&a, "/people::::n-rows"), ["2"]);

    // _key names documents; bodies are JSON subtrees with dual
    // exposure.
    assert_eq!(values(&a, "/people/*:::name"), ["ada", "bo"]);
    assert_eq!(values(&a, "/people/ada::role"), ["engineer"]);
    assert_eq!(values(&a, "/people/ada/langs/* @| count"), ["2"]);
    assert_eq!(values(&a, "/cities/*[::pop > 500000]::name"), ["Oslo"]);

    // Edge collections are typed crosslinks, both directions,
    // their attributes on $-.
    assert_eq!(values(&a, "/people/ada->lives_in::name"), ["Oslo"]);
    assert_eq!(values(&a, "/people/bo<-mentors::name"), ["Ada"]);
    assert_eq!(
        values(&a, "/people/*->lives_in[$-::since > 2020]::name"),
        ["Novi Sad"]
    );

    // _id-convention resolution: bare key with a collection hint.
    assert_eq!(values(&a, "/people/ada::city-->cities::country"), ["NO"]);
}
