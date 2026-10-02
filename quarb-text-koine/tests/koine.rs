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
    assert_eq!(
        values(&m, "//section/paragraph[1]::"),
        ["The emus advanced."]
    );
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
    assert_eq!(
        d(r#"<TEI xmlns="http://www.tei-c.org/ns/1.0"/>"#),
        Some("tei")
    );
    assert_eq!(
        d(r#"<book xmlns="http://docbook.org/ns/docbook"/>"#),
        Some("docbook")
    );
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

/// A TEI speech on the text level: a `<sp><speaker>` block keeps
/// its printed speech prefix as the paragraph's lemma; a
/// `<said who>` paragraph is a paragraph of the words alone — its
/// speaker is unseen metadata (at-aphanes), which the text level
/// leaves out and the literary reading keeps.
#[test]
fn tei_speech_on_the_text_level() {
    let tei = r##"<TEI xmlns="http://www.tei-c.org/ns/1.0">
<text><body>
<div type="chapter"><head>CHAPTER I</head>
<p><said who="#polly">“Tom!”</said></p>
<p>No answer.</p>
<sp who="#tom"><speaker>Tom</speaker><p>Nothing, aunt.</p></sp>
</div>
</body></text></TEI>"##;
    let m = quarb_text_koine::parse_xml_as(tei, "tei").unwrap();
    assert_eq!(values(&m, "//paragraph @| count"), ["3"]);
    assert_eq!(
        values(&m, "//paragraph::"),
        ["“Tom!”", "No answer.", "Nothing, aunt."]
    );
    assert_eq!(values(&m, "//paragraph[::lemma]::lemma"), ["Tom"]);
    // The unseen key never leaks into the prose.
    assert_eq!(values(&m, r#"//paragraph[:: *= "polly"] @| count"#), ["0"]);
}

/// The literary reading (`lit:`): inline simmeres are nodes named
/// A block stage direction has no reading of its own, so its
/// paragraphs wear the sim as a trait: the inline form is a
/// `stage-direction` span, the block form a
/// `paragraph<stage-direction>` — the trait tells it from a
/// line's own paragraphs, in both sources.
#[test]
fn lit_block_stage_directions_wear_their_sim() {
    let atd = "\
@@@!litogramma

@#() Ревизор

@:# Явление I

@:[
Комната в доме городничего.
]:@

@: Городничий
Я пригласил вас, господа. @:( Садится. ):@

@:[
Все садятся.
]:@

Пренеприятное известие.
:@
#:@
#@
";
    let m = quarb_text_koine::parse_lit_str(atd, std::path::Path::new(".")).unwrap();
    assert_eq!(values(&m, "//paragraph<stage-direction> @| count"), ["2"]);
    assert_eq!(values(&m, "//stage-direction @| count"), ["1"]);
    assert_eq!(values(&m, "//dialogue @| count"), ["1"]);
    assert_eq!(values(&m, "//paragraph @| count"), ["3"]);
    // The flattening reading is untouched: no traits, no dialogue.
    let m = quarb_text_koine::parse_str(atd, std::path::Path::new(".")).unwrap();
    assert_eq!(values(&m, "//paragraph<stage-direction> @| count"), ["0"]);

    let tei = r##"<TEI xmlns="http://www.tei-c.org/ns/1.0">
<text><body>
<div type="scene"><head>Явление I</head>
<stage>Комната в доме городничего.</stage>
<sp who="#городничий"><speaker>Городничий</speaker><p>Я пригласил вас, господа. <stage>Садится.</stage></p></sp>
<stage>Все садятся.</stage>
</div>
</body></text></TEI>"##;
    let m = quarb_text_koine::parse_lit_xml_as(tei, "tei").unwrap();
    assert_eq!(values(&m, "//paragraph<stage-direction> @| count"), ["2"]);
    assert_eq!(values(&m, "//stage-direction @| count"), ["1"]);
    assert_eq!(
        values(&m, r#"//dialogue[::lemma = "Городничий"] @| count"#),
        ["1"]
    );
}

/// The cast layer (ruling #79): a cast table declares characters
/// the source did not, annotates the word tokens its patterns
/// match with the character's id as their `prosopon`, and every
/// mention — a speech, a name span, a token — reaches its character
/// as `->character`, which lists its mentions back as `<-character`.
#[test]
fn lit_cast_layer() {
    let tei = r##"<TEI xmlns="http://www.tei-c.org/ns/1.0">
<text><body>
<div type="chapter"><head>CHAPTER I</head>
<p><said who="#polly">“Tom!”</said></p>
<p>No answer. Aunt <persName ref="#polly">Polly</persName> looked for Tom, and Tom's cousin Mary laughed.</p>
</div>
</body></text></TEI>"##;
    let mut m = quarb_text_koine::parse_lit_xml_as(tei, "tei").unwrap();
    m.tokenize_with(None);
    let row = |id: &str, name: &str, pat: &str| quarb_text::CastRow {
        id: id.to_string(),
        name: name.to_string(),
        pattern: Some(regex::Regex::new(pat).unwrap()),
        words: 1,
        fields: vec![
            ("name".to_string(), name.to_string()),
            ("role".to_string(), "x".to_string()),
        ],
    };
    m.apply_cast(&[
        row("tom", "Tom Sawyer", "^Tom"),
        row("polly", "Aunt Polly", "^Polly$"),
        row("mary", "Mary", "^Mary$"),
    ]);
    assert_eq!(values(&m, "//character @| count"), ["3"]);
    assert_eq!(
        values(&m, r#"//character[::onym = "tom"]::lemma"#),
        ["Tom Sawyer"]
    );
    assert_eq!(values(&m, r#"//character[::onym = "tom"]::role"#), ["x"]);
    // Tom, Tom's: two word tokens by the pattern; the token inside
    // the speech is also his by pattern (the speech's prosopon is
    // Polly's, and the token's own annotation wins).
    assert_eq!(values(&m, r#"//token[::prosopon = "tom"] @| count"#), ["3"]);
    // Polly: the name span by its ref, the token by the pattern.
    assert_eq!(
        values(&m, r#"//character[::onym = "polly"]<-character @| count"#),
        ["3"]
    );
    assert_eq!(
        values(&m, r#"//dialogue-line->character::lemma"#),
        ["Aunt Polly"]
    );
    assert_eq!(
        values(&m, r#"//token[:: = "Mary"]->character::onym"#),
        ["mary"]
    );
}

/// by their litogramma sim, genoses are traits, the aphanes
/// monosims are properties, and spans nest by containment.
#[test]
fn lit_keeps_the_inline_simmeres() {
    let tei = r##"<TEI xmlns="http://www.tei-c.org/ns/1.0">
<text><body>
<div type="chapter"><head>CHAPTER I</head>
<p><said who="#polly">“Tom!”</said></p>
<p>No answer. <said who="#polly">“What’s gone with that boy?”</said> she said.</p>
<p><said who="#tom">“Aunt <persName ref="#polly">Polly</persName>!”</said> he cried, in the <placeName ref="#garden">garden</placeName>.</p>
<p><foreign xml:lang="la">Sic transit</foreign> the <emph>glory</emph> of <date when="1876-06-01">June</date>.</p>
</div>
</body></text></TEI>"##;
    let m = quarb_text_koine::parse_lit_xml_as(tei, "tei").unwrap();
    // The whole-paragraph speech is a dialogue line, the inline
    // one a quotation wearing `<said>`; both know their speaker.
    assert_eq!(values(&m, "//dialogue-line::prosopon"), ["polly"]);
    assert_eq!(values(&m, "//dialogue-line::"), ["“Tom!”"]);
    assert_eq!(
        values(&m, "//quotation<said>::"),
        ["“What’s gone with that boy?”", "“Aunt Polly!”"]
    );
    assert_eq!(values(&m, "//quotation<said>::prosopon"), ["polly", "tom"]);
    // The prose of the block is the words alone.
    assert_eq!(
        values(&m, "//paragraph[1]::"),
        ["No answer. “What’s gone with that boy?” she said."]
    );
    // A name inside a speech is the speech's child; the name's
    // referent and the place's are their own fields.
    assert_eq!(values(&m, "//quotation/annotation<persname>::"), ["Polly"]);
    assert_eq!(values(&m, "//annotation<persname>::prosopon"), ["polly"]);
    assert_eq!(values(&m, "//annotation<placename>::chora"), ["garden"]);
    assert_eq!(values(&m, "//annotation<date>::chronos"), ["1876-06-01"]);
    // Foreign is emphasis with the language after the genos.
    assert_eq!(values(&m, "//emphasis<foreign>::::lang"), ["la"]);
    assert_eq!(values(&m, "//emphasis::"), ["Sic transit", "glory"]);
    // No key leaks into the prose.
    assert_eq!(values(&m, r#"//paragraph[:: *= "polly"] @| count"#), ["0"]);
}

/// The literary reading's tokens: a span reaches the tokens it
/// covers, a token answers the innermost span's fields.
#[test]
fn lit_spans_reach_their_tokens() {
    let tei = r##"<TEI xmlns="http://www.tei-c.org/ns/1.0">
<text><body>
<div type="chapter"><head>CHAPTER I</head>
<p><said who="#polly">“Tom!”</said></p>
<p>No answer. <said who="#polly">“What’s gone with that boy?”</said> she said.</p>
<p><said who="#tom">“Aunt <persName ref="#polly">Polly</persName>!”</said> he cried.</p>
</div>
</body></text></TEI>"##;
    let mut m = quarb_text_koine::parse_lit_xml_as(tei, "tei").unwrap();
    m.tokenize_with(None);
    assert_eq!(
        values(&m, r#"//dialogue-line[::prosopon = "polly"] @| count"#),
        ["1"]
    );
    assert_eq!(values(&m, "//quotation<said> @| count"), ["2"]);
    // The span reaches its tokens.
    assert_eq!(
        values(
            &m,
            r#"//quotation<said>[::prosopon = "tom"]->token<word> @| count"#
        ),
        ["2"]
    );
    assert_eq!(
        values(
            &m,
            r#"//quotation<said>[::prosopon = "polly"]->token<word> @| count"#
        ),
        ["5"]
    );
    // A token answers the innermost span covering it, else its
    // block: the name's referent on "Polly", the speaker on the
    // rest of Tom's cry, the dialogue line's speaker on "Tom!".
    assert_eq!(
        values(&m, r#"//token<word>[::prosopon = "polly"]:: @| join(" ")"#),
        ["Tom What’s gone with that boy Polly"]
    );
    assert_eq!(
        values(&m, r#"//token<word>[::prosopon = "tom"]::"#),
        ["Aunt"]
    );
    // The shape beneath a speech block is the corpus reading's.
    assert_eq!(values(&m, "//dialogue-line/sentence/token @| count"), ["4"]);
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
    assert_eq!(
        values(&m, "//bib::auctor"),
        ["Donald E. Knuth", "Leslie Lamport"]
    );
    assert_eq!(values(&m, "//bib<liber>::onym"), ["knuth84"]);
    assert_eq!(values(&m, "//bib[::ephemeris]::onym"), ["lamport94"]);
    assert_eq!(values(&m, "//bib::::genus"), ["liber", "commentarius"]);
    // The plain form: the full data, values in source order.
    assert_eq!(
        values(&m, r#"//bib[::onym = "knuth84"]::"#),
        ["Donald E. Knuth. The {TeX}book. 1984"]
    );
    // The alias census removes the friction: BibLaTeX's own
    // field names — and any covered language, case-insensitively
    // — answer beside the Latin canon. On a bib node the fields
    // outrank the general vocabulary, so ::title is titulus.
    assert_eq!(
        values(&m, "//bib::author"),
        ["Donald E. Knuth", "Leslie Lamport"]
    );
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
    assert_eq!(
        values(&m, "//*<deixis>->footnote::"),
        ["The footnote body."]
    );
}

/// A cast table's pattern of several words matches a run of word
/// tokens under one parent (the first token is the mention), a
/// `prefix` column is a stem, and a drama speech resolves by its
/// printed prefix.
#[test]
fn lit_cast_table_runs_and_speeches() {
    let tei = r##"<TEI xmlns="http://www.tei-c.org/ns/1.0">
<text><body>
<div type="act"><head>ACT I</head>
<sp><speaker>Aunt Polly</speaker><p>Tom! Where is that boy?</p></sp>
<p>Aunt Polly looked; Aunt
Polly sighed. Polly's cat slept.</p>
</div>
</body></text></TEI>"##;
    let mut m = quarb_text_koine::parse_lit_xml_as(tei, "tei").unwrap();
    m.tokenize_with(None);
    let dir = std::env::temp_dir().join(format!("quarb-cast-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("cast.csv");
    std::fs::write(&csv, "id,prefix,name,role\npolly,Aunt Polly,Aunt Polly,the guardian\ntom,Tom,Tom Sawyer,the boy\n").unwrap();
    let rows = quarb_text::CastRow::read_csv(csv.to_str().unwrap()).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(rows[0].words, 2);
    m.apply_cast(&rows);
    // "Aunt Polly" twice as a run under one paragraph, the first
    // token the mention; the bare "Polly's" is not the two-word
    // pattern; the words of her speech answer her through their
    // block, as before; "Tom!" once.
    assert_eq!(
        values(&m, r#"//token[::prosopon = "polly"][:: = "Aunt"] @| count"#),
        ["2"]
    );
    assert_eq!(
        values(
            &m,
            r#"//token[::prosopon = "polly"][:: = "Polly"] @| count"#
        ),
        ["0"]
    );
    assert_eq!(
        values(&m, r#"//token[::prosopon = "polly"][:: = "boy"] @| count"#),
        ["1"]
    );
    assert_eq!(values(&m, r#"//token[::prosopon = "tom"] @| count"#), ["1"]);
    assert_eq!(values(&m, r#"//dialogue->character::onym"#), ["polly"]);
    assert_eq!(
        values(
            &m,
            r#"//character[::onym = "polly"]<-character[:::name = "token"] @| count"#
        ),
        ["2"]
    );
    assert_eq!(
        values(&m, r#"//character[::onym = "polly"]::role"#),
        ["the guardian"]
    );
}

const RNC: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<html>
<head>
<meta content="А. С. Пушкин" name="author"/>
<title>Капитанская дочка</title>
</head>
<body>
<p><se><w><ana lex="отец" gr="S,m,anim=sg,nom" sem="r:concr t:hum"/>Отец</w> <w><ana lex="мой" gr="A-PRO=m,sg,nom"/>мой</w> <w><ana lex="служить" gr="V,ipf,intr,act=praet,sg,indic,m"/>служил</w>.</se> <se><w><ana lex="я" gr="S-PRO,sg,1p=nom"/>Я</w> <w><ana lex="жить" gr="V,ipf,intr,act=praet,sg,indic,m"/>жил</w> <w><ana lex="недоросль" gr="S,m,anim=sg,ins"/><ana lex="недоросль" gr="S,m,anim=pl,dat"/>недорослем</w>.</se></p>
<p><se><w><ana lex="матушка" gr="S,f,anim=sg,nom"/>Матушка</w> <w><ana lex="быть" gr="V,ipf,intr,act=praet,sg,indic,f"/>была</w> <w><ana lex="ещё" gr="ADV"/>ещё</w> <w><ana lex="я" gr="S-PRO,sg,1p=ins"/>мною</w> <w><ana lex="брюхатый" gr="A=f,sg,nom,plen"/>брюхата</w>.</se></p>
</body>
</html>
"#;

/// The parsing pack (ruling #80): a corpus the source itself parsed
/// mounts with its own tokens and sentences — the RNC export's
/// `w`/`ana` become tokens carrying lemma, part of speech,
/// features and semantic class; the untokenized punctuation is
/// still a token; the prose is untouched.
#[test]
fn parsing_pack_rnc() {
    assert_eq!(quarb_text_koine::detect_xml_kind(RNC), Some("rnc"));
    let mut m = quarb_text_koine::parse_lit_xml_as(RNC, "rnc").unwrap();
    m.tokenize_with(None);
    assert_eq!(
        values(&m, r#"//paragraph[:: *= "недорослем"]::"#),
        ["Отец мой служил. Я жил недорослем."]
    );
    // Every prose block has its sentence; the declared ones carry
    // the source's id (a running number here).
    assert_eq!(values(&m, "//sentence[::id]::id"), ["1", "2", "3"]);
    assert_eq!(
        values(&m, r#"//sentence[::id = "1"]::"#),
        ["Отец мой служил."]
    );
    assert_eq!(values(&m, r#"//token[::lemma = "служить"]::"#), ["служил"]);
    assert_eq!(values(&m, r#"//token[::upos = "S"] @| count"#), ["3"]);
    assert_eq!(
        values(&m, r#"//token[:: = "Отец"]::feats"#),
        ["m,anim=sg,nom"]
    );
    assert_eq!(
        values(&m, r#"//token[:: = "Отец"]::sem"#),
        ["r:concr_t:hum"]
    );
    assert_eq!(
        values(&m, r#"//token[:: = "недорослем"]::alt"#),
        ["недоросль S m,anim=pl,dat"]
    );
    assert_eq!(values(&m, "//sentence[::id]//token<punct> @| count"), ["3"]);
    assert_eq!(values(&m, "//sentence[::id]//token<word> @| count"), ["11"]);
    // The corpus reading of the same file carries the layer too.
    let mut c = quarb_text_koine::parse_xml_as(RNC, "rnc").unwrap();
    c.tokenize_with(None);
    assert_eq!(values(&c, r#"//token[::lemma = "я"] @| count"#), ["2"]);
}

/// TEI's `w`/`pc`/`s` (at-epimerismos through atrep's importer):
/// the sentence ids, the lemma and part of speech, every
/// `Key=Value` of `msd` as a property; a `w` without attributes
/// stays plain prose.
#[test]
fn parsing_pack_tei() {
    let tei = r##"<TEI xmlns="http://www.tei-c.org/ns/1.0">
<text><body>
<p><s xml:id="s1"><w lemma="школа" pos="NOUN" msd="Case=Nom|Number=Sing">Школа</w> <w lemma="учить" pos="VERB">учит</w><pc pos="PUNCT">.</pc></s> <s>Plain <w>words</w> stay.</s></p>
</body></text></TEI>"##;
    let mut m = quarb_text_koine::parse_lit_xml_as(tei, "tei").unwrap();
    m.tokenize_with(None);
    assert_eq!(
        values(&m, "/paragraph[1]::"),
        ["Школа учит. Plain words stay."]
    );
    assert_eq!(values(&m, "//sentence @| count"), ["2"]);
    assert_eq!(values(&m, "//sentence[1]::id"), ["s1"]);
    assert_eq!(values(&m, r#"//token[:: = "Школа"]::Case"#), ["Nom"]);
    assert_eq!(values(&m, r#"//token[::upos = "VERB"]::lemma"#), ["учить"]);
    assert_eq!(
        values(&m, r#"//sentence[1]//token[:: = "."]::upos"#),
        ["PUNCT"]
    );
    assert_eq!(values(&m, r#"//token[:: = "words"]::lemma"#), [""]);
    assert_eq!(values(&m, "//sentence[2]//token<word> @| count"), ["3"]);
}

/// PROIEL: heads point at token onyms — `->head` and `<-head`
/// follow them, the relation is `::deprel`.
#[test]
fn parsing_pack_proiel() {
    let proiel = r#"<?xml version="1.0" encoding="UTF-8"?>
<proiel schema-version="2.0">
<source id="atrep" language="und">
<title>Greek New Testament</title>
<div>
<title>Matthew</title>
<sentence id="1">
<token id="1" form="Βίβλος" citation-part="MATT 1.1" lemma="βίβλος" part-of-speech="Nb" morphology="-s---fn--i" head-id="2" relation="sub" presentation-after=" "/>
<token id="2" form="γενέσεως" citation-part="MATT 1.1" lemma="γένεσις" part-of-speech="Nb" morphology="-s---fg--i" relation="pred" presentation-after=" "/>
<token id="3" form="Ἰησοῦ" citation-part="MATT 1.1" lemma="Ἰησοῦς" part-of-speech="Ne" morphology="-s---mg--i" head-id="2" relation="atr" presentation-after="."/>
</sentence>
</div>
</source>
</proiel>
"#;
    assert_eq!(quarb_text_koine::detect_xml_kind(proiel), Some("proiel"));
    let mut m = quarb_text_koine::parse_lit_xml_as(proiel, "proiel").unwrap();
    m.tokenize_with(None);
    assert_eq!(
        values(&m, r#"//token[:: = "Βίβλος"]->head::"#),
        ["γενέσεως"]
    );
    assert_eq!(values(&m, r#"//token[:: = "Βίβλος"]::deprel"#), ["sub"]);
    assert_eq!(
        values(&m, r#"//token[:: = "γενέσεως"]<-head @| count"#),
        ["2"]
    );
    assert_eq!(
        values(&m, r#"//token[:: = "Ἰησοῦ"]::feats"#),
        ["-s---mg--i"]
    );
}

/// Citation coordinates (ruling #81): `?scheme=` turns USFM's
/// chapter and verse markers into milestones with full-path
/// values under the scheme, and `::cite` answers the coordinate
/// in force at any node — a token's, a sentence's, a paragraph's;
/// a container cites as its first milestone.
#[test]
fn lit_scheme_and_cite() {
    let usfm = "\\id JHN\n\\h John\n\\c 3\n\\p\n\\v 16 For God so loved the world. \\v 17 For God sent the Son.\n\\c 4\n\\p\n\\v 1 The Lord knew.\n";
    let mut m = quarb_text_koine::parse_lit_xml_as_with(usfm, "usfm", Some("web")).unwrap();
    m.tokenize_with(None);
    assert_eq!(
        values(&m, "//milestone::value"),
        ["jhn.3", "jhn.3.16", "jhn.3.17", "jhn.4", "jhn.4.1"]
    );
    assert_eq!(values(&m, "//milestone[1]::scheme"), ["web"]);
    assert_eq!(values(&m, r#"//token[:: = "loved"]::cite"#), ["jhn.3.16"]);
    assert_eq!(values(&m, r#"//token[:: = "sent"]::cite"#), ["jhn.3.17"]);
    assert_eq!(values(&m, r#"//token[:: = "knew"]::cite"#), ["jhn.4.1"]);
    assert_eq!(
        values(&m, r#"//sentence[::cite = "jhn.3.17"]::"#),
        ["For God sent the Son."]
    );
    // Without a scheme the markers stay markers: no milestone, no cite.
    let mut p = quarb_text_koine::parse_lit_xml_as(usfm, "usfm").unwrap();
    p.tokenize_with(None);
    assert_eq!(values(&p, "//milestone @| count"), ["0"]);
    assert_eq!(values(&p, r#"//token[:: = "loved"]::cite"#), [""]);
}

/// Old spelling (ruling #82): under a spelling table the tokens'
/// `::lower` and `::modern` read the modern spelling of a
/// historical edition, `::` stays the edition's own, and an
/// unknown table refuses.
#[test]
fn corpus_modernize_folds_old_spelling() {
    let md = "Я ѣхалъ на перекладныхъ изъ Тифлиса. Міръ объѣхалъ.\n";
    let mut m = quarb_text_koine::parse_markdown(md).unwrap();
    assert!(m.set_orthography("klingon").is_err());
    m.set_orthography("дореформенная").unwrap();
    m.tokenize_with(None);
    assert_eq!(values(&m, r#"//token[::lower = "ехал"]::"#), ["ѣхалъ"]);
    assert_eq!(values(&m, r#"//token[:: = "Міръ"]::modern"#), ["Мир"]);
    assert_eq!(
        values(&m, r#"//token[:: = "объѣхалъ"]::lower"#),
        ["объехал"]
    );
    assert_eq!(values(&m, r#"//token[::lower = "изъ"] @| count"#), ["0"]);
    assert_eq!(values(&m, r#"//token[::lower = "из"] @| count"#), ["1"]);
}

/// Where no milestone governs, `::cite` is structural: the
/// enclosing sections' names outermost-in (ruling #81 addendum).
#[test]
fn structural_cite_names_the_sections() {
    let md =
        "# The Play\n\n## Act One\n\n### Scene II\n\nA word here.\n\n## Act Two\n\nAnother word.\n";
    let mut m = quarb_text_koine::parse_markdown(md).unwrap();
    m.tokenize_with(None);
    assert_eq!(
        values(&m, r#"//token[:: = "here"]::cite"#),
        ["The Play / Act One / Scene II"]
    );
    assert_eq!(
        values(&m, r#"//token[:: = "Another"]::cite"#),
        ["The Play / Act Two"]
    );
    assert_eq!(
        values(&m, r#"//section[::lemma = "Act Two"]::cite"#),
        ["The Play / Act Two"]
    );
}

/// An inline-bodied note (at-usfm's `\f`, the `note` sim) is a
/// footnote: its body out of the prose under a `footnote` node,
/// the callout at its offset in the block; the corpus reading
/// tokenizes the text, not the apparatus, and the note cites as
/// its callout does.
#[test]
fn inline_notes_are_footnotes_not_prose() {
    let usfm = "\\id JHN\n\\c 3\n\\p\n\\v 16 For God so loved\\f + \\fr 3:16 \\ft The phrase is Greek.\\f* the world. \\v 17 God sent the Son.\n";
    let mut m = quarb_text_koine::parse_lit_xml_as_with(usfm, "usfm", Some("web")).unwrap();
    m.tokenize_with(None);
    assert_eq!(
        values(&m, r#"//sentence[::cite = "jhn.3.16"]::"#),
        ["For God so loved the world."]
    );
    assert_eq!(values(&m, "//footnote<deixis> @| count"), ["1"]);
    assert_eq!(
        values(&m, r#"//footnote<deixis>[::cite = "jhn.3.16"]::"#),
        ["3:16 The phrase is Greek."]
    );
    assert_eq!(values(&m, r#"//token[:: = "phrase"] @| count"#), ["0"]);
    assert_eq!(values(&m, r#"//token[:: = "loved"]::cite"#), ["jhn.3.16"]);
    // The text level reads the same file through the same importer.
    let t = quarb_text_koine::parse_xml_as(usfm, "usfm").unwrap();
    assert_eq!(values(&t, "//footnote<deixis> @| count"), ["1"]);
    assert_eq!(
        values(&t, r#"//paragraph[:: *= "loved"]::"#),
        ["For God so loved the world. God sent the Son."]
    );
}

/// TEI's `<choice>` (at-aphanes' paradosis): the regularized
/// reading is the prose, the original the `choice` span's
/// `::original`, so a token under it answers both.
#[test]
fn lit_choice_keeps_the_original_beside_the_reading() {
    let tei = r##"<TEI xmlns="http://www.tei-c.org/ns/1.0">
<text><body>
<p><choice><orig>Ye olde</orig><reg>The old</reg></choice> lady pulled her spectacles down.</p>
</body></text></TEI>"##;
    let mut m = quarb_text_koine::parse_lit_xml_as(tei, "tei").unwrap();
    m.tokenize_with(None);
    assert_eq!(
        values(&m, "//paragraph[1]::"),
        ["The old lady pulled her spectacles down."]
    );
    assert_eq!(
        values(&m, "//choice | %(t = ::; ::paradosis)"),
        ["%(t = \"The old\"; paradosis = \"Ye olde\")"]
    );
    assert_eq!(values(&m, "//token[::paradosis]::"), ["The", "old"]);
    // The text level keeps the reading and drops the pair.
    let t = quarb_text_koine::parse_xml_as(tei, "tei").unwrap();
    assert_eq!(
        values(&t, "//paragraph[1]::"),
        ["The old lady pulled her spectacles down."]
    );
}

/// A TEI speech carries its speaker pointer in the dialogue lemma
/// (`<sp who>` → `@: @?:(id)PREFIX`), so under the literary
/// reading the speech answers `::prosopon` and reaches its
/// character by key; a speech with a printed prefix and no
/// pointer keeps the bare lemma and links by a cast pattern on
/// the prefix, the implicit form.
#[test]
fn lit_tei_speech_keys_its_speaker() {
    let tei = r##"<TEI xmlns="http://www.tei-c.org/ns/1.0">
<text><body>
<div type="act"><head>ACT I</head>
<div type="scene"><head>SCENE I</head>
<sp who="#barnardo"><speaker>BARNARDO.</speaker><p>Who’s there?</p></sp>
<sp><speaker>FRANCISCO.</speaker><p>Nay, answer me.</p></sp>
<sp who="#voice"><speaker>A VOICE.</speaker><p>Within.</p></sp>
</div></div>
</body></text></TEI>"##;
    let mut m = quarb_text_koine::parse_lit_xml_as(tei, "tei").unwrap();
    m.tokenize_with(None);
    assert_eq!(
        values(&m, "//dialogue::lemma"),
        ["BARNARDO.", "FRANCISCO.", "A VOICE."]
    );
    assert_eq!(
        values(&m, "//dialogue[::prosopon]::prosopon"),
        ["barnardo", "voice"]
    );
    assert_eq!(
        values(&m, "//dialogue::"),
        ["Who’s there?", "Nay, answer me.", "Within."]
    );
    let dir = std::env::temp_dir().join(format!("quarb-sp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("cast.csv");
    std::fs::write(
        &csv,
        "id,pattern,name\nbarnardo,^(BARNARDO|Barnardo)\\.?$,Barnardo\nfrancisco,^(FRANCISCO|Francisco)\\.?$,Francisco\n",
    )
    .unwrap();
    let rows = quarb_text::CastRow::read_csv(csv.to_str().unwrap()).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    m.apply_cast(&rows);
    // the pointer links Barnardo by key, the prefix links
    // Francisco by pattern, and the voice names a character the
    // table does not declare
    assert_eq!(
        values(&m, "//dialogue | %(l = ::lemma; c = ->character::onym)"),
        [
            "%(l = \"BARNARDO.\"; c = \"barnardo\")",
            "%(l = \"FRANCISCO.\"; c = \"francisco\")",
            "%(l = \"A VOICE.\"; c = null)"
        ]
    );
}

/// The gazetteer (ruling #87): a places table declares `place`
/// nodes as a cast table declares characters; a word token a
/// row's pattern matches answers `::chora` and reaches the place
/// as `->place`, the place its mentions as `<-place`, and the
/// row's other columns are the place's fields.
#[test]
fn lit_places_table_declares_the_gazetteer() {
    let tei = r##"<TEI xmlns="http://www.tei-c.org/ns/1.0">
<text><body>
<div type="chapter"><head>I</head>
<p>We sailed from New York and came to Gibraltar; Paris was far off.</p>
<p>Paris at last. Paris!</p>
</div>
</body></text></TEI>"##;
    let mut m = quarb_text_koine::parse_lit_xml_as(tei, "tei").unwrap();
    m.tokenize_with(None);
    let dir = std::env::temp_dir().join(format!("quarb-places-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("places.csv");
    std::fs::write(
        &csv,
        "id,pattern,name,country,lat,lon\nnew-york,^New York$,New York,United States,40.71,-74.01\nparis,^Paris$,Paris,France,48.86,2.35\n",
    )
    .unwrap();
    let rows = quarb_text::CastRow::read_csv(csv.to_str().unwrap()).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    m.apply_places(&rows);
    assert_eq!(
        values(&m, "//place | %(::onym; ::country; n = (<-place @| count))"),
        [
            "%(onym = \"new-york\"; country = \"United States\"; n = 1)",
            "%(onym = \"paris\"; country = \"France\"; n = 3)"
        ]
    );
    assert_eq!(values(&m, r#"//token[::chora = "paris"] @| count"#), ["3"]);
    assert_eq!(
        values(&m, "//token[:: = \"Gibraltar\"] | ->place @| count"),
        ["0"]
    );
    assert_eq!(
        values(
            &m,
            "//paragraph | %(where = (//token[::chora] | %(p = ->place::lemma) | :p @| unique @| join(\", \")))"
        ),
        ["%(where = \"New York, Paris\")", "%(where = \"Paris\")"]
    );
}
