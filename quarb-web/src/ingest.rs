//! The Wikipedia ingest: the Wikimedia Enterprise HTML dumps (a
//! tar.gz of NDJSON members, one article per line) streamed into a
//! store — page rows, the Parsoid HTML verbatim, the same text
//! lowering the grafts use, every wikilink with its section, the
//! declared categories, the redirects — then the SQL passes that
//! resolve links by URL (through redirects), choose the tree, rank
//! it, and compute the analytics.

use crate::contract::*;
use crate::db::{analyze, PageRecord, Sink, SqlStore};
use serde_json::Value as Json;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::sync::mpsc;

/// How the site's tree is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tree {
    /// URL path directories (every article under `wiki/`).
    Path,
    /// The category DAG, made a tree by the shortest-path parent
    /// from the root category (ties by declared order); categories
    /// no path reaches form a forest beside it.
    Category,
}

pub struct Options {
    /// The site's base URL; read off the first record when empty.
    pub base_url: String,
    /// Stop after this many records (a pilot).
    pub limit: Option<usize>,
    /// Text-lowering workers.
    pub workers: usize,
    pub tree: Tree,
    /// The root category for `Tree::Category` (`Categoria:Enciclopedia`).
    pub root_category: Option<String>,
    /// The snapshot label recorded on the site (the dump run).
    pub snapshot: Option<String>,
    /// Leave links written by templates (navboxes) out of the
    /// analytics.
    pub exclude_templates: bool,
}

#[derive(Default, Debug)]
pub struct Summary {
    pub articles: u64,
    pub categories: u64,
    pub redirects: u64,
    pub links: u64,
    pub skipped: u64,
    pub site: Option<SiteId>,
}

/// Ids minted for rows the dump does not number.
const REDIRECT_BASE: i64 = 1_000_000_000;
const CATEGORY_BASE: i64 = 2_000_000_000;
const DIR_BASE: i64 = 3_000_000_000;

/// A URL as the store keys it: percent-decoded path, no fragment
/// or query, so a Parsoid `./Guerra_degli_em%C3%B9` and the dump's
/// own `…/wiki/Guerra_degli_emù` are one string.
pub fn normalize_url(u: &str) -> String {
    let Ok(mut url) = url::Url::parse(u) else { return u.to_string() };
    url.set_fragment(None);
    url.set_query(None);
    let path = percent_encoding::percent_decode_str(url.path()).decode_utf8_lossy().into_owned();
    format!("{}://{}{}", url.scheme(), url.host_str().unwrap_or(""), path)
}

/// What a worker makes of one article record.
struct Processed {
    row: LightRow,
    html: String,
    plain: String,
    links: Vec<LinkRow>,
    categories: Vec<String>,
    redirects: Vec<(String, String)>,
}

fn instant_secs(v: &Option<quarb::Value>) -> i64 {
    match v {
        Some(quarb::Value::Instant { secs, .. }) => *secs,
        _ => i64::MIN,
    }
}

fn field<'a>(v: &'a Json, key: &str) -> Option<&'a Json> {
    v.get(key).filter(|x| !x.is_null())
}

fn text(v: &Json, key: &str) -> Option<String> {
    field(v, key).and_then(|x| x.as_str()).map(str::to_string)
}

/// Whether a title names a page of another namespace than the
/// articles and the categories — the namespaces of the Italian and
/// the canonical English Wikipedia; `Categoria:` pages are rows.
pub fn namespaced(title: &str) -> bool {
    let Some((prefix, _)) = title.split_once(':') else { return false };
    let p = prefix.replace('_', " ");
    const NS: &[&str] = &[
        "Speciale", "Special", "Discussione", "Talk", "Utente", "User", "Discussioni utente", "User talk",
        "Wikipedia", "Project", "Discussioni Wikipedia", "Project talk", "File", "Image", "Immagine",
        "Discussioni file", "File talk", "MediaWiki", "Discussioni MediaWiki", "Template", "Discussioni template",
        "Template talk", "Aiuto", "Help", "Discussioni aiuto", "Help talk", "Discussioni categoria", "Category talk",
        "Portale", "Discussioni portale", "Progetto", "Discussioni progetto", "Modulo", "Module", "Discussioni modulo",
        "Module talk", "Gadget", "Definizione gadget", "Media", "TimedText", "Category",
    ];
    NS.iter().any(|n| n.eq_ignore_ascii_case(&p))
}

