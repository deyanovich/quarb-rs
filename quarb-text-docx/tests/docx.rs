//! A docx built in-test (the quarb-archive fixture pattern):
//! declared outline levels (direct and via basedOn), the quote
//! chain, bullet/ordered/nested numbering, the declared header
//! row, and the accepted view of tracked changes — asserted at
//! the block level and through real queries.

use std::io::Write;

use quarb::QueryResult;
use quarb_text_docx::{blocks, parse};

const STYLES: &str = r#"<?xml version="1.0"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="Heading1">
    <w:outlineLvl w:val="0"/>
  </w:style>
  <w:style w:type="paragraph" w:styleId="Heading2">
    <w:outlineLvl w:val="1"/>
  </w:style>
  <w:style w:type="paragraph" w:styleId="MyTitle">
    <w:basedOn w:val="Heading1"/>
  </w:style>
  <w:style w:type="paragraph" w:styleId="Quote"/>
  <w:style w:type="paragraph" w:styleId="FancyQuote">
    <w:basedOn w:val="Quote"/>
  </w:style>
</w:styles>"#;

const NUMBERING: &str = r#"<?xml version="1.0"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="10">
    <w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl>
    <w:lvl w:ilvl="1"><w:numFmt w:val="bullet"/></w:lvl>
  </w:abstractNum>
  <w:abstractNum w:abstractNumId="20">
    <w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl>
  </w:abstractNum>
  <w:num w:numId="1"><w:abstractNumId w:val="10"/></w:num>
  <w:num w:numId="2"><w:abstractNumId w:val="20"/></w:num>
</w:numbering>"#;

fn p_styled(style: &str, text: &str) -> String {
    format!(
        "<w:p><w:pPr><w:pStyle w:val=\"{style}\"/></w:pPr>\
         <w:r><w:t>{text}</w:t></w:r></w:p>"
    )
}

fn p(text: &str) -> String {
    format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>")
}

fn p_num(num: &str, ilvl: u8, text: &str) -> String {
    format!(
        "<w:p><w:pPr><w:numPr><w:ilvl w:val=\"{ilvl}\"/>\
         <w:numId w:val=\"{num}\"/></w:numPr></w:pPr>\
         <w:r><w:t>{text}</w:t></w:r></w:p>"
    )
}

