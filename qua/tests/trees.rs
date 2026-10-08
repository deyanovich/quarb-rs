//! Trees other trees refer to: `tree IDENTITY: PATH;` in a model,
//! and the identities every mount carries by itself.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-trees-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("site")).unwrap();
    std::fs::create_dir_all(dir.join("docs/guide")).unwrap();
    let w = |p: &str, text: &str| std::fs::write(dir.join(p), text).unwrap();
    w(
        "main.json",
        r##"{"home": {"$ref": "other.json#/shared/x"},
             "far":  {"$ref": "https://example.org/schema.json#/definitions/address"},
             "page": {"$ref": "https://docs.example.org/guide/intro.html"},
             "book": {"$ref": "urn:isbn:0141439513"},
             "row":  {"$ref": "data.csv!/row[2]"},
             "lost": {"$ref": "nowhere.json#/x"}}"##,
    );
    w("other.json", r#"{"shared": {"x": {"v": 42}}}"#);
    w(
        "schema.json",
        r#"{"definitions": {"address": {"type": "object"}}}"#,
    );
    w(
        "catalogue.json",
        r#"{"0141439513": {"title": "Pride and Prejudice"}}"#,
    );
    w("data.csv", "id,name\n1,ada\n2,bo\n");
    w(
        "site/a.html",
        r#"<html><body><h1 id="top">A</h1><a href="b.html#sec">to b</a></body></html>"#,
    );
    w(
        "site/b.html",
        r#"<html><body><h2 id="sec">B section</h2><a href="a.html">back</a></body></html>"#,
    );
    w(
        "docs/guide/intro.html",
        "<html><body><h1>Intro</h1></body></html>",
    );
    w(
        "trees.model",
        "mount main: main.json;\n\
         mount other: other.json;\n\
         mount schema: schema.json;\n\
         mount catalogue: catalogue.json;\n\
         mount data: data.csv;\n\
         mount docs: docs/;\n\
         tree https://example.org/schema.json: /schema;\n\
         tree https://docs.example.org/: /docs;\n\
         tree urn:isbn:: /catalogue;\n",
    );
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

#[test]
fn a_mounts_own_target_is_its_identity() {
    let dir = fixture("auto");
    // two files: `$ref: "other.json#/shared/x"` resolves beside main
    let (out, _) = qua(
        &dir,
        &["/main/home-->::v", "main=main.json", "other=other.json"],
    );
    assert_eq!(out, "42");
    // a directory is a base over its files: a relative href with a
    // fragment lands on the element inside the sibling page
    let (out, _) = qua(&dir, &["--graft", "//a-->::", "site"]);
    assert!(out.starts_with("B section"), "{out}");
    let (out, _) = qua(&dir, &["--graft", "/site//a-->::", "site=site"]);
    assert!(out.starts_with("B section"), "{out}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn declared_trees_exact_base_urn_and_locator() {
    let dir = fixture("model");
    let run = |q: &str| qua(&dir, &["--graft", "--model", "trees.model", q]).0;
    // an exact identity for a local file
    assert_eq!(run("/main/far-->::type"), "object");
    // a base over a directory of pages: the remainder is the path
    assert_eq!(run("/main/page-->//h1::"), "Intro");
    // a base whose separator is not `/`
    assert_eq!(run("/main/book-->::title"), "Pride and Prejudice");
    // the engine's own locator boundary, a Quarb path inside the tree
    assert_eq!(run("/main/row-->::name"), "bo");
    // an identity nothing answers is reported under --explain
    let (out, err) = qua(
        &dir,
        &["--explain", "--model", "trees.model", "/main/lost-->"],
    );
    assert_eq!(out, "");
    assert!(
        err.contains("unresolved external reference") && err.contains("nowhere.json"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_tree_is_refused_where_its_path_names_no_node_or_is_declared_twice() {
    let dir = fixture("refuse");
    std::fs::write(
        dir.join("bad.model"),
        "mount main: main.json;\ntree https://example.org/x: /nowhere;\n",
    )
    .unwrap();
    let (_, err) = qua(&dir, &["--model", "bad.model", "/main"]);
    assert!(err.contains("names no node"), "{err}");
    std::fs::write(
        dir.join("twice.model"),
        "mount main: main.json;\ntree https://example.org/x: /main;\ntree https://example.org/x: /main;\n",
    )
    .unwrap();
    let (_, err) = qua(&dir, &["--model", "twice.model", "/main"]);
    assert!(err.contains("declared twice"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}
