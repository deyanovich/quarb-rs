//! Adapter composition: descend *through* a leaf into its content.
//!
//! A filesystem's `.json` file, an archive's `config.xml` entry, a
//! message's HTML part — trees of files are full of leaves whose
//! *content* is itself a tree in another notation. Composition
//! grafts that inner tree onto the outer one: wrap any adapter in
//! a [`ComposeAdapter`] and a leaf whose name or content says
//! "parse me" gains the parsed arbor as its children:
//!
//! ```text
//! /data/store.json/books/*[/price:: < 12]/title::
//!       └ outer (fs) ┘└──── inner (json) ────┘
//! ```
//!
//! The graft happens on first touch, lazily: a leaf is only read
//! and parsed when navigation actually enters it. Detection is by
//! extension (`.json`, `.xml`, `.html`/`.htm`/`.xhtml`/`.svg`,
//! `.csv`/`.tsv`), else by sniffing the content's first
//! character; a parse failure simply leaves the leaf a leaf.
//! Archive leaves (`.zip`, `.tar`, `.tar.gz`) are binary, so they
//! graft by *path* rather than content — available when the outer
//! substrate has real paths (see
//! [`ComposeAdapter::with_source_paths`]) — and the grafted
//! archive composes in turn, so one path runs filesystem → tar →
//! JSON without a seam. The
//! inner root is *identified with* the outer leaf node — the leaf
//! itself answers the inner root's children — so there is no
//! phantom node between the file and its content.
//!
//! Locators show the boundary with a bang, jar-URL style:
//! `store.json!/books/1`.

use quarb::{AstAdapter, NodeId, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

/// A parsed inner arbor.
enum Inner {
    Json(quarb_json::JsonAdapter),
    Xml(quarb_xml::XmlAdapter),
    Html(quarb_html::HtmlAdapter),
    Csv(quarb_csv::CsvAdapter),
    Code(quarb_code::CodeAdapter),
    /// A grafted archive, itself composed so its own parseable
    /// entries graft in turn.
    Archive(Box<ComposeAdapter<quarb_archive::ArchiveAdapter>>),
}

impl Inner {
    fn adapter(&self) -> &dyn AstAdapter {
        match self {
            Inner::Json(a) => a,
            Inner::Xml(a) => a,
            Inner::Html(a) => a,
            Inner::Csv(a) => a,
            Inner::Code(a) => a,
            Inner::Archive(a) => &**a,
        }
    }

    fn locator(&self, node: NodeId) -> String {
        match self {
            Inner::Json(a) => a.pointer(node),
            Inner::Xml(a) => a.locator(node),
            Inner::Html(a) => a.locator(node),
            Inner::Csv(a) => a.locator(node),
            Inner::Code(a) => a.locator(node),
            Inner::Archive(a) => a.locator(node, |o| a.outer().locator(o)),
        }
    }
}

/// Names [`quarb_archive::ArchiveAdapter::open`] can take: the
/// zip family, tar, and gzip (`.gz` alone included — the open
/// checks magic bytes, and a failure leaves the leaf a leaf).
fn archive_name(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(ext.as_str(), "zip" | "tar" | "tgz" | "gz")
}

/// Parse `content` by `name`'s extension, else by sniffing.
fn parse_inner(name: &str, content: &str) -> Option<Inner> {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "json" => {
            return quarb_json::JsonAdapter::parse(content)
                .ok()
                .map(Inner::Json);
        }
        "xml" | "svg" | "xhtml" => {
            return quarb_xml::XmlAdapter::parse(content).ok().map(Inner::Xml);
        }
        "html" | "htm" => return Some(Inner::Html(quarb_html::HtmlAdapter::parse(content))),
        "yaml" | "yml" => return quarb_yaml::parse(content).ok().map(Inner::Json),
        "toml" => return quarb_toml::parse(content).ok().map(Inner::Json),
        "md" | "markdown" => return Some(Inner::Html(quarb_markdown::parse(content))),
        "csv" => return quarb_csv::CsvAdapter::parse(content).ok().map(Inner::Csv),
        "tsv" => {
            return quarb_csv::CsvAdapter::parse_with_delimiter(content, b'\t')
                .ok()
                .map(Inner::Csv);
        }
        ext if quarb_code::supported(ext) => {
            return quarb_code::CodeAdapter::parse(content, ext)
                .ok()
                .map(Inner::Code);
        }
        _ => {}
    }
    // Content sniff for extensionless names.
    let t = content.trim_start();
    if t.starts_with('{') || t.starts_with('[') {
        return quarb_json::JsonAdapter::parse(content)
            .ok()
            .map(Inner::Json);
    }
    if t.starts_with("<?xml") {
        return quarb_xml::XmlAdapter::parse(content).ok().map(Inner::Xml);
    }
    None
}

