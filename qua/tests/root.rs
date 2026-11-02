//! A root on a mount (spec: Mounts, the root option): `?root=PATH`
//! on any source, `--root PATH` on one, mounts the subtree at PATH as
//! the tree. Paths print relative to it, `//` sweeps it, and what
//! lies above or beside it is not mounted at all.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-root-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("j.json"), r#"{"a":{"b":{"c":1},"d":2},"e":3}"#).unwrap();
    let db = rusqlite::Connection::open(dir.join("roads.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE stations (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
         INSERT INTO stations VALUES (1, 'Ephesus'), (2, 'Sardis');
         CREATE TABLE stages (id INTEGER PRIMARY KEY, from_id INTEGER REFERENCES stations(id),
           to_id INTEGER REFERENCES stations(id));
         INSERT INTO stages VALUES (1, 1, 2);",
    )
    .unwrap();
    std::fs::write(
        dir.join("rooted.model"),
        "mount j: j.json?root=/a;\nmount whole: j.json;\n",
    )
    .unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).trim_end().to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.success(),
    )
}

fn lines(dir: &Path, args: &[&str]) -> Vec<String> {
    let (out, err, ok) = qua(dir, args);
    assert!(ok, "{args:?}: {err}");
    out.lines().map(str::to_string).collect()
}

#[test]
fn the_subtree_is_the_tree() {
    let dir = fixture("tree");
    // paths print relative to the root; `//` sweeps the subtree only
    assert_eq!(
        lines(&dir, &["//*", "j.json?root=/a"]),
        ["/b", "/b/c", "/d"]
    );
    assert_eq!(lines(&dir, &["/b/c", "j.json?root=/a"]), ["/b/c"]);
    // the root's parent is none: a step up from a child stops there
    assert_eq!(
        lines(&dir, &["/b | \\ | \\", "j.json?root=/a"]),
        Vec::<String>::new()
    );
    // `--root` is the sugar on one source
    assert_eq!(lines(&dir, &["--root", "/a", "//c", "j.json"]), ["/b/c"]);
    assert_eq!(lines(&dir, &["--root", "/a/b", "//*", "j.json"]), ["/c"]);
    // a mount among several carries its own root
    assert_eq!(
        lines(&dir, &["/x//*", "x=j.json?root=/a", "y=j.json"]),
        ["/x/b", "/x/b/c", "/x/d"]
    );
    assert_eq!(
        lines(&dir, &["/y/e", "x=j.json?root=/a", "y=j.json"]),
        ["/y/e"]
    );
    // the model's mount form
    assert_eq!(
        lines(&dir, &["--model", "rooted.model", "/j//*", "-"]),
        ["/j/b", "/j/b/c", "/j/d"]
    );
    assert_eq!(
        lines(&dir, &["--model", "rooted.model", "/whole/a/d", "-"]),
        ["/whole/a/d"]
    );
}

#[test]
fn a_path_no_node_matches_refuses_at_mount() {
    let dir = fixture("refuse");
    let (_, err, ok) = qua(&dir, &["//*", "j.json?root=/zz"]);
    assert!(!ok);
    assert!(err.contains("root: no node at \"/zz\""), "{err}");
    let (_, err, ok) = qua(&dir, &["--root", "/a", "//*", "x=j.json", "y=j.json"]);
    assert!(!ok);
    assert!(err.contains("one mounted source"), "{err}");
}

#[test]
fn what_lies_beside_the_root_is_not_mounted() {
    let dir = fixture("beside");
    // whole: a stage reaches its stations through the foreign keys
    assert_eq!(
        lines(&dir, &["/stages/*::from_id-->::name", "roads.db"]),
        ["Ephesus"]
    );
    // rooted at the stages: the stations are not there, and the hop
    // that would leave the root answers nothing
    assert_eq!(
        lines(&dir, &["//*", "roads.db?root=/stages"]),
        ["/1", "/1/id", "/1/from_id", "/1/to_id"]
    );
    assert_eq!(
        lines(
            &dir,
            &["/*::from_id-->::name @| count", "roads.db?root=/stages"]
        ),
        ["0"]
    );
    // the key itself stays a property
    assert_eq!(
        lines(&dir, &["/1::from_id", "roads.db?root=/stages"]),
        ["1"]
    );
}
