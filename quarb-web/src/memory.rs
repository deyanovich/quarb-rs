//! An in-memory store built from a set of pages: the tree from
//! their paths, each page's declared identity from its head, the
//! links from its DOM, and the analytics over the link graph. The
//! form behind a tarball or a directory of built pages — a whole
//! site small enough to hold.

use crate::contract::*;
use quarb::Value;
use std::collections::{BTreeMap, HashMap};

/// The site a set of pages belongs to.
#[derive(Clone, Debug)]
pub struct SiteInput {
    /// `https://quarb.org/` — the base every path joins against
    /// when a page declares no canonical URL of its own.
    pub base_url: String,
    pub snapshot: Option<String>,
}

pub struct MemoryStore {
    catalog: Catalog,
    rows: Vec<LightRow>,
    html: HashMap<PageKey, String>,
    /// container → parent (None = top) → children in rank order
    children: HashMap<(Container, Option<PageKey>), Vec<PageKey>>,
    out_links: HashMap<PageKey, Vec<LinkRow>>,
    in_links: HashMap<PageKey, Vec<PageKey>>,
    /// category/tag row → member pages, in document order
    members: HashMap<PageKey, Vec<PageKey>>,
    /// page → its category rows / tag rows
    page_categories: HashMap<PageKey, Vec<PageKey>>,
    page_tags: HashMap<PageKey, Vec<PageKey>>,
    by_url: HashMap<String, PageKey>,
}

/// A page as read from a file: its site-relative path and its
/// bytes as text.
pub struct PageFile {
    pub path: String,
    pub html: String,
}

fn host_of(base_url: &str) -> String {
    url::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| base_url.trim_end_matches('/').to_string())
}

/// The site-relative path a URL names, when it is on the site:
/// `https://quarb.org/guides/` → `guides/index.html`, `/` →
/// `index.html`, percent-decoded, fragment and query dropped.
pub(crate) fn site_path(url: &url::Url, base: &url::Url) -> Option<String> {
    if url.host_str() != base.host_str() || url.scheme() != base.scheme() {
        return None;
    }
    let raw = url.path();
    let mut p = percent_encoding::percent_decode_str(raw)
        .decode_utf8_lossy()
        .into_owned();
    let base_path = base.path().trim_end_matches('/');
    if !base_path.is_empty() {
        p = p.strip_prefix(base_path)?.to_string();
    }
    let p = p.trim_start_matches('/');
    if p.is_empty() || p.ends_with('/') {
        return Some(format!("{p}index.html"));
    }
    Some(p.to_string())
}

/// Every hyperlink in the page's body, in document order, with
/// its anchor text and the lemma of the nearest preceding
/// heading — the crawler's view, complete where the text level's
/// prose refs are selective.
/// One anchor as the walk found it.
pub(crate) struct Found {
    pub href: String,
    pub anchor: Option<String>,
    pub section: Option<String>,
    /// `class="new"` — a wiki's own redlink mark.
    pub red: bool,
    /// Inside a transclusion (`typeof="mw:Transclusion"` or an
    /// `about="#mwt…"` ancestor): written by a template.
    pub transcluded: bool,
}

