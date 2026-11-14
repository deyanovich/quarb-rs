//! The name of a list item is its position, counted from one, in
//! every source that names items by position (ruling 2026-11-14):
//! `/shelves/1` is the first shelf and `:name` agrees with `:index`.
//! Until 0.63 the name counted from zero while the index and every
//! position counted from one. A reference the document itself
//! writes (a JSON Pointer) counts as its standard does, from zero,
//! and lands on the node named one higher.

fn run(args: &[&str]) -> (bool, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout)
            .unwrap()
            .trim_end()
            .to_string(),
        String::from_utf8(out.stderr)
            .unwrap()
            .trim_end()
            .to_string(),
    )
}

fn ok(args: &[&str]) -> String {
    let (success, out, err) = run(args);
    assert!(success, "qua {args:?}: {err}");
    out
}

fn fixture(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("qua-list-names-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (file, text) in files {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

const SHELF: &str = r##"{"shelves": [{"label": "epic", "books": ["Iliad", "Odyssey"]},
              {"label": "history", "books": ["Histories"]}],
 "keeper": {"$ref": "#/shelves/1"}}"##;

#[test]
fn a_json_array_names_its_items_from_one() {
    let d = fixture("json", &[("shelf.json", SHELF)]);
    let f = d.join("shelf.json");
    let f = f.to_str().unwrap();
    // the name, the index and the path agree
    assert_eq!(
        ok(&["/shelves/* | %(:name; :index; :path; ::label)", f]),
        "%(name = \"1\"; index = 1; path = \"/shelves/1\"; label = \"epic\")\n\
         %(name = \"2\"; index = 2; path = \"/shelves/2\"; label = \"history\")"
    );
    // a step by name, the same node as the position
    assert_eq!(ok(&["/shelves/1::label", f]), "epic");
    assert_eq!(ok(&["/shelves/*[1]::label", f]), "epic");
    assert_eq!(ok(&["/shelves/2/books/1::", f]), "Histories");
    // zero names nothing, and neither does a padded number
    assert_eq!(ok(&["/shelves/0 @| count", f]), "0");
    assert_eq!(ok(&["/shelves/01 @| count", f]), "0");
    assert_eq!(ok(&["/shelves/3 @| count", f]), "0");
    // what Quarb prints of a node pastes back into a query
    assert_eq!(ok(&["/shelves/*", f]), "/shelves/1\n/shelves/2");
    assert_eq!(ok(&["/shelves/* | $$_ @| last", f]), "/shelves/2");
    // a list is still written as a list
    assert_eq!(
        ok(&["/shelves/1/books | json", f]),
        r#"["Iliad", "Odyssey"]"#
    );
    assert_eq!(
        ok(&["/shelves/2 | json", f]),
        r#"{"label": "history", "books": ["Histories"]}"#
    );
}

#[test]
fn a_pointer_in_the_data_counts_from_zero() {
    let d = fixture("pointer", &[("shelf.json", SHELF)]);
    let f = d.join("shelf.json");
    let f = f.to_str().unwrap();
    // `#/shelves/1` is RFC 6901: the SECOND shelf, named `/shelves/2`
    assert_eq!(
        ok(&["/keeper::'$ref'--> | %(:path; ::label)", f]),
        "%(path = \"/shelves/2\"; label = \"history\")"
    );
}

#[test]
fn the_other_notations_name_their_items_alike() {
    let d = fixture(
        "notations",
        &[
            ("lines.jsonl", "{\"t\": \"Iliad\"}\n{\"t\": \"Odyssey\"}\n"),
            (
                "one.yaml",
                "shelves:\n  - label: epic\n  - label: history\n",
            ),
            ("docs.yaml", "kind: A\n---\nkind: B\n"),
            (
                "s.toml",
                "[[shelves]]\nlabel = \"epic\"\n[[shelves]]\nlabel = \"history\"\n",
            ),
        ],
    );
    let at = |f: &str| d.join(f).to_str().unwrap().to_string();
    // a JSON lines file: the lines
    assert_eq!(
        ok(&["/* | %(:name; ::t)", &at("lines.jsonl")]),
        "%(name = \"1\"; t = \"Iliad\")\n%(name = \"2\"; t = \"Odyssey\")"
    );
    assert_eq!(ok(&["/2::t", &at("lines.jsonl")]), "Odyssey");
    assert_eq!(ok(&["/0 @| count", &at("lines.jsonl")]), "0");
    // a YAML sequence, and the documents of a file
    assert_eq!(ok(&["/shelves/1::label", &at("one.yaml")]), "epic");
    assert_eq!(
        ok(&["/* | %(:name; ::kind)", &at("docs.yaml")]),
        "%(name = \"1\"; kind = \"A\")\n%(name = \"2\"; kind = \"B\")"
    );
    assert_eq!(ok(&["/1::kind", &at("docs.yaml")]), "A");
    // a TOML array of tables
    assert_eq!(
        ok(&["/shelves/* | %(:name; ::label)", &at("s.toml")]),
        "%(name = \"1\"; label = \"epic\")\n%(name = \"2\"; label = \"history\")"
    );
}

/// A mapping keyed 1, 2 (a map by id, a common shape) has the names
/// a two-item list has. The node says which it is: a record keeps
/// its keys on the way out, a list is written as a list. (The
/// serializers went by the names alone, so with items named from
/// one such a map was written as an array and lost its keys; until
/// 0.63 the same happened to a map keyed from zero.)
#[test]
fn a_record_keyed_like_a_list_keeps_its_keys() {
    let d = fixture(
        "keys",
        &[
            (
                "keys.json",
                r#"{"a": {"1": "x", "2": "y"}, "b": {"0": "x", "1": "y"}, "c": ["p", "q"], "d": {"1": {"n": 1}, "2": {"n": 2}}}"#,
            ),
            (
                "keys.yaml",
                "byid:\n  1: Homer\n  2: Hesiod\nlist:\n  - Homer\n  - Hesiod\n",
            ),
        ],
    );
    let f = d.join("keys.json");
    let f = f.to_str().unwrap();
    assert_eq!(ok(&["/a | json", f]), r#"{"1": "x", "2": "y"}"#);
    assert_eq!(ok(&["/b | json", f]), r#"{"0": "x", "1": "y"}"#);
    assert_eq!(ok(&["/c | json", f]), r#"["p", "q"]"#);
    assert_eq!(ok(&["/d | json", f]), r#"{"1": {"n": 1}, "2": {"n": 2}}"#);
    // the whole document comes back as it was written
    assert_eq!(
        ok(&["^ | json", f]),
        r#"{"a": {"1": "x", "2": "y"}, "b": {"0": "x", "1": "y"}, "c": ["p", "q"], "d": {"1": {"n": 1}, "2": {"n": 2}}}"#
    );
    // a YAML mapping with integer keys, beside a sequence
    let y = d.join("keys.yaml");
    let y = y.to_str().unwrap();
    assert_eq!(ok(&["/byid | json", y]), r#"{"1": "Homer", "2": "Hesiod"}"#);
    assert_eq!(ok(&["/list | json", y]), r#"["Homer", "Hesiod"]"#);
}

/// A mount no name can be had for (a target with no stem, `..`) is
/// named by its place among the inputs, counted from one: `doc1`,
/// `doc2` (ruling 2026-11-14; it was `doc0`, `doc1`).
#[test]
fn a_nameless_mount_is_named_from_one() {
    let d = fixture("nameless", &[("a/b/note.txt", "x")]);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(d.join("a/b"))
        .args(["/* | :name", "..", "../.."])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim_end(),
        "doc1\ndoc2"
    );
}
