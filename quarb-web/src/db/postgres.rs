//! The PostgreSQL store: the corpus store. Every operation is one
//! indexed statement; the export fills it with COPY; the text
//! prefilter rides pg_trgm once the index pass has run.

use super::*;
use bytes::Bytes;
use futures_util::SinkExt;
use std::cell::RefCell;
use tokio::runtime::Runtime;
use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, NoTls};

pub struct PostgresStore {
    rt: Runtime,
    client: RefCell<Client>,
    catalog: Catalog,
}

const ROW_COLS: &str = "id, site_id, container, parent_id, kind, name, path, title, url, depth, tree_rank, \
    category, categories, tags, description, modified_secs, modified_off, published_secs, published_off, \
    redirect_to, in_degree, out_degree, mutual_degree, pagerank, redlinks";

fn boxed(params: &[Param]) -> Vec<Box<dyn ToSql + Sync>> {
    params
        .iter()
        .map(|p| -> Box<dyn ToSql + Sync> {
            match p {
                Param::Str(s) => Box::new(s.clone()),
                Param::Int(i) => Box::new(*i),
                Param::Float(f) => Box::new(*f),
            }
        })
        .collect()
}

fn row_from(r: &tokio_postgres::Row) -> LightRow {
    LightRow {
        key: PageKey(r.get::<_, i64>(0) as u64),
        site: SiteId(r.get::<_, i64>(1) as u32),
        container: container_of(&r.get::<_, String>(2)),
        parent: r.get::<_, Option<i64>>(3).map(|p| PageKey(p as u64)),
        kind: kind_of(&r.get::<_, String>(4)),
        name: r.get(5),
        path: r.get(6),
        title: r.get(7),
        url: r.get(8),
        depth: r.get::<_, i64>(9) as u32,
        tree_rank: r.get::<_, i64>(10) as u64,
        category: r.get(11),
        categories: list_json(&r.get::<_, String>(12)),
        tags: list_json(&r.get::<_, String>(13)),
        description: r.get(14),
        modified: instant(r.get(15), r.get(16)),
        published: instant(r.get(17), r.get(18)),
        redirect_to: r.get::<_, Option<i64>>(19).map(|p| PageKey(p as u64)),
        analytics: Analytics {
            in_degree: r.get::<_, i64>(20) as u32,
            out_degree: r.get::<_, i64>(21) as u32,
            mutual_degree: r.get::<_, i64>(22) as u32,
            pagerank: r.get(23),
            redlinks: r.get::<_, i64>(24) as u32,
        },
    }
}

impl PostgresStore {
    /// Connect (a `postgres://` URL or a key=value string) and read
    /// the catalog.
    pub fn open(config: &str) -> Result<PostgresStore, String> {
        let (rt, client) = Self::connect(config)?;
        let catalog = Self::read_catalog(&rt, &client)?;
        Ok(PostgresStore { rt, client: RefCell::new(client), catalog })
    }

    /// Connect and create the schema (tables that exist stay).
    pub fn empty(config: &str) -> Result<PostgresStore, String> {
        let (rt, client) = Self::connect(config)?;
        for stmt in ddl(Dialect::Postgres) {
            rt.block_on(client.batch_execute(&stmt)).map_err(|e| format!("{e}: {stmt}"))?;
        }
        let catalog = Self::read_catalog(&rt, &client)?;
        Ok(PostgresStore { rt, client: RefCell::new(client), catalog })
    }

    /// Create the schema and fill it from `store`.
    pub fn create<S: WebStore>(config: &str, store: &S) -> Result<PostgresStore, String> {
        let mut me = Self::empty(config)?;
        {
            let mut w = me.sink();
            export(store, &mut w)?;
        }
        me.reload()?;
        Ok(me)
    }

