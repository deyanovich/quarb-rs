//! Live tests, gated on `QUARB_DATASTORE` (a target with the
//! quarb-ds music fixture). Run with `-- --ignored`.
use quarb_datastore::DatastoreAdapter;

#[test]
#[ignore = "needs QUARB_DATASTORE and network"]
fn native_key_references() {
    let Ok(t) = std::env::var("QUARB_DATASTORE") else {
        return;
    };
    let a = DatastoreAdapter::connect(&t).unwrap();
    let v = |q: &str| match quarb::run(q, &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    };
    assert_eq!(v("/tracks/* @| count"), ["3"]);
    assert_eq!(v("/tracks/mars::album~>::artist~>::name"), ["Holst"]);
    assert_eq!(v("/albums/mikrokosmos->artist::name"), ["Bartok"]);
}
