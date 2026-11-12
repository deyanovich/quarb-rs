//! Hash (d) of the columnar round (2026-11-06): an XML document with
//! one repeated child under its document element — a dictionary of
//! entries, a dump of pages — is streamed under qua where it is large
//! (128 MiB and up) or where `--stream` asks: the element and the
//! prologue parsed whole, each child parsed into its own arbor as a
//! step reaches it. The answers are the whole read's (`--no-stream`,
//! and the default for a file of this size), compared here on a
//! generated document under `--stream`.
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    let test = std::thread::current()
        .name()
        .unwrap_or("t")
        .replace(':', "-");
    let dir = std::env::temp_dir().join(format!("quarb-stream-xml-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let forms = ["λόγος", "μῆνις", "ἀοιδός", "θεός", "ἄνθρωπος", "πόλις"];
    let senses = ["word", "wrath &amp; rage", "singer", "god", "man", "city"];
    let mut out = vec![
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>".to_string(),
        "<dictionary xmlns:x=\"urn:x\" lang=\"grc\">".to_string(),
    ];
    for i in 0..5000u32 {
        let f = (i as usize * 7) % forms.len();
        let mut entry = format!(
            "  <entry id=\"e{i}\" x:n=\"{i}\"><form>{} {i}</form><sense n=\"{}\">{}</sense>",
            forms[f],
            i % 3,
            senses[f]
        );
        if i % 5 == 0 {
            entry.push_str(&format!("<see ref=\"e{}\"/>", i.saturating_sub(1)));
        }
        if i % 7 == 0 {
            entry.push_str("<note>a <b>bold</b> note</note>");
        }
        entry.push_str("</entry>");
        out.push(entry);
        if i % 1000 == 999 {
            out.push(format!("  <note id=\"n{i}\">milestone {i}</note>"));
        }
    }
    out.push("</dictionary>".to_string());
    std::fs::write(dir.join("dict.xml"), out.join("\n") + "\n").unwrap();
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
    for q in [
        "/dictionary/entry @| count",
        "/dictionary/* @| count",
        "/dictionary/note @| count",
        "/dictionary/entry[..3]/form | ::",
        "/dictionary/entry[::id = \"e7\"]/sense | ::",
        "//sense[::n = \"2\"] @| count",
        "//b @| count",
        "/dictionary:::n-attrs",
        "/dictionary | ::lang",
        "/dictionary/entry[3] | ::",
        "/dictionary/entry[3]:::local-name",
        "/dictionary/entry[8]/note/b | ::",
        "//entry[::id = \"e4999\"] | %",
        "/dictionary/entry[6]/see | ::ref",
        "/dictionary/note[2] | ::",
        // children read out of the file's order
        "/dictionary/entry @| reverse @| [..3] | ::id",
        "/dictionary/entry @| sort_by(::id) @| [..3] | ::id",
        "/dictionary/entry | .n(/sense::n) @|{.n} max_by(::id) | ::id",
    ] {
        let (a, ea, ga) = qua(&d, &["--stream", q, "dict.xml"]);
        let (b, eb, gb) = qua(&d, &["--no-stream", q, "dict.xml"]);
        assert_eq!(ga, gb, "{q}: {ea} / {eb}");
        assert_eq!(a, b, "{q}");
    }
    let (out, err, good) = qua(&d, &["--stream", "/dictionary/entry @| count", "dict.xml"]);
    assert!(good, "{err}");
    assert_eq!(out, "5000");
    assert_eq!(
        qua(&d, &["--stream", "/dictionary/note @| count", "dict.xml"]).0,
        "5"
    );
}
