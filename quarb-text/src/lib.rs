//! The text level: a shared, source-independent semantics for
//! written documents — sections, paragraphs, quotes, lists, and
//! verbatim blocks — produced by format crates and served by this
//! crate's single adapter.
//!
//! The block model follows the atrep markup language (litogramma's
//! koine core): every block is `(kind, taxis?, lemma?, body,
//! hypograph?)` — the lemma is the head or title, the hypograph the
//! footer or attribution, and a paragraph is the degenerate
//! lemma-less, hypograph-less block. The format adapters (`quarb-text-html`,
//! `quarb-text-markdown`, the built-in plain-text reader) lower
//! their format into the [`Block`] event stream; this crate derives
//! the section tree and implements the adapter once, so
//! `//section[::lemma ...]`, `//paragraph`, and `//blockquote` read
//! identically over any text substrate — including an atrep
//! document mounted by `quarb-atrep`.
//!
//! - Node names are the structural kinds: `section`, `paragraph`,
//!   `blockquote`, `unordered-list`, `ordered-list`,
//!   `unordered-item`, `ordered-item`, `verbatim` — plus the
//!   apparatus: `footnote` and `endnote` (callout and body share
//!   the family name, ruling #35), `aside` (litogramma's fourth
//!   deixis-attached family — anchored content, not apparatus),
//!   `index-mark` (ruling #36), and the verse vocabulary
//!   (ruling #37): `verse` holding `strophe`s holding `stichos`
//!   lines, the stichos `::taxis` the citation coordinate.
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
//! - Every kind admits `::lemma`, `::taxis`, and `::hypograph` —
//!   the atrep model, where these are universal affordances of a
//!   block rather than privileges of particular kinds.
//! - Tables denormalize into nested lists: an `ordered-list`
//!   carrying the `<table>` trait (`::lemma` = the caption), one
//!   `ordered-item` per row (`::taxis` = row number, `<row>`
//!   trait), one `unordered-item` per cell (`<cell>` trait) whose
//!   `::lemma` is the column name — from the header row in grids,
//!   from the row's `th` label otherwise; headerless cells carry
//!   no lemma. A lemma'd item flattens as `lemma: prose`, so a
//!   row exists, the bare cell text otherwise. Empty cells are
//!   skipped.

use quarb::{AstAdapter, NodeId, Value};

pub mod render;
pub use render::{Render, render_node, render_nodes};

/// A block-level event in the text-level vocabulary — what a format
/// adapter emits. Headings arrive flat; the section tree is
/// derived here, once, for every adapter.
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
    /// An in-text note callout (the deixis, ruling #35): `onym`
    /// names the body it cites within its family. Emitted after
    /// the flow block it sits in; it becomes that block's child,
    /// in order, and the block's own text stays clean of markers.
    /// `family: None` means the source declares none (an HTML
    /// noteref): the callout takes its resolved body's family,
    /// and a dangling one defaults to footnote.
    NoteRef {
        onym: String,
        family: Option<NoteFamily>,
        /// The declared spelling placed this pair in the margin
        /// (a Tufte-style sidenote): surfaced as `::::form =
        /// "margin"` on both ends — the family stays footnote,
        /// placement is presentation.
        margin: bool,
    },
    /// An index mark (ruling #36): an invisible anchor declaring
    /// this place concerns `term`. The back-of-book index is a
    /// query over these, never a stored structure.
    IndexMark { term: String },
    /// A reference mark — the text-level mention (html's in-prose
    /// `<a href>`, markdown's `[text](url)`, LaTeX's `\ref`):
    /// `target` is the identifier as written, `text` the authored
    /// link text (LaTeX `\ref` has none), and `internal` is the
    /// producer's declaration that the target names a label in
    /// *this* document (`#fragment`, a `\ref` key) rather than
    /// another document. A mention, not an attachment: refs do
    /// not carry `<deixis>`.
    Ref {
        target: String,
        text: Option<String>,
        internal: bool,
    },
    /// A point anchor — an invisible in-flow node bearing an
    /// `onym` at this position (LaTeX's mid-flow `\label`, an
    /// inline html `id=`). The IndexMark construction: child of
    /// its flow block, document order.
    Anchor { onym: String },
    /// A citation mark — the bibliography's mention (LaTeX's
    /// `\cite`, koine's cite monosim): `target` is the authored
    /// key, resolved against the `bib` bearers — a namespace of
    /// its own, separate from labels (LaTeX precedent).
    Cite { target: String },
    /// One bibliographic entry: a block bearing its authored
    /// `key`. Unstructured sources (\bibitem) carry the full
    /// reference as `text`; structured ones (bibliogramma,
    /// BibTeX/BibLaTeX through it) carry `fields` — canonical
    /// campus name → value, Latin per the bibliogramma
    /// vocabulary — and the entry's `genus` (liber,
    /// commentarius, …).
    Bib {
        key: String,
        text: String,
        fields: Vec<(String, String)>,
        genus: Option<String>,
    },
    /// A block label: the innermost open section takes `onym` as
    /// the name it bears (a heading's `id`; a `\label` attached
    /// to a sectioning command — the promotion rule). With no
    /// open section it degrades to a point [`Block::Anchor`].
    Label { onym: String },
    /// A verse block (ruling #37 — litogramma's stichoi model):
    /// strophes of lines, denormalized like [`Block::Table`].
    /// Lines arrive flattened by the adapter and are kept as
    /// authored; the stichos `::taxis` numbers lines 1-based,
    /// CONTINUOUSLY across strophes — the citation coordinate.
    Verse {
        lemma: Option<String>,
        strophes: Vec<Vec<String>>,
        hypograph: Option<String>,
    },
    /// A table, denormalized here into nested lists (rows =
    /// ordered items with the `<row>` trait, cells = unordered
    /// items with the `<cell>` trait and the column name as
    /// `::lemma`). Header *detection* is the adapter's job; the
    /// lowering rule lives here. A cell's own `label` (a row's
    /// `th`) wins over the positional `headers` entry.
    Table {
        lemma: Option<String>,
        headers: Option<Vec<String>>,
        rows: Vec<Vec<Cell>>,
    },
}

