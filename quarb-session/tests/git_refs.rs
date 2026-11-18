//! A session's document over a git repository answers it as it
//! stands: `Doc::run` looks at the refs before each query, as qua's
//! standing session does (the ruling of 2026-11-17). A notebook, a
//! REPL and a kernel keep one document open across their lines, and
//! a commit made beside them was answered by the listing read first.

use std::path::Path;

use quarb_session::doc::{Doc, Options};

#[test]
fn a_document_over_a_repository_answers_it_as_it_stands() {
    let dir = std::env::temp_dir().join(format!("quarb-session-git-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
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
    };
    git(&["init", "-q", "-b", "master"]);
    git(&["commit", "-q", "--allow-empty", "-m", "canto 1"]);

    let spec = format!("git:{}", repo.display());
    let doc = Doc::open(Path::new(&spec), &Options::default()).unwrap();
    let run = quarb::Run::ambient();
    let answer = |q: &str| -> Vec<String> {
        match doc.run(q, &run).unwrap().result {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
            quarb::QueryResult::Nodes(ns) => vec![ns.len().to_string()],
        }
    };
    assert_eq!(answer("/HEAD | ::subject"), ["canto 1"]);
    assert_eq!(answer("/commits/* @| count"), ["1"]);
    assert_eq!(answer("/tags/* @| count"), ["0"]);

    // the repository moves beside the open document
    git(&["commit", "-q", "--allow-empty", "-m", "canto 2"]);
    git(&["tag", "c02"]);
    assert_eq!(answer("/HEAD | ::subject"), ["canto 2"]);
    assert_eq!(answer("/commits/* @| count"), ["2"]);
    assert_eq!(answer("/tags/* | :name"), ["c02"]);
    // …and the closing-command door looks too
    git(&["commit", "-q", "--allow-empty", "-m", "canto 3"]);
    let out = doc.run_rendering("/HEAD | ::subject", &run).unwrap();
    match out.result {
        quarb::QueryResult::Values(vs) => assert_eq!(vs[0].to_string(), "canto 3"),
        other => panic!("{other:?}"),
    }
    let _ = std::fs::remove_dir_all(dir);
}