/// The path segment a URL ends with, decoded.
fn last_segment(url: &str) -> String {
    url.trim_end_matches('/').rsplit('/').next().unwrap_or(url).to_string()
}

fn process(line: &str, site: SiteId, base: &url::Url, wiki_dir: i64, tree: Tree) -> Option<Processed> {
    let v: Json = serde_json::from_str(line).ok()?;
    let id = field(&v, "identifier")?.as_i64()?;
    let name = text(&v, "name")?;
    let url = normalize_url(&text(&v, "url")?);
    let ns = field(&v, "namespace").and_then(|n| n.get("identifier")).and_then(|x| x.as_i64()).unwrap_or(0);
    let html = field(&v, "article_body").and_then(|b| b.get("html")).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let modified = text(&v, "date_modified").and_then(|s| quarb_text_html::instant(&s));
    let published = text(&v, "date_created").and_then(|s| quarb_text_html::instant(&s));
    let description = text(&v, "abstract").map(|a| quarb_text::normalize_ws(&a)).filter(|a| !a.is_empty());
    let categories: Vec<String> = field(&v, "categories")
        .and_then(|c| c.as_array())
        .map(|a| a.iter().filter_map(|c| text(c, "name")).collect())
        .unwrap_or_default();
    let redirects: Vec<(String, String)> = field(&v, "redirects")
        .and_then(|c| c.as_array())
        .map(|a| a.iter().filter_map(|r| Some((text(r, "name")?, normalize_url(&text(r, "url")?)))).collect())
        .unwrap_or_default();
    let wikidata = field(&v, "main_entity").and_then(|m| m.get("identifier")).and_then(|x| x.as_str()).map(str::to_string);
    let path = url::Url::parse(&url)
        .ok()
        .and_then(|u| crate::memory::site_path(&u, base))
        .unwrap_or_else(|| last_segment(&url));
    let is_category = ns == 14;
    let plain = if html.is_empty() { String::new() } else { quarb_text_html::parse(&html).plain_text() };
    // Links: the DOM's anchors, joined against the page's URL and
    // its `<base href>`; targets normalized like page URLs.
    let (base_href, found) = crate::memory::dom_links(&html);
    let page_url = url::Url::parse(&url).ok();
    let join_base = match (&page_url, base_href) {
        (Some(pu), Some(b)) => pu.join(&b).ok().or_else(|| Some(pu.clone())),
        (pu, _) => pu.clone(),
    };
    let mut links = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Some(jb) = join_base {
        for f in found {
            let Ok(u) = jb.join(&f.href) else { continue };
            if u.host_str() != base.host_str() {
                continue;
            }
            let to = normalize_url(u.as_str());
            // A link into another namespace (Speciale:, File:,
            // Template:, …) leads to a page the dump does not
            // carry; it is neither a link nor a redlink here.
            if namespaced(&last_segment(&to)) {
                continue;
            }
            if to == url || !seen.insert(to.clone()) {
                continue;
            }
            links.push(LinkRow { to: None, to_url: to, anchor: f.anchor, section: f.section, red: f.red, via_template: f.transcluded });
        }
    }
    let mut tags = Vec::new();
    if let Some(w) = wikidata {
        tags.push(format!("wikidata:{w}"));
    }
    // The page properties Parsoid writes into the head: a
    // disambiguation page, a hidden category, a noindex.
    for (prop, tag) in [("mw:PageProp/disambiguation", "disambiguation"), ("mw:PageProp/hiddencat", "hiddencat"), ("mw:PageProp/noindex", "noindex")] {
        if html.contains(&format!("property=\"{prop}\"")) {
            tags.push(tag.to_string());
        }
    }
    let row = LightRow {
        key: PageKey(id as u64),
        site,
        container: if is_category && tree == Tree::Path { Container::Categories } else { Container::Pages },
        parent: if is_category { None } else { Some(PageKey(wiki_dir as u64)) },
        kind: if is_category { PageKind::Category } else { PageKind::Page },
        name: if is_category { name.clone() } else { last_segment(&path) },
        path: if is_category { quarb_text::HeadMeta::trait_name(&name) } else { path },
        title: Some(name),
        url: Some(url),
        depth: 0,
        tree_rank: 0,
        category: categories.first().cloned(),
        categories: categories.clone(),
        tags,
        description,
        modified,
        published,
        redirect_to: None,
        analytics: Analytics::default(),
    };
    Some(Processed { row, html, plain, links, categories, redirects })
}