/// One graft: an inner arbor mounted at an outer leaf.
struct Graft {
    outer: NodeId,
    inner: Inner,
}

/// Grafted node ids carry this tag bit; the rest indexes the intern
/// table. The bit sits at the top of the 56-bit id window that
/// `MountAdapter` leaves an inner adapter — it reserves the high byte
/// (bits 56–63) for the mount index — so a grafted id survives being
/// packed into a mount and still round-trips. It must stay below bit
/// 56 for that reason; the top bit (1 << 63) collided with the mount
/// byte. Outer adapters use small sequential indices, far below this
/// bit, so a set bit 55 unambiguously marks a graft.
const GRAFT_BIT: u64 = 1 << 55;

/// Any adapter, with parseable leaf content grafted as subtrees.
pub struct ComposeAdapter<A: AstAdapter> {
    outer: A,
    grafts: RefCell<Vec<Graft>>,
    /// Outer leaf → its graft index (`None`: probed, not
    /// parseable).
    probed: RefCell<HashMap<NodeId, Option<usize>>>,
    /// (graft, inner id) → interned composite id, and the reverse.
    interned: RefCell<HashMap<(usize, NodeId), NodeId>>,
    reverse: RefCell<Vec<(usize, NodeId)>>,
    /// Maps an outer leaf to a filesystem path, when the outer
    /// substrate has one. Enables archive grafts, which are
    /// binary and open by path rather than parsing from text.
    source_path: Option<fn(&A, NodeId) -> Option<PathBuf>>,
}

impl<A: AstAdapter> ComposeAdapter<A> {
    pub fn new(outer: A) -> Self {
        ComposeAdapter {
            outer,
            grafts: RefCell::new(Vec::new()),
            probed: RefCell::new(HashMap::new()),
            interned: RefCell::new(HashMap::new()),
            reverse: RefCell::new(Vec::new()),
            source_path: None,
        }
    }

    /// Like [`new`](Self::new), with a hook mapping outer leaves
    /// to filesystem paths, so archive leaves (`.zip`,
    /// `.tar[.gz]`) graft too. The grafted archive composes in
    /// turn: its own parseable entries graft, and one path walks
    /// filesystem → archive → document.
    pub fn with_source_paths(outer: A, source_path: fn(&A, NodeId) -> Option<PathBuf>) -> Self {
        ComposeAdapter {
            source_path: Some(source_path),
            ..Self::new(outer)
        }
    }

    /// The wrapped adapter (for outer-specific calls).
    pub fn outer(&self) -> &A {
        &self.outer
    }

    /// A combined locator: `outer_locator(leaf)!inner-path` for
    /// grafted nodes.
    pub fn locator(&self, node: NodeId, outer_locator: impl Fn(NodeId) -> String) -> String {
        match self.split(node) {
            None => outer_locator(node),
            Some((g, inner)) => {
                let grafts = self.grafts.borrow();
                let graft = &grafts[g];
                format!(
                    "{}!{}",
                    outer_locator(graft.outer),
                    graft.inner.locator(inner)
                )
            }
        }
    }

    /// Decode a composite id.
    fn split(&self, node: NodeId) -> Option<(usize, NodeId)> {
        if node.0 & GRAFT_BIT == 0 {
            return None;
        }
        self.reverse
            .borrow()
            .get((node.0 & !GRAFT_BIT) as usize)
            .copied()
    }

    fn intern(&self, graft: usize, inner: NodeId) -> NodeId {
        if let Some(&id) = self.interned.borrow().get(&(graft, inner)) {
            return id;
        }
        let mut rev = self.reverse.borrow_mut();
        let id = NodeId(GRAFT_BIT | rev.len() as u64);
        rev.push((graft, inner));
        self.interned.borrow_mut().insert((graft, inner), id);
        id
    }

