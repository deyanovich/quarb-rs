//! End-to-end tests: queries run through the engine against a
//! realistic page lowered to the text level.

use quarb_text::TextModel;

const DOC: &str = r##"<!doctype html>
<html lang="en">
  <head><title>Site</title><style>p { color: red }</style></head>
  <body>
    <nav>Menu Home About</nav>
    <header>Site chrome</header>
    <main>
      <h1>Guide</h1>
      <p>Intro paragraph with <a href="/x">a link</a>.</p>
      <div class="wrap">
        <h2>Usage</h2>
        <p>Use it.</p>
      </div>
      <blockquote><p>Quoted wisdom.</p><footer>— Sage</footer></blockquote>
      <ul>
        <li>alpha</li>
        <li>beta <ul><li>beta-child</li></ul></li>
      </ul>
      <ol start="3">
        <li>third</li>
        <li>fourth</li>
      </ol>
      <pre><code class="language-rust">fn main() {}</code></pre>
      <table>
        <caption>Crew</caption>
        <thead><tr><th>Name</th><th>Role</th></tr></thead>
        <tbody>
          <tr><td>Alice</td><td>captain</td></tr>
          <tr><td>Bob</td><td></td></tr>
        </tbody>
      </table>
      <script>alert("soup")</script>
    </main>
    <footer>copyright</footer>
  </body>
</html>"##;

fn model() -> TextModel {
    quarb_text_html::parse(DOC)
}

fn values(query: &str) -> Vec<String> {
    let model = model();
    match quarb::run(query, &model).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    }
}

fn nodes(query: &str) -> Vec<String> {
    let model = model();
    let mut got: Vec<String> = match quarb::run(query, &model).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns.into_iter().map(|n| model.locator(n)).collect(),
        quarb::QueryResult::Values(_) => panic!("expected nodes"),
    };
    got.sort();
    got
}

/// Soup — nav, header, footer, script, style, head — leaves no
/// trace in the document prose.
#[test]
fn soup_is_dropped() {
    let prose = values("::").remove(0);
    for soup in ["Menu", "Site chrome", "alert", "copyright", "color"] {
        assert!(!prose.contains(soup), "soup {soup:?} leaked into {prose:?}");
    }
}

/// Flat h1/h2 derive enclosing sections — the h2 inside a
/// transparent div still sections.
#[test]
fn sections_nest_through_wrappers() {
    assert_eq!(values("/section::lemma"), vec!["Guide"]);
    assert_eq!(values("/section/section::lemma"), vec!["Usage"]);
    assert_eq!(values("/section/section::::level"), vec!["2"]);
}

/// Inline markup flattens into paragraph prose.
#[test]
fn inline_flattens() {
    assert_eq!(
        values("/section/paragraph::"),
        vec!["Intro paragraph with a link."]
    );
}

/// The blockquote's trailing footer is its attribution.
#[test]
fn blockquote_attribution() {
    assert_eq!(values("//blockquote::hypograph"), vec!["— Sage"]);
    assert_eq!(values("//blockquote/paragraph::"), vec!["Quoted wisdom."]);
}

/// Real lists and their nesting; the ordered list keeps its start.
#[test]
fn lists() {
    assert_eq!(
        values("/section/section/unordered-list/unordered-item::"),
        vec!["alpha", "beta\nbeta-child"]
    );
    // Document order: the ol (start=3), then the table's rows.
    assert_eq!(values("//ordered-item::taxis"), vec!["3", "4", "1", "2"]);
}

/// Fenced code becomes a verbatim block with its language.
#[test]
fn verbatim() {
    assert_eq!(values("//verbatim::::lang"), vec!["rust"]);
    assert_eq!(values("//verbatim::"), vec!["fn main() {}"]);
}