/// A record reader over one dump file: every tar member is
/// NDJSON. A thread reads the archive member by member and
/// hands lines over a bounded channel, so nothing is held.
fn lines_of(path: &Path) -> Result<Box<dyn Iterator<Item = String>>, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path.to_string_lossy().to_string();
    let gz = name.ends_with(".gz") || name.ends_with(".tgz");
    let is_tar = name.ends_with(".tar.gz") || name.ends_with(".tgz") || name.ends_with(".tar");
    let (tx, rx) = mpsc::sync_channel::<String>(1024);
    std::thread::spawn(move || {
        let reader: Box<dyn Read> = if gz { Box::new(flate2::read::GzDecoder::new(f)) } else { Box::new(f) };
        let send_lines = |r: Box<dyn Read + '_>| {
            for l in BufReader::with_capacity(1 << 20, r).lines() {
                let Ok(l) = l else { return false };
                if l.trim().is_empty() {
                    continue;
                }
                if tx.send(l).is_err() {
                    return false;
                }
            }
            true
        };
        if is_tar {
            let mut ar = tar::Archive::new(reader);
            let Ok(entries) = ar.entries() else { return };
            for e in entries {
                let Ok(e) = e else { return };
                if !e.header().entry_type().is_file() {
                    continue;
                }
                if !send_lines(Box::new(e)) {
                    return;
                }
            }
        } else {
            send_lines(reader);
        }
    });
    Ok(Box::new(rx.into_iter()))
}

