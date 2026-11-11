//! End-to-end tests: XPath expressions translated by the importer
//! and executed through the engine against a parsed XML document.

use quarb_xml::XmlAdapter;

const DOC: &str = r#"<?xml version="1.0"?>
<EXAMPLE prop1="gnome is great">
  <head>
   <title>Welcome to Gnome</title>
  </head>
  <chapter id="chapter1">
   <title>The Linux adventure</title>
   <p>bla bla bla ...</p>
   <image href="linus.gif"/>
   <p>...</p>
  </chapter>
  <chapter id="chapter2">
   <title>Chapter 2</title>
   <p>this is chapter 2 ...</p>
  </chapter>
  <chapter id="chapter3">
   <title>Chapter 3</title>
   <p>this is chapter 3 ...</p>
  </chapter>
</EXAMPLE>"#;

/// An XPath answers a node set; a Quarb navigation one thread per
/// arrival (spec: Expansion paths), so the round trip collapses
/// them where the query ends on nodes.
fn via_xpath(xpath: &str) -> Vec<String> {
    let mut query = quarb_xpath::translate(xpath).unwrap().query;
    let value_form = query.contains('|') || query.contains("::") || query.ends_with(')');
    if !value_form {
        query.push_str(" @| collapse");
    }
    let adapter = XmlAdapter::parse(DOC).unwrap();
    match quarb::run(&query, &adapter).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns.into_iter().map(|n| adapter.locator(n)).collect(),
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
    }
}

#[test]
fn navigation() {
    assert_eq!(
        via_xpath("/child::EXAMPLE/child::head"),
        vec!["/EXAMPLE/head"]
    );
    assert_eq!(
        via_xpath("/descendant::title")[..2],
        ["/EXAMPLE/head/title", "/EXAMPLE/chapter[1]/title"]
    );
    assert_eq!(
        via_xpath("/descendant::p/ancestor::chapter"),
        vec![
            "/EXAMPLE/chapter[1]",
            "/EXAMPLE/chapter[2]",
            "/EXAMPLE/chapter[3]"
        ]
    );
    assert_eq!(
        via_xpath("//image/parent::chapter"),
        vec!["/EXAMPLE/chapter[1]"]
    );
    assert_eq!(via_xpath("//image/.."), vec!["/EXAMPLE/chapter[1]"]);
}

#[test]
fn predicates() {
    assert_eq!(
        via_xpath("//chapter[@id='chapter2']"),
        vec!["/EXAMPLE/chapter[2]"]
    );
    assert_eq!(
        via_xpath("//chapter[title='Chapter 3']"),
        vec!["/EXAMPLE/chapter[3]"]
    );
    assert_eq!(via_xpath("//chapter[image]"), vec!["/EXAMPLE/chapter[1]"]);
    assert_eq!(
        via_xpath("//chapter[not(image)]"),
        vec!["/EXAMPLE/chapter[2]", "/EXAMPLE/chapter[3]"]
    );
    assert_eq!(
        via_xpath("//chapter[image/@href='linus.gif']"),
        vec!["/EXAMPLE/chapter[1]"]
    );
    assert_eq!(
        via_xpath("//chapter[contains(@id, '3')]"),
        vec!["/EXAMPLE/chapter[3]"]
    );
    assert_eq!(via_xpath("//chapter[starts-with(@id, 'chap')]").len(), 3);
}

#[test]
fn projections_and_aggregates() {
    assert_eq!(
        via_xpath("//chapter/@id"),
        vec!["chapter1", "chapter2", "chapter3"]
    );
    assert_eq!(via_xpath("//head/title/text()"), vec!["Welcome to Gnome"]);
    assert_eq!(via_xpath("count(//p)"), vec!["4"]);
    assert_eq!(via_xpath("//head | //image").len(), 2);
}