pub(crate) fn dom_links(html: &str) -> (Option<String>, Vec<Found>) {
    use scraper::{Html, Node, Selector};
    let doc = Html::parse_document(html);
    let base = quarb_text_html::base_href(&doc);
    let body = Selector::parse("body").unwrap();
    let Some(body) = doc.select(&body).next() else {
        return (base, Vec::new());
    };
    let mut out = Vec::new();
    let mut section: Option<String> = None;
    // A depth-first walk, tracking the last heading seen and the
    // transclusion depth (a node and its subtree are transcluded
    // when an ancestor carries the marker).
    let mut stack = vec![(*body, false)];
    while let Some((node, in_template)) = stack.pop() {
        let mut transcluded = in_template;
        if let Node::Element(el) = node.value() {
            let tag = el.name();
            if matches!(tag, "nav" | "header" | "footer" | "script" | "style") {
                continue;
            }
            if el
                .attr("typeof")
                .is_some_and(|t| t.contains("mw:Transclusion"))
                || el.attr("about").is_some_and(|a| a.starts_with("#mwt"))
            {
                transcluded = true;
            }
            // A heading names the section the links after it sit
            // under — and may carry links of its own (an index's
            // `<h3><a href>`), so the walk goes on into it.
            if matches!(tag, "h1" | "h2" | "h3" | "h4") {
                let t: String = scraper::ElementRef::wrap(node)
                    .map(|e| e.text().collect::<String>())
                    .unwrap_or_default();
                section = Some(quarb_text::normalize_ws(&t));
            }
            if tag == "a"
                && let Some(href) = el.attr("href")
                && !href.trim().is_empty()
            {
                let anchor = scraper::ElementRef::wrap(node)
                    .map(|e| quarb_text::normalize_ws(&e.text().collect::<String>()))
                    .filter(|s| !s.is_empty());
                let red = el
                    .attr("class")
                    .is_some_and(|c| c.split_whitespace().any(|w| w == "new"));
                out.push(Found {
                    href: href.trim().to_string(),
                    anchor,
                    section: section.clone(),
                    red,
                    transcluded,
                });
            }
        }
        // children in document order: push reversed
        let kids: Vec<_> = node.children().collect();
        for c in kids.into_iter().rev() {
            stack.push((c, transcluded));
        }
    }
    (base, out)
}

/// The base URL a set of pages is served from, from the first
/// page that declares a canonical URL ending in its own path.
fn infer_base(files: &[PageFile]) -> Option<String> {
    use scraper::{Html, Selector};
    let sel = Selector::parse("link[rel=canonical]").ok()?;
    let mut sorted: Vec<&PageFile> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    for f in sorted {
        let lower = f.path.to_ascii_lowercase();
        if !(lower.ends_with(".html") || lower.ends_with(".htm")) {
            continue;
        }
        let doc = Html::parse_document(&f.html);
        let Some(href) = doc.select(&sel).next().and_then(|e| e.value().attr("href")) else {
            continue;
        };
        let Ok(u) = url::Url::parse(href.trim()) else {
            continue;
        };
        let rel = f.path.trim_start_matches("./").trim_start_matches('/');
        let path = u.path();
        let stem = rel.strip_suffix("index.html").unwrap_or(rel);
        // `…/guides/jq.html` ends the canonical path, or the
        // directory form `…/guides/` for an index page.
        let prefix = if path.ends_with(rel) {
            &path[..path.len() - rel.len()]
        } else if path.ends_with(stem) {
            &path[..path.len() - stem.len()]
        } else {
            continue;
        };
        let mut b = u.clone();
        b.set_path(if prefix.is_empty() { "/" } else { prefix });
        b.set_query(None);
        b.set_fragment(None);
        return Some(b.to_string());
    }
    None
}

struct Built {
    row: LightRow,
    html: Option<String>,
}

