//! The Wikipedia ingest on a synthetic Enterprise HTML dump: the
//! record fields, Parsoid's `./Title` links under a `<base href>`,
//! percent-encoded targets, a redirect, a category page, the two
//! trees, the passes after the rows are in.
#![cfg(all(feature = "ingest", feature = "sqlite"))]

use quarb::QueryResult;
use quarb_web::db::sqlite::SqliteStore;
use quarb_web::db::SqlStore;
use quarb_web::ingest::{self, Options, Tree};
use quarb_web::{SiteId, WebAdapter, WebStore};
use std::io::Write;

fn record(id: i64, name: &str, ns: i64, cats: &[&str], redirects: &[&str], body: &str) -> String {
    let url = format!("https://it.example.org/wiki/{}", name.replace(' ', "_"));
    let cats: Vec<String> = cats.iter().map(|c| format!("{{\"name\":\"{c}\",\"url\":\"https://it.example.org/wiki/{}\"}}", c.replace(' ', "_"))).collect();
    let reds: Vec<String> = redirects.iter().map(|r| format!("{{\"name\":\"{r}\",\"url\":\"https://it.example.org/wiki/{}\"}}", r.replace(' ', "_"))).collect();
    let html = format!(
        "<!DOCTYPE html><html><head><title>{name}</title><base href=\"//it.example.org/wiki/\"></head><body><section><h2 id=\"Storia\">Storia</h2>{body}</section></body></html>"
    );
    serde_json::json!({
        "name": name,
        "identifier": id,
        "url": url,
        "date_modified": "2025-03-01T10:00:00Z",
        "namespace": {"identifier": ns, "name": if ns == 14 {"Categoria"} else {""}},
        "categories": cats.iter().map(|c| serde_json::from_str::<serde_json::Value>(c).unwrap()).collect::<Vec<_>>(),
        "redirects": reds.iter().map(|c| serde_json::from_str::<serde_json::Value>(c).unwrap()).collect::<Vec<_>>(),
        "main_entity": {"identifier": format!("Q{id}")},
        "abstract": format!("{name}, in breve."),
        "article_body": {"html": html, "wikitext": ""}
    })
    .to_string()
}

fn dump(name: &str, lines: &[String]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-web-wiki-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let f = std::fs::File::create(&path).unwrap();
    let gz = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
    let mut ar = tar::Builder::new(gz);
    let body = lines.join("\n") + "\n";
    let mut h = tar::Header::new_gnu();
    h.set_size(body.len() as u64);
    h.set_mode(0o644);
    h.set_cksum();
    ar.append_data(&mut h, "itwiki_0.ndjson", body.as_bytes()).unwrap();
    let gz = ar.into_inner().unwrap();
    gz.finish().unwrap().flush().unwrap();
    path
}

fn fixture() -> (std::path::PathBuf, std::path::PathBuf) {
    let ns0 = dump(
        "ns0.json.tar.gz",
        &[
            record(1, "Roma", 0, &["Categoria:Città", "Categoria:Capitali"], &["Rome"], "<p>Capitale del <a rel=\"mw:WikiLink\" href=\"./Lazio\">Lazio</a> e dell'<a href=\"./Italia\">Italia</a>. Vedi <a href=\"./Guerra_degli_em%C3%B9\">la guerra</a> e <a href=\"./Pagina_inesistente?action=edit&amp;redlink=1\" class=\"new\">una pagina che non c'è</a>.</p>"),
            record(2, "Lazio", 0, &["Categoria:Regioni"], &[], "<p>Regione con capoluogo <a href=\"./Rome\">Roma</a>.</p>"),
            record(3, "Italia", 0, &["Categoria:Geografia"], &[], "<p>Uno stato. <a href=\"./Roma#Storia\">Roma</a> e <a href=\"./Lazio\">Lazio</a>.</p><table about=\"#mwt1\" typeof=\"mw:Transclusion\"><tr><td><a href=\"./Guerra_degli_em%C3%B9\">navbox</a></td></tr></table>"),
            record(4, "Guerra degli emù", 0, &["Categoria:Guerre"], &[], "<p>Una mitragliatrice Lewis contro gli emù. Torna a <a href=\"./Roma\">Roma</a>.</p>"),
        ],
    );
    let ns14 = dump(
        "ns14.json.tar.gz",
        &[
            record(100, "Categoria:Enciclopedia", 14, &[], &[], "<p>La radice.</p>"),
            record(101, "Categoria:Geografia", 14, &["Categoria:Enciclopedia"], &[], "<p>Luoghi: <a href=\"./Categoria:Citt%C3%A0\">le città</a>.</p>"),
            record(102, "Categoria:Città", 14, &["Categoria:Geografia"], &[], "<p>Città.</p>"),
            record(103, "Categoria:Regioni", 14, &["Categoria:Geografia"], &[], "<p>Regioni.</p>"),
            record(104, "Categoria:Capitali", 14, &["Categoria:Città"], &[], "<p>Capitali.</p>"),
        ],
    );
    (ns0, ns14)
}

