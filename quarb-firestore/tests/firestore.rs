//! Live tests, gated on `QUARB_FIRESTORE` (a target with the
//! quarb-fs music fixture). Run with `-- --ignored`.
use quarb_firestore::FirestoreAdapter;

#[test]
#[ignore = "needs QUARB_FIRESTORE and network"]
fn native_references() {
    let Ok(t) = std::env::var("QUARB_FIRESTORE") else {
        return;
    };
    let a = FirestoreAdapter::connect(&t).unwrap();
    let v = |q: &str| match quarb::run(q, &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    };
    assert_eq!(v("/tracks/* @| count"), ["3"]);
    assert_eq!(v("/tracks/mars::album~>::artist~>::name"), ["Holst"]);
    assert_eq!(v("/tracks/venus->album::title"), ["The Planets"]);
    assert_eq!(v("/tracks/*[::price < 1] @| count"), ["2"]);
}
