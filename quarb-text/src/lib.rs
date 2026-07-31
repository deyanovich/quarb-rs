//! The text level: a shared, source-independent semantics for
//! written documents — sections, paragraphs, quotes, lists, and
//! verbatim blocks — produced by format crates and served by this
//! crate's single adapter.
//!
//! The block model follows the atrep markup language (litogramma's
//! koine core): every block is `(kind, taxis?, lemma?, body,
//! hypograph?)` — the lemma is the head or title, the hypograph the
//! footer or attribution, and a paragraph is the degenerate
//! lemma-less, hypograph-less block. Producers (`quarb-text-html`,
//! `quarb-text-markdown`, the built-in plain-text reader) lower
//! their format into the [`Block`] event stream; this crate derives
//! the section tree and implements the adapter once, so
//! `//section[::lemma ...]`, `//paragraph`, and `//blockquote` read
//! identically over any text substrate — including an atrep
//! document mounted by `quarb-atrep`.
//!
//! - Node names are the structural kinds: `section`, `paragraph`,
//!   `blockquote`, `unordered-list`, `ordered-list`,
//!   `unordered-item`, `ordered-item`, `verbatim`.
//! - `::lemma`, `::hypograph`, and `::taxis` are properties; bare
//!   `::` (and `::text`) is the flattened prose of the subtree,
//!   lemma first, hypograph last.
//! - `::::level` on a section is the source heading level;
//!   `::::lang` on a verbatim block is its declared language.
//! - Sections are derived from the flat heading stream by the
//!   outline rule: a heading closes every open section at its
//!   level or deeper, then opens a section under the nearest
//!   shallower one. Content before the first heading belongs to
//!   the document root. A heading inside an open container
//!   (blockquote, list) is decorative, not sectioning: it lowers
//!   to a paragraph of its text.
//! - Tables denormalize into nested lists: an `ordered-list`
//!   carrying the `<table>` trait (`::lemma` = the caption), one
//!   `ordered-item` per row (`::taxis` = row number), one
//!   `unordered-item` per cell — `Header: value` where a header
//!   row exists, the bare cell text otherwise. Empty cells are
//!   skipped.

use quarb::{AstAdapter, NodeId, Value};

pub mod render;
pub use render::{Render, render_node, render_nodes};

/// A block-level event in the text-level vocabulary — what a format
/// producer emits. Headings arrive flat; the section tree is
/// derived here, once, for every producer.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// A flat heading: `level` is the source level (`h2` → 2, a
    /// LaTeX `\section` → its depth), `lemma` its text.
    Heading { level: u8, lemma: String },
    /// A plain paragraph — the implicit, lemma-less block.
    Paragraph { text: String },
    /// Inline content belonging directly to the open container (a
    /// list item's own text, a bare-text blockquote). With no open
    /// container it is read as a paragraph.
    Text { text: String },
    /// Open a nesting container. Items take their `unordered-` /
    /// `ordered-` flavor (and taxis) from the enclosing list.
    Open { kind: Container, lemma: Option<String> },
    /// Close the innermost open container, optionally with its
    /// hypograph (footer or attribution).
    Close { hypograph: Option<String> },
    /// A verbatim block — code or other preformatted lines, kept
    /// as authored.
    Verbatim { lang: Option<String>, text: String },
    /// A table, denormalized here into nested lists (rows =
    /// ordered items, cells = unordered items, `Header: value`
    /// when `headers` is present). Header *detection* is the
    /// producer's job; the lowering rule lives here.
    Table {
        lemma: Option<String>,
        headers: Option<Vec<String>>,
        rows: Vec<Vec<String>>,
    },
}

/// The nesting containers a producer opens and closes explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Blockquote,
    UnorderedList,
    /// `start` is the first item's ordinal (Markdown's `3.` lists).
    OrderedList { start: i64 },
    /// A list item; flavor and taxis come from the enclosing list.
    Item,
}

/// The structural kind of a node — also its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Document,
    Section,
    Paragraph,
    Blockquote,
    UnorderedList,
    OrderedList,
    UnorderedItem,
    OrderedItem,
    Verbatim,
}

