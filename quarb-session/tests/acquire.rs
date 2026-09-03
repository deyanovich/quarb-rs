//! The acquisition arrow, end to end at the session layer:
//! `::href-->` lands on a sibling source mounted under the href's
//! URL (on the fragment's element, when the href carries one),
//! and records an unresolved external reference when the document
//! is not mounted.

use quarb_session::doc::Doc;
use quarb_session::{Executor, LocalExecutor};

const HOME: &str = r##"<html><head><title>Home</title></head><body>
  <a href="/about.html">About</a>
  <a href="https://other.example/faq.html">FAQ</a>
  <a href="#top" id="top">Top</a></body></html>"##;
const ABOUT: &str =
    "<html><head><title>About Us</title></head><body><p>Hi</p></body></html>";

fn mount(parts: &[(&str, &str, Option<&str>)]) -> LocalExecutor {
    let mut docs = Vec::new();
    let mut urls = Vec::new();
    for (name, text, url) in parts {
        let mut doc = Doc::parse(text, "html").unwrap();
        if let Some(u) = url {
            doc.attach_url(u);
            urls.push((name.to_string(), u.to_string()));
        }
        docs.push((name.to_string(), doc));
    }
    let doc = if docs.len() == 1 {
        docs.remove(0).1
    } else {
        Doc::mount_docs(docs).unwrap()
    };
    let roots = doc.url_roots(&urls);
    LocalExecutor::new(doc, (1_753_500_000, 0), false).with_url_roots(roots)
}

#[test]
fn arrow_lands_on_a_mounted_sibling() {
    let ex = mount(&[
        ("home", HOME, Some("https://shop.example/index.html")),
        ("about", ABOUT, Some("https://shop.example/about.html")),
    ]);
    // The relative href joins against home's URL and lands on the
    // about source's root; navigation continues inside it.
    let cells = ex.run("/home//a::href--> //title::").unwrap();
    let lines: Vec<String> = cells.iter().map(|c| c.display()).collect();
    assert_eq!(lines, vec!["About Us"], "landed: {lines:?}");
    // The FAQ link stays unresolved — other.example is not mounted.
    assert_eq!(ex.refs(), vec!["https://other.example/faq.html"]);
}

#[test]
fn unmounted_reference_surfaces_not_errors() {
    let ex = mount(&[("home", HOME, Some("https://shop.example/"))]);
    let cells = ex.run("//a::href--> //title::").unwrap();
    assert!(cells.is_empty(), "nothing mounted to land on");
    let refs = ex.refs();
    assert_eq!(
        refs,
        vec![
            "https://other.example/faq.html".to_string(),
            "https://shop.example/about.html".to_string(),
        ],
        "both cross-document hrefs recorded: {refs:?}"
    );
}

#[test]
fn fragment_hrefs_stay_in_document() {
    let ex = mount(&[("home", HOME, Some("https://shop.example/"))]);
    // #top resolves in-document (the classic resolve door) and is
    // never surfaced as external.
    let cells = ex.run("//a[::href = \"#top\"]::href--> ::id").unwrap();
    let lines: Vec<String> = cells.iter().map(|c| c.display()).collect();
    assert_eq!(lines, vec!["top"]);
    assert_eq!(ex.refs(), Vec::<String>::new());
}

#[test]
fn no_url_no_relative_resolution() {
    // Pasted html with no declared URL: absolute hrefs still
    // reference out, relative ones reference nothing.
    let ex = mount(&[("home", HOME, None)]);
    ex.run("//a::href--> //title::").unwrap();
    assert_eq!(ex.refs(), vec!["https://other.example/faq.html"]);
}

