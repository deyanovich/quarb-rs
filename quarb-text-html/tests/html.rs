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
            quarb::QueryResult::Values(vs) => {
                vs.iter().map(|v| v.to_string()).collect::<Vec<_>>()
            }
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
        quarb::QueryResult::Values(vs) => {
            vs.iter().map(|v| v.to_string()).collect::<Vec<_>>()
        }
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
        vals(&m, "//unordered-item[::lemma =~ /Lewis/]::"),
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
