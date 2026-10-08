//! An EPUB built in-test: the container → package → spine chain,
//! spine order over manifest order, linear="no" skipped, hrefs
//! resolved against the package directory (with percent-escapes),
//! and the chapters' own headings carrying the outline.

use std::io::Write;

use quarb::QueryResult;
use quarb_text_epub::parse;

const CONTAINER: &str = r#"<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf"
              media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#;

const OPF: &str = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <manifest>
    <item id="c2" href="ch%202.xhtml" media-type="application/xhtml+xml"/>
    <item id="c1" href="text/ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="notes" href="notes.xhtml" media-type="application/xhtml+xml"/>
    <item id="ghost" href="missing.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="c1"/>
    <itemref idref="c2"/>
    <itemref idref="notes" linear="no"/>
    <itemref idref="ghost"/>
  </spine>
</package>"#;

const CH1: &str = r#"<html><body>
<h1>The war</h1>
<p>Emus advanced on the wheat.</p>
<blockquote><p>They can face machine guns.</p></blockquote>
</body></html>"#;

const CH2: &str = r#"<html><body>
<h1>Aftermath</h1>
<h2>Questions</h2>
<p>Asked in parliament.</p>
</body></html>"#;

const NOTES: &str = r#"<html><body><p>NOT IN READING ORDER</p></body></html>"#;

fn epub() -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut cursor);
        let o = zip::write::SimpleFileOptions::default();
        for (name, body) in [
            ("META-INF/container.xml", CONTAINER),
            ("OEBPS/content.opf", OPF),
            ("OEBPS/text/ch1.xhtml", CH1),
            ("OEBPS/ch 2.xhtml", CH2),
            ("OEBPS/notes.xhtml", NOTES),
        ] {
            z.start_file(name, o).unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }
    cursor.into_inner()
}

fn values(model: &quarb_text::TextModel, q: &str) -> Vec<String> {
    match quarb::run(q, model).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| model.locator(*n)).collect(),
    }
}

#[test]
fn spine_order_and_chapter_outline() {
    let m = parse(&epub()).unwrap();
    // declared spine order, not manifest order; the chapters' own
    // headings carry the outline
    assert_eq!(values(&m, "/section::lemma"), ["The war", "Aftermath"]);
    assert_eq!(
        values(&m, r#"/section[::lemma = "Aftermath"]/section::lemma"#),
        ["Questions"]
    );
    assert_eq!(
        values(&m, r#"//section[:: *= "machine guns"]::lemma"#),
        ["The war"]
    );
    assert_eq!(values(&m, "//blockquote @| count"), ["1"]);
}

#[test]
fn linear_no_is_out_of_the_reading() {
    let m = parse(&epub()).unwrap();
    assert!(
        values(&m, r#"//paragraph[:: *= "NOT IN READING ORDER"]"#).is_empty(),
        "linear=\"no\" spine items are declared out of order"
    );
}

#[test]
fn not_an_epub_refuses() {
    assert!(parse(b"not a zip").is_err());
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut cursor);
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("word/document.xml", o).unwrap();
        z.write_all(b"<w:document/>").unwrap();
        z.finish().unwrap();
    }
    let bytes = cursor.into_inner();
    let e = match parse(&bytes) {
        Err(e) => e,
        Ok(_) => panic!("expected an error"),
    };
    assert!(e.to_string().contains("container.xml"), "{e}");
}

/// Finding 15: each chapter's ids are its own namespace — the same
/// `id` in two chapters names two bearers, a `#frag` reference
/// lands in its own chapter and `other.xhtml#frag` in that one.
#[test]
fn chapter_ids_keep_to_their_chapter() {
    const A: &str = r##"<html><body><h1>A</h1>
<p id="n1">A's note.</p>
<p>See <a href="#n1">here</a> and <a href="b.xhtml#n1">there</a>.</p>
</body></html>"##;
    const B: &str = r##"<html><body><h1>B</h1>
<p id="n1">B's note.</p>
</body></html>"##;
    let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <manifest>
    <item id="a" href="a.xhtml" media-type="application/xhtml+xml"/>
    <item id="b" href="b.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="a"/><itemref idref="b"/></spine>
</package>"#;
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut cursor);
        let opts = zip::write::SimpleFileOptions::default();
        z.start_file("META-INF/container.xml", opts).unwrap();
        z.write_all(CONTAINER.as_bytes()).unwrap();
        z.start_file("OEBPS/content.opf", opts).unwrap();
        z.write_all(opf.as_bytes()).unwrap();
        z.start_file("OEBPS/a.xhtml", opts).unwrap();
        z.write_all(A.as_bytes()).unwrap();
        z.start_file("OEBPS/b.xhtml", opts).unwrap();
        z.write_all(B.as_bytes()).unwrap();
        z.finish().unwrap();
    }
    let m = parse(&cursor.into_inner()).unwrap();
    assert_eq!(values(&m, "//ref @| count"), ["2"]);
    assert_eq!(values(&m, "//ref-->::"), ["A's note.", "B's note."]);
    assert_eq!(values(&m, "//ref<dangling> @| count"), ["0"]);
    assert_eq!(
        values(&m, "//paragraph[::onym = \"a.xhtml#n1\"]::"),
        ["A's note."]
    );
}