/// The table denormalizes: `<table>`-traited ordered list, caption
/// as lemma, `Header: value` cells, empty cell skipped.
#[test]
fn table_denormalizes() {
    assert_eq!(values("//*<table>::lemma"), vec!["Crew"]);
    assert_eq!(
        values("//*<table>//unordered-item::"),
        vec!["Name: Alice", "Role: captain", "Name: Bob"]
    );
    assert_eq!(nodes("//*<table>").len(), 1);
}

/// The round trip: render the text-level reading to HTML, re-parse
/// it, and the reading is unchanged — flat headings re-derive the
/// same sections, the footer round-trips as the attribution.
#[test]
fn html_round_trips() {
    use quarb::AstAdapter as _;
    let m = model();
    let html = quarb_text::render_node(&m, m.root(), quarb_text::Render::Html);
    let m2 = quarb_text_html::parse(&html);
    // Not compared: `//paragraph::` — the table's caption re-parses
    // as a caption paragraph (the rendered denormalization IS a
    // paragraph + list); the prose (`::`) stays identical.
    for q in [
        "//section::lemma",
        "//blockquote::hypograph",
        "//ordered-item::taxis",
        "//verbatim::::lang",
        "::",
    ] {
        let a = quarb::run(q, &m).unwrap();
        let b = quarb::run(q, &m2).unwrap();
        let show = |r: quarb::QueryResult| match r {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
            quarb::QueryResult::Nodes(_) => panic!("expected values"),
        };
        assert_eq!(show(a), show(b), "round trip diverged on {q}");
    }
}

/// A Wikipedia-style infobox: leading lone th = the table's title
/// (lemma), row labels carry onto their values, a mid-table lone
/// th stays a bare subheading line.
#[test]
fn infobox_row_labels() {
    let m = quarb_text_html::parse(
        r#"<table class="infobox">
        <tbody>
        <tr><th colspan="2">Emu War</th></tr>
        <tr><td colspan="2">A man holding an emu</td></tr>
        <tr><th>Date</th><td>2 November 1932</td></tr>
        <tr><th colspan="2">Belligerents</th></tr>
        <tr><th>Result</th><td>Emu victory</td></tr>
        </tbody></table>"#,
    );
    let vals = |q: &str| match quarb::run(q, &m).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        _ => panic!("expected values"),
    };
    assert_eq!(vals("//*<table>::lemma"), vec!["Emu War"]);
    assert_eq!(
        vals("//*<table>//unordered-item::"),
        vec![
            "A man holding an emu",
            "Date: 2 November 1932",
            "Belligerents",
            "Result: Emu victory",
        ]
    );
}

fn vals(model: &TextModel, query: &str) -> Vec<String> {
    match quarb::run(query, model).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    }
}

/// A `dl` mounts as a list whose items carry `::lemma` — dt is
/// property projection, dd is content; dl/dt/dd itself is just
/// HTML's serialization of "items with lemmas" (ruling #25).
#[test]
fn definition_lists_mount_as_lemma_items() {
    let m = quarb_text_html::parse(
        r#"<dl>
             <dt>Emu</dt>
             <dd>A large flightless bird.</dd>
             <dt>Lewis gun</dt>
             <dt>Machine gun</dt>
             <dd>The army's contribution.</dd>
             <dd>Mounted on a truck.</dd>
           </dl>"#,
    );
    assert_eq!(
        vals(&m, "//unordered-item::lemma"),
        vec!["Emu", "Lewis gun, Machine gun"]
    );
    assert_eq!(
        vals(&m, "//unordered-item[::lemma = \"Emu\"]::"),
        vec!["Emu: A large flightless bird."]
    );
    // Two dds fold into the one item, space-joined like li text;
    // the lemma joins inline.
    assert_eq!(
        vals(&m, "//unordered-item[::lemma == (/Lewis/)]::"),
        vec!["Lewis gun, Machine gun: The army's contribution. Mounted on a truck."]
    );
}