    /// Drop every table of the schema (a fresh corpus).
    pub fn drop_all(config: &str) -> Result<(), String> {
        let (rt, client) = Self::connect(config)?;
        rt.block_on(client.batch_execute(
            "DROP TABLE IF EXISTS page_terms, links, page_text, page_html, pages, sites CASCADE",
        ))
        .map_err(|e| e.to_string())
    }

    /// Write rows into this store with COPY (one transaction from
    /// `begin` to `finish`).
    pub fn sink(&mut self) -> impl Sink + '_ {
        CopyWriter { rt: &self.rt, client: self.client.get_mut(), bufs: Default::default() }
    }

    fn connect(config: &str) -> Result<(Runtime, Client), String> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
        let client = rt
            .block_on(async {
                let (client, connection) = tokio_postgres::connect(config, NoTls).await?;
                tokio::spawn(connection);
                Ok::<_, tokio_postgres::Error>(client)
            })
            .map_err(|e| format!("connecting: {e}"))?;
        Ok((rt, client))
    }

    fn read_catalog(rt: &Runtime, client: &Client) -> Result<Catalog, String> {
        let rows = rt
            .block_on(client.query(
                "SELECT id, host, base_url, snapshot, hierarchy, lowering, page_count, link_count FROM sites ORDER BY id",
                &[],
            ))
            .map_err(|e| e.to_string())?;
        let mut lowering = String::new();
        let sites = rows
            .iter()
            .map(|r| {
                lowering = r.get(5);
                SiteRow {
                    id: SiteId(r.get::<_, i32>(0) as u32),
                    host: r.get(1),
                    base_url: r.get(2),
                    snapshot: r.get(3),
                    hierarchy: hierarchy_of(&r.get::<_, String>(4)),
                    page_count: r.get::<_, i64>(6) as u64,
                    link_count: r.get::<_, i64>(7) as u64,
                }
            })
            .collect();
        Ok(Catalog { sites, lowering })
    }

    fn keys(&self, sql: &str, params: &[Param]) -> Vec<PageKey> {
        let b = boxed(params);
        let refs: Vec<&(dyn ToSql + Sync)> = b.iter().map(|x| x.as_ref()).collect();
        let client = self.client.borrow();
        self.rt
            .block_on(client.query(sql, &refs))
            .map(|rows| rows.iter().map(|r| PageKey(r.get::<_, i64>(0) as u64)).collect())
            .unwrap_or_default()
    }
}

impl WebStore for PostgresStore {
    fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    fn row(&self, key: PageKey) -> Option<LightRow> {
        let client = self.client.borrow();
        let r = self
            .rt
            .block_on(client.query_opt(&format!("SELECT {ROW_COLS} FROM pages WHERE id = $1"), &[&(key.0 as i64)]))
            .ok()??;
        Some(row_from(&r))
    }

    fn children(&self, site: SiteId, container: Container, parent: Option<PageKey>) -> Vec<PageKey> {
        match parent {
            Some(p) => self.keys(
                "SELECT id FROM pages WHERE site_id = $1 AND container = $2 AND parent_id = $3 ORDER BY tree_rank",
                &[Param::Int(site.0 as i64), Param::Str(container.name().into()), Param::Int(p.0 as i64)],
            ),
            None => self.keys(
                "SELECT id FROM pages WHERE site_id = $1 AND container = $2 AND parent_id IS NULL ORDER BY tree_rank",
                &[Param::Int(site.0 as i64), Param::Str(container.name().into())],
            ),
        }
    }

    fn children_named(&self, site: SiteId, container: Container, parent: Option<PageKey>, name: &str) -> Vec<PageKey> {
        match parent {
            Some(p) => self.keys(
                "SELECT id FROM pages WHERE site_id = $1 AND container = $2 AND parent_id = $3 AND name = $4 ORDER BY tree_rank",
                &[Param::Int(site.0 as i64), Param::Str(container.name().into()), Param::Int(p.0 as i64), Param::Str(name.into())],
            ),
            None => self.keys(
                "SELECT id FROM pages WHERE site_id = $1 AND container = $2 AND parent_id IS NULL AND name = $3 ORDER BY tree_rank",
                &[Param::Int(site.0 as i64), Param::Str(container.name().into()), Param::Str(name.into())],
            ),
        }
    }

