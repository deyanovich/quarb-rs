//! The analytics pass over a store: degrees, mutual links,
//! redlinks, PageRank, and the site totals — each column defined
//! as exactly the cardinality the corresponding axis returns, so
//! `[::in_degree = 0]` and `[!<-link]` are one test.

use super::*;
use std::collections::{HashMap, HashSet};

/// PageRank's damping and iteration count (the memory store uses
/// the same).
pub const DAMPING: f64 = 0.85;
pub const ITERATIONS: usize = 20;

/// The analytics of a page graph: `pages` in document order,
/// `edges` the resolved links (duplicates allowed; degrees count
/// distinct neighbours), `reds` the redlink counts. One
/// computation for every store, so the in-memory store and the
/// pass over a database give the same numbers.
pub fn compute(pages: &[PageKey], edges: &[(PageKey, PageKey)], reds: &HashMap<PageKey, u32>) -> Vec<(PageKey, Analytics)> {
    let idx: HashMap<PageKey, usize> = pages.iter().enumerate().map(|(i, &k)| (k, i)).collect();
    let n = pages.len();
    let mut outs: Vec<HashSet<usize>> = vec![HashSet::new(); n];
    let mut ins: Vec<HashSet<usize>> = vec![HashSet::new(); n];
    for (f, t) in edges {
        if let (Some(&a), Some(&b)) = (idx.get(f), idx.get(t))
            && a != b
        {
            outs[a].insert(b);
            ins[b].insert(a);
        }
    }
    // PageRank, dangling mass spread.
    let nf = n.max(1) as f64;
    let mut pr = vec![1.0 / nf; n];
    for _ in 0..ITERATIONS {
        let mut next = vec![(1.0 - DAMPING) / nf; n];
        let mut dangling = 0.0;
        for i in 0..n {
            if outs[i].is_empty() {
                dangling += pr[i];
                continue;
            }
            let share = DAMPING * pr[i] / outs[i].len() as f64;
            for &t in &outs[i] {
                next[t] += share;
            }
        }
        let spread = DAMPING * dangling / nf;
        for v in &mut next {
            *v += spread;
        }
        pr = next;
    }
    pages
        .iter()
        .enumerate()
        .map(|(i, &k)| {
            let mutual = outs[i].iter().filter(|t| ins[i].contains(t)).count() as u32;
            (
                k,
                Analytics {
                    in_degree: ins[i].len() as u32,
                    out_degree: outs[i].len() as u32,
                    mutual_degree: mutual,
                    pagerank: pr[i],
                    redlinks: reds.get(&k).copied().unwrap_or(0),
                },
            )
        })
        .collect()
}

/// Run the analytics for `site` over a store: the edge list from
/// the links table (template links excluded when asked), the
/// computation above, the columns written back, the totals.
pub fn analyze(store: &dyn SqlStore, site: SiteId, exclude_templates: bool) -> Result<(), String> {
    // The graph's nodes: every page, and every category that has a
    // page of its own (a category named only in heads is a term,
    // not a node) — the rows the link axes can reach, so a degree
    // column is exactly what the axis returns.
    let mut pages: Vec<PageKey> = Vec::new();
    for c in Container::ALL {
        pages.extend(store.descendants_of_kind(site, c, None, PageKind::Page).into_iter().map(|(k, _)| k));
    }
    pages.extend(store.documented(site, PageKind::Category));
    let edges = store.link_edges(site, exclude_templates);
    let reds: HashMap<PageKey, u32> = store.red_counts(site).into_iter().collect();
    let rows = compute(&pages, &edges, &reds);
    let s = site.0 as i64;
    // Rows outside the graph carry no numbers (a term row, a
    // directory): a previous pass's values must not linger.
    store.execute(&format!(
        "UPDATE pages SET in_degree = 0, out_degree = 0, mutual_degree = 0, pagerank = 0, redlinks = 0 WHERE site_id = {s}"
    ))?;
    store.set_analytics(&rows)?;
    store.execute(&format!(
        "UPDATE sites SET \
           page_count = (SELECT count(*) FROM pages WHERE site_id = {s} AND kind = 'page'), \
           link_count = (SELECT count(*) FROM links l JOIN pages p ON p.id = l.from_id WHERE p.site_id = {s} AND l.to_id IS NOT NULL) \
         WHERE id = {s}"
    ))?;
    Ok(())
}