/// One table cell as an adapter hands it over: the text, plus the
/// label a row-shaped dialect attaches directly (an infobox row's
/// `th`). Grid dialects leave `label` empty and let the lowering
/// zip the header row on by position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub label: Option<String>,
    pub text: String,
}

impl From<&str> for Cell {
    fn from(text: &str) -> Self {
        Cell {
            label: None,
            text: text.to_string(),
        }
    }
}

impl From<String> for Cell {
    fn from(text: String) -> Self {
        Cell { label: None, text }
    }
}

/// The nesting containers an adapter opens and closes explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Container {
    Blockquote,
    UnorderedList,
    /// `start` is the first item's ordinal (Markdown's `3.` lists).
    OrderedList { start: i64 },
    /// A list item; flavor and taxis come from the enclosing list.
    Item,
    /// A note body (the noted, ruling #35): opens a note node of
    /// its family at the document root — litogramma's canonical
    /// document-end placement — holding its own blocks. `margin`
    /// as on [`Block::NoteRef`].
    Note {
        onym: String,
        family: NoteFamily,
        margin: bool,
    },
}

/// The deixis-attached families litogramma's canon names
/// (ruling #35, as amended): each family's callout and body
/// share its name. `Aside` rides the same construction —
/// koine's `aside` sim, an anchored body whose insertion point
/// is a deixis — but is content, not apparatus: its bodies do
/// not carry `<note>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoteFamily {
    Footnote,
    Endnote,
    Aside,
}

impl NoteFamily {
    fn kind(self) -> Kind {
        match self {
            NoteFamily::Footnote => Kind::Footnote,
            NoteFamily::Endnote => Kind::Endnote,
            NoteFamily::Aside => Kind::Aside,
        }
    }
}

/// The structural kind of a node — also its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    /// Callout and body both — they share the name by
    /// construction (litogramma F5): `//footnote` gathers the
    /// whole apparatus, `<deixis>`/`<note>` tell them apart.
    Footnote,
    /// The second note family, same construction.
    Endnote,
    /// The anchored-content family, same construction
    /// (litogramma's aside): body + insertion-point deixis. Content, not
    /// apparatus — no `<note>` on its bodies.
    Aside,
    /// An index mark (ruling #36): `::term`, in flow position.
    IndexMark,
    /// A reference mark: `::target`, `-->` resolves it.
    Ref,
    /// A citation mark: `::target` the authored key, resolved
    /// against the `bib` namespace.
    Cit,
    /// One bibliographic entry: `::onym` the key it bears, its
    /// projection the full reference text.
    Bib,
    /// A point anchor: `::onym`, an invisible in-flow bearer.
    Anchor,
    /// The verse vocabulary (ruling #37): the stichoi container…
    Verse,
    /// …its strophes…
    Strophe,
    /// …and its lines, `::taxis` the citation coordinate.
    Stichos,
}

impl Kind {
    fn name(self) -> Option<&'static str> {
        Some(match self {
            Kind::Document => return None,
            Kind::Section => "section",
            Kind::Paragraph => "paragraph",
            Kind::Blockquote => "blockquote",
            Kind::Footnote => "footnote",
            Kind::Endnote => "endnote",
            Kind::Aside => "aside",
            Kind::IndexMark => "index-mark",
            Kind::Ref => "ref",
            Kind::Cit => "cit",
            Kind::Bib => "bib",
            Kind::Anchor => "anchor",
            Kind::Verse => "verse",
            Kind::Strophe => "strophe",
            Kind::Stichos => "stichos",
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
    /// The node is a denormalized table row (`<row>` trait).
    row: bool,
    /// The node is a denormalized table cell (`<cell>` trait).
    cell: bool,
    /// The apparatus (ruling #35): the note name as written —
    /// and, on an index mark (ruling #36), the term.
    onym: Option<String>,
    /// A footnote callout (`<deixis>`) rather than a body.
    deixis: bool,
    /// A callout whose body is missing (`<dangling>`).
    dangling: bool,
    /// A callout whose source declared no family: resolution may
    /// re-kind it from the body it reaches.
    family_open: bool,
    /// The declared spelling was a margin form (`::::form`).
    margin: bool,
    /// Callout -> body edge, once resolved.
    note_edge: Option<NodeId>,
    /// Body <- callouts (the reverse index).
    cites: Vec<NodeId>,
    /// A ref's target identifier, as written.
    target: Option<String>,
    /// The producer declared the target in-document.
    internal: bool,
    /// A structured bib entry's fields (campus → value) and its
    /// genus, per the bibliogramma vocabulary.
    fields: Vec<(String, String)>,
    genus: Option<String>,
    /// Ref -> bearer edge, once resolved (in-document).
    ref_edge: Option<NodeId>,
    /// Bearer <- refs (the reverse index).
    ref_cites: Vec<NodeId>,
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
            row: false,
            cell: false,
            onym: None,
            deixis: false,
            dangling: false,
            family_open: false,
            margin: false,
            note_edge: None,
            cites: Vec::new(),
            target: None,
            internal: false,
            fields: Vec::new(),
            genus: None,
            ref_edge: None,
            ref_cites: Vec::new(),
            parent,
            children: Vec::new(),
        }
    }
}

