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

use quarb::source::SourceDecoding;
use quarb::{AstAdapter, NodeId, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

/// A parsed inner arbor.
enum Inner {
    Json(quarb_json::JsonAdapter),
    Xml(quarb_xml::XmlAdapter),
    Html(quarb_html::HtmlAdapter),
    Text(quarb_text::TextModel),
    Csv(quarb_csv::CsvAdapter),
    #[cfg(feature = "source")]
    Syntax(quarb_tree_sitter::TreeSitterAdapter),
    #[cfg(feature = "source")]
    Code(quarb_code::CodeModel),
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
            Inner::Text(a) => a,
            Inner::Csv(a) => a,
            #[cfg(feature = "source")]
            Inner::Syntax(a) => a,
            #[cfg(feature = "source")]
            Inner::Code(a) => a,
            Inner::Archive(a) => &**a,
        }
    }

    fn locator(&self, node: NodeId) -> String {
        match self {
            Inner::Json(a) => a.pointer(node),
            Inner::Xml(a) => a.locator(node),
            Inner::Html(a) => a.locator(node),
            Inner::Text(a) => a.locator(node),
            Inner::Csv(a) => a.locator(node),
            #[cfg(feature = "source")]
            Inner::Syntax(a) => a.locator(node),
            #[cfg(feature = "source")]
            Inner::Code(a) => a.locator(node),
            Inner::Archive(a) => a.locator(node, |o| a.outer().locator(o)),
        }
    }
}

/// The level source-file leaves graft at: the syntax level (the
/// literal parse — the default) or the code level (identifiers
/// as names). Chosen per mount by the `code:` target prefix, and
/// inherited by nested composes (an archive inside a `code:`
/// mount grafts its source entries at the code level too) — the
/// filesystem-into-declarations namespace has no seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceGraft {
    #[default]
    Syntax,
    Code,
}

/// The level marked-up documents (`.html`, `.htm`, `.md`) graft
/// at: their DOM by default, or the text level — sections,
/// paragraphs, lists, and what the page declares about itself
/// in its head — when the mount asks for the reader's model
/// (`text:` on an archive or directory).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DocumentGraft {
    #[default]
    Dom,
    Text,
}

/// The declaration reporter a host hands the source boundary: by
/// the source's name, the XML prologue for XML-family names, the
/// HTML prescan for HTML names, both tried (prologue first) for a
/// name that says nothing — a plain text file declares nothing
/// either way.
pub fn declared_encoding(name: &str, bytes: &[u8]) -> Option<String> {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "xml" | "svg" | "xhtml" | "fb2" | "tei" | "rss" | "atom" | "opml" | "xsd" | "xsl" => {
            quarb_xml::declared_encoding(bytes)
        }
        "html" | "htm" => quarb_html::declared_encoding(bytes),
        // RTF's header names its code page (`\ansicpg1251`).
        "rtf" => quarb_text_rtf::declared_encoding(bytes),
        _ => quarb_xml::declared_encoding(bytes).or_else(|| quarb_html::declared_encoding(bytes)),
    }
}

/// The rule a host mount carries: `?encoding=` validated at open,
/// the reporter above.
pub fn source_decoding(
    explicit: Option<&str>,
) -> Result<SourceDecoding, quarb::source::SourceError> {
    SourceDecoding::default()
        .with_declared(declared_encoding)
        .with_option(explicit)
}

/// Read one file at the source boundary under the host rule:
/// `?encoding=`, then a byte-order mark, then what the file
/// declares, then strict UTF-8.
pub fn read_source(
    path: &std::path::Path,
    explicit: Option<&str>,
) -> std::io::Result<Result<quarb::source::Decoded, quarb::source::SourceError>> {
    let bytes = std::fs::read(path)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    Ok(source_decoding(explicit).and_then(|d| d.decode(name, &bytes)))
}

/// Whether a leaf's name says archive: the one list quarb-archive
/// keeps.
fn archive_name(name: &str) -> bool {
    quarb_archive::archive_extension(name.rsplit('.').next().unwrap_or(""))
}

/// The notations a leaf can be declared to carry (spec: Grafting):
/// the one vocabulary `?format=NOTATION` on a mount and `<NOTATION>`
/// on a hop share, the Text Readings' names of the formats. Source
/// languages come besides, by their grammar names.
pub const NOTATIONS: &[&str] = &[
    "json", "jsonl", "yaml", "toml", "xml", "html", "markdown", "text", "csv", "tsv", "fb2",
];