/// Resolve link targets by URL, then through redirects, for `site`.
pub fn resolve_links(store: &dyn SqlStore, site: SiteId, dialect: Dialect) -> Result<(u64, u64), String> {
    let s = site.0 as i64;
    let by_url = match dialect {
        // A lookup table keyed by URL: the correlated form let the
        // planner pick the kind index and scan every page per link.
        Dialect::Sqlite => {
            store.execute("DROP TABLE IF EXISTS url_ids")?;
            store.execute(&format!(
                "CREATE TEMP TABLE url_ids AS SELECT url AS to_url, min(id) AS id FROM pages \
                 WHERE site_id = {s} AND url IS NOT NULL AND kind IN ('page', 'redirect', 'category') GROUP BY url"
            ))?;
            store.execute("CREATE INDEX url_ids_u ON url_ids (to_url)")?;
            "UPDATE links SET to_id = (SELECT u.id FROM url_ids u WHERE u.to_url = links.to_url) \
             WHERE to_id IS NULL AND to_url IN (SELECT to_url FROM url_ids)"
                .to_string()
        }
        Dialect::Postgres => format!(
            "UPDATE links l SET to_id = p.id FROM pages p WHERE l.to_id IS NULL AND p.url = l.to_url AND p.site_id = {s} AND p.kind IN ('page', 'redirect', 'category')"
        ),
    };
    let n = store.execute(&by_url)?;
    if dialect == Dialect::Sqlite {
        store.execute("DROP TABLE url_ids")?;
    }
    let through = match dialect {
        Dialect::Sqlite => "UPDATE links SET to_id = (SELECT r.redirect_to FROM pages r WHERE r.id = links.to_id AND r.kind = 'redirect') \
                            WHERE EXISTS (SELECT 1 FROM pages r WHERE r.id = links.to_id AND r.kind = 'redirect' AND r.redirect_to IS NOT NULL)"
            .to_string(),
        Dialect::Postgres => "UPDATE links l SET to_id = r.redirect_to FROM pages r WHERE r.id = l.to_id AND r.kind = 'redirect' AND r.redirect_to IS NOT NULL".to_string(),
    };
    // Chains: at most three hops.
    let mut m = 0;
    for _ in 0..3 {
        let k = store.execute(&through)?;
        m += k;
        if k == 0 {
            break;
        }
    }
    Ok((n, m))
}

/// The index pass: pg_trgm on the plain text and the titles
/// (PostgreSQL only; SQLite has no substring index).
pub fn index(store: &dyn SqlStore) -> Result<Vec<String>, String> {
    match store.dialect() {
        Dialect::Sqlite => Ok(vec!["sqlite: no substring index (the planner's text prefilter scans page_text)".to_string()]),
        Dialect::Postgres => {
            let mut done = Vec::new();
            for sql in [
                "CREATE EXTENSION IF NOT EXISTS pg_trgm",
                "CREATE INDEX IF NOT EXISTS page_text_trgm ON page_text USING gin (plain gin_trgm_ops)",
                "CREATE INDEX IF NOT EXISTS pages_title_trgm ON pages USING gin (title gin_trgm_ops)",
                "ANALYZE pages",
                "ANALYZE page_text",
                "ANALYZE links",
            ] {
                store.execute(sql)?;
                done.push(sql.to_string());
            }
            Ok(done)
        }
    }
}

/// Pre-order ranks for a tree given as (id, parent, name): siblings
/// in byte order of their names, depth from 1. Returns
/// (id, parent, depth, rank).
pub fn rank_tree(nodes: &[(i64, Option<i64>, String)]) -> Vec<(i64, Option<i64>, i64, i64)> {
    let mut kids: HashMap<Option<i64>, Vec<usize>> = HashMap::new();
    for (i, (_, parent, _)) in nodes.iter().enumerate() {
        kids.entry(*parent).or_default().push(i);
    }
    for v in kids.values_mut() {
        v.sort_by(|&a, &b| nodes[a].2.as_bytes().cmp(nodes[b].2.as_bytes()));
    }
    let mut out = Vec::with_capacity(nodes.len());
    let mut rank = 0i64;
    let mut stack: Vec<(usize, i64)> = kids.get(&None).cloned().unwrap_or_default().into_iter().rev().map(|i| (i, 1)).collect();
    while let Some((i, depth)) = stack.pop() {
        rank += 1;
        out.push((nodes[i].0, nodes[i].1, depth, rank));
        if let Some(cs) = kids.get(&Some(nodes[i].0)) {
            for &c in cs.iter().rev() {
                stack.push((c, depth + 1));
            }
        }
    }
    out
}
