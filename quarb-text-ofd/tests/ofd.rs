//! An OFD package at the text level: pages' text codes as lines
//! under the outline's sections, with geometry and the declared
//! identity. The packages are written in-test (the format is a zip
//! of XML), after the entries real producers write.
use std::io::Write;

use quarb::{AstAdapter, NodeId, QueryResult, Value};

const NS: &str = r#"xmlns:ofd="http://www.ofdspec.org/2016""#;

fn zip(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, body) in entries {
        w.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        w.write_all(body.as_bytes()).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn text_object(id: u32, x: f64, y: f64, w: f64, h: f64, cy: f64, text: &str) -> String {
    format!(
        r#"<ofd:TextObject ID="{id}" Boundary="{x} {y} {w} {h}" Font="3" Size="3.5"><ofd:TextCode X="0" Y="{cy}">{text}</ofd:TextCode></ofd:TextObject>"#
    )
}

fn page(objects: &[String]) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><ofd:Page {NS}><ofd:Content><ofd:Layer ID="2">{}</ofd:Layer></ofd:Content></ofd:Page>"#,
        objects.concat()
    )
}

/// Two pages of Tom Sawyer: a heading and two lines on each, a
/// running head from a template page, and an outline of two
/// chapters when `outline` is set.
fn sawyer(outline: bool) -> Vec<u8> {
    let entry = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><ofd:OFD {NS} Version="1.0" DocType="OFD"><ofd:DocBody><ofd:DocInfo><ofd:DocID>1</ofd:DocID><ofd:Title>The Adventures of Tom Sawyer</ofd:Title><ofd:Author>Mark Twain</ofd:Author><ofd:Keywords><ofd:Keyword>boys</ofd:Keyword><ofd:Keyword>Mississippi</ofd:Keyword></ofd:Keywords><ofd:CreationDate>1876-06-01</ofd:CreationDate><ofd:Creator>quarb test</ofd:Creator><ofd:CustomDatas><ofd:CustomData Name="edition">first</ofd:CustomData></ofd:CustomDatas></ofd:DocInfo><ofd:DocRoot>Doc_0/Document.xml</ofd:DocRoot></ofd:DocBody></ofd:OFD>"#
    );
    let outlines = if outline {
        r#"<ofd:Outlines><ofd:OutlineElem Title="Chapter I" Count="1"><ofd:Actions><ofd:Action Event="CLICK"><ofd:Goto><ofd:Dest Type="XYZ" PageID="1" Top="20"/></ofd:Goto></ofd:Action></ofd:Actions><ofd:OutlineElem Title="Aunt Polly"><ofd:Actions><ofd:Action Event="CLICK"><ofd:Goto><ofd:Dest Type="XYZ" PageID="1" Top="40"/></ofd:Goto></ofd:Action></ofd:Actions></ofd:OutlineElem></ofd:OutlineElem><ofd:OutlineElem Title="Chapter II"><ofd:Actions><ofd:Action Event="CLICK"><ofd:Goto><ofd:Bookmark Name="ch2"/></ofd:Goto></ofd:Action></ofd:Actions></ofd:OutlineElem></ofd:Outlines><ofd:Bookmarks><ofd:Bookmark Name="ch2"><ofd:Dest Type="Fit" PageID="21"/></ofd:Bookmark></ofd:Bookmarks>"#
    } else {
        ""
    };
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><ofd:Document {NS}><ofd:CommonData><ofd:PageArea><ofd:PhysicalBox>0 0 210 297</ofd:PhysicalBox></ofd:PageArea><ofd:PublicRes>PublicRes.xml</ofd:PublicRes><ofd:TemplatePage ID="9" BaseLoc="Tpls/Tpl_0/Content.xml"/><ofd:MaxUnitID>40</ofd:MaxUnitID></ofd:CommonData><ofd:Pages><ofd:Page ID="1" BaseLoc="Pages/Page_0/Content.xml"/><ofd:Page ID="21" BaseLoc="Pages/Page_1/Content.xml"/></ofd:Pages>{outlines}</ofd:Document>"#
    );
    let page1 = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><ofd:Page {NS}><ofd:Template TemplateID="9" ZOrder="Background"/><ofd:Content><ofd:Layer ID="2">{}{}{}{}{}</ofd:Layer></ofd:Content></ofd:Page>"#,
        // The front matter, above the first destination.
        text_object(
            10,
            30.0,
            10.0,
            60.0,
            5.0,
            4.0,
            "The Adventures of Tom Sawyer"
        ),
        text_object(11, 30.0, 20.0, 40.0, 5.0, 4.0, "CHAPTER I"),
        // Two codes on one baseline: a gap between their extents
        // is a space.
        text_object(12, 30.0, 30.0, 20.0, 5.0, 4.0, "“TOM!” No"),
        text_object(13, 52.0, 30.0, 20.0, 5.0, 4.0, "answer."),
        text_object(
            14,
            30.0,
            40.0,
            80.0,
            5.0,
            4.0,
            "The old lady pulled her spectacles down."
        ),
    );
    let page2 = page(&[
        text_object(30, 30.0, 20.0, 40.0, 5.0, 4.0, "CHAPTER II"),
        text_object(31, 30.0, 30.0, 80.0, 5.0, 4.0, "Saturday morning was come."),
        // No gap: the two codes meet.
        text_object(32, 30.0, 40.0, 10.0, 5.0, 4.0, "Tom"),
        text_object(33, 40.0, 40.0, 30.0, 5.0, 4.0, " appeared."),
    ]);
    let template = page(&[text_object(40, 150.0, 5.0, 40.0, 4.0, 3.0, "Tom Sawyer")]);
    let res = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><ofd:Res {NS} BaseLoc="Res"><ofd:Fonts><ofd:Font ID="3" FontName="Times" FamilyName="Times"/></ofd:Fonts></ofd:Res>"#
    );
    zip(&[
        ("OFD.xml", &entry),
        ("Doc_0/Document.xml", &document),
        ("Doc_0/PublicRes.xml", &res),
        ("Doc_0/Pages/Page_0/Content.xml", &page1),
        ("Doc_0/Pages/Page_1/Content.xml", &page2),
        ("Doc_0/Tpls/Tpl_0/Content.xml", &template),
    ])
}