/// The reference pass: collect every borne name (block labels
/// and point anchors — deixis callouts pair within their own
/// families and stay out of this namespace), then land each
/// internal ref on its bearer, `<dangling>` when nothing bears
/// the name. External refs resolve at query time, through the
/// engine's reference machinery.
fn resolve_refs(nodes: &mut [Node]) -> std::collections::HashMap<String, NodeId> {
    let mut onyms: std::collections::HashMap<String, NodeId> =
        std::collections::HashMap::new();
    // Citation keys are a namespace of their own, separate from
    // labels (LaTeX's \bibcite vs \newlabel precedent): `cit`
    // resolves against `bib` bearers only.
    let mut bibs: std::collections::HashMap<String, NodeId> =
        std::collections::HashMap::new();
    for (i, n) in nodes.iter().enumerate() {
        if n.deixis || matches!(n.kind, Kind::IndexMark | Kind::Ref | Kind::Cit) {
            continue;
        }
        if n.kind == Kind::Bib {
            if let Some(k) = &n.onym {
                bibs.entry(k.clone()).or_insert(NodeId(i as u64));
            }
            continue;
        }
        let bearer = n.kind == Kind::Anchor
            || (!matches!(n.kind, Kind::Footnote | Kind::Endnote | Kind::Aside)
                && n.onym.is_some());
        if bearer && let Some(o) = &n.onym {
            onyms.entry(o.clone()).or_insert(NodeId(i as u64));
        }
    }
    for i in 0..nodes.len() {
        let (map, key) = match nodes[i].kind {
            Kind::Ref if nodes[i].internal => (
                &onyms,
                nodes[i]
                    .target
                    .as_deref()
                    .map(|t| t.trim_start_matches('#').to_string())
                    .unwrap_or_default(),
            ),
            Kind::Cit => (
                &bibs,
                nodes[i].target.clone().unwrap_or_default(),
            ),
            _ => continue,
        };
        match map.get(&key) {
            Some(&bearer) => {
                nodes[i].ref_edge = Some(bearer);
                let me = NodeId(i as u64);
                nodes[bearer.0 as usize].ref_cites.push(me);
            }
            None => nodes[i].dangling = true,
        }
    }
    onyms
}

/// Collapse whitespace runs to single spaces and trim — the prose
/// normalization adapters apply to inline content. Verbatim text
/// is the exception: it is kept as authored.
pub fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A Quarb adapter over a text-level document.
pub struct TextModel {
    nodes: Vec<Node>,
    root: NodeId,
    /// Every borne name → its bearer (block labels and point
    /// anchors, one namespace, first bearer in document order
    /// wins) — the landing map for `-->` and for a sibling
    /// document's `#fragment`.
    onyms: std::collections::HashMap<String, NodeId>,
    /// The document's own URL, when the mount knows it.
    document_url: Option<url::Url>,
    /// Alias → canonical campus, for bib field lookup: the
    /// bibliogramma vocabulary's census (BibLaTeX's field names
    /// are its English rows), so `::author` answers as
    /// `::auctor` — in any covered language. Set by the koine
    /// route, which holds the dialektos.
    bib_aliases: std::collections::HashMap<String, String>,
}

