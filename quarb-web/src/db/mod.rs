//! The index-backed stores: one schema in two dialects, an export
//! that fills either from any [`WebStore`], and the little the
//! planner needs from a store beyond the contract.

use crate::contract::*;
use quarb::Value;

#[cfg(feature = "sqlite")]
pub mod sqlite;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod analyze;

/// The SQL dialect a store speaks — the one construct that differs
/// is the parameter marker and the substring test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    Postgres,
}

/// A statement parameter.
#[derive(Clone, Debug, PartialEq)]
pub enum Param {
    Str(String),
    Int(i64),
    Float(f64),
}

/// What the planner asks of a store beyond the level's contract:
/// its dialect, a key list for a WHERE clause over `pages p`, and
/// a count for the same.
pub trait SqlStore: WebStore {
    fn dialect(&self) -> Dialect;
    /// `SELECT p.id FROM pages p WHERE <where> ORDER BY p.tree_rank`.
    fn keys_where(&self, where_sql: &str, params: &[Param]) -> Vec<PageKey>;
    /// `SELECT count(*) FROM pages p WHERE <where>`.
    fn count_where(&self, where_sql: &str, params: &[Param]) -> i64;
    /// The first `n` keys by `column` (descending or not), ties in
    /// document order — the engine's stable sort, cut by the store.
    fn keys_where_top(&self, where_sql: &str, params: &[Param], column: &str, descending: bool, n: i64) -> Vec<PageKey> {
        let _ = (where_sql, params, column, descending, n);
        Vec::new()
    }
    /// The planner's row estimate for a WHERE clause, as the
    /// engine's EXPLAIN reports it, for `--explain`.
    fn estimate_where(&self, _where_sql: &str, _params: &[Param]) -> Option<String> {
        None
    }
    /// Run a statement (the analytics and index passes).
    fn execute(&self, sql: &str) -> Result<u64, String>;
    /// Every row of a container, for the tree pass.
    fn tree_nodes(&self, site: SiteId, container: Container) -> Vec<TreeNode>;
    /// Write (id, parent, depth, rank) for the rows of a tree.
    fn set_tree(&self, rows: &[(i64, Option<i64>, i64, i64)]) -> Result<(), String>;
    /// Refresh the catalog after a pass changed the tables.
    fn reload(&mut self) -> Result<(), String>;
    /// Every membership row of a site as (page, term, pos), for
    /// the category tree pass.
    fn term_edges(&self, site: SiteId) -> Vec<(i64, i64, i64)>;
    /// Every resolved link of a site as (from, to), template links
    /// excluded when asked — the edge list the analytics run on.
    fn link_edges(&self, site: SiteId, exclude_templates: bool) -> Vec<(PageKey, PageKey)>;
    /// Redlinks per page (links the site marks as leading nowhere).
    fn red_counts(&self, site: SiteId) -> Vec<(PageKey, u32)>;
    /// Write the analytics columns in bulk.
    fn set_analytics(&self, rows: &[(PageKey, Analytics)]) -> Result<(), String>;
    /// Move rows between containers (the category tree puts the
    /// categories into the pages container).
    fn set_container(&self, site: SiteId, kind: PageKind, container: Container) -> Result<u64, String> {
        self.execute(&format!(
            "UPDATE pages SET container = '{}' WHERE site_id = {} AND kind = '{}'",
            container.name(),
            site.0,
            kind.name()
        ))
    }
}

