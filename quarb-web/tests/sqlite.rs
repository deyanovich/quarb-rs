//! The SQLite store against the in-memory one, and the planner's
//! rungs against the scan: every query in the corpus must give
//! the same lines through all three routes.
#![cfg(feature = "sqlite")]

mod common;
use common::{files, page, site};
use quarb::{QueryResult, Value};
use quarb_web::db::sqlite::SqliteStore;
use quarb_web::db::{Dialect, SqlStore};
use quarb_web::plan::{Rung, plan};
use quarb_web::{MemoryStore, PageFile, SiteInput, WebAdapter, WebStore};

fn tmp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-web-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn lines<S: WebStore>(a: &WebAdapter<S>, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryResult::Values(v) => v.into_iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(n) => n.into_iter().map(|n| a.locator(n)).collect(),
    }
}

/// The corpus: every shape the planner knows, and the ones it
/// must refuse.
const CORPUS: &[&str] = &[
    "//page @| count",
    "//page::path",
    "//page<tag:JSON>::title",
    "//page<category:Guides> @| count",
    "//page<orphan>::path",
    "//page[::in_degree = 0]::title",
    "//page[::in_degree > 1 && ::out_degree >= 1]::path",
    "//page[::title *= \"SQL\"]::path",
    "//page[::title == \"Quarb\"*]::path",
    "//page[::title == (/for (jq|SQL)/)]::path",
    "//page[::category = \"Start\" || ::category = \"Guides\"] @| count",
    "//page[!<-link]::path",
    "//page[(<-link @| count) > 1]::path",
    "//page[//paragraph[:: *= \"jq guide\"]]::path",
    "//page[//section[::lemma = \"Joins\"]]::title",
    "//page[//paragraph[:: == (/jq|SQL/)]] @| count",
    "//page[::title *= \"users\"]->link::title",
    "//page[::title *= \"users\"]<-link | link",
    "//page[::in_degree > 0] @| top(2; ::pagerank) | %(::title; ::in_degree)",
    "//page @| top(3; ::depth) | %(::path)",
    "//page @| top(2; ::in_degree) | %(::path; ::in_degree)",
    "//page @| bottom(3; ::depth) | %(::path)",
    "//page<category:Guides> @| top(2; ::out_degree) | %(::path)",
    "//page @| top(0; ::depth) | %(::path)",
    "//page @| top(99; ::depth) @| count",
    "/sites/example.org/pages//page[::title *= \"SQL\"]::path",
    "/sites/*/pages//page[::depth = 2] @| count",
    "/sites/example.org/pages/guides//page::path",
    "/sites/example.org/pages/guides//page @| count",
    "/sites/example.org/pages/guides//page[::in_degree > 0] @| count",
    "/sites/example.org/pages/guides//page[//paragraph[:: *= \"jq\"]]::path",
    "//page[::title *= \"SQL\"]//section::lemma",
    "//page[::title *= \"SQL\"]//ref-->::title",
    "//page[\\\\<dir>::path = \"guides\"]::path",
    "//page[::title *= \"SQL\"]\\::path",
    "//page[::title *= \"SQL\"]\\/*::path",
    "//page[::title *= \"jq\"]->tag<-page::path",
    "//page[::description *= \"front\"] | link",
    "//page[::title != \"Home\"] @| count",
    "//page[::pagerank > 0.2]::title",
    "//page[::title *= \"%\"] @| count",
    "//page[::title *= \"_\"] @| count",
    "//page[//paragraph[:: *= \"100% sure\"]]::path",
    "//page[//paragraph[:: *= \"under_score\"]]::path",
    "//page[//paragraph[:: *= \"it's\"]]::path",
    "//page[//paragraph[:: *= \"back\\\\slash\"]]::path",
    "//page[//paragraph[:: *= \"hidden style\"]] @| count",
    "//page[//paragraph[:: *= \"boundary crossing\"]] @| count",
    "//page[//paragraph[:: *= \"split word\"]]::path",
    "//page[//paragraph[:: *= \"non breaking\"]]::path",
    "//page[//paragraph[:: *= \"Notes\"]]::path",
];

