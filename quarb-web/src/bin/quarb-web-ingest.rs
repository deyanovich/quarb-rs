//! `quarb-web-ingest`: fill and finish an index-backed store for
//! the web level.
//!
//! - `crawl <dir|archive> --into <store>`: a directory of built
//!   pages or an archive of them, through the in-memory build
//!   (tree, identity, links, analytics).
//! - `wikipedia <dump.tar.gz>… --into <store>`: the Wikimedia
//!   Enterprise HTML dumps, streamed; then the resolve, tree, and
//!   analytics passes.
//! - `analyze --store <store>`: the analytics pass alone.
//! - `index --store <store>`: the substring indexes (PostgreSQL).
//!
//! A store is a `site.db` path (SQLite) or a `postgres://` URL.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use quarb_web::db::postgres::PostgresStore;
use quarb_web::db::sqlite::SqliteStore;
use quarb_web::db::{SqlStore, analyze};
use quarb_web::ingest::{self, Options, Tree};
use quarb_web::{SiteId, WebStore};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "quarb-web-ingest", version, about = "Fill a web-level store")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Read a directory of pages or an archive of them into a store.
    Crawl {
        source: PathBuf,
        /// The store to write: a site.db path or a postgres:// URL.
        #[arg(long)]
        into: String,
        /// The base URL pages join against when no page declares a
        /// canonical URL.
        #[arg(long, default_value = "")]
        base: String,
    },
    /// Stream Wikimedia Enterprise HTML dumps into a store, then
    /// resolve links, build the tree, and run the analytics.
    Wikipedia {
        /// The dump files (NS14 before NS0 is not required).
        dumps: Vec<PathBuf>,
        #[arg(long)]
        into: String,
        /// Stop after this many articles (a pilot slice).
        #[arg(long)]
        limit: Option<usize>,
        /// Text-lowering workers.
        #[arg(long, default_value_t = 4)]
        workers: usize,
        /// The tree: `path` (URL directories) or `category`.
        #[arg(long, default_value = "path")]
        tree: String,
        /// The root category for --tree=category.
        #[arg(long)]
        root: Option<String>,
        /// A label for the snapshot (the dump run id).
        #[arg(long)]
        snapshot: Option<String>,
        /// Drop the store's tables first.
        #[arg(long)]
        fresh: bool,
        /// Leave template-written links (navboxes) out of the analytics.
        #[arg(long)]
        no_templates: bool,
    },
    /// Recompute the analytics columns of a store.
    Analyze {
        #[arg(long)]
        store: String,
        /// Leave template-written links (navboxes) out.
        #[arg(long)]
        no_templates: bool,
    },
    /// Build the substring indexes (PostgreSQL: pg_trgm).
    Index {
        #[arg(long)]
        store: String,
    },
}

enum Store {
    Sqlite(SqliteStore),
    Postgres(PostgresStore),
}

impl Store {
    fn open(spec: &str) -> Result<Store> {
        if spec.starts_with("postgres://") || spec.starts_with("postgresql://") {
            PostgresStore::open(spec)
                .map(Store::Postgres)
                .map_err(|e| anyhow::anyhow!("{e}"))
        } else {
            SqliteStore::open(std::path::Path::new(spec))
                .map(Store::Sqlite)
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
    }
    fn empty(spec: &str, fresh: bool) -> Result<Store> {
        if spec.starts_with("postgres://") || spec.starts_with("postgresql://") {
            if fresh {
                PostgresStore::drop_all(spec).map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            PostgresStore::empty(spec)
                .map(Store::Postgres)
                .map_err(|e| anyhow::anyhow!("{e}"))
        } else {
            SqliteStore::empty(std::path::Path::new(spec))
                .map(Store::Sqlite)
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
    }
    fn sql(&mut self) -> &mut dyn SqlStore {
        match self {
            Store::Sqlite(s) => s,
            Store::Postgres(s) => s,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Crawl { source, into, base } => {
            let t = std::time::Instant::now();
            let site = if source.is_dir() {
                quarb_web::fs::open_dir(&source, &base)
                    .with_context(|| format!("reading {}", source.display()))?
            } else {
                quarb_web::archive::open_path(&source, &base)
                    .with_context(|| format!("reading {}", source.display()))?
            };
            let cat = site.store().catalog();
            eprintln!(
                "read {}: {} page(s), {} link(s) in {:.1?}",
                cat.sites[0].host,
                cat.sites[0].page_count,
                cat.sites[0].link_count,
                t.elapsed()
            );
            let t = std::time::Instant::now();
            if into.starts_with("postgres://") || into.starts_with("postgresql://") {
                PostgresStore::drop_all(&into).map_err(|e| anyhow::anyhow!("{e}"))?;
                PostgresStore::create(&into, site.store())
                    .map_err(|e| anyhow::anyhow!("writing {into}: {e}"))?;
            } else {
                SqliteStore::create(std::path::Path::new(&into), site.store())
                    .map_err(|e| anyhow::anyhow!("writing {into}: {e}"))?;
            }
            eprintln!("wrote {into} in {:.1?}", t.elapsed());
        }
        Cmd::Wikipedia {
            dumps,
            into,
            limit,
            workers,
            tree,
            root,
            snapshot,
            fresh,
            no_templates,
        } => {
            let opts = Options {
                base_url: String::new(),
                limit,
                workers,
                tree: match tree.as_str() {
                    "category" => Tree::Category,
                    _ => Tree::Path,
                },
                root_category: root,
                snapshot,
                exclude_templates: no_templates,
            };
            let t = std::time::Instant::now();
            let mut store = Store::empty(&into, fresh)?;
            let summary = match &mut store {
                Store::Sqlite(s) => ingest::wikipedia(&dumps, &mut s.sink(), &opts),
                Store::Postgres(s) => ingest::wikipedia(&dumps, &mut s.sink(), &opts),
            }
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            eprintln!(
                "ingested {} article(s), {} categor(ies), {} redirect(s), {} link(s), {} skipped in {:.1?}",
                summary.articles,
                summary.categories,
                summary.redirects,
                summary.links,
                summary.skipped,
                t.elapsed()
            );
            let t = std::time::Instant::now();
            let site = summary.site.unwrap_or(SiteId(1));
            for line in
                ingest::finish(store.sql(), site, &opts).map_err(|e| anyhow::anyhow!("{e}"))?
            {
                eprintln!("{line}");
            }
            eprintln!("finished in {:.1?}", t.elapsed());
        }
        Cmd::Analyze {
            store,
            no_templates,
        } => {
            let mut st = Store::open(&store)?;
            let sites: Vec<SiteId> = st.sql().catalog().sites.iter().map(|s| s.id).collect();
            for s in sites {
                let t = std::time::Instant::now();
                analyze::analyze(st.sql(), s, no_templates).map_err(|e| anyhow::anyhow!("{e}"))?;
                eprintln!("site {}: analytics in {:.1?}", s.0, t.elapsed());
            }
        }
        Cmd::Index { store } => {
            let mut st = Store::open(&store)?;
            for line in analyze::index(st.sql()).map_err(|e| anyhow::anyhow!("{e}"))? {
                eprintln!("{line}");
            }
        }
    }
    Ok(())
}
