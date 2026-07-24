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
        values(&a, "/*[:::name = \"ghcnd-states.txt\"];;;size"),
        ["1086"]
    );
    let content = values(&a, "/*[:::name = \"ghcnd-countries.txt\"]::");
    assert!(content[0].contains("Afghanistan"));
}

/// Signed-request round trip against any S3-compatible endpoint
/// that VALIDATES SigV4 (MinIO does; see quarb-aws's signer pin).
/// Point QUARB_S3_TEST at `s3://bucket?endpoint=http://…` with
/// AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY in the environment,
/// the bucket holding `configs/services.json`.
#[test]
#[ignore = "needs QUARB_S3_TEST + credentials (e.g. a seeded MinIO)"]
fn s3_signed_listing_and_content() {
    let target = std::env::var("QUARB_S3_TEST").expect("QUARB_S3_TEST target");
    let a = ObjstoreAdapter::connect(&target).unwrap();
    let names = values(&a, "/configs/*:::name");
    assert!(
        names.contains(&"services.json".to_string()),
        "listing under a signed prefix: {names:?}"
    );
    let content = values(&a, "/configs/services.json::");
    assert!(content[0].contains("services"), "signed content read");
}

/// SharedKey round trip against any Blob-compatible endpoint that
/// VALIDATES signatures (Azurite does). Point QUARB_AZ_TEST at
/// `az://account/container?endpoint=http://…` with
/// AZURE_STORAGE_KEY in the environment, the container holding
/// `readme.md`.
#[test]
#[ignore = "needs QUARB_AZ_TEST + AZURE_STORAGE_KEY (e.g. a seeded Azurite)"]
fn azure_shared_key_listing_and_content() {
    let target = std::env::var("QUARB_AZ_TEST").expect("QUARB_AZ_TEST target");
    let a = ObjstoreAdapter::connect(&target).unwrap();
    let names = values(&a, "/*:::name");
    assert!(
        names.contains(&"readme.md".to_string()),
        "signed listing: {names:?}"
    );
    let content = values(&a, "/readme.md::");
    assert!(!content[0].is_empty(), "signed content read");
}