impl MemoryStore {
    /// Build the store from a site's pages. Paths are site-relative
    /// (`guides/jq.html`); only `.html`/`.htm` files become pages,
    /// every path prefix a directory.
    pub fn build(mut site: SiteInput, files: Vec<PageFile>) -> MemoryStore {
        // No base given: read it off a page's canonical URL — the
        // origin plus whatever precedes the page's own path.
        if site.base_url.is_empty() {
            site.base_url = infer_base(&files).unwrap_or_else(|| "https://localhost/".to_string());
        }
        let base = url::Url::parse(&site.base_url).ok();
        let host = host_of(&site.base_url);
        let sid = SiteId(1);
        let mut next_key = 1u64;

        // 1. Rows: a directory for every path prefix, a page for
        // every html file. Prefixes sort before what they contain,
        // so creating rows in path order sees every parent first.
        let mut pages: BTreeMap<String, String> = BTreeMap::new();
        for f in files {
            let path = f
                .path
                .trim_start_matches("./")
                .trim_start_matches('/')
                .to_string();
            let lower = path.to_ascii_lowercase();
            // An html file by name, or a page fetched under an
            // extension-less URL (`docs/intro`) whose body is
            // markup — a site as its sitemap names it.
            let last = lower.rsplit('/').next().unwrap_or(&lower);
            let extensionless = !last.contains('.') && !path.is_empty();
            if lower.ends_with(".html")
                || lower.ends_with(".htm")
                || (extensionless && f.html.trim_start().starts_with('<'))
            {
                pages.insert(path, f.html);
            }
        }
        let mut dirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for path in pages.keys() {
            let mut cur = String::new();
            let segs: Vec<&str> = path.split('/').collect();
            for seg in &segs[..segs.len() - 1] {
                if !cur.is_empty() {
                    cur.push('/');
                }
                cur.push_str(seg);
                dirs.insert(cur.clone());
            }
        }
        let blank =
            |k: PageKey, parent: Option<PageKey>, kind: PageKind, name: String, path: String| {
                LightRow {
                    key: k,
                    site: sid,
                    container: Container::Pages,
                    parent,
                    kind,
                    name,
                    path,
                    title: None,
                    url: None,
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
                }
            };
        let mut by_path: BTreeMap<String, usize> = BTreeMap::new();
        let mut built: Vec<Built> = Vec::new();
        let mut entries: Vec<(String, Option<String>)> =
            dirs.into_iter().map(|d| (d, None)).collect();
        entries.extend(pages.into_iter().map(|(p, h)| (p, Some(h))));
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (path, html) in entries {
            let parent = path
                .rfind('/')
                .and_then(|i| by_path.get(&path[..i]))
                .map(|&i| built[i].row.key);
            let k = PageKey(next_key);
            next_key += 1;
            let name = path.rsplit('/').next().unwrap_or(&path).to_string();
            let Some(html) = html else {
                built.push(Built {
                    row: blank(k, parent, PageKind::Dir, name, path.clone()),
                    html: None,
                });
                by_path.insert(path, built.len() - 1);
                continue;
            };
            // The page's declared identity, read once at build from
            // the head alone — the lowering waits for the graft.
            let head = quarb_text_html::head_meta(&scraper::Html::parse_document(&html));
            let url = match head.declared_value("canonical") {
                Some(Value::Str(u)) => Some(u),
                _ => base
                    .as_ref()
                    .and_then(|b| b.join(&path).ok())
                    .map(|u| u.to_string()),
            };
            let mut row = blank(k, parent, PageKind::Page, name, path.clone());
            row.title = head.title.clone();
            row.url = url;
            row.category = head.category.clone();
            row.categories = head.category.iter().cloned().collect();
            row.tags = head.tags.clone();
            row.description = head.description.clone();
            row.modified = head.modified.clone();
            row.published = head.published.clone();
            built.push(Built {
                row,
                html: Some(html),
            });
            by_path.insert(path, built.len() - 1);
        }

        // 2. Tree order: pre-order, siblings by name (byte order).
        let mut kids: HashMap<Option<PageKey>, Vec<usize>> = HashMap::new();
        for (i, b) in built.iter().enumerate() {
            kids.entry(b.row.parent).or_default().push(i);
        }
        for v in kids.values_mut() {
            v.sort_by(|&a, &b| {
                built[a]
                    .row
                    .name
                    .as_bytes()
                    .cmp(built[b].row.name.as_bytes())
            });
        }
        let mut rank = 0u64;
        let mut stack: Vec<(usize, u32)> = kids
            .get(&None)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .rev()
            .map(|i| (i, 1))
            .collect();
        while let Some((i, depth)) = stack.pop() {
            rank += 1;
            built[i].row.tree_rank = rank;
            built[i].row.depth = depth;
            let k = built[i].row.key;
            if let Some(cs) = kids.get(&Some(k)) {
                for &c in cs.iter().rev() {
                    stack.push((c, depth + 1));
                }
            }
        }

        // 3. Categories and tags as rows of their own containers.
        let mut cat_names: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut tag_names: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, b) in built.iter().enumerate() {
            if b.row.kind != PageKind::Page {
                continue;
            }
            for c in &b.row.categories {
                cat_names.entry(c.clone()).or_default().push(i);
            }
            for t in &b.row.tags {
                tag_names.entry(t.clone()).or_default().push(i);
            }
        }
        let mut rows: Vec<LightRow> = built.iter().map(|b| b.row.clone()).collect();
        let mut members: HashMap<PageKey, Vec<PageKey>> = HashMap::new();
        let mut page_categories: HashMap<PageKey, Vec<PageKey>> = HashMap::new();
        let mut page_tags: HashMap<PageKey, Vec<PageKey>> = HashMap::new();
        let mut children: HashMap<(Container, Option<PageKey>), Vec<PageKey>> = HashMap::new();
        let mut key_of_name: HashMap<(PageKind, String), PageKey> = HashMap::new();
        for (container, names, kind) in [
            (Container::Categories, &cat_names, PageKind::Category),
            (Container::Tags, &tag_names, PageKind::Tag),
        ] {
            let mut top = Vec::new();
            for (rank, (name, pages)) in names.iter().enumerate() {
                let k = PageKey(next_key);
                next_key += 1;
                rows.push(LightRow {
                    key: k,
                    site: sid,
                    container,
                    parent: None,
                    kind,
                    name: name.clone(),
                    // The path column carries the trait spelling
                    // (`<tag:X>` / `<category:X>` name the term by it).
                    path: quarb_text::HeadMeta::trait_name(name),
                    title: Some(name.clone()),
                    url: None,
                    depth: 1,
                    tree_rank: rank as u64 + 1,
                    category: None,
                    categories: Vec::new(),
                    tags: Vec::new(),
                    description: None,
                    modified: None,
                    published: None,
                    redirect_to: None,
                    analytics: Analytics::default(),
                });
                top.push(k);
                let mut ms: Vec<PageKey> = pages.iter().map(|&i| built[i].row.key).collect();
                ms.sort_by_key(|k| rows[(k.0 - 1) as usize].tree_rank);
                members.insert(k, ms);
                key_of_name.insert((kind, name.clone()), k);
            }
            children.insert((container, None), top);
        }
        // A page's categories and tags in its own declared order.
        for b in &built {
            if b.row.kind != PageKind::Page {
                continue;
            }
            let cs: Vec<PageKey> = b
                .row
                .categories
                .iter()
                .filter_map(|c| key_of_name.get(&(PageKind::Category, c.clone())).copied())
                .collect();
            let ts: Vec<PageKey> = b
                .row
                .tags
                .iter()
                .filter_map(|t| key_of_name.get(&(PageKind::Tag, t.clone())).copied())
                .collect();
            if !cs.is_empty() {
                page_categories.insert(b.row.key, cs);
            }
            if !ts.is_empty() {
                page_tags.insert(b.row.key, ts);
            }
        }
        // the tree's child lists, in rank order
        for (parent, is) in &kids {
            let mut ks: Vec<PageKey> = is.iter().map(|&i| built[i].row.key).collect();
            ks.sort_by_key(|k| rows[(k.0 - 1) as usize].tree_rank);
            children.insert((Container::Pages, *parent), ks);
        }

