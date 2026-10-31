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
        ["Quarb for SQL users", "Quarb for jq users", "Notes"]
    );
    // Document order is the tree's pre-order, siblings by pagerank
    // descending (the most linked first), ties by name.
    assert_eq!(
        strs(&a, "//page::path"),
        [
            "index.html",
            "about.html",
            "guides/sql.html",
            "guides/jq.html",
            "guides/notes.html"
        ]
    );
    // A page answers to its stem as well as its file name.
    assert_eq!(
        strs(&a, "/sites/example.org/pages/guides/jq::title"),
        ["Quarb for jq users"]
    );
    assert_eq!(strs(&a, "/sites/*<site>::host"), ["example.org"]);
    assert_eq!(strs(&a, "/sites/example.org:::n-pages"), ["5"]);
}

#[test]
fn declared_identity() {
    let a = site();
    assert_eq!(strs(&a, "//page<tag:JSON> @| count"), ["2"]);
    assert_eq!(
        strs(&a, "//page<category:Guides>::title"),
        ["Quarb for SQL users", "Quarb for jq users", "Notes"]
    );
    assert_eq!(
        strs(&a, "//page[::category = \"Start\"]::path"),
        ["index.html", "about.html"]
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
        ["Quarb for SQL users", "Quarb for jq users"]
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
    // fragment lands on the node bearing the name inside the page
    // (`sql.html#joins` on its Joins section); self-links dropped.
    let targets = strs(&a, "/sites/example.org/pages/index.html->link");
    assert_eq!(targets.len(), 3, "{targets:?}");
    assert_eq!(targets[0], "/sites/example.org/pages/guides/jq.html");
    assert!(
        targets[1].starts_with("/sites/example.org/pages/guides/sql.html!"),
        "{targets:?}"
    );
    assert_eq!(targets[2], "/sites/example.org/pages/about.html");
    assert_eq!(
        strs(&a, "/sites/example.org/pages/guides/jq<-link::path"),
        ["index.html", "guides/sql.html"]
    );
    assert_eq!(strs(&a, "//page<orphan>::path"), ["guides/notes.html"]);
    // A link inside a heading counts (an index's `<h3><a href>`).
    assert_eq!(
        strs(&a, "/sites/example.org/pages/about.html<-link::path"),
        ["index.html", "guides/sql.html"]
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
        ["about", "index.html", "docs/index.html", "docs/intro"]
    );
    assert_eq!(
        values("//page[::path = \"about\"] | <-link::path"),
        ["index.html", "docs/index.html"]
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

#[test]
fn a_redirect_stub_stands_for_its_target() {
    use quarb_web::{MemoryStore, PageFile, SiteInput, WebAdapter};
    let mut files = common::files();
    files.push(PageFile {
        path: "spec/quarb-v1.0-draft.164.html".into(),
        html: common::page("The spec", "", "<p>The language.</p>"),
    });
    // a meta refresh, with the script browsers run (extension-less,
    // as quarb.org packs its stubs)
    files.push(PageFile {
        path: "spec/latest".into(),
        html: "<!doctype html><html><head><meta charset=\"utf-8\"><title>latest</title><meta http-equiv=\"refresh\" content=\"0; url=quarb-v1.0-draft.164.html\"><script>location.replace(\"quarb-v1.0-draft.164.html\" + location.hash)</script></head><body><p>Redirecting to <a href=\"quarb-v1.0-draft.164.html\">the spec</a>.</p></body></html>".into(),
    });
    // a script alone, with a site-absolute target
    files.push(PageFile {
        path: "guide/latest".into(),
        html: "<!doctype html><html><head><title>latest</title><script>\n  location.replace('/guides/jq.html' + location.hash);\n</script></head><body></body></html>".into(),
    });
    let hub_body = "<p>The entries of the reference, one per form, each a page of its own with the grammar it rests on, the commands it names and the recipe that shows it at work; find one by name below or read the list in the order of the specification.</p><ul><li><a href=\"entries/hop.html\">hop</a></li><li><a href=\"entries/group.html\">group</a></li></ul>";
    files.push(PageFile {
        path: "reference/index.html".into(),
        html: format!(
            "<!doctype html><html><head><title>Reference</title><script>var q = new URLSearchParams(location.search).get('entry'); if (q) location.replace('entries/' + q + '.html');</script></head><body>{hub_body}</body></html>"
        ),
    });
    let a = WebAdapter::new(MemoryStore::build(
        SiteInput {
            base_url: "https://example.org/".into(),
            snapshot: None,
        },
        files,
    ));
    assert_eq!(strs(&a, "//page<redirect> @| count"), ["2"]);
    assert_eq!(
        strs(&a, "//page<redirect>::path"),
        ["guide/latest", "spec/latest"]
    );
    assert_eq!(
        strs(&a, "/sites/example.org/pages/spec/latest->redirect::path"),
        ["spec/quarb-v1.0-draft.164.html"]
    );
    assert_eq!(
        strs(
            &a,
            "/sites/example.org/pages/spec/quarb-v1.0-draft.164.html<-redirect::path"
        ),
        ["spec/latest"]
    );
    assert_eq!(
        strs(&a, "/sites/example.org/pages/guide/latest->redirect::path"),
        ["guides/jq.html"]
    );
    // the stub's own text is no content: no links, no page in the graph
    assert_eq!(
        strs(&a, "/sites/example.org/pages/spec/latest->link @| count"),
        ["0"]
    );
    assert_eq!(
        strs(&a, "//page[:name = \"latest\"] | ->redirect:name"),
        ["jq.html", "quarb-v1.0-draft.164.html"]
    );
    // a hub whose script sends the browser on under a condition (a
    // find box deep-linking, old anchors forwarded) keeps its body
    // and stays a page — the Language Reference hub, not a stub
    assert_eq!(
        strs(
            &a,
            "/sites/example.org/pages/reference/index.html<redirect> @| count"
        ),
        ["0"]
    );
    assert_eq!(
        strs(
            &a,
            "/sites/example.org/pages/reference/index.html->redirect @| count"
        ),
        ["0"]
    );
    // an ordinary page is no redirect
    assert_eq!(
        strs(&a, "/sites/example.org/pages/about.html->redirect @| count"),
        ["0"]
    );
}

#[test]
fn a_link_to_no_page_points_at_a_resource() {
    use quarb_web::{MemoryStore, PageFile, SiteInput, WebAdapter};
    // the spec hub links its PDF in the body, as does a second page;
    // a recipe page's PDF link sits in the frame (the footer) and does
    // not count; an external link leaves the site; a missing page
    // stays a redlink
    let mut files = common::files();
    // the spec hub carries every marker by hand: the PDF alternate,
    // the Russian edition, a breadcrumb list, a header nav, a footer
    files.push(PageFile {
        path: "spec.html".into(),
        html: common::page(
            "The spec",
            "<link rel=\"alternate\" type=\"application/pdf\" href=\"spec/quarb.pdf\"><link rel=\"alternate\" hreflang=\"ru\" href=\"https://example.ru/spec.html\"><link rel=\"canonical\" href=\"https://example.org/spec.html\"><script type=\"application/ld+json\">{\"@context\": \"https://schema.org\", \"@type\": \"BreadcrumbList\", \"itemListElement\": [{\"@type\": \"ListItem\", \"position\": 1, \"name\": \"Home\", \"item\": \"https://example.org/index.html\"}, {\"@type\": \"ListItem\", \"position\": 2, \"name\": \"Spec\", \"item\": \"https://example.org/spec.html\"}]}</script>",
            "<header><nav><a href=\"index.html\">Home</a> <a href=\"about.html\">About</a></nav></header><main><p>Read it as a <a href=\"spec/quarb.pdf\">PDF</a>, or the <a href=\"https://www.rfc-editor.org/rfc/rfc8259\">JSON RFC</a> it rests on; <a href=\"missing-page\">a page that is not there</a>.</p></main><footer><a href=\"recipes/book.pdf\">the book</a> <a href=\"about.html\">About</a></footer>",
        ),
    });
    files.push(PageFile {
        path: "recipes.html".into(),
        html: common::page(
            "Recipes",
            "",
            "<p>The <a href=\"spec/quarb.pdf\">spec PDF</a> and the <a href=\"recipes/book.pdf\">book</a>.</p>",
        ),
    });
    let a = WebAdapter::new(MemoryStore::build(
        SiteInput {
            base_url: "https://example.org/".into(),
            snapshot: None,
        },
        files,
    ));
    // a resource is not a page; `//resource` finds them, one per URL,
    // after every page in the document order (no pagerank), by name
    // — a same-site file's path, another host's URL
    assert_eq!(strs(&a, "//page @| count"), ["7"]);
    assert_eq!(
        strs(&a, "//resource::url"),
        [
            "https://example.ru/spec.html",
            "https://www.rfc-editor.org/rfc/rfc8259",
            "https://example.org/recipes/book.pdf",
            "https://example.org/spec/quarb.pdf"
        ]
    );
    assert_eq!(strs(&a, "//resource<pdf> @| count"), ["2"]);
    assert_eq!(
        strs(&a, "//resource<external>::url"),
        [
            "https://example.ru/spec.html",
            "https://www.rfc-editor.org/rfc/rfc8259"
        ]
    );
    // the pages linking a PDF, and the pages leaving the site
    assert_eq!(
        strs(&a, "//page[->link<pdf>]::title"),
        ["Recipes", "The spec"]
    );
    assert_eq!(strs(&a, "//page[->link<external>]::title"), ["The spec"]);
    // `<-link` from a resource: the frame's link does not count
    assert_eq!(
        strs(&a, "//resource[::path = \"recipes/book.pdf\"]<-link::title"),
        ["Recipes"]
    );
    assert_eq!(
        strs(&a, "//resource[::path = \"spec/quarb.pdf\"]<-link::title"),
        ["Recipes", "The spec"]
    );
    // the pagerank graph is pages alone; a missing page stays red
    assert_eq!(strs(&a, "//resource::pagerank"), ["0", "0", "0", "0"]);
    assert_eq!(
        strs(&a, "/sites/example.org/pages/spec.html::redlinks"),
        ["1"]
    );
    // the edges by role: the head's alternates, the crumbs, the
    // header nav and the footer each under their own label
    let spec = "/sites/example.org/pages/spec.html";
    assert_eq!(strs(&a, &format!("{spec}->pdf::path")), ["spec/quarb.pdf"]);
    assert_eq!(strs(&a, "//page[->pdf]::title"), ["The spec"]);
    assert_eq!(
        strs(&a, &format!("{spec}->altlang::url")),
        ["https://example.ru/spec.html"]
    );
    assert_eq!(
        strs(
            &a,
            &format!("{spec}->altlang[$-::hreflang = \"ru\"] @| count")
        ),
        ["1"]
    );
    assert_eq!(strs(&a, &format!("{spec}->canonical::path")), ["spec.html"]);
    // the crumb's position rides the edge: the hop's own predicate
    // reads `$-`, the command after a plain hop the plural `@-`
    assert_eq!(
        strs(&a, &format!("{spec}->breadcrumb[$-::position = 1]::path")),
        ["index.html"]
    );
    assert_eq!(
        strs(&a, &format!("{spec}->breadcrumb | @-::position")),
        ["1"]
    );
    assert_eq!(
        strs(&a, &format!("{spec}->navbar::path")),
        ["index.html", "about.html"]
    );
    assert_eq!(
        strs(&a, &format!("{spec}->footer::path")),
        ["recipes/book.pdf", "about.html"]
    );
    assert_eq!(
        strs(&a, &format!("{spec}->footer<pdf>::path")),
        ["recipes/book.pdf"]
    );
    // the content links alone are `->link`, and `<-navbar` tells the
    // chrome from the content on the pointed-at page
    assert_eq!(
        strs(&a, &format!("{spec}->link::path")),
        ["spec/quarb.pdf", "https://www.rfc-editor.org/rfc/rfc8259"]
    );
    assert_eq!(
        strs(&a, "/sites/example.org/pages/about.html<-navbar::path"),
        ["spec.html"]
    );
    assert_eq!(
        strs(&a, "/sites/example.org/pages/about.html<-footer::path"),
        ["spec.html"]
    );
    assert!(
        !strs(&a, "/sites/example.org/pages/about.html<-link::path")
            .contains(&"spec.html".to_string())
    );
}

#[test]
fn the_edge_inventory_reads_every_marker() {
    use quarb_web::{MemoryStore, PageFile, SiteInput, WebAdapter};
    // a page carrying the declared relations by hand: head links by
    // rel, an asset, an anchor with hints alone and one with a
    // relation, an aside, a table of contents, a fragment link into
    // a sibling page, Open Graph media, JSON-LD entities with ids
    let mut files = common::files();
    files.push(PageFile {
        path: "guides/intro.html".into(),
        html: common::page(
            "Intro",
            "<link rel=\"prev\" href=\"../index.html\"><link rel=\"next\" href=\"jq.html\"><link rel=\"author\" href=\"../about.html\"><link rel=\"license\" href=\"https://creativecommons.org/licenses/by/4.0/\"><link rel=\"stylesheet\" href=\"../style.css\"><link rel=\"icon\" href=\"../icon.png\"><meta property=\"og:image\" content=\"https://example.org/img/cover.png\"><meta property=\"article:author\" content=\"https://example.org/about.html\"><script type=\"application/ld+json\">{\"@context\": \"https://schema.org\", \"@type\": \"Article\", \"author\": {\"@type\": \"Person\", \"@id\": \"https://example.org/about.html\", \"name\": \"The author\"}, \"publisher\": {\"@type\": \"Organization\", \"name\": \"No id\"}, \"sameAs\": [\"https://github.com/example/intro\"], \"isPartOf\": \"https://example.org/guides/jq.html\"}</script>",
            "<nav aria-label=\"Table of contents\"><a href=\"#start\">Start</a></nav><main><h2 id=\"start\">Start</h2><p>See the <a href=\"jq.html#filters\">filters</a>, a <a href=\"https://spam.example/\" rel=\"nofollow ugc\">user link</a> and the <a href=\"sql.html\" rel=\"help\" title=\"the help\">help</a>.</p></main><aside><a href=\"notes.html\">Notes</a></aside>",
        ),
    });
    let a = WebAdapter::new(MemoryStore::build(
        SiteInput {
            base_url: "https://example.org/".into(),
            snapshot: None,
        },
        files,
    ));
    let intro = "/sites/example.org/pages/guides/intro.html";
    // the head's relations by name, the asset as a resource
    assert_eq!(strs(&a, &format!("{intro}->prev::path")), ["index.html"]);
    assert_eq!(
        strs(&a, &format!("{intro}->next::path")),
        ["guides/jq.html"]
    );
    assert_eq!(strs(&a, &format!("{intro}->author::path")), ["about.html"]);
    assert_eq!(
        strs(&a, &format!("{intro}->license::url")),
        ["https://creativecommons.org/licenses/by/4.0/"]
    );
    assert_eq!(
        strs(
            &a,
            &format!("{intro}->asset[$-::rel = \"stylesheet\"]::path")
        ),
        ["style.css"]
    );
    assert_eq!(
        strs(&a, &format!("{intro}->asset::path")),
        ["style.css", "icon.png"]
    );
    assert_eq!(strs(&a, &format!("{intro}->asset<png> @| count")), ["1"]);
    // an anchor's hints ride as properties, its relation labels it
    assert_eq!(
        strs(&a, &format!("{intro}->link[$-::nofollow]::url")),
        ["https://spam.example/"]
    );
    assert_eq!(
        strs(
            &a,
            &format!("{intro}->help[$-::text = \"help\" && $-::title = \"the help\"]::path")
        ),
        ["guides/sql.html"]
    );
    // the positions: the aside and the table of contents
    assert_eq!(
        strs(&a, &format!("{intro}->aside::path")),
        ["guides/notes.html"]
    );
    assert_eq!(strs(&a, &format!("{intro}->toc @| count")), ["1"]);
    // Open Graph and JSON-LD: edges named after the property
    assert_eq!(
        strs(&a, &format!("{intro}->image<png>::url")),
        ["https://example.org/img/cover.png"]
    );
    assert_eq!(strs(&a, &format!("{intro}->author::path @| count")), ["1"]);
    assert_eq!(
        strs(&a, &format!("{intro}->sameas<external> @| count")),
        ["1"]
    );
    assert_eq!(
        strs(&a, &format!("{intro}->ispartof::path")),
        ["guides/jq.html"]
    );
    assert_eq!(strs(&a, &format!("{intro}->publisher @| count")), ["0"]);
    // the pagerank graph is the content links alone
    assert_eq!(
        strs(&a, "/sites/example.org/pages/guides/sql.html::in_degree"),
        ["2"]
    );
}
