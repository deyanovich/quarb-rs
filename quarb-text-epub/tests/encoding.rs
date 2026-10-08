//! Finding 22: a spine member decodes under the mount's rule —
//! `?encoding=`, its byte-order mark, its XML declaration, else
//! strict UTF-8 — and one that decodes under none is an error
//! naming it, never a chapter silently skipped.

use std::io::Write;

use quarb::QueryResult;
use quarb::source::SourceDecoding;
use quarb_text_epub::{parse, parse_with_decoding};

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

const CONTAINER: &str = r#"<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#;

const OPF: &str = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <manifest><item id="c1" href="c1.xhtml" media-type="application/xhtml+xml"/></manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#;

/// A chapter holding «Привет» in windows-1251.
const CH_CP1251: &[u8] = b"<html><body><p>\xcf\xf0\xe8\xe2\xe5\xf2</p></body></html>";

fn epub(chapter: &[u8]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut cursor);
        let opts = zip::write::SimpleFileOptions::default();
        z.start_file("META-INF/container.xml", opts).unwrap();
        z.write_all(CONTAINER.as_bytes()).unwrap();
        z.start_file("OEBPS/content.opf", opts).unwrap();
        z.write_all(OPF.as_bytes()).unwrap();
        z.start_file("OEBPS/c1.xhtml", opts).unwrap();
        z.write_all(chapter).unwrap();
        z.finish().unwrap();
    }
    cursor.into_inner()
}

#[test]
fn chapters_decode_by_declaration_option_or_strict_utf8() {
    let mut declared = b"<?xml version=\"1.0\" encoding=\"windows-1251\"?>".to_vec();
    declared.extend_from_slice(CH_CP1251);
    let m = parse(&epub(&declared)).unwrap();
    assert_eq!(values(&m, "//paragraph::"), ["Привет"]);

    let err = refusal(parse(&epub(CH_CP1251)));
    assert!(
        err.contains("OEBPS/c1.xhtml") && err.contains("?encoding="),
        "{err}"
    );

    let d = SourceDecoding::default()
        .with_option(Some("windows-1251"))
        .unwrap();
    let m = parse_with_decoding(&epub(CH_CP1251), &d).unwrap();
    assert_eq!(values(&m, "//paragraph::"), ["Привет"]);
}
