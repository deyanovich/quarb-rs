//! The web level over an in-memory store built from a handful of
//! pages: the tree, the declared identity, the links, the graft.

use quarb::{QueryResult, Value};
mod common;
use common::{page, site};
use quarb_web::{MemoryStore, PageFile, SiteInput, WebAdapter};

fn values(a: &WebAdapter<MemoryStore>, q: &str) -> Vec<Value> {
    match quarb::run(q, a).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryResult::Values(v) => v,
        QueryResult::Nodes(n) => n.into_iter().map(|n| Value::Str(a.locator(n))).collect(),
    }
}

fn strs(a: &WebAdapter<MemoryStore>, q: &str) -> Vec<String> {
    values(a, q).into_iter().map(|v| v.to_string()).collect()
}

#[test]
fn the_tree_from_paths() {
    let a = site();
    assert_eq!(strs(&a, "//page @| count"), ["5"]);
    assert_eq!(strs(&a, "//dir::path"), ["guides"]);
    assert_eq!(
        strs(&a, "/sites/example.org/pages/guides/*::title"),
        ["Quarb for jq users", "Notes", "Quarb for SQL users"]
    );
    // Document order is the tree's pre-order, siblings bytewise.
    assert_eq!(
        strs(&a, "//page::path"),
        [
            "about.html",
            "guides/jq.html",
            "guides/notes.html",
            "guides/sql.html",
            "index.html"
        ]
    );
    // A page answers to its stem as well as its file name.
    assert_eq!(
        strs(&a, "/sites/example.org/pages/guides/jq::title"),
        ["Quarb for jq users"]
    );
    assert_eq!(strs(&a, "/sites/*<site>::host"), ["example.org"]);
    assert_eq!(strs(&a, "/sites/example.org::::n-pages"), ["5"]);
}

#[test]
fn declared_identity() {
    let a = site();
    assert_eq!(strs(&a, "//page<tag:JSON> @| count"), ["2"]);
    assert_eq!(
        strs(&a, "//page<category:Guides>::title"),
        ["Quarb for jq users", "Notes", "Quarb for SQL users"]
    );
    assert_eq!(
        strs(&a, "//page[::category = \"Start\"]::path"),
        ["about.html", "index.html"]
    );
    // The canonical URL wins over the joined path; the rest join.
    assert_eq!(
        strs(&a, "/sites/example.org/pages/guides/jq::href"),
        ["https://example.org/guides/jq.html"]
    );
    assert_eq!(
        strs(&a, "/sites/example.org/pages/about.html::href"),
        ["https://example.org/about.html"]
    );
    assert_eq!(
        strs(&a, "/sites/example.org/pages/index.html::description"),
        ["The front page"]
    );
    // Tags and categories are rows of their own containers.
    assert_eq!(strs(&a, "//tag::title"), ["JSON", "SQL", "jq"]);
    assert_eq!(strs(&a, "//category::title"), ["Guides", "Start"]);
    assert_eq!(
        strs(&a, "/sites/example.org/tags/JSON<-page::title"),
        ["Quarb for jq users", "Quarb for SQL users"]
    );
    assert_eq!(
        strs(&a, "/sites/example.org/pages/guides/jq->tag::title"),
        ["jq", "JSON"]
    );
    assert_eq!(
        strs(&a, "/sites/example.org/pages/guides/jq->category::title"),
        ["Guides"]
    );
}

