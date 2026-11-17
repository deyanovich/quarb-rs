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
    assert_eq!(values(&a, "/branches/master/src/lib.rs:::size"), ["32_B"]);
}

#[test]
fn dag_navigation() {
    let dir = fixture("dag");
    let a = GitAdapter::open(&dir).unwrap();
    // Bare ~> walks first parents; the merge has two via ->.
    assert_eq!(values(&a, "/HEAD::parent-->::subject"), ["second: grow"]);
    assert_eq!(values(&a, "/HEAD->parent @| count"), ["2"]);
    assert_eq!(values(&a, "/HEAD:::n-parents"), ["2"]);
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
    assert_eq!(values(&a, "/HEAD:::n-changed"), ["0"]);
    assert_eq!(values(&a, "/commits/'HEAD~1'::changed"), ["src/lib.rs"]);
}

#[test]
fn tag_metadata() {
    let dir = fixture("tagmeta");
    let a = GitAdapter::open(&dir).unwrap();
    // The `start` tag sits on the first commit; nothing else is
    // tagged.
    assert_eq!(values(&a, "/tags/start:::tags"), ["start"]);
    assert_eq!(values(&a, "/HEAD:::n-tags"), ["0"]);
    assert_eq!(
        values(&a, "/commits/*[:::n-tags > 0] | ::subject"),
        ["first: skeleton"]
    );
    // `git describe`: the nearest tagged ancestor, as a targeted
    // proximal walk.
    assert_eq!(
        values(&a, "/HEAD(->parent)+[:::n-tags > 0]? | :::tags"),
        ["start"]
    );
}

#[test]
fn provenance_repo_and_commit_instant() {
    let dir = fixture("prov");
    let a = GitAdapter::open(&dir).unwrap();
    // A commit's :instant is its author date — the same instant
    // ::date mints; ref aliases answer for their commit.
    assert_eq!(values(&a, "/HEAD:instant"), values(&a, "/HEAD::date"));
    // A tree entry answers "this repo, as of that commit".
    assert_eq!(
        values(&a, "/tags/start/src/lib.rs:instant"),
        ["2020-01-01T00:00:00Z"]
    );
    // The source is the repository path, on every node; structural
    // dirs have no commit ancestor, so no instant (null displays
    // empty), and git assigns no dpid.
    assert_eq!(values(&a, "/commits:source"), [dir.display().to_string()]);
    assert_eq!(values(&a, "/commits:instant"), [""]);
    assert_eq!(values(&a, "/HEAD:dpid"), [""]);
    // The composite: repo, plus the commit instant where one exists.
    assert_eq!(
        values(&a, "/tags/start:provenance"),
        [format!("?{}@2020-01-01T00:00:00Z", dir.display())]
    );
}

/// Finding 22: a blob is bytes; it decodes under the mount's rule
/// (`?encoding=`, a byte-order mark, else strict UTF-8) and never
/// with replacement characters — a blob that decodes under none has
/// no text.
#[test]
fn blobs_decode_under_the_mounts_rule() {
    let dir = fixture("encoding");
    // «Привет» in windows-1251.
    std::fs::write(dir.join("ru.txt"), b"\xcf\xf0\xe8\xe2\xe5\xf2\n").unwrap();
    sh(&dir, &["add", "-A"]);
    sh(&dir, &["commit", "-q", "-m", "ru: a cp1251 file"]);
    let strict = GitAdapter::open(&dir).unwrap();
    match quarb::run("/branches/master/ru.txt::", &strict).unwrap() {
        quarb::QueryResult::Values(vs) => {
            assert!(vs.iter().all(|v| matches!(v, quarb::Value::Null)), "{vs:?}")
        }
        other => panic!("{other:?}"),
    }
    let decoding = quarb::source::SourceDecoding::default()
        .with_option(Some("windows-1251"))
        .unwrap();
    let ru = GitAdapter::open_with_decoding(&dir, decoding).unwrap();
    assert_eq!(values(&ru, "/branches/master/ru.txt::"), ["Привет\n"]);
}