impl TextModel {
    /// Assemble the document tree from a adapter's event stream.
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
        // The block a NoteRef callout attaches to: the last flow
        // node created or appended to (ruling #35).
        let mut last_flow: Option<NodeId> = None;
        // The block a Label names: the most recently opened block
        // of any kind — a heading's section, a paragraph, a
        // container, a verbatim — the html-id / attached-\label
        // parity rule.
        let mut last_block: Option<NodeId> = None;

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
                    last_flow = Some(id);
                    last_block = Some(id);
                }
                Block::Paragraph { text } => {
                    let text = normalize_ws(&text);
                    if text.is_empty() {
                        continue;
                    }
                    let parent = cursor(&sections, &containers, root);
                    let id = push(&mut nodes, Kind::Paragraph, parent);
                    nodes[id.0 as usize].text = text;
                    last_flow = Some(id);
                    last_block = Some(id);
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
                            last_flow = Some(open);
                        }
                        None => {
                            let parent = sections.last().copied().unwrap_or(root);
                            let id = push(&mut nodes, Kind::Paragraph, parent);
                            nodes[id.0 as usize].text = text;
                            last_flow = Some(id);
                        }
                    }
                }
                Block::Open { kind, lemma } => {
                    // A note body opens at the document root —
                    // litogramma's canonical document-end
                    // placement — whatever else is open.
                    let kind = match kind {
                        Container::Note { onym, family, margin } => {
                            let id = push(&mut nodes, family.kind(), root);
                            let n = &mut nodes[id.0 as usize];
                            n.onym = Some(onym.trim().to_string()).filter(|o| !o.is_empty());
                            n.margin = margin;
                            containers.push(id);
                            last_flow = Some(id);
                            continue;
                        }
                        other => other,
                    };
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
                        Container::Note { .. } => unreachable!("handled above"),
                    };
                    let id = push(&mut nodes, nkind, parent);
                    last_block = Some(id);
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
                // The callout: a `footnote` child of the flow
                // block it sits in, `<deixis>`-traited; the edge
                // to its body resolves after the stream.
                Block::NoteRef { onym, family, margin } => {
                    let parent = last_flow.unwrap_or(root);
                    // A declared family names the callout now; an
                    // undeclared one is settled at resolution from
                    // the body it reaches (footnote when dangling).
                    let kind = family.map(NoteFamily::kind).unwrap_or(Kind::Footnote);
                    let id = push(&mut nodes, kind, parent);
                    let n = &mut nodes[id.0 as usize];
                    n.deixis = true;
                    n.family_open = family.is_none();
                    n.margin = margin;
                    n.onym = Some(onym.trim().to_string()).filter(|o| !o.is_empty());
                }
                Block::Ref { target, text, internal } => {
                    let parent = last_flow.unwrap_or(root);
                    let id = push(&mut nodes, Kind::Ref, parent);
                    let n = &mut nodes[id.0 as usize];
                    n.internal = internal;
                    n.target = Some(target.trim().to_string()).filter(|t| !t.is_empty());
                    n.text = text.map(|t| normalize_ws(&t)).unwrap_or_default();
                }
                Block::Cite { target } => {
                    let parent = last_flow.unwrap_or(root);
                    let id = push(&mut nodes, Kind::Cit, parent);
                    let n = &mut nodes[id.0 as usize];
                    n.internal = true;
                    n.target = Some(target.trim().to_string()).filter(|t| !t.is_empty());
                }
                Block::Bib {
                    key,
                    text,
                    fields,
                    genus,
                } => {
                    let parent = cursor(&sections, &containers, root);
                    let id = push(&mut nodes, Kind::Bib, parent);
                    last_block = Some(id);
                    let n = &mut nodes[id.0 as usize];
                    n.onym = Some(key.trim().to_string()).filter(|k| !k.is_empty());
                    n.text = normalize_ws(&text);
                    n.fields = fields
                        .into_iter()
                        .map(|(k, v)| (k.trim().to_string(), normalize_ws(&v)))
                        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
                        .collect();
                    n.genus = genus.map(|g| g.trim().to_string()).filter(|g| !g.is_empty());
                }
                Block::Anchor { onym } => {
                    let parent = last_flow.unwrap_or(root);
                    let id = push(&mut nodes, Kind::Anchor, parent);
                    nodes[id.0 as usize].onym =
                        Some(onym.trim().to_string()).filter(|o| !o.is_empty());
                }
                Block::Label { onym } => {
                    let onym = onym.trim().to_string();
                    if onym.is_empty() {
                        continue;
                    }
                    // The most recently opened block bears the
                    // name — a heading's section, a paragraph, a
                    // blockquote: the html-id / attached-\label
                    // parity rule. A label before any block, or on
                    // a block already named, degrades to a point.
                    let free = last_block
                        .map(|b| nodes[b.0 as usize].onym.is_none())
                        .unwrap_or(false);
                    match (last_block, free) {
                        (Some(b), true) => {
                            nodes[b.0 as usize].onym = Some(onym);
                        }
                        _ => {
                            let parent = last_flow.unwrap_or(root);
                            let id = push(&mut nodes, Kind::Anchor, parent);
                            nodes[id.0 as usize].onym = Some(onym);
                        }
                    }
                }
                Block::IndexMark { term } => {
                    let parent = last_flow.unwrap_or(root);
                    let id = push(&mut nodes, Kind::IndexMark, parent);
                    nodes[id.0 as usize].onym =
                        Some(quarb_term(&term)).filter(|t| !t.is_empty());
                }
                Block::Verbatim { lang, text } => {
                    let parent = cursor(&sections, &containers, root);
                    let id = push(&mut nodes, Kind::Verbatim, parent);
                    last_block = Some(id);
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
                Block::Verse {
                    lemma,
                    strophes,
                    hypograph,
                } => {
                    let parent = cursor(&sections, &containers, root);
                    let id = lower_verse(&mut nodes, parent, lemma, strophes, hypograph);
                    last_flow = Some(id);
                    last_block = Some(id);
                }
            }
        }

        resolve_notes(&mut nodes);
        let onyms = resolve_refs(&mut nodes);
        flatten_prose(&mut nodes);
        TextModel {
            nodes,
            root,
            onyms,
            document_url: None,
            bib_aliases: Default::default(),
        }
    }

    /// Register the bib-field alias census (alias → canonical
    /// campus), lowercased keys — the friction remover: a field
    /// authored or queried under any covered name answers.
    pub fn set_bib_aliases(&mut self, map: std::collections::HashMap<String, String>) {
        self.bib_aliases = map;
    }

    /// Declare the document's own URL — the base a relative ref
    /// target joins against when `-->` reaches across documents.
    pub fn set_document_url(&mut self, url: &str) {
        self.document_url = url::Url::parse(url).ok();
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
/// Ruling #35: wire each callout to its body by (family, onym) —
/// first body wins on a duplicate — build the reverse index, and
/// mark the dangling: a callout with no body keeps its node,
/// carries `<dangling>` and `::::resolved = false`, and emits no
/// edge.
fn resolve_notes(nodes: &mut [Node]) {
    let mut bodies: std::collections::HashMap<(Kind, String), NodeId> = Default::default();
    for (i, n) in nodes.iter().enumerate() {
        if matches!(n.kind, Kind::Footnote | Kind::Endnote | Kind::Aside)
            && !n.deixis
            && let Some(o) = &n.onym
        {
            bodies.entry((n.kind, o.clone())).or_insert(NodeId(i as u64));
        }
    }
    for i in 0..nodes.len() {
        if !matches!(nodes[i].kind, Kind::Footnote | Kind::Endnote | Kind::Aside)
            || !nodes[i].deixis
        {
            continue;
        }
        let Some(onym) = nodes[i].onym.clone() else {
            nodes[i].dangling = true;
            continue;
        };
        let hit = if nodes[i].family_open {
            // No declared family (an HTML noteref): the body's
            // family is the callout's; footnote when dangling.
            [Kind::Footnote, Kind::Endnote, Kind::Aside]
                .into_iter()
                .find_map(|k| bodies.get(&(k, onym.clone())).map(|b| (k, *b)))
        } else {
            bodies.get(&(nodes[i].kind, onym)).map(|b| (nodes[i].kind, *b))
        };
        match hit {
            Some((family, body)) => {
                nodes[i].kind = family;
                nodes[i].note_edge = Some(body);
                let callout = NodeId(i as u64);
                nodes[body.0 as usize].cites.push(callout);
            }
            None => nodes[i].dangling = true,
        }
    }
}

/// An index term as written, with the `|...` formatting and range
/// directives stripped (v1 of ruling #36; ranges and
/// see-references are recorded follow-ups).
fn quarb_term(term: &str) -> String {
    normalize_ws(term.split('|').next().unwrap_or(term))
}

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
/// The column name lands as the cell's `::lemma` — a cell's own
/// label (a row's `th`) wins over the positional header entry —
/// and never as folded text: addressing is property projection,
/// the flattening rule alone spells `lemma: value`.
fn lower_table(
    nodes: &mut Vec<Node>,
    parent: NodeId,
    lemma: Option<String>,
    headers: Option<Vec<String>>,
    rows: Vec<Vec<Cell>>,
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
        nodes[item.0 as usize].row = true;
        let cells = push(nodes, Kind::UnorderedList, item);
        for (j, cell) in row.into_iter().enumerate() {
            let value = normalize_ws(&cell.text);
            if value.is_empty() {
                continue;
            }
            let label = cell
                .label
                .as_deref()
                .or_else(|| headers.as_ref().and_then(|h| h.get(j)).map(|h| h.as_str()))
                .map(normalize_ws)
                .filter(|h| !h.is_empty());
            let cell_item = push(nodes, Kind::UnorderedItem, cells);
            let n = &mut nodes[cell_item.0 as usize];
            n.cell = true;
            n.lemma = label;
            n.text = value;
        }
    }
}

