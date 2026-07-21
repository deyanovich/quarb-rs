//! End-to-end tests against a mock cluster: `QUARB_KUBECTL`
//! points at a generated shell script that serves the canned
//! JSON in `tests/fixtures/` (path `/api/v1/pods` → file
//! `_api_v1_pods.json`), so the whole adapter — discovery,
//! listings, owner chains, single-object fetches — runs its
//! real subprocess transport with no cluster and no kubectl.
//!
//! One test function: the env var is process-global.

use quarb_kubernetes::KubernetesAdapter;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

#[test]
fn mock_cluster() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("kubectl");
    let fixtures = format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"));
    let mut f = std::fs::File::create(&script).unwrap();
    write!(
        f,
        "#!/bin/sh\n\
         while [ $# -gt 0 ] && [ \"$1\" != --raw ]; do shift; done\n\
         [ \"$1\" = --raw ] || exit 2\n\
         f=\"{fixtures}/$(printf %s \"$2\" | tr / _).json\"\n\
         [ -f \"$f\" ] || {{ echo \"no fixture for $2\" >&2; exit 1; }}\n\
         cat \"$f\"\n"
    )
    .unwrap();
    drop(f);
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    // SAFETY: single-threaded test binary (one #[test]).
    unsafe { std::env::set_var("QUARB_KUBECTL", &script) };

    let a = KubernetesAdapter::connect("k8s:").unwrap();
    let v = |q: &str| match quarb::run(q, &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    };

    // Discovery: subresources and list-less resources dropped,
    // listings sorted.
    assert_eq!(
        v("/*"),
        ["/deployments", "/namespaces", "/nodes", "/pods", "/replicasets"]
    );

    // The all-namespaces view beside the scoped one.
    assert_eq!(v("/pods/* @| count"), ["3"]);
    assert_eq!(v("/namespaces/default/pods/* @| count"), ["2"]);

    // Properties, label fallthrough, instants.
    assert_eq!(v("/namespaces/default/pods/web-1::phase"), ["Running"]);
    assert_eq!(v("/pods/*[::app = 'web']::name"), ["web-1"]);
    assert_eq!(v("/pods/*[::created > 2026-01-01] @| count"), ["2"]);

    // The object's JSON descends as tree.
    assert_eq!(
        v("/namespaces/default/pods/web-1/spec/containers/0/image::"),
        ["nginx:alpine"]
    );

    // Owner chain: Pod ~> ReplicaSet ~> Deployment, the targets
    // arriving by single GET.
    assert_eq!(
        v("/namespaces/default/pods/web-1::owner~>::owner~>::name"),
        ["web"]
    );

    // Placement and namespace references.
    assert_eq!(v("/pods/web-1->node::name"), ["worker-1"]);
    assert_eq!(v("/pods/sys-1::namespace~>::phase"), ["Active"]);

    // Kind traits; the namespace hybrid answers fields and holds
    // listings.
    assert_eq!(v("/pods/*<pod> @| count"), ["3"]);
    assert_eq!(v("/namespaces/default::phase"), ["Active"]);
    assert_eq!(v("/namespaces/default;;;n-fields"), ["3"]);
}