#[test]
fn links_from_the_dom() {
    let a = site();
    // Every hyperlink of the body, li included, resolved; a
    // fragment does not split the target; self-links dropped.
    assert_eq!(
        strs(&a, "/sites/example.org/pages/index.html->link::path"),
        ["guides/jq.html", "guides/sql.html", "about.html"]
    );
    assert_eq!(
        strs(&a, "/sites/example.org/pages/guides/jq<-link::path"),
        ["guides/sql.html", "index.html"]
    );
    assert_eq!(strs(&a, "//page<orphan>::path"), ["guides/notes.html"]);
    // A link inside a heading counts (an index's `<h3><a href>`).
    assert_eq!(
        strs(&a, "/sites/example.org/pages/about.html<-link::path"),
        ["guides/sql.html", "index.html"]
    );
    assert_eq!(strs(&a, "//page[::in_degree = 0]::title"), ["Notes"]);
    assert_eq!(
        strs(&a, "/sites/example.org/pages/guides/jq::redlinks"),
        ["1"]
    );
    assert_eq!(
        strs(&a, "/sites/example.org/pages/guides/jq::mutual_degree"),
        ["1"]
    );
    assert_eq!(
        strs(&a, "/sites/example.org/pages/index.html::out_degree"),
        ["3"]
    );
    // Rank sums to one over the graph's nodes: the pages (a
    // category with no page of its own is a term, not a node).
    let total: f64 = values(&a, "//page::pagerank")
        .iter()
        .map(|v| match v {
            Value::Float(f) => *f,
            _ => 0.0,
        })
        .sum();
    assert!((total - 1.0).abs() < 1e-9, "{total}");
}

#[test]
fn the_graft_and_the_prose_arrow() {
    let a = site();
    assert_eq!(
        strs(&a, "/sites/example.org/pages/guides/sql//section::lemma"),
        ["Quarb for SQL users", "Joins", "About"]
    );
    assert_eq!(strs(&a, "//page//paragraph @| count"), ["6"]);
    // A prose ref leaving its page lands on the page it names —
    // on the fragment's bearer when it names one.
    assert_eq!(
        strs(&a, "/sites/example.org/pages/index.html//paragraph//ref-->"),
        [
            "/sites/example.org/pages/guides/jq.html",
            "/sites/example.org/pages/guides/sql.html!/section/section"
        ]
    );
    assert_eq!(
        strs(&a, "/sites/example.org/pages/guides/sql//ref-->::title"),
        ["Quarb for jq users", "Home", "About"]
    );
    // The text level's deep link narrows through the page's URL.
    assert_eq!(
        strs(
            &a,
            "/sites/example.org/pages/guides/sql//section[::lemma = \"Joins\"]::href"
        ),
        ["https://example.org/guides/sql.html#joins"]
    );
    assert_eq!(
        strs(&a, "//page<tag:jq> | link"),
        [
            "%(title = \"Quarb for jq users\"; href = \"https://example.org/guides/jq.html\"; text = null)"
        ]
    );
    // Ascent from a graft node reaches the page, then its dir.
    assert_eq!(
        strs(
            &a,
            "/sites/example.org/pages/guides/sql//section[::lemma = \"Joins\"]\\\\<dir>::path"
        ),
        ["guides"]
    );
}

#[test]
fn a_model_alias_reaches_the_level_by_kind() {
    // `alias group category`: the written name is rewritten and
    // the level answers under its own rule — the kind name.
    let a = site();
    let model = quarb_model::parse_model("alias group category;\nalias <s/^tag://i>;").unwrap();
    let m = quarb_model::ModelAdapter::new(a, model);
    let got = match quarb::run("//group::title", &m).unwrap() {
        QueryResult::Values(v) => v.into_iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        _ => panic!("values"),
    };
    assert_eq!(got, ["Guides", "Start"]);
    let got = match quarb::run("//page<json> @| count", &m).unwrap() {
        QueryResult::Values(v) => v.into_iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        _ => panic!("values"),
    };
    assert_eq!(got, ["2"]);
}