fn adversarial() -> Vec<PageFile> {
    let mut f = files();
    f.push(PageFile {
        path: "edge.html".into(),
        html: page(
            "Edge cases",
            "<meta name=\"keywords\" content=\"edge\"><style>.x { content: 'hidden style' }</style>",
            "<p>100% sure about under_score and it's a back\\slash.</p><p>boundary</p><p>crossing lines.</p><p>A split <b>word</b> here and a non&nbsp;breaking space.</p>",
        ),
    });
    f
}

fn stores(name: &str) -> (WebAdapter<MemoryStore>, std::path::PathBuf) {
    let mem = WebAdapter::new(MemoryStore::build(
        SiteInput {
            base_url: "https://example.org/".into(),
            snapshot: None,
        },
        adversarial(),
    ));
    let path = tmp(&format!("{name}.db"));
    SqliteStore::create(&path, mem.store()).unwrap();
    (mem, path)
}

#[test]
fn the_sqlite_store_answers_like_the_memory_store() {
    let (mem, path) = stores("answers");
    let db = WebAdapter::new(SqliteStore::open(&path).unwrap());
    for q in CORPUS {
        assert_eq!(lines(&mem, q), lines(&db, q), "{q}");
    }
    // The catalog round-trips.
    let c = db.store().catalog();
    assert_eq!(c.sites[0].host, "example.org");
    assert_eq!(c.sites[0].page_count, 6);
}

#[test]
fn every_rung_agrees_with_the_scan() {
    let (mem, path) = stores("rungs");
    let mut full = 0;
    let mut prefilter = 0;
    let mut scan = 0;
    for q in CORPUS {
        let expected = lines(&mem, q);
        let p = plan(q, Dialect::Sqlite, false);
        let store = SqliteStore::open(&path).unwrap();
        match p.rung {
            Rung::Full => {
                full += 1;
                let n = store.count_where(&p.where_sql, &p.params);
                assert_eq!(
                    expected,
                    vec![n.to_string()],
                    "{q}: full rung ({})",
                    p.where_sql
                );
            }
            Rung::Prefilter => {
                prefilter += 1;
                let keys = match &p.limit {
                    Some(l) => {
                        store.keys_where_top(&p.where_sql, &p.params, &l.column, l.descending, l.n)
                    }
                    None => store.keys_where(&p.where_sql, &p.params),
                };
                let scoped = WebAdapter::new(store).with_scope(keys);
                assert_eq!(
                    expected,
                    lines(&scoped, q),
                    "{q}: prefilter rung ({})",
                    p.where_sql
                );
            }
            Rung::Scan => {
                scan += 1;
            }
        }
    }
    assert!(full >= 3, "{full} full");
    assert!(prefilter >= 20, "{prefilter} prefilter");
    assert!(scan >= 3, "{scan} scan");
}

