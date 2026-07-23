//! End-to-end tests: queries run through the engine against parsed
//! XML documents.

use quarb_xml::XmlAdapter;

const DOC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<library xmlns:dc="http://purl.org/dc/elements/1.1/">
  <book id="b1" pages="192">
    <dc:title>Ins &amp; Outs</dc:title>
    <author>Ada</author>
    <author>Bo</author>
  </book>
  <book id="b2" pages="352">
    <dc:title><![CDATA[Tags & <literals>]]></dc:title>
    <author>Cy</author>
  </book>
  <loan book="b1" reader="Ada"/>
</library>"#;

fn nodes(query: &str) -> Vec<String> {
    let adapter = XmlAdapter::parse(DOC).unwrap();
    let mut got: Vec<String> = match quarb::run(query, &adapter).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns.into_iter().map(|n| adapter.locator(n)).collect(),
        quarb::QueryResult::Values(_) => panic!("expected nodes"),
    };
    got.sort();
    got
}

fn values(query: &str) -> Vec<String> {
    let adapter = XmlAdapter::parse(DOC).unwrap();
    match quarb::run(query, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    }
}

#[test]
fn navigate_by_tag() {
    assert_eq!(
        nodes("/library/book"),
        vec!["/library/book[1]", "/library/book[2]"]
    );
    assert_eq!(
        nodes("//author"),
        vec![
            "/library/book[1]/author[1]",
            "/library/book[1]/author[2]",
            "/library/book[2]/author"
        ]
    );
    // a self-closing element is a leaf node
    assert_eq!(nodes("//loan"), vec!["/library/loan"]);
    assert_eq!(values("//loan:::is-leaf"), vec!["true"]);
}

#[test]
fn text_and_entities() {
    // &amp; unescaped in text content
    assert_eq!(
        values(r#"//book[::id = "b1"]/'dc:title'::"#),
        vec!["Ins & Outs"]
    );
    // CDATA is taken verbatim, markup un-mangled
    assert_eq!(
        values(r#"//book[::id = "b2"]/'dc:title'::"#),
        vec!["Tags & <literals>"]
    );
    // the bare default projection is the text content
    assert_eq!(values("//author::"), vec!["Ada", "Bo", "Cy"]);
}

#[test]
fn attributes_as_properties() {
    assert_eq!(values("//book::pages"), vec!["192", "352"]);
    assert_eq!(values("//loan::reader"), vec!["Ada"]);
    // via metadata too
    assert_eq!(values("//loan;;;tag"), vec!["loan"]);
    assert_eq!(values("//loan;;;n-attrs"), vec!["2"]);
    assert_eq!(values("//book::id"), vec!["b1", "b2"]);
}

#[test]
fn namespaced_names() {
    // a prefixed tag navigates as a quoted name segment
    assert_eq!(
        nodes("//'dc:title'"),
        vec!["/library/book[1]/dc:title", "/library/book[2]/dc:title"]
    );
    assert_eq!(values("/library/book/'dc:title';;;tag").len(), 2);
    // prefix-agnostic filtering via ;;;local-name
    assert_eq!(
        nodes(r#"//*[;;;local-name = "title"]"#),
        vec!["/library/book[1]/dc:title", "/library/book[2]/dc:title"]
    );
    assert_eq!(values("//'dc:title';;;ns-prefix"), vec!["dc", "dc"]);
    assert_eq!(values("//'dc:title';;;local-name"), vec!["title", "title"]);
    // an unprefixed tag has no ns-prefix: the projection is null,
    // which renders empty
    assert_eq!(values("//author;;;ns-prefix"), vec!["", "", ""]);
}

#[test]
fn predicates_and_pipelines() {
    // numeric coercion of a string attribute in a predicate
    assert_eq!(values("//book[::pages > 200]::id"), vec!["b2"]);
    assert_eq!(
        values("//author:: @| join(\", \")"),
        vec!["Ada, Bo, Cy"]
    );
    assert_eq!(values("//book @| count"), vec!["2"]);
}

#[test]
fn resolve_idref() {
    // the loan's book="b1" IDREF resolves to <book id="b1">
    assert_eq!(nodes("//loan::book~>"), vec!["/library/book[1]"]);
    // follow the reference and read the target's attribute
    assert_eq!(values("//loan::book~>::pages"), vec!["192"]);
    // reverse resolution: which nodes' book resolves to book[1]?
    assert_eq!(values("//book::book<~::reader"), vec!["Ada"]);
}

#[test]
fn numeric_aggregation_over_string_attributes() {
    assert_eq!(values("//book::pages @| sum"), vec!["544"]);
    assert_eq!(values("//book::pages @| avg"), vec!["272"]);
    assert_eq!(values("//book::pages @| max"), vec!["352"]);
}

#[test]
fn malformed_xml_is_an_error() {
    // mismatched end tag
    assert!(XmlAdapter::parse("<a><b></a>").is_err());
    // unclosed element at end of input
    assert!(XmlAdapter::parse("<a><b>").is_err());
    // a second root element
    assert!(XmlAdapter::parse("<a/><b/>").is_err());
    // no root element at all
    assert!(XmlAdapter::parse("").is_err());
    assert!(XmlAdapter::parse("   \n").is_err());
    // duplicate attribute
    assert!(XmlAdapter::parse(r#"<a x="1" x="2"/>"#).is_err());
    // unknown entity reference
    assert!(XmlAdapter::parse("<a>&nope;</a>").is_err());
}
