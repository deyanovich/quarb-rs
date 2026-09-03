//! The koine reading end to end: a litogramma-style document
//! lowered to the text level answers the same queries as every
//! other text-level mount — sections, apparatus, prose, lists,
//! quotes.

use std::path::Path;

use quarb::QueryResult;
use quarb_text_koine::{KoineError, parse_str};

fn values(model: &quarb_text::TextModel, q: &str) -> Vec<String> {
    match quarb::run(q, model).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| model.locator(*n)).collect(),
    }
}

const DOC: &str = "\
@@@!koine

@#() The war
The emus @/advanced/@ on the wheat districts.@^(n1) See @>(second).

@##() First attempt
The Lewis gun jammed at Campion.

@\"
They can face machine guns.
\"@ An observer

@--
@-
alpha
-@
@-
beta
-@
--@
##@
#@(first)

@#() Second attempt
More prose here.
#@(second)

@^
Twenty thousand of them.
^@(n1)
";

fn mount() -> quarb_text::TextModel {
    parse_str(DOC, Path::new(".")).unwrap()
}

#[test]
fn enclosing_headings_rebuild_their_nesting() {
    let m = mount();
    assert_eq!(
        values(&m, "//section::lemma"),
        ["The war", "First attempt", "Second attempt"]
    );
    assert_eq!(values(&m, "/section/section::lemma"), ["First attempt"]);
    assert_eq!(values(&m, "/section[1]/section::::level"), ["2"]);
}