impl Kind {
    fn name(self) -> Option<&'static str> {
        Some(match self {
            Kind::Document => return None,
            Kind::Section => "section",
            Kind::Paragraph => "paragraph",
            Kind::Blockquote => "blockquote",
            Kind::UnorderedList => "unordered-list",
            Kind::OrderedList => "ordered-list",
            Kind::UnorderedItem => "unordered-item",
            Kind::OrderedItem => "ordered-item",
            Kind::Verbatim => "verbatim",
        })
    }
}

struct Node {
    kind: Kind,
    lemma: Option<String>,
    hypograph: Option<String>,
    taxis: Option<i64>,
    /// Source heading level, on sections.
    level: Option<u8>,
    /// Declared language, on verbatim blocks.
    lang: Option<String>,
    /// First ordinal of an ordered list (not exposed; feeds the
    /// items' taxis).
    start: i64,
    /// The node's own (direct) text, before subtree flattening.
    text: String,
    /// The flattened prose of the subtree — the `::` projection.
    prose: String,
    /// The node heads a denormalized table (`<table>` trait).
    table: bool,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
}

impl Node {
    fn new(kind: Kind, parent: Option<NodeId>) -> Self {
        Node {
            kind,
            lemma: None,
            hypograph: None,
            taxis: None,
            level: None,
            lang: None,
            start: 1,
            text: String::new(),
            prose: String::new(),
            table: false,
            parent,
            children: Vec::new(),
        }
    }
}

/// Collapse whitespace runs to single spaces and trim — the prose
/// normalization producers apply to inline content. Verbatim text
/// is the exception: it is kept as authored.
pub fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A Quarb adapter over a text-level document.
pub struct TextModel {
    nodes: Vec<Node>,
    root: NodeId,
}

impl TextModel {
    /// Assemble the document tree from a producer's event stream.
    ///
    /// Iterative throughout (the stream is flat; prose flattening
    /// runs over indices), so pathological nesting cannot overflow
    /// the call stack. Lenient on malformed streams: a stray
    /// `Close` is ignored, unclosed containers close at the end.
    pub fn build(blocks: Vec<Block>) -> Self {
        let mut nodes = vec![Node::new(Kind::Document, None)];
        let root = NodeId(0);
        // Innermost-last stack of open *sections* (outline-derived).
        let mut sections: Vec<NodeId> = Vec::new();
        // Innermost-last stack of open explicit containers.
        let mut containers: Vec<NodeId> = Vec::new();

        for block in blocks {
            match block {
                Block::Heading { level, lemma } => {
                    let lemma = normalize_ws(&lemma);
                    if !containers.is_empty() {
                        // Decorative heading inside a container:
                        // not sectioning — lower to a paragraph.
                        if !lemma.is_empty() {
                            let parent = *containers.last().unwrap();
                            let id = push(&mut nodes, Kind::Paragraph, parent);
                            nodes[id.0 as usize].text = lemma;
                        }
                        continue;
                    }
                    while let Some(&open) = sections.last() {
                        if nodes[open.0 as usize].level >= Some(level) {
                            sections.pop();
                        } else {
                            break;
                        }
                    }
                    let parent = sections.last().copied().unwrap_or(root);
                    let id = push(&mut nodes, Kind::Section, parent);
                    let n = &mut nodes[id.0 as usize];
                    n.lemma = Some(lemma);
                    n.level = Some(level);
                    sections.push(id);
                }
                Block::Paragraph { text } => {
                    let text = normalize_ws(&text);
                    if text.is_empty() {
                        continue;
                    }
                    let parent = cursor(&sections, &containers, root);
                    let id = push(&mut nodes, Kind::Paragraph, parent);
                    nodes[id.0 as usize].text = text;
                }
                Block::Text { text } => {
                    let text = normalize_ws(&text);
                    if text.is_empty() {
                        continue;
                    }
                    match containers.last() {
                        Some(&open) => {
                            let own = &mut nodes[open.0 as usize].text;
                            if !own.is_empty() {
                                own.push(' ');
                            }
                            own.push_str(&text);
                        }
                        None => {
                            let parent = sections.last().copied().unwrap_or(root);
                            let id = push(&mut nodes, Kind::Paragraph, parent);
                            nodes[id.0 as usize].text = text;
                        }
                    }
                }
                Block::Open { kind, lemma } => {
                    let parent = cursor(&sections, &containers, root);
                    let (nkind, start) = match kind {
                        Container::Blockquote => (Kind::Blockquote, None),
                        Container::UnorderedList => (Kind::UnorderedList, None),
                        Container::OrderedList { start } => (Kind::OrderedList, Some(start)),
                        Container::Item => (
                            match nodes[parent.0 as usize].kind {
                                Kind::OrderedList => Kind::OrderedItem,
                                _ => Kind::UnorderedItem,
                            },
                            None,
                        ),
                    };
                    let id = push(&mut nodes, nkind, parent);
                    nodes[id.0 as usize].lemma =
                        lemma.map(|l| normalize_ws(&l)).filter(|l| !l.is_empty());
                    if let Some(start) = start {
                        nodes[id.0 as usize].start = start;
                    }
                    if nkind == Kind::OrderedItem {
                        // `push` already appended this item, so the
                        // count includes it.
                        let nth = nodes[parent.0 as usize]
                            .children
                            .iter()
                            .filter(|&&c| nodes[c.0 as usize].kind == Kind::OrderedItem)
                            .count() as i64;
                        let start = nodes[parent.0 as usize].start;
                        nodes[id.0 as usize].taxis = Some(start + nth - 1);
                    }
                    containers.push(id);
                }
                Block::Close { hypograph } => {
                    if let Some(open) = containers.pop() {
                        nodes[open.0 as usize].hypograph =
                            hypograph.map(|h| normalize_ws(&h)).filter(|h| !h.is_empty());
                    }
                }
                Block::Verbatim { lang, text } => {
                    let parent = cursor(&sections, &containers, root);
                    let id = push(&mut nodes, Kind::Verbatim, parent);
                    let n = &mut nodes[id.0 as usize];
                    n.lang = lang.filter(|l| !l.is_empty());
                    n.text = text;
                }
                Block::Table {
                    lemma,
                    headers,
                    rows,
                } => {
                    let parent = cursor(&sections, &containers, root);
                    lower_table(&mut nodes, parent, lemma, headers, rows);
                }
            }
        }

        flatten_prose(&mut nodes);
        TextModel { nodes, root }
    }

