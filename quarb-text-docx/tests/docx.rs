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
    // the shared table rule's lowering: rows are items, cells carry the
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

fn expect_err(
    r: Result<quarb_text::TextModel, quarb_text_docx::DocxError>,
) -> quarb_text_docx::DocxError {
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
    assert_eq!(values(&m, r"//*<note><-footnote\*::"), ["Emus advanced."]);
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
            Block::Heading {
                level: 1,
                lemma: "T".into()
            },
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

/// Word's citation machinery: CITATION fields are cit marks
/// (the tag as target, `\p` as the locator; the cached printed
/// result stays prose), the b:Sources custom XML part is the
/// bib list under the Latin campi, and the BIBLIOGRAPHY field's
/// generated listing is not prose.
#[test]
fn citation_fields_and_the_sources_part() {
    const SOURCES: &str = r#"<?xml version="1.0"?>
<b:Sources xmlns:b="http://schemas.openxmlformats.org/officeDocument/2006/bibliography">
  <b:Source><b:Tag>Gib76</b:Tag><b:SourceType>Book</b:SourceType>
    <b:Author><b:Author><b:NameList><b:Person><b:Last>Gibbon</b:Last><b:First>Edward</b:First></b:Person></b:NameList></b:Author></b:Author>
    <b:Title>The History of the Decline and Fall of the Roman Empire</b:Title>
    <b:Year>1776</b:Year><b:City>London</b:City><b:Publisher>Strahan &amp; Cadell</b:Publisher></b:Source>
  <b:Source><b:Tag>Her</b:Tag><b:SourceType>Book</b:SourceType>
    <b:Author><b:Author><b:Corporate>Herodotus</b:Corporate></b:Author></b:Author>
    <b:Title>The Histories</b:Title></b:Source>
  <b:Source><b:Tag>Hom</b:Tag><b:SourceType>JournalArticle</b:SourceType>
    <b:Author><b:Author><b:NameList><b:Person><b:Last>Homer</b:Last></b:Person></b:NameList></b:Author></b:Author>
    <b:Title>The Iliad</b:Title><b:JournalName>Epics</b:JournalName></b:Source>
</b:Sources>"#;
    let body = "<w:p><w:r><w:t xml:space=\"preserve\">Gibbon says so </w:t></w:r>\
        <w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
        <w:r><w:instrText xml:space=\"preserve\"> CITATION Gib76 \\l 1033 \\p 15 </w:instrText></w:r>\
        <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>\
        <w:r><w:t>(Gibbon, 1776, p. 15)</w:t></w:r>\
        <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>\
        <w:r><w:t xml:space=\"preserve\"> and Herodotus </w:t></w:r>\
        <w:fldSimple w:instr=\" CITATION Her \\l 1033 \"><w:r><w:t>(Herodotus)</w:t></w:r></w:fldSimple>\
        <w:r><w:t xml:space=\"preserve\"> and a ghost </w:t></w:r>\
        <w:fldSimple w:instr=\" CITATION Twa83 \\l 1033 \"><w:r><w:t>(Twain, 1883)</w:t></w:r></w:fldSimple>\
        <w:r><w:t>.</w:t></w:r></w:p>\
        <w:p><w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr><w:r><w:t>References</w:t></w:r></w:p>\
        <w:p><w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
        <w:r><w:instrText xml:space=\"preserve\"> BIBLIOGRAPHY </w:instrText></w:r>\
        <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>\
        <w:r><w:t>Gibbon, E. (1776). The History.</w:t></w:r></w:p>\
        <w:p><w:r><w:t>Herodotus. The Histories.</w:t></w:r>\
        <w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p>\
        <w:p><w:r><w:t>Closing words.</w:t></w:r></w:p>";
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
        z.start_file("customXml/item1.xml", o).unwrap();
        z.write_all(SOURCES.as_bytes()).unwrap();
        z.finish().unwrap();
    }
    let m = parse(&cursor.into_inner()).unwrap();
    // The marks, in order, with the one locator.
    assert_eq!(values(&m, "//cit::target"), ["Gib76", "Her", "Twa83"]);
    assert_eq!(values(&m, "//cit[::locator]::locator"), ["15"]);
    // The printed result is prose; the field machinery is not.
    assert_eq!(
        values(&m, "//paragraph[1]::"),
        [
            "Gibbon says so (Gibbon, 1776, p. 15) and Herodotus (Herodotus) and a ghost (Twain, 1883)."
        ]
    );
    // The sources: tag, authors "Last, First" (a corporate name
    // as is), the campi under the Latin canon, the genus.
    assert_eq!(values(&m, "//bib::onym"), ["Gib76", "Her", "Hom"]);
    assert_eq!(
        values(&m, "//bib::auctor"),
        ["Gibbon, Edward", "Herodotus", "Homer"]
    );
    assert_eq!(
        values(&m, "//bib[::officina]::officina"),
        ["Strahan & Cadell"]
    );
    assert_eq!(
        values(&m, "//bib:::genus"),
        ["liber", "liber", "commentarius"]
    );
    assert_eq!(values(&m, "//bib[::onym = \"Hom\"]::ephemeris"), ["Epics"]);
    // Resolution in-document: the arrow, the two linters.
    assert_eq!(
        values(&m, "//cit[::target = \"Gib76\"]->*::titulus"),
        ["The History of the Decline and Fall of the Roman Empire"]
    );
    assert_eq!(values(&m, "//cit<dangling>::target"), ["Twa83"]);
    assert_eq!(values(&m, "//bib[!<-cit]::onym"), ["Hom"]);
    // The BIBLIOGRAPHY listing is dropped; the heading and the
    // paragraph after the field stay.
    assert_eq!(values(&m, "//section::lemma"), ["References"]);
    assert_eq!(values(&m, "//section//paragraph::"), ["Closing words."]);
}

