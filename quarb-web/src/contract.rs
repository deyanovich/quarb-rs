//! The web level's store contract: what a store must answer for
//! the level to navigate it, in operations rather than SQL, each
//! with its document-order rule. The in-memory stores (a tarball,
//! a directory) hold everything; a database store answers each
//! operation with an indexed statement and never loads the arbor.

use quarb::Value;

/// A site's id within a store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SiteId(pub u32);

/// A row's key within a store — opaque to the level, which
/// interns keys into handles per session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PageKey(pub u64);

/// What a row of the site's tree is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PageKind {
    /// A directory of the URL tree (no document of its own).
    Dir,
    /// A page: a document, grafted at the text level on entry.
    Page,
    /// A category the site declares (a shelf); members are the
    /// pages that declare it.
    Category,
    /// A tag the site declares; members are the pages that carry it.
    Tag,
    /// A page that stands for another (`->redirect` reaches it).
    Redirect,
}

impl PageKind {
    pub fn name(self) -> &'static str {
        match self {
            PageKind::Dir => "dir",
            PageKind::Page => "page",
            PageKind::Category => "category",
            PageKind::Tag => "tag",
            PageKind::Redirect => "redirect",
        }
    }
}

/// The three containers every site holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Container {
    /// The site's tree: directories and pages, by URL path (or by
    /// the site's declared hierarchy).
    Pages,
    /// The declared categories.
    Categories,
    /// The declared tags.
    Tags,
}

impl Container {
    pub fn name(self) -> &'static str {
        match self {
            Container::Pages => "pages",
            Container::Categories => "categories",
            Container::Tags => "tags",
        }
    }
    pub const ALL: [Container; 3] = [Container::Pages, Container::Categories, Container::Tags];
}

/// The relations a page carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinkKind {
    /// A hyperlink to another page of the store (`->link` / `<-link`).
    Link,
    /// Membership in a declared category (`->category` / `<-page`).
    Category,
    /// A declared tag (`->tag` / `<-page`).
    Tag,
    /// A redirect (`->redirect`).
    Redirect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkDir {
    Out,
    In,
}

/// How a site's tree was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hierarchy {
    /// URL path directories — the rule for any site.
    Directories,
    /// A declared hierarchy (a category tree).
    Categories,
}

/// The numbers a store keeps about a page, each defined as the
/// cardinality the corresponding axis returns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Analytics {
    pub in_degree: u32,
    pub out_degree: u32,
    pub mutual_degree: u32,
    pub pagerank: f64,
    /// Links to same-site paths no page answers to.
    pub redlinks: u32,
}

/// A row of a site's tree or of its category/tag containers —
/// everything the level needs without the document.
#[derive(Clone, Debug)]
pub struct LightRow {
    pub key: PageKey,
    pub site: SiteId,
    pub container: Container,
    pub parent: Option<PageKey>,
    pub kind: PageKind,
    /// The name the row answers to in navigation (a path segment,
    /// a category's name).
    pub name: String,
    /// The site-relative path (`guides/jq.html`), or the name for
    /// a category or tag row.
    pub path: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub depth: u32,
    /// Pre-order rank within the container — the document-order
    /// contract every enumeration follows.
    pub tree_rank: u64,
    pub category: Option<String>,
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    pub description: Option<String>,
    pub modified: Option<Value>,
    pub published: Option<Value>,
    pub redirect_to: Option<PageKey>,
    pub analytics: Analytics,
}

/// One outgoing link as the page wrote it.
#[derive(Clone, Debug)]
pub struct LinkRow {
    /// The page it lands on, when the store holds one.
    pub to: Option<PageKey>,
    /// The absolute URL as resolved.
    pub to_url: String,
    /// The anchor text.
    pub anchor: Option<String>,
    /// The lemma of the section the link sits under, if any.
    pub section: Option<String>,
    /// A link the site itself marks as leading nowhere (a wiki's
    /// redlink), or a same-site path no page answers to.
    pub red: bool,
    /// A link written by a template (a navbox), not by the page's
    /// own text.
    pub via_template: bool,
}