/// The schema, dialect-neutral but for the integer and float
/// spellings substituted per dialect.
pub fn ddl(dialect: Dialect) -> Vec<String> {
    let (big, dbl) = match dialect {
        Dialect::Sqlite => ("INTEGER", "REAL"),
        Dialect::Postgres => ("BIGINT", "DOUBLE PRECISION"),
    };
    let stmts = [
        "CREATE TABLE IF NOT EXISTS sites (
            id INTEGER PRIMARY KEY,
            host TEXT NOT NULL,
            base_url TEXT NOT NULL,
            snapshot TEXT,
            hierarchy TEXT NOT NULL,
            lowering TEXT NOT NULL,
            page_count {BIG} NOT NULL DEFAULT 0,
            link_count {BIG} NOT NULL DEFAULT 0
        )",
        "CREATE TABLE IF NOT EXISTS pages (
            id {BIG} PRIMARY KEY,
            site_id {BIG} NOT NULL REFERENCES sites(id),
            container TEXT NOT NULL,
            parent_id {BIG} REFERENCES pages(id),
            kind TEXT NOT NULL,
            name TEXT NOT NULL,
            path TEXT NOT NULL,
            title TEXT,
            url TEXT,
            depth {BIG} NOT NULL,
            tree_rank {BIG} NOT NULL,
            category TEXT,
            categories TEXT NOT NULL DEFAULT '[]',
            tags TEXT NOT NULL DEFAULT '[]',
            description TEXT,
            modified_secs {BIG},
            modified_off {BIG},
            published_secs {BIG},
            published_off {BIG},
            redirect_to {BIG} REFERENCES pages(id),
            in_degree {BIG} NOT NULL DEFAULT 0,
            out_degree {BIG} NOT NULL DEFAULT 0,
            mutual_degree {BIG} NOT NULL DEFAULT 0,
            pagerank {DBL} NOT NULL DEFAULT 0,
            redlinks {BIG} NOT NULL DEFAULT 0
        )",
        "CREATE INDEX IF NOT EXISTS pages_path ON pages (site_id, container, path)",
        "CREATE INDEX IF NOT EXISTS pages_tree ON pages (site_id, container, parent_id, tree_rank)",
        "CREATE INDEX IF NOT EXISTS pages_named ON pages (site_id, container, parent_id, name)",
        "CREATE INDEX IF NOT EXISTS pages_rank ON pages (site_id, container, tree_rank)",
        "CREATE INDEX IF NOT EXISTS pages_kind ON pages (site_id, container, kind, tree_rank)",
        "CREATE INDEX IF NOT EXISTS pages_url ON pages (url)",
        "CREATE INDEX IF NOT EXISTS pages_title ON pages (site_id, title)",
        "CREATE INDEX IF NOT EXISTS pages_in_degree ON pages (site_id, in_degree)",
        "CREATE INDEX IF NOT EXISTS pages_out_degree ON pages (site_id, out_degree)",
        "CREATE INDEX IF NOT EXISTS pages_pagerank ON pages (site_id, pagerank)",
        "CREATE TABLE IF NOT EXISTS page_html (
            page_id {BIG} PRIMARY KEY REFERENCES pages(id),
            html TEXT NOT NULL
        )",
        "CREATE TABLE IF NOT EXISTS page_text (
            page_id {BIG} PRIMARY KEY REFERENCES pages(id),
            plain TEXT NOT NULL
        )",
        "CREATE TABLE IF NOT EXISTS links (
            from_id {BIG} NOT NULL REFERENCES pages(id),
            pos {BIG} NOT NULL,
            to_id {BIG} REFERENCES pages(id),
            to_url TEXT NOT NULL,
            anchor TEXT,
            section TEXT,
            kind TEXT NOT NULL DEFAULT 'link',
            via_template {BIG} NOT NULL DEFAULT 0,
            PRIMARY KEY (from_id, pos)
        )",
        "CREATE INDEX IF NOT EXISTS links_to ON links (to_id, from_id)",
        "CREATE TABLE IF NOT EXISTS page_terms (
            page_id {BIG} NOT NULL REFERENCES pages(id),
            term_id {BIG} NOT NULL REFERENCES pages(id),
            pos {BIG} NOT NULL,
            PRIMARY KEY (page_id, pos),
            UNIQUE (page_id, term_id)
        )",
        "CREATE INDEX IF NOT EXISTS terms_members ON page_terms (term_id, page_id)",
    ];
    let fk = match dialect {
        Dialect::Sqlite => "REFERENCES pages(id)",
        Dialect::Postgres => "REFERENCES pages(id) DEFERRABLE INITIALLY DEFERRED",
    };
    stmts
        .iter()
        .map(|s| s.replace("{BIG}", big).replace("{DBL}", dbl).replace("REFERENCES pages(id)", fk))
        .collect()
}

/// What the tree pass needs of a row.
#[derive(Clone, Debug)]
pub struct TreeNode {
    pub id: i64,
    pub parent: Option<i64>,
    pub name: String,
    pub kind: String,
    pub redirect_to: Option<i64>,
}

