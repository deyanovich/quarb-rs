//! The reader's model of a DjVu — `text:scan.djvu` — built from
//! declared structure only, by the print reading's doctrine (the
//! PDF pair, `quarb-text-pdf`): what the file says, never a guess.
//!
//! - **Sections from the outline** (the `NAVM` bookmarks), where
//!   there is one: the author's heading ladder. A bookmark's title
//!   is the section's `::lemma`, its nesting the tree, its extent
//!   the pages from its target to the next entry's. Pages before
//!   the first bookmark stay at the root. Without an outline the
//!   tree is `page/*` and what lies beneath.
//! - **A page's zones are its tree**, named as the text layer
//!   (`TXTa` / `TXTz`) names them: `column`, `region`, `paragraph`,
//!   `line`. Words are a line's text. A declared paragraph is kept
//!   (unlike the PDF reading, the format declares them); none is
//!   minted.
//! - **Geometry** as adapter metadata on every zone: `:::page`,
//!   `:::x`, `:::y`, `:::width`, `:::height`, in the page's
//!   pixel coordinates (top-left origin).
//! - A page with no text layer has no zones; the document still
//!   mounts with its pages, and `:::text-layer` on the document
//!   node says whether any page carries text. No refusal.
//! - A multi-page file is one document, bundled or indirect (the
//!   indirect form resolves its components beside the index). The
//!   `ANT` metadata (title, author, subject, publisher, year,
//!   keywords) is declared identity: properties of the document
//!   node. Images are outside the text level.
//!
//! The text layer is read only; OCR of a scan without one stays
//! outside the engine.

use djvu_rs::text::{TextZone, TextZoneKind};
use djvu_rs::{DjVuBookmark, DjVuDocument};
use quarb::{AstAdapter, NodeId, Value};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum DjvuTextError {
    #[error("not a DjVu document: {0}")]
    Parse(String),
    #[error("reading {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------
// The extracted model
// ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZoneKind {
    Column,
    Region,
    Paragraph,
    Line,
}

impl ZoneKind {
    fn name(self) -> &'static str {
        match self {
            ZoneKind::Column => "column",
            ZoneKind::Region => "region",
            ZoneKind::Paragraph => "paragraph",
            ZoneKind::Line => "line",
        }
    }
}

#[derive(Debug, Clone)]
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

enum Node {
    Root {
        children: Vec<usize>,
    },
    Section {
        lemma: String,
        level: u8,
        parent: usize,
        children: Vec<usize>,
    },
    Page {
        /// 1-based, as a reader counts.
        index: u32,
        width: u32,
        height: u32,
        dpi: u32,
        has_text: bool,
        parent: usize,
        children: Vec<usize>,
    },
    Zone {
        kind: ZoneKind,
        page: u32,
        rect: Rect,
        /// A line's text (its words joined as the layer has them);
        /// empty above the line.
        text: String,
        parent: usize,
        children: Vec<usize>,
    },
}

pub struct DjvuText {
    nodes: Vec<Node>,
    /// The `ANT` metadata, as (key, value): the declared identity.
    identity: Vec<(String, String)>,
    n_pages: usize,
}

/// Read a DjVu document from its bytes: a bundled `DJVM`, or a
/// single-page `DJVU`. An indirect document (an index whose pages
/// are files beside it) goes through [`parse_file`].
pub fn parse(bytes: &[u8]) -> Result<DjvuText, DjvuTextError> {
    let doc = DjVuDocument::parse(bytes).map_err(|e| DjvuTextError::Parse(e.to_string()))?;
    Ok(assemble(&doc))
}