/// Stream `files` into `sink`. Category rows are written last (an
/// article names categories before their own records appear), the
/// site row first (from the first record's URL when no base was
/// given). Links land with `to_id` unset; [`finish`] resolves them.
pub fn wikipedia(files: &[std::path::PathBuf], sink: &mut dyn Sink, opts: &Options) -> Result<Summary, String> {
    let mut summary = Summary::default();
    let site = SiteId(1);
    summary.site = Some(site);
    let mut base: Option<url::Url> = url::Url::parse(&opts.base_url).ok();
    let mut started = false;
    let wiki_dir = DIR_BASE + 1;
    // Category rows by name: the id (an NS14 record's own, else
    // minted), and the record once seen.
    let mut cat_ids: HashMap<String, i64> = HashMap::new();
    let mut cat_rows: HashMap<i64, Processed> = HashMap::new();
    let mut next_cat = CATEGORY_BASE;
    let mut next_redirect = REDIRECT_BASE;
    let mut seen_pages: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut n_articles = 0usize;
    let mut n_sent = 0usize;
    sink.begin()?;
    let workers = opts.workers.max(1);
    for file in files {
        let lines = lines_of(file)?;
        // A bounded pipeline: the reader feeds lines to workers, the
        // workers lower the HTML, this thread writes in arrival order.
        let (tx_line, rx_line) = mpsc::sync_channel::<String>(workers * 4);
        let mut tx_line = Some(tx_line);
        let (tx_done, rx_done) = mpsc::sync_channel::<Option<Processed>>(workers * 4);
        let rx_line = std::sync::Arc::new(std::sync::Mutex::new(rx_line));
        let base_for_workers = std::sync::Arc::new(std::sync::Mutex::new(base.clone()));
        std::thread::scope(|scope| -> Result<(), String> {
            for _ in 0..workers {
                let rx = rx_line.clone();
                let tx = tx_done.clone();
                let base_cell = base_for_workers.clone();
                let tree = opts.tree;
                scope.spawn(move || {
                    loop {
                        let line = {
                            let Ok(guard) = rx.lock() else { return };
                            match guard.recv() {
                                Ok(l) => l,
                                Err(_) => return,
                            }
                        };
                        let b = base_cell.lock().ok().and_then(|g| g.clone());
                        let Some(b) = b else {
                            let _ = tx.send(None);
                            continue;
                        };
                        let p = process(&line, site, &b, wiki_dir, tree);
                        if tx.send(p).is_err() {
                            return;
                        }
                    }
                });
            }
            drop(tx_done);
            let mut lines = lines;
            let mut tx_line = tx_line.take();
            let mut in_flight = 0usize;
            let mut exhausted = false;
            loop {
                while !exhausted && in_flight < workers * 4 {
                    match lines.next() {
                        Some(l) => {
                            if base.is_none() {
                                // The base from the first record's URL.
                                if let Ok(v) = serde_json::from_str::<Json>(&l)
                                    && let Some(u) = text(&v, "url")
                                    && let Ok(mut pu) = url::Url::parse(&u)
                                {
                                    pu.set_path("/");
                                    pu.set_query(None);
                                    pu.set_fragment(None);
                                    base = Some(pu.clone());
                                    if let Ok(mut g) = base_for_workers.lock() {
                                        *g = Some(pu);
                                    }
                                }
                            }
                            if opts.limit.is_some_and(|n| n_sent >= n) {
                                exhausted = true;
                                break;
                            }
                            tx_line.as_ref().ok_or("closed")?.send(l).map_err(|e| e.to_string())?;
                            in_flight += 1;
                            n_sent += 1;
                        }
                        None => exhausted = true,
                    }
                }
                if in_flight == 0 {
                    break;
                }
                let Ok(p) = rx_done.recv() else { break };
                if exhausted && in_flight == 1 {
                    // The last result is in hand: close the line
                    // channel so the workers return and the scope
                    // can end.
                    drop(tx_line.take());
                }
                in_flight -= 1;
                let Some(p) = p else {
                    summary.skipped += 1;
                    continue;
                };
                if !started {
                    let b = base.clone().ok_or("no base URL")?;
                    sink.site(
                        &SiteRow {
                            id: site,
                            host: b.host_str().unwrap_or("").to_string(),
                            base_url: b.to_string(),
                            snapshot: opts.snapshot.clone(),
                            hierarchy: match opts.tree {
                                Tree::Path => Hierarchy::Directories,
                                Tree::Category => Hierarchy::Categories,
                            },
                            page_count: 0,
                            link_count: 0,
                        },
                        &format!("quarb-text-html {}", env!("CARGO_PKG_VERSION")),
                    )?;
                    // The one directory of the path tree.
                    sink.page(&PageRecord {
                        row: &LightRow {
                            key: PageKey(wiki_dir as u64),
                            site,
                            container: Container::Pages,
                            parent: None,
                            kind: PageKind::Dir,
                            name: "wiki".into(),
                            path: "wiki".into(),
                            title: None,
                            url: None,
                            depth: 1,
                            tree_rank: 1,
                            category: None,
                            categories: Vec::new(),
                            tags: Vec::new(),
                            description: None,
                            modified: None,
                            published: None,
                            redirect_to: None,
                            analytics: Analytics::default(),
                        },
                        html: None,
                        plain: None,
                    })?;
                    started = true;
                }
                if p.row.kind == PageKind::Category {
                    let id = *cat_ids.entry(p.row.title.clone().unwrap_or_default()).or_insert(p.row.key.0 as i64);
                    // Its parents are categories too, with or
                    // without a record of their own in the dump.
                    for c in &p.categories {
                        cat_ids.entry(c.clone()).or_insert_with(|| {
                            next_cat += 1;
                            next_cat
                        });
                    }
                    let mut p = p;
                    p.row.key = PageKey(id as u64);
                    // A renamed page appears twice under one
                    // identifier: the later modification wins.
                    let newer = cat_rows.get(&id).is_none_or(|old| instant_secs(&old.row.modified) <= instant_secs(&p.row.modified));
                    if newer {
                        cat_rows.insert(id, p);
                    } else {
                        summary.skipped += 1;
                    }
                    continue;
                }
                let id = p.row.key.0 as i64;
                if !seen_pages.insert(id) {
                    summary.skipped += 1;
                    continue;
                }
                n_articles += 1;
                summary.articles += 1;
                sink.page(&PageRecord { row: &p.row, html: Some(&p.html), plain: Some(&p.plain) })?;
                for (pos, l) in p.links.iter().enumerate() {
                    sink.link(p.row.key, pos as u32, l)?;
                }
                summary.links += p.links.len() as u64;
                for (pos, c) in p.categories.iter().enumerate() {
                    let cid = *cat_ids.entry(c.clone()).or_insert_with(|| {
                        next_cat += 1;
                        next_cat
                    });
                    sink.term(p.row.key, PageKey(cid as u64), pos as u32)?;
                }
                for (name, url) in &p.redirects {
                    next_redirect += 1;
                    summary.redirects += 1;
                    let path = url::Url::parse(url).ok().and_then(|u| base.as_ref().and_then(|b| crate::memory::site_path(&u, b))).unwrap_or_else(|| last_segment(url));
                    sink.page(&PageRecord {
                        row: &LightRow {
                            key: PageKey(next_redirect as u64),
                            site,
                            container: Container::Pages,
                            parent: Some(PageKey(wiki_dir as u64)),
                            kind: PageKind::Redirect,
                            name: last_segment(&path),
                            path,
                            title: Some(name.clone()),
                            url: Some(url.clone()),
                            depth: 0,
                            tree_rank: 0,
                            category: None,
                            categories: Vec::new(),
                            tags: Vec::new(),
                            description: None,
                            modified: None,
                            published: None,
                            redirect_to: Some(p.row.key),
                            analytics: Analytics::default(),
                        },
                        html: None,
                        plain: None,
                    })?;
                }
            }
            drop(tx_line.take());
            Ok(())
        })?;
    }
    // Every category named or seen, as a row; parents from the
    // NS14 record's own categories.
    // One row per id: a name whose id already has a record (a
    // renamed page's old name) writes nothing.
    let mut names: Vec<(String, i64)> = cat_ids.iter().map(|(n, i)| (n.clone(), *i)).collect();
    names.sort();
    let mut written: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for (name, id) in &names {
        if !written.insert(*id) {
            continue;
        }
        summary.categories += 1;
        match cat_rows.get(id) {
            Some(p) => {
                sink.page(&PageRecord { row: &p.row, html: Some(&p.html), plain: Some(&p.plain) })?;
                for (pos, l) in p.links.iter().enumerate() {
                    sink.link(p.row.key, pos as u32, l)?;
                }
            }
            None => {
                // Named by an article, no page of its own in the dump.
                let b = base.clone().ok_or("no base URL")?;
                sink.page(&PageRecord {
                    row: &LightRow {
                        key: PageKey(*id as u64),
                        site,
                        container: if opts.tree == Tree::Path { Container::Categories } else { Container::Pages },
                        parent: None,
                        kind: PageKind::Category,
                        name: name.clone(),
                        path: quarb_text::HeadMeta::trait_name(name),
                        title: Some(name.clone()),
                        url: Some(format!("{}wiki/{}", b, name.replace(' ', "_"))),
                        depth: 0,
                        tree_rank: 0,
                        category: None,
                        categories: Vec::new(),
                        tags: Vec::new(),
                        description: None,
                        modified: None,
                        published: None,
                        redirect_to: None,
                        analytics: Analytics::default(),
                    },
                    html: None,
                    plain: None,
                })?;
            }
        }
    }
    for (id, p) in &cat_rows {
        for (pos, c) in p.categories.iter().enumerate() {
            if let Some(pid) = cat_ids.get(c)
                && pid != id
            {
                sink.term(PageKey(*id as u64), PageKey(*pid as u64), pos as u32)?;
            }
        }
    }
    sink.finish()?;
    Ok(summary)
}

