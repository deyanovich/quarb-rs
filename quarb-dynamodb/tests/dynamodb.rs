//! Live-gated integration tests: point QUARB_DYNAMODB_TEST at a
//! seeded endpoint (e.g. DynamoDB Local carrying the artists /
//! tracks fixture from the repo's verification battery) with
//! credentials in the environment, then
//! `cargo test -p quarb-dynamodb -- --ignored`.

use quarb_dynamodb::DynamodbAdapter;

fn values(a: &DynamodbAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    }
}

#[test]
#[ignore = "needs QUARB_DYNAMODB_TEST + credentials (e.g. seeded DynamoDB Local)"]
fn scan_filter_resolve() {
    let target = std::env::var("QUARB_DYNAMODB_TEST").expect("QUARB_DYNAMODB_TEST");
    let a = DynamodbAdapter::connect(&target).unwrap();
    // deterministic key-sorted listing, hash-named items repeating
    assert_eq!(values(&a, "/tracks/*:::name"), ["Bartok", "Bartok", "Holst"]);
    // the sort key stays a property to filter on
    assert_eq!(values(&a, "/tracks/Bartok[::song = 'Ostinato']::secs"), ["105"]);
    // nested attribute trees: map field, list elements
    assert_eq!(values(&a, "/artists/Holst/contact::fee"), ["1200"]);
    assert_eq!(values(&a, "/artists/Holst/labels/* @| count"), ["2"]);
    // hinted resolution is a GetItem, hint-less tries the _id stem
    assert_eq!(
        values(&a, "/tracks/*[::song = 'Jupiter']::artist_id~>artists::country"),
        ["England"]
    );
    assert_eq!(
        values(&a, "/tracks/*[::song = 'Bourree']::artist_id~>::country"),
        ["Hungary"]
    );
}