/// Read a DjVu document by path: bundled or single-page as
/// [`parse`], and the indirect form with its components resolved
/// from the index's own directory.
pub fn parse_file(path: &Path) -> Result<DjvuText, DjvuTextError> {
    let bytes = std::fs::read(path)?;
    // Bundled and single-page documents read from their bytes; an
    // indirect index resolves its page files beside itself.
    let base = path.parent().unwrap_or(Path::new("."));
    let doc = DjVuDocument::parse_from_dir(&bytes, base)
        .map_err(|e| DjvuTextError::Parse(e.to_string()))?;
    Ok(assemble(&doc))
}

// ---------------------------------------------------------------
// Assembly: pages under the outline's sections, zones under pages
// ---------------------------------------------------------------

fn assemble(doc: &DjVuDocument) -> DjvuText {
    let n_pages = doc.page_count();
    let mut nodes = vec![Node::Root {
        children: Vec::new(),
    }];
    // Component ids, for bookmarks that name a page by its file.
    let components: Vec<String> = doc
        .component_directory()
        .map(|cs| {
            cs.into_iter()
                .filter(|c| c.kind == 'P')
                .map(|c| c.id)
                .collect()
        })
        .unwrap_or_default();
    let outline = flatten_outline(doc.bookmarks(), n_pages, &components);
    // The pages, in order, each with its zones; a bookmark's target
    // page opens its section, the pages before the first stay at
    // the root.
    let mut page_nodes: Vec<usize> = Vec::with_capacity(n_pages);
    let mut sections: Vec<(u8, usize)> = Vec::new(); // (level, node)
    let mut next_entry = 0;
    for i in 0..n_pages {
        while next_entry < outline.len() && outline[next_entry].page == i {
            let entry = &outline[next_entry];
            while sections.last().is_some_and(|(l, _)| *l >= entry.level) {
                sections.pop();
            }
            let parent = sections.last().map_or(0, |(_, n)| *n);
            let sn = nodes.len();
            nodes.push(Node::Section {
                lemma: entry.title.clone(),
                level: entry.level,
                parent,
                children: Vec::new(),
            });
            push_child(&mut nodes, parent, sn);
            sections.push((entry.level, sn));
            next_entry += 1;
        }
        let parent = sections.last().map_or(0, |(_, n)| *n);
        let pn = nodes.len();
        let (width, height, dpi, layer) = match doc.page(i) {
            Ok(page) => (
                page.width() as u32,
                page.height() as u32,
                page.dpi() as u32,
                page.text_layer().ok().flatten(),
            ),
            Err(_) => (0, 0, 0, None),
        };
        let has_text = layer.as_ref().is_some_and(|l| !l.text.trim().is_empty());
        nodes.push(Node::Page {
            index: i as u32 + 1,
            width,
            height,
            dpi,
            has_text,
            parent,
            children: Vec::new(),
        });
        push_child(&mut nodes, parent, pn);
        page_nodes.push(pn);
        if let Some(layer) = layer {
            for z in &layer.zones {
                add_zone(&mut nodes, z, i as u32 + 1, pn);
            }
        }
    }
    let identity = doc
        .metadata()
        .ok()
        .flatten()
        .map(|m| {
            let mut out = Vec::new();
            for (k, v) in [
                ("title", m.title),
                ("author", m.author),
                ("subject", m.subject),
                ("publisher", m.publisher),
                ("year", m.year),
                ("keywords", m.keywords),
            ] {
                if let Some(v) = v {
                    out.push((k.to_string(), v));
                }
            }
            out.extend(m.extra);
            out
        })
        .unwrap_or_default();
    DjvuText {
        nodes,
        identity,
        n_pages,
    }
}

fn push_child(nodes: &mut [Node], parent: usize, child: usize) {
    match &mut nodes[parent] {
        Node::Root { children }
        | Node::Section { children, .. }
        | Node::Page { children, .. }
        | Node::Zone { children, .. } => children.push(child),
    }
}