/// The pushdown scanning round (2026-11-06): a prefix over the
/// history walks k commits, and the full enumeration finds the same
/// commit nodes again.
#[test]
fn a_prefix_over_the_history_walks_k_commits() {
    let dir = fixture("prefix");
    let a = GitAdapter::open(&dir).unwrap();
    let first = match quarb::run("/commits/* @| [..2]", &a).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns,
        other => panic!("{other:?}"),
    };
    assert_eq!(first.len(), 2);
    let all = match quarb::run("/commits/*", &a).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns,
        other => panic!("{other:?}"),
    };
    assert!(all.len() > 2);
    assert_eq!(&all[..2], &first[..]);
    let subjects = match quarb::run("/commits/* @| [..2] | ::subject", &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    let all_subjects = match quarb::run("/commits/* | ::subject", &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    assert_eq!(subjects, all_subjects[..2]);
}

/// A walk along `parent` reads ahead. Each commit a walk arrived at
/// was read by its own `git show`: a walk of n commits started n
/// processes, and a history of 1,568 commits took 5.8 s to walk
/// where listing it takes 0.08 s. A commit the walk arrives at is
/// now read with the next of its ancestors, twice as many at each
/// read, and the values are the listing's own.
#[test]
fn a_walk_along_the_history_reads_ahead() {
    let dir = std::env::temp_dir().join("quarb-git-test-ahead");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    sh(&dir, &["init", "-q", "-b", "master"]);
    for n in 1..=40 {
        sh(
            &dir,
            &["commit", "-q", "--allow-empty", "-m", &format!("canto {n}")],
        );
    }
    // a commit asked for by name is one commit read
    let a = GitAdapter::open(&dir).unwrap();
    assert_eq!(values(&a, "/commits/HEAD | ::subject"), ["canto 40"]);
    assert_eq!(a.header_reads(), 1);
    // thirty more along the walk: reads of 2, 4, 8 and 16 commits
    let walked = values(&a, "/commits/HEAD(->parent){;30} | ::subject");
    assert_eq!(walked.len(), 31);
    assert_eq!(walked[0], "canto 40");
    assert_eq!(walked[30], "canto 10");
    assert_eq!(a.header_reads(), 5, "one read a commit would be 31");
    // what the walk read is what the listing reads
    let b = GitAdapter::open(&dir).unwrap();
    let listed = values(&b, "/commits/* | %(::subject; ::author; ::date; ::parent)");
    assert_eq!(b.header_reads(), 1);
    assert_eq!(
        values(
            &a,
            "/commits/HEAD(->parent){;30} | %(::subject; ::author; ::date; ::parent)"
        ),
        listed[..31]
    );
    // and nothing more was started for it
    assert_eq!(a.header_reads(), 5);
    // a second commit by name, off any walk, is again one commit
    let c = GitAdapter::open(&dir).unwrap();
    assert_eq!(values(&c, "/commits/'HEAD~20' | ::subject"), ["canto 20"]);
    assert_eq!(values(&c, "/commits/'HEAD~3' | ::subject"), ["canto 37"]);
    assert_eq!(c.header_reads(), 2);
}

/// A sweep of the refs reads their commits together (the user,
/// 2026-11-17, on `/tags/* | ::subject`). It started one `git show`
/// a ref: 448 tags on 167 commits took 0.7 s, where reading those
/// commits in one process takes 0.03 s. The commit of a listed ref
/// is now read with the commits of the refs after it in the
/// listing, twice as many at each read, and the values are the
/// listing's own.
#[test]
fn a_sweep_of_the_refs_reads_their_commits_together() {
    let dir = std::env::temp_dir().join("quarb-git-test-tips");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    sh(&dir, &["init", "-q", "-b", "master"]);
    for n in 1..=30 {
        sh(
            &dir,
            &["commit", "-q", "--allow-empty", "-m", &format!("canto {n}")],
        );
        sh(&dir, &["tag", &format!("c{n:02}")]);
    }
    // thirty tags: reads of 2, 4, 8 and 16 commits
    let a = GitAdapter::open(&dir).unwrap();
    let swept = values(&a, "/tags/* | ::subject");
    assert_eq!(swept.len(), 30);
    assert_eq!(swept[0], "canto 1");
    assert_eq!(swept[29], "canto 30");
    assert_eq!(a.header_reads(), 4, "one read a tag would be 30");
    // what the sweep read is what the listing reads (the listing
    // in its own order: every commit here is a ref's, of one date)
    let b = GitAdapter::open(&dir).unwrap();
    let mut listed = values(&b, "/commits/* | %(::subject; ::author; ::date; ::parent)");
    assert_eq!(b.header_reads(), 1);
    listed.sort();
    let mut by_tag = values(&a, "/tags/* | %(::subject; ::author; ::date; ::parent)");
    by_tag.sort();
    assert_eq!(by_tag, listed);
    // and nothing more was started for it, nor for a walk from a
    // tag over commits the sweep holds
    assert_eq!(values(&a, "/tags/c30(->parent){;5} | ::subject").len(), 6);
    assert_eq!(a.header_reads(), 4);
    // a tag asked for by name is one read
    let c = GitAdapter::open(&dir).unwrap();
    assert_eq!(values(&c, "/tags/c07 | ::subject"), ["canto 7"]);
    assert_eq!(c.header_reads(), 1);
    // an annotated tag is read at its commit, a second tag of a
    // commit adds no read, and the branches are swept alike
    sh(
        &dir,
        &["tag", "-a", "-m", "the last canto", "z-last", "c30"],
    );
    sh(&dir, &["branch", "earlier", "c10"]);
    sh(&dir, &["branch", "middle", "c20"]);
    let d = GitAdapter::open(&dir).unwrap();
    assert_eq!(
        values(&d, "/branches/* | ::subject"),
        ["canto 10", "canto 30", "canto 20"]
    );
    assert_eq!(d.header_reads(), 2, "two commits, then the third");
    let swept = values(&d, "/tags/* | ::subject");
    assert_eq!(swept.len(), 31);
    assert_eq!(swept[30], "canto 30");
    // twenty-seven commits are left to read: 2, 4, 8 and 13
    assert_eq!(d.header_reads(), 6);
}