/// A row as the schema stores it, for the export.
pub struct PageRecord<'a> {
    pub row: &'a LightRow,
    pub html: Option<&'a str>,
    pub plain: Option<&'a str>,
}

/// Where an export writes: any store that can be filled.
pub trait Sink {
    fn begin(&mut self) -> Result<(), String>;
    fn site(&mut self, site: &SiteRow, lowering: &str) -> Result<(), String>;
    fn page(&mut self, rec: &PageRecord) -> Result<(), String>;
    fn link(&mut self, from: PageKey, pos: u32, link: &LinkRow) -> Result<(), String>;
    fn term(&mut self, page: PageKey, term: PageKey, pos: u32) -> Result<(), String>;
    fn finish(&mut self) -> Result<(), String>;
}

/// Copy a store into a sink: sites, every row of every container
/// in tree order (parents before children), each page's html and
/// plain text (the text lowering the grafts use, so a text
/// prefilter is a superset by construction), links, memberships.
pub fn export<S: WebStore>(store: &S, sink: &mut dyn Sink) -> Result<(), String> {
    sink.begin()?;
    let cat = store.catalog();
    for site in &cat.sites {
        sink.site(site, &cat.lowering)?;
    }
    let mut pages: Vec<PageKey> = Vec::new();
    for site in &cat.sites {
        for c in Container::ALL {
            let mut stack: Vec<Option<PageKey>> = vec![None];
            while let Some(parent) = stack.pop() {
                // Children in rank order; a stack reverses, so
                // push reversed to keep parents before children
                // and siblings in order.
                let kids = store.children(site.id, c, parent);
                for k in kids.iter().rev() {
                    stack.push(Some(*k));
                }
                if let Some(k) = parent {
                    let Some(row) = store.row(k) else { continue };
                    let html = store.html(k);
                    let plain = html.as_deref().map(|h| quarb_text_html::parse(h).plain_text());
                    sink.page(&PageRecord {
                        row: &row,
                        html: html.as_deref(),
                        plain: plain.as_deref(),
                    })?;
                    if row.kind == PageKind::Page || row.kind == PageKind::Redirect {
                        pages.push(k);
                    }
                }
            }
        }
    }
    for &k in &pages {
        for (pos, l) in store.link_rows(k).iter().enumerate() {
            sink.link(k, pos as u32, l)?;
        }
        let mut pos = 0u32;
        for t in store.related(k, LinkKind::Category, LinkDir::Out) {
            sink.term(k, t, pos)?;
            pos += 1;
        }
        for t in store.related(k, LinkKind::Tag, LinkDir::Out) {
            sink.term(k, t, pos)?;
            pos += 1;
        }
    }
    sink.finish()
}

/// The instant a stored (secs, offset) pair denotes.
pub fn instant(secs: Option<i64>, off: Option<i64>) -> Option<Value> {
    secs.map(|s| Value::Instant {
        secs: s,
        nanos: 0,
        offset_min: off.map(|o| o as i16),
    })
}

/// The stored form of an instant value.
pub fn instant_cols(v: &Option<Value>) -> (Option<i64>, Option<i64>) {
    match v {
        Some(Value::Instant { secs, offset_min, .. }) => (Some(*secs), offset_min.map(|o| o as i64)),
        _ => (None, None),
    }
}

pub fn container_name(c: Container) -> &'static str {
    c.name()
}

pub fn container_of(name: &str) -> Container {
    match name {
        "categories" => Container::Categories,
        "tags" => Container::Tags,
        _ => Container::Pages,
    }
}

pub fn kind_of(name: &str) -> PageKind {
    match name {
        "dir" => PageKind::Dir,
        "category" => PageKind::Category,
        "tag" => PageKind::Tag,
        "redirect" => PageKind::Redirect,
        _ => PageKind::Page,
    }
}

pub fn hierarchy_name(h: Hierarchy) -> &'static str {
    match h {
        Hierarchy::Directories => "directories",
        Hierarchy::Categories => "categories",
    }
}

pub fn hierarchy_of(name: &str) -> Hierarchy {
    if name == "categories" { Hierarchy::Categories } else { Hierarchy::Directories }
}

/// A JSON list of strings, as the tag and category columns hold.
pub fn json_list(v: &[String]) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".to_string())
}

pub fn list_json(s: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(s).unwrap_or_default()
}