    /// The graft at an outer leaf, probing (read + parse) on first
    /// touch. Only childless outer nodes with text content are
    /// candidates.
    fn graft_at(&self, node: NodeId) -> Option<usize> {
        if let Some(&g) = self.probed.borrow().get(&node) {
            return g;
        }
        let g = (|| {
            if !self.outer.children(node).is_empty() {
                return None;
            }
            let name = self.outer.name(node)?;
            if archive_name(&name)
                && let Some(path_of) = self.source_path
                && let Some(path) = path_of(&self.outer, node)
                && let Ok(a) = quarb_archive::ArchiveAdapter::open(&path)
            {
                let inner = Inner::Archive(Box::new(ComposeAdapter::new(a)));
                let mut grafts = self.grafts.borrow_mut();
                grafts.push(Graft { outer: node, inner });
                return Some(grafts.len() - 1);
            }
            let content = match self.outer.default_value(node)? {
                Value::Str(s) => s,
                _ => return None,
            };
            let inner = parse_inner(&name, &content)?;
            let mut grafts = self.grafts.borrow_mut();
            grafts.push(Graft { outer: node, inner });
            Some(grafts.len() - 1)
        })();
        self.probed.borrow_mut().insert(node, g);
        g
    }

    /// Map an inner node up: the inner root becomes the outer
    /// leaf.
    fn wrap(&self, graft: usize, inner: NodeId) -> NodeId {
        let grafts = self.grafts.borrow();
        if inner == grafts[graft].inner.adapter().root() {
            grafts[graft].outer
        } else {
            drop(grafts);
            self.intern(graft, inner)
        }
    }
}

impl<A: AstAdapter> AstAdapter for ComposeAdapter<A> {
    fn root(&self) -> NodeId {
        self.outer.root()
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        match self.split(node) {
            Some((g, inner)) => {
                let ids: Vec<NodeId> = {
                    let grafts = self.grafts.borrow();
                    grafts[g].inner.adapter().children(inner)
                };
                ids.into_iter().map(|c| self.wrap(g, c)).collect()
            }
            None => {
                let outer = self.outer.children(node);
                if !outer.is_empty() {
                    return outer;
                }
                match self.graft_at(node) {
                    Some(g) => {
                        let ids: Vec<NodeId> = {
                            let grafts = self.grafts.borrow();
                            let a = grafts[g].inner.adapter();
                            a.children(a.root())
                        };
                        ids.into_iter().map(|c| self.wrap(g, c)).collect()
                    }
                    None => Vec::new(),
                }
            }
        }
    }

    fn name(&self, node: NodeId) -> Option<String> {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g].inner.adapter().name(inner),
            None => self.outer.name(node),
        }
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        match self.split(node) {
            Some((g, inner)) => {
                let p = self.grafts.borrow()[g].inner.adapter().parent(inner)?;
                Some(self.wrap(g, p))
            }
            None => self.outer.parent(node),
        }
    }

    fn traits(&self, node: NodeId) -> Vec<String> {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g].inner.adapter().traits(inner),
            None => self.outer.traits(node),
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g]
                .inner
                .adapter()
                .property(inner, name),
            None => self.outer.property(node, name),
        }
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g].inner.adapter().default_value(inner),
            None => self.outer.default_value(node),
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g].inner.adapter().metadata(inner, key),
            None => self.outer.metadata(node, key),
        }
    }

    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        match self.split(node) {
            Some((g, inner)) => {
                let t = self.grafts.borrow()[g]
                    .inner
                    .adapter()
                    .resolve(inner, property, hint)?;
                Some(self.wrap(g, t))
            }
            None => self.outer.resolve(node, property, hint),
        }
    }

    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        match self.split(node) {
            Some((g, inner)) => {
                let ls: Vec<(String, NodeId)> = {
                    let grafts = self.grafts.borrow();
                    grafts[g].inner.adapter().links(inner)
                };
                ls.into_iter().map(|(l, n)| (l, self.wrap(g, n))).collect()
            }
            None => self.outer.links(node),
        }
    }

    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        match self.split(node) {
            Some((g, inner)) => {
                let ls: Vec<(String, NodeId)> = {
                    let grafts = self.grafts.borrow();
                    grafts[g].inner.adapter().backlinks(inner)
                };
                ls.into_iter().map(|(l, n)| (l, self.wrap(g, n))).collect()
            }
            None => self.outer.backlinks(node),
        }
    }

    fn link_property(
        &self,
        source: NodeId,
        label: &str,
        target: NodeId,
        name: &str,
    ) -> Option<Value> {
        // An edge lives on one side of the graft boundary: both
        // endpoints are inner (same graft) or both outer.
        match (self.split(source), self.split(target)) {
            (Some((g, src)), Some((gt, tgt))) if g == gt => self.grafts.borrow()[g]
                .inner
                .adapter()
                .link_property(src, label, tgt, name),
            (None, None) => self.outer.link_property(source, label, target, name),
            _ => None,
        }
    }
}