#[test]
fn prose_reads_clean() {
    let m = mount();
    // emphasis unwraps, the deixis leaves no marker, the ref
    // keeps its key as written
    assert_eq!(
        values(&m, r#"//section[::lemma = "The war"]/paragraph[1]::"#),
        ["The emus advanced on the wheat districts. See second."]
    );
}

#[test]
fn the_apparatus_is_shared() {
    let m = mount();
    assert_eq!(values(&m, "//footnote @| count"), ["2"]);
    assert_eq!(
        values(&m, "//*<deixis>->footnote::"),
        ["Twenty thousand of them."]
    );
    assert_eq!(values(&m, "//*<note>::onym"), ["n1"]);
    assert_eq!(
        values(&m, r"//*<note><-footnote\*::"),
        ["The emus advanced on the wheat districts. See second."]
    );
    assert_eq!(values(&m, "//*<dangling> @| count"), ["0"]);
}


#[test]
fn quotes_and_lists_map() {
    let m = mount();
    assert_eq!(values(&m, "//blockquote::hypograph"), ["An observer"]);
    assert_eq!(values(&m, "//unordered-item::"), ["alpha", "beta"]);
}

#[test]
fn a_dialektos_definition_refuses() {
    match parse_str("@@@!atrep\n", Path::new(".")) {
        Err(KoineError::NotADocument) => {}
        Err(other) => panic!("wrong error: {other}"),
        Ok(_) => panic!("expected a refusal"),
    }
}

/// Markdown through atrep's endomorphosis: at-markdown's flat
/// heading endos carry their level in the name, flat item runs
/// wrap into lists, fence info strings become ::::lang.
#[test]
fn markdown_imports_through_at_markdown() {
    let m = quarb_text_koine::parse_markdown(
        "# The war\n\nThe emus advanced.\n\n## First attempt\n\n\
         - alpha\n- beta\n\n```rust\nfn main() {}\n```\n",
    )
    .unwrap();
    assert_eq!(values(&m, "//section::lemma"), ["The war", "First attempt"]);
    assert_eq!(values(&m, "/section/section::lemma"), ["First attempt"]);
    assert_eq!(values(&m, "//unordered-item::"), ["alpha", "beta"]);
    assert_eq!(values(&m, "//verbatim::::lang"), ["rust"]);
    assert_eq!(values(&m, "//verbatim::"), ["fn main() {}"]);
}

/// HTML through atrep's endomorphosis: headings, paragraphs, and
/// real list containers arrive via at-html.
#[test]
fn html_imports_through_at_html() {
    let m = quarb_text_koine::parse_html(
        "<h1>The war</h1><p>The emus <em>advanced</em>.</p>\
         <ol><li>first</li><li>second</li></ol>",
    )
    .unwrap();
    assert_eq!(values(&m, "//section::lemma"), ["The war"]);
    assert_eq!(values(&m, "//section/paragraph::"), ["The emus advanced."]);
    assert_eq!(values(&m, "//ordered-item::"), ["first", "second"]);
}

/// reStructuredText through atrep's endomorphosis: rst declares
/// footnotes, and they land in the shared apparatus — callout,
/// body, edge, and the clean paragraph.
#[test]
fn rst_footnotes_join_the_apparatus() {
    let m = quarb_text_koine::parse_rst(
        "The war\n=======\n\nThe emus advanced. [#count]_\n\n\
         .. [#count] Twenty thousand of them.\n",
    )
    .unwrap();
    assert_eq!(values(&m, "//section::lemma"), ["The war"]);
    assert_eq!(
        values(&m, "//*<deixis>->footnote::"),
        ["Twenty thousand of them."]
    );
    assert_eq!(values(&m, "//section/paragraph[1]::"), ["The emus advanced."]);
    assert_eq!(values(&m, "//*<dangling> @| count"), ["0"]);
}

#[test]
fn refs_resolve_against_section_onyms() {
    let m = mount();
    // `@>(second)` mentions; `#@(second)` names its section —
    // the mention lands and projects as the target's lemma.
    assert_eq!(values(&m, "//ref::target"), ["second"]);
    assert_eq!(values(&m, "//ref--> ::lemma"), ["Second attempt"]);
    assert_eq!(values(&m, "//ref::"), ["Second attempt"]);
    assert_eq!(
        values(&m, r#"//section[::onym = "second"]<-- ::target"#),
        ["second"]
    );
}

/// XML identity is declared, not guessed: namespace first, then
/// DOCTYPE public id, then an unambiguous root; bare <article>
/// (JATS or DocBook 4) refuses.
#[test]
fn xml_identity_from_declarations() {
    use quarb_text_koine::detect_xml_kind as d;
    assert_eq!(d(r#"<TEI xmlns="http://www.tei-c.org/ns/1.0"/>"#), Some("tei"));
    assert_eq!(d(r#"<book xmlns="http://docbook.org/ns/docbook"/>"#), Some("docbook"));
    assert_eq!(
        d(r#"<osis xmlns="http://www.bibletechnologies.net/2003/OSIS/namespace"/>"#),
        Some("osis")
    );
    assert_eq!(
        d(r#"<!DOCTYPE article PUBLIC "-//NLM//DTD JATS (Z39.96) v1.2//EN" "x.dtd"><article/>"#),
        Some("jats")
    );
    assert_eq!(
        d(r#"<!DOCTYPE book PUBLIC "-//OASIS//DTD DocBook XML V4.5//EN" "x.dtd"><book/>"#),
        Some("docbook")
    );
    assert_eq!(d(r#"<usx version="3.0"/>"#), Some("usx"));
    assert_eq!(d("<TEI/>"), Some("tei"));
    // undeclared <article>: honestly ambiguous
    assert_eq!(d("<article><front/></article>"), None);
}

/// TEI end to end by its declared namespace: divs with heads
/// become the outline through at-tei's vocabulary.
#[test]
fn tei_imports_by_namespace() {
    let tei = r#"<TEI xmlns="http://www.tei-c.org/ns/1.0">
<text><body>
<div type="chapter"><head>The war</head>
<p>The emus advanced on the wheat districts.</p>
<div type="section"><head>First attempt</head>
<p>The Lewis gun jammed.</p></div></div>
</body></text></TEI>"#;
    let kind = quarb_text_koine::detect_xml_kind(tei).unwrap();
    assert_eq!(kind, "tei");
    let m = quarb_text_koine::parse_xml_as(tei, kind).unwrap();
    assert_eq!(values(&m, "//section::lemma"), ["The war", "First attempt"]);
    assert_eq!(values(&m, "/section/section::lemma"), ["First attempt"]);
    assert_eq!(
        values(&m, r#"//section[::lemma = "First attempt"]/paragraph::"#),
        ["The Lewis gun jammed."]
    );
}

/// Ruling #37 through the koine route: core stichoi become
/// verse/strophe/stichos with the line taxis.
#[test]
fn stichoi_lower_to_the_verse_vocabulary() {
    let m = parse_str(
        "@@@!koine\n\n@@@=\nHappy the man, whose wish && care\nA few paternal acres bound\n=@@@\n",
        Path::new("."),
    )
    .unwrap();
    assert_eq!(values(&m, "//verse @| count"), ["1"]);
    assert_eq!(
        values(&m, "//stichos[::taxis = 2]::"),
        ["A few paternal acres bound"]
    );
}

#[test]
fn bibtex_mounts_as_bib_entries() {
    // atrep's importer parses BibTeX/BibLaTeX into bibliogramma;
    // the entries land as bib blocks with the campi and genera
    // canonicalized to the Latin vocabulary — BibLaTeX's
    // journaltitle answers as ::ephemeris, @book as <liber>.
    let m = quarb_text_koine::parse_bibtex(
        "@book{knuth84,\n  author = {Donald E. Knuth},\n  title = {The {TeX}book},\n  year = 1984,\n}\n\
         @article{lamport94,\n  author = \"Leslie Lamport\",\n  title = {How to Write a Long Formula},\n  journaltitle = {Formal Aspects},\n  year = {1994},\n}\n",
    )
    .unwrap();
    assert_eq!(values(&m, "//bib::onym"), ["knuth84", "lamport94"]);
    assert_eq!(values(&m, "//bib::auctor"), ["Donald E. Knuth", "Leslie Lamport"]);
    assert_eq!(values(&m, "//bib<liber>::onym"), ["knuth84"]);
    assert_eq!(values(&m, "//bib[::ephemeris]::onym"), ["lamport94"]);
    assert_eq!(
        values(&m, "//bib::::genus"),
        ["liber", "commentarius"]
    );
    // The plain form: the full data, values in source order.
    assert_eq!(
        values(&m, r#"//bib[::onym = "knuth84"]::"#),
        ["Donald E. Knuth. The {TeX}book. 1984"]
    );
    // The alias census removes the friction: BibLaTeX's own
    // field names — and any covered language, case-insensitively
    // — answer beside the Latin canon. On a bib node the fields
    // outrank the general vocabulary, so ::title is titulus.
    assert_eq!(values(&m, "//bib::author"), ["Donald E. Knuth", "Leslie Lamport"]);
    assert_eq!(
        values(&m, "//bib::title"),
        ["The {TeX}book", "How to Write a Long Formula"]
    );
    assert_eq!(values(&m, "//bib[::journaltitle]::onym"), ["lamport94"]);
    assert_eq!(values(&m, "//bib[::journal]::onym"), ["lamport94"]);
    assert_eq!(values(&m, "//bib::год"), ["1984", "1994"]);
    assert_eq!(
        values(&m, "//bib::Titel"),
        ["The {TeX}book", "How to Write a Long Formula"]
    );
}

#[test]
fn endo_links_become_refs() {
    // The org route: [[target][desc]] arrives as the `><` link
    // endo whose grammata is the target — the mention lands as a
    // ref, external by URL, internal by fragment.
    let m = quarb_text_koine::parse_org(
        "* Guide\n\nSee [[https://quarb.org/spec][the spec]] and [[#usage]].\n",
    )
    .unwrap();
    assert_eq!(
        values(&m, "//ref | %(t = ::target; i = ::::resolved)"),
        [
            "%(t = \"https://quarb.org/spec\"; i = null)",
            "%(t = \"#usage\"; i = false)"
        ]
    );
    // The importers' own normalization keeps the target visible.
    assert_eq!(
        values(&m, "//paragraph::"),
        ["See the spec (https://quarb.org/spec) and #usage."]
    );
}

#[test]
fn markdown_route_links_and_footnotes() {
    // atrep 0.3.1: the visible-URL link sim and the markdown
    // footnote extension both reach the koine route — links as
    // ref nodes (the autolink IS the sim; [text](url) projects
    // as "text (url)"), footnotes as the shared apparatus.
    let m = quarb_text_koine::parse_markdown(
        "See [the spec](https://quarb.org/spec) and <https://quarb.org>.\n\n\
         A note[^1].\n\n[^1]: The footnote body.\n",
    )
    .unwrap();
    assert_eq!(
        values(&m, "//ref::target"),
        ["https://quarb.org/spec", "https://quarb.org"]
    );
    assert_eq!(
        values(&m, "//paragraph[1]::"),
        ["See the spec (https://quarb.org/spec) and https://quarb.org."]
    );
    assert_eq!(values(&m, "//*<deixis>->footnote::"), ["The footnote body."]);
}