/// Lower a verse block (ruling #37): `verse` → `strophe`s →
/// `stichos` lines. The stichos taxis numbers lines 1-based,
/// continuously across strophes — Iliad 1.34 is a taxis; the
/// strophe taxis is its ordinal. Empty lines are dropped (the
/// strophe boundary carries the separation).
fn lower_verse(
    nodes: &mut Vec<Node>,
    parent: NodeId,
    lemma: Option<String>,
    strophes: Vec<Vec<String>>,
    hypograph: Option<String>,
) -> NodeId {
    let verse = push(nodes, Kind::Verse, parent);
    {
        let n = &mut nodes[verse.0 as usize];
        n.lemma = lemma.map(|l| normalize_ws(&l)).filter(|l| !l.is_empty());
        n.hypograph = hypograph.map(|h| normalize_ws(&h)).filter(|h| !h.is_empty());
    }
    let mut line_no = 0i64;
    for (i, strophe) in strophes.into_iter().enumerate() {
        let sid = push(nodes, Kind::Strophe, verse);
        nodes[sid.0 as usize].taxis = Some(i as i64 + 1);
        for line in strophe {
            let line = line.trim_end().to_string();
            if line.trim().is_empty() {
                continue;
            }
            line_no += 1;
            let lid = push(nodes, Kind::Stichos, sid);
            let n = &mut nodes[lid.0 as usize];
            n.taxis = Some(line_no);
            n.text = line;
        }
    }
    verse
}

