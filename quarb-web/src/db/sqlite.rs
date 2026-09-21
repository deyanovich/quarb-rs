//! The SQLite store: a `site.db` file holding one or more sites —
//! the store every test runs on, and the export format an edge
//! database imports.

use super::*;
use rusqlite::{Connection, OptionalExtension, params_from_iter};
use std::cell::RefCell;
use std::path::Path;

pub struct SqliteStore {
    conn: RefCell<Connection>,
    catalog: Catalog,
}

fn to_sql(p: &Param) -> rusqlite::types::Value {
    match p {
        Param::Str(s) => rusqlite::types::Value::Text(s.clone()),
        Param::Int(i) => rusqlite::types::Value::Integer(*i),
        Param::Float(f) => rusqlite::types::Value::Real(*f),
    }
}

const ROW_COLS: &str = "id, site_id, container, parent_id, kind, name, path, title, url, depth, tree_rank, \
    category, categories, tags, description, modified_secs, modified_off, published_secs, published_off, \
    redirect_to, in_degree, out_degree, mutual_degree, pagerank, redlinks";

fn row_from(r: &rusqlite::Row) -> rusqlite::Result<LightRow> {
    Ok(LightRow {
        key: PageKey(r.get::<_, i64>(0)? as u64),
        site: SiteId(r.get::<_, i64>(1)? as u32),
        container: container_of(&r.get::<_, String>(2)?),
        parent: r.get::<_, Option<i64>>(3)?.map(|p| PageKey(p as u64)),
        kind: kind_of(&r.get::<_, String>(4)?),
        name: r.get(5)?,
        path: r.get(6)?,
        title: r.get(7)?,
        url: r.get(8)?,
        depth: r.get::<_, i64>(9)? as u32,
        tree_rank: r.get::<_, i64>(10)? as u64,
        category: r.get(11)?,
        categories: list_json(&r.get::<_, String>(12)?),
        tags: list_json(&r.get::<_, String>(13)?),
        description: r.get(14)?,
        modified: instant(r.get(15)?, r.get(16)?),
        published: instant(r.get(17)?, r.get(18)?),
        redirect_to: r.get::<_, Option<i64>>(19)?.map(|p| PageKey(p as u64)),
        analytics: Analytics {
            in_degree: r.get::<_, i64>(20)? as u32,
            out_degree: r.get::<_, i64>(21)? as u32,
            mutual_degree: r.get::<_, i64>(22)? as u32,
            pagerank: r.get(23)?,
            redlinks: r.get::<_, i64>(24)? as u32,
        },
    })
}

