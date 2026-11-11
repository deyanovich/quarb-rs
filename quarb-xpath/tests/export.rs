//! Exporter assertions, plus the differential rig: the emitted
//! XPath runs through `xmllint --xpath` and the original Quarb
//! through the engine, over the same document. Skips gracefully
//! when `xmllint` is not installed.

use quarb_xpath::export;
use std::process::Command;

const DOC: &str = r#"<?xml version="1.0"?>
<library>
  <shelf label="history">
    <book id="b1" pages="512"><title>The Histories</title></book>
    <book id="b2" pages="606"><title>The Peloponnesian War</title></book>
  </shelf>
  <shelf label="scifi">
    <book id="b3" pages="412"><title>Dune</title></book>
  </shelf>
</library>"#;

fn x(quarb: &str) -> String {
    export(quarb).unwrap().query
}

#[test]
fn translations() {
    assert_eq!(x("//book/title::"), "//book/title/text()");
    assert_eq!(x("//book[::pages > 500]::id"), "//book[@pages > 500]/@id");
    assert_eq!(x("//book @| count"), "count(//book)");
    assert_eq!(x("//book::pages @| sum"), "sum(//book/@pages)");
    assert_eq!(x("//book[-1]::id"), "//book[last()]/@id");
    assert_eq!(x("//h2>>p::"), "//h2/following-sibling::p/text()");
    assert_eq!(
        x("//book[!(::pages > 400)]::id"),
        "//book[not((@pages > 400))]/@id"
    );
    assert_eq!(
        x("//title:: || //author::"),
        "//title/text() | //author/text()"
    );
    assert_eq!(
        x("//shelf/book[1..2]::id"),
        "//shelf/book[position() >= 1 and position() <= 2]/@id"
    );
    assert_eq!(x("//book$ @| count"), "count(//book[not(*)])");
}

#[test]
fn refusals() {
    let err = |q: &str| export(q).unwrap_err().to_string();
    assert!(err("//book[::t == (/x/)]").contains("regex"));
    assert!(err("//*.rs").contains("glob"));
    assert!(err("//book<block>").contains("trait"));
    assert!(err("/a <=> /b[::x = $$0::x]").contains("correlation"));
    assert!(err("//book | upper").contains("pipeline"));
    assert!(err("//book @| mean").contains("count() and sum()"));
}

#[test]
fn differential_against_xmllint() {
    if Command::new("xmllint").arg("--version").output().is_err() {
        eprintln!("skipping: xmllint !installed");
        return;
    }
    let dir = std::env::temp_dir().join("quarb-xpath-export-test");
    std::fs::create_dir_all(&dir).unwrap();
    let doc = dir.join("library.xml");
    std::fs::write(&doc, DOC).unwrap();
    let adapter = quarb_xml::XmlAdapter::parse(DOC).unwrap();

    // Numeric results compare exactly.
    let cases = [
        "//book @| count",
        "//book::pages @| sum",
        "//book[::pages > 500] @| count",
        "//shelf[::label = \"scifi\"]/book @| count",
        "//book[!(::pages > 500)] @| count",
    ];
    for quarb in cases {
        let xpath = export(quarb).unwrap().query;
        let out = Command::new("xmllint")
            .args(["--xpath", &xpath, doc.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "xmllint failed for {xpath}");
        let lint = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let got = match quarb::run(quarb, &adapter).unwrap() {
            quarb::QueryResult::Values(vs) => vs
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
            quarb::QueryResult::Nodes(_) => panic!("expected values for {quarb}"),
        };
        // xmllint prints a whole XPath number bare; Quarb keeps the
        // float's point (ruling 2026-11-06): compare without it
        let got_bare = got
            .strip_suffix(".0")
            .map(str::to_string)
            .unwrap_or(got.clone());
        assert!(
            got == lint || got_bare == lint,
            "differential mismatch for: {quarb}: {got} vs {lint}"
        );
    }
}

/// `:::texts`, the element's immediate text runs, is `text()`
/// (ruling 2026-11-11); the spread after it is the shape `text()`
/// already has. The bare projection still exports to `text()` with
/// its note: it is all the descendant text.
#[test]
fn the_text_runs_export_as_text() {
    assert_eq!(x("//book/title:::texts | ..."), "//book/title/text()");
    assert_eq!(x("//book/title:::texts"), "//book/title/text()");
    assert_eq!(
        x("//book/title:::texts | ... @| count"),
        "count(//book/title/text())"
    );
    assert_eq!(
        x("//book[/title:::texts = \"Emma\"]::id"),
        "//book[title/text() = \"Emma\"]/@id"
    );
    let t = export("//book/title:::texts | ...").unwrap();
    assert!(
        t.notes
            .iter()
            .any(|n| n.contains("':::texts' lists the element's immediate text runs"))
    );
    // the other facts have no XPath
    assert!(export("//book:::tag").is_err());
    // and a spread after anything else is still a pipeline
    assert!(export("//book::id | ...").is_err());
}
