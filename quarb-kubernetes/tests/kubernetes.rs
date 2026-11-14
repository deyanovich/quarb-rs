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
        [
            "/deployments",
            "/namespaces",
            "/nodes",
            "/pods",
            "/replicasets"
        ]
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
        v("/namespaces/default/pods/web-1/spec/containers/1/image::"),
        ["nginx:alpine"]
    );

    // The record of a node is what it answers as properties: an
    // object's identity and its labels, a map's flat entries. It
    // listed the children (`metadata`, `spec`, `status`) with nulls
    // under them, and a map's entries as nulls.
    let pod = v("/namespaces/default/pods/web-1 | %");
    assert!(
        pod[0].starts_with(r#"%(name = "web-1"; namespace = "default"; kind = "Pod"; "#),
        "{pod:?}"
    );
    assert!(pod[0].contains(r#"phase = "Running""#), "{pod:?}");
    assert!(pod[0].contains(r#"app = "web""#), "{pod:?}");
    assert!(!pod[0].contains("null"), "{pod:?}");
    assert!(!pod[0].contains("metadata"), "{pod:?}");
    // a flat entry of a map is a property too, as a JSON object's
    assert_eq!(
        v("/namespaces/default/pods/web-1/metadata::name"),
        ["web-1"]
    );
    assert_eq!(
        v("/namespaces/default/pods/web-1/spec/containers/1 | %(::image)"),
        [r#"%(image = "nginx:alpine")"#]
    );
    // a listing has no properties, and the serializers are untouched
    assert_eq!(v("/namespaces/default/pods | %"), ["%()"]);
    assert!(v("/namespaces/default/pods/web-1 | json")[0].starts_with(r#"{"metadata": {"#));

    // Owner chain: Pod ~> ReplicaSet ~> Deployment, the targets
    // arriving by single GET.
    assert_eq!(
        v("/namespaces/default/pods/web-1::owner-->::owner-->::name"),
        ["web"]
    );

    // Placement and namespace references.
    assert_eq!(v("/pods/web-1->node::name"), ["worker-1"]);

    // The links are read off the object they leave, and the cluster
    // records nothing of what points at one: walking a link back is
    // refused, never answered empty (spec: The adapter contract) —
    // the backward hop, the headless one and the navop wildcard. The
    // reverse arrow over a scope looks for the referrers itself.
    let refused = |q: &str| match quarb::run(q, &a) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("{q} was answered"),
    };
    for q in [
        "/nodes/worker-1<-node",
        "/nodes/worker-1<-*",
        "/nodes/worker-1--node",
        "/nodes/worker-1~*",
    ] {
        let e = refused(q);
        assert!(e.contains("does not support backlinks"), "{q}: {e}");
    }
    // The edges table is another matter (the user's answer of
    // 2026-11-10): every edge among the nodes of a set leaves one of
    // them, so the table is read off the set's forward links and is
    // short of nothing. A set no edge runs within has none.
    assert_eq!(v("/pods/* @| edges(<-node) | dim"), ["0, 4"]);
    assert_eq!(v("/pods/* @| edges(--*) | dim"), ["0, 4"]);
    assert_eq!(
        v("/*/* @| edges(<-node) | #(:source; :target; :label) @| csv"),
        ["source,target,label\n/pods/web-1,/nodes/worker-1,node\n/pods/db-1,/nodes/worker-1,node"]
    );
    assert_eq!(v("/*/* @| edges(--*) | dim"), v("/*/* @| edges(->*) | dim"));
    assert!(refused("/nodes/worker-1<-node").contains("'<-' walks links back"));
    assert!(refused("/nodes/worker-1--node").contains("'--' walks links back"));
    // a listing is nothing's target: nothing is there to walk back
    assert_eq!(v("/nodes<-*"), [] as [&str; 0]);
    // forward as before, and the referrers found in a scope
    assert_eq!(
        v("/nodes/worker-1<--(/pods/*::node)::name"),
        ["web-1", "db-1"]
    );
    assert!(refused("/nodes/worker-1<-node").contains("'<--(/scope/*::a)'"));
    assert_eq!(v("/pods/sys-1::namespace-->::phase"), ["Active"]);

    // Kind traits; the namespace hybrid answers fields and holds
    // listings.
    assert_eq!(v("/pods/*<pod> @| count"), ["3"]);
    assert_eq!(v("/namespaces/default::phase"), ["Active"]);
    assert_eq!(v("/namespaces/default:::n-fields"), ["3"]);
}
