//! An HWPX package at the text level: headings from the declared
//! outline levels, lists from the declared numbering, tables, notes,
//! hyperlinks, the accepted view of tracked changes, the package
//! file's identity. The packages are written in-test after the
//! shape Hancom's office writes.
use std::io::Write;

use quarb::{AstAdapter, QueryResult, Value};

const NS: &str = r#"xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph" xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section" xmlns:hh="http://www.hancom.co.kr/hwpml/2011/head" xmlns:hc="http://www.hancom.co.kr/hwpml/2011/core""#;

fn zip(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, body) in entries {
        w.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        w.write_all(body.as_bytes()).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn p(pr: u32, runs: &str) -> String {
    format!(
        r#"<hp:p id="0" paraPrIDRef="{pr}" styleIDRef="0" pageBreak="0" columnBreak="0" merged="0">{runs}<hp:linesegarray><hp:lineseg textpos="0"/></hp:linesegarray></hp:p>"#
    )
}

fn t(text: &str) -> String {
    format!(r#"<hp:run charPrIDRef="0"><hp:t>{text}</hp:t></hp:run>"#)
}

/// Paragraph properties: 0 plain, 1 outline level 0, 2 outline level
/// 1, 3 bullet (bullet 1, level 0), 4 bullet level 1, 5 number
/// (numbering 2, start 1) level 0.
fn header() -> String {
    let pr = |id: u32, kind: &str, level: u32, idref: u32| {
        format!(
            r#"<hh:paraPr id="{id}" tabPrIDRef="0" condense="0" fontLineHeight="0" snapToGrid="1" suppressLineNumbers="0" checked="0"><hh:align horizontal="JUSTIFY" vertical="BASELINE"/><hh:heading type="{kind}" idRef="{idref}" level="{level}"/></hh:paraPr>"#
        )
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes" ?><hh:head {NS} version="1.4" secCnt="1"><hh:refList><hh:numberings itemCnt="1"><hh:numbering id="2" start="1"><hh:paraHead start="1" level="1" numFormat="DIGIT"/></hh:numbering></hh:numberings><hh:bullets itemCnt="1"><hh:bullet id="1" char="" useImage="0"/></hh:bullets><hh:paraProperties itemCnt="6">{}{}{}{}{}{}</hh:paraProperties><hh:styles itemCnt="1"><hh:style id="0" type="PARA" name="바탕글" engName="Normal" paraPrIDRef="0" charPrIDRef="0"/></hh:styles></hh:refList></hh:head>"#,
        pr(0, "NONE", 0, 0),
        pr(1, "OUTLINE", 0, 0),
        pr(2, "OUTLINE", 1, 0),
        pr(3, "BULLET", 0, 1),
        pr(4, "BULLET", 1, 1),
        pr(5, "NUMBER", 0, 2),
    )
}

fn package(section: &str) -> Vec<u8> {
    let container = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes" ?><ocf:container xmlns:ocf="urn:oasis:names:tc:opendocument:xmlns:container"><ocf:rootfiles><ocf:rootfile full-path="Contents/content.hpf" media-type="application/hwpml-package+xml"/></ocf:rootfiles></ocf:container>"#;
    let hpf = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes" ?><opf:package xmlns:opf="http://www.idpf.org/2007/opf/" version="" unique-identifier="" id=""><opf:metadata><opf:title>Emma</opf:title><opf:language>en</opf:language><opf:meta name="creator" content="text">Jane Austen</opf:meta><opf:meta name="subject" content="text">A novel</opf:meta><opf:meta name="keyword" content="text">Highbury; manners</opf:meta><opf:meta name="CreatedDate" content="text">1815-12-23T00:00:00Z</opf:meta></opf:metadata><opf:manifest><opf:item id="header" href="Contents/header.xml" media-type="application/xml"/><opf:item id="section0" href="Contents/section0.xml" media-type="application/xml"/></opf:manifest><opf:spine><opf:itemref idref="header"/><opf:itemref idref="section0"/></opf:spine></opf:package>"#;
    let sec = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes" ?><hs:sec {NS}>{section}</hs:sec>"#
    );
    let header = header();
    zip(&[
        ("mimetype", "application/hwp+zip"),
        ("META-INF/container.xml", container),
        ("Contents/content.hpf", hpf),
        ("Contents/header.xml", &header),
        ("Contents/section0.xml", &sec),
    ])
}

fn values(q: &str, a: &impl AstAdapter) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| a.name(*n).unwrap_or_default()).collect(),
    }
}