    /// Read plain text: blank-line-separated paragraphs, each
    /// collapsed to one line — the atramento paragraph rule. No
    /// headings, no markup.
    pub fn parse_plain(text: &str) -> Self {
        let mut blocks = Vec::new();
        let mut para: Vec<&str> = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                if !para.is_empty() {
                    blocks.push(Block::Paragraph {
                        text: para.join(" "),
                    });
                    para.clear();
                }
            } else {
                para.push(line);
            }
        }
        if !para.is_empty() {
            blocks.push(Block::Paragraph {
                text: para.join(" "),
            });
        }
        Self::build(blocks)
    }

    /// A locator path to `node`, like `/section[2]/paragraph[3]`,
    /// for rendering. A `[n]` index is added only to disambiguate
    /// same-name siblings.
    pub fn locator(&self, node: NodeId) -> String {
        let mut segments = Vec::new();
        let mut cur = Some(node);
        while let Some(id) = cur {
            let n = &self.nodes[id.0 as usize];
            if let Some(name) = n.kind.name() {
                segments.push(self.segment(id, name));
            }
            cur = n.parent;
        }
        segments.reverse();
        format!("/{}", segments.join("/"))
    }

    fn segment(&self, node: NodeId, name: &str) -> String {
        let Some(parent) = self.nodes[node.0 as usize].parent else {
            return name.to_string();
        };
        let siblings = &self.nodes[parent.0 as usize].children;
        let same_name: Vec<NodeId> = siblings
            .iter()
            .copied()
            .filter(|&s| self.nodes[s.0 as usize].kind == self.nodes[node.0 as usize].kind)
            .collect();
        if same_name.len() > 1 {
            let n = same_name.iter().position(|&s| s == node).unwrap() + 1;
            format!("{name}[{n}]")
        } else {
            name.to_string()
        }
    }
}

/// Where the next block lands: the innermost open container, else
/// the innermost open section, else the root.
fn cursor(sections: &[NodeId], containers: &[NodeId], root: NodeId) -> NodeId {
    containers
        .last()
        .or(sections.last())
        .copied()
        .unwrap_or(root)
}