fn docx(body: &str) -> Vec<u8> {
    let document = format!(
        "<?xml version=\"1.0\"?>\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:body>{body}</w:body></w:document>"
    );
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut cursor);
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("word/document.xml", o).unwrap();
        z.write_all(document.as_bytes()).unwrap();
        z.start_file("word/styles.xml", o).unwrap();
        z.write_all(STYLES.as_bytes()).unwrap();
        z.start_file("word/numbering.xml", o).unwrap();
        z.write_all(NUMBERING.as_bytes()).unwrap();
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
fn declared_outline_makes_sections() {
    let body = [
        p_styled("Heading1", "The war"),
        p("Emus advanced."),
        p_styled("Heading2", "First attempt"),
        p("The gun jammed."),
        // basedOn inheritance: MyTitle is a level-1 heading
        p_styled("MyTitle", "Aftermath"),
        // direct formatting outranks the (absent) style
        "<w:p><w:pPr><w:outlineLvl w:val=\"1\"/></w:pPr>\
         <w:r><w:t>Questions</w:t></w:r></w:p>"
            .to_string(),
        p("Asked in parliament."),
    ]
    .concat();
    let m = parse(&docx(&body)).unwrap();
    // sections nest by declared level: 2 under 1, and the
    // direct-formatting heading nests under MyTitle's inherited 1
    assert_eq!(values(&m, "/section[1]/section::lemma"), ["First attempt"]);
    assert_eq!(values(&m, "/section[2]/section::lemma"), ["Questions"]);
    assert_eq!(
        values(&m, "//section::lemma"),
        ["The war", "First attempt", "Aftermath", "Questions"]
    );
    assert_eq!(
        values(&m, r#"//section[::lemma = "Questions"]//paragraph::"#),
        ["Asked in parliament."]
    );
}

#[test]
fn quotes_group_and_lists_nest() {
    let body = [
        p_num("1", 0, "alpha"),
        p_num("1", 1, "alpha-one"),
        p_num("1", 0, "beta"),
        p_num("2", 0, "first"),
        p_styled("FancyQuote", "We few."),
        p_styled("FancyQuote", "We happy few."),
        p("Prose resumes."),
    ]
    .concat();
    let m = parse(&docx(&body)).unwrap();
    assert_eq!(
        values(&m, "//unordered-item::"),
        ["alpha\nalpha-one", "alpha-one", "beta"],
        "bullet items, nested under alpha (flattened prose joins by line)"
    );
    assert_eq!(values(&m, "//ordered-item::"), ["first"]);
    // the two consecutive quote paragraphs are ONE blockquote
    assert_eq!(values(&m, "//blockquote @| count"), ["1"]);
    assert_eq!(values(&m, "//blockquote::"), ["We few. We happy few."]);
    assert_eq!(values(&m, "/paragraph::"), ["Prose resumes."]);
}

#[test]
fn tracked_changes_read_accepted() {
    let body = "<w:p><w:r><w:t>kept </w:t></w:r>\
        <w:ins><w:r><w:t>inserted </w:t></w:r></w:ins>\
        <w:del><w:r><w:delText>deleted </w:delText></w:r></w:del>\
        <w:r><w:t>tail</w:t></w:r></w:p>";
    let m = parse(&docx(body)).unwrap();
    assert_eq!(values(&m, "/paragraph::"), ["kept inserted tail"]);
}

#[test]
fn declared_header_row_labels_cells() {
    let body = "<w:tbl>\
        <w:tr><w:trPr><w:tblHeader/></w:trPr>\
          <w:tc><w:p><w:r><w:t>Name</w:t></w:r></w:p></w:tc>\
          <w:tc><w:p><w:r><w:t>Port</w:t></w:r></w:p></w:tc></w:tr>\
        <w:tr>\
          <w:tc><w:p><w:r><w:t>web</w:t></w:r></w:p></w:tc>\
          <w:tc><w:p><w:r><w:t>8080</w:t></w:r></w:p></w:tc></w:tr>\
        </w:tbl>";
    let m = parse(&docx(body)).unwrap();
    // ruling #25's lowering: rows are items, cells carry the
    // column name as ::lemma
    assert_eq!(
        values(&m, r#"//*<cell>[::lemma = "Port"]::"#),
        ["Port: 8080"]
    );
}

#[test]
fn headerless_table_stays_lemmaless() {
    let body = "<w:tbl><w:tr>\
        <w:tc><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc>\
        <w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p></w:tc>\
        </w:tr></w:tbl>";
    let m = parse(&docx(body)).unwrap();
    assert_eq!(values(&m, "//*<cell> @| count"), ["2"]);
    assert_eq!(values(&m, "//*<cell>[::lemma] @| count"), ["0"]);
}

#[test]
fn body_level_nine_is_not_a_heading() {
    let body = "<w:p><w:pPr><w:outlineLvl w:val=\"9\"/></w:pPr>\
        <w:r><w:t>Body text</w:t></w:r></w:p>";
    let m = parse(&docx(body)).unwrap();
    assert_eq!(values(&m, "//section @| count"), ["0"]);
    assert_eq!(values(&m, "/paragraph::"), ["Body text"]);
}

fn expect_err(r: Result<quarb_text::TextModel, quarb_text_docx::DocxError>) -> quarb_text_docx::DocxError {
    match r {
        Err(e) => e,
        Ok(_) => panic!("expected an error"),
    }
}

#[test]
fn footnotes_are_the_apparatus() {
    // a body in word/footnotes.xml, a callout in the run stream;
    // the separator pseudo-notes must not become notes
    let footnotes = r#"<?xml version="1.0"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:t> </w:t></w:r></w:p></w:footnote>
  <w:footnote w:id="2"><w:p><w:r><w:t>Twenty thousand of them.</w:t></w:r></w:p></w:footnote>
</w:footnotes>"#;
    let body = "<w:p><w:r><w:t>Emus advanced.</w:t></w:r>\
        <w:r><w:footnoteReference w:id=\"2\"/></w:r></w:p>";
    let document = format!(
        "<?xml version=\"1.0\"?>\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:body>{body}</w:body></w:document>"
    );
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut cursor);
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("word/document.xml", o).unwrap();
        z.write_all(document.as_bytes()).unwrap();
        z.start_file("word/styles.xml", o).unwrap();
        z.write_all(STYLES.as_bytes()).unwrap();
        z.start_file("word/footnotes.xml", o).unwrap();
        z.write_all(footnotes.as_bytes()).unwrap();
        z.finish().unwrap();
    }
    let m = parse(&cursor.into_inner()).unwrap();
    // the flow stays clean; the apparatus is complete
    assert_eq!(values(&m, "/paragraph::"), ["Emus advanced."]);
    assert_eq!(values(&m, "//footnote @| count"), ["2"]);
    assert_eq!(
        values(&m, "//*<deixis>->footnote::"),
        ["Twenty thousand of them."]
    );
    assert_eq!(values(&m, "//*<note>::onym"), ["2"]);
    assert_eq!(
        values(&m, r"//*<note><-footnote\*::"),
        ["Emus advanced."]
    );
    assert_eq!(values(&m, "//*<dangling> @| count"), ["0"]);
}