/// A zone of the layer into the arena: the page zone itself is the
/// page node (not repeated); columns, regions, paragraphs and lines
/// nest as declared; words and characters are a line's text.
fn add_zone(nodes: &mut Vec<Node>, z: &TextZone, page: u32, parent: usize) {
    let kind = match z.kind {
        TextZoneKind::Page => {
            for c in &z.children {
                add_zone(nodes, c, page, parent);
            }
            return;
        }
        TextZoneKind::Column => ZoneKind::Column,
        TextZoneKind::Region => ZoneKind::Region,
        TextZoneKind::Para => ZoneKind::Paragraph,
        TextZoneKind::Line => ZoneKind::Line,
        TextZoneKind::Word | TextZoneKind::Character => return,
    };
    let text = if kind == ZoneKind::Line {
        line_text(z)
    } else {
        String::new()
    };
    let n = nodes.len();
    nodes.push(Node::Zone {
        kind,
        page,
        rect: Rect {
            x: z.rect.x,
            y: z.rect.y,
            width: z.rect.width,
            height: z.rect.height,
        },
        text,
        parent,
        children: Vec::new(),
    });
    push_child(nodes, parent, n);
    if kind != ZoneKind::Line {
        for c in &z.children {
            add_zone(nodes, c, page, n);
        }
    }
}

