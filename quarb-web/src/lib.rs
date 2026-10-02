//! The web level: a site as an arbor — its tree of directories
//! and pages, the categories and tags it declares, the links
//! between its pages — with each page grafted at the text level
//! on entry. A store answers the level's questions
//! ([`WebStore`]); the level owns the vocabulary, the node ids,
//! the grafts, and the crosslinks.
//!
//! ```text
//! /sites/<host>                              <site>
//! /sites/<host>/pages/<dir>/<page>           <dir>, <page>, <tag:X>, <category:X>
//! /sites/<host>/pages/<dir>/<page>/section…  the text level
//! /sites/<host>/categories/<name>            <category>; <-page lists members
//! /sites/<host>/tags/<name>                  <tag>
//! ```
//!
//! Crosslinks: `->link` / `<-link` (every hyperlink of the DOM,
//! resolved), `->category` / `->tag` (declared), `<-page` from a
//! category or tag to its members; a prose `ref` inside a page
//! resolves (`-->`) to the page it names when the store holds
//! one.

#[cfg(feature = "archive")]
pub mod archive;
pub mod contract;
pub mod db;
#[cfg(feature = "fs")]
pub mod fs;
#[cfg(feature = "ingest")]
pub mod ingest;
pub mod memory;
pub mod plan;

pub use contract::*;
pub use memory::{MemoryStore, PageFile, SiteInput};

use quarb::adapter::Provenance;
use quarb::{AstAdapter, NodeId, Value};
use quarb_text::TextModel;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

// NodeId layout: kind (bits 52–54) | handle (bits 20–51) | inner (bits 0–19).
const KIND_SHIFT: u32 = 52;
const HANDLE_SHIFT: u32 = 20;
const HANDLE_MASK: u64 = (1 << 32) - 1;
const INNER_MASK: u64 = (1 << 20) - 1;

const K_ROOT: u64 = 0;
const K_SITES: u64 = 1;
const K_SITE: u64 = 2;
const K_CONTAINER: u64 = 3;
const K_ROW: u64 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Node {
    Root,
    Sites,
    Site(SiteId),
    Container(SiteId, Container),
    /// A row of a container; `inner` > 0 is a text-level node of
    /// the page's graft (its index + 1).
    Row(PageKey, u32),
}

fn container_idx(c: Container) -> u64 {
    match c {
        Container::Pages => 0,
        Container::Categories => 1,
        Container::Tags => 2,
    }
}

fn container_of(i: u64) -> Container {
    match i {
        0 => Container::Pages,
        1 => Container::Categories,
        _ => Container::Tags,
    }
}

/// A candidate set the level's enumeration is narrowed to (the
/// planner's prefilter rung): the pages themselves and every
/// directory above one. Enumeration of the pages container is
/// filtered to them; keyed rows, crosslinks, and grafts are not.
pub struct Scope {
    pages: HashSet<PageKey>,
    ancestors: HashSet<PageKey>,
}

/// The web level over a store.
pub struct WebAdapter<S: WebStore> {
    store: S,
    scope: Option<Scope>,
    /// Page keys interned into 32-bit handles, per session.
    handles: RefCell<Vec<PageKey>>,
    by_key: RefCell<HashMap<PageKey, u32>>,
    /// Text-level grafts, one per page entered.
    grafts: RefCell<HashMap<PageKey, Rc<TextModel>>>,
    rows: RefCell<HashMap<PageKey, Rc<LightRow>>>,
    /// Crosslink lists per row, kept for the session: the engine
    /// asks for a hop's edges once per successor (to label the
    /// arrival), so a hub's list would otherwise be fetched once
    /// per backlink.
    links_out: RefCell<HashMap<PageKey, Rc<Vec<(String, NodeId)>>>>,
    links_in: RefCell<HashMap<PageKey, Rc<Vec<(String, NodeId)>>>>,
}