#[test]
fn positional_predicates() {
    assert_eq!(via_xpath("//chapter[last()]"), vec!["/EXAMPLE/chapter[3]"]);
    assert_eq!(via_xpath("//chapter[last()]/@id"), vec!["chapter3"]);
    assert_eq!(
        via_xpath("//chapter[last()-1]"),
        vec!["/EXAMPLE/chapter[2]"]
    );
    // the last p in the whole document (per-source descendant list)
    assert_eq!(via_xpath("//p[last()]"), vec!["/EXAMPLE/chapter[3]/p"]);
    assert_eq!(
        via_xpath("//chapter[position() > 1]"),
        vec!["/EXAMPLE/chapter[2]", "/EXAMPLE/chapter[3]"]
    );
    assert_eq!(
        via_xpath("//chapter[position() <= 2]"),
        vec!["/EXAMPLE/chapter[1]", "/EXAMPLE/chapter[2]"]
    );
}

/// `[n]` positions among the hop's per-source results. On a child
/// hop this matches XPath exactly; on an abbreviated `//` hop it
/// indexes the whole per-source descendant list (XPath positions
/// within each parent), which the importer flags in its notes.
#[test]
fn index_predicates() {
    // child axis: exact, no note
    let tr = quarb_xpath::translate("/EXAMPLE/chapter[2]").unwrap();
    assert!(tr.notes.is_empty());
    assert_eq!(
        via_xpath("/EXAMPLE/chapter[2]"),
        vec!["/EXAMPLE/chapter[2]"]
    );
    assert_eq!(
        via_xpath("/EXAMPLE/chapter[1]/p[2]"),
        vec!["/EXAMPLE/chapter[1]/p[2]"]
    );
    // abbreviated //: all chapters share one parent, so [2] agrees
    // with XPath here — but the importer still notes the general
    // divergence
    let tr = quarb_xpath::translate("//chapter[2]").unwrap();
    assert!(tr.notes.iter().any(|n| n.contains("within each parent")));
    assert_eq!(via_xpath("//chapter[2]"), vec!["/EXAMPLE/chapter[2]"]);
    // ... and //p[1] is the first p in the whole document (XPath's
    // //p[1] would be one p per chapter)
    assert_eq!(via_xpath("//p[1]"), vec!["/EXAMPLE/chapter[1]/p[1]"]);
}

/// `text()` is the element's immediate text nodes: `:::texts`, the
/// list of its text runs (ruling 2026-11-11). A terminal `text()`
/// spreads the lists into one answer per run; a compared `text()`
/// holds when any run satisfies the comparison.
#[test]
fn text_is_the_immediate_text_runs() {
    let tr = |x: &str| quarb_xpath::translate(x).unwrap();
    assert_eq!(tr("//p/text()").query, "//p:::texts | ...");
    assert_eq!(
        tr("//title/text() | //chapter/@id").query,
        "//title:::texts || //chapter::id | ..."
    );
    assert_eq!(tr("count(//p/text())").query, "//p:::texts | ... @| count");
    assert_eq!(
        tr("//chapter[title/text() = 'Chapter 2']/@id").query,
        "//chapter[/title:::texts = \"Chapter 2\"]::id"
    );
    assert!(
        tr("//p/text()")
            .notes
            .iter()
            .any(|n| n.contains("':::texts' lists the element's immediate text runs"))
    );
    // a function reads the first text node in XPath: noted
    let t = tr("//p[contains(text(), 'chapter')]");
    assert_eq!(t.query, "//p[:::texts *= \"chapter\"]");
    assert!(
        t.notes
            .iter()
            .any(|n| n.contains("XPath reads the first text node"))
    );

    assert_eq!(via_xpath("//head/title/text()"), ["Welcome to Gnome"]);
    assert_eq!(
        via_xpath("//chapter[title/text() = 'Chapter 2']/@id"),
        ["chapter2"]
    );
    assert_eq!(via_xpath("count(//title/text())"), ["4"]);

    // mixed content: the runs, not the descendant text
    let mixed = XmlAdapter::parse(
        r#"<doc><p id="a">Hello <b>world</b> again</p><p id="b"><b>only</b></p></doc>"#,
    )
    .unwrap();
    let run = |x: &str| match quarb::run(&tr(x).query, &mixed).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    };
    assert_eq!(run("//p/text()"), ["Hello", "again"]);
    assert_eq!(run("//p[text() = 'again']/@id"), ["a"]);
    assert_eq!(
        run("//p[contains(text(), 'world')]/@id"),
        Vec::<String>::new()
    );
    assert_eq!(run("count(//p/text())"), ["2"]);
}