fn lines<S: WebStore>(a: &WebAdapter<S>, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryResult::Values(v) => v.into_iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(n) => n.into_iter().map(|n| a.locator(n)).collect(),
    }
}

fn build(tree: Tree, name: &str) -> SqliteStore {
    let (ns0, ns14) = fixture();
    let path = std::env::temp_dir().join(format!("quarb-web-wiki-{}-{name}.db", std::process::id()));
    let mut store = SqliteStore::empty(&path).unwrap();
    let opts = Options {
        base_url: String::new(),
        limit: None,
        workers: 2,
        tree,
        root_category: Some("Categoria:Enciclopedia".into()),
        snapshot: Some("test".into()),
        exclude_templates: false,
    };
    let summary = ingest::wikipedia(&[ns0, ns14], &mut store.sink(), &opts).unwrap();
    assert_eq!((summary.articles, summary.categories, summary.redirects), (4, 6, 1), "{summary:?}");
    let log = ingest::finish(&mut store, SiteId(1), &opts).unwrap();
    assert!(log[0].contains("links resolved"), "{log:?}");
    store
}

#[test]
fn the_path_tree() {
    let store = build(Tree::Path, "path");
    let a = WebAdapter::new(store);
    assert_eq!(lines(&a, "/sites/*::host"), ["it.example.org"]);
    assert_eq!(lines(&a, "//page @| count"), ["4"]);
    assert_eq!(lines(&a, "//page::path"), ["wiki/Guerra_degli_emù", "wiki/Italia", "wiki/Lazio", "wiki/Roma"]);
    assert_eq!(lines(&a, "//dir::path"), ["wiki"]);
    // Links: through the redirect (`./Rome` → Roma), percent-decoded
    // (`Guerra_degli_em%C3%B9`), fragment dropped; the missing page
    // is a redlink.
    assert_eq!(lines(&a, "/sites/it.example.org/pages/wiki/Roma->link::title"), ["Lazio", "Italia", "Guerra degli emù"]);
    assert_eq!(lines(&a, "/sites/it.example.org/pages/wiki/Lazio->link::title"), ["Roma"]);
    assert_eq!(lines(&a, "/sites/it.example.org/pages/wiki/Roma<-link::title"), ["Guerra degli emù", "Italia", "Lazio"]);
    // A template-written link counts as a link and is flagged;
    // the analytics can leave it out.
    assert_eq!(lines(&a, "/sites/it.example.org/pages/wiki/Italia->link::title"), ["Roma", "Lazio", "Guerra degli emù"]);
    assert_eq!(lines(&a, "/sites/it.example.org/pages/wiki/Guerra_degli_emù::in_degree"), ["2"]);
    {
        let store = a.store();
        quarb_web::db::analyze::analyze(store, SiteId(1), true).unwrap();
    }
    let b = WebAdapter::new(SqliteStore::open(&std::env::temp_dir().join(format!("quarb-web-wiki-{}-path.db", std::process::id()))).unwrap());
    assert_eq!(lines(&b, "/sites/it.example.org/pages/wiki/Guerra_degli_emù::in_degree"), ["1"]);
    assert_eq!(lines(&a, "/sites/it.example.org/pages/wiki/Roma | %(::in_degree; ::out_degree; ::mutual_degree; ::redlinks)"), ["%(in_degree = 3; out_degree = 3; mutual_degree = 3; redlinks = 1)"]);
    assert_eq!(lines(&a, "//redirect::title"), ["Rome"]);
    assert_eq!(lines(&a, "//redirect->redirect::title"), ["Roma"]);
    // Declared identity and the categories container.
    assert_eq!(lines(&a, "/sites/it.example.org/pages/wiki/Roma::category"), ["Categoria:Città"]);
    assert_eq!(lines(&a, "/sites/it.example.org/pages/wiki/Roma->category::title"), ["Categoria:Città", "Categoria:Capitali"]);
    assert_eq!(lines(&a, "//category @| count"), ["6"]);
    assert_eq!(lines(&a, "/sites/it.example.org/categories/\"Categoria:Geografia\"->category::title"), ["Categoria:Enciclopedia"]);
    assert_eq!(lines(&a, "/sites/it.example.org/categories/\"Categoria:Città\"<-page::title"), ["Roma"]);
    assert_eq!(lines(&a, "/sites/it.example.org/pages/wiki/Roma::description"), ["Roma, in breve."]);
    assert_eq!(lines(&a, "/sites/it.example.org/pages/wiki/Roma<tag:wikidata-Q1> @| count"), ["1"]);
    // The graft and its links, with the section.
    assert_eq!(lines(&a, "/sites/it.example.org/pages/wiki/Roma//section::lemma"), ["Storia"]);
    assert_eq!(lines(&a, "/sites/it.example.org/pages/wiki/Italia//paragraph//ref-->"), ["/sites/it.example.org/pages/wiki/Roma!/section", "/sites/it.example.org/pages/wiki/Lazio"]);
    // The text prefilter: the plain text carries the prose.
    let p = quarb_web::plan::plan("//page[//paragraph[:: *= \"mitragliatrice Lewis\"]]::title", quarb_web::db::Dialect::Sqlite, false);
    let keys = a.store().keys_where(&p.where_sql, &p.params);
    assert_eq!(keys.len(), 1);
    let total: f64 = [lines(&a, "//page::pagerank"), lines(&a, "//category::pagerank")].concat().iter().map(|s| s.parse::<f64>().unwrap()).sum();
    assert!((total - 1.0).abs() < 1e-9, "{total}");
}