impl<S: WebStore> WebAdapter<S> {
    pub fn new(store: S) -> Self {
        WebAdapter {
            store,
            scope: None,
            handles: RefCell::new(Vec::new()),
            by_key: RefCell::new(HashMap::new()),
            grafts: RefCell::new(HashMap::new()),
            rows: RefCell::new(HashMap::new()),
            links_out: RefCell::new(HashMap::new()),
            links_in: RefCell::new(HashMap::new()),
        }
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    /// Narrow enumeration of the pages container to `keys` (and
    /// the directories above them). The engine still tests every
    /// predicate on what it visits, so a superset is sound.
    pub fn with_scope(mut self, keys: Vec<PageKey>) -> Self {
        // The ancestors, level by level in batches: the parents of
        // the candidates, then their parents, until none is new.
        let mut ancestors: HashSet<PageKey> = HashSet::new();
        let mut frontier: Vec<PageKey> = keys.clone();
        while !frontier.is_empty() {
            let mut next: Vec<PageKey> = Vec::new();
            for chunk in frontier.chunks(2000) {
                for r in self.store.rows(chunk) {
                    if let Some(p) = r.parent
                        && ancestors.insert(p)
                    {
                        next.push(p);
                    }
                    self.rows
                        .borrow_mut()
                        .entry(r.key)
                        .or_insert_with(|| Rc::new(r));
                }
            }
            frontier = next;
        }
        self.scope = Some(Scope {
            pages: keys.into_iter().collect(),
            ancestors,
        });
        self
    }

    pub fn scoped(&self) -> bool {
        self.scope.is_some()
    }

    /// Whether a row of the pages container is enumerated under
    /// the scope: pages in the set, directories above one.
    fn in_scope(&self, k: PageKey, r: &LightRow) -> bool {
        let Some(sc) = &self.scope else { return true };
        if r.container != Container::Pages {
            return true;
        }
        // A row that holds rows (a directory, a category in the
        // category tree) stays when it is above a candidate.
        match r.kind {
            PageKind::Dir => sc.ancestors.contains(&k),
            PageKind::Category => sc.ancestors.contains(&k) || sc.pages.contains(&k),
            _ => sc.pages.contains(&k),
        }
    }

    fn scoped_keys(&self, keys: Vec<PageKey>) -> Vec<PageKey> {
        if self.scope.is_none() {
            return keys;
        }
        keys.into_iter()
            .filter(|&k| self.row(k).is_some_and(|r| self.in_scope(k, &r)))
            .collect()
    }

    fn intern(&self, key: PageKey) -> u32 {
        if let Some(&h) = self.by_key.borrow().get(&key) {
            return h;
        }
        let mut hs = self.handles.borrow_mut();
        let h = hs.len() as u32;
        hs.push(key);
        self.by_key.borrow_mut().insert(key, h);
        h
    }

    fn encode(&self, n: Node) -> NodeId {
        let (kind, handle, inner) = match n {
            Node::Root => (K_ROOT, 0, 0),
            Node::Sites => (K_SITES, 0, 0),
            Node::Site(s) => (K_SITE, s.0 as u64, 0),
            Node::Container(s, c) => (K_CONTAINER, (s.0 as u64) << 2 | container_idx(c), 0),
            Node::Row(k, inner) => (K_ROW, self.intern(k) as u64, inner as u64),
        };
        NodeId(kind << KIND_SHIFT | (handle & HANDLE_MASK) << HANDLE_SHIFT | (inner & INNER_MASK))
    }

    fn decode(&self, id: NodeId) -> Node {
        let kind = id.0 >> KIND_SHIFT;
        let handle = (id.0 >> HANDLE_SHIFT) & HANDLE_MASK;
        let inner = (id.0 & INNER_MASK) as u32;
        match kind {
            K_ROOT => Node::Root,
            K_SITES => Node::Sites,
            K_SITE => Node::Site(SiteId(handle as u32)),
            K_CONTAINER => Node::Container(SiteId((handle >> 2) as u32), container_of(handle & 3)),
            _ => Node::Row(self.handles.borrow()[handle as usize], inner),
        }
    }

    fn row(&self, key: PageKey) -> Option<Rc<LightRow>> {
        if let Some(r) = self.rows.borrow().get(&key) {
            return Some(r.clone());
        }
        let r = Rc::new(self.store.row(key)?);
        let mut cache = self.rows.borrow_mut();
        // A bound on what a sweep holds: past it the cache starts
        // over (rows re-hydrate in chunks as the sweep goes on).
        if cache.len() >= ROW_CACHE_ROWS {
            cache.clear();
        }
        cache.insert(key, r.clone());
        Some(r)
    }

    fn site_row(&self, s: SiteId) -> Option<&SiteRow> {
        self.store.catalog().sites.iter().find(|r| r.id == s)
    }

    /// The page's text-level graft, parsed on first entry.
    fn graft(&self, key: PageKey) -> Option<Rc<TextModel>> {
        if let Some(g) = self.grafts.borrow().get(&key) {
            return Some(g.clone());
        }
        let html = self.store.html(key)?;
        let mut model = quarb_text_html::parse(&html);
        if let Some(row) = self.row(key) {
            if let Some(u) = &row.url {
                model.set_document_url(u);
            }
            model.set_document_path(&row.path);
        }
        let g = Rc::new(model);
        self.grafts.borrow_mut().insert(key, g.clone());
        Some(g)
    }

    /// The rows a crosslink hop is about to read, hydrated in one
    /// statement per chunk (a hub's six thousand backlinks are one
    /// round trip, not six thousand).
    fn hydrate(&self, keys: &[PageKey]) {
        let missing: Vec<PageKey> = {
            let cache = self.rows.borrow();
            keys.iter()
                .copied()
                .filter(|k| !cache.contains_key(k))
                .collect()
        };
        if missing.len() < 2 {
            return;
        }
        for chunk in missing.chunks(2000) {
            let rows = self.store.rows(chunk);
            let mut cache = self.rows.borrow_mut();
            if cache.len() + rows.len() > ROW_CACHE_ROWS {
                cache.clear();
            }
            for r in rows {
                cache.insert(r.key, Rc::new(r));
            }
        }
    }

    /// A text-level node of `key`'s graft as a level node.
    fn inner_id(&self, key: PageKey, n: NodeId) -> NodeId {
        self.encode(Node::Row(key, n.0 as u32 + 1))
    }

    fn page_node(&self, key: PageKey) -> NodeId {
        self.encode(Node::Row(key, 0))
    }

    /// The row a level node belongs to, with the graft node when
    /// inside a page: the page node maps to the graft's root.
    fn split(&self, id: NodeId) -> Option<(PageKey, Option<NodeId>)> {
        match self.decode(id) {
            Node::Row(k, 0) => Some((k, None)),
            Node::Row(k, i) => Some((k, Some(NodeId(i as u64 - 1)))),
            _ => None,
        }
    }

    /// The graft node a level node stands for — the page node is
    /// the graft's document root.
    fn graft_node(&self, id: NodeId) -> Option<(PageKey, Rc<TextModel>, NodeId)> {
        let (k, inner) = self.split(id)?;
        let g = self.graft(k)?;
        let n = inner.unwrap_or(g.root());
        Some((k, g, n))
    }

    fn map_back(&self, key: PageKey, g: &TextModel, n: NodeId) -> NodeId {
        if n == g.root() {
            self.page_node(key)
        } else {
            self.inner_id(key, n)
        }
    }

    /// A human-readable locator: `/sites/<host>/pages/<path>`, with
    /// the text-level path after `!` inside a page.
    pub fn locator(&self, id: NodeId) -> String {
        match self.decode(id) {
            Node::Root => "/".to_string(),
            Node::Sites => "/sites".to_string(),
            Node::Site(s) => format!("/sites/{}", self.host(s)),
            Node::Container(s, c) => format!("/sites/{}/{}", self.host(s), c.name()),
            Node::Row(k, inner) => {
                let Some(r) = self.row(k) else {
                    return format!("/?{}", k.0);
                };
                let base = format!(
                    "/sites/{}/{}/{}",
                    self.host(r.site),
                    r.container.name(),
                    r.path
                );
                if inner == 0 {
                    return base;
                }
                match self.graft(k) {
                    Some(g) => format!("{base}!{}", g.locator(NodeId(inner as u64 - 1))),
                    None => base,
                }
            }
        }
    }

    fn host(&self, s: SiteId) -> String {
        self.site_row(s).map(|r| r.host.clone()).unwrap_or_default()
    }

    fn stem(name: &str) -> &str {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".html") {
            &name[..name.len() - 5]
        } else if lower.ends_with(".htm") {
            &name[..name.len() - 4]
        } else {
            name
        }
    }