#[test]
fn core_properties_declare_the_identity() {
    let core = "<?xml version=\"1.0\"?>\
        <cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" \
        xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\">\
        <dc:title>Pride and Prejudice</dc:title><dc:creator>Jane Austen</dc:creator>\
        <dc:description>A novel.</dc:description><dc:subject>Novels</dc:subject>\
        <cp:keywords>regency; manners</cp:keywords><dc:language>en-GB</dc:language>\
        <dcterms:created>1813-01-28T00:00:00Z</dcterms:created></cp:coreProperties>";
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut cursor);
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("word/document.xml", o).unwrap();
        z.write_all(b"<?xml version=\"1.0\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:r><w:t>It is a truth universally acknowledged.</w:t></w:r></w:p></w:body></w:document>").unwrap();
        z.start_file("docProps/core.xml", o).unwrap();
        z.write_all(core.as_bytes()).unwrap();
        z.finish().unwrap();
    }
    let m = parse(&cursor.into_inner()).unwrap();
    assert_eq!(values(&m, "^::title"), ["Pride and Prejudice"]);
    assert_eq!(values(&m, "^::author"), ["Jane Austen"]);
    assert_eq!(values(&m, "^::description"), ["A novel."]);
    assert_eq!(values(&m, "^::tags"), ["regency, manners"]);
    // the record is verbatim: the names the package writes
    assert_eq!(
        values(&m, "::head:\"dcterms:created\""),
        ["1813-01-28T00:00:00Z"]
    );
    assert_eq!(values(&m, "::published"), ["1813-01-28T00:00:00Z"]);
    assert_eq!(
        values(&m, "::head"),
        [concat!(
            r#"%("dc:title" = "Pride and Prejudice"; "dc:creator" = "Jane Austen"; "#,
            r#""dc:description" = "A novel."; "dc:subject" = "Novels"; "#,
            r#""cp:keywords" = "regency; manners"; "dc:language" = "en-GB"; "#,
            r#""dcterms:created" = "1813-01-28T00:00:00Z")"#
        )]
    );
    // the declared language is the curated `::lang`
    assert_eq!(values(&m, "::lang"), ["en-GB"]);
    // the plain names this reader answered under until 0.63 are told
    // where they went (RETIRED_KEY names the field): `:::title` read
    // the title, and answered null in silence once the record took
    // the file's names
    let head = m.head_meta();
    assert_eq!(head.retired("title").as_deref(), Some(r#""dc:title""#));
    assert_eq!(head.retired("author").as_deref(), Some(r#""dc:creator""#));
    assert_eq!(
        head.retired("created").as_deref(),
        Some(r#""dcterms:created""#)
    );
    assert_eq!(head.retired("dc:title").as_deref(), Some(r#""dc:title""#));
    // a name the file never declared has nowhere to point
    assert_eq!(head.retired("modified"), None);
    assert_eq!(head.retired("publisher"), None);
    assert_eq!(values(&m, "^:::title"), [""]);
}

#[test]
fn a_body_level_bookmark_names_the_block_that_follows() {
    // pandoc writes a heading's bookmark between the paragraphs,
    // not inside the heading's.
    let body = concat!(
        "<w:bookmarkStart w:id=\"17\" w:name=\"book-the-first\"/>",
        "<w:p><w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr><w:r><w:t>Book the First</w:t></w:r></w:p>",
        "<w:bookmarkEnd w:id=\"17\"/>",
        "<w:p><w:r><w:t>Recalled to life.</w:t></w:r></w:p>",
    );
    let m = parse(&docx(body)).unwrap();
    assert_eq!(values(&m, "//section::onym"), ["book-the-first"]);
    assert_eq!(values(&m, "//paragraph::onym"), [""]);
}

/// A docx of a body and whatever other parts are named.
fn docx_with(body: &str, parts: &[(&str, &str)]) -> Vec<u8> {
    let document = format!(
        "<?xml version=\"1.0\"?>\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
         xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\">\
         <w:body>{body}</w:body></w:document>"
    );
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut cursor);
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("word/document.xml", o).unwrap();
        z.write_all(document.as_bytes()).unwrap();
        for (name, text) in parts {
            z.start_file(*name, o).unwrap();
            z.write_all(text.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }
    cursor.into_inner()
}

/// A text box as Word writes it: the modern drawing and, for older
/// readers, the same content again as VML.
fn text_box(inner: &str) -> String {
    format!(
        "<w:r><mc:AlternateContent>\
           <mc:Choice Requires=\"wps\"><w:drawing><wps:txbx><w:txbxContent>{inner}\
             </w:txbxContent></wps:txbx></w:drawing></mc:Choice>\
           <mc:Fallback><w:pict><v:shape><v:textbox><w:txbxContent>{inner}\
             </w:txbxContent></v:textbox></v:shape></w:pict></mc:Fallback>\
         </mc:AlternateContent></w:r>"
    )
}

/// A text box is blocks of its own after the paragraph it is
/// anchored in, read once. Its text was merged into that paragraph,
/// twice (the drawing and its VML fallback), and its paragraph
/// properties overwrote the paragraph's own.
#[test]
fn a_text_box_is_blocks_of_its_own_after_its_paragraph() {
    let boxed = format!(
        "{}{}",
        p_styled("Heading2", "Aside"),
        p("The suitors feast in the hall.")
    );
    let body = format!(
        "<w:p><w:r><w:t>Penelope weaves </w:t></w:r>{}<w:r><w:t>by day.</w:t></w:r></w:p>{}",
        text_box(&boxed),
        p("She unravels by night.")
    );
    let m = parse(&docx_with(&body, &[("word/styles.xml", STYLES)])).unwrap();
    // the anchor paragraph is its own sentence, and a paragraph (the
    // box's heading style did not make it a heading)
    assert_eq!(
        values(&m, "//paragraph::"),
        [
            "Penelope weaves by day.",
            "The suitors feast in the hall.",
            "She unravels by night."
        ]
    );
    // the box's own heading is a heading, once
    assert_eq!(values(&m, "//section::lemma"), ["Aside"]);
    // block order: the anchor, then the box
    assert_eq!(values(&m, "/* | :name"), ["paragraph", "section"]);

    // a box in a paragraph that holds nothing else, and the VML form
    // alone (an older writer): read all the same
    let old = format!(
        "<w:p><w:r><w:pict><v:shape><v:textbox><w:txbxContent>{}</w:txbxContent>\
         </v:textbox></v:shape></w:pict></w:r></w:p>",
        p("A note in the margin.")
    );
    let m = parse(&docx_with(&old, &[("word/styles.xml", STYLES)])).unwrap();
    assert_eq!(values(&m, "//paragraph::"), ["A note in the margin."]);

    // in a table cell the box's text is the cell's, once
    let cell = format!(
        "<w:tbl><w:tr><w:tc><w:p><w:r><w:t>loom </w:t></w:r>{}</w:p></w:tc></w:tr></w:tbl>",
        text_box(&p("shroud"))
    );
    let m = parse(&docx_with(&cell, &[("word/styles.xml", STYLES)])).unwrap();
    assert_eq!(values(&m, "//*<cell>::"), ["loom shroud"]);

    // a fallback that says what its choice could not is read
    let only_fallback = "<w:p><w:r><w:t>Face: </w:t></w:r>\
        <mc:AlternateContent><mc:Choice Requires=\"w16\"><w:r><w16:glyph/></w:r></mc:Choice>\
        <mc:Fallback><w:r><w:t>smiling</w:t></w:r></mc:Fallback></mc:AlternateContent></w:p>";
    let m = parse(&docx_with(only_fallback, &[("word/styles.xml", STYLES)])).unwrap();
    assert_eq!(values(&m, "//paragraph::"), ["Face: smiling"]);
}

/// A list declared only through a paragraph style (Word's List
/// Bullet, List Number: the `w:numPr` is in styles.xml) is a list;
/// it read as plain paragraphs.
#[test]
fn a_list_declared_through_a_style_is_a_list() {
    let styles = r#"<?xml version="1.0"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="ListBullet">
    <w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr>
  </w:style>
  <w:style w:type="paragraph" w:styleId="ListNumber">
    <w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr>
  </w:style>
  <w:style w:type="paragraph" w:styleId="ListNumberTight">
    <w:basedOn w:val="ListNumber"/>
  </w:style>
  <w:style w:type="paragraph" w:styleId="ListBullet2">
    <w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr>
  </w:style>
  <w:style w:type="paragraph" w:styleId="Heading1">
    <w:pPr><w:numPr><w:numId w:val="2"/></w:numPr><w:outlineLvl w:val="0"/></w:pPr>
  </w:style>
</w:styles>"#;
    // the second bullet level is linked to its style in numbering.xml
    let numbering = r#"<?xml version="1.0"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="10">
    <w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl>
    <w:lvl w:ilvl="1"><w:pStyle w:val="ListBullet2"/><w:numFmt w:val="bullet"/></w:lvl>
  </w:abstractNum>
  <w:abstractNum w:abstractNumId="20">
    <w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl>
  </w:abstractNum>
  <w:num w:numId="1"><w:abstractNumId w:val="10"/></w:num>
  <w:num w:numId="2"><w:abstractNumId w:val="20"/></w:num>
</w:numbering>"#;
    let unnumbered = "<w:p><w:pPr><w:pStyle w:val=\"ListBullet\"/>\
        <w:numPr><w:numId w:val=\"0\"/></w:numPr></w:pPr>\
        <w:r><w:t>No list for me.</w:t></w:r></w:p>";
    let body = format!(
        "{}{}{}{}{}{}{}{}",
        p_styled("Heading1", "Provisions"),
        p_styled("ListBullet", "grain"),
        p_styled("ListBullet2", "barley"),
        p_styled("ListBullet", "wine"),
        p_styled("ListNumber", "load the ship"),
        p_styled("ListNumberTight", "raise the mast"),
        unnumbered,
        p("Then they sailed.")
    );
    let m = parse(&docx_with(
        &body,
        &[
            ("word/styles.xml", styles),
            ("word/numbering.xml", numbering),
        ],
    ))
    .unwrap();
    // a numbered heading style is a heading, not a list item
    assert_eq!(values(&m, "//section::lemma"), ["Provisions"]);
    // two items at the top, the first holding the nested list
    assert_eq!(
        values(&m, "/section/unordered-list/unordered-item @| count"),
        ["2"]
    );
    assert_eq!(
        values(&m, "/section/unordered-list/unordered-item[2]::"),
        ["wine"]
    );
    // the nested level, by the style the level is linked to
    assert_eq!(
        values(&m, "//unordered-item/unordered-list/unordered-item::"),
        ["barley"]
    );
    // a style based on a list style is in its list
    assert_eq!(
        values(&m, "//ordered-item::"),
        ["load the ship", "raise the mast"]
    );
    // the paragraph's own zero takes it out of its style's list
    assert_eq!(
        values(&m, "//paragraph::"),
        ["No list for me.", "Then they sailed."]
    );
}

/// `w:tblHeader` is an on/off property: `w:val="false"` declares no
/// header row. Any tblHeader made the first row a header.
#[test]
fn a_header_row_declared_off_is_no_header() {
    let table = |flag: &str| {
        format!(
            "<w:tbl><w:tr><w:trPr>{flag}</w:trPr>\
               <w:tc><w:p><w:r><w:t>Name</w:t></w:r></w:p></w:tc>\
               <w:tc><w:p><w:r><w:t>Port</w:t></w:r></w:p></w:tc></w:tr>\
             <w:tr><w:tc><w:p><w:r><w:t>web</w:t></w:r></w:p></w:tc>\
               <w:tc><w:p><w:r><w:t>8080</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"
        )
    };
    for off in [
        "<w:tblHeader w:val=\"false\"/>",
        "<w:tblHeader w:val=\"0\"/>",
        "<w:tblHeader w:val=\"off\"/>",
    ] {
        let m = parse(&docx(&table(off))).unwrap();
        assert_eq!(values(&m, "//*<cell>[::lemma] @| count"), ["0"], "{off}");
        assert_eq!(values(&m, "//*<cell> @| count"), ["4"], "{off}");
    }
    for on in [
        "<w:tblHeader/>",
        "<w:tblHeader w:val=\"true\"/>",
        "<w:tblHeader w:val=\"1\"/>",
        "<w:tblHeader w:val=\"on\"></w:tblHeader>",
    ] {
        let m = parse(&docx(&table(on))).unwrap();
        assert_eq!(
            values(&m, r#"//*<cell>[::lemma = "Port"]::"#),
            ["Port: 8080"],
            "{on}"
        );
    }
}

/// A note callout in a table cell is kept (it was lost, and its
/// body dangled unreferenced); the marks in a note's own text stand
/// on its paragraph (they were dropped).
#[test]
fn callouts_in_cells_and_marks_in_notes_are_kept() {
    let footnotes = r#"<?xml version="1.0"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:footnote w:id="2"><w:p><w:r><w:t>Measured at Piraeus.</w:t></w:r>
    <w:r><w:fldChar w:fldCharType="begin"/></w:r>
    <w:r><w:instrText> XE "Piraeus" </w:instrText></w:r>
    <w:r><w:fldChar w:fldCharType="end"/></w:r></w:p></w:footnote>
</w:footnotes>"#;
    let body = "<w:tbl><w:tr>\
        <w:tc><w:p><w:r><w:t>grain</w:t></w:r></w:p></w:tc>\
        <w:tc><w:p><w:r><w:t>40 talents</w:t></w:r>\
          <w:r><w:footnoteReference w:id=\"2\"/></w:r></w:p></w:tc>\
        </w:tr></w:tbl>";
    let m = parse(&docx_with(
        body,
        &[
            ("word/styles.xml", STYLES),
            ("word/footnotes.xml", footnotes),
        ],
    ))
    .unwrap();
    assert_eq!(values(&m, "//*<cell>::"), ["grain", "40 talents"]);
    // the callout reaches its body, and nothing dangles
    assert_eq!(
        values(&m, "//*<deixis>->footnote::"),
        ["Measured at Piraeus."]
    );
    assert_eq!(values(&m, "//*<dangling> @| count"), ["0"]);
    assert_eq!(values(&m, "//*<note>[!<-footnote] @| count"), ["0"]);
    // the index mark in the note
    assert_eq!(values(&m, "//index-mark::term"), ["Piraeus"]);
}

/// The BIBLIOGRAPHY field's listing is dropped; the text of the
/// paragraph it ends in is not (the paragraph was emptied).
#[test]
fn the_bibliography_field_keeps_the_text_around_it() {
    let body = "<w:p><w:r><w:t>Sources: </w:t></w:r>\
        <w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
        <w:r><w:instrText> BIBLIOGRAPHY </w:instrText></w:r>\
        <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>\
        <w:r><w:t>Gibbon, E. (1776). The Decline and Fall.</w:t></w:r></w:p>\
        <w:p><w:r><w:t>Herodotus. The Histories.</w:t></w:r>\
        <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>\
        <w:r><w:t> End of the list.</w:t></w:r></w:p>";
    let m = parse(&docx(body)).unwrap();
    assert_eq!(
        values(&m, "//paragraph::"),
        ["Sources:", "End of the list."]
    );
    // a field that begins and ends in one paragraph
    let one = "<w:p><w:r><w:t>See </w:t></w:r>\
        <w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
        <w:r><w:instrText> BIBLIOGRAPHY </w:instrText></w:r>\
        <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>\
        <w:r><w:t>Gibbon, E. (1776).</w:t></w:r>\
        <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>\
        <w:r><w:t>above.</w:t></w:r></w:p>";
    let m = parse(&docx(one)).unwrap();
    assert_eq!(values(&m, "//paragraph::"), ["See above."]);
}