#[test]
fn not_a_docx_refuses() {
    assert!(parse(b"not a zip").is_err());
    // a zip without word/document.xml is not a Word document
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut cursor);
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("other.txt", o).unwrap();
        z.write_all(b"x").unwrap();
        z.finish().unwrap();
    }
    let e = expect_err(parse(&cursor.into_inner()));
    assert!(e.to_string().contains("word/document.xml"), "{e}");
}

#[test]
fn blocks_stream_shape() {
    use quarb_text::Block;
    let body = [p_styled("Heading1", "T"), p("x")].concat();
    let b = blocks(&docx(&body)).unwrap();
    assert_eq!(
        b,
        vec![
            Block::Heading { level: 1, lemma: "T".into() },
            Block::Paragraph { text: "x".into() },
        ]
    );
}

#[test]
fn endnotes_are_their_own_family() {
    let endnotes = r#"<?xml version="1.0"?>
<w:endnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:endnote w:type="separator" w:id="-1"><w:p><w:r><w:t> </w:t></w:r></w:p></w:endnote>
  <w:endnote w:id="2"><w:p><w:r><w:t>Filed with the ministry.</w:t></w:r></w:p></w:endnote>
</w:endnotes>"#;
    let body = "<w:p><w:r><w:t>The plan.</w:t></w:r>\
        <w:r><w:endnoteReference w:id=\"2\"/></w:r></w:p>";
    let document = format!(
        "<?xml version=\"1.0\"?>\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:body>{body}</w:body></w:document>"
    );
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut cursor);
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("word/document.xml", o).unwrap();
        z.write_all(document.as_bytes()).unwrap();
        z.start_file("word/endnotes.xml", o).unwrap();
        z.write_all(endnotes.as_bytes()).unwrap();
        z.finish().unwrap();
    }
    let m = parse(&cursor.into_inner()).unwrap();
    // its own family, its own edge label; the flow stays clean
    assert_eq!(values(&m, "/paragraph::"), ["The plan."]);
    assert_eq!(values(&m, "//endnote @| count"), ["2"]);
    assert_eq!(values(&m, "//footnote @| count"), ["0"]);
    assert_eq!(
        values(&m, "//*<deixis>->endnote::"),
        ["Filed with the ministry."]
    );
    // <note> marks the body; the callout answers <deixis>
    assert_eq!(values(&m, "//*<note> @| count"), ["1"]);
    assert_eq!(values(&m, "//*<deixis> @| count"), ["1"]);
}

#[test]
fn xe_fields_are_index_marks() {
    // both declared spellings: the simple field, and the
    // instruction split across w:instrText runs
    let body = "<w:p><w:r><w:t>Emus advanced.</w:t></w:r>\
        <w:fldSimple w:instr=\" XE &quot;emu&quot; \"/>\
        <w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
        <w:r><w:instrText> XE </w:instrText></w:r>\
        <w:r><w:instrText>\"wheat districts\" \\b</w:instrText></w:r>\
        <w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p>\
        <w:p><w:r><w:t>A PAGEREF field keeps only its </w:t></w:r>\
        <w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
        <w:r><w:instrText> PAGEREF war </w:instrText></w:r>\
        <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>\
        <w:r><w:t>7</w:t></w:r>\
        <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>\
        <w:r><w:t>.</w:t></w:r></w:p>";
    let m = parse(&docx(body)).unwrap();
    assert_eq!(values(&m, "//index-mark::term"), ["emu", "wheat districts"]);
    // marks are invisible in the prose; a non-XE field keeps only
    // its cached result text
    assert_eq!(values(&m, "/paragraph[1]::"), ["Emus advanced."]);
    assert_eq!(
        values(&m, "/paragraph[2]::"),
        ["A PAGEREF field keeps only its 7."]
    );
}