#[derive(Clone, Debug)]
pub struct SiteRow {
    pub id: SiteId,
    pub host: String,
    pub base_url: String,
    pub snapshot: Option<String>,
    pub hierarchy: Hierarchy,
    pub page_count: u64,
    pub link_count: u64,
}

/// What a store knows about itself.
#[derive(Clone, Debug)]
pub struct Catalog {
    pub sites: Vec<SiteRow>,
    /// The identity of the text lowering the store was built with
    /// (`quarb-text-html <version>`), the guard behind any prefilter
    /// over stored plain text.
    pub lowering: String,
}

/// The store contract. Every enumeration returns keys in tree-rank
/// order; keyed lookups are free of enumeration.
pub trait WebStore {
    fn catalog(&self) -> &Catalog;
    fn row(&self, key: PageKey) -> Option<LightRow>;
    /// The children of `parent` inside `container` (`None` = the
    /// container's top level), in tree-rank order.
    fn children(&self, site: SiteId, container: Container, parent: Option<PageKey>)
    -> Vec<PageKey>;
    /// The children named `name` — one for a tree, possibly several
    /// where a store admits duplicates.
    fn children_named(
        &self,
        site: SiteId,
        container: Container,
        parent: Option<PageKey>,
        name: &str,
    ) -> Vec<PageKey>;
    /// The pages a relation reaches from `key` (Out) or the pages
    /// that reach `key` through it (In), in document order.
    fn related(&self, key: PageKey, kind: LinkKind, dir: LinkDir) -> Vec<PageKey>;
    /// A page's outgoing links as written, in document order.
    fn link_rows(&self, key: PageKey) -> Vec<LinkRow>;
    /// The page's document.
    fn html(&self, key: PageKey) -> Option<String>;
    /// The page a URL names, if the store holds it — the landing
    /// rung of a cross-document reference.
    fn page_by_url(&self, url: &str) -> Option<PageKey>;
    /// Every row of `kind` below `parent` inside `container`
    /// (`None` = the whole container), each with its depth below
    /// `parent` (children are 1), in tree-rank order — the fast
    /// path behind `//page`. The default walks `children`; a
    /// store with a rank index answers in one statement.
    fn descendants_of_kind(
        &self,
        site: SiteId,
        container: Container,
        parent: Option<PageKey>,
        kind: PageKind,
    ) -> Vec<(PageKey, u32)> {
        let mut out = Vec::new();
        let mut stack: Vec<(Option<PageKey>, u32)> = vec![(parent, 0)];
        // Depth-first in rank order: children are already ranked,
        // so a stack of reversed child lists yields pre-order.
        while let Some((p, depth)) = stack.pop() {
            let kids = self.children(site, container, p);
            for k in kids.into_iter().rev() {
                stack.push((Some(k), depth + 1));
            }
            if let Some(k) = p
                && depth > 0
                && self.row(k).is_some_and(|r| r.kind == kind)
            {
                out.push((k, depth));
            }
        }
        out
    }
    /// A batching hint: the level is about to ask `related` for
    /// every key in `keys` in direction `dir`.
    fn prefetch(&self, _keys: &[PageKey], _dir: LinkDir) {}
    /// The rows of `kind` in `site` that carry a document — the
    /// graph's nodes for the analytics (a page always does; a
    /// category only when the site has a page for it).
    fn documented(&self, site: SiteId, kind: PageKind) -> Vec<PageKey> {
        let mut out = Vec::new();
        for c in Container::ALL {
            for (k, _) in self.descendants_of_kind(site, c, None, kind) {
                if self.html(k).is_some() {
                    out.push(k);
                }
            }
        }
        out
    }
    /// Many rows at once (a sweep's hydration); the default asks
    /// one by one, a database store answers in chunks.
    fn rows(&self, keys: &[PageKey]) -> Vec<LightRow> {
        keys.iter().filter_map(|&k| self.row(k)).collect()
    }
}
