//! End-to-end tests against a mock GitHub: `QUARB_GH` points at
//! a generated shell script serving the canned JSON in
//! `tests/fixtures/` (path `/users/ada/repos` →
//! `_users_ada_repos.json`, query strings stripped), so the
//! whole adapter — direct addressing, sections, the edge fabric
//! in both directions, edge data — runs its real subprocess
//! transport offline.
//!
//! One test function: the env var is process-global.

use quarb_github::GithubAdapter;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

#[test]
fn mock_github() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("gh");
    let fixtures = format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"));
    let mut f = std::fs::File::create(&script).unwrap();
    write!(
        f,
        "#!/bin/sh\n\
         [ \"$1\" = api ] || exit 2\n\
         p=\"${{2%%\\?*}}\"\n\
         f=\"{fixtures}/$(printf %s \"$p\" | tr / _).json\"\n\
         [ -f \"$f\" ] || {{ echo \"HTTP 404: no fixture for $p\" >&2; exit 1; }}\n\
         cat \"$f\"\n"
    )
    .unwrap();
    drop(f);
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    // SAFETY: single-threaded test binary (one #[test]).
    unsafe { std::env::set_var("QUARB_GH", &script) };

    let a = GithubAdapter::connect("github:").unwrap();
    let v = |q: &str| match quarb::run(q, &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    };

    // Addressing: users by login, repos beneath them; the tree
    // mirrors github.com URLs.
    assert_eq!(v("/ada::name"), ["Ada Lovelace"]);
    assert_eq!(v("/ada/*::name @| sort"), ["boiler", "fork-of-kettle"]);
    assert_eq!(v("/ada/boiler::stars"), ["42"]);
    assert_eq!(v("/ada/boiler::license"), ["MIT"]);

    // Topics and flags are traits.
    assert_eq!(v("/ada/*<cli>::name"), ["boiler"]);
    assert_eq!(v("/ada/*<fork>::name"), ["fork-of-kettle"]);

    // Sections: the issues listing keeps PRs out; labels are
    // traits; closed issues answer by direct address.
    assert_eq!(v("/ada/boiler/issues/* @| count"), ["1"]);
    assert_eq!(v("/ada/boiler/issues/*<bug>::title"), ["Steam leak"]);
    assert_eq!(v("/ada/boiler/issues/3::state"), ["closed"]);
    assert_eq!(v("/ada/boiler/pulls/2::head"), ["valve"]);
    assert_eq!(v("/ada/boiler/releases/*::tag"), ["v1.0"]);

    // The file tree; content is the value.
    assert_eq!(v("/ada/boiler/files/*<dir>"), ["/ada/boiler/files/src"]);
    assert_eq!(v("/ada/boiler/files/README.md::"), ["boil the ocean\n"]);
    assert_eq!(
        v("/ada/boiler/files/src/main.rs::size"),
        ["20"]
    );

    // The social fabric, both directions.
    assert_eq!(v("/ada->follows::login"), ["grace"]);
    assert_eq!(v("/grace<-follows::login"), ["ada"]);
    assert_eq!(v("/ada->org::login"), ["tesslab"]);
    assert_eq!(v("/tesslab<-org::login"), ["ada"]);
    // An org answers the <org> shorthand and the forge-neutral
    // <group> alias too.
    assert_eq!(v("/tesslab<org>::name"), ["Tesla Lab"]);
    assert_eq!(v("/tesslab<group>::name"), ["Tesla Lab"]);
    assert_eq!(v("/ada->starred::name"), ["kettle"]);
    assert_eq!(v("/tesslab/kettle<-parent::full-name"), ["ada/fork-of-kettle"]);

    // A fork's upstream resolves (the listing object lacks
    // `parent`; the adapter refetches the full repo) — as a
    // property and as a reference.
    assert_eq!(v("/ada/*<fork>::parent"), ["tesslab/kettle"]);
    assert_eq!(v("/ada/fork-of-kettle::parent~>::owner"), ["tesslab"]);

    // The contributor edge carries data.
    assert_eq!(
        v("/ada/boiler(->contributor[$-::contributions > 100])::login"),
        ["ada"]
    );

    // Issue references: author and assignee edges.
    assert_eq!(v("/ada/boiler/issues/1->author::login"), ["grace"]);
    assert_eq!(v("/ada/boiler/issues/1->assignee::login"), ["ada"]);
    assert_eq!(v("/ada/boiler/issues/1::author~>::bio"), ["compiler pioneer"]);

    // Instants and bodies.
    assert_eq!(v("/ada/*[::created > 2015-01-01] @| count"), ["2"]);
    assert_eq!(v("/ada/boiler/issues/1::"), ["It hisses."]);

    // An anchored target roots its entity.
    let b = GithubAdapter::connect("github:ada/boiler").unwrap();
    let vb = |q: &str| match quarb::run(q, &b).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| b.locator(n)).collect(),
    };
    assert_eq!(vb("/*::stars"), ["42"]);
}