/// A line's text: the layer's own text for the line when it has
/// one, else its words joined by a space (a layer may carry the
/// words alone).
fn line_text(z: &TextZone) -> String {
    let own = z.text.trim();
    if !own.is_empty() {
        return own.to_string();
    }
    z.children
        .iter()
        .map(|w| w.text.trim())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

struct OutlineEntry {
    level: u8,
    title: String,
    /// 0-based page index.
    page: usize,
}

/// The bookmarks in document order with their levels, each resolved
/// to a page; one that names no page this document has is left out
/// (a bookmark to another document, a malformed target).
fn flatten_outline(
    bookmarks: &[DjVuBookmark],
    n_pages: usize,
    components: &[String],
) -> Vec<OutlineEntry> {
    fn walk(
        bs: &[DjVuBookmark],
        level: u8,
        n_pages: usize,
        components: &[String],
        out: &mut Vec<OutlineEntry>,
    ) {
        for b in bs {
            if let Some(page) = bookmark_page(&b.url, n_pages, components) {
                out.push(OutlineEntry {
                    level,
                    title: b.title.clone(),
                    page,
                });
            }
            walk(&b.children, level + 1, n_pages, components, out);
        }
    }
    let mut out = Vec::new();
    walk(bookmarks, 1, n_pages, components, &mut out);
    // Sections open in page order; the outline's order is kept for
    // entries on one page.
    out.sort_by_key(|e| e.page);
    out
}

/// A bookmark's target: `#N` (a 1-based page number) or `#NAME`
/// (a page component's id), the DjVu internal URL forms.
fn bookmark_page(url: &str, n_pages: usize, components: &[String]) -> Option<usize> {
    let target = url.strip_prefix('#').unwrap_or(url);
    if let Ok(n) = target.parse::<usize>() {
        return (n >= 1 && n <= n_pages).then_some(n - 1);
    }
    components.iter().position(|c| c == target)
}

// ---------------------------------------------------------------
// The adapter
// ---------------------------------------------------------------

impl DjvuText {
    fn kids(&self, i: usize) -> &[usize] {
        match &self.nodes[i] {
            Node::Root { children }
            | Node::Section { children, .. }
            | Node::Page { children, .. }
            | Node::Zone { children, .. } => children,
        }
    }

    fn flatten(&self, i: usize, out: &mut Vec<String>) {
        if let Node::Zone {
            kind: ZoneKind::Line,
            text,
            ..
        } = &self.nodes[i]
        {
            out.push(text.clone());
            return;
        }
        for &c in self.kids(i) {
            self.flatten(c, out);
        }
    }

    pub fn locator(&self, node: NodeId) -> String {
        match &self.nodes[node.0 as usize] {
            Node::Root { .. } => "/".into(),
            Node::Section { lemma, .. } => format!("section '{lemma}'"),
            Node::Page { index, .. } => format!("page {index}"),
            Node::Zone {
                kind, page, rect, ..
            } => format!("p{page}:{}@{},{}", kind.name(), rect.x, rect.y),
        }
    }
}

impl AstAdapter for DjvuText {
    /// This source defines no references to resolve.
    fn resolves_references(&self) -> bool {
        false
    }

    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.kids(node.0 as usize)
            .iter()
            .map(|&i| NodeId(i as u64))
            .collect()
    }
    fn name(&self, node: NodeId) -> Option<String> {
        Some(match &self.nodes[node.0 as usize] {
            Node::Root { .. } => return None,
            Node::Section { .. } => "section".into(),
            Node::Page { .. } => "page".into(),
            Node::Zone { kind, .. } => kind.name().into(),
        })
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        match &self.nodes[node.0 as usize] {
            Node::Root { .. } => None,
            Node::Section { parent, .. }
            | Node::Page { parent, .. }
            | Node::Zone { parent, .. } => Some(NodeId(*parent as u64)),
        }
    }
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        match (&self.nodes[node.0 as usize], name) {
            (Node::Section { lemma, .. }, "lemma") => Some(Value::Str(lemma.clone())),
            // The declared identity: the ANT metadata on the
            // document node.
            (Node::Root { .. }, key) => self
                .identity
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| Value::Str(v.clone()))
                .or_else(|| (key == "text").then(|| self.default_value(node)).flatten()),
            (_, "text") => self.default_value(node),
            _ => None,
        }
    }
    fn properties(&self, node: NodeId) -> Vec<(String, Value)> {
        match &self.nodes[node.0 as usize] {
            Node::Root { .. } => self
                .identity
                .iter()
                .map(|(k, v)| (k.clone(), Value::Str(v.clone())))
                .collect(),
            Node::Section { lemma, .. } => vec![("lemma".into(), Value::Str(lemma.clone()))],
            _ => Vec::new(),
        }
    }
    fn default_value(&self, node: NodeId) -> Option<Value> {
        match &self.nodes[node.0 as usize] {
            Node::Zone {
                kind: ZoneKind::Line,
                text,
                ..
            } => Some(Value::Str(text.clone())),
            _ => {
                let mut out = Vec::new();
                self.flatten(node.0 as usize, &mut out);
                (!out.is_empty()).then(|| Value::Str(out.join("\n")))
            }
        }
    }
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match (&self.nodes[node.0 as usize], key) {
            (Node::Zone { page, .. }, "page") => Some(Value::Int(*page as i64)),
            (Node::Zone { rect, .. }, "x") => Some(Value::Int(rect.x as i64)),
            (Node::Zone { rect, .. }, "y") => Some(Value::Int(rect.y as i64)),
            (Node::Zone { rect, .. }, "width") => Some(Value::Int(rect.width as i64)),
            (Node::Zone { rect, .. }, "height") => Some(Value::Int(rect.height as i64)),
            (Node::Section { level, .. }, "level") => Some(Value::Int(*level as i64)),
            (Node::Page { index, .. }, "page") => Some(Value::Int(*index as i64)),
            (Node::Page { width, .. }, "width") => Some(Value::Int(*width as i64)),
            (Node::Page { height, .. }, "height") => Some(Value::Int(*height as i64)),
            (Node::Page { dpi, .. }, "dpi") => Some(Value::Int(*dpi as i64)),
            (Node::Page { has_text, .. }, "text-layer") => Some(Value::Bool(*has_text)),
            (Node::Root { .. }, "text-layer") => Some(Value::Bool(
                self.nodes
                    .iter()
                    .any(|n| matches!(n, Node::Page { has_text: true, .. })),
            )),
            (Node::Root { .. }, "n-pages") => Some(Value::Int(self.n_pages as i64)),
            _ => None,
        }
    }
}