/// The infobox dialect: a row's `th` label becomes the value
/// cell's `::lemma`, addressable without a regex.
#[test]
fn row_label_tables_carry_lemmas() {
    let m = quarb_text_html::parse(
        r#"<table>
             <tr><th colspan="2">Emu War</th></tr>
             <tr><th>Location</th><td>Campion</td></tr>
             <tr><th>Result</th><td>Emu victory</td></tr>
           </table>"#,
    );
    assert_eq!(vals(&m, "//*<table>::lemma"), vec!["Emu War"]);
    assert_eq!(
        vals(&m, "//*<cell>[::lemma = \"Result\"]::"),
        vec!["Result: Emu victory"]
    );
    assert_eq!(
        vals(&m, "//*<row>[::taxis = 1]/*/*::lemma"),
        vec!["Location"]
    );
}

/// The declared note vocabularies (ruling #35): a noteref callout
/// declares no family and takes its body's; endnote-typed bodies
/// land in the endnote family, footnote-typed in the footnote one.
#[test]
fn note_vocabularies_carry_their_families() {
    let m = quarb_text_html::parse(
        r##"<p>The plan<a epub:type="noteref" href="#en1">1</a>
             held.
             The field<a role="doc-noteref" href="#fn1">2</a> did not.</p>
           <aside epub:type="endnote" id="en1"><p>Filed with the ministry.</p></aside>
           <aside epub:type="footnote" id="fn1"><p>At Campion.</p></aside>"##,
    );
    // marker digits stay out of the prose
    assert_eq!(
        vals(&m, "/paragraph::"),
        ["The plan held. The field did not."]
    );
    // each callout took its resolved body's family
    assert_eq!(
        vals(&m, "//*<deixis>->endnote::"),
        ["Filed with the ministry."]
    );
    assert_eq!(vals(&m, "//*<deixis>->footnote::"), ["At Campion."]);
    assert_eq!(vals(&m, "//endnote @| count"), ["2"]);
    assert_eq!(vals(&m, "//footnote @| count"), ["2"]);
    // <note> marks the two bodies, whichever family
    assert_eq!(vals(&m, "//*<note> @| count"), ["2"]);
    assert_eq!(vals(&m, "//*<dangling> @| count"), ["0"]);
}