#[test]
fn fragment_lands_inside_the_target_document() {
    // The fragment of a cross-document href is the crossref
    // *within* the target: page.html#team lands on the element
    // with id="team" inside the mounted target, not on its root.
    let home = r##"<html><body><a href="/about.html#team">Team</a></body></html>"##;
    let about = r##"<html><head><title>About Us</title></head><body>
      <p id="team">The team</p></body></html>"##;
    let ex = mount(&[
        ("home", home, Some("https://shop.example/index.html")),
        ("about", about, Some("https://shop.example/about.html")),
    ]);
    let cells = ex.run("/home//a::href--> ::id").unwrap();
    let lines: Vec<String> = cells.iter().map(|c| c.display()).collect();
    assert_eq!(lines, vec!["team"], "landed on the #team element: {lines:?}");
    assert_eq!(ex.refs(), Vec::<String>::new());
}

#[test]
fn bare_arrow_and_rel_semantics() {
    // `//a-->`: each node's own reference property (href), the
    // hint filtering on rel, and the cross-document rung riding
    // along — one arrow, all rungs.
    let home = r##"<html><body>
      <a rel="next" href="/about.html">About</a>
      <a rel="nofollow" href="https://other.example/x.html">X</a>
      <a href="#here" id="here">Here</a></body></html>"##;
    let ex = mount(&[
        ("home", home, Some("https://shop.example/index.html")),
        ("about", ABOUT, Some("https://shop.example/about.html")),
    ]);
    // Bare: the fragment link lands in-document, the about link
    // lands on the mounted sibling, the nofollow link surfaces.
    let cells = ex.run("/home//a--> //title::").unwrap();
    let lines: Vec<String> = cells.iter().map(|c| c.display()).collect();
    assert_eq!(lines, vec!["About Us"], "cross-doc landing: {lines:?}");
    assert_eq!(ex.refs(), vec!["https://other.example/x.html"]);
    // The rel hint filters which references resolve at all: only
    // the nofollow link is followed, so only it surfaces.
    let cells = ex.run("/home//a-->nofollow //title::").unwrap();
    assert!(cells.is_empty());
    assert_eq!(ex.refs(), vec!["https://other.example/x.html"]);
    // rel=next reaches the mounted sibling; nothing surfaces.
    let cells = ex.run("/home//a-->next //title::").unwrap();
    let lines: Vec<String> = cells.iter().map(|c| c.display()).collect();
    assert_eq!(lines, vec!["About Us"]);
    assert_eq!(ex.refs(), Vec::<String>::new());
}

#[test]
fn text_level_refs_cross_documents() {
    // The text level speaks the same reference machinery: an
    // external mention surfaces, a mounted sibling answers, and a
    // fragment lands on the bearer *inside* it — a labeled block
    // by its onym.
    let guide = r##"<html><body><h2>Guide</h2>
      <p>See the <a href="/spec.html#usage">usage section</a>
      and the <a href="https://other.example/faq.html">FAQ</a>.</p>
      </body></html>"##;
    let spec = r##"<html><body><h2 id="usage">Usage</h2>
      <p>All of it.</p></body></html>"##;
    let mut g = Doc::parse(guide, "text-html").unwrap();
    g.attach_url("https://shop.example/guide.html");
    let mut sp = Doc::parse(spec, "text-html").unwrap();
    sp.attach_url("https://shop.example/spec.html");
    let doc = Doc::mount_docs(vec![
        ("guide".to_string(), g),
        ("spec".to_string(), sp),
    ])
    .unwrap();
    let urls = vec![
        ("guide".to_string(), "https://shop.example/guide.html".to_string()),
        ("spec".to_string(), "https://shop.example/spec.html".to_string()),
    ];
    let roots = doc.url_roots(&urls);
    let ex = LocalExecutor::new(doc, (1_753_500_000, 0), false).with_url_roots(roots);
    let cells = ex.run("/guide//ref--> ::lemma").unwrap();
    let lines: Vec<String> = cells.iter().map(|c| c.display()).collect();
    assert_eq!(lines, vec!["Usage"], "fragment lands on the labeled section: {lines:?}");
    assert_eq!(ex.refs(), vec!["https://other.example/faq.html"]);
}