/// The canonical notation a name declares by its extension (`.json`,
/// `.yml`, `.htm`, `.ndjson`), a source language by its grammar's
/// extension; None for no extension or one no reader takes.
pub fn notation_of_name(name: &str) -> Option<String> {
    let (_, ext) = name.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    let n = match ext.as_str() {
        "json" => "json",
        "jsonl" | "ndjson" => "jsonl",
        "xml" | "svg" | "xhtml" => "xml",
        "fb2" => "fb2",
        "html" | "htm" => "html",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "md" | "markdown" => "markdown",
        "txt" => "text",
        "csv" => "csv",
        "tsv" => "tsv",
        _ => {
            #[cfg(feature = "source")]
            if quarb_tree_sitter::supported(&ext) {
                return Some(ext);
            }
            return None;
        }
    };
    Some(n.to_string())
}

/// Whether a word names a notation (`?format=`, `<NOTATION>`).
pub fn is_notation(name: &str) -> bool {
    let n = canonical_notation(name);
    if NOTATIONS.contains(&n.as_str()) {
        return true;
    }
    #[cfg(feature = "source")]
    if quarb_tree_sitter::supported(&n) {
        return true;
    }
    false
}

/// A notation's canonical spelling: the extension synonyms read as
/// their notation (`yml` yaml, `md` markdown, `ndjson` jsonl, `txt`
/// text).
pub fn canonical_notation(name: &str) -> String {
    let n = name.trim().to_ascii_lowercase();
    match n.as_str() {
        "yml" => "yaml".into(),
        "md" => "markdown".into(),
        "ndjson" => "jsonl".into(),
        "txt" | "plain" => "text".into(),
        "htm" => "html".into(),
        _ => n,
    }
}

/// The notation a text declares about itself in its first bytes
/// (the Text Readings' declared identity): an XML declaration or a
/// DOCTYPE. Nothing else counts; a brace or a bracket is a guess,
/// not a declaration.
pub fn self_declared(content: &str) -> Option<&'static str> {
    let t = content.trim_start_matches('\u{feff}').trim_start();
    if t.starts_with("<?xml") {
        return Some("xml");
    }
    if t.len() >= 9 && t[..9].eq_ignore_ascii_case("<!doctype") {
        let rest = t[9..].trim_start();
        return Some(
            if rest.len() >= 4 && rest[..4].eq_ignore_ascii_case("html") {
                "html"
            } else {
                "xml"
            },
        );
    }
    None
}

/// Parse `content` as `notation` (canonical): the reader that
/// notation names, at the mount's graft levels. None when the text
/// does not parse as it, or no reader here takes the notation.
fn parse_as(
    notation: &str,
    content: &str,
    graft: SourceGraft,
    doc: DocumentGraft,
    csv: quarb_csv::CsvOptions,
) -> Option<Inner> {
    #[cfg(not(feature = "source"))]
    let _ = graft;
    let ext = canonical_notation(notation);
    match ext.as_str() {
        "json" => {
            return quarb_json::JsonAdapter::parse(content)
                .ok()
                .map(Inner::Json);
        }
        "jsonl" => {
            return quarb_json::JsonAdapter::parse_lines(content)
                .ok()
                .map(Inner::Json);
        }
        "xml" => {
            // A FictionBook declared by its namespace grafts at the
            // text level through the koine route when the level
            // asks for it; the DOM otherwise, like any XML.
            #[cfg(feature = "koine")]
            if doc == DocumentGraft::Text
                && quarb_text_koine::detect_xml_kind(content) == Some("fb2")
                && let Ok(m) = quarb_text_koine::parse_xml_as(content, "fb2")
            {
                return Some(Inner::Text(m));
            }
            return quarb_xml::XmlAdapter::parse(content).ok().map(Inner::Xml);
        }
        "fb2" => {
            #[cfg(feature = "koine")]
            if doc == DocumentGraft::Text
                && let Ok(m) = quarb_text_koine::parse_xml_as(content, "fb2")
            {
                return Some(Inner::Text(m));
            }
            return quarb_xml::XmlAdapter::parse(content).ok().map(Inner::Xml);
        }
        "html" => {
            return Some(match doc {
                DocumentGraft::Dom => Inner::Html(quarb_html::HtmlAdapter::parse(content)),
                DocumentGraft::Text => Inner::Text(quarb_text_html::parse(content)),
            });
        }
        "yaml" => return quarb_yaml::parse(content).ok().map(Inner::Json),
        "toml" => return quarb_toml::parse(content).ok().map(Inner::Json),
        "markdown" => {
            return Some(match doc {
                DocumentGraft::Dom => Inner::Html(quarb_markdown::parse(content)),
                DocumentGraft::Text => Inner::Text(quarb_text_markdown::parse(content)),
            });
        }
        // Plain text grafts at the text level (blank-line
        // paragraphs); html/md keep their DOM-level graft.
        "text" => return Some(Inner::Text(quarb_text::TextModel::parse_plain(content))),
        "csv" => {
            return quarb_csv::CsvAdapter::parse_declared(content, b',', csv)
                .ok()
                .map(Inner::Csv);
        }
        "tsv" => {
            return quarb_csv::CsvAdapter::parse_declared(content, b'\t', csv)
                .ok()
                .map(Inner::Csv);
        }
        #[cfg(feature = "source")]
        ext if quarb_tree_sitter::supported(ext) => {
            return match graft {
                SourceGraft::Syntax => quarb_tree_sitter::TreeSitterAdapter::parse(content, ext)
                    .ok()
                    .map(Inner::Syntax),
                SourceGraft::Code => quarb_code::CodeModel::parse(content, ext)
                    .ok()
                    .map(Inner::Code),
            };
        }
        _ => {}
    }
    None
}