/// EPUB's `marginalia` word (the one declared sidenote-like
/// vocabulary in HTML space) lands in the aside family: the
/// element's flow position becomes the insertion-point deixis,
/// the body goes to the document end, and no `<note>` — content,
/// not apparatus. Tufte-style CLASSES stay unread: guessing.
#[test]
fn marginalia_is_the_aside_family() {
    let m = quarb_text_html::parse(
        r##"<p>The advance stalled at the fence line.</p>
           <aside epub:type="marginalia"><p>See the map.</p></aside>
           <p>A second push<a epub:type="noteref" href="#mg">2</a> followed.</p>
           <aside epub:type="marginalia" id="mg"><p>Contested figure.</p></aside>
           <div class="sidenote">A Tufte class, honestly unread as apparatus.</div>"##,
    );
    assert_eq!(
        vals(&m, "//*<deixis>->aside::"),
        ["See the map.", "Contested figure."]
    );
    // the id-less aside anchors where it stood
    assert_eq!(
        vals(&m, r#"/aside[::onym = "m1"]<-aside\*::"#),
        ["The advance stalled at the fence line."]
    );
    // the cited one resolves the noteref through the open family
    assert_eq!(vals(&m, r#"/aside[::onym = "mg"]<-aside @| count"#), ["2"]);
    assert_eq!(vals(&m, "//*<note> @| count"), ["0"]);
    // the Tufte class is ordinary prose, not an aside
    assert_eq!(vals(&m, "//aside @| count"), ["5"]);
    assert_eq!(vals(&m, "//*<dangling> @| count"), ["0"]);
}

// ---------------------------------------------------------------
// Declared identity: what the head says lands on the document.

const META_DOC: &str = r##"<!doctype html>
<html lang="en">
  <head>
    <title>Quarb — Capsa: The Data a Query Carries</title>
    <link rel="canonical" href="https://quarb.org/capsa.html">
    <meta name="description" content="The register and the marks">
    <meta name="keywords" content="capsa, register, Execution Model">
    <meta name="author" content="The Quarb Project">
    <meta property="og:type" content="article">
    <meta property="article:section" content="Data Model">
    <meta property="article:tag" content="marks">
    <meta property="article:published_time" content="2026-09-05T10:00:00Z">
    <script type="application/ld+json">
    {"@context": "https://schema.org", "@type": "TechArticle",
     "headline": "Capsa", "keywords": ["capsa", "topic"],
     "author": {"@type": "Organization", "name": "The Quarb Project"}}
    </script>
  </head>
  <body>
    <main>
      <h1>Capsa</h1>
      <p>The capsule of state. See <a rel="tag" href="/tags/context">context</a>.</p>
    </main>
  </body>
</html>"##;

#[test]
fn head_declarations_land_on_the_document() {
    use quarb::{AstAdapter, Value};
    let m = quarb_text_html::parse(META_DOC);
    let root = m.root();
    // The curated core: tags as traits, in trait spelling; the
    // category too.
    let traits = m.traits(root);
    for t in [
        "tag:capsa",
        "tag:register",
        "tag:Execution-Model",
        "tag:marks",
        "tag:topic",
        "tag:context",
        "category:Data-Model",
    ] {
        assert!(
            traits.contains(&t.to_string()),
            "missing trait {t} in {traits:?}"
        );
    }
    assert_eq!(
        m.property(root, "tags"),
        Some(Value::list(
            [
                "capsa",
                "register",
                "Execution Model",
                "topic",
                "marks",
                "context"
            ]
            .iter()
            .map(|s| Value::Str(s.to_string()))
            .collect()
        ))
    );
    assert_eq!(
        m.property(root, "category"),
        Some(Value::Str("Data Model".into()))
    );
    assert_eq!(
        m.property(root, "description"),
        Some(Value::Str("The register and the marks".into()))
    );
    assert_eq!(
        m.property(root, "author"),
        Some(Value::Str("The Quarb Project".into()))
    );
    assert_eq!(
        m.property(root, "title"),
        Some(Value::Str("Quarb — Capsa: The Data a Query Carries".into()))
    );
    assert!(matches!(
        m.property(root, "published"),
        Some(Value::Instant { .. })
    ));
    // The lossless layer: every declaration under its own name.
    assert_eq!(
        m.metadata(root, "og:type"),
        Some(Value::Str("article".into()))
    );
    assert_eq!(
        m.metadata(root, "article:section"),
        Some(Value::Str("Data Model".into()))
    );
    assert_eq!(
        m.metadata(root, "schema:@type"),
        Some(Value::Str("TechArticle".into()))
    );
    assert_eq!(
        m.metadata(root, "schema:author.name"),
        Some(Value::Str("The Quarb Project".into()))
    );
    assert_eq!(m.metadata(root, "lang"), Some(Value::Str("en".into())));
    assert_eq!(
        m.metadata(root, "canonical"),
        Some(Value::Str("https://quarb.org/capsa.html".into()))
    );
    // Sections are untouched: no page traits leak onto them.
    let section = m.children(root)[0];
    assert!(!m.traits(section).contains(&"tag:capsa".to_string()));
    assert!(
        !traits.contains(&"capsa".to_string()),
        "tags are namespaced"
    );
    // The prose is still the prose.
    assert_eq!(nodes_of(&m, "//section"), vec!["/section"]);
}

fn nodes_of(m: &TextModel, query: &str) -> Vec<String> {
    match quarb::run(query, m).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns.into_iter().map(|n| m.locator(n)).collect(),
        quarb::QueryResult::Values(_) => panic!("expected nodes"),
    }
}

/// The document's declared identity through the query language:
/// the root is where a query starts, so a leading projection
/// reads the page itself.
#[test]
fn head_declarations_answer_queries() {
    let m = quarb_text_html::parse(META_DOC);
    let vals = |q: &str| match quarb::run(q, &m).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    };
    assert_eq!(vals("::category"), vec!["Data Model"]);
    // A declared name with a colon is quoted, like an XML
    // namespaced attribute: `::::"og:type"`.
    assert_eq!(vals("::::\"og:type\""), vec!["article"]);
    assert_eq!(vals("(())::::\"article:section\""), vec!["Data Model"]);
    assert_eq!(vals("::::\"schema:@type\""), vec!["TechArticle"]);
}

/// Links into a document: the canonical URL is the base, a
/// section with an id links exactly, a paragraph below it adds a
/// text fragment, and `| link` packages title, href, and text.
#[test]
fn links_narrow_as_far_as_html_allows() {
    use quarb::{AstAdapter, Value};
    let doc = r##"<!doctype html><html><head><title>Capsa</title>
<link rel="canonical" href="https://quarb.org/capsa.html"></head>
<body><main><h1>Capsa</h1><h2 id="marks">Marks</h2>
<p>A mark is a pocketed reference to a node, filed as the path passes it and recalled later by name or by position, the capsa's node store.</p>
<p>Short one.</p></main></body></html>"##;
    let m = quarb_text_html::parse(doc);
    let vals = |q: &str| match quarb::run(q, &m).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    };
    assert_eq!(
        m.property(m.root(), "href"),
        Some(Value::Str("https://quarb.org/capsa.html".into()))
    );
    assert_eq!(
        vals("//section[::lemma = \"Marks\"]::href"),
        vec!["https://quarb.org/capsa.html#marks"]
    );
    assert_eq!(
        vals("//section[::lemma = \"Marks\"]::anchor"),
        vec!["marks"]
    );
    assert_eq!(
        vals("//paragraph[1]::href"),
        vec![
            "https://quarb.org/capsa.html#marks:~:text=A%20mark%20is%20a%20pocketed%20reference,by%20position%2C%20the%20capsa%27s%20node%20store%2E"
        ]
    );
    assert_eq!(
        vals("//paragraph[2]::href"),
        vec!["https://quarb.org/capsa.html#marks:~:text=Short%20one%2E"]
    );
    assert_eq!(
        vals("//paragraph[2] | link"),
        vec![
            "%(title = \"Capsa\"; href = \"https://quarb.org/capsa.html#marks:~:text=Short%20one%2E\"; text = \"Short one.\")"
        ]
    );
    // A projected topic still links the section it came from.
    assert_eq!(
        vals("//section[::lemma = \"Marks\"]::lemma | link | :href"),
        vec!["https://quarb.org/capsa.html#marks"]
    );
    // No URL declared, no path given: nothing to link.
    let bare = quarb_text_html::parse("<h1>T</h1><p>x</p>");
    assert_eq!(bare.property(bare.root(), "href"), None);
    // A mount path stands in, and a non-HTML form links the document alone.
    let mut doc2 = quarb_text_html::parse("<h1>T</h1><h2 id=\"a\">A</h2><p>x</p>");
    doc2.set_document_path("/notes/page.html");
    assert_eq!(
        quarb::run("//paragraph::href", &doc2)
            .map(|r| format!("{r:?}"))
            .unwrap(),
        "Values([Str(\"/notes/page.html#a:~:text=x\")])"
    );
    let mut pdf = quarb_text_html::parse("<h1>T</h1><h2 id=\"a\">A</h2><p>x</p>");
    pdf.set_document_path("/notes/paper.pdf");
    assert_eq!(
        quarb::run("//paragraph::href", &pdf)
            .map(|r| format!("{r:?}"))
            .unwrap(),
        "Values([Str(\"/notes/paper.pdf\")])"
    );
}