impl SqliteStore {
    /// Open an existing store.
    pub fn open(path: &Path) -> Result<SqliteStore, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        let catalog = Self::read_catalog(&conn)?;
        Ok(SqliteStore {
            conn: RefCell::new(conn),
            catalog,
        })
    }

    /// Create a store at `path` (replacing any file there) and fill
    /// it from `store`.
    pub fn create<S: WebStore>(path: &Path, store: &S) -> Result<SqliteStore, String> {
        if path.exists() {
            std::fs::remove_file(path).map_err(|e| e.to_string())?;
        }
        let mut conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = OFF;")
            .map_err(|e| e.to_string())?;
        for stmt in ddl(Dialect::Sqlite) {
            conn.execute(&stmt, [])
                .map_err(|e| format!("{e}: {stmt}"))?;
        }
        {
            let mut w = Writer { conn: &mut conn };
            export(store, &mut w)?;
        }
        let catalog = Self::read_catalog(&conn)?;
        Ok(SqliteStore {
            conn: RefCell::new(conn),
            catalog,
        })
    }

    /// An empty store with the schema, for an ingest to fill
    /// through [`SqliteStore::sink`].
    pub fn empty(path: &Path) -> Result<SqliteStore, String> {
        if path.exists() {
            std::fs::remove_file(path).map_err(|e| e.to_string())?;
        }
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = OFF;")
            .map_err(|e| e.to_string())?;
        for stmt in ddl(Dialect::Sqlite) {
            conn.execute(&stmt, [])
                .map_err(|e| format!("{e}: {stmt}"))?;
        }
        Ok(SqliteStore {
            conn: RefCell::new(conn),
            catalog: Catalog {
                sites: Vec::new(),
                lowering: String::new(),
            },
        })
    }

    /// Write rows into this store (one transaction from `begin` to
    /// `finish`).
    pub fn sink(&mut self) -> impl Sink + '_ {
        Writer {
            conn: self.conn.get_mut(),
        }
    }

    fn read_catalog(conn: &Connection) -> Result<Catalog, String> {
        let mut st = conn
            .prepare("SELECT id, host, base_url, snapshot, hierarchy, lowering, page_count, link_count FROM sites ORDER BY id")
            .map_err(|e| e.to_string())?;
        let mut lowering = String::new();
        let sites = st
            .query_map([], |r| {
                lowering = r.get(5)?;
                Ok(SiteRow {
                    id: SiteId(r.get::<_, i64>(0)? as u32),
                    host: r.get(1)?,
                    base_url: r.get(2)?,
                    snapshot: r.get(3)?,
                    hierarchy: hierarchy_of(&r.get::<_, String>(4)?),
                    page_count: r.get::<_, i64>(6)? as u64,
                    link_count: r.get::<_, i64>(7)? as u64,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(Catalog { sites, lowering })
    }

    fn keys(&self, sql: &str, params: &[Param]) -> Vec<PageKey> {
        let conn = self.conn.borrow();
        let Ok(mut st) = conn.prepare_cached(sql) else {
            return Vec::new();
        };
        st.query_map(params_from_iter(params.iter().map(to_sql)), |r| {
            r.get::<_, i64>(0)
        })
        .map(|rows| {
            rows.filter_map(|r| r.ok())
                .map(|i| PageKey(i as u64))
                .collect()
        })
        .unwrap_or_default()
    }
}

impl WebStore for SqliteStore {
    fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    fn row(&self, key: PageKey) -> Option<LightRow> {
        let conn = self.conn.borrow();
        let mut st = conn
            .prepare_cached(&format!("SELECT {ROW_COLS} FROM pages WHERE id = ?1"))
            .ok()?;
        st.query_row([key.0 as i64], row_from)
            .optional()
            .ok()
            .flatten()
    }

    fn children(
        &self,
        site: SiteId,
        container: Container,
        parent: Option<PageKey>,
    ) -> Vec<PageKey> {
        match parent {
            Some(p) => self.keys(
                "SELECT id FROM pages WHERE site_id = ?1 AND container = ?2 AND parent_id = ?3 ORDER BY tree_rank",
                &[Param::Int(site.0 as i64), Param::Str(container.name().into()), Param::Int(p.0 as i64)],
            ),
            None => self.keys(
                "SELECT id FROM pages WHERE site_id = ?1 AND container = ?2 AND parent_id IS NULL ORDER BY tree_rank",
                &[Param::Int(site.0 as i64), Param::Str(container.name().into())],
            ),
        }
    }

    fn children_named(
        &self,
        site: SiteId,
        container: Container,
        parent: Option<PageKey>,
        name: &str,
    ) -> Vec<PageKey> {
        match parent {
            Some(p) => self.keys(
                "SELECT id FROM pages WHERE site_id = ?1 AND container = ?2 AND parent_id = ?3 AND name = ?4 ORDER BY tree_rank",
                &[Param::Int(site.0 as i64), Param::Str(container.name().into()), Param::Int(p.0 as i64), Param::Str(name.into())],
            ),
            None => self.keys(
                "SELECT id FROM pages WHERE site_id = ?1 AND container = ?2 AND parent_id IS NULL AND name = ?3 ORDER BY tree_rank",
                &[Param::Int(site.0 as i64), Param::Str(container.name().into()), Param::Str(name.into())],
            ),
        }
    }

    fn related(&self, key: PageKey, kind: LinkKind, dir: LinkDir) -> Vec<PageKey> {
        let k = Param::Int(key.0 as i64);
        match (kind, dir) {
            (LinkKind::Link, LinkDir::Out) => self.keys(
                "SELECT to_id FROM links WHERE from_id = ?1 AND to_id IS NOT NULL ORDER BY pos",
                &[k],
            ),
            (LinkKind::Link, LinkDir::In) => self.keys(
                "SELECT l.from_id FROM links l JOIN pages p ON p.id = l.from_id WHERE l.to_id = ?1 ORDER BY p.tree_rank",
                &[k],
            ),
            (LinkKind::Category, LinkDir::Out) => self.keys(
                "SELECT t.term_id FROM page_terms t JOIN pages r ON r.id = t.term_id WHERE t.page_id = ?1 AND r.kind = 'category' ORDER BY t.pos",
                &[k],
            ),
            (LinkKind::Tag, LinkDir::Out) => self.keys(
                "SELECT t.term_id FROM page_terms t JOIN pages r ON r.id = t.term_id WHERE t.page_id = ?1 AND r.kind = 'tag' ORDER BY t.pos",
                &[k],
            ),
            (LinkKind::Category | LinkKind::Tag, LinkDir::In) => self.keys(
                "SELECT t.page_id FROM page_terms t JOIN pages p ON p.id = t.page_id WHERE t.term_id = ?1 ORDER BY p.tree_rank",
                &[k],
            ),
            (LinkKind::Redirect, LinkDir::Out) => self.keys("SELECT redirect_to FROM pages WHERE id = ?1 AND redirect_to IS NOT NULL", &[k]),
            (LinkKind::Redirect, LinkDir::In) => self.keys("SELECT id FROM pages WHERE redirect_to = ?1 ORDER BY tree_rank", &[k]),
        }
    }

    fn link_rows(&self, key: PageKey) -> Vec<LinkRow> {
        let conn = self.conn.borrow();
        let Ok(mut st) = conn.prepare_cached("SELECT to_id, to_url, anchor, section, kind, via_template FROM links WHERE from_id = ?1 ORDER BY pos") else {
            return Vec::new();
        };
        st.query_map([key.0 as i64], |r| {
            Ok(LinkRow {
                to: r.get::<_, Option<i64>>(0)?.map(|i| PageKey(i as u64)),
                to_url: r.get(1)?,
                anchor: r.get(2)?,
                section: r.get(3)?,
                red: r.get::<_, String>(4)? == "red",
                via_template: r.get::<_, i64>(5)? != 0,
            })
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    fn html(&self, key: PageKey) -> Option<String> {
        let conn = self.conn.borrow();
        let mut st = conn
            .prepare_cached("SELECT html FROM page_html WHERE page_id = ?1")
            .ok()?;
        st.query_row([key.0 as i64], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    fn page_by_url(&self, url: &str) -> Option<PageKey> {
        let u = url.split('#').next().unwrap_or(url);
        let conn = self.conn.borrow();
        let mut st = conn.prepare_cached("SELECT id FROM pages WHERE url = ?1 AND kind IN ('page', 'redirect', 'category') ORDER BY tree_rank LIMIT 1").ok()?;
        if let Some(id) = st
            .query_row([u], |r| r.get::<_, i64>(0))
            .optional()
            .ok()
            .flatten()
        {
            return Some(PageKey(id as u64));
        }
        // A directory URL lands on its index page: the path form.
        let site = self.catalog.sites.first()?;
        let base = url::Url::parse(&site.base_url).ok()?;
        let parsed = url::Url::parse(u).ok()?;
        let path = crate::memory::site_path(&parsed, &base)?;
        let mut st = conn
            .prepare_cached("SELECT id FROM pages WHERE site_id = ?1 AND container = 'pages' AND kind = 'page' AND path = ?2")
            .ok()?;
        st.query_row(rusqlite::params![site.id.0 as i64, path], |r| {
            r.get::<_, i64>(0)
        })
        .optional()
        .ok()
        .flatten()
        .map(|i| PageKey(i as u64))
    }

    fn documented(&self, site: SiteId, kind: PageKind) -> Vec<PageKey> {
        self.keys(
            "SELECT p.id FROM pages p JOIN page_html h ON h.page_id = p.id WHERE p.site_id = ?1 AND p.kind = ?2 ORDER BY p.container, p.tree_rank",
            &[Param::Int(site.0 as i64), Param::Str(kind.name().into())],
        )
    }

    fn rows(&self, keys: &[PageKey]) -> Vec<LightRow> {
        if keys.is_empty() {
            return Vec::new();
        }
        let conn = self.conn.borrow();
        let marks = vec!["?"; keys.len()].join(", ");
        let Ok(mut st) = conn.prepare(&format!(
            "SELECT {ROW_COLS} FROM pages WHERE id IN ({marks})"
        )) else {
            return Vec::new();
        };
        st.query_map(params_from_iter(keys.iter().map(|k| k.0 as i64)), row_from)
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    /// One statement: the subtree is the rank range after the
    /// parent up to the next row at its depth or above.
    fn descendants_of_kind(
        &self,
        site: SiteId,
        container: Container,
        parent: Option<PageKey>,
        kind: PageKind,
    ) -> Vec<(PageKey, u32)> {
        let conn = self.conn.borrow();
        let (from_rank, base_depth): (i64, i64) = match parent {
            Some(p) => {
                let Ok(v) = conn.query_row(
                    "SELECT tree_rank, depth FROM pages WHERE id = ?1",
                    [p.0 as i64],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                ) else {
                    return Vec::new();
                };
                v
            }
            None => (0, 0),
        };
        let end: i64 = match parent {
            Some(_) => conn
                .query_row(
                    "SELECT min(tree_rank) FROM pages WHERE site_id = ?1 AND container = ?2 AND tree_rank > ?3 AND depth <= ?4",
                    rusqlite::params![site.0 as i64, container.name(), from_rank, base_depth],
                    |r| r.get::<_, Option<i64>>(0),
                )
                .ok()
                .flatten()
                .unwrap_or(i64::MAX),
            None => i64::MAX,
        };
        let Ok(mut st) = conn.prepare_cached(
            "SELECT id, depth FROM pages WHERE site_id = ?1 AND container = ?2 AND kind = ?3 AND tree_rank > ?4 AND tree_rank < ?5 ORDER BY tree_rank",
        ) else {
            return Vec::new();
        };
        st.query_map(
            rusqlite::params![site.0 as i64, container.name(), kind.name(), from_rank, end],
            |r| {
                Ok((
                    PageKey(r.get::<_, i64>(0)? as u64),
                    (r.get::<_, i64>(1)? - base_depth) as u32,
                ))
            },
        )
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }
}

impl SqlStore for SqliteStore {
    fn dialect(&self) -> Dialect {
        Dialect::Sqlite
    }
    fn keys_where(&self, where_sql: &str, params: &[Param]) -> Vec<PageKey> {
        self.keys(
            &format!("SELECT p.id FROM pages p WHERE {where_sql} ORDER BY p.tree_rank"),
            params,
        )
    }
    fn keys_where_top(
        &self,
        where_sql: &str,
        params: &[Param],
        column: &str,
        descending: bool,
        n: i64,
    ) -> Vec<PageKey> {
        self.keys(
            &format!(
                "SELECT p.id FROM pages p WHERE {where_sql} ORDER BY {column} {}, p.tree_rank LIMIT {n}",
                if descending { "DESC" } else { "ASC" }
            ),
            params,
        )
    }
    fn count_where(&self, where_sql: &str, params: &[Param]) -> i64 {
        let conn = self.conn.borrow();
        conn.query_row(
            &format!("SELECT count(*) FROM pages p WHERE {where_sql}"),
            params_from_iter(params.iter().map(to_sql)),
            |r| r.get(0),
        )
        .unwrap_or(0)
    }
    fn execute(&self, sql: &str) -> Result<u64, String> {
        self.conn
            .borrow()
            .execute(sql, [])
            .map(|n| n as u64)
            .map_err(|e| format!("{e}: {sql}"))
    }
    fn tree_nodes(&self, site: SiteId, container: Container) -> Vec<TreeNode> {
        let conn = self.conn.borrow();
        let Ok(mut st) = conn.prepare("SELECT id, parent_id, name, kind, redirect_to FROM pages WHERE site_id = ?1 AND container = ?2") else {
            return Vec::new();
        };
        st.query_map(rusqlite::params![site.0 as i64, container.name()], |r| {
            Ok(TreeNode {
                id: r.get(0)?,
                parent: r.get(1)?,
                name: r.get(2)?,
                kind: r.get(3)?,
                redirect_to: r.get(4)?,
            })
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }
    fn set_tree(&self, rows: &[(i64, Option<i64>, i64, i64)]) -> Result<(), String> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut st = tx
                .prepare(
                    "UPDATE pages SET parent_id = ?2, depth = ?3, tree_rank = ?4 WHERE id = ?1",
                )
                .map_err(|e| e.to_string())?;
            for (id, parent, depth, rank) in rows {
                st.execute(rusqlite::params![id, parent, depth, rank])
                    .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }
    fn reload(&mut self) -> Result<(), String> {
        self.catalog = Self::read_catalog(&self.conn.borrow())?;
        Ok(())
    }
    fn link_edges(&self, site: SiteId, exclude_templates: bool) -> Vec<(PageKey, PageKey)> {
        let conn = self.conn.borrow();
        let sql = format!(
            "SELECT l.from_id, l.to_id FROM links l JOIN pages p ON p.id = l.from_id WHERE p.site_id = ?1 AND l.to_id IS NOT NULL{}",
            if exclude_templates {
                " AND l.via_template = 0"
            } else {
                ""
            }
        );
        let Ok(mut st) = conn.prepare(&sql) else {
            return Vec::new();
        };
        st.query_map([site.0 as i64], |r| {
            Ok((
                PageKey(r.get::<_, i64>(0)? as u64),
                PageKey(r.get::<_, i64>(1)? as u64),
            ))
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }
    fn red_counts(&self, site: SiteId) -> Vec<(PageKey, u32)> {
        let conn = self.conn.borrow();
        let Ok(mut st) = conn.prepare("SELECT l.from_id, count(*) FROM links l JOIN pages p ON p.id = l.from_id WHERE p.site_id = ?1 AND l.kind = 'red' GROUP BY l.from_id") else {
            return Vec::new();
        };
        st.query_map([site.0 as i64], |r| {
            Ok((
                PageKey(r.get::<_, i64>(0)? as u64),
                r.get::<_, i64>(1)? as u32,
            ))
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }
    fn set_analytics(&self, rows: &[(PageKey, Analytics)]) -> Result<(), String> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut st = tx
                .prepare("UPDATE pages SET in_degree = ?2, out_degree = ?3, mutual_degree = ?4, pagerank = ?5, redlinks = ?6 WHERE id = ?1")
                .map_err(|e| e.to_string())?;
            for (k, a) in rows {
                st.execute(rusqlite::params![
                    k.0 as i64,
                    a.in_degree as i64,
                    a.out_degree as i64,
                    a.mutual_degree as i64,
                    a.pagerank,
                    a.redlinks as i64
                ])
                .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }
    fn term_edges(&self, site: SiteId) -> Vec<(i64, i64, i64)> {
        let conn = self.conn.borrow();
        let Ok(mut st) = conn.prepare("SELECT t.page_id, t.term_id, t.pos FROM page_terms t JOIN pages p ON p.id = t.page_id WHERE p.site_id = ?1") else {
            return Vec::new();
        };
        st.query_map([site.0 as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }
    fn estimate_where(&self, where_sql: &str, params: &[Param]) -> Option<String> {
        let conn = self.conn.borrow();
        let mut st = conn
            .prepare(&format!(
                "EXPLAIN QUERY PLAN SELECT p.id FROM pages p WHERE {where_sql}"
            ))
            .ok()?;
        let lines: Vec<String> = st
            .query_map(params_from_iter(params.iter().map(to_sql)), |r| {
                r.get::<_, String>(3)
            })
            .ok()?
            .filter_map(|r| r.ok())
            .collect();
        Some(lines.join("; "))
    }
}

/// The export sink: one transaction, prepared inserts.
struct Writer<'a> {
    conn: &'a mut Connection,
}

impl Sink for Writer<'_> {
    fn begin(&mut self) -> Result<(), String> {
        self.conn.execute_batch("BEGIN").map_err(|e| e.to_string())
    }
    fn site(&mut self, s: &SiteRow, lowering: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO sites (id, host, base_url, snapshot, hierarchy, lowering, page_count, link_count) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![s.id.0 as i64, s.host, s.base_url, s.snapshot, hierarchy_name(s.hierarchy), lowering, s.page_count as i64, s.link_count as i64],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn page(&mut self, rec: &PageRecord) -> Result<(), String> {
        let r = rec.row;
        let (ms, mo) = instant_cols(&r.modified);
        let (ps, po) = instant_cols(&r.published);
        self.conn
            .execute(
                &format!("INSERT INTO pages ({ROW_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)"),
                rusqlite::params![
                    r.key.0 as i64,
                    r.site.0 as i64,
                    r.container.name(),
                    r.parent.map(|p| p.0 as i64),
                    r.kind.name(),
                    r.name,
                    r.path,
                    r.title,
                    r.url,
                    r.depth as i64,
                    r.tree_rank as i64,
                    r.category,
                    json_list(&r.categories),
                    json_list(&r.tags),
                    r.description,
                    ms,
                    mo,
                    ps,
                    po,
                    r.redirect_to.map(|p| p.0 as i64),
                    r.analytics.in_degree as i64,
                    r.analytics.out_degree as i64,
                    r.analytics.mutual_degree as i64,
                    r.analytics.pagerank,
                    r.analytics.redlinks as i64,
                ],
            )
            .map_err(|e| e.to_string())?;
        if let Some(h) = rec.html {
            self.conn
                .execute(
                    "INSERT INTO page_html (page_id, html) VALUES (?1, ?2)",
                    rusqlite::params![r.key.0 as i64, h],
                )
                .map_err(|e| e.to_string())?;
        }
        if let Some(t) = rec.plain {
            self.conn
                .execute(
                    "INSERT INTO page_text (page_id, plain) VALUES (?1, ?2)",
                    rusqlite::params![r.key.0 as i64, t],
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn link(&mut self, from: PageKey, pos: u32, l: &LinkRow) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO links (from_id, pos, to_id, to_url, anchor, section, kind, via_template) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![from.0 as i64, pos as i64, l.to.map(|p| p.0 as i64), l.to_url, l.anchor, l.section, if l.red { "red" } else { "link" }, l.via_template as i64],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn term(&mut self, page: PageKey, term: PageKey, pos: u32) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO page_terms (page_id, term_id, pos) VALUES (?1, ?2, ?3)",
                rusqlite::params![page.0 as i64, term.0 as i64, pos as i64],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn finish(&mut self) -> Result<(), String> {
        self.conn.execute_batch("COMMIT").map_err(|e| e.to_string())
    }
}