    /// A page's children are its document; a row holding rows (a
    /// directory, a category) lists them first and, when it has a
    /// document of its own (a category page), that document after.
    fn row_children(&self, r: &LightRow) -> Vec<NodeId> {
        if r.kind == PageKind::Page {
            let Some(g) = self.graft(r.key) else {
                return Vec::new();
            };
            return g
                .children(g.root())
                .into_iter()
                .map(|n| self.inner_id(r.key, n))
                .collect();
        }
        let kids = self.store.children(r.site, r.container, Some(r.key));
        let mut out: Vec<NodeId> = self
            .scoped_keys(kids)
            .into_iter()
            .map(|k| self.page_node(k))
            .collect();
        if r.kind == PageKind::Category
            && let Some(g) = self.graft(r.key)
        {
            out.extend(
                g.children(g.root())
                    .into_iter()
                    .map(|n| self.inner_id(r.key, n)),
            );
        }
        out
    }

    /// The page a URL names, as a level node, landing on the
    /// fragment's bearer when the URL carries one.
    fn page_for_url(&self, url: &str) -> Option<NodeId> {
        let key = self.store.page_by_url(url)?;
        if let Some((_, frag)) = url.split_once('#')
            && !frag.is_empty()
            && let Some(g) = self.graft(key)
            && let Some(n) = g.resolve_fragment(g.root(), frag)
        {
            return Some(self.map_back(key, &g, n));
        }
        Some(self.page_node(key))
    }

    fn instant_of(v: &Option<Value>) -> Option<(i64, u32, Option<i16>)> {
        match v {
            Some(Value::Instant {
                secs,
                nanos,
                offset_min,
            }) => Some((*secs, *nanos, *offset_min)),
            _ => None,
        }
    }
}

/// The most rows the level keeps hydrated at once (a light row is
/// a few hundred bytes: some hundred megabytes at the bound).
const ROW_CACHE_ROWS: usize = 500_000;
/// The most rows whose crosslink lists stay memoized.
const LINK_MEMO_ROWS: usize = 100_000;

/// Page properties a site declares about a page (a wiki's
/// disambiguation, hidden-category, and noindex marks), carried
/// as tags and worn as bare traits.
const PAGE_PROPS: &[&str] = &["disambiguation", "hiddencat", "noindex"];