fn emma() -> Vec<u8> {
    let section = [
        p(1, &t("Volume I")),
        p(2, &t("Chapter I")),
        p(
            0,
            &format!(
                "{}{}",
                t("Emma Woodhouse, handsome, clever, and rich,"),
                t(" had lived nearly twenty-one years.")
            ),
        ),
        p(3, &t("Her father")),
        p(4, &t("affectionate")),
        p(4, &t("indulgent")),
        p(3, &t("Her governess")),
        p(5, &t("First")),
        p(5, &t("Second")),
        p(0, &t("The real evils of Emma's situation.")),
        p(2, &t("Chapter II")),
        p(0, &t("Mr. Weston was a native of Highbury.")),
    ]
    .concat();
    package(&section)
}

#[test]
fn headings_from_the_declared_outline_levels() {
    let m = quarb_text_hwpx::parse(&emma()).unwrap();
    assert_eq!(values("/section::lemma", &m), ["Volume I"]);
    assert_eq!(
        values("/section/section::lemma", &m),
        ["Chapter I", "Chapter II"]
    );
    assert_eq!(
        values("/section/section[1]/paragraph::", &m),
        [
            "Emma Woodhouse, handsome, clever, and rich, had lived nearly twenty-one years.",
            "The real evils of Emma's situation.",
        ]
    );
}

#[test]
fn lists_from_the_declared_numbering_nest_by_level() {
    let m = quarb_text_hwpx::parse(&emma()).unwrap();
    assert_eq!(values("//unordered-list @| count", &m), ["2"]);
    assert_eq!(values("//ordered-list @| count", &m), ["1"]);
    assert_eq!(
        values("/section/section[1]/unordered-list/unordered-item::", &m),
        ["Her father\naffectionate\nindulgent", "Her governess"]
    );
    assert_eq!(
        values(
            "/section/section[1]/unordered-list/unordered-item[1]/unordered-list/unordered-item::",
            &m
        ),
        ["affectionate", "indulgent"]
    );
    assert_eq!(
        values("/section/section[1]/ordered-list/ordered-item::", &m),
        ["First", "Second"]
    );
}