fn push(nodes: &mut Vec<Node>, kind: Kind, parent: NodeId) -> NodeId {
    let id = NodeId(nodes.len() as u64);
    nodes.push(Node::new(kind, Some(parent)));
    nodes[parent.0 as usize].children.push(id);
    id
}

/// Denormalize a table into nested lists (see the module doc).
fn lower_table(
    nodes: &mut Vec<Node>,
    parent: NodeId,
    lemma: Option<String>,
    headers: Option<Vec<String>>,
    rows: Vec<Vec<String>>,
) {
    let list = push(nodes, Kind::OrderedList, parent);
    {
        let n = &mut nodes[list.0 as usize];
        n.table = true;
        n.lemma = lemma.map(|l| normalize_ws(&l)).filter(|l| !l.is_empty());
    }
    for (i, row) in rows.into_iter().enumerate() {
        let item = push(nodes, Kind::OrderedItem, list);
        nodes[item.0 as usize].taxis = Some(i as i64 + 1);
        let cells = push(nodes, Kind::UnorderedList, item);
        for (j, cell) in row.into_iter().enumerate() {
            let value = normalize_ws(&cell);
            if value.is_empty() {
                continue;
            }
            let header = headers
                .as_ref()
                .and_then(|h| h.get(j))
                .map(|h| normalize_ws(h))
                .filter(|h| !h.is_empty());
            let text = match header {
                Some(h) => format!("{h}: {value}"),
                None => value,
            };
            let cell_item = push(nodes, Kind::UnorderedItem, cells);
            nodes[cell_item.0 as usize].text = text;
        }
    }
}

/// Compute every node's flattened prose: lemma first, then the
/// node's own text, then its children's prose in order, then the
/// hypograph, block-joined with newlines. Children always carry
/// larger indices than their parents (nodes are interned in
/// document order), so one reverse index scan suffices — no
/// recursion.
fn flatten_prose(nodes: &mut [Node]) {
    for i in (0..nodes.len()).rev() {
        let mut parts: Vec<String> = Vec::new();
        if let Some(lemma) = &nodes[i].lemma
            && !lemma.is_empty()
        {
            parts.push(lemma.clone());
        }
        if !nodes[i].text.is_empty() {
            parts.push(nodes[i].text.clone());
        }
        for &child in nodes[i].children.clone().iter() {
            let prose = &nodes[child.0 as usize].prose;
            if !prose.is_empty() {
                parts.push(prose.clone());
            }
        }
        if let Some(hypograph) = &nodes[i].hypograph
            && !hypograph.is_empty()
        {
            parts.push(hypograph.clone());
        }
        nodes[i].prose = parts.join("\n");
    }
}

impl AstAdapter for TextModel {
    fn root(&self) -> NodeId {
        self.root
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.nodes[node.0 as usize].children.clone()
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.nodes[node.0 as usize].kind.name().map(str::to_string)
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes[node.0 as usize].parent
    }

    /// The `<block>` family on every block node, plus `<table>` on
    /// a list that denormalizes a table. Kinds are node names, not
    /// traits.
    fn traits(&self, node: NodeId) -> Vec<String> {
        let n = &self.nodes[node.0 as usize];
        let mut out = Vec::new();
        if n.kind != Kind::Document {
            out.push("block".to_string());
        }
        if n.table {
            out.push("table".to_string());
        }
        out
    }

    /// `::lemma` (title), `::hypograph` (footer or attribution),
    /// `::taxis` (ordinal), `::text` (the flattened prose, same as
    /// the bare projection).
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        match name {
            "lemma" => n.lemma.clone().map(Value::Str),
            "hypograph" => n.hypograph.clone().map(Value::Str),
            "taxis" => n.taxis.map(Value::Int),
            "text" => Some(Value::Str(n.prose.clone())),
            _ => None,
        }
    }

    /// The default projection is the flattened prose of the
    /// subtree — lemma first, hypograph last.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        Some(Value::Str(self.nodes[node.0 as usize].prose.clone()))
    }

    /// `::::level` on sections (the source heading level) and
    /// `::::lang` on verbatim blocks (the declared language).
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        match key {
            "level" => n.level.map(|l| Value::Int(l as i64)),
            "lang" => n.lang.clone().map(Value::Str),
            _ => None,
        }
    }
}