/// The passes after the rows are in: links by URL and through
/// redirects, the tree (ranks; the category tree when chosen),
/// the analytics.
pub fn finish(store: &mut dyn SqlStore, site: SiteId, opts: &Options) -> Result<Vec<String>, String> {
    let mut log = Vec::new();
    let dialect = store.dialect();
    let t = std::time::Instant::now();
    let (n, m) = analyze::resolve_links(store, site, dialect)?;
    log.push(format!("links resolved: {n} by URL, {m} through redirects ({:.1?})", t.elapsed()));
    eprintln!("{}", log.last().unwrap());
    match opts.tree {
        Tree::Path => {
            let nodes: Vec<(i64, Option<i64>, String)> = store.tree_nodes(site, Container::Pages).into_iter().map(|n| (n.id, n.parent, n.name)).collect();
            let ranked = analyze::rank_tree(&nodes);
            store.set_tree(&ranked)?;
            let cats: Vec<(i64, Option<i64>, String)> = store.tree_nodes(site, Container::Categories).into_iter().map(|n| (n.id, n.parent, n.name)).collect();
            let ranked = analyze::rank_tree(&cats);
            store.set_tree(&ranked)?;
            log.push(format!("path tree: {} row(s) ranked", nodes.len() + cats.len()));
        }
        Tree::Category => {
            let t = std::time::Instant::now();
            let placed = category_tree(store, site, opts.root_category.as_deref())?;
            log.push(format!("{placed} ({:.1?})", t.elapsed()));
            eprintln!("{}", log.last().unwrap());
        }
    }
    let t = std::time::Instant::now();
    analyze::analyze(store, site, opts.exclude_templates)?;
    store.reload()?;
    eprintln!("analytics ({:.1?})", t.elapsed());
    log.push(format!(
        "analytics: degrees, mutual, redlinks, pagerank, totals{}",
        if opts.exclude_templates { " (template links excluded)" } else { "" }
    ));
    Ok(log)
}