/// Compute every node's flattened prose: lemma first, then the
/// node's own text, then its children's prose in order, then the
/// hypograph, block-joined with newlines. On a list *item*, the
/// lemma joins the rest with `: ` instead — an item's lemma names
/// its content inline (a table cell reads `Outcome: Emus won`, a
/// definition reads `term: description`), where a section's lemma
/// opens its block. Children always carry larger indices than
/// their parents (nodes are interned in document order), so one
/// reverse index scan suffices — no recursion.
fn flatten_prose(nodes: &mut [Node]) {
    for i in (0..nodes.len()).rev() {
        let inline_lemma = matches!(
            nodes[i].kind,
            Kind::UnorderedItem | Kind::OrderedItem
        );
        let mut lemma_part: Option<String> = None;
        let mut parts: Vec<String> = Vec::new();
        if let Some(lemma) = &nodes[i].lemma
            && !lemma.is_empty()
        {
            if inline_lemma {
                lemma_part = Some(lemma.clone());
            } else {
                parts.push(lemma.clone());
            }
        }
        if !nodes[i].text.is_empty() {
            parts.push(nodes[i].text.clone());
        }
        let mut child_parts: Vec<String> = Vec::new();
        for &child in nodes[i].children.clone().iter() {
            // A ref's recorded link text is already part of the
            // flow it was met in; an anchor is invisible. Neither
            // adds prose (the deixis/index-mark rule).
            if matches!(
                nodes[child.0 as usize].kind,
                Kind::Ref | Kind::Cit | Kind::Anchor
            ) {
                continue;
            }
            let prose = &nodes[child.0 as usize].prose;
            if !prose.is_empty() {
                child_parts.push(prose.clone());
            }
        }
        // Ruling #37: a verse block separates its strophes by a
        // blank line (lemma and hypograph still join normally) —
        // byte-compatible with the old verbatim lowering.
        if nodes[i].kind == Kind::Verse {
            let joined = child_parts.join("\n\n");
            if !joined.is_empty() {
                parts.push(joined);
            }
        } else {
            parts.extend(child_parts);
        }
        if let Some(hypograph) = &nodes[i].hypograph
            && !hypograph.is_empty()
        {
            parts.push(hypograph.clone());
        }
        let mut prose = parts.join("\n");
        if let Some(lemma) = lemma_part {
            prose = if prose.is_empty() {
                lemma
            } else {
                format!("{lemma}: {prose}")
            };
        }
        nodes[i].prose = prose;
    }
}

impl TextModel {
    /// The body prose: everything between the lemma and the
    /// hypograph — the simmere anatomy's third member, derived
    /// from the flattened prose by construction (the lemma joins
    /// a block on its own line, an item with `: `; the hypograph
    /// closes on its own line).
    fn grammata(&self, node: NodeId) -> String {
        let n = &self.nodes[node.0 as usize];
        let mut s = n.prose.as_str();
        if let Some(lemma) = &n.lemma
            && !lemma.is_empty()
            && let Some(rest) = s.strip_prefix(lemma.as_str())
        {
            s = rest
                .strip_prefix(": ")
                .or_else(|| rest.strip_prefix('\n'))
                .unwrap_or(rest);
        }
        if let Some(h) = &n.hypograph
            && !h.is_empty()
            && let Some(rest) = s.strip_suffix(h.as_str())
        {
            s = rest.strip_suffix('\n').unwrap_or(rest);
        }
        s.trim_end().to_string()
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
        // A callout is inline apparatus, not a block (ruling #35);
        // so is an index mark (ruling #36). Strophes and stichos
        // lines are sub-block structure (ruling #37, litogramma's
        // own family rule) — the verse block carries the trait.
        if n.kind != Kind::Document
            && !n.deixis
            && !matches!(
                n.kind,
                Kind::IndexMark
                    | Kind::Ref
                    | Kind::Cit
                    | Kind::Anchor
                    | Kind::Strophe
                    | Kind::Stichos
            )
        {
            out.push("block".to_string());
        }
        // The reference vocabulary (atrep's semantic traits): a
        // dangling mention keeps its node and the linter finds it;
        // `<target>` marks what bears a referable name — anchors
        // and labeled blocks, never the apparatus families (their
        // onyms pair within the family, not in this namespace).
        if matches!(n.kind, Kind::Ref | Kind::Cit) && n.dangling {
            out.push("dangling".to_string());
        }
        // A structured entry's genus (liber, commentarius, …)
        // surfaces as its trait, the genos rule.
        if n.kind == Kind::Bib && let Some(g) = &n.genus {
            out.push(g.clone());
        }
        if n.kind == Kind::Anchor
            || (n.onym.is_some()
                && !n.deixis
                && !matches!(
                    n.kind,
                    Kind::Footnote
                        | Kind::Endnote
                        | Kind::Aside
                        | Kind::IndexMark
                        | Kind::Ref
                        | Kind::Cit
                        | Kind::Bib
                ))
        {
            out.push("target".to_string());
        }
        if matches!(n.kind, Kind::Footnote | Kind::Endnote | Kind::Aside) {
            // The body IS the note — `<note>` marks it, whichever
            // note family; the callout carries `<deixis>` alone.
            // An aside body is content, not apparatus: no
            // `<note>`. No both-ends trait: `//footnote` already
            // gathers a family whole, `//*<deixis>` the callouts
            // across families, `//*<note>` the note bodies.
            if n.deixis {
                out.push("deixis".to_string());
            } else if n.kind != Kind::Aside {
                out.push("note".to_string());
            }
            if n.dangling {
                out.push("dangling".to_string());
            }
        }
        if n.table {
            out.push("table".to_string());
        }
        if n.row {
            out.push("row".to_string());
        }
        if n.cell {
            out.push("cell".to_string());
        }
        out
    }

