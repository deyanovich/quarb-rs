//! Finding 22: a package member decodes under the mount's rule —
//! `?encoding=`, its byte-order mark, its XML declaration, else
//! strict UTF-8 — and a member that decodes under none is an error
//! naming it, never a part silently missing.

use std::io::Write;

use quarb::QueryResult;
use quarb::source::SourceDecoding;
use quarb_text_docx::{parse, parse_with_decoding};

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

fn docx(document: &[u8]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut cursor);
        let opts = zip::write::SimpleFileOptions::default();
        z.start_file("word/document.xml", opts).unwrap();
        z.write_all(document).unwrap();
        z.finish().unwrap();
    }
    cursor.into_inner()
}

/// A body holding «Привет» in windows-1251.
const BODY_CP1251: &[u8] = b"<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:r><w:t>\xcf\xf0\xe8\xe2\xe5\xf2</w:t></w:r></w:p></w:body></w:document>";

#[test]
fn members_decode_by_declaration_option_or_strict_utf8() {
    let mut declared = b"<?xml version=\"1.0\" encoding=\"windows-1251\"?>".to_vec();
    declared.extend_from_slice(BODY_CP1251);
    let m = parse(&docx(&declared)).unwrap();
    assert_eq!(values(&m, "//paragraph::"), ["Привет"]);

    let err = refusal(parse(&docx(BODY_CP1251)));
    assert!(
        err.contains("word/document.xml") && err.contains("?encoding="),
        "{err}"
    );

    let d = SourceDecoding::default()
        .with_option(Some("windows-1251"))
        .unwrap();
    let m = parse_with_decoding(&docx(BODY_CP1251), &d).unwrap();
    assert_eq!(values(&m, "//paragraph::"), ["Привет"]);
}