#[test]
fn the_planner_names_its_rungs() {
    let cases: &[(&str, Rung, &str)] = &[
        ("//page @| count", Rung::Full, "count"),
        ("//page<tag:JSON> @| count", Rung::Full, "count"),
        ("//page[::in_degree = 0] @| count", Rung::Full, "count"),
        ("//page[!<-link] @| count", Rung::Full, "count"),
        ("//page[::title *= \"x\"] @| count", Rung::Full, "count"),
        (
            "//page[::title == \"Q\"*] @| count",
            Rung::Prefilter,
            "verifies",
        ),
        (
            "//page[//paragraph[:: *= \"x\"]] @| count",
            Rung::Prefilter,
            "text-level",
        ),
        (
            "//page[::title *= \"x\"]::path",
            Rung::Prefilter,
            "candidates",
        ),
        (
            "/sites/example.org/pages/guides//page @| count",
            Rung::Scan,
            "subtree prefix",
        ),
        (
            "/sites/example.org/pages/guides//page[::in_degree > 0] @| count",
            Rung::Scan,
            "subtree prefix",
        ),
        (
            "/sites/example.org/pages/guides//page[//paragraph[:: *= \"jq\"]]::path",
            Rung::Prefilter,
            "text-level",
        ),
        (
            "/sites/example.org/pages//page @| count",
            Rung::Full,
            "count",
        ),
        (
            "//page @| top(3; ::depth) | %(::path)",
            Rung::Prefilter,
            "top",
        ),
        (
            "//page[::title == \"Q\"*] @| top(3; ::depth) | %(::path)",
            Rung::Prefilter,
            "verifies",
        ),
        (
            "//page @| top(3; ::title) | %(::path)",
            Rung::Scan,
            "no predicate",
        ),
        ("//page::path", Rung::Scan, "no predicate"),
        (
            "//page[::title *= \"x\"]\\/*::path",
            Rung::Scan,
            "re-enters",
        ),
        (
            "//page[::title *= \"x\"] <=> //page",
            Rung::Scan,
            "correlation",
        ),
        ("//page[2]::path", Rung::Scan, "positional"),
        (
            "//page[(^//page @| count) > 5]::path",
            Rung::Scan,
            "re-enters",
        ),
        (
            "//page[^/sites/*::host = \"x\"]::path",
            Rung::Scan,
            "re-enters",
        ),
        (
            "//page[::lemma = \"x\"]::path",
            Rung::Scan,
            "cannot express",
        ),
        ("//tag::title", Rung::Scan, "no //page"),
        ("//page[::title *= \"x\"]<--::path", Rung::Scan, "re-enters"),
    ];
    for (q, rung, reason) in cases {
        let p = plan(q, Dialect::Sqlite, false);
        assert_eq!(&p.rung, rung, "{q}: {}", p.reason);
        assert!(p.reason.contains(reason), "{q}: {}", p.reason);
    }
    // Under a model the planner stands down.
    assert_eq!(
        plan("//page @| count", Dialect::Sqlite, true).rung,
        Rung::Scan
    );
    // Postgres renders numbered parameters and LIKE.
    let p = plan(
        "//page[::title *= \"x\" && ::in_degree > 2]::path",
        Dialect::Postgres,
        false,
    );
    assert!(
        p.where_sql.contains("$1") && p.where_sql.contains("$2"),
        "{}",
        p.where_sql
    );
    assert!(p.where_sql.contains("LIKE"), "{}", p.where_sql);
}

#[test]
fn regex_factors_are_required_substrings() {
    use quarb_web::plan::regex_factors;
    assert_eq!(
        regex_factors("mitragliatric[ei] Lewis"),
        ["mitragliatric", " Lewis"]
    );
    assert_eq!(regex_factors("(?i)mitra"), Vec::<String>::new());
    assert_eq!(regex_factors("jq|SQL"), Vec::<String>::new());
    assert_eq!(
        regex_factors("^Quarb for (jq|SQL) users$"),
        ["Quarb for ", " users"]
    );
    assert_eq!(regex_factors("ab+cd"), ["cd"]);
}

#[test]
fn the_scope_leaves_crosslinks_and_grafts_alone() {
    let (mem, path) = stores("scope");
    let store = SqliteStore::open(&path).unwrap();
    // Scope to one page: its links reach outside the scope, its
    // graft is intact, and enumeration is narrowed.
    let keys = store.keys_where("p.kind = 'page' AND p.path = 'guides/jq.html'", &[]);
    let scoped = WebAdapter::new(store).with_scope(keys);
    assert_eq!(lines(&scoped, "//page::path"), ["guides/jq.html"]);
    assert_eq!(
        lines(&scoped, "//page->link::path"),
        lines(&mem, "/sites/example.org/pages/guides/jq->link::path")
    );
    assert_eq!(
        lines(&scoped, "//page<-link::path"),
        lines(&mem, "/sites/example.org/pages/guides/jq<-link::path")
    );
    assert_eq!(
        lines(&scoped, "//page//section::lemma"),
        lines(&mem, "/sites/example.org/pages/guides/jq//section::lemma")
    );
    assert_eq!(lines(&scoped, "//dir::path"), ["guides"]);
    assert_eq!(
        lines(&scoped, "//tag @| count"),
        lines(&mem, "//tag @| count")
    );
    let _ = Value::Null;
}