/// The kind a bare name denotes, when it is one of the level's
/// own words.
fn kind_named(name: &str) -> Option<PageKind> {
    Some(match name {
        "page" => PageKind::Page,
        "dir" => PageKind::Dir,
        "category" => PageKind::Category,
        "tag" => PageKind::Tag,
        "redirect" => PageKind::Redirect,
        _ => return None,
    })
}

fn list(v: &[String]) -> Value {
    Value::list(v.iter().cloned().map(Value::Str).collect())
}

impl<S: WebStore> AstAdapter for WebAdapter<S> {
    fn root(&self) -> NodeId {
        self.encode(Node::Root)
    }

    fn children(&self, id: NodeId) -> Vec<NodeId> {
        match self.decode(id) {
            Node::Root => vec![self.encode(Node::Sites)],
            Node::Sites => self
                .store
                .catalog()
                .sites
                .iter()
                .map(|s| self.encode(Node::Site(s.id)))
                .collect(),
            Node::Site(s) => Container::ALL
                .iter()
                .map(|&c| self.encode(Node::Container(s, c)))
                .collect(),
            Node::Container(s, c) => {
                let kids = self.store.children(s, c, None);
                self.scoped_keys(kids)
                    .into_iter()
                    .map(|k| self.page_node(k))
                    .collect()
            }
            Node::Row(k, 0) => match self.row(k) {
                Some(r) => self.row_children(&r),
                None => Vec::new(),
            },
            Node::Row(k, _) => {
                let Some((_, g, n)) = self.graft_node(id) else {
                    return Vec::new();
                };
                g.children(n)
                    .into_iter()
                    .map(|c| self.inner_id(k, c))
                    .collect()
            }
        }
    }

    fn name(&self, id: NodeId) -> Option<String> {
        match self.decode(id) {
            Node::Root => None,
            Node::Sites => Some("sites".to_string()),
            Node::Site(s) => Some(self.host(s)),
            Node::Container(_, c) => Some(c.name().to_string()),
            Node::Row(k, 0) => self.row(k).map(|r| r.name.clone()),
            Node::Row(..) => {
                let (_, g, n) = self.graft_node(id)?;
                g.name(n)
            }
        }
    }

    fn answers_to(&self, id: NodeId, name: &str) -> bool {
        match self.decode(id) {
            Node::Row(k, 0) => match self.row(k) {
                // A row answers to its kind (`//page`, `//dir`,
                // `//tag`), to its path segment, to the segment's
                // stem, and to its title.
                Some(r) => {
                    r.kind.name() == name
                        || r.name == name
                        || (r.kind == PageKind::Page && Self::stem(&r.name) == name)
                        || r.title.as_deref() == Some(name)
                }
                None => false,
            },
            Node::Row(..) => match self.graft_node(id) {
                Some((_, g, n)) => g.answers_to(n, name),
                None => false,
            },
            _ => self.name(id).as_deref() == Some(name),
        }
    }

    fn children_named(&self, id: NodeId, name: &str) -> Vec<NodeId> {
        match self.decode(id) {
            Node::Container(s, c) => {
                if kind_named(name).is_some() {
                    return self
                        .children(id)
                        .into_iter()
                        .filter(|c| self.answers_to(*c, name))
                        .collect();
                }
                let mut ks = self.store.children_named(s, c, None, name);
                if ks.is_empty() && c == Container::Pages {
                    ks = self
                        .store
                        .children_named(s, c, None, &format!("{name}.html"));
                }
                self.scoped_keys(ks)
                    .into_iter()
                    .map(|k| self.page_node(k))
                    .collect()
            }
            Node::Row(k, 0) => {
                let Some(r) = self.row(k) else {
                    return Vec::new();
                };
                if r.kind == PageKind::Page {
                    let Some(g) = self.graft(k) else {
                        return Vec::new();
                    };
                    return g
                        .children_named(g.root(), name)
                        .into_iter()
                        .map(|n| self.inner_id(k, n))
                        .collect();
                }
                if kind_named(name).is_some() {
                    return self
                        .row_children(&r)
                        .into_iter()
                        .filter(|c| self.answers_to(*c, name))
                        .collect();
                }
                let mut ks = self
                    .store
                    .children_named(r.site, r.container, Some(k), name);
                if ks.is_empty() && r.container == Container::Pages {
                    ks = self.store.children_named(
                        r.site,
                        r.container,
                        Some(k),
                        &format!("{name}.html"),
                    );
                }
                let mut out: Vec<NodeId> = self
                    .scoped_keys(ks)
                    .into_iter()
                    .map(|c| self.page_node(c))
                    .collect();
                if r.kind == PageKind::Category
                    && let Some(g) = self.graft(k)
                {
                    out.extend(
                        g.children_named(g.root(), name)
                            .into_iter()
                            .map(|n| self.inner_id(k, n)),
                    );
                }
                out
            }
            Node::Row(k, _) => {
                let Some((_, g, n)) = self.graft_node(id) else {
                    return Vec::new();
                };
                g.children_named(n, name)
                    .into_iter()
                    .map(|c| self.inner_id(k, c))
                    .collect()
            }
            _ => self
                .children(id)
                .into_iter()
                .filter(|c| self.answers_to(*c, name))
                .collect(),
        }
    }

