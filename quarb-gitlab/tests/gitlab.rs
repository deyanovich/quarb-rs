//! End-to-end tests against a mock GitLab: `QUARB_GLAB` points
//! at a generated shell script serving the canned JSON in
//! `tests/fixtures/` (path `groups/tesslab/projects` →
//! `groups_tesslab_projects.json`; the adapter's pagination
//! suffix is stripped, other query strings are part of the
//! name), so the whole adapter — deep group addressing, mixed
//! group children, sections, member edge data, fork backlinks —
//! runs its real subprocess transport offline.
//!
//! One test function: the env var is process-global.

use quarb_gitlab::GitlabAdapter;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

#[test]
fn mock_gitlab() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("glab");
    let fixtures = format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"));
    let mut f = std::fs::File::create(&script).unwrap();
    write!(
        f,
        "#!/bin/sh\n\
         [ \"$1\" = api ] || exit 2\n\
         p=$(printf %s \"$2\" | sed 's/[?&]per_page=100&page=[0-9]*$//')\n\
         f=\"{fixtures}/$(printf %s \"$p\" | tr / _).json\"\n\
         [ -f \"$f\" ] || {{ echo \"404 Not Found: $p\" >&2; exit 1; }}\n\
         cat \"$f\"\n"
    )
    .unwrap();
    drop(f);
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    // SAFETY: single-threaded test binary (one #[test]).
    unsafe { std::env::set_var("QUARB_GLAB", &script) };

    let a = GitlabAdapter::connect("gitlab:").unwrap();
    let v = |q: &str| match quarb::run(q, &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    };

    // Deep addressing: group / subgroup / project, one GET per
    // segment; a group's children mix subgroups and projects.
    assert_eq!(v("/tesslab/instruments/gauge::name"), ["gauge"]);
    assert_eq!(v("/tesslab/*"), ["/tesslab/instruments", "/tesslab/kettle"]);
    assert_eq!(v("/tesslab//*<project>::name @| sort"), ["gauge", "kettle"]);
    // <repo> is the forge-neutral alias for <project>.
    assert_eq!(v("/tesslab//*<repo>::name @| sort"), ["gauge", "kettle"]);
    assert_eq!(v("/tesslab/kettle::stars"), ["128"]);
    assert_eq!(v("/tesslab/kettle<thermo>::description"), [
        "Boils anything. Library."
    ]);

    // Issues: the open listing, labels as traits (scoped labels
    // included), closed by direct address.
    assert_eq!(v("/tesslab/kettle/issues/* @| count"), ["2"]);
    assert_eq!(v("/tesslab/kettle/issues/*<bug>::title"), ["Whistle never stops"]);
    assert_eq!(
        v("/tesslab/kettle/issues/*<'temp::high'>::author"),
        ["grace"]
    );
    assert_eq!(v("/tesslab/kettle/issues/2::state"), ["closed"]);
    assert_eq!(v("/tesslab/kettle/issues/4::"), ["Steam escapes the schedule."]);

    // Merge requests and releases.
    assert_eq!(v("/tesslab/kettle/mrs/5<draft>::title"), ["Add pressure valve"]);
    assert_eq!(v("/tesslab/kettle/mrs/5->reviewer::username"), ["grace"]);
    assert_eq!(v("/tesslab/kettle/mrs/5::source-branch"), ["valve"]);
    assert_eq!(v("/tesslab/kettle/releases/*::tag"), ["v1.0"]);

    // Pipelines and their jobs, statuses as traits.
    assert_eq!(v("/tesslab/kettle/pipelines/* @| count"), ["2"]);
    assert_eq!(
        v("/tesslab/kettle/pipelines/*<failed>/*<failed>::name"),
        ["test"]
    );
    assert_eq!(v("/tesslab/kettle/pipelines/902::ref"), ["valve"]);

    // The file tree; content is the value.
    assert_eq!(v("/tesslab/kettle/files/src/main.rs;;;path"), [
        "/tesslab/kettle/files/src/main.rs"
    ]);
    assert_eq!(v("/tesslab/kettle/files/README.md::"), [
        "# kettle\n\nBoils anything. Library.\n"
    ]);

    // Member edges carry access as edge data.
    assert_eq!(
        v("/tesslab/kettle(->member[$-::role = 'developer'])::username"),
        ["grace"]
    );
    assert_eq!(
        v("/tesslab(->member[$-::access >= 50])::username"),
        ["ada"]
    );

    // Forks: parent property, reference, and backlinks.
    assert_eq!(v("/ada/*<fork>::parent"), ["tesslab/kettle"]);
    assert_eq!(v("/ada/kettle::parent~>::stars"), ["128"]);
    assert_eq!(v("/tesslab/kettle<-parent"), ["/ada/kettle"]);

    // Instants.
    assert_eq!(
        v("/tesslab/kettle/issues/*[::created > 2026-06-01] @| count"),
        ["1"]
    );

    // An anchored target roots its entity.
    let b = GitlabAdapter::connect("gitlab:tesslab/instruments").unwrap();
    let vb = |q: &str| match quarb::run(q, &b).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| b.locator(n)).collect(),
    };
    assert_eq!(vb("/*/*::name"), ["gauge"]);
}