#[test]
fn tables_notes_links_and_the_accepted_view() {
    let table = r#"<hp:run charPrIDRef="0"><hp:tbl id="1" zOrder="0" numberingType="TABLE" rowCnt="3" colCnt="2"><hp:tr><hp:tc name="" header="1" hasMargin="0" protect="0" editable="0" dirty="0" borderFillIDRef="4"><hp:subList id="">{H1}</hp:subList><hp:cellAddr colAddr="0" rowAddr="0"/><hp:cellSpan colSpan="1" rowSpan="1"/></hp:tc><hp:tc name="" header="1" hasMargin="0" protect="0" editable="0" dirty="0" borderFillIDRef="4"><hp:subList id="">{H2}</hp:subList><hp:cellAddr colAddr="1" rowAddr="0"/><hp:cellSpan colSpan="1" rowSpan="1"/></hp:tc></hp:tr><hp:tr><hp:tc name="" header="0" hasMargin="0" protect="0" editable="0" dirty="0" borderFillIDRef="4"><hp:subList id="">{C1}</hp:subList><hp:cellAddr colAddr="0" rowAddr="1"/><hp:cellSpan colSpan="1" rowSpan="1"/></hp:tc><hp:tc name="" header="0" hasMargin="0" protect="0" editable="0" dirty="0" borderFillIDRef="4"><hp:subList id="">{C2}</hp:subList><hp:cellAddr colAddr="1" rowAddr="1"/><hp:cellSpan colSpan="1" rowSpan="1"/></hp:tc></hp:tr></hp:tbl></hp:run>"#
        .replace("{H1}", &p(0, &t("title"))).replace("{H2}", &p(0, &t("price")))
        .replace("{C1}", &p(0, &t("Emma"))).replace("{C2}", &p(0, &t("25")));
    let note = format!(
        r#"<hp:run charPrIDRef="0"><hp:t>Highbury</hp:t><hp:footNote number="1">{}</hp:footNote><hp:t> is a village.</hp:t></hp:run>"#,
        p(0, &t("A large and populous village."))
    );
    let link = r#"<hp:run charPrIDRef="0"><hp:ctrl><hp:fieldBegin id="7" type="HYPERLINK" name="" editable="1" dirty="0" zorder="-1" fieldid="1"><hp:parameters cnt="1" name=""><hp:stringParam name="Command">https://www.gutenberg.org/ebooks/158;1;0;0</hp:stringParam></hp:parameters></hp:fieldBegin></hp:ctrl></hp:run><hp:run charPrIDRef="1"><hp:t>Read it</hp:t></hp:run><hp:run charPrIDRef="0"><hp:ctrl><hp:fieldEnd beginIDRef="7" fieldid="1"/></hp:ctrl><hp:t> online.</hp:t></hp:run>"#;
    let tracked = r#"<hp:run charPrIDRef="0"><hp:t>Mr. </hp:t><hp:deleteBegin Id="2" TcId="2"/><hp:t>Woodhouse</hp:t><hp:deleteEnd Id="2" TcId="2" paraend="0"/><hp:insertBegin Id="1" TcId="1"/><hp:t>Knightley</hp:t><hp:insertEnd Id="1" TcId="1" paraend="0"/><hp:t> called.</hp:t></hp:run>"#;
    let header_footer = format!(
        r#"<hp:run charPrIDRef="0"><hp:ctrl><hp:header id="1" applyPageType="BOTH"><hp:subList id="">{}</hp:subList></hp:header></hp:ctrl></hp:run>"#,
        p(0, &t("RUNNING HEAD"))
    );
    let section = [
        p(1, &t("Chapter I")),
        p(0, &format!("{}{}", header_footer, table)),
        p(0, &note),
        p(0, link),
        p(0, tracked),
    ]
    .concat();
    let m = quarb_text_hwpx::parse(&package(&section)).unwrap();
    assert_eq!(values("//*<table>//*<cell> | ::body", &m), ["Emma", "25"]);
    assert_eq!(
        values("//*<table>//*<cell> | ::lemma", &m),
        ["title", "price"]
    );
    // The body (`<note>`) at the document end; the callout
    // (`<deixis>`) under the paragraph answers the same text.
    assert_eq!(
        values("//footnote<note>::", &m),
        ["A large and populous village."]
    );
    assert_eq!(
        values("/section/paragraph[:: *= \"village\"]::", &m),
        ["Highbury is a village."]
    );
    assert_eq!(
        values(
            "/section/paragraph[:: *= \"village\"]/*<deixis> @| count",
            &m
        ),
        ["1"]
    );
    assert_eq!(
        values("//ref | %(::target; t = ::)", &m),
        ["%(target = \"https://www.gutenberg.org/ebooks/158\"; t = \"Read it\")"]
    );
    assert_eq!(
        values("//paragraph[:: *= \"called\"]::", &m),
        ["Mr. Knightley called."]
    );
    assert_eq!(values("//paragraph[:: *= \"RUNNING\"] @| count", &m), ["0"]);
}

#[test]
fn the_package_file_is_the_identity() {
    let h = quarb_text_hwpx::head(&emma()).unwrap();
    assert_eq!(h.title.as_deref(), Some("Emma"));
    assert_eq!(h.author.as_deref(), Some("Jane Austen"));
    assert_eq!(h.description.as_deref(), Some("A novel"));
    assert_eq!(h.tags, ["Highbury", "manners"]);
    assert!(h.published.is_some());
    assert!(quarb_text_hwpx::is_hwpx(&emma()));
    assert!(!quarb_text_hwpx::is_hwpx(b"PK\x03\x04nope"));
    let err = match quarb_text_hwpx::parse(&zip(&[("x.txt", "hi")])) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a zip with no package file parsed"),
    };
    assert!(err.contains("OWPML package file"), "{err}");
}
