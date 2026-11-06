//! Hash (d) of the columnar round (2026-11-06): a JSON document with
//! one array — at the root, or under a key of the root object — is
//! streamed under qua: the rest of the document parsed whole, the
//! array's elements parsed into their own arbors as a step reaches
//! them. The answers are the whole read's (`--no-stream`), compared
//! here on a generated document above the streaming threshold.
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    let test = std::thread::current()
        .name()
        .unwrap_or("t")
        .replace(':', "-");
    let dir = std::env::temp_dir().join(format!("quarb-stream-json-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let titles = ["Iliad", "Odyssey", "Aeneid", "Emma", "Persuasion", "Hamlet"];
    let authors = ["Homer", "Virgil", "Austen", "Shakespeare", "Herodotus"];
    let mut books = Vec::new();
    for i in 0..6000u32 {
        let mut b = format!(
            "{{\"id\": {i}, \"title\": \"{} {i}\", \"author\": \"{}\", \"price\": {}.{:02}, \"tags\": [\"a\", \"b\"]",
            titles[(i as usize * 7) % titles.len()],
            authors[(i as usize * 3) % authors.len()],
            5 + (i * 37) % 55,
            (i * 13) % 100
        );
        if i % 11 != 0 {
            b.push_str(&format!(", \"year\": {}", -800 + ((i as i64 * 101) % 2700)));
        }
        if i % 7 == 0 {
            b.push_str(&format!(", \"ref\": \"#/books/{}\"", i.saturating_sub(1)));
        }
        b.push('}');
        books.push(b);
    }
    let array = books.join(",\n ");
    std::fs::write(
        dir.join("keyed.json"),
        format!("{{\"meta\": {{\"n\": 6000, \"source\": \"test\"}},\n \"books\": [\n {array}\n ],\n \"note\": \"end\"}}\n"),
    )
    .unwrap();
    std::fs::write(dir.join("root.json"), format!("[{array}]")).unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_qua"))
        .current_dir(dir)
        .env("QUARB_LOCALE", "C")
        .env("HOME", dir)
        .args(args)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
        out.status.success(),
    )
}

#[test]
fn the_streamed_document_answers_as_the_whole_read() {
    let d = fixture();
    for (q, f) in [
        ("/books/* @| count", "keyed.json"),
        ("/books/*[..3] | ::title", "keyed.json"),
        ("/books/*[::year > 1800] @| count", "keyed.json"),
        ("/books/*[::year > 1800] | ::title | first", "keyed.json"),
        ("//title @| count", "keyed.json"),
        ("/meta | %", "keyed.json"),
        ("/note | ::", "keyed.json"),
        ("/books @| count", "keyed.json"),
        ("/books:::length", "keyed.json"),
        ("/books:::type", "keyed.json"),
        ("/books/*[2]/tags/* | ::", "keyed.json"),
        ("/books/*[8]/ref--> | ::title", "keyed.json"),
        ("/books/* | ::price | sum", "keyed.json"),
        ("/* @| count", "root.json"),
        ("/*[..2] | ::title", "root.json"),
        ("/*[::author = \"Homer\"] @| count", "root.json"),
        ("//tags/* @| count", "root.json"),
        ("/*[3] | %", "root.json"),
    ] {
        let (a, ea, ga) = qua(&d, &[q, f]);
        let (b, eb, gb) = qua(&d, &["--no-stream", q, f]);
        assert_eq!(ga, gb, "{q} on {f}: {ea} / {eb}");
        assert_eq!(a, b, "{q} on {f}");
    }
    let (out, err, good) = qua(&d, &["/books/* @| count", "keyed.json"]);
    assert!(good, "{err}");
    assert_eq!(out, "6000");
    let (out, _, _) = qua(&d, &["/*[::author = \"Homer\"] @| count", "root.json"]);
    assert_eq!(out, "1200");
}