    fn parent(&self, id: NodeId) -> Option<NodeId> {
        match self.decode(id) {
            Node::Root => None,
            Node::Sites => Some(self.root()),
            Node::Site(_) => Some(self.encode(Node::Sites)),
            Node::Container(s, _) => Some(self.encode(Node::Site(s))),
            Node::Row(k, 0) => {
                let r = self.row(k)?;
                Some(match r.parent {
                    Some(p) => self.page_node(p),
                    None => self.encode(Node::Container(r.site, r.container)),
                })
            }
            Node::Row(k, _) => {
                let (_, g, n) = self.graft_node(id)?;
                let p = g.parent(n)?;
                Some(self.map_back(k, &g, p))
            }
        }
    }

    fn traits(&self, id: NodeId) -> Vec<String> {
        match self.decode(id) {
            Node::Site(_) => vec!["site".to_string()],
            Node::Row(k, 0) => {
                let Some(r) = self.row(k) else {
                    return Vec::new();
                };
                let mut out = vec![r.kind.name().to_string()];
                if r.kind == PageKind::Category {
                    for t in &r.tags {
                        if PAGE_PROPS.contains(&t.as_str()) {
                            out.push(t.clone());
                        }
                    }
                }
                if r.kind == PageKind::Page {
                    // The declared identity, each in its namespace,
                    // spelled as declared.
                    if let Some(c) = &r.category {
                        out.push(format!("category:{}", quarb_text::HeadMeta::trait_name(c)));
                    }
                    for t in &r.tags {
                        out.push(format!("tag:{}", quarb_text::HeadMeta::trait_name(t)));
                        // A page property the site declares about
                        // the page itself is a bare trait too.
                        if PAGE_PROPS.contains(&t.as_str()) {
                            out.push(t.clone());
                        }
                    }
                    if r.analytics.in_degree == 0 {
                        out.push("orphan".to_string());
                    }
                    // …plus what the text level says of the document
                    // (`<table>` on a denormalized list and the like).
                    if let Some(g) = self.graft(k) {
                        for t in g.traits(g.root()) {
                            if !out.contains(&t) {
                                out.push(t);
                            }
                        }
                    }
                }
                out
            }
            Node::Row(..) => match self.graft_node(id) {
                Some((_, g, n)) => g.traits(n),
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn has_trait(&self, id: NodeId, name: &str) -> bool {
        match self.decode(id) {
            Node::Row(k, 0) => {
                let Some(r) = self.row(k) else { return false };
                if name == r.kind.name() {
                    return true;
                }
                if r.kind == PageKind::Category && PAGE_PROPS.contains(&name) {
                    return r.tags.iter().any(|t| t == name);
                }
                if r.kind != PageKind::Page {
                    return false;
                }
                if let Some(t) = name.strip_prefix("tag:") {
                    return r
                        .tags
                        .iter()
                        .any(|x| quarb_text::HeadMeta::trait_name(x) == t);
                }
                if let Some(c) = name.strip_prefix("category:") {
                    return r
                        .category
                        .as_deref()
                        .is_some_and(|x| quarb_text::HeadMeta::trait_name(x) == c);
                }
                if name == "orphan" {
                    return r.analytics.in_degree == 0;
                }
                if PAGE_PROPS.contains(&name) {
                    return r.tags.iter().any(|t| t == name);
                }
                self.graft(k).is_some_and(|g| g.has_trait(g.root(), name))
            }
            Node::Row(..) => match self.graft_node(id) {
                Some((_, g, n)) => g.has_trait(n, name),
                None => false,
            },
            _ => self.traits(id).iter().any(|t| t == name),
        }
    }

    fn property(&self, id: NodeId, name: &str) -> Option<Value> {
        match self.decode(id) {
            Node::Root | Node::Sites | Node::Container(..) => None,
            Node::Site(s) => {
                let r = self.site_row(s)?;
                match name {
                    "host" => Some(Value::Str(r.host.clone())),
                    "href" | "url" => Some(Value::Str(r.base_url.clone())),
                    "snapshot" => r.snapshot.clone().map(Value::Str),
                    "pages" => Some(Value::Int(r.page_count as i64)),
                    "links" => Some(Value::Int(r.link_count as i64)),
                    _ => None,
                }
            }
            Node::Row(k, 0) => {
                let r = self.row(k)?;
                let a = &r.analytics;
                let v = match name {
                    "title" => r.title.clone().map(Value::Str),
                    "href" | "url" => r.url.clone().map(Value::Str),
                    "path" => Some(Value::Str(r.path.clone())),
                    "category" => r.category.clone().map(Value::Str),
                    "categories" => Some(list(&r.categories)),
                    "tags" => Some(list(&r.tags)),
                    "description" => r.description.clone().map(Value::Str),
                    "published" => r.published.clone(),
                    "modified" => r.modified.clone(),
                    "depth" => Some(Value::Int(r.depth as i64)),
                    "in_degree" | "in-degree" => Some(Value::Int(a.in_degree as i64)),
                    "out_degree" | "out-degree" => Some(Value::Int(a.out_degree as i64)),
                    "mutual_degree" | "mutual-degree" => Some(Value::Int(a.mutual_degree as i64)),
                    "pagerank" => Some(Value::Float(a.pagerank)),
                    "redlinks" => Some(Value::Int(a.redlinks as i64)),
                    "link" if r.kind == PageKind::Page => Some(Value::Record(vec![
                        (
                            "title".to_string(),
                            r.title.clone().map(Value::Str).unwrap_or(Value::Null),
                        ),
                        (
                            "href".to_string(),
                            r.url.clone().map(Value::Str).unwrap_or(Value::Null),
                        ),
                        (
                            "text".to_string(),
                            r.description.clone().map(Value::Str).unwrap_or(Value::Null),
                        ),
                    ])),
                    _ => None,
                };
                if v.is_some() || !matches!(r.kind, PageKind::Page | PageKind::Category) {
                    return v;
                }
                let g = self.graft(k)?;
                g.property(g.root(), name)
            }
            Node::Row(..) => {
                let (_, g, n) = self.graft_node(id)?;
                g.property(n, name)
            }
        }
    }

    fn default_value(&self, id: NodeId) -> Option<Value> {
        match self.decode(id) {
            Node::Row(k, 0) => {
                let r = self.row(k)?;
                match r.kind {
                    PageKind::Page => {
                        let g = self.graft(k)?;
                        g.default_value(g.root())
                    }
                    // A category page: its document's text when it
                    // has one, else its name.
                    PageKind::Category => match self.graft(k) {
                        Some(g) => g
                            .default_value(g.root())
                            .or_else(|| r.title.clone().map(Value::Str)),
                        None => r.title.clone().map(Value::Str),
                    },
                    _ => r.title.clone().map(Value::Str),
                }
            }
            Node::Row(..) => {
                let (_, g, n) = self.graft_node(id)?;
                g.default_value(n)
            }
            _ => None,
        }
    }

    fn metadata(&self, id: NodeId, key: &str) -> Option<Value> {
        match self.decode(id) {
            Node::Root => match key {
                "n-sites" => Some(Value::Int(self.store.catalog().sites.len() as i64)),
                "lowering" => Some(Value::Str(self.store.catalog().lowering.clone())),
                _ => None,
            },
            Node::Site(s) => {
                let r = self.site_row(s)?;
                match key {
                    "n-pages" => Some(Value::Int(r.page_count as i64)),
                    "n-links" => Some(Value::Int(r.link_count as i64)),
                    "base" => Some(Value::Str(r.base_url.clone())),
                    "hierarchy" => Some(Value::Str(
                        match r.hierarchy {
                            Hierarchy::Directories => "directories",
                            Hierarchy::Categories => "categories",
                        }
                        .to_string(),
                    )),
                    _ => None,
                }
            }
            Node::Row(k, 0) => {
                let r = self.row(k)?;
                match key {
                    "path" => Some(Value::Str(r.path.clone())),
                    "depth" => Some(Value::Int(r.depth as i64)),
                    "rank" => Some(Value::Int(r.tree_rank as i64)),
                    "kind" => Some(Value::Str(r.kind.name().to_string())),
                    "key" => Some(Value::Int(r.key.0 as i64)),
                    _ if r.kind == PageKind::Page => {
                        let g = self.graft(k)?;
                        g.metadata(g.root(), key)
                    }
                    _ => None,
                }
            }
            Node::Row(..) => {
                let (_, g, n) = self.graft_node(id)?;
                g.metadata(n, key)
            }
            _ => None,
        }
    }

    fn aliased_metadata(&self, id: NodeId) -> &'static [&'static str] {
        match self.decode(id) {
            Node::Row(_, i) if i > 0 => &["level", "lang", "form"],
            _ => &[],
        }
    }

    fn links(&self, id: NodeId) -> Vec<(String, NodeId)> {
        match self.decode(id) {
            Node::Row(k, 0) => {
                if let Some(v) = self.links_out.borrow().get(&k) {
                    return v.as_ref().clone();
                }
                let Some(r) = self.row(k) else {
                    return Vec::new();
                };
                let mut out = Vec::new();
                match r.kind {
                    PageKind::Page | PageKind::Redirect => {
                        let ts = self.store.related(k, LinkKind::Link, LinkDir::Out);
                        self.hydrate(&ts);
                        for t in ts {
                            out.push(("link".to_string(), self.page_node(t)));
                        }
                        for t in self.store.related(k, LinkKind::Category, LinkDir::Out) {
                            out.push(("category".to_string(), self.page_node(t)));
                        }
                        for t in self.store.related(k, LinkKind::Tag, LinkDir::Out) {
                            out.push(("tag".to_string(), self.page_node(t)));
                        }
                        if let Some(t) = r.redirect_to {
                            out.push(("redirect".to_string(), self.page_node(t)));
                        }
                    }
                    // A category page links like any page, and its
                    // declared parents are the DAG above it.
                    PageKind::Category => {
                        let ts = self.store.related(k, LinkKind::Link, LinkDir::Out);
                        self.hydrate(&ts);
                        for t in ts {
                            out.push(("link".to_string(), self.page_node(t)));
                        }
                        for t in self.store.related(k, LinkKind::Category, LinkDir::Out) {
                            out.push(("category".to_string(), self.page_node(t)));
                        }
                    }
                    _ => {}
                }
                let mut memo = self.links_out.borrow_mut();
                if memo.len() >= LINK_MEMO_ROWS {
                    memo.clear();
                }
                memo.insert(k, Rc::new(out.clone()));
                out
            }
            Node::Row(k, _) => {
                let Some((_, g, n)) = self.graft_node(id) else {
                    return Vec::new();
                };
                g.links(n)
                    .into_iter()
                    .map(|(l, t)| (l, self.map_back(k, &g, t)))
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn backlinks(&self, id: NodeId) -> Vec<(String, NodeId)> {
        match self.decode(id) {
            Node::Row(k, 0) => {
                if let Some(v) = self.links_in.borrow().get(&k) {
                    return v.as_ref().clone();
                }
                let Some(r) = self.row(k) else {
                    return Vec::new();
                };
                let mut out = Vec::new();
                match r.kind {
                    PageKind::Page | PageKind::Redirect => {
                        let ss = self.store.related(k, LinkKind::Link, LinkDir::In);
                        self.hydrate(&ss);
                        for s in ss {
                            out.push(("link".to_string(), self.page_node(s)));
                        }
                        for s in self.store.related(k, LinkKind::Redirect, LinkDir::In) {
                            out.push(("redirect".to_string(), self.page_node(s)));
                        }
                    }
                    // Members arrive under their own kind: `<-page`
                    // the pages, `<-category` the subcategories; a
                    // category page is linked to like any page.
                    PageKind::Category | PageKind::Tag => {
                        if r.kind == PageKind::Category {
                            let ss = self.store.related(k, LinkKind::Link, LinkDir::In);
                            self.hydrate(&ss);
                            for s in ss {
                                out.push(("link".to_string(), self.page_node(s)));
                            }
                        }
                        let kind = if r.kind == PageKind::Category {
                            LinkKind::Category
                        } else {
                            LinkKind::Tag
                        };
                        for s in self.store.related(k, kind, LinkDir::In) {
                            let label = self.row(s).map(|m| m.kind.name()).unwrap_or("page");
                            out.push((label.to_string(), self.page_node(s)));
                        }
                    }
                    PageKind::Dir => {}
                }
                let mut memo = self.links_in.borrow_mut();
                if memo.len() >= LINK_MEMO_ROWS {
                    memo.clear();
                }
                memo.insert(k, Rc::new(out.clone()));
                out
            }
            Node::Row(k, _) => {
                let Some((_, g, n)) = self.graft_node(id) else {
                    return Vec::new();
                };
                g.backlinks(n)
                    .into_iter()
                    .map(|(l, t)| (l, self.map_back(k, &g, t)))
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// Inside a page the text level resolves in-document refs; a
    /// ref that leaves the document lands on the page the store
    /// holds under that URL (its fragment's bearer when named).
    fn resolve(&self, id: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        match self.decode(id) {
            Node::Row(k, 0) => {
                let r = self.row(k)?;
                match property {
                    "redirect" => r.redirect_to.map(|t| self.page_node(t)),
                    "category" => self
                        .store
                        .related(k, LinkKind::Category, LinkDir::Out)
                        .first()
                        .map(|t| self.page_node(*t)),
                    _ => None,
                }
            }
            Node::Row(k, _) => {
                let (_, g, n) = self.graft_node(id)?;
                if let Some(t) = g.resolve(n, property, hint) {
                    return Some(self.map_back(k, &g, t));
                }
                let url = g.external_ref(n, property, hint)?;
                self.page_for_url(&url)
            }
            _ => None,
        }
    }

    fn external_ref(&self, id: NodeId, property: &str, hint: Option<&str>) -> Option<String> {
        let (_, g, n) = self.graft_node(id)?;
        let url = g.external_ref(n, property, hint)?;
        // A URL the store answers is not external to this arbor.
        if self.store.page_by_url(&url).is_some() {
            return None;
        }
        Some(url)
    }

    fn resolve_fragment(&self, id: NodeId, fragment: &str) -> Option<NodeId> {
        let (k, g, n) = self.graft_node(id)?;
        let t = g.resolve_fragment(n, fragment)?;
        Some(self.map_back(k, &g, t))
    }

    fn ref_property(&self, id: NodeId) -> Option<String> {
        match self.decode(id) {
            Node::Row(_, 0) => None,
            Node::Row(..) => {
                let (_, g, n) = self.graft_node(id)?;
                g.ref_property(n)
            }
            _ => None,
        }
    }

    fn ref_label(&self, id: NodeId, property: &str) -> Option<String> {
        let (_, g, n) = self.graft_node(id)?;
        g.ref_label(n, property)
    }

    /// A URL the store holds lands on its page — the arrow's
    /// landing rung without a host registration.
    fn document_by_ref(&self, id: &str) -> Option<NodeId> {
        self.page_for_url(id)
    }

    /// `//page`, `//dir`, `//tag`, `//category` below the root, the
    /// sites, a site, a container, or a directory, from the store's
    /// rank order — no walk of the tree.
    fn descendants_named(&self, id: NodeId, name: &str) -> Option<Vec<(NodeId, usize)>> {
        let kind = kind_named(name)?;
        let sites: Vec<SiteId> = self.store.catalog().sites.iter().map(|s| s.id).collect();
        // (site, container, parent, depth offset from `id`)
        let scopes: Vec<(SiteId, Container, Option<PageKey>, usize)> = match self.decode(id) {
            Node::Root => sites
                .iter()
                .flat_map(|&s| Container::ALL.iter().map(move |&c| (s, c, None, 3)))
                .collect(),
            Node::Sites => sites
                .iter()
                .flat_map(|&s| Container::ALL.iter().map(move |&c| (s, c, None, 2)))
                .collect(),
            Node::Site(s) => Container::ALL.iter().map(|&c| (s, c, None, 1)).collect(),
            Node::Container(s, c) => vec![(s, c, None, 0)],
            // A directory, or a category holding rows beneath it (the
            // category tree): its subtree is one rank range.
            Node::Row(k, 0) => {
                let r = self.row(k)?;
                if !matches!(r.kind, PageKind::Dir | PageKind::Category) {
                    return None;
                }
                vec![(r.site, r.container, Some(k), 0)]
            }
            Node::Row(..) => return None,
        };
        let mut out = Vec::new();
        for (s, c, parent, offset) in scopes {
            // Under a scope the whole container is not enumerated:
            // the candidates (and the rows above them) are known,
            // their rows already in hand, and rank order is theirs.
            let mut found = match (&self.scope, parent) {
                (Some(sc), None) if c == Container::Pages => {
                    let cache = self.rows.borrow();
                    let mut v: Vec<(&Rc<LightRow>, u32)> = sc
                        .pages
                        .iter()
                        .chain(sc.ancestors.iter())
                        .filter_map(|k| cache.get(k))
                        .filter(|r| r.site == s && r.container == c && r.kind == kind)
                        .map(|r| (r, r.depth))
                        .collect();
                    v.sort_by_key(|(r, _)| r.tree_rank);
                    v.into_iter().map(|(r, d)| (r.key, d)).collect()
                }
                _ => self.store.descendants_of_kind(s, c, parent, kind),
            };
            if let Some(sc) = &self.scope
                && c == Container::Pages
            {
                found.retain(|(k, _)| match kind {
                    PageKind::Dir => sc.ancestors.contains(k),
                    PageKind::Category => sc.ancestors.contains(k) || sc.pages.contains(k),
                    _ => sc.pages.contains(k),
                });
            }
            // Hydrate the rows the sweep is about to read, in chunks.
            let missing: Vec<PageKey> = {
                let cache = self.rows.borrow();
                found
                    .iter()
                    .map(|(k, _)| *k)
                    .filter(|k| !cache.contains_key(k))
                    .collect()
            };
            for chunk in missing.chunks(2000) {
                let rows = self.store.rows(chunk);
                let mut cache = self.rows.borrow_mut();
                if cache.len() + rows.len() > ROW_CACHE_ROWS {
                    cache.clear();
                }
                for r in rows {
                    cache.insert(r.key, Rc::new(r));
                }
            }
            for (k, d) in found {
                out.push((self.page_node(k), d as usize + offset));
            }
        }
        Some(out)
    }

    fn prefetch_links(&self, nodes: &[NodeId], dir: quarb::LinkDir) {
        let keys: Vec<PageKey> = nodes
            .iter()
            .filter_map(|&n| match self.decode(n) {
                Node::Row(k, 0) => Some(k),
                _ => None,
            })
            .collect();
        if !keys.is_empty() {
            let d = match dir {
                quarb::LinkDir::Out => LinkDir::Out,
                quarb::LinkDir::In => LinkDir::In,
            };
            self.store.prefetch(&keys, d);
        }
    }

    fn provenance(&self, id: NodeId) -> Provenance {
        match self.decode(id) {
            Node::Site(s) => Provenance {
                source: self.site_row(s).map(|r| r.base_url.clone()),
                ..Default::default()
            },
            Node::Row(k, _) => match self.row(k) {
                Some(r) if r.kind == PageKind::Page => {
                    let instant = Self::instant_of(&r.modified);
                    Provenance {
                        source: r.url.clone(),
                        path: Some("/".to_string()),
                        instant_from: instant.map(|_| quarb::InstantFrom::Node),
                        instant,
                        dpid: Some(r.path.clone()),
                    }
                }
                _ => Provenance::default(),
            },
            _ => Provenance::default(),
        }
    }
}
