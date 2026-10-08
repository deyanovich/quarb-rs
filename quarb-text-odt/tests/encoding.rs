//! Finding 22: a flat document (and every package part) decodes
//! under the mount's rule — `?encoding=`, its byte-order mark, its
//! XML declaration, else strict UTF-8 — never with replacement
//! characters.

use quarb::QueryResult;
use quarb::source::SourceDecoding;
use quarb_text_odt::{parse, parse_with_decoding};

fn refusal(r: Result<quarb_text::TextModel, impl std::fmt::Display>) -> String {
    match r {
        Err(e) => e.to_string(),
        Ok(_) => panic!("decoded where a refusal was due"),
    }
}

fn values(model: &quarb_text::TextModel, q: &str) -> Vec<String> {
    match quarb::run(q, model).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| model.locator(*n)).collect(),
    }
}

const NS: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0""#;

/// A flat document holding «Привет» in windows-1251, with or
/// without the declaration.
fn flat(declared: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if declared {
        out.extend_from_slice(b"<?xml version=\"1.0\" encoding=\"windows-1251\"?>");
    }
    out.extend_from_slice(
        format!("<office:document {NS} office:mimetype=\"application/vnd.oasis.opendocument.text\"><office:body><office:text><text:p>").as_bytes(),
    );
    out.extend_from_slice(b"\xcf\xf0\xe8\xe2\xe5\xf2");
    out.extend_from_slice(b"</text:p></office:text></office:body></office:document>");
    out
}

#[test]
fn a_flat_document_decodes_by_declaration_option_or_strict_utf8() {
    let m = parse(&flat(true)).unwrap();
    assert_eq!(values(&m, "//paragraph::"), ["Привет"]);

    let err = refusal(parse(&flat(false)));
    assert!(err.contains("?encoding="), "{err}");

    let d = SourceDecoding::default()
        .with_option(Some("windows-1251"))
        .unwrap();
    let m = parse_with_decoding(&flat(false), &d).unwrap();
    assert_eq!(values(&m, "//paragraph::"), ["Привет"]);
}