    fn related(&self, key: PageKey, kind: LinkKind, dir: LinkDir) -> Vec<PageKey> {
        let k = Param::Int(key.0 as i64);
        match (kind, dir) {
            (LinkKind::Link, LinkDir::Out) => self.keys("SELECT to_id FROM links WHERE from_id = $1 AND to_id IS NOT NULL ORDER BY pos", &[k]),
            (LinkKind::Link, LinkDir::In) => self.keys(
                "SELECT l.from_id FROM links l JOIN pages p ON p.id = l.from_id WHERE l.to_id = $1 ORDER BY p.tree_rank",
                &[k],
            ),
            (LinkKind::Category, LinkDir::Out) => self.keys(
                "SELECT t.term_id FROM page_terms t JOIN pages r ON r.id = t.term_id WHERE t.page_id = $1 AND r.kind = 'category' ORDER BY t.pos",
                &[k],
            ),
            (LinkKind::Tag, LinkDir::Out) => self.keys(
                "SELECT t.term_id FROM page_terms t JOIN pages r ON r.id = t.term_id WHERE t.page_id = $1 AND r.kind = 'tag' ORDER BY t.pos",
                &[k],
            ),
            (LinkKind::Category | LinkKind::Tag, LinkDir::In) => self.keys(
                "SELECT t.page_id FROM page_terms t JOIN pages p ON p.id = t.page_id WHERE t.term_id = $1 ORDER BY p.tree_rank",
                &[k],
            ),
            (LinkKind::Redirect, LinkDir::Out) => self.keys("SELECT redirect_to FROM pages WHERE id = $1 AND redirect_to IS NOT NULL", &[k]),
            (LinkKind::Redirect, LinkDir::In) => self.keys("SELECT id FROM pages WHERE redirect_to = $1 ORDER BY tree_rank", &[k]),
        }
    }