    /// `::lemma` (title), `::hypograph` (footer or attribution),
    /// `::taxis` (ordinal), `::text` (the flattened prose, same as
    /// the bare projection).
    /// The Greek anatomy — `::lemma`, `::grammata`, `::hypograph`,
    /// `::taxis` — plus the friendly aliases (`::title`, `::body`,
    /// `::attribution`, `::ord`), answered here because this
    /// adapter's property surface IS the vocabulary; on data
    /// adapters those spellings stay ordinary field names. The
    /// Greek is canon in docs and reflection preserves whichever
    /// spelling was written.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        // A structured entry answers its campi first — under the
        // canonical Latin name or any census alias (BibLaTeX's
        // field names included), case-insensitively — ahead of
        // the general vocabulary, so `::title` on a bib means
        // titulus, not the lemma alias.
        if n.kind == Kind::Bib && !n.fields.is_empty() {
            let canon = self
                .bib_aliases
                .get(&name.to_lowercase())
                .map(String::as_str)
                .unwrap_or(name);
            if let Some((_, v)) = n.fields.iter().find(|(k, _)| k == canon) {
                return Some(Value::Str(v.clone()));
            }
        }
        match name {
            "lemma" | "title" => n.lemma.clone().map(Value::Str),
            "onym" => n.onym.clone().map(Value::Str),
            // On an index mark the onym IS the term (ruling #36).
            "term" if n.kind == Kind::IndexMark => n.onym.clone().map(Value::Str),
            // A ref's word: the identifier as written. Never the
            // resolved content — that is what `-->` is for.
            "target" if matches!(n.kind, Kind::Ref | Kind::Cit) => {
                n.target.clone().map(Value::Str)
            }
            // A structured entry answers its campi (auctor,
            // titulus, annus, …) — the bibliogramma vocabulary,
            // canonicalized upstream.
            _ if n.kind == Kind::Bib => {
                let canon = self
                    .bib_aliases
                    .get(&name.to_lowercase())
                    .map(String::as_str)
                    .unwrap_or(name);
                n.fields
                    .iter()
                    .find(|(k, _)| k == canon)
                    .map(|(_, v)| Value::Str(v.clone()))
            }
            "hypograph" | "attribution" => n.hypograph.clone().map(Value::Str),
            "taxis" | "ord" => n.taxis.map(Value::Int),
            "grammata" | "body" => {
                let g = self.grammata(node);
                if g.is_empty() { None } else { Some(Value::Str(g)) }
            }
            "text" => Some(Value::Str(n.prose.clone())),
            _ => None,
        }
    }

    /// The default projection is the flattened prose of the
    /// subtree — lemma first, hypograph last.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        if n.kind == Kind::IndexMark {
            // Invisible in the surrounding prose; its own
            // projection is the term it declares.
            return Some(Value::Str(n.onym.clone().unwrap_or_default()));
        }
        if n.deixis {
            // The atrep rule: a resolved reference projects as its
            // target's rendered form, degrading to the raw onym
            // when dangling.
            return Some(Value::Str(match n.note_edge {
                Some(body) => self.nodes[body.0 as usize].prose.clone(),
                None => n.onym.clone().unwrap_or_default(),
            }));
        }
        if n.kind == Kind::Ref {
            // The authored link text; LaTeX \ref has none, so the
            // atrep projection rule applies — the target's
            // rendered form (its lemma, else its prose), degrading
            // to the raw target when dangling.
            if !n.text.is_empty() {
                return Some(Value::Str(n.text.clone()));
            }
            return Some(Value::Str(match n.ref_edge {
                Some(t) => {
                    let b = &self.nodes[t.0 as usize];
                    b.lemma.clone().unwrap_or_else(|| b.prose.clone())
                }
                None => n.target.clone().unwrap_or_default(),
            }));
        }
        if n.kind == Kind::Bib && n.prose.is_empty() && !n.fields.is_empty() {
            // A structured entry's plain form: its field values in
            // source order — the full data, no citation styling.
            return Some(Value::Str(
                n.fields
                    .iter()
                    .map(|(_, v)| v.as_str())
                    .collect::<Vec<_>>()
                    .join(". "),
            ));
        }
        if n.kind == Kind::Cit {
            // The mark's own identity: the raw key. The entry is
            // one arrow away (`//cit--> ::`); rendered citation
            // styles are presentation, not structure.
            return Some(Value::Str(n.target.clone().unwrap_or_default()));
        }
        if n.kind == Kind::Anchor {
            // Invisible in the prose; its own projection is the
            // name it bears.
            return Some(Value::Str(n.onym.clone().unwrap_or_default()));
        }
        Some(Value::Str(n.prose.clone()))
    }

    /// The edge label is the family name both ends share —
    /// `->footnote` / `->endnote` — the node's own kind, since
    /// resolution re-kinds an open-family callout to its body.
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let n = &self.nodes[node.0 as usize];
        if let Some(t) = n.ref_edge {
            // The atrep rule: a resolved mention emits its typed
            // crosslink — `->ref`, or `->cit` for a citation.
            let label = if n.kind == Kind::Cit { "cit" } else { "ref" };
            return vec![(label.to_string(), t)];
        }
        match n.note_edge {
            Some(body) => vec![(n.kind.name().unwrap_or("footnote").to_string(), body)],
            None => Vec::new(),
        }
    }

    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let n = &self.nodes[node.0 as usize];
        let mut out: Vec<(String, NodeId)> = n
            .ref_cites
            .iter()
            .map(|&c| {
                let label = if self.nodes[c.0 as usize].kind == Kind::Cit {
                    "cit"
                } else {
                    "ref"
                };
                (label.to_string(), c)
            })
            .collect();
        let label = n.kind.name().unwrap_or("footnote");
        out.extend(n.cites.iter().map(|&c| (label.to_string(), c)));
        out
    }

    /// `//ref-->`: land on the bearer — the block if a block
    /// bears the name, the point anchor if a point does. The
    /// text-level hints choose the reading: `-->block` normalizes
    /// a point landing to its enclosing block, `-->point` selects
    /// point targets only; bare is the honest exact landing.
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let n = &self.nodes[node.0 as usize];
        if !matches!(n.kind, Kind::Ref | Kind::Cit) || property != "target" {
            return None;
        }
        let landed = n.ref_edge?;
        let is_point = self.nodes[landed.0 as usize].kind == Kind::Anchor;
        match hint {
            None | Some("*") => Some(landed),
            Some("point") => is_point.then_some(landed),
            Some("block") => {
                if !is_point {
                    return Some(landed);
                }
                // The anchor's enclosing block.
                let mut at = self.nodes[landed.0 as usize].parent;
                while let Some(b) = at {
                    let bn = &self.nodes[b.0 as usize];
                    if bn.kind != Kind::Document
                        && !matches!(bn.kind, Kind::Ref | Kind::Anchor | Kind::IndexMark)
                    {
                        return Some(b);
                    }
                    at = bn.parent;
                }
                None
            }
            Some(_) => None,
        }
    }

    /// The bare arrow's property: a ref resolves its target.
    fn ref_property(&self, node: NodeId) -> Option<String> {
        matches!(self.nodes[node.0 as usize].kind, Kind::Ref | Kind::Cit)
            .then(|| "target".to_string())
    }

    /// A ref the producer declared external: the absolute URL,
    /// relative targets joined against the document's own URL —
    /// the same acquisition rung the html DOM reading speaks.
    fn external_ref(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<String> {
        if !matches!(hint, None | Some("*")) {
            return None;
        }
        let n = &self.nodes[node.0 as usize];
        if n.kind != Kind::Ref || property != "target" || n.internal {
            return None;
        }
        let t = n.target.as_deref()?.trim();
        if t.is_empty() || t.starts_with('#') {
            return None;
        }
        let u = match url::Url::parse(t) {
            Ok(u) => u,
            Err(url::ParseError::RelativeUrlWithoutBase) => {
                self.document_url.as_ref()?.join(t).ok()?
            }
            Err(_) => return None,
        };
        if u.scheme() != "http" && u.scheme() != "https" {
            return None;
        }
        Some(u.into())
    }

    /// A sibling document's `#fragment` lands here: the bearer of
    /// that name — a labeled block, or a point anchor.
    fn resolve_fragment(&self, _node: NodeId, fragment: &str) -> Option<NodeId> {
        self.onyms.get(fragment).copied()
    }

    /// Ruling #29: the text level's surface is the vocabulary
    /// itself — no document can introduce a property name — so
    /// its two annotations answer at `::` as well.
    fn aliased_metadata(&self, _node: NodeId) -> &'static [&'static str] {
        &["level", "lang", "form"]
    }

    /// `::::level` on sections (the source heading level) and
    /// `::::lang` on verbatim blocks (the declared language).
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        match key {
            "level" => n.level.map(|l| Value::Int(l as i64)),
            "lang" => n.lang.clone().map(Value::Str),
            // `::::resolved` on a callout: the broken-apparatus
            // linter's fact (ruling #35).
            "resolved" if n.deixis => Some(Value::Bool(!n.dangling)),
            // …and on an internal ref: the broken-cross-reference
            // linter's fact. External refs resolve at query time,
            // so the fact is not theirs to answer.
            "resolved" if n.kind == Kind::Cit || (n.kind == Kind::Ref && n.internal) => {
                Some(Value::Bool(n.ref_edge.is_some()))
            }
            // The declared spelling was a margin form (a Tufte
            // sidenote): family footnote, placement preserved.
            "form" if n.margin => Some(Value::Str("margin".to_string())),
            // The entry's genus, also queryable as an annotation.
            "genus" if n.kind == Kind::Bib => n.genus.clone().map(Value::Str),
            _ => None,
        }
    }
}
