//! The Tom Sawyer fixture (pandoc's .odt and its flat .fodt from
//! one Markdown source): headings by outline level, the quote
//! style, bullet and nested numbered lists, the declared header
//! row, inline footnotes with their callouts, bookmarks, links,
//! emphasis and code spans, and `meta.xml` — asserted at the
//! block level and through real queries. Hand-built packages
//! cover what pandoc does not write: the parent-chain outline
//! level, tracked changes, index marks, reference fields and an
//! endnote.

use std::io::Write;

use quarb::{AstAdapter, QueryResult};
use quarb_text::Block;
use quarb_text_odt::{blocks, declares_text, parse};

const ODT: &[u8] = include_bytes!("fixtures/tom-sawyer.odt");
const FODT: &[u8] = include_bytes!("fixtures/tom-sawyer.fodt");

fn values(model: &quarb_text::TextModel, q: &str) -> Vec<String> {
    match quarb::run(q, model).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns
            .iter()
            .map(|n| model.name(*n).unwrap_or_default())
            .collect(),
    }
}

fn package(content: &str, styles: &str, meta: Option<&str>) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut cursor);
        let opts = zip::write::SimpleFileOptions::default();
        z.start_file("mimetype", opts).unwrap();
        z.write_all(b"application/vnd.oasis.opendocument.text")
            .unwrap();
        z.start_file("content.xml", opts).unwrap();
        z.write_all(content.as_bytes()).unwrap();
        z.start_file("styles.xml", opts).unwrap();
        z.write_all(styles.as_bytes()).unwrap();
        if let Some(m) = meta {
            z.start_file("meta.xml", opts).unwrap();
            z.write_all(m.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }
    cursor.into_inner()
}

const NS: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0""#;

fn content(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content {NS}><office:body><office:text>{body}</office:text></office:body></office:document-content>"#
    )
}

fn styles(inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-styles {NS}><office:styles>{inner}</office:styles></office:document-styles>"#
    )
}

#[test]
fn the_fixture_reads_as_its_markdown_source_does() {
    for bytes in [ODT, FODT] {
        let m = parse(bytes).unwrap();
        assert_eq!(
            values(&m, "//section::lemma"),
            ["Chapter I", "The old lady"]
        );
        assert_eq!(
            values(&m, "//section[::lemma = \"The old lady\"]:depth"),
            ["2"]
        );
        // The title and author paragraphs stand before the first
        // heading; the chapter's own paragraphs are in its section.
        assert_eq!(
            values(&m, "/section[1]/paragraph[1]::"),
            ["“Tom!” No answer. “Tom!” No answer."]
        );
        assert_eq!(
            values(&m, "//blockquote::"),
            ["Well, I lay if I get hold of you I’ll —"]
        );
        assert_eq!(
            values(&m, "//unordered-list[1]/unordered-item @| count"),
            ["2"]
        );
        assert_eq!(
            values(&m, "//unordered-list[1]/unordered-item[1]::"),
            ["a bullet"]
        );
        // The nested list's kind is the declared list style's:
        // pandoc's flat file restates no styles, so there it is
        // unordered by the rule.
        let nested = if bytes.starts_with(b"PK") {
            "ordered-list/ordered-item"
        } else {
            "unordered-list/unordered-item"
        };
        assert_eq!(
            values(
                &m,
                &format!("//unordered-list[1]/unordered-item[2]/{nested}::")
            ),
            ["nested one", "nested two"]
        );
        assert_eq!(
            values(&m, r#"//*<cell>[::lemma = "Age"]::"#),
            ["Age: 12", "Age: 10"]
        );
        // Notes: the callout at its offset in the paragraph, the
        // body at the document end, resolved both ways.
        assert_eq!(values(&m, "//footnote<deixis> @| count"), ["2"]);
        assert_eq!(
            values(&m, "//footnote<note>::"),
            ["The first call.", "The second call, with emphasis."]
        );
        assert_eq!(
            values(&m, "//footnote<deixis>[1]->footnote::"),
            ["The first call."]
        );
        // A byte offset: the curly quotes are three bytes each.
        assert_eq!(values(&m, "//footnote<deixis>[1]:::at"), ["10"]);
        // The link and the heading's bookmark.
        assert_eq!(
            values(&m, "//ref::target"),
            ["https://www.gutenberg.org/ebooks/74"]
        );
        assert_eq!(values(&m, "//ref::"), ["Gutenberg text"]);
        assert_eq!(values(&m, "//section[1]::onym"), ["chapter-i"]);
        // Emphasis and code from the character styles' declared
        // properties — the package declares them, the flat file
        // (pandoc's) names them only.
        if bytes.starts_with(b"PK") {
            assert_eq!(values(&m, "//emphasis::"), ["through", "emphasis"]);
            assert_eq!(values(&m, "//code::"), ["code"]);
        } else {
            assert_eq!(values(&m, "//emphasis @| count"), ["0"]);
        }
        // meta.xml under the head's keys (the flat file carries no
        // meta).
        if bytes.starts_with(b"PK") {
            assert_eq!(values(&m, "^::title"), ["The Adventures of Tom Sawyer"]);
            assert_eq!(values(&m, "^::author"), ["Mark Twain"]);
        }
    }
}

#[test]
fn the_package_declares_itself() {
    assert!(declares_text(ODT));
    assert!(!declares_text(FODT));
    assert!(!declares_text(b"not a zip"));
    let ods = {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut cursor);
            let opts = zip::write::SimpleFileOptions::default();
            z.start_file("mimetype", opts).unwrap();
            z.write_all(b"application/vnd.oasis.opendocument.spreadsheet")
                .unwrap();
            z.finish().unwrap();
        }
        cursor.into_inner()
    };
    assert!(!declares_text(&ods));
    let e = match parse(&ods) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a spreadsheet read as text"),
    };
    assert!(e.contains("not an OpenDocument text"), "{e}");
}