    fn link_rows(&self, key: PageKey) -> Vec<LinkRow> {
        let client = self.client.borrow();
        self.rt
            .block_on(client.query("SELECT to_id, to_url, anchor, section, kind, via_template FROM links WHERE from_id = $1 ORDER BY pos", &[&(key.0 as i64)]))
            .map(|rows| {
                rows.iter()
                    .map(|r| LinkRow {
                        to: r.get::<_, Option<i64>>(0).map(|i| PageKey(i as u64)),
                        to_url: r.get(1),
                        anchor: r.get(2),
                        section: r.get(3),
                        red: r.get::<_, String>(4) == "red",
                        via_template: r.get::<_, i64>(5) != 0,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn html(&self, key: PageKey) -> Option<String> {
        let client = self.client.borrow();
        self.rt
            .block_on(client.query_opt("SELECT html FROM page_html WHERE page_id = $1", &[&(key.0 as i64)]))
            .ok()?
            .map(|r| r.get(0))
    }

    fn page_by_url(&self, url: &str) -> Option<PageKey> {
        let u = url.split('#').next().unwrap_or(url);
        let hit = self.keys("SELECT id FROM pages WHERE url = $1 AND kind IN ('page', 'redirect', 'category') ORDER BY tree_rank LIMIT 1", &[Param::Str(u.into())]);
        if let Some(k) = hit.first() {
            return Some(*k);
        }
        let parsed = url::Url::parse(u).ok()?;
        for site in &self.catalog.sites {
            let base = url::Url::parse(&site.base_url).ok()?;
            if let Some(path) = crate::memory::site_path(&parsed, &base) {
                let hit = self.keys(
                    "SELECT id FROM pages WHERE site_id = $1 AND container = 'pages' AND kind = 'page' AND path = $2",
                    &[Param::Int(site.id.0 as i64), Param::Str(path)],
                );
                if let Some(k) = hit.first() {
                    return Some(*k);
                }
            }
        }
        None
    }

    fn documented(&self, site: SiteId, kind: PageKind) -> Vec<PageKey> {
        self.keys(
            "SELECT p.id FROM pages p JOIN page_html h ON h.page_id = p.id WHERE p.site_id = $1 AND p.kind = $2 ORDER BY p.container, p.tree_rank",
            &[Param::Int(site.0 as i64), Param::Str(kind.name().into())],
        )
    }

    fn rows(&self, keys: &[PageKey]) -> Vec<LightRow> {
        if keys.is_empty() {
            return Vec::new();
        }
        let ids: Vec<i64> = keys.iter().map(|k| k.0 as i64).collect();
        let client = self.client.borrow();
        self.rt
            .block_on(client.query(&format!("SELECT {ROW_COLS} FROM pages WHERE id = ANY($1)"), &[&ids]))
            .map(|rows| rows.iter().map(row_from).collect())
            .unwrap_or_default()
    }

    fn descendants_of_kind(&self, site: SiteId, container: Container, parent: Option<PageKey>, kind: PageKind) -> Vec<(PageKey, u32)> {
        let client = self.client.borrow();
        let (from_rank, base_depth): (i64, i64) = match parent {
            Some(p) => {
                let Ok(r) = self.rt.block_on(client.query_one("SELECT tree_rank, depth FROM pages WHERE id = $1", &[&(p.0 as i64)])) else {
                    return Vec::new();
                };
                (r.get(0), r.get::<_, i64>(1))
            }
            None => (0, 0),
        };
        let end: i64 = match parent {
            Some(_) => self
                .rt
                .block_on(client.query_one(
                    "SELECT min(tree_rank) FROM pages WHERE site_id = $1 AND container = $2 AND tree_rank > $3 AND depth <= $4",
                    &[&(site.0 as i64), &container.name(), &from_rank, &base_depth],
                ))
                .ok()
                .and_then(|r| r.get::<_, Option<i64>>(0))
                .unwrap_or(i64::MAX),
            None => i64::MAX,
        };
        self.rt
            .block_on(client.query(
                "SELECT id, depth FROM pages WHERE site_id = $1 AND container = $2 AND kind = $3 AND tree_rank > $4 AND tree_rank < $5 ORDER BY tree_rank",
                &[&(site.0 as i64), &container.name(), &kind.name(), &from_rank, &end],
            ))
            .map(|rows| rows.iter().map(|r| (PageKey(r.get::<_, i64>(0) as u64), (r.get::<_, i64>(1) - base_depth) as u32)).collect())
            .unwrap_or_default()
    }

    /// One statement for a frontier's links.
    fn prefetch(&self, _keys: &[PageKey], _dir: LinkDir) {}
}

impl SqlStore for PostgresStore {
    fn dialect(&self) -> Dialect {
        Dialect::Postgres
    }
    fn keys_where(&self, where_sql: &str, params: &[Param]) -> Vec<PageKey> {
        self.keys(&format!("SELECT p.id FROM pages p WHERE {where_sql} ORDER BY p.tree_rank"), params)
    }
    fn keys_where_top(&self, where_sql: &str, params: &[Param], column: &str, descending: bool, n: i64) -> Vec<PageKey> {
        self.keys(
            &format!(
                "SELECT p.id FROM pages p WHERE {where_sql} ORDER BY {column} {}, p.tree_rank LIMIT {n}",
                if descending { "DESC" } else { "ASC" }
            ),
            params,
        )
    }
    fn count_where(&self, where_sql: &str, params: &[Param]) -> i64 {
        let b = boxed(params);
        let refs: Vec<&(dyn ToSql + Sync)> = b.iter().map(|x| x.as_ref()).collect();
        let client = self.client.borrow();
        self.rt
            .block_on(client.query_one(&format!("SELECT count(*) FROM pages p WHERE {where_sql}"), &refs))
            .map(|r| r.get::<_, i64>(0))
            .unwrap_or(0)
    }
    fn estimate_where(&self, where_sql: &str, params: &[Param]) -> Option<String> {
        let b = boxed(params);
        let refs: Vec<&(dyn ToSql + Sync)> = b.iter().map(|x| x.as_ref()).collect();
        let client = self.client.borrow();
        let rows = self.rt.block_on(client.query(&format!("EXPLAIN SELECT p.id FROM pages p WHERE {where_sql}"), &refs)).ok()?;
        Some(rows.iter().map(|r| r.get::<_, String>(0).trim().to_string()).collect::<Vec<_>>().join("; "))
    }
    fn execute(&self, sql: &str) -> Result<u64, String> {
        let client = self.client.borrow();
        self.rt.block_on(client.execute(sql, &[])).map_err(|e| format!("{e}: {sql}"))
    }
    fn tree_nodes(&self, site: SiteId, container: Container) -> Vec<TreeNode> {
        let client = self.client.borrow();
        self.rt
            .block_on(client.query("SELECT id, parent_id, name, kind, redirect_to FROM pages WHERE site_id = $1 AND container = $2", &[&(site.0 as i64), &container.name()]))
            .map(|rows| {
                rows.iter()
                    .map(|r| TreeNode { id: r.get(0), parent: r.get(1), name: r.get(2), kind: r.get(3), redirect_to: r.get(4) })
                    .collect()
            })
            .unwrap_or_default()
    }
    fn set_tree(&self, rows: &[(i64, Option<i64>, i64, i64)]) -> Result<(), String> {
        let client = self.client.borrow();
        self.rt
            .block_on(async {
                client.batch_execute("BEGIN; CREATE TEMP TABLE tree_in (id BIGINT PRIMARY KEY, parent_id BIGINT, depth BIGINT, tree_rank BIGINT) ON COMMIT DROP").await?;
                let sink = client.copy_in("COPY tree_in (id, parent_id, depth, tree_rank) FROM STDIN").await?;
                let mut sink = std::pin::pin!(sink);
                let mut buf = String::new();
                for (id, parent, depth, rank) in rows {
                    buf.push_str(&format!("{id}\t{}\t{depth}\t{rank}\n", parent.map(|p| p.to_string()).unwrap_or_else(|| "\\N".into())));
                    if buf.len() > 1 << 20 {
                        sink.send(Bytes::from(std::mem::take(&mut buf))).await?;
                    }
                }
                if !buf.is_empty() {
                    sink.send(Bytes::from(buf)).await?;
                }
                sink.as_mut().finish().await?;
                client
                    .batch_execute("UPDATE pages p SET parent_id = t.parent_id, depth = t.depth, tree_rank = t.tree_rank FROM tree_in t WHERE p.id = t.id; COMMIT")
                    .await?;
                Ok::<_, tokio_postgres::Error>(())
            })
            .map_err(|e| e.to_string())
    }
    fn reload(&mut self) -> Result<(), String> {
        self.catalog = Self::read_catalog(&self.rt, self.client.get_mut())?;
        Ok(())
    }
    fn link_edges(&self, site: SiteId, exclude_templates: bool) -> Vec<(PageKey, PageKey)> {
        let client = self.client.borrow();
        let sql = format!(
            "SELECT l.from_id, l.to_id FROM links l JOIN pages p ON p.id = l.from_id WHERE p.site_id = $1 AND l.to_id IS NOT NULL{}",
            if exclude_templates { " AND l.via_template = 0" } else { "" }
        );
        self.rt
            .block_on(client.query(&sql, &[&(site.0 as i64)]))
            .map(|rows| rows.iter().map(|r| (PageKey(r.get::<_, i64>(0) as u64), PageKey(r.get::<_, i64>(1) as u64))).collect())
            .unwrap_or_default()
    }
    fn red_counts(&self, site: SiteId) -> Vec<(PageKey, u32)> {
        let client = self.client.borrow();
        self.rt
            .block_on(client.query(
                "SELECT l.from_id, count(*) FROM links l JOIN pages p ON p.id = l.from_id WHERE p.site_id = $1 AND l.kind = 'red' GROUP BY l.from_id",
                &[&(site.0 as i64)],
            ))
            .map(|rows| rows.iter().map(|r| (PageKey(r.get::<_, i64>(0) as u64), r.get::<_, i64>(1) as u32)).collect())
            .unwrap_or_default()
    }
    fn set_analytics(&self, rows: &[(PageKey, Analytics)]) -> Result<(), String> {
        let client = self.client.borrow();
        self.rt
            .block_on(async {
                client.batch_execute("BEGIN; CREATE TEMP TABLE an_in (id BIGINT PRIMARY KEY, in_degree BIGINT, out_degree BIGINT, mutual_degree BIGINT, pagerank DOUBLE PRECISION, redlinks BIGINT) ON COMMIT DROP").await?;
                let sink = client.copy_in("COPY an_in (id, in_degree, out_degree, mutual_degree, pagerank, redlinks) FROM STDIN").await?;
                let mut sink = std::pin::pin!(sink);
                let mut buf = String::new();
                for (k, a) in rows {
                    buf.push_str(&format!("{}\t{}\t{}\t{}\t{}\t{}\n", k.0, a.in_degree, a.out_degree, a.mutual_degree, a.pagerank, a.redlinks));
                    if buf.len() > 1 << 20 {
                        sink.send(Bytes::from(std::mem::take(&mut buf))).await?;
                    }
                }
                if !buf.is_empty() {
                    sink.send(Bytes::from(buf)).await?;
                }
                sink.as_mut().finish().await?;
                client
                    .batch_execute("UPDATE pages p SET in_degree = a.in_degree, out_degree = a.out_degree, mutual_degree = a.mutual_degree, pagerank = a.pagerank, redlinks = a.redlinks FROM an_in a WHERE p.id = a.id; COMMIT")
                    .await?;
                Ok::<_, tokio_postgres::Error>(())
            })
            .map_err(|e| e.to_string())
    }
    fn term_edges(&self, site: SiteId) -> Vec<(i64, i64, i64)> {
        let client = self.client.borrow();
        self.rt
            .block_on(client.query(
                "SELECT t.page_id, t.term_id, t.pos FROM page_terms t JOIN pages p ON p.id = t.page_id WHERE p.site_id = $1",
                &[&(site.0 as i64)],
            ))
            .map(|rows| rows.iter().map(|r| (r.get(0), r.get(1), r.get(2))).collect())
            .unwrap_or_default()
    }
}

/// COPY text-format escaping.
fn copy_field(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n").replace('\r', "\\r")
}

fn copy_opt(s: Option<&str>) -> String {
    match s {
        Some(s) => copy_field(s),
        None => "\\N".to_string(),
    }
}

fn copy_num<T: ToString>(v: Option<T>) -> String {
    v.map(|v| v.to_string()).unwrap_or_else(|| "\\N".to_string())
}

/// The COPY sink: one buffer per table, flushed in dependency
/// order whenever one grows past a few megabytes and at the end.
struct CopyWriter<'a> {
    rt: &'a Runtime,
    client: &'a mut Client,
    bufs: [String; 5],
}

const TABLES: [(&str, &str); 5] = [
    ("pages", ROW_COLS),
    ("page_html", "page_id, html"),
    ("page_text", "page_id, plain"),
    ("links", "from_id, pos, to_id, to_url, anchor, section, kind, via_template"),
    ("page_terms", "page_id, term_id, pos"),
];

impl CopyWriter<'_> {
    fn flush(&mut self) -> Result<(), String> {
        for (i, (table, cols)) in TABLES.iter().enumerate() {
            if self.bufs[i].is_empty() {
                continue;
            }
            let data = std::mem::take(&mut self.bufs[i]);
            self.rt
                .block_on(async {
                    let sink = self.client.copy_in(&format!("COPY {table} ({cols}) FROM STDIN")).await?;
                    let mut sink = std::pin::pin!(sink);
                    sink.send(Bytes::from(data)).await?;
                    sink.as_mut().finish().await?;
                    Ok::<_, tokio_postgres::Error>(())
                })
                .map_err(|e| format!("COPY {table}: {e}"))?;
        }
        Ok(())
    }

    fn maybe_flush(&mut self) -> Result<(), String> {
        if self.bufs.iter().any(|b| b.len() > 8 << 20) {
            self.flush()?;
        }
        Ok(())
    }
}

impl Sink for CopyWriter<'_> {
    fn begin(&mut self) -> Result<(), String> {
        self.rt.block_on(self.client.batch_execute("BEGIN")).map_err(|e| e.to_string())
    }
    fn site(&mut self, s: &SiteRow, lowering: &str) -> Result<(), String> {
        self.rt
            .block_on(self.client.execute(
                "INSERT INTO sites (id, host, base_url, snapshot, hierarchy, lowering, page_count, link_count) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                &[&(s.id.0 as i32), &s.host, &s.base_url, &s.snapshot, &hierarchy_name(s.hierarchy), &lowering, &(s.page_count as i64), &(s.link_count as i64)],
            ))
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn page(&mut self, rec: &PageRecord) -> Result<(), String> {
        let r = rec.row;
        let (ms, mo) = instant_cols(&r.modified);
        let (ps, po) = instant_cols(&r.published);
        let line = [
            r.key.0.to_string(),
            r.site.0.to_string(),
            r.container.name().to_string(),
            copy_num(r.parent.map(|p| p.0)),
            r.kind.name().to_string(),
            copy_field(&r.name),
            copy_field(&r.path),
            copy_opt(r.title.as_deref()),
            copy_opt(r.url.as_deref()),
            r.depth.to_string(),
            r.tree_rank.to_string(),
            copy_opt(r.category.as_deref()),
            copy_field(&json_list(&r.categories)),
            copy_field(&json_list(&r.tags)),
            copy_opt(r.description.as_deref()),
            copy_num(ms),
            copy_num(mo),
            copy_num(ps),
            copy_num(po),
            copy_num(r.redirect_to.map(|p| p.0)),
            r.analytics.in_degree.to_string(),
            r.analytics.out_degree.to_string(),
            r.analytics.mutual_degree.to_string(),
            r.analytics.pagerank.to_string(),
            r.analytics.redlinks.to_string(),
        ]
        .join("\t");
        self.bufs[0].push_str(&line);
        self.bufs[0].push('\n');
        if let Some(h) = rec.html {
            self.bufs[1].push_str(&format!("{}\t{}\n", r.key.0, copy_field(h)));
        }
        if let Some(t) = rec.plain {
            self.bufs[2].push_str(&format!("{}\t{}\n", r.key.0, copy_field(t)));
        }
        self.maybe_flush()
    }
    fn link(&mut self, from: PageKey, pos: u32, l: &LinkRow) -> Result<(), String> {
        self.bufs[3].push_str(&format!(
            "{}\t{pos}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            from.0,
            copy_num(l.to.map(|p| p.0)),
            copy_field(&l.to_url),
            copy_opt(l.anchor.as_deref()),
            copy_opt(l.section.as_deref()),
            if l.red { "red" } else { "link" },
            l.via_template as i64
        ));
        self.maybe_flush()
    }
    fn term(&mut self, page: PageKey, term: PageKey, pos: u32) -> Result<(), String> {
        self.bufs[4].push_str(&format!("{}\t{}\t{pos}\n", page.0, term.0));
        self.maybe_flush()
    }
    fn finish(&mut self) -> Result<(), String> {
        self.flush()?;
        self.rt.block_on(self.client.batch_execute("COMMIT")).map_err(|e| e.to_string())
    }
}
