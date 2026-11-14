//! Exporter assertions, plus the differential rig: the emitted jq
//! runs through the real `jq` binary and the original Quarb through
//! the engine, over the same document — outputs must agree. Skips
//! gracefully when `jq` is not installed.

use quarb_jq::export;
use std::process::Command;

const DOC: &str = r#"{
  "books": [
    {"title": "The Histories", "author": "Herodotus", "price": 22.5, "genre": "history"},
    {"title": "Walden", "author": "Thoreau", "price": 9.99, "genre": "essay"},
    {"title": "Emma", "author": "Austen", "price": 7.5, "genre": "classic"},
    {"title": "The Peloponnesian War", "author": "Thucydides", "price": 18.0, "genre": "history"}
  ],
  "clerk": {"name": "Ines"}
}"#;

fn x(quarb: &str) -> String {
    export(quarb).unwrap().query
}

#[test]
fn translations() {
    assert_eq!(x("/books/*/title::"), ".books[].title");
    assert_eq!(
        x("/books/*[/price:: < 12]/title::"),
        ".books[] | select(.price < 12).title"
    );
    assert_eq!(x("/books/*/price:: @| sum"), "[.books[].price] | add");
    assert_eq!(x("/books/*[2..3]/title::"), ".books[1:3][].title");
    assert_eq!(
        x(r#"/books/* | %(t = /title::; p = /price::)"#),
        ".books[] | {t: .title, p: .price}"
    );
    assert_eq!(
        x("/books/*[/title:: == (/^S/)]/title::"),
        ".books[] | select((.title | test(\"^S\"))).title"
    );
    // a path that opens on an array: jq's `.[]`, where a bare `[]`
    // (which was written) is jq's empty array
    assert_eq!(x("/*/title::"), ".[].title");
    assert_eq!(x("/*"), ".[]");
    assert_eq!(x("/*[2]/title::"), ".[1].title");
}

/// The items of a list are named from one (ruling 2026-11-14): a
/// numeric step is the item, jq's zero-based index, and the note
/// says how the same step reads under an object. `0` and `01` name
/// no item and stay keys.
#[test]
fn a_numeric_step_is_a_list_item() {
    assert_eq!(x("/books/1/title::"), ".books[0].title");
    assert_eq!(x("/books/4"), ".books[3]");
    assert_eq!(x("/2/title::"), ".[1].title");
    assert_eq!(x("/books/0/title::"), ".books.\"0\".title");
    assert_eq!(x("/books/01"), ".books.\"01\"");
    let t = export("/books/1/title::").unwrap();
    assert_eq!(t.notes.len(), 1, "{:?}", t.notes);
    assert!(t.notes[0].contains("numeric step: '/1'"), "{:?}", t.notes);
    assert!(t.notes[0].contains(".\"1\""), "{:?}", t.notes);
    assert!(export("/books/*/title::").unwrap().notes.is_empty());
}

#[test]
fn refusals() {
    let err = |q: &str| export(q).unwrap_err().to_string();
    assert!(err("//book/title::").contains("'//' axis"));
    assert!(err("/books/*<block>/title::").contains("trait"));
    assert!(err("/a <=> /b[::x = $$0::x]").contains("correlation"));
    assert!(err("/books/* | .t(/title::) | $.t").contains("command"));
    assert!(err("/books/*.rs/x::").contains("glob"));
}

#[test]
fn differential_against_jq() {
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!("skipping: jq binary !installed");
        return;
    }
    let dir = std::env::temp_dir().join("quarb-jq-export-test");
    std::fs::create_dir_all(&dir).unwrap();
    let doc = dir.join("store.json");
    std::fs::write(&doc, DOC).unwrap();

    let adapter = quarb_json::JsonAdapter::parse(DOC).unwrap();
    let cases = [
        "/books/*/title::",
        "/books/*[/price:: < 12]/title::",
        "/books/*/price:: @| sum",
        "/books/*[/genre:: = \"history\"]/title::",
        "/books/*[2..3]/title::",
        "/books/*/author:: @| join(\", \")",
        "/books/*[/title:: == (/^S/)]/title::",
        "/clerk/name::",
        "/books/1/title::",
        "/books/4/author::",
        "/books/*[2]/title::",
    ];
    for quarb in cases {
        let filter = export(quarb).unwrap().query;
        let out = Command::new("jq")
            .args(["-r", &filter, doc.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "jq failed for {filter}");
        let jq_lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        let quarb_lines: Vec<String> = match quarb::run(quarb, &adapter).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
            quarb::QueryResult::Nodes(_) => panic!("expected values for {quarb}"),
        };
        assert_eq!(quarb_lines, jq_lines, "differential mismatch for: {quarb}");
    }
}