#[test]
fn outline_levels_come_from_the_style_chain() {
    let st = styles(
        r#"<style:style style:name="Heading_20_1" style:family="paragraph" style:default-outline-level="1"/>
           <style:style style:name="MyTitle" style:family="paragraph" style:parent-style-name="Heading_20_1"/>
           <style:style style:name="Quotations" style:family="paragraph"/>
           <style:style style:name="FancyQuote" style:family="paragraph" style:parent-style-name="Quotations"/>"#,
    );
    let body = r#"<text:p text:style-name="MyTitle">Styled heading</text:p>
        <text:h text:outline-level="3">Direct heading</text:h>
        <text:p text:style-name="FancyQuote">Quoted one</text:p>
        <text:p text:style-name="Quotations">Quoted two</text:p>
        <text:p>Plain</text:p>"#;
    let b = blocks(&package(&content(body), &st, None)).unwrap();
    assert!(matches!(&b[0], Block::Heading { level: 1, lemma } if lemma == "Styled heading"));
    assert!(matches!(&b[1], Block::Heading { level: 3, lemma } if lemma == "Direct heading"));
    // Consecutive quote paragraphs share one blockquote.
    assert!(matches!(&b[2], Block::Open { .. }));
    assert!(matches!(&b[3], Block::Text { text } if text == "Quoted one"));
    assert!(matches!(&b[4], Block::Text { text } if text == "Quoted two"));
    assert!(matches!(&b[5], Block::Close { .. }));
    assert!(matches!(&b[6], Block::Paragraph { text } if text == "Plain"));
    assert_eq!(b.len(), 7);
}

#[test]
fn tracked_changes_read_as_the_accepted_view() {
    let body = r#"<text:tracked-changes>
          <text:changed-region text:id="ct1"><text:deletion><office:change-info><dc:creator>x</dc:creator></office:change-info><text:p>DELETED TEXT</text:p></text:deletion></text:changed-region>
          <text:changed-region text:id="ct2"><text:insertion><office:change-info><dc:creator>x</dc:creator></office:change-info></text:insertion></text:changed-region>
        </text:tracked-changes>
        <text:p>Before <text:change text:change-id="ct1"/><text:change-start text:change-id="ct2"/>inserted<text:change-end text:change-id="ct2"/> after.</text:p>"#;
    let b = blocks(&package(&content(body), &styles(""), None)).unwrap();
    assert_eq!(b.len(), 1);
    assert!(matches!(&b[0], Block::Paragraph { text } if text == "Before inserted after."));
}

#[test]
fn marks_refs_and_an_endnote() {
    let body = r##"<text:p>See <text:alphabetical-index-mark text:string-value="Aunt Polly"/>Polly<text:alphabetical-index-mark-start text:id="m1"/>Tom Sawyer<text:alphabetical-index-mark-end text:id="m1"/> in <text:bookmark-ref text:ref-name="chapter-i" text:reference-format="text">Chapter I</text:bookmark-ref>.<text:note text:id="e1" text:note-class="endnote"><text:note-citation>i</text:note-citation><text:note-body><text:p>An endnote.</text:p></text:note-body></text:note></text:p>
        <text:p><text:bookmark text:name="later"/>A later paragraph<text:reference-mark text:name="point"/> with a point.</text:p>
        <text:p>A <text:a xlink:href="#chapter-i|outline">heading link</text:a> and <text:a xlink:href="http://example.org/">an external one</text:a>.</text:p>
        <text:table-of-content><text:index-body><text:p>Generated contents line</text:p></text:index-body></text:table-of-content>"##;
    let m = parse(&package(&content(body), &styles(""), None)).unwrap();
    assert_eq!(
        values(&m, "//index-mark::term"),
        ["Aunt Polly", "Tom Sawyer"]
    );
    assert_eq!(values(&m, "//endnote<deixis> @| count"), ["1"]);
    assert_eq!(values(&m, "//endnote<note>::"), ["An endnote."]);
    assert_eq!(
        values(&m, "//ref::target"),
        ["chapter-i", "chapter-i", "http://example.org/"]
    );
    assert_eq!(values(&m, "//ref[1]::"), ["Chapter I"]);
    assert_eq!(values(&m, "//paragraph[2]::onym"), ["later"]);
    assert_eq!(values(&m, "//anchor::onym"), ["point"]);
    // Three in the flow, one in the endnote's body.
    assert_eq!(values(&m, "//paragraph @| count"), ["4"]);
    assert_eq!(
        values(&m, "//paragraph[1]::"),
        ["See PollyTom Sawyer in Chapter I."]
    );
}

#[test]
fn head_from_meta() {
    let meta = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-meta {NS}><office:meta><dc:title>Pride and Prejudice</dc:title><meta:initial-creator>Jane Austen</meta:initial-creator><dc:description>A novel.</dc:description><dc:subject>Novels</dc:subject><meta:keyword>regency</meta:keyword><meta:keyword>manners</meta:keyword><dc:language>en-GB</dc:language></office:meta></office:document-meta>"#
    );
    let m = parse(&package(
        &content("<text:p>It is a truth universally acknowledged.</text:p>"),
        &styles(""),
        Some(&meta),
    ))
    .unwrap();
    assert_eq!(values(&m, "^::title"), ["Pride and Prejudice"]);
    assert_eq!(values(&m, "^::author"), ["Jane Austen"]);
    assert_eq!(values(&m, "^::description"), ["A novel."]);
    assert_eq!(values(&m, "^::tags"), ["regency, manners"]);
}
