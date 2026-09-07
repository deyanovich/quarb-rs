//! The SQL analytics pass gives the in-memory store's numbers:
//! degrees, mutual links, redlinks, PageRank.
#![cfg(feature = "sqlite")]

mod common;
use common::site;
use quarb_web::db::sqlite::SqliteStore;
use quarb_web::db::{analyze, SqlStore};
use quarb_web::{Container, PageKind, SiteId, WebStore};

#[test]
fn the_sql_pass_matches_the_memory_pass() {
    let mem = site();
    let path = std::env::temp_dir().join(format!("quarb-web-analytics-{}.db", std::process::id()));
    let mut db = SqliteStore::create(&path, mem.store()).unwrap();
    db.execute("UPDATE pages SET in_degree = 0, out_degree = 0, mutual_degree = 0, pagerank = 0, redlinks = 0").unwrap();
    db.execute("UPDATE sites SET page_count = 0, link_count = 0").unwrap();
    analyze::analyze(&db, SiteId(1), false).unwrap();
    db.reload().unwrap();
    let pages = mem.store().descendants_of_kind(SiteId(1), Container::Pages, None, PageKind::Page);
    assert!(!pages.is_empty());
    for (k, _) in pages {
        let a = mem.store().row(k).unwrap();
        let b = db.row(k).unwrap();
        assert_eq!(a.analytics.in_degree, b.analytics.in_degree, "{}", a.path);
        assert_eq!(a.analytics.out_degree, b.analytics.out_degree, "{}", a.path);
        assert_eq!(a.analytics.mutual_degree, b.analytics.mutual_degree, "{}", a.path);
        assert_eq!(a.analytics.redlinks, b.analytics.redlinks, "{}", a.path);
        assert!((a.analytics.pagerank - b.analytics.pagerank).abs() < 1e-9, "{}: {} vs {}", a.path, a.analytics.pagerank, b.analytics.pagerank);
    }
    assert_eq!(db.catalog().sites[0].page_count, mem.store().catalog().sites[0].page_count);
    assert_eq!(db.catalog().sites[0].link_count, mem.store().catalog().sites[0].link_count);
}
