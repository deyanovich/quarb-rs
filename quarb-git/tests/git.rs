//! Offline tests over a purpose-built temporary repository.

use quarb_git::GitAdapter;
use std::process::Command;

fn sh(dir: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .env("GIT_AUTHOR_NAME", "Ada")
        .env("GIT_AUTHOR_EMAIL", "ada@example.org")
        .env("GIT_COMMITTER_NAME", "Ada")
        .env("GIT_COMMITTER_EMAIL", "ada@example.org")
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// init → two commits on master (a file changes between them) → a
/// branch with one commit → merge, and a tag on the first commit.
fn fixture(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-git-test-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    sh(&dir, &["init", "-q", "-b", "master"]);
    std::fs::write(dir.join("src/lib.rs"), "pub fn one() {}\n").unwrap();
    std::fs::write(dir.join("README.md"), "v1\n").unwrap();
    sh(&dir, &["add", "-A"]);
    sh(&dir, &["commit", "-q", "-m", "first: skeleton"]);
    sh(&dir, &["tag", "start"]);
    std::fs::write(dir.join("src/lib.rs"), "pub fn one() {}\npub fn two() {}\n").unwrap();
    sh(&dir, &["add", "-A"]);
    sh(&dir, &["commit", "-q", "-m", "second: grow"]);
    sh(&dir, &["checkout", "-q", "-b", "feature", "start"]);
    std::fs::write(dir.join("FEATURE.md"), "f\n").unwrap();
    sh(&dir, &["add", "-A"]);
    sh(&dir, &["commit", "-q", "-m", "feature: add"]);
    sh(&dir, &["checkout", "-q", "master"]);
    sh(
        &dir,
        &["merge", "-q", "--no-ff", "-m", "merge: feature", "feature"],
    );
    dir
}

fn values(a: &GitAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    }
}

#[test]
fn refs_commits_and_time_travel() {
    let dir = fixture("refs");
    let a = GitAdapter::open(&dir).unwrap();
    // Refs enumerate; HEAD aliases the merge commit.
    assert_eq!(values(&a, "/branches/* @| count"), ["2"]);
    assert_eq!(values(&a, "/tags/*::subject"), ["first: skeleton"]);
    assert_eq!(values(&a, "/HEAD::subject"), ["merge: feature"]);
    assert_eq!(values(&a, "/HEAD::author"), ["Ada"]);
    // All four commits enumerate.
    assert_eq!(values(&a, "/commits/* @| count"), ["4"]);
    // Time travel: the file at the tag vs at HEAD.
    assert_eq!(
        values(&a, "/tags/start/src/lib.rs::"),
        ["pub fn one() {}\n"]
    );
    assert_eq!(values(&a, "/branches/master/src/lib.rs::;size"), ["32"]);
}

#[test]
fn dag_navigation() {
    let dir = fixture("dag");
    let a = GitAdapter::open(&dir).unwrap();
    // Bare ~> walks first parents; the merge has two via ->.
    assert_eq!(values(&a, "/HEAD::parent~>::subject"), ["second: grow"]);
    assert_eq!(values(&a, "/HEAD->parent @| count"), ["2"]);
    assert_eq!(values(&a, "/HEAD::;n-parents"), ["2"]);
    // Backlinks: the tag commit fathered two branches of history.
    assert_eq!(values(&a, "/tags/start<-parent @| count"), ["2"]);
    // Rev-syntax aliasing through children_named, no enumeration.
    assert_eq!(values(&a, "/commits/'HEAD~1'::subject"), ["second: grow"]);
    assert_eq!(values(&a, "/commits/'start'::subject"), ["first: skeleton"]);
}

#[test]
fn history_analytics() {
    let dir = fixture("analytics");
    let a = GitAdapter::open(&dir).unwrap();
    assert_eq!(values(&a, "/commits/*[::author = \"Ada\"] @| count"), ["4"]);
    assert_eq!(
        values(
            &a,
            "/commits/*[::subject *= \"feature\"] | ::subject @| sort"
        ),
        ["feature: add", "merge: feature"]
    );
    assert_eq!(values(&a, "/HEAD//*<blob> @| count"), ["3"]);
}

#[test]
fn diff_surface() {
    let dir = fixture("diff");
    let a = GitAdapter::open(&dir).unwrap();
    // `git log -- src/lib.rs`: the two commits that touched it.
    assert_eq!(
        values(&a, "/commits/*[/src/lib.rs<changed>] | ::subject @| sort"),
        ["first: skeleton", "second: grow"]
    );
    // Tree-prefix form: anything under src/.
    assert_eq!(values(&a, "/commits/*[/src<changed>] @| count"), ["2"]);
    // The changed list and its count (a clean merge changes
    // nothing, matching git's own diff of a merge).
    assert_eq!(values(&a, "/HEAD::;n-changed"), ["0"]);
    assert_eq!(values(&a, "/commits/'HEAD~1'::changed"), ["src/lib.rs"]);
}

#[test]
fn tag_metadata() {
    let dir = fixture("tagmeta");
    let a = GitAdapter::open(&dir).unwrap();
    // The `start` tag sits on the first commit; nothing else is
    // tagged.
    assert_eq!(values(&a, "/tags/start::;tags"), ["start"]);
    assert_eq!(values(&a, "/HEAD::;n-tags"), ["0"]);
    assert_eq!(
        values(&a, "/commits/*[::;n-tags > 0] | ::subject"),
        ["first: skeleton"]
    );
    // `git describe`: the nearest tagged ancestor, as a targeted
    // proximal walk.
    assert_eq!(
        values(&a, "/HEAD(->parent)+[::;n-tags > 0]? | ::;tags"),
        ["start"]
    );
}