#[test]
fn the_name_index_agrees_with_the_walk() {
    // `//page` through the store's rank order and through a model
    // layer that forces the walk (a name alias) must agree, order
    // included; the same for `//dir` and a subtree.
    let a = site();
    let model = quarb_model::parse_model("alias leaf page;").unwrap();
    let walked = quarb_model::ModelAdapter::new(site(), model);
    for q in [
        "//page::path",
        "//dir::path",
        "/sites/example.org/pages/guides//page::path",
        "//tag::title",
        "//page @| count",
    ] {
        let fast = strs(&a, q);
        let slow = match quarb::run(q, &walked).unwrap() {
            QueryResult::Values(v) => v.into_iter().map(|v| v.to_string()).collect::<Vec<_>>(),
            _ => panic!(),
        };
        assert_eq!(fast, slow, "{q}");
    }
}

#[test]
fn base_href_moves_the_join_base() {
    let files = vec![
        PageFile {
            path: "wiki/Roma.html".into(),
            html: page(
                "Roma",
                "<base href=\"//example.org/wiki/\"><link rel=\"canonical\" href=\"https://example.org/wiki/Roma.html\">",
                "<p>See <a href=\"./Lazio.html\">Lazio</a>.</p>",
            ),
        },
        PageFile {
            path: "wiki/Lazio.html".into(),
            html: page(
                "Lazio",
                "<link rel=\"canonical\" href=\"https://example.org/wiki/Lazio.html\">",
                "<p>A region.</p>",
            ),
        },
    ];
    let a = WebAdapter::new(MemoryStore::build(
        SiteInput {
            base_url: String::new(),
            snapshot: None,
        },
        files,
    ));
    assert_eq!(
        strs(&a, "/sites/example.org/pages/wiki/Roma->link::title"),
        ["Lazio"]
    );
    assert_eq!(
        strs(&a, "/sites/example.org/pages/wiki/Roma//ref-->::title"),
        ["Lazio"]
    );
    assert_eq!(strs(&a, "/sites/*::host"), ["example.org"]);
}

/// Pages fetched under extension-less URLs (a site as its sitemap
/// names it) are pages, and links written either way meet: `/docs/`
/// with the directory's index, `/about` with an extension-less page.
#[test]
fn extensionless_pages_and_both_link_spellings() {
    use quarb_web::{MemoryStore, PageFile, SiteInput, WebAdapter};
    let files = vec![
        PageFile { path: "index.html".into(), html: r#"<title>Home</title><p><a href="/docs/">d</a> <a href="/about">a</a> <a href="/docs/intro/">i</a></p>"#.into() },
        PageFile { path: "about".into(), html: r#"<title>About</title><p><a href="/docs/index.html">d</a></p>"#.into() },
        PageFile { path: "docs/index.html".into(), html: r#"<title>Docs</title><p><a href="/about">a</a></p>"#.into() },
        PageFile { path: "docs/intro".into(), html: r#"<title>Intro</title><p>x</p>"#.into() },
        PageFile { path: "notes".into(), html: "plain text, not a page".into() },
    ];
    let store = MemoryStore::build(
        SiteInput {
            base_url: "https://x.example/".into(),
            snapshot: None,
        },
        files,
    );
    let a = WebAdapter::new(store);
    let values = |q: &str| -> Vec<String> {
        match quarb::run(q, &a).unwrap() {
            quarb::QueryResult::Values(v) => v.into_iter().map(|v| v.display_form()).collect(),
            quarb::QueryResult::Nodes(n) => n.into_iter().map(|n| a.locator(n)).collect(),
        }
    };
    assert_eq!(
        values("//page::path"),
        ["about", "docs/index.html", "docs/intro", "index.html"]
    );
    assert_eq!(
        values("//page[::path = \"about\"] | <-link::path"),
        ["docs/index.html", "index.html"]
    );
    assert_eq!(
        values("//page[::path = \"docs/index.html\"] | <-link::path"),
        ["about", "index.html"]
    );
    assert_eq!(
        values("//page[::path = \"docs/intro\"] | <-link::path"),
        ["index.html"]
    );
    assert_eq!(values("//page[::redlinks > 0] @| count"), ["0"]);
}
