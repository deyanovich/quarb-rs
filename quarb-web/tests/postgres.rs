//! The PostgreSQL store against the memory store and the planner,
//! on a live server: set QUARB_WEB_PG to a URL prefix such as
//! `postgres://postgres@127.0.0.1:15433/` (databases qweb_parity,
//! qweb_www must exist). Skipped otherwise.
#![cfg(feature = "postgres")]

mod common;
use common::site;
use quarb::QueryResult;
use quarb_web::db::postgres::PostgresStore;
use quarb_web::db::{analyze, Dialect, SqlStore};
use quarb_web::plan::{plan, Rung};
use quarb_web::{WebAdapter, WebStore};

fn url(db: &str) -> Option<String> {
    std::env::var("QUARB_WEB_PG").ok().map(|p| format!("{p}{db}"))
}

fn lines<S: WebStore>(a: &WebAdapter<S>, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryResult::Values(v) => v.into_iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(n) => n.into_iter().map(|n| a.locator(n)).collect(),
    }
}

const CORPUS: &[&str] = &[
    "//page @| count",
    "//page::path",
    "//dir::path",
    "//tag::title",
    "//page<tag:JSON>::title",
    "//page<category:Guides> @| count",
    "//page<orphan>::path",
    "//page[::in_degree > 1 && ::out_degree >= 1]::path",
    "//page[::title *= \"SQL\"]::path",
    "//page[::title == \"Quarb\"*]::path",
    "//page[::title == (/for (jq|SQL)/)]::path",
    "//page[!<-link]::path",
    "//page[(<-link @| count) > 1]::path",
    "//page[//paragraph[:: *= \"jq guide\"]]::path",
    "//page[//section[::lemma = \"Joins\"]]::title",
    "//page[::title *= \"users\"]->link::title",
    "//page[::title *= \"users\"]<-link | link",
    "//page[::in_degree > 0] @| top(2; ::pagerank) | %(::title; ::in_degree)",
    "//page @| top(3; ::depth) | %(::path)",
    "//page @| bottom(2; ::in_degree) | %(::path)",
    "/sites/example.org/pages//page[::title *= \"SQL\"]::path",
    "/sites/example.org/pages/guides//page::path",
    "//page[::title *= \"SQL\"]//section::lemma",
    "//page[::title *= \"SQL\"]//ref-->::title",
    "//page[\\\\<dir>::path = \"guides\"]::path",
    "//page[::title *= \"jq\"]->tag<-page::path",
    "//page[::title != \"Home\"] @| count",
    "//page[::pagerank > 0.2]::title",
    "/sites/example.org/tags/JSON<-page::title",
    "/sites/example.org/pages/index.html//paragraph//ref-->",
];

#[test]
fn the_postgres_store_agrees_with_the_memory_store_and_the_planner() {
    let Some(u) = url("qweb_parity") else {
        eprintln!("QUARB_WEB_PG unset: skipped");
        return;
    };
    let mem = site();
    PostgresStore::drop_all(&u).unwrap();
    let pg = PostgresStore::create(&u, mem.store()).unwrap();
    let db = WebAdapter::new(pg);
    for q in CORPUS {
        assert_eq!(lines(&mem, q), lines(&db, q), "{q}");
    }
    // Every rung.
    for q in CORPUS {
        let expected = lines(&mem, q);
        let p = plan(q, Dialect::Postgres, false);
        let store = PostgresStore::open(&u).unwrap();
        match p.rung {
            Rung::Full => {
                let n = store.count_where(&p.where_sql, &p.params);
                assert_eq!(expected, vec![n.to_string()], "{q}: full ({})", p.where_sql);
            }
            Rung::Prefilter => {
                assert!(store.estimate_where(&p.where_sql, &p.params).is_some());
                let keys = match &p.limit {
                    Some(l) => store.keys_where_top(&p.where_sql, &p.params, &l.column, l.descending, l.n),
                    None => store.keys_where(&p.where_sql, &p.params),
                };
                let scoped = WebAdapter::new(store).with_scope(keys);
                assert_eq!(expected, lines(&scoped, q), "{q}: prefilter ({})", p.where_sql);
            }
            Rung::Scan => {}
        }
    }
    // The analytics pass recomputes what the memory store built,
    // and the index pass runs.
    let mut pg = PostgresStore::open(&u).unwrap();
    pg.execute("UPDATE pages SET in_degree = 0, out_degree = 0, mutual_degree = 0, pagerank = 0, redlinks = 0").unwrap();
    analyze::analyze(&pg, quarb_web::SiteId(1), false).unwrap();
    pg.reload().unwrap();
    for (k, _) in mem.store().descendants_of_kind(quarb_web::SiteId(1), quarb_web::Container::Pages, None, quarb_web::PageKind::Page) {
        let a = mem.store().row(k).unwrap().analytics;
        let b = pg.row(k).unwrap().analytics;
        assert_eq!((a.in_degree, a.out_degree, a.mutual_degree, a.redlinks), (b.in_degree, b.out_degree, b.mutual_degree, b.redlinks));
        assert!((a.pagerank - b.pagerank).abs() < 1e-9);
    }
    let done = analyze::index(&pg).unwrap();
    assert!(done.iter().any(|s| s.contains("gin_trgm_ops")));
    // The text prefilter after the index pass still agrees.
    let p = plan("//page[//paragraph[:: *= \"jq guide\"]]::path", Dialect::Postgres, false);
    let keys = pg.keys_where(&p.where_sql, &p.params);
    assert_eq!(keys.len(), 2, "{}", p.where_sql);
}