/// One graft: an inner arbor mounted at an outer leaf.
struct Graft {
    outer: NodeId,
    inner: Inner,
    /// The notation the leaf was read as.
    notation: String,
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
/// A host's reader for a document leaf: the path in, its text-level
/// model out, or `None` to leave the leaf to the default grafting.
pub type DocumentReader = std::rc::Rc<dyn Fn(&std::path::Path) -> Option<quarb_text::TextModel>>;

/// A host's reader for a document leaf that has no path: the outer
/// adapter and the leaf in, its text-level model out (a row's body
/// under `?doc=`, a model file's `document` member), or `None` to
/// leave the leaf to the default grafting (spec: The Corpus Reading).
pub type DocumentNodes<A> = std::rc::Rc<dyn Fn(&A, NodeId) -> Option<quarb_text::TextModel>>;

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
    /// The level source-file leaves graft at.
    source_graft: SourceGraft,
    /// The level marked-up documents graft at.
    document_graft: DocumentGraft,
    /// Maps an outer leaf to the URL its document is served
    /// from, so a text-level graft joins relative references
    /// against it and `-->` can land on a sibling document.
    document_url: Option<fn(&A, NodeId) -> Option<String>>,
    /// URL → outer leaf, built on first `document_by_ref`.
    urls: RefCell<Option<HashMap<String, NodeId>>>,
    /// The host's own reading of a document leaf, by path: when it
    /// answers, its model is the graft (a corpus reading with its
    /// tokens, a format this crate does not parse); when it
    /// declines, the leaf grafts as it otherwise would.
    document_reader: Option<DocumentReader>,
    /// The mount's rule at the source boundary (`?encoding=` and
    /// the declaration reporter): every leaf this layer reads by
    /// path decodes under it, and the layer answers `:::encoding`
    /// / `:::replaced` on the leaf it grafted. A leaf with no path
    /// grafts from the outer adapter's text as before.
    decoding: SourceDecoding,
    /// Per graft: the record of its decoding, when this layer did
    /// it.
    decoded: RefCell<HashMap<usize, (String, usize)>>,
    /// The mount's `?delimiter=` / `?decimal=`, for the delimited
    /// leaves it grafts.
    csv: quarb_csv::CsvOptions,
    /// The mount's `?format=NOTATION`: every leaf beneath reads as
    /// it (spec: Grafting).
    default_notation: Option<String>,
    /// The host's reading of document leaves that have no path.
    document_nodes: Option<DocumentNodes<A>>,
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
            source_graft: SourceGraft::default(),
            document_graft: DocumentGraft::default(),
            document_url: None,
            urls: RefCell::new(None),
            document_reader: None,
            decoding: SourceDecoding::default(),
            decoded: RefCell::new(HashMap::new()),
            csv: quarb_csv::CsvOptions::default(),
            default_notation: None,
            document_nodes: None,
        }
    }

    /// The delimited-text options for this mount (builder-style).
    /// The host's reading of document leaves by node: a leaf the
    /// reader answers grafts as that text-level model (the leaves
    /// `?doc=` names, a model file's `document` members).
    pub fn with_document_nodes(mut self, reader: DocumentNodes<A>) -> Self {
        self.document_nodes = Some(reader);
        self
    }

    /// Every leaf beneath the mount reads as `notation`, the mount's
    /// `?format=NOTATION` (spec: Grafting); a leaf whose text does not
    /// parse as it stays a leaf.
    pub fn with_default_notation(mut self, notation: Option<String>) -> Self {
        self.default_notation = notation.map(|n| canonical_notation(&n));
        self
    }

    pub fn with_csv_options(mut self, csv: quarb_csv::CsvOptions) -> Self {
        self.csv = csv;
        self
    }

    /// The source-boundary rule for this mount (builder-style):
    /// the `?encoding=` option and the reporter the host chose.
    /// Reaches grafted archives too.
    pub fn with_decoding(mut self, decoding: SourceDecoding) -> Self {
        self.decoding = decoding;
        self
    }

    /// Hand document leaves to the host's reader first
    /// (builder-style); needs the outer leaves' paths
    /// ([`with_source_paths`](Self::with_source_paths)).
    pub fn with_document_reader(mut self, reader: DocumentReader) -> Self {
        self.document_reader = Some(reader);
        self
    }

    /// Declare where each outer leaf's document is served from
    /// (builder-style): a text-level graft joins its relative
    /// references against that URL, and `-->` from any graft
    /// lands on the leaf holding the referenced document (its
    /// fragment's bearer when named) without a host registration.
    pub fn with_document_urls(mut self, document_url: fn(&A, NodeId) -> Option<String>) -> Self {
        self.document_url = Some(document_url);
        self
    }

    /// The outer leaf serving `url` (fragment-stripped), from the
    /// declared URLs; the whole outer tree is walked once.
    fn leaf_by_url(&self, url: &str) -> Option<NodeId> {
        let f = self.document_url?;
        if self.urls.borrow().is_none() {
            let mut map = HashMap::new();
            let mut stack = vec![self.outer.root()];
            while let Some(n) = stack.pop() {
                let kids = self.outer.children(n);
                if kids.is_empty() {
                    if let Some(u) = f(&self.outer, n) {
                        map.entry(u.split('#').next().unwrap_or(&u).to_string())
                            .or_insert(n);
                    }
                } else {
                    stack.extend(kids);
                }
            }
            *self.urls.borrow_mut() = Some(map);
        }
        self.urls.borrow().as_ref()?.get(url).copied()
    }

    /// Choose the level source-file leaves graft at
    /// (builder-style; the default is the syntax level).
    pub fn with_source_graft(mut self, graft: SourceGraft) -> Self {
        self.source_graft = graft;
        self
    }

    /// Choose the level marked-up documents graft at
    /// (builder-style; the default is the DOM).
    pub fn with_document_graft(mut self, graft: DocumentGraft) -> Self {
        self.document_graft = graft;
        self
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
    /// The graft's fill for a node inside a grafted document: the
    /// outer leaf's provenance (the file's path and mtime) with the
    /// mtime re-labelled as the source rung, and the node's path
    /// inside the document.
    fn graft_fill(&self, outer_leaf: NodeId, inner_path: String) -> quarb::Provenance {
        let mut fill = self.outer.provenance(outer_leaf).as_source_rung();
        fill.path = Some(inner_path);
        fill
    }

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
        self.graft_probe(node, None)
    }

    /// Read an outer leaf as `notation`, the query's `<NOTATION>` at
    /// a hop (spec: Grafting): true when the leaf is grafted as that
    /// notation afterwards, whether by this call or before it.
    pub fn graft_as(&self, node: NodeId, notation: &str) -> bool {
        let want = canonical_notation(notation);
        if let Some(Some(g)) = self.probed.borrow().get(&node).copied() {
            return self.grafts.borrow()[g].notation == want;
        }
        // a leaf probed as nothing may still read under a request
        self.probed.borrow_mut().remove(&node);
        match self.graft_probe(node, Some(&want)) {
            Some(g) => self.grafts.borrow()[g].notation == want,
            None => false,
        }
    }

    fn graft_probe(&self, node: NodeId, requested: Option<&str>) -> Option<usize> {
        if let Some(&g) = self.probed.borrow().get(&node) {
            return g;
        }
        let g = (|| {
            if !self.outer.children(node).is_empty() {
                return None;
            }
            // a document leaf the host reads by node (spec: The
            // Corpus Reading): its model is the graft
            if let Some(read) = &self.document_nodes
                && let Some(model) = read(&self.outer, node)
            {
                let mut grafts = self.grafts.borrow_mut();
                grafts.push(Graft {
                    outer: node,
                    inner: Inner::Text(model),
                    notation: "document".into(),
                });
                return Some(grafts.len() - 1);
            }
            let name = self.outer.name(node)?;
            if archive_name(&name)
                && let Some(path_of) = self.source_path
                && let Some(path) = path_of(&self.outer, node)
                && let Ok(a) =
                    quarb_archive::ArchiveAdapter::open_with_decoding(&path, self.decoding.clone())
            {
                let inner = Inner::Archive(Box::new(
                    ComposeAdapter::new(a)
                        .with_source_graft(self.source_graft)
                        .with_document_graft(self.document_graft)
                        .with_decoding(self.decoding.clone())
                        .with_csv_options(self.csv)
                        .with_default_notation(self.default_notation.clone()),
                ));
                let mut grafts = self.grafts.borrow_mut();
                grafts.push(Graft {
                    outer: node,
                    inner,
                    notation: "archive".into(),
                });
                return Some(grafts.len() - 1);
            }
            if let Some(read) = &self.document_reader
                && let Some(path_of) = self.source_path
                && let Some(path) = path_of(&self.outer, node)
                && let Some(model) = read(&path)
            {
                let mut grafts = self.grafts.borrow_mut();
                grafts.push(Graft {
                    outer: node,
                    inner: Inner::Text(model),
                    notation: "document".into(),
                });
                return Some(grafts.len() - 1);
            }
            // A leaf grafts only as declared (spec: Grafting): by
            // the substrate (a json column), by the mount's
            // `?format=`, by its name's extension, or by what the
            // bytes say of themselves (an XML declaration, a
            // DOCTYPE). A leaf nothing declares is a leaf, unread;
            // a named leaf whose extension no reader takes (an
            // image, a binary) likewise. Only a nameless leaf is
            // worth reading for its own declaration.
            let declared = requested
                .map(|r| canonical_notation(r))
                .or_else(|| {
                    self.outer
                        .declared_notation(node)
                        .map(|n| canonical_notation(&n))
                })
                .or_else(|| self.default_notation.clone())
                .or_else(|| notation_of_name(&name));
            if declared.is_none() && name.contains('.') {
                return None;
            }
            // Through the path when there is one: the bytes meet
            // the boundary here, under the mount's rule, and the
            // record stays with the graft. A leaf whose bytes refuse
            // to decode (not UTF-8, nothing declared) stays a leaf.
            let mut record = None;
            let content = match self
                .source_path
                .and_then(|path_of| path_of(&self.outer, node))
                .and_then(|p| std::fs::read(p).ok())
            {
                Some(bytes) => {
                    let d = self.decoding.decode(&name, &bytes).ok()?;
                    record = Some((d.encoding, d.replaced));
                    d.text
                }
                None => match self.outer.default_value(node)? {
                    Value::Str(s) => s,
                    _ => return None,
                },
            };
            let notation = declared.or_else(|| self_declared(&content).map(str::to_string))?;
            let mut inner = parse_as(
                &notation,
                &content,
                self.source_graft,
                self.document_graft,
                self.csv,
            )?;
            if let (Inner::Text(model), Some(f)) = (&mut inner, self.document_url)
                && let Some(u) = f(&self.outer, node)
            {
                model.set_document_url(&u);
            }
            let mut grafts = self.grafts.borrow_mut();
            grafts.push(Graft {
                outer: node,
                inner,
                notation,
            });
            if let Some(r) = record {
                self.decoded.borrow_mut().insert(grafts.len() - 1, r);
            }
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

    /// A child by name: the outer source's own lookup first (a
    /// database reads `/books/7` by key), and the listing only where
    /// the node may hold a grafted document — a leaf with a path or
    /// with text. A miss on a node that cannot hold one stays a
    /// miss, without listing its children.
    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        match self.split(node) {
            Some((g, inner)) => {
                let ids: Vec<NodeId> = {
                    let grafts = self.grafts.borrow();
                    grafts[g].inner.adapter().children_named(inner, name)
                };
                ids.into_iter().map(|c| self.wrap(g, c)).collect()
            }
            None => {
                let named = self.outer.children_named(node, name);
                if !named.is_empty() {
                    return named;
                }
                if self.source_path.is_none()
                    && !matches!(self.outer.default_value(node), Some(Value::Str(_)))
                {
                    return Vec::new();
                }
                self.children(node)
                    .into_iter()
                    .filter(|&c| self.answers_to(c, name))
                    .collect()
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

    fn child_host(&self, node: NodeId) -> NodeId {
        match self.split(node) {
            Some((g, inner)) => {
                let h = self.grafts.borrow()[g].inner.adapter().child_host(inner);
                self.wrap(g, h)
            }
            None => self.outer.child_host(node),
        }
    }

    fn properties(&self, node: NodeId) -> Vec<(String, Value)> {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g].inner.adapter().properties(inner),
            None => self.outer.properties(node),
        }
    }

    /// The walk is open when the outer tree opens to it (a graft is
    /// read whole once reached).
    fn walks_for_references(&self) -> bool {
        self.outer.walks_for_references()
    }
    /// A declaration answers on its own side of the boundary; across
    /// it only a reference property of the source's own can reach.
    fn can_refer_to(&self, source: NodeId, target: NodeId) -> Option<bool> {
        match (self.split(source), self.split(target)) {
            (None, None) => self.outer.can_refer_to(source, target),
            (Some((g, s)), Some((h, t))) if g == h => {
                self.grafts.borrow()[g].inner.adapter().can_refer_to(s, t)
            }
            _ => self.ref_property(source).map(|_| true),
        }
    }
    fn reference_properties(&self, source: NodeId, target: NodeId) -> Vec<String> {
        match (self.split(source), self.split(target)) {
            (None, None) => self.outer.reference_properties(source, target),
            (Some((g, s)), Some((h, t))) if g == h => self.grafts.borrow()[g]
                .inner
                .adapter()
                .reference_properties(s, t),
            _ => self.ref_property(source).into_iter().collect(),
        }
    }
    /// The outer source's own index over a scope of its own nodes
    /// (a database reading `/books/*` by key).
    fn index_referrers(
        &self,
        scope: &str,
        property: &str,
        label: Option<&str>,
        target: NodeId,
    ) -> Option<Vec<NodeId>> {
        match self.split(target) {
            None => self.outer.index_referrers(scope, property, label, target),
            Some(_) => None,
        }
    }
    fn tree_identities(&self) -> Vec<(String, NodeId)> {
        self.outer.tree_identities()
    }
    fn reverse_label(&self, target: NodeId) -> Option<String> {
        match self.split(target) {
            Some((g, inner)) => self.grafts.borrow()[g].inner.adapter().reverse_label(inner),
            None => self.outer.reverse_label(target),
        }
    }
    fn resolve_needs_label(&self, node: NodeId, property: &str) -> bool {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g]
                .inner
                .adapter()
                .resolve_needs_label(inner, property),
            None => self.outer.resolve_needs_label(node, property),
        }
    }
    fn property_names(&self, node: NodeId) -> Vec<String> {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g]
                .inner
                .adapter()
                .property_names(inner),
            None => self.outer.property_names(node),
        }
    }
    /// The outer source, or any document grafted so far.
    fn resolves_references(&self) -> bool {
        self.outer.resolves_references()
            || self
                .grafts
                .borrow()
                .iter()
                .any(|g| g.inner.adapter().resolves_references())
    }

    /// The outer tree's non-tree nodes, then each graft's (the
    /// grafts already read).
    fn non_tree_nodes(&self) -> Vec<NodeId> {
        let mut out = self.outer.non_tree_nodes();
        let grafts = self.grafts.borrow();
        for (g, graft) in grafts.iter().enumerate() {
            out.extend(
                graft
                    .inner
                    .adapter()
                    .non_tree_nodes()
                    .into_iter()
                    .map(|n| self.wrap(g, n)),
            );
        }
        out
    }

    /// A written name reaches a node by the owning adapter's rule
    /// (its aliases included).
    fn answers_to(&self, node: NodeId, name: &str) -> bool {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g]
                .inner
                .adapter()
                .answers_to(inner, name),
            None => self.outer.answers_to(node, name),
        }
    }

    /// The graft root IS the outer leaf, so the leaf wears the
    /// grafted document's own traits too — a page's declared tags
    /// on the file that holds it.
    fn traits(&self, node: NodeId) -> Vec<String> {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g].inner.adapter().traits(inner),
            None => {
                let mut out = self.outer.traits(node);
                if let Some(g) = self.graft_at(node) {
                    let grafts = self.grafts.borrow();
                    let a = grafts[g].inner.adapter();
                    for t in a.traits(a.root()) {
                        if !out.contains(&t) {
                            out.push(t);
                        }
                    }
                }
                out
            }
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g]
                .inner
                .adapter()
                .property(inner, name),
            // The graft root IS the outer leaf, so a property the
            // outer can't answer falls through to the grafted
            // document's root — `::key` dual exposure works *at*
            // the graft, not only one level in.
            None => self.outer.property(node, name).or_else(|| {
                let g = self.graft_at(node)?;
                let grafts = self.grafts.borrow();
                let a = grafts[g].inner.adapter();
                a.property(a.root(), name)
            }),
        }
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g].inner.adapter().default_value(inner),
            // A leaf that did not graft (plain text, a format this
            // crate does not parse) is still a source: read through
            // its path under the mount's rule, so `?encoding=` and
            // a declaration reach it. The outer adapter's own text
            // (strict UTF-8) stands where there is no path.
            None => {
                if self.decoding.explicit.is_some()
                    && self.outer.children(node).is_empty()
                    && let Some(path_of) = self.source_path
                    && let Some(p) = path_of(&self.outer, node)
                    && p.is_file()
                    && let Ok(bytes) = std::fs::read(&p)
                {
                    let name = self.outer.name(node).unwrap_or_default();
                    return self
                        .decoding
                        .decode(&name, &bytes)
                        .ok()
                        .map(|d| Value::Str(d.text));
                }
                self.outer.default_value(node)
            }
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g].inner.adapter().metadata(inner, key),
            // Like properties: what the outer cannot answer, the
            // grafted document's root may (`:::"og:type"` on the
            // file that holds the page).
            None => self.outer.metadata(node, key).or_else(|| {
                let g = self.graft_at(node)?;
                if matches!(key, "encoding" | "replaced") {
                    // A leaf the host's reader read by path has no
                    // record yet: decode its bytes under the mount's
                    // rule once, for the record alone.
                    if !self.decoded.borrow().contains_key(&g)
                        && let Some(path_of) = self.source_path
                        && let Some(p) = path_of(&self.outer, node)
                        && let Ok(bytes) = std::fs::read(&p)
                    {
                        let name = self.outer.name(node).unwrap_or_default();
                        if let Ok(d) = self.decoding.decode(&name, &bytes) {
                            self.decoded
                                .borrow_mut()
                                .insert(g, (d.encoding, d.replaced));
                        }
                    }
                    if let Some((enc, replaced)) = self.decoded.borrow().get(&g) {
                        return Some(match key {
                            "encoding" => Value::Str(enc.clone()),
                            _ => Value::Int(*replaced as i64),
                        });
                    }
                }
                let grafts = self.grafts.borrow();
                let a = grafts[g].inner.adapter();
                a.metadata(a.root(), key)
            }),
        }
    }

    fn metadata_keys(&self, node: NodeId) -> Vec<String> {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g].inner.adapter().metadata_keys(inner),
            None => {
                let mut keys = self.outer.metadata_keys(node);
                if self.graft_at(node).is_some() {
                    for k in ["encoding", "replaced"] {
                        if self.metadata(node, k).is_some() && !keys.iter().any(|x| x == k) {
                            keys.push(k.to_string());
                        }
                    }
                }
                keys
            }
        }
    }

    /// A graft's own aliases are not merged: a node either belongs
    /// to the outer document or to a graft, and the alias list is
    /// consulted only after the owning adapter's `property` misses.
    fn aliased_metadata(&self, node: NodeId) -> &'static [&'static str] {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g]
                .inner
                .adapter()
                .aliased_metadata(inner),
            None => self.outer.aliased_metadata(node),
        }
    }

    /// A graft is a provenance layer: the grafted document's own
    /// components win, and the outer *leaf* it was parsed from fills
    /// the rest (with an fs outer: the file's path and mtime). The
    /// jar-style composite locator is deliberately not the source —
    /// `?src` names a document, and the intra-document address is
    /// dpid's job.
    fn provenance(&self, node: NodeId) -> quarb::Provenance {
        match self.split(node) {
            Some((g, inner)) => {
                // Clone what we need out of the borrow before calling
                // into `self.outer` (which may re-enter the graft
                // tables).
                let (inner_prov, outer_leaf, inner_path) = {
                    let grafts = self.grafts.borrow();
                    let a = grafts[g].inner.adapter();
                    (
                        a.provenance(inner),
                        grafts[g].outer,
                        quarb::name_path(a, inner),
                    )
                };
                inner_prov.or(self.graft_fill(outer_leaf, inner_path))
            }
            None => self.outer.provenance(node),
        }
    }
    /// The same layering, entry by entry.
    fn provenance_list(&self, node: NodeId) -> quarb::ProvenanceList {
        match self.split(node) {
            Some((g, inner)) => {
                let (inner_list, outer_leaf, inner_path) = {
                    let grafts = self.grafts.borrow();
                    let a = grafts[g].inner.adapter();
                    (
                        a.provenance_list(inner),
                        grafts[g].outer,
                        quarb::name_path(a, inner),
                    )
                };
                inner_list.or(self.graft_fill(outer_leaf, inner_path))
            }
            None => self.outer.provenance_list(node),
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

    /// A trait test reaches the graft — and, on the outer leaf,
    /// the graft's root, as `traits` does.
    fn has_trait(&self, node: NodeId, name: &str) -> bool {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g]
                .inner
                .adapter()
                .has_trait(inner, name),
            None => {
                // `<json>` on a hop reads the leaf as JSON (spec:
                // Grafting): the trait holds when it grafts as such
                if is_notation(name) && self.outer.children(node).is_empty() {
                    return self.graft_as(node, name) || self.outer.has_trait(node, name);
                }
                self.outer.has_trait(node, name)
                    || self.graft_at(node).is_some_and(|g| {
                        let grafts = self.grafts.borrow();
                        let a = grafts[g].inner.adapter();
                        a.has_trait(a.root(), name)
                    })
            }
        }
    }

    fn ref_property(&self, node: NodeId) -> Option<String> {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g].inner.adapter().ref_property(inner),
            None => self.outer.ref_property(node),
        }
    }

    /// A graft's external references (a text-level ref leaving
    /// its document) come through; an outer leaf's own do too.
    fn external_ref(
        &self,
        node: NodeId,
        property: &str,
        hint: Option<&str>,
    ) -> Option<quarb::ExternalRef> {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g]
                .inner
                .adapter()
                .external_ref(inner, property, hint),
            None => self.outer.external_ref(node, property, hint),
        }
    }

    fn ref_label(&self, node: NodeId, property: &str) -> Option<String> {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g]
                .inner
                .adapter()
                .ref_label(inner, property),
            None => self.outer.ref_label(node, property),
        }
    }

    /// A fragment lands inside the document: on an outer leaf
    /// that holds a graft, inside that graft.
    fn resolve_fragment(&self, node: NodeId, fragment: &str) -> Option<NodeId> {
        match self.split(node) {
            Some((g, inner)) => {
                let t = self.grafts.borrow()[g]
                    .inner
                    .adapter()
                    .resolve_fragment(inner, fragment)?;
                Some(self.wrap(g, t))
            }
            None => match self.graft_at(node) {
                Some(g) => {
                    let t = {
                        let grafts = self.grafts.borrow();
                        let a = grafts[g].inner.adapter();
                        a.resolve_fragment(a.root(), fragment)?
                    };
                    Some(self.wrap(g, t))
                }
                None => self.outer.resolve_fragment(node, fragment),
            },
        }
    }

    /// The leaf serving a URL, from the declared document URLs;
    /// else whatever the outer answers.
    fn document_by_ref(&self, id: &str) -> Option<NodeId> {
        if let Some(leaf) = self.leaf_by_url(id) {
            // The graft is parsed here so a fragment can land.
            let _ = self.graft_at(leaf);
            return Some(leaf);
        }
        self.outer.document_by_ref(id)
    }

    /// A reverse index answers on one side of the boundary only:
    /// inside a graft, the graft's own; on the outer tree, the
    /// outer's declared references (a catalog key, a model `ref`),
    /// which no graft can hold a referrer for.
    fn reverse_resolve(
        &self,
        node: NodeId,
        property: Option<&str>,
        hint: Option<&str>,
    ) -> Option<Vec<NodeId>> {
        match self.split(node) {
            Some((g, inner)) => {
                let v = self.grafts.borrow()[g]
                    .inner
                    .adapter()
                    .reverse_resolve(inner, property, hint)?;
                Some(v.into_iter().map(|t| self.wrap(g, t)).collect())
            }
            None => self.outer.reverse_resolve(node, property, hint),
        }
    }

    /// Inside a graft the graft's index answers; on the outer tree
    /// a name may also live inside grafts, so the walk stays.
    fn descendants_named(&self, node: NodeId, name: &str) -> Option<Vec<(NodeId, usize)>> {
        let (g, inner) = self.split(node)?;
        let v = self.grafts.borrow()[g]
            .inner
            .adapter()
            .descendants_named(inner, name)?;
        Some(v.into_iter().map(|(t, d)| (self.wrap(g, t), d)).collect())
    }

    fn descendants_where(
        &self,
        node: NodeId,
        name: &str,
        property: &str,
        value: &Value,
    ) -> Option<Vec<(NodeId, usize)>> {
        let (g, inner) = self.split(node)?;
        let v = self.grafts.borrow()[g]
            .inner
            .adapter()
            .descendants_where(inner, name, property, value)?;
        Some(v.into_iter().map(|(t, d)| (self.wrap(g, t), d)).collect())
    }

    fn select(&self, from: NodeId, plan: &quarb::StepPlan<'_>) -> Option<quarb::Selection> {
        match self.split(from) {
            // Inside a graft: its adapter's own answer, wrapped.
            Some((g, inner)) => {
                let sel = self.grafts.borrow()[g]
                    .inner
                    .adapter()
                    .select(inner, plan)?;
                Some(quarb::Selection {
                    nodes: sel
                        .nodes
                        .into_iter()
                        .map(|(t, d)| (self.wrap(g, t), d))
                        .collect(),
                    applied: sel.applied,
                })
            }
            // Outside: the outer adapter's descendants may cross
            // into grafts, which the walk handles; a child hop is
            // this adapter's own `children_named`.
            None => match plan.axis {
                quarb::PlanAxis::Child => quarb::adapter::select_by_hooks(self, from, plan),
                quarb::PlanAxis::Descendant => None,
            },
        }
    }

    fn index_lookup(&self, scope: &str, property: &str, value: &Value) -> Option<Vec<NodeId>> {
        self.outer.index_lookup(scope, property, value)
    }

    fn select_plan(&self, scope: &[&str], plan: &quarb::StepPlan<'_>) -> Option<Vec<bool>> {
        self.outer.select_plan(scope, plan)
    }

    fn estimate(&self, scope: &[&str], plan: &quarb::StepPlan<'_>) -> Option<quarb::Estimate> {
        self.outer.estimate(scope, plan)
    }

    fn has_node(&self, node: NodeId) -> bool {
        match self.split(node) {
            Some((g, inner)) => self.grafts.borrow()[g].inner.adapter().has_node(inner),
            None => self.outer.has_node(node),
        }
    }

    fn unit_scale(&self, expr: &str) -> Option<(f64, String)> {
        self.outer.unit_scale(expr)
    }

    fn quantifier_bound(&self) -> usize {
        self.outer.quantifier_bound()
    }

    fn prefetch_links(&self, nodes: &[NodeId], dir: quarb::LinkDir) {
        let outer: Vec<NodeId> = nodes
            .iter()
            .copied()
            .filter(|n| self.split(*n).is_none())
            .collect();
        if !outer.is_empty() {
            self.outer.prefetch_links(&outer, dir);
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
