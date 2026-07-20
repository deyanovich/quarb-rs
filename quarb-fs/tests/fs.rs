//! End-to-end tests: queries run through the engine against a real
//! temporary directory tree.

use quarb_fs::{FsAdapter, FsOptions};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// Build this tree in a fresh temp dir:
/// ```text
/// root ├ src ├ main.rs
///      │     └ lib.rs
///      ├ docs └ guide.txt
///      └ README.md
/// ```
fn sample_tree() -> TempDir {
    let dir = TempDir::new().unwrap();
    let p = dir.path();
    fs::create_dir(p.join("src")).unwrap();
    fs::create_dir(p.join("docs")).unwrap();
    fs::write(p.join("src/main.rs"), "").unwrap();
    fs::write(p.join("src/lib.rs"), "").unwrap();
    fs::write(p.join("docs/guide.txt"), "").unwrap();
    fs::write(p.join("README.md"), "").unwrap();
    dir
}

/// Run a query with explicit options and return the results as
/// filenames relative to root, sorted for stable comparison.
fn names_with(query: &str, dir: &TempDir, opts: FsOptions) -> Vec<String> {
    let adapter = FsAdapter::with_options(dir.path(), opts).unwrap();
    let root = dir.path().canonicalize().unwrap();
    let nodes = match quarb::run(query, &adapter).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns,
        quarb::QueryResult::Values(_) => panic!("expected nodes"),
    };
    let mut got: Vec<String> = nodes
        .into_iter()
        .map(|n| {
            adapter
                .path(n)
                .strip_prefix(&root)
                .unwrap_or(&PathBuf::from(""))
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    got.sort();
    got
}

/// Run a projection query and return the scalar values as strings.
fn values(query: &str, dir: &TempDir) -> Vec<String> {
    let adapter = FsAdapter::new(dir.path()).unwrap();
    match quarb::run(query, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    }
}

/// Run a query with default options (ignore-aware, hidden skipped).
fn names(query: &str, dir: &TempDir) -> Vec<String> {
    names_with(query, dir, FsOptions::default())
}

/// A tree exercising ignore rules:
/// ```text
/// root ├ .gitignore   (ignores target/ and *.tmp)
///      ├ .hidden
///      ├ scratch.tmp
///      ├ src    └ main.rs
///      └ target └ junk.rs
/// ```
fn tree_with_ignore() -> TempDir {
    let dir = TempDir::new().unwrap();
    let p = dir.path();
    fs::write(p.join(".gitignore"), "target/\n*.tmp\n").unwrap();
    fs::write(p.join(".hidden"), "").unwrap();
    fs::write(p.join("scratch.tmp"), "").unwrap();
    fs::create_dir(p.join("src")).unwrap();
    fs::write(p.join("src/main.rs"), "").unwrap();
    fs::create_dir(p.join("target")).unwrap();
    fs::write(p.join("target/junk.rs"), "").unwrap();
    dir
}

#[test]
fn descendant_glob_finds_all_rust_files() {
    let dir = sample_tree();
    assert_eq!(names("//*.rs", &dir), vec!["src/lib.rs", "src/main.rs"]);
}

#[test]
fn child_path_navigates_to_one_file() {
    let dir = sample_tree();
    assert_eq!(names("/src/main.rs", &dir), vec!["src/main.rs"]);
}

#[test]
fn child_wildcard_lists_top_level() {
    let dir = sample_tree();
    assert_eq!(names("/*", &dir), vec!["README.md", "docs", "src"]);
}

#[test]
fn descendant_by_name() {
    let dir = sample_tree();
    assert_eq!(names("//guide.txt", &dir), vec!["docs/guide.txt"]);
}

#[test]
fn no_match_is_empty() {
    let dir = sample_tree();
    assert!(names("/nonexistent", &dir).is_empty());
}

#[test]
fn unsupported_operator_errors() {
    let dir = sample_tree();
    let adapter = FsAdapter::new(dir.path()).unwrap();
    // an unknown pipeline function reports "not yet supported"
    let err = quarb::run("//file @| bogus", &adapter).unwrap_err();
    assert!(matches!(err, quarb::QuarbError::Unsupported(_)));
}

#[test]
#[cfg(unix)]
fn symlink_crosslinks() {
    let dir = TempDir::new().unwrap();
    let p = dir.path();
    fs::write(p.join("real.txt"), "content").unwrap();
    std::os::unix::fs::symlink("real.txt", p.join("link.txt")).unwrap();

    // -> follows the symlink to its target
    assert_eq!(names("//link.txt->target", &dir), vec!["real.txt"]);
    // ->* matches any crosslink label
    assert_eq!(names("//link.txt->*", &dir), vec!["real.txt"]);
    // follow the link, then project the target's content
    assert_eq!(values("//link.txt->target::", &dir), vec!["content"]);
    // <- finds the symlink pointing at real.txt
    assert_eq!(names("//real.txt<-target", &dir), vec!["link.txt"]);
    // the symlink node carries the <symlink> trait
    assert_eq!(names("//*<symlink>", &dir), vec!["link.txt"]);
    // a link axis works as a structural predicate: "has any
    // outgoing crosslink" selects the symlink node
    assert_eq!(names("//*[->*]", &dir), vec!["link.txt"]);
}

#[test]
fn pipelines_and_union() {
    let dir = TempDir::new().unwrap();
    let p = dir.path();
    fs::create_dir(p.join("src")).unwrap();
    fs::write(p.join("src/a.rs"), "aa").unwrap(); // 2 bytes
    fs::write(p.join("src/b.rs"), "bbbb").unwrap(); // 4 bytes
    fs::write(p.join("notes.md"), "hi").unwrap();

    // count of code files (aggregation via @|)
    assert_eq!(values("//*<code> @| count", &dir), vec!["2"]);
    // total size of code files
    assert_eq!(values("//*<code>;;;size @| sum", &dir), vec!["6 B"]);
    // sorted joined names
    assert_eq!(
        values("//*<code>:::name @| sort @| join(\", \")", &dir),
        vec!["a.rs, b.rs"]
    );
    // union of code and text files, counted
    assert_eq!(values("//*<code> || //*<text> @| count", &dir), vec!["3"]);
}

#[test]
fn predicates() {
    let dir = TempDir::new().unwrap();
    let p = dir.path();
    fs::create_dir(p.join("src")).unwrap();
    fs::write(p.join("src/big.rs"), "x".repeat(2000)).unwrap();
    fs::write(p.join("src/small.rs"), "x").unwrap();
    fs::write(p.join("README.md"), "hello").unwrap();

    // size comparison
    assert_eq!(names("//*[;;;size > 1000]", &dir), vec!["src/big.rs"]);
    // extension equality (bare-name literal)
    assert_eq!(
        names("//*[;;;extension = rs]", &dir),
        vec!["src/big.rs", "src/small.rs"]
    );
    // regex match on the name
    assert_eq!(
        names("//*[:::name =~ ~(^small)]", &dir),
        vec!["src/small.rs"]
    );
    // structural condition: dirs that contain a big.rs
    assert_eq!(names("/*[/big.rs]", &dir), vec!["src"]);
    // boolean combination
    assert_eq!(
        names("//*[;;;is-file and ;;;size < 10]", &dir),
        vec!["README.md", "src/small.rs"]
    );
}

#[test]
fn trait_filters() {
    let dir = sample_tree(); // src/{main.rs,lib.rs}, docs/guide.txt, README.md
    // <code> matches .rs; <dir> matches directories
    assert_eq!(names("//*<code>", &dir), vec!["src/lib.rs", "src/main.rs"]);
    assert_eq!(names("/*<dir>", &dir), vec!["docs", "src"]);
    // <text> matches .md and .txt (OR via alternation too)
    assert_eq!(
        names("//*<text>", &dir),
        vec!["README.md", "docs/guide.txt"]
    );
    assert_eq!(
        names("//*<code||text>", &dir),
        vec!["README.md", "docs/guide.txt", "src/lib.rs", "src/main.rs"]
    );
    // <file> AND <code> — both required
    assert_eq!(
        names("//*<file><code>", &dir),
        vec!["src/lib.rs", "src/main.rs"]
    );
}

#[test]
fn projections_return_scalars() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("hello.txt"), "hi there").unwrap();
    assert_eq!(values("//hello.txt:::name", &dir), vec!["hello.txt"]);
    assert_eq!(values("//hello.txt:::is-leaf", &dir), vec!["true"]);
    assert_eq!(values("//hello.txt;;;size", &dir), vec!["8 B"]);
    assert_eq!(values("//hello.txt;;;extension", &dir), vec!["txt"]);
    assert_eq!(values("//hello.txt;;;is-file", &dir), vec!["true"]);
    // bare :: is the default projection — file content
    assert_eq!(values("//hello.txt::", &dir), vec!["hi there"]);
}

#[test]
fn respects_gitignore_and_hidden_by_default() {
    let dir = tree_with_ignore();
    // target/ is gitignored and dotfiles are hidden, so only the
    // real source file remains.
    assert_eq!(names("//*.rs", &dir), vec!["src/main.rs"]);
    assert_eq!(names("/*", &dir), vec!["src"]);
}

#[test]
fn flags_can_include_ignored_and_hidden() {
    let dir = tree_with_ignore();
    let all = FsOptions {
        hidden: true,
        respect_ignore: false,
    };
    assert_eq!(
        names_with("//*.rs", &dir, all),
        vec!["src/main.rs", "target/junk.rs"]
    );
}