        // 4. Links from the DOM, resolved against the page's URL.
        let key_by_path: HashMap<&str, PageKey> = by_path
            .iter()
            .map(|(p, &i)| (p.as_str(), built[i].row.key))
            .collect();
        let mut by_url: HashMap<String, PageKey> = HashMap::new();
        for r in &rows {
            if let Some(u) = &r.url {
                by_url.insert(u.trim_end_matches('#').to_string(), r.key);
            }
        }
        let mut out_links: HashMap<PageKey, Vec<LinkRow>> = HashMap::new();
        let mut html: HashMap<PageKey, String> = HashMap::new();
        for b in built.iter_mut() {
            let Some(h) = b.html.take() else { continue };
            let k = b.row.key;
            let page_url = b.row.url.as_deref().and_then(|u| url::Url::parse(u).ok());
            let mut seen: HashMap<PageKey, ()> = HashMap::new();
            let mut lrs = Vec::new();
            let (base_href, found) = dom_links(&h);
            // `<base href>` moves the join base (Parsoid pages
            // write `//host/wiki/`), itself relative to the page.
            let join_base = match (&page_url, base_href) {
                (Some(pu), Some(b)) => pu.join(&b).ok().or_else(|| Some(pu.clone())),
                (pu, _) => pu.clone(),
            };
            for f in found {
                let Some(pu) = &join_base else { break };
                let Ok(mut u) = pu.join(&f.href) else {
                    continue;
                };
                u.set_fragment(None);
                let to = base
                    .as_ref()
                    .and_then(|bs| site_path(&u, bs))
                    .and_then(|p| {
                        path_variants(&p)
                            .into_iter()
                            .find_map(|v| key_by_path.get(v.as_str()).copied())
                    });
                // A same-site path no page answers to is a redlink.
                let red = f.red
                    || (to.is_none()
                        && base
                            .as_ref()
                            .is_some_and(|bs| bs.host_str() == u.host_str()));
                if let Some(t) = to {
                    if t == k || seen.contains_key(&t) {
                        continue;
                    }
                    seen.insert(t, ());
                }
                lrs.push(LinkRow {
                    to,
                    to_url: u.to_string(),
                    anchor: f.anchor,
                    section: f.section,
                    red,
                    via_template: f.transcluded,
                });
            }
            out_links.insert(k, lrs);
            html.insert(k, h);
        }
        // 5. Analytics over the page graph — the same computation
        // the index-backed stores run.
        // The graph's nodes: the pages, and any category with a
        // document of its own (none from a set of files).
        let pages: Vec<PageKey> = rows
            .iter()
            .filter(|r| {
                r.kind == PageKind::Page
                    || (r.kind == PageKind::Category && html.contains_key(&r.key))
            })
            .map(|r| r.key)
            .collect();
        let mut edges: Vec<(PageKey, PageKey)> = Vec::new();
        let mut reds: HashMap<PageKey, u32> = HashMap::new();
        for (from, lrs) in &out_links {
            for l in lrs {
                if let Some(t) = l.to {
                    edges.push((*from, t));
                }
                if l.red {
                    *reds.entry(*from).or_default() += 1;
                }
            }
        }
        let computed = crate::db::analyze::compute(&pages, &edges, &reds);
        let link_count = edges.len() as u64;
        let mut in_links: HashMap<PageKey, Vec<PageKey>> = HashMap::new();
        for (from, to) in &edges {
            in_links.entry(*to).or_default().push(*from);
        }
        for v in in_links.values_mut() {
            v.sort_by_key(|k| rows[(k.0 - 1) as usize].tree_rank);
            v.dedup();
        }
        for (k, a) in computed {
            rows[(k.0 - 1) as usize].analytics = a;
        }
        let catalog = Catalog {
            sites: vec![SiteRow {
                id: sid,
                host,
                base_url: site.base_url,
                snapshot: site.snapshot,
                hierarchy: Hierarchy::Directories,
                page_count: rows.iter().filter(|r| r.kind == PageKind::Page).count() as u64,
                link_count,
            }],
            lowering: format!("quarb-text-html {}", env!("CARGO_PKG_VERSION")),
        };
        MemoryStore {
            catalog,
            rows,
            html,
            children,
            out_links,
            in_links,
            members,
            page_categories,
            page_tags,
            by_url,
        }
    }

    fn row_ref(&self, key: PageKey) -> Option<&LightRow> {
        self.rows.get((key.0.checked_sub(1)?) as usize)
    }
}