/// The category DAG as a tree: BFS from the root category over
/// subcategory membership; a page's parent is its declared category
/// with the smallest depth (ties by declared order); categories no
/// path reaches hang from the site root as a forest.
fn category_tree(store: &mut dyn SqlStore, site: SiteId, root: Option<&str>) -> Result<String, String> {
    // Every row into one container (the ingest may already have
    // put the categories there), then read them back with kinds.
    let t = std::time::Instant::now();
    store.set_container(site, PageKind::Category, Container::Pages)?;
    let rows = store.tree_nodes(site, Container::Pages);
    eprintln!("tree: {} row(s) read ({:.1?})", rows.len(), t.elapsed());
    let cat_ids: std::collections::HashSet<i64> = rows.iter().filter(|r| r.kind == "category").map(|r| r.id).collect();
    let cats: Vec<(i64, Option<i64>, String)> = rows.iter().filter(|r| r.kind == "category").map(|r| (r.id, r.parent, r.name.clone())).collect();
    let nodes: Vec<(i64, Option<i64>, String)> = rows.iter().filter(|r| r.kind != "category" && r.kind != "dir").map(|r| (r.id, r.parent, r.name.clone())).collect();
    let redirect_of: HashMap<i64, i64> = rows.iter().filter_map(|r| r.redirect_to.map(|t| (r.id, t))).collect();
    let mut all: Vec<(i64, Option<i64>, String)> = nodes.iter().chain(cats.iter()).cloned().collect();
    let edges = store.term_edges(site);
    eprintln!("tree: {} membership edge(s) read ({:.1?})", edges.len(), t.elapsed());
    // child → parents (categories only) in declared order; page →
    // categories in declared order.
    let mut parents: HashMap<i64, Vec<(i64, i64)>> = HashMap::new();
    for (page, term, pos) in &edges {
        parents.entry(*page).or_default().push((*pos, *term));
    }
    for v in parents.values_mut() {
        v.sort();
    }
    // BFS depths over categories from the root.
    let root_id = root.and_then(|r| all.iter().find(|n| cat_ids.contains(&n.0) && n.2 == r).map(|n| n.0));
    let mut depth: HashMap<i64, i64> = HashMap::new();
    let mut parent_of: HashMap<i64, Option<i64>> = HashMap::new();
    if let Some(r) = root_id {
        // children lists: parent category → subcategories
        let mut subs: HashMap<i64, Vec<(i64, i64)>> = HashMap::new();
        for (child, ps) in &parents {
            if cat_ids.contains(child) {
                for (pos, p) in ps {
                    subs.entry(*p).or_default().push((*pos, *child));
                }
            }
        }
        depth.insert(r, 1);
        parent_of.insert(r, None);
        let mut queue = std::collections::VecDeque::from([r]);
        while let Some(c) = queue.pop_front() {
            let d = depth[&c];
            if let Some(ss) = subs.get(&c) {
                let mut ss = ss.clone();
                ss.sort();
                for (_, s) in ss {
                    if !depth.contains_key(&s) {
                        depth.insert(s, d + 1);
                        parent_of.insert(s, Some(c));
                        queue.push_back(s);
                    }
                }
            }
        }
    }
    let reached = depth.len();
    let mut unreached = 0;
    for c in &cats {
        if !depth.contains_key(&c.0) {
            unreached += 1;
            parent_of.insert(c.0, None);
        }
    }
    // Pages: the shallowest declared category; else the site root.
    for n in &nodes {
        if cat_ids.contains(&n.0) {
            continue;
        }
        let choice = parents
            .get(&n.0)
            .and_then(|ps| ps.iter().filter(|(_, p)| cat_ids.contains(p)).min_by_key(|(pos, p)| (depth.get(p).copied().unwrap_or(i64::MAX), *pos)).map(|(_, p)| *p));
        parent_of.insert(n.0, choice);
    }
    // A redirect stands beside the page it stands for.
    for (r, t) in &redirect_of {
        if let Some(p) = parent_of.get(t).copied() {
            parent_of.insert(*r, p);
        }
    }
    for r in all.iter_mut() {
        if let Some(p) = parent_of.get(&r.0) {
            r.1 = *p;
        }
    }
    let ranked = analyze::rank_tree(&all);
    eprintln!("tree: ranked ({:.1?}); writing", t.elapsed());
    store.set_tree(&ranked)?;
    eprintln!("tree: written ({:.1?})", t.elapsed());
    // Drop the path tree's directory row: nothing hangs from it now.
    store.execute(&format!("DELETE FROM pages WHERE site_id = {} AND kind = 'dir'", site.0))?;
    Ok(format!(
        "category tree: root {}, {reached} categor{} reached, {unreached} unreached (a forest beside), {} row(s) ranked",
        root.unwrap_or("(none)"),
        if reached == 1 { "y" } else { "ies" },
        ranked.len()
    ))
}
