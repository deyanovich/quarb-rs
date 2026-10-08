//! `koine:report.docx`: Word through atrep's importer (declared
//! structure only), the second route beside the native
//! `text:report.docx` — the two agree where litogramma's model and
//! the native reading coincide.

use std::path::Path;

use quarb::QueryResult;

fn values(model: &quarb_text::TextModel, q: &str) -> Vec<String> {
    match quarb::run(q, model).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| model.locator(*n)).collect(),
    }
}

#[test]
fn the_koine_route_agrees_with_the_native_reading() {
    let bytes =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/iliad.docx"))
            .unwrap();
    let koine = quarb_text_koine::parse_docx(&bytes).unwrap();
    let native = quarb_text_docx::parse(&bytes).unwrap();
    for q in [
        "//section::lemma",
        "//paragraph @| count",
        "//footnote @| count",
        "//section[::lemma = \"The Quarrel\"]/paragraph[1]::",
    ] {
        assert_eq!(values(&koine, q), values(&native, q), "{q}");
    }
    assert_eq!(
        values(&koine, "//section::lemma"),
        [
            "Book the First",
            "Argument",
            "The Quarrel",
            "Book the Second",
            "Argument"
        ]
    );
}