impl WebStore for MemoryStore {
    fn catalog(&self) -> &Catalog {
        &self.catalog
    }
    fn row(&self, key: PageKey) -> Option<LightRow> {
        self.row_ref(key).cloned()
    }
    fn children(
        &self,
        _site: SiteId,
        container: Container,
        parent: Option<PageKey>,
    ) -> Vec<PageKey> {
        self.children
            .get(&(container, parent))
            .cloned()
            .unwrap_or_default()
    }
    fn children_named(
        &self,
        site: SiteId,
        container: Container,
        parent: Option<PageKey>,
        name: &str,
    ) -> Vec<PageKey> {
        self.children(site, container, parent)
            .into_iter()
            .filter(|k| self.row_ref(*k).is_some_and(|r| r.name == name))
            .collect()
    }
    fn related(&self, key: PageKey, kind: LinkKind, dir: LinkDir) -> Vec<PageKey> {
        match (kind, dir) {
            (LinkKind::Link, LinkDir::Out) => self
                .out_links
                .get(&key)
                .map(|v| v.iter().filter_map(|l| l.to).collect())
                .unwrap_or_default(),
            (LinkKind::Link, LinkDir::In) => self.in_links.get(&key).cloned().unwrap_or_default(),
            (LinkKind::Category, LinkDir::Out) => {
                self.page_categories.get(&key).cloned().unwrap_or_default()
            }
            (LinkKind::Tag, LinkDir::Out) => self.page_tags.get(&key).cloned().unwrap_or_default(),
            (LinkKind::Category | LinkKind::Tag, LinkDir::In) => {
                self.members.get(&key).cloned().unwrap_or_default()
            }
            (LinkKind::Redirect, _) => Vec::new(),
        }
    }
    fn link_rows(&self, key: PageKey) -> Vec<LinkRow> {
        self.out_links.get(&key).cloned().unwrap_or_default()
    }
    fn html(&self, key: PageKey) -> Option<String> {
        self.html.get(&key).cloned()
    }
    /// Rows of `kind` below `parent`, by rank: the container's rows
    /// are in rank order already, and a subtree is a contiguous
    /// run of ranks after its root, bounded by depth.
    fn descendants_of_kind(
        &self,
        _site: SiteId,
        container: Container,
        parent: Option<PageKey>,
        kind: PageKind,
    ) -> Vec<(PageKey, u32)> {
        let (from_rank, base_depth) = match parent.and_then(|p| self.row_ref(p)) {
            Some(r) => (r.tree_rank, r.depth),
            None => (0, 0),
        };
        let mut out = Vec::new();
        let mut ranked: Vec<&LightRow> = self
            .rows
            .iter()
            .filter(|r| r.container == container && r.tree_rank > from_rank)
            .collect();
        ranked.sort_by_key(|r| r.tree_rank);
        for r in ranked {
            if r.depth <= base_depth {
                break;
            }
            if r.kind == kind {
                out.push((r.key, r.depth - base_depth));
            }
        }
        out
    }

