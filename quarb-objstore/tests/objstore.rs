//! Live tests against public buckets (network-gated: `--ignored`).

use quarb_objstore::ObjstoreAdapter;

fn values(a: &impl quarb::AstAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    }
}

#[test]
#[ignore = "needs network"]
fn gcs_public_listing() {
    let a = ObjstoreAdapter::connect("gs://gcp-public-data-landsat").unwrap();
    let v = values(&a, "/*<prefix> @| count");
    assert!(v[0].parse::<i64>().unwrap() >= 3, "prefixes: {v:?}");
}

#[test]
#[ignore = "needs network"]
fn s3_public_listing_and_content() {
    let a = ObjstoreAdapter::connect("s3://noaa-ghcn-pds").unwrap();
    assert_eq!(
        values(&a, "/*[:::name = \"ghcnd-states.txt\"]::;size"),
        ["1086"]
    );
    let content = values(&a, "/*[:::name = \"ghcnd-countries.txt\"]::");
    assert!(content[0].contains("Afghanistan"));
}