fn values(q: &str, a: &impl AstAdapter) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| a.name(*n).unwrap_or_default()).collect(),
    }
}

#[test]
fn pages_and_lines_without_an_outline() {
    let a = quarb_text_ofd::parse(&sawyer(false)).unwrap();
    assert_eq!(values("/page @| count", &a), ["2"]);
    assert_eq!(
        values("/page[1]/line::", &a),
        [
            "Tom Sawyer",
            "The Adventures of Tom Sawyer",
            "CHAPTER I",
            "“TOM!” No answer.",
            "The old lady pulled her spectacles down.",
        ]
    );
    // The template's running head is on page 1 (the page draws
    // it); page 2 has no template.
    assert_eq!(
        values("/page[2]/line::", &a),
        ["CHAPTER II", "Saturday morning was come.", "Tom appeared."]
    );
    assert_eq!(values("/page[2]:::page", &a), ["2"]);
    assert_eq!(values("/page[2]:::number", &a), ["2"]);
    // A page extent is a length in the page's millimetres.
    assert_eq!(values("/page[1]:::width", &a), ["210_mm"]);
    assert_eq!(values("^:::n-pages", &a), ["2"]);
}

#[test]
fn geometry_is_the_bounding_box_in_page_units() {
    let a = quarb_text_ofd::parse(&sawyer(false)).unwrap();
    // The joined line spans both codes' boundaries.
    assert_eq!(
        values(
            r#"//line[:: = "“TOM!” No answer."] | %(:::page; :::x; :::y; :::width; :::height; :::baseline)"#,
            &a
        ),
        ["%(page = 1; x = 30_mm; y = 30_mm; width = 42_mm; height = 5_mm; baseline = 34_mm)"]
    );
}

#[test]
fn the_outline_derives_the_sections() {
    let a = quarb_text_ofd::parse(&sawyer(true)).unwrap();
    assert_eq!(values("/section::lemma", &a), ["Chapter I", "Chapter II"]);
    assert_eq!(
        values("//section::lemma", &a),
        ["Chapter I", "Aunt Polly", "Chapter II"]
    );
    // Front matter stays at the root: the running head (y 5)
    // and the title (y 10) come before the first destination.
    assert_eq!(
        values("/line::", &a),
        ["Tom Sawyer", "The Adventures of Tom Sawyer"]
    );
    assert_eq!(
        values("/section[1]/line::", &a),
        ["CHAPTER I", "“TOM!” No answer."]
    );
    assert_eq!(
        values("/section[1]/section/line::", &a),
        ["The old lady pulled her spectacles down."]
    );
    // The bookmark destination is page-granular: everything on
    // page 2.
    assert_eq!(
        values("/section[2]//line::", &a),
        ["CHAPTER II", "Saturday morning was come.", "Tom appeared."]
    );
    assert_eq!(values("/section[1]/section:::level", &a), ["2"]);
    assert_eq!(values("/section[2]//line:::page", &a), ["2", "2", "2"]);
}

#[test]
fn docinfo_is_the_declared_identity() {
    let a = quarb_text_ofd::parse(&sawyer(false)).unwrap();
    assert_eq!(values("^::title", &a), ["The Adventures of Tom Sawyer"]);
    assert_eq!(values("^::author", &a), ["Mark Twain"]);
    assert_eq!(values("^::keywords", &a), ["boys; Mississippi"]);
    assert_eq!(values("^::created", &a), ["1876-06-01"]);
    assert_eq!(values("^::edition", &a), ["first"]);
    assert_eq!(values("^::doc-id", &a), ["1"]);
    let root = a.root();
    assert_eq!(root, NodeId(0));
    assert!(a.properties(root).iter().any(|(k, _)| k == "creator"));
}

#[test]
fn not_a_package_is_refused_by_name() {
    let err = quarb_text_ofd::parse(b"PK\x03\x04 not really")
        .unwrap_err()
        .to_string();
    assert!(err.starts_with("not an OFD package"), "{err}");
    let empty = zip(&[("hello.txt", "hi")]);
    let err = quarb_text_ofd::parse(&empty).unwrap_err().to_string();
    assert!(err.contains("no OFD.xml"), "{err}");
    assert!(!quarb_text_ofd::is_ofd_package(&empty));
    assert!(quarb_text_ofd::is_ofd_package(&sawyer(false)));
}