    fn page_by_url(&self, url: &str) -> Option<PageKey> {
        let u = url.split('#').next().unwrap_or(url);
        if let Some(k) = self.by_url.get(u) {
            return Some(*k);
        }
        // A URL naming a directory lands on its index page.
        let base = url::Url::parse(&self.catalog.sites[0].base_url).ok()?;
        let parsed = url::Url::parse(u).ok()?;
        let path = site_path(&parsed, &base)?;
        let variants = path_variants(&path);
        self.rows
            .iter()
            .find(|r| {
                r.container == Container::Pages
                    && r.kind == PageKind::Page
                    && variants.contains(&r.path)
            })
            .map(|r| r.key)
    }
}

/// The paths a link target may be stored under, most literal
/// first: the path itself; a directory URL's index page
/// (`docs/` → `docs/index.html`, already applied by
/// [`site_path`]) also answers to the bare directory when the
/// site was fetched under extension-less URLs (`docs`), and an
/// extension-less target answers to its index page when the
/// site was packed as files. Both spellings of one page meet.
pub(crate) fn path_variants(path: &str) -> Vec<String> {
    let mut out = vec![path.to_string()];
    if let Some(dir) = path.strip_suffix("/index.html") {
        out.push(dir.to_string());
    } else if path == "index.html" {
        // The root has no bare spelling.
    } else {
        let last = path.rsplit('/').next().unwrap_or(path);
        if !last.contains('.') {
            out.push(format!("{path}/index.html"));
        }
    }
    out
}