#[test]
fn the_category_tree() {
    let store = build(Tree::Category, "cat");
    let a = WebAdapter::new(store);
    assert_eq!(lines(&a, "//page @| count"), ["4"]);
    assert_eq!(lines(&a, "//dir @| count"), ["0"]);
    // Roma's shallowest declared category is Città (depth 3), so it
    // hangs there, not under Capitali (depth 4).
    assert_eq!(
        lines(&a, "/sites/it.example.org/pages/\"Categoria:Enciclopedia\"/\"Categoria:Geografia\"/\"Categoria:Città\"/Roma::title"),
        ["Roma"]
    );
    assert_eq!(lines(&a, "/sites/it.example.org/pages/\"Categoria:Enciclopedia\"/\"Categoria:Geografia\"/*::title"), ["Categoria:Città", "Categoria:Regioni", "Italia", "Storia"]);
    // An unreached category (Guerre) is a root beside the tree,
    // with its page beneath.
    assert_eq!(lines(&a, "/sites/it.example.org/pages/*::title"), ["Categoria:Enciclopedia", "Categoria:Guerre"]);
    assert_eq!(lines(&a, "/sites/it.example.org/pages/\"Categoria:Guerre\"/*::title"), ["Guerra degli emù"]);
    // Ascent and the DAG beside the tree.
    assert_eq!(lines(&a, "//page[::title = \"Roma\"]\\\\<category>::title"), ["Categoria:Città", "Categoria:Geografia", "Categoria:Enciclopedia"]);
    assert_eq!(lines(&a, "//page[::title = \"Roma\"]->category::title"), ["Categoria:Città", "Categoria:Capitali"]);
    assert_eq!(lines(&a, "//category[::title = \"Categoria:Capitali\"]<-page::title"), ["Roma"]);
    // A category page links like any page, in either direction.
    assert_eq!(lines(&a, "//category[::title = \"Categoria:Geografia\"]->link::title"), ["Categoria:Città"]);
    assert_eq!(lines(&a, "//category[::title = \"Categoria:Città\"]<-link::title"), ["Categoria:Geografia"]);
    assert_eq!(lines(&a, "//category[::title = \"Categoria:Città\"] | %(in = ::in_degree; n = (<-link @| count))"), ["%(in = 1; n = 1)"]);
    assert_eq!(lines(&a, "/sites/*::::hierarchy"), ["categories"]);
    // The prefilter under a category prefix: candidates by column,
    // the walk narrowed to them and their ancestors.
    let p = quarb_web::plan::plan("/sites/it.example.org/pages/\"Categoria:Enciclopedia\"//category[//paragraph[:: *= \"Luoghi\"]]::title", quarb_web::db::Dialect::Sqlite, false);
    assert_eq!(p.rung, quarb_web::plan::Rung::Prefilter, "{}", p.reason);
    let keys = a.store().keys_where(&p.where_sql, &p.params);
    assert_eq!(keys.len(), 1);
    // Below a prefix a column test walks (Scan); the same
    // candidate set, built by hand, still gives the walk's answer.
    let p = quarb_web::plan::plan("/sites/it.example.org/pages/\"Categoria:Enciclopedia\"//category[::depth = 3]::title", quarb_web::db::Dialect::Sqlite, false);
    assert_eq!(p.rung, quarb_web::plan::Rung::Scan, "{}", p.reason);
    let keys = [keys, a.store().keys_where("p.kind = 'category' AND p.depth = 3", &[])].concat();
    let store = SqliteStore::open(&std::env::temp_dir().join(format!("quarb-web-wiki-{}-cat.db", std::process::id()))).unwrap();
    let scoped = WebAdapter::new(store).with_scope(keys);
    assert_eq!(lines(&scoped, "/sites/it.example.org/pages/\"Categoria:Enciclopedia\"//category[::depth = 3]::title"), ["Categoria:Città", "Categoria:Regioni"]);
    // A category page carries its document beneath its rows: the
    // child axis reads its own text, the descendant axis the
    // subtree's.
    assert_eq!(lines(&a, "/sites/it.example.org/pages/\"Categoria:Enciclopedia\"/section//paragraph::"), ["La radice."]);
    assert_eq!(lines(&a, "//category[/section//paragraph[:: *= \"Luoghi\"]]::title"), ["Categoria:Geografia"]);
    assert_eq!(lines(&a, "//category[//paragraph[:: *= \"Luoghi\"]]::title"), ["Categoria:Enciclopedia", "Categoria:Geografia"]);
    // Enumeration in tree order through the name index.
    assert_eq!(lines(&a, "//category::title"), ["Categoria:Enciclopedia", "Categoria:Geografia", "Categoria:Città", "Categoria:Capitali", "Categoria:Regioni", "Categoria:Guerre"]);
}
