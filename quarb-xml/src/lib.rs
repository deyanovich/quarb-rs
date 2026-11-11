//! XML adapter for Quarb.
//!
//! Maps a well-formed XML document onto the arbor model, parsing
//! with `quick-xml`. Unlike the HTML adapter, parsing is strict:
//! mismatched or unclosed tags, duplicate attributes, and unknown
//! entity references are errors.
//!
//! - Elements are the nodes; text, CDATA, comments, processing
//!   instructions, and the doctype are not navigated (text is
//!   exposed as a projection instead).
//! - A node's *name* is its tag exactly as written, including any
//!   namespace prefix (`dc:title`). Prefixes are not resolved to
//!   URIs. Since `:` is not a bare-name character in Quarb, a
//!   prefixed name is navigated with a quoted segment
//!   (`//'dc:title'`); `:::local-name` and `:::ns-prefix` metadata
//!   allow prefix-agnostic filtering (`//*[:::local-name =
//!   "title"]`). The document root is unnamed; the document
//!   element is its single child.
//! - Nodes have no traits: XML has no universal structural
//!   vocabulary, and leaf/container distinctions are already
//!   available as core metadata (`:is-leaf`, `:n-children`).
//! - Attributes are properties: `::pages`, `::href`. The default
//!   projection (`::`) and `::text` are the element's text
//!   content; `:::tag`, `:::id`, `:::local-name`, `:::ns-prefix`,
//!   `:::n-attrs`, and any `:::attr` expose metadata; `:::texts`
//!   is the list of the element's immediate text runs (XPath's
//!   `text()`).
//! - An attribute holding an ID reference resolves: `::ref-->`
//!   follows a bare IDREF value (`book="b1"`) or a fragment
//!   (`href="#b1"`) to the element with that `id` or `xml:id`.
//! - Input is UTF-8 text; an `encoding` declaration in the XML
//!   prologue is ignored. Only the five predefined entities and
//!   numeric character references are resolved.

use quarb::names::Names;
use quarb::{AstAdapter, NodeId, Value};
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Decoder, Reader};
use std::collections::HashMap;

/// The text with its white space collapsed to single spaces and
/// trimmed; `None` when nothing is left.
fn collapsed(text: &str) -> Option<String> {
    let mut words = text.split_whitespace();
    let mut out = words.next()?.to_string();
    for w in words {
        out.push(' ');
        out.push_str(w);
    }
    Some(out)
}

struct Node {
    /// The tag name as written, in the document's name table;
    /// `None` for the document root.
    tag: Option<u32>,
    /// The attributes: the name in the table, the value.
    attrs: Vec<(u32, String)>,
    /// The element's text content (all descendant text and CDATA,
    /// concatenated in document order).
    text: String,
    /// Where this element's text starts within its parent's: the
    /// parent's own text runs are what its children's texts leave
    /// (`:::texts`).
    at: u32,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
}

impl Node {
    fn attr(&self, names: &Names, name: &str) -> Option<&str> {
        let k = names.key(name)?;
        self.attrs
            .iter()
            .find(|(a, _)| *a == k)
            .map(|(_, v)| v.as_str())
    }
}

/// An error building an [`XmlAdapter`] from document text.
#[derive(Debug, thiserror::Error)]
pub enum XmlError {
    #[error(transparent)]
    Syntax(#[from] quick_xml::Error),
    #[error("unknown entity reference '&{0};'")]
    UnknownEntity(String),
    #[error("unclosed element '<{0}>'")]
    Unclosed(String),
    #[error("no root element")]
    NoRoot,
    #[error("content after the root element")]
    TrailingContent,
}

/// The encoding an XML document declares in its prologue
/// (`<?xml version="1.0" encoding="windows-1251"?>`), reported for
/// the engine's source boundary. The adapter decodes nothing: the
/// boundary applies the declaration after `?encoding=` and a
/// byte-order mark have had their say. The prologue is scanned as
/// ASCII-compatible bytes, as the XML recommendation's appendix F
/// reads it; a document whose first bytes are not `<?xml` declares
/// nothing.
pub fn declared_encoding(bytes: &[u8]) -> Option<String> {
    quarb::source::xml_declared_encoding(bytes)
}

/// A Quarb adapter over a parsed XML document.
pub struct XmlAdapter {
    nodes: Vec<Node>,
    /// The document's tag and attribute names, each once.
    names: Names,
    /// `id` / `xml:id` attribute value → the element carrying it.
    ids: HashMap<String, NodeId>,
    root: NodeId,
}

impl XmlAdapter {
    /// Parse `xml` and build the adapter. The document must be
    /// well-formed: exactly one root element, matched tags, unique
    /// attributes, and only predefined or numeric entities.
    pub fn parse(xml: &str) -> Result<Self, XmlError> {
        let mut nodes = vec![Node {
            tag: None,
            attrs: Vec::new(),
            text: String::new(),
            at: 0,
            parent: None,
            children: Vec::new(),
        }];
        let mut ids = HashMap::new();
        let root = NodeId(0);

        let mut names = Names::new();
        let mut reader = Reader::from_str(xml);
        let decoder = reader.decoder();
        // Indices of the currently open elements; the synthetic
        // root stays at the bottom.
        let mut stack: Vec<usize> = vec![0];

        loop {
            match reader.read_event()? {
                Event::Start(e) => {
                    let idx = intern(&mut nodes, &mut names, &mut ids, &stack, decoder, &e)?;
                    stack.push(idx);
                }
                Event::Empty(e) => {
                    intern(&mut nodes, &mut names, &mut ids, &stack, decoder, &e)?;
                }
                Event::End(_) => {
                    let closed = stack.pop().expect("end event matches an open element");
                    let text = std::mem::take(&mut nodes[closed].text);
                    let parent = *stack.last().expect("stack holds the root");
                    nodes[parent].text.push_str(&text);
                    nodes[closed].text = text;
                }
                Event::Text(e) => {
                    let open = *stack.last().expect("stack holds the root");
                    let text = e.xml_content().map_err(quick_xml::Error::from)?;
                    nodes[open].text.push_str(&text);
                }
                Event::CData(e) => {
                    let open = *stack.last().expect("stack holds the root");
                    let text = e.decode().map_err(quick_xml::Error::from)?;
                    nodes[open].text.push_str(&text);
                }
                Event::GeneralRef(e) => {
                    let open = *stack.last().expect("stack holds the root");
                    if let Some(ch) = e.resolve_char_ref()? {
                        nodes[open].text.push(ch);
                    } else {
                        let name = e.decode().map_err(quick_xml::Error::from)?;
                        let Some(resolved) = resolve_predefined_entity(&name) else {
                            return Err(XmlError::UnknownEntity(name.into_owned()));
                        };
                        nodes[open].text.push_str(resolved);
                    }
                }
                Event::Decl(_) | Event::PI(_) | Event::Comment(_) | Event::DocType(_) => {}
                Event::Eof => break,
            }
        }

        if stack.len() > 1 {
            let open = stack.pop().expect("checked non-root");
            let tag = nodes[open].tag.expect("open elements are tagged");
            return Err(XmlError::Unclosed(names.get(tag).to_string()));
        }
        match nodes[0].children.len() {
            0 => return Err(XmlError::NoRoot),
            1 => {}
            _ => return Err(XmlError::TrailingContent),
        }
        // The synthetic root's text is the document element's, not
        // any stray whitespace around it.
        nodes[0].text = nodes[nodes[0].children[0].0 as usize].text.clone();

        Ok(XmlAdapter {
            nodes,
            names,
            ids,
            root,
        })
    }

    /// A locator path to `node`, like `/library/book[2]/author`, for
    /// rendering. A `[n]` index is added only to disambiguate
    /// same-tag siblings.
    /// The element carrying `id` / `xml:id` = `id`, if any.
    pub fn node_by_id(&self, id: &str) -> Option<NodeId> {
        self.ids.get(id).copied()
    }

    pub fn locator(&self, node: NodeId) -> String {
        let mut segments = Vec::new();
        let mut cur = Some(node);
        while let Some(id) = cur {
            let n = &self.nodes[id.0 as usize];
            if let Some(tag) = n.tag {
                segments.push(self.segment(id, tag));
            }
            cur = n.parent;
        }
        segments.reverse();
        format!("/{}", segments.join("/"))
    }

    fn segment(&self, node: NodeId, tag: u32) -> String {
        let name = self.names.get(tag);
        let Some(parent) = self.nodes[node.0 as usize].parent else {
            return name.to_string();
        };
        let siblings = &self.nodes[parent.0 as usize].children;
        let same_tag: Vec<NodeId> = siblings
            .iter()
            .copied()
            .filter(|&s| self.nodes[s.0 as usize].tag == Some(tag))
            .collect();
        if same_tag.len() > 1 {
            let n = same_tag.iter().position(|&s| s == node).unwrap() + 1;
            format!("{name}[{n}]")
        } else {
            name.to_string()
        }
    }
}

/// Intern the element behind a `Start`/`Empty` event as a child of
/// the innermost open element, returning its arena index.
fn intern(
    nodes: &mut Vec<Node>,
    names: &mut Names,
    ids: &mut HashMap<String, NodeId>,
    stack: &[usize],
    decoder: Decoder,
    e: &BytesStart,
) -> Result<usize, XmlError> {
    let idx = nodes.len();
    let this = NodeId(idx as u64);
    let tag = names.intern(
        &decoder
            .decode(e.name().as_ref())
            .map_err(quick_xml::Error::from)?,
    );
    let mut attrs = Vec::new();
    for attr in e.attributes() {
        let attr = attr.map_err(quick_xml::Error::from)?;
        let key = decoder
            .decode(attr.key.as_ref())
            .map_err(quick_xml::Error::from)?
            .into_owned();
        let value = attr.decode_and_unescape_value(decoder)?.into_owned();
        if key == "id" || key == "xml:id" {
            ids.entry(value.clone()).or_insert(this);
        }
        attrs.push((names.intern(&key), value));
    }
    let parent = *stack.last().expect("stack holds the root");
    // (the parent's text stands still while this element is open)
    let at = u32::try_from(nodes[parent].text.len()).unwrap_or(u32::MAX);
    nodes.push(Node {
        tag: Some(tag),
        attrs,
        text: String::new(),
        at,
        parent: Some(NodeId(parent as u64)),
        children: Vec::new(),
    });
    nodes[parent].children.push(this);
    Ok(idx)
}

/// The name before / after the first `:` of a qualified name.
fn split_qualified(tag: &str) -> (Option<&str>, &str) {
    match tag.split_once(':') {
        Some((prefix, local)) => (Some(prefix), local),
        None => (None, tag),
    }
}

impl AstAdapter for XmlAdapter {
    /// The arena knows how many nodes a step reaches; nothing is read.
    fn estimate(&self, scope: &[&str], plan: &quarb::StepPlan<'_>) -> Option<quarb::Estimate> {
        quarb::adapter::arena_estimate(self, scope, plan, quarb::Latency::Memory, None)
    }

    fn root(&self) -> NodeId {
        self.root
    }

    /// In memory: an unscoped reverse resolution or value match may
    /// walk the arbor.
    fn walks_for_references(&self) -> bool {
        true
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.nodes[node.0 as usize].children.clone()
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.nodes[node.0 as usize]
            .tag
            .map(|t| self.names.get(t).to_string())
    }

    /// The kind first (spec: Traits): `document` for the root,
    /// `element` for every element (its name is the document's
    /// vocabulary, not the adapter's). A `table` element (TEI,
    /// DocBook, HTML in XML) then carries `tabula`, the one trait
    /// a table has in every substrate (spec: The Table):
    /// `//*<tabula>` finds it, `| #` reads it.
    fn traits(&self, node: NodeId) -> Vec<String> {
        let Some(name) = self.name(node) else {
            return vec!["document".to_string()];
        };
        let local = name.rsplit(':').next().unwrap_or("");
        let mut out = vec!["element".to_string()];
        if local == "table" {
            out.push("tabula".to_string());
        }
        out
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes[node.0 as usize].parent
    }

    /// `::text` is the element's text content; any other name is an
    /// attribute value.
    /// Attributes, by name (`::pages`). Element text is the bare
    /// projection (`::`) — no attribute name is shadowed.
    /// XML's own reference syntax: XLink's `xlink:href`, else a
    /// plain `href`. The bare arrows read it.
    fn ref_property(&self, node: NodeId) -> Option<String> {
        let n = &self.nodes[node.0 as usize];
        ["xlink:href", "href"]
            .into_iter()
            .find(|a| n.attr(&self.names, a).is_some())
            .map(str::to_string)
    }

    /// An element's properties are its attributes.
    fn property_names(&self, node: NodeId) -> Vec<String> {
        self.nodes[node.0 as usize]
            .attrs
            .iter()
            .map(|(k, _)| self.names.get(*k).to_string())
            .collect()
    }

    /// Any attribute may hold a reference (`#id`, an IDREF) to any
    /// element of the document: the format declares no narrower one.
    fn can_refer_to(&self, _source: NodeId, _target: NodeId) -> Option<bool> {
        Some(true)
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        self.nodes[node.0 as usize]
            .attr(&self.names, name)
            .map(|v| Value::Str(v.to_string()))
    }

    /// The default projection of an element is its text content.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        Some(Value::Str(self.nodes[node.0 as usize].text.clone()))
    }

    /// `:::tag`, `:::local-name`, `:::ns-prefix`, and `:::n-attrs`
    /// — facts about the element, never its data: attributes are
    /// properties (`::pages`), text is the bare projection.
    /// `:::texts` is the structural reading of the text beside the
    /// bare projection's (ruling 2026-11-11): XPath's `text()`, the
    /// list of the element's immediate text runs, no child
    /// element's text in it; each run with its white space
    /// collapsed and trimmed, null where there is none.
    /// The `:::` keys this adapter answers (`:meta` lists the ones
    /// a node answers).
    fn metadata_catalogue(&self) -> &'static [&'static str] {
        &["tag", "local-name", "ns-prefix", "n-attrs", "texts"]
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        match key {
            "texts" => {
                n.tag?;
                // the element's text less its children's, in order
                let mut runs = Vec::new();
                let mut from = 0;
                for c in &n.children {
                    let c = &self.nodes[c.0 as usize];
                    let at = (c.at as usize).min(n.text.len());
                    runs.extend(collapsed(n.text.get(from..at).unwrap_or("")));
                    from = (at + c.text.len()).min(n.text.len());
                }
                runs.extend(collapsed(n.text.get(from..).unwrap_or("")));
                (!runs.is_empty()).then(|| Value::list(runs.into_iter().map(Value::Str).collect()))
            }
            "tag" => n.tag.map(|t| Value::Str(self.names.get(t).to_string())),
            "local-name" => n
                .tag
                .map(|t| Value::Str(split_qualified(self.names.get(t)).1.to_string())),
            "ns-prefix" => n
                .tag
                .and_then(|t| split_qualified(self.names.get(t)).0)
                .map(|p| Value::Str(p.to_string())),
            "n-attrs" => Some(Value::Int(n.attrs.len() as i64)),
            _ => None,
        }
    }

    /// Follow an attribute that is an ID reference — a bare IDREF
    /// value (`book="b1"`) or a fragment (`href="#b1"`) — to the
    /// element carrying that `id` or `xml:id`.
    fn resolve(&self, node: NodeId, property: &str, _hint: Option<&str>) -> Option<NodeId> {
        let value = self.nodes[node.0 as usize].attr(&self.names, property)?;
        let target = value.strip_prefix('#').unwrap_or(value);
        self.ids.get(target).copied()
    }
}

/// An XML document with one repeated child streamed (hash (d) of the
/// columnar round, 2026-11-06): the document element's children —
/// `<page>` under `<mediawiki>`, `<entry>` under `<dictionary>` — as
/// byte spans found by one pass of the reader, each parsed into its
/// own arbor when a step reaches it, wrapped in the document
/// element's own start tag so its prefixes resolve as in the whole
/// parse; the document element itself, its attributes and the
/// prologue parsed whole as the PREFIX document with the children
/// left out. The node ids pack the child's index above its arbor's
/// node; the prefix's nodes keep their own ids below.
pub struct XmlStream {
    size: u64,
    /// The prologue and the document element, childless.
    prefix: XmlAdapter,
    /// The document element in the prefix: the children's parent.
    anchor: NodeId,
    /// The document element's start and end tags, as written.
    root_start: String,
    root_end: String,
    /// The children's spans and their tags (interned).
    offsets: Vec<(u64, u32)>,
    tags: Vec<u32>,
    tag_names: Vec<String>,
    /// The whitespace text of the document element between its
    /// children, one piece before each child and one after the last.
    gaps: Vec<String>,
    /// `id` / `xml:id` values anywhere in the children → the child.
    ids: HashMap<String, u32>,
    last: std::cell::RefCell<Option<(u32, XmlAdapter)>>,
    reader: std::cell::RefCell<(std::io::BufReader<std::fs::File>, u32)>,
}

const CHILD_SHIFT: u32 = 32;

impl XmlStream {
    /// The file scanned for its document element's children; `None`
    /// where the document is not of that shape (text beside the
    /// children, no children, or children under half the file),
    /// where the whole read is the right one.
    pub fn open(path: &std::path::Path) -> Result<Option<Self>, XmlError> {
        use std::io::Seek;
        let bytes =
            std::fs::read(path).map_err(|e| quick_xml::Error::Io(std::sync::Arc::new(e)))?;
        let size = bytes.len() as u64;
        let mut reader = Reader::from_reader(std::io::Cursor::new(&bytes));
        let decoder = reader.decoder();
        let mut buf = Vec::new();
        let mut depth = 0usize;
        let mut root_span: Option<(usize, usize)> = None; // the start tag's bytes
        let mut root_end_span: Option<(usize, usize)> = None;
        let mut root_tag: Option<String> = None;
        let mut offsets: Vec<(u64, u32)> = Vec::new();
        let mut tags: Vec<u32> = Vec::new();
        let mut tag_names: Vec<String> = Vec::new();
        let mut tag_index: HashMap<String, u32> = HashMap::new();
        let mut gaps: Vec<String> = vec![String::new()];
        let mut ids: HashMap<String, u32> = HashMap::new();
        let mut child_start: usize = 0;
        let mut intern_tag = |name: &str| -> u32 {
            if let Some(&k) = tag_index.get(name) {
                return k;
            }
            let k = tag_names.len() as u32;
            tag_names.push(name.to_string());
            tag_index.insert(name.to_string(), k);
            k
        };
        loop {
            let before = reader.buffer_position() as usize;
            let event = reader.read_event_into(&mut buf)?;
            let after = reader.buffer_position() as usize;
            match event {
                // the document element: its start tag kept; childless
                // (`<root/>`) it has nothing to stream
                Event::Empty(_) if depth == 0 => return Ok(None),
                Event::Start(e) if depth == 0 => {
                    root_tag = Some(
                        decoder
                            .decode(e.name().as_ref())
                            .map_err(quick_xml::Error::from)?
                            .into_owned(),
                    );
                    root_span = Some((before, after));
                    depth = 1;
                }
                // a child of the document element: its span opened, or
                // whole when empty
                Event::Start(e) | Event::Empty(e) if depth == 1 => {
                    let empty = bytes[before..after].ends_with(b"/>");
                    child_start = before;
                    let name = decoder
                        .decode(e.name().as_ref())
                        .map_err(quick_xml::Error::from)?
                        .into_owned();
                    let k = intern_tag(&name);
                    let i = offsets.len() as u32;
                    for attr in e.attributes() {
                        let attr = attr.map_err(quick_xml::Error::from)?;
                        let key = decoder
                            .decode(attr.key.as_ref())
                            .map_err(quick_xml::Error::from)?;
                        if key == "id" || key == "xml:id" {
                            let v = attr.decode_and_unescape_value(decoder)?.into_owned();
                            ids.entry(v).or_insert(i);
                        }
                    }
                    tags.push(k);
                    if empty {
                        offsets.push((before as u64, (after - before) as u32));
                        gaps.push(String::new());
                    } else {
                        depth = 2;
                        offsets.push((before as u64, 0));
                    }
                }
                Event::Start(e) => {
                    for attr in e.attributes() {
                        let attr = attr.map_err(quick_xml::Error::from)?;
                        let key = decoder
                            .decode(attr.key.as_ref())
                            .map_err(quick_xml::Error::from)?;
                        if key == "id" || key == "xml:id" {
                            let v = attr.decode_and_unescape_value(decoder)?.into_owned();
                            ids.entry(v).or_insert(offsets.len() as u32 - 1);
                        }
                    }
                    depth += 1;
                }
                Event::Empty(e) => {
                    for attr in e.attributes() {
                        let attr = attr.map_err(quick_xml::Error::from)?;
                        let key = decoder
                            .decode(attr.key.as_ref())
                            .map_err(quick_xml::Error::from)?;
                        if key == "id" || key == "xml:id" {
                            let v = attr.decode_and_unescape_value(decoder)?.into_owned();
                            ids.entry(v).or_insert(offsets.len() as u32 - 1);
                        }
                    }
                }
                Event::End(_) => {
                    if depth == 2 {
                        let last = offsets.last_mut().expect("an open child");
                        last.1 = (after - child_start) as u32;
                        gaps.push(String::new());
                        depth = 1;
                    } else if depth == 1 {
                        root_end_span = Some((before, after));
                        depth = 0;
                    } else {
                        depth -= 1;
                    }
                }
                Event::Text(e) if depth == 1 => {
                    let text = e.xml_content().map_err(quick_xml::Error::from)?;
                    if !text.trim().is_empty() {
                        // text beside the children: the document is no stream
                        return Ok(None);
                    }
                    gaps.last_mut().expect("a gap").push_str(&text);
                }
                Event::CData(_) | Event::GeneralRef(_) if depth == 1 => return Ok(None),
                Event::Eof => break,
                _ => {}
            }
            buf.clear();
        }
        let (Some((rs, re)), Some((es, ee)), Some(_)) = (root_span, root_end_span, root_tag) else {
            return Ok(None);
        };
        if offsets.len() < 2 {
            return Ok(None);
        }
        let covered: u64 = offsets.iter().map(|&(_, l)| l as u64).sum();
        if covered * 2 < size {
            return Ok(None);
        }
        let root_start = String::from_utf8_lossy(&bytes[rs..re]).into_owned();
        let root_end = String::from_utf8_lossy(&bytes[es..ee]).into_owned();
        // the prefix: the prologue, the document element, childless
        let mut prefix_text = String::from_utf8_lossy(&bytes[..rs]).into_owned();
        prefix_text.push_str(&root_start);
        if !root_start.ends_with("/>") {
            prefix_text.push_str(&root_end);
        }
        let prefix = XmlAdapter::parse(&prefix_text)?;
        let anchor = *prefix
            .children(prefix.root())
            .first()
            .ok_or(XmlError::NoRoot)?;
        let mut file = std::io::BufReader::with_capacity(
            1 << 16,
            std::fs::File::open(path).map_err(|e| quick_xml::Error::Io(std::sync::Arc::new(e)))?,
        );
        file.seek(std::io::SeekFrom::Start(0))
            .map_err(|e| quick_xml::Error::Io(std::sync::Arc::new(e)))?;
        Ok(Some(XmlStream {
            size,
            prefix,
            anchor,
            root_start,
            root_end,
            offsets,
            tags,
            tag_names,
            gaps,
            ids,
            last: std::cell::RefCell::new(None),
            reader: std::cell::RefCell::new((file, u32::MAX)),
        }))
    }

    pub fn n_children(&self) -> usize {
        self.offsets.len()
    }

    fn pack(i: u32, local: NodeId) -> NodeId {
        NodeId(((i as u64 + 1) << CHILD_SHIFT) | local.0)
    }

    fn unpack(node: NodeId) -> Option<(u32, NodeId)> {
        let i = (node.0 >> CHILD_SHIFT) as u32;
        (i > 0).then(|| (i - 1, NodeId(node.0 & ((1u64 << CHILD_SHIFT) - 1))))
    }

    /// The child's own node in its arbor: the document element's
    /// first child there.
    const LOCAL_ROOT: NodeId = NodeId(2);

    /// Child `i` parsed (or the one last parsed) in the document
    /// element's wrapping, and `f` on its arbor.
    fn with_child<T>(&self, i: u32, f: impl FnOnce(&XmlAdapter) -> T) -> Option<T> {
        {
            let last = self.last.borrow();
            if let Some((k, a)) = &*last
                && *k == i
            {
                return Some(f(a));
            }
        }
        use std::io::{Read, Seek};
        let &(offset, len) = self.offsets.get(i as usize)?;
        let text = {
            let mut r = self.reader.borrow_mut();
            if r.1 != i {
                r.0.seek(std::io::SeekFrom::Start(offset)).ok()?;
            } else {
                let here = r.0.stream_position().ok()?;
                if here != offset {
                    r.0.seek(std::io::SeekFrom::Start(offset)).ok()?;
                }
            }
            let mut buf = vec![0u8; len as usize];
            r.0.read_exact(&mut buf).ok()?;
            r.1 = i + 1;
            if let Some(&(next, _)) = self.offsets.get(i as usize + 1) {
                let gap = next - (offset + len as u64);
                if gap > 0 && gap < 1 << 16 {
                    let mut skip = vec![0u8; gap as usize];
                    r.0.read_exact(&mut skip).ok()?;
                }
            }
            String::from_utf8(buf).ok()?
        };
        let wrapped = format!("{}{}{}", self.root_start, text, self.root_end);
        let a = XmlAdapter::parse(&wrapped).ok()?;
        let out = f(&a);
        *self.last.borrow_mut() = Some((i, a));
        Some(out)
    }

    /// The children as the plan reaches them: a child step, nothing
    /// tested but the name.
    fn children_plan(&self, from: NodeId, plan: &quarb::StepPlan<'_>) -> bool {
        from == self.anchor
            && plan.conjuncts.is_empty()
            && plan.traits.is_empty()
            && matches!(plan.axis, quarb::PlanAxis::Child)
    }

    fn matching(&self, name: Option<&str>) -> impl Iterator<Item = u32> + '_ {
        let k = name
            .and_then(|n| self.tag_names.iter().position(|t| t == n))
            .map(|k| k as u32);
        let named = name.is_some();
        (0..self.offsets.len() as u32).filter(move |&i| !named || Some(self.tags[i as usize]) == k)
    }

    /// The node's locator, as the whole read spells it: a child among
    /// same-tag siblings numbered when the tag repeats.
    pub fn locator(&self, node: NodeId) -> String {
        match Self::unpack(node) {
            None => self.prefix.locator(node),
            Some((i, local)) => {
                let base = self.prefix.locator(self.anchor);
                let tag = self.tags[i as usize];
                let repeats = self.tags.iter().filter(|&&t| t == tag).count() > 1;
                let nth = self.tags[..i as usize]
                    .iter()
                    .filter(|&&t| t == tag)
                    .count()
                    + 1;
                let segment = if repeats {
                    format!("{}[{nth}]", self.tag_names[tag as usize])
                } else {
                    self.tag_names[tag as usize].clone()
                };
                let inner = self.with_child(i, |a| a.locator(local)).unwrap_or_default();
                // the arbor's locator: /root/child[/…]: the first two
                // segments are the wrapping and the child itself
                let rest: Vec<&str> = inner.trim_start_matches('/').splitn(3, '/').collect();
                let tail = rest.get(2).map(|r| format!("/{r}")).unwrap_or_default();
                format!("{base}/{segment}{tail}")
            }
        }
    }
}

impl AstAdapter for XmlStream {
    fn estimate(&self, _scope: &[&str], plan: &quarb::StepPlan<'_>) -> Option<quarb::Estimate> {
        let n = self.matching(plan.name).count() as u64;
        let nodes = match plan.limit {
            Some(k) if self.children_plan(self.anchor, plan) => (n.min(k as u64), n.min(k as u64)),
            _ => (n, n),
        };
        Some(quarb::Estimate {
            nodes,
            bytes: Some((self.size, self.size)),
            requests: None,
            latency: quarb::Latency::Local,
            confidence: quarb::Confidence::Bound,
        })
    }

    fn root(&self) -> NodeId {
        self.prefix.root()
    }

    fn walks_for_references(&self) -> bool {
        true
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        match Self::unpack(node) {
            Some((i, local)) => self
                .with_child(i, |a| {
                    a.children(local)
                        .into_iter()
                        .map(|c| Self::pack(i, c))
                        .collect()
                })
                .unwrap_or_default(),
            None if node == self.anchor => (0..self.offsets.len() as u32)
                .map(|i| Self::pack(i, Self::LOCAL_ROOT))
                .collect(),
            None => self.prefix.children(node),
        }
    }

    fn count(&self, from: NodeId, plan: &quarb::StepPlan<'_>) -> Option<usize> {
        self.children_plan(from, plan)
            .then(|| self.matching(plan.name).count())
    }

    fn select(&self, from: NodeId, plan: &quarb::StepPlan<'_>) -> Option<quarb::Selection> {
        let k = plan.limit?;
        if !self.children_plan(from, plan) {
            return None;
        }
        Some(quarb::Selection {
            nodes: self
                .matching(plan.name)
                .take(k)
                .map(|i| (Self::pack(i, Self::LOCAL_ROOT), 1))
                .collect(),
            applied: Vec::new(),
        })
    }

    fn name(&self, node: NodeId) -> Option<String> {
        match Self::unpack(node) {
            Some((i, local)) if local == Self::LOCAL_ROOT => {
                Some(self.tag_names[self.tags[i as usize] as usize].clone())
            }
            Some((i, local)) => self.with_child(i, |a| a.name(local)).flatten(),
            None => self.prefix.name(node),
        }
    }

    fn traits(&self, node: NodeId) -> Vec<String> {
        match Self::unpack(node) {
            Some((i, local)) => self.with_child(i, |a| a.traits(local)).unwrap_or_default(),
            None => self.prefix.traits(node),
        }
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        match Self::unpack(node) {
            Some((_, local)) if local == Self::LOCAL_ROOT => Some(self.anchor),
            Some((i, local)) => self
                .with_child(i, |a| a.parent(local).map(|p| Self::pack(i, p)))
                .flatten(),
            None => self.prefix.parent(node),
        }
    }

    fn ref_property(&self, node: NodeId) -> Option<String> {
        match Self::unpack(node) {
            Some((i, local)) => self.with_child(i, |a| a.ref_property(local)).flatten(),
            None => self.prefix.ref_property(node),
        }
    }

    fn property_names(&self, node: NodeId) -> Vec<String> {
        match Self::unpack(node) {
            Some((i, local)) => self
                .with_child(i, |a| a.property_names(local))
                .unwrap_or_default(),
            None => self.prefix.property_names(node),
        }
    }

    fn can_refer_to(&self, _source: NodeId, _target: NodeId) -> Option<bool> {
        Some(true)
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        match Self::unpack(node) {
            Some((i, local)) => self.with_child(i, |a| a.property(local, name)).flatten(),
            None => self.prefix.property(node, name),
        }
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        match Self::unpack(node) {
            Some((i, local)) => self.with_child(i, |a| a.default_value(local)).flatten(),
            // the document element's text is its children's, in order,
            // with its own whitespace between; the document node's too,
            // as the text flows up in the whole read
            None if node == self.anchor || node == self.prefix.root() => {
                let mut text = String::new();
                for i in 0..self.offsets.len() as u32 {
                    text.push_str(&self.gaps[i as usize]);
                    if let Some(Some(Value::Str(t))) =
                        self.with_child(i, |a| a.default_value(Self::LOCAL_ROOT))
                    {
                        text.push_str(&t);
                    }
                }
                if let Some(tail) = self.gaps.get(self.offsets.len()) {
                    text.push_str(tail);
                }
                Some(Value::Str(text))
            }
            None => self.prefix.default_value(node),
        }
    }

    fn metadata_catalogue(&self) -> &'static [&'static str] {
        &["tag", "local-name", "ns-prefix", "n-attrs", "texts"]
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match Self::unpack(node) {
            Some((i, local)) => self.with_child(i, |a| a.metadata(local, key)).flatten(),
            // the document element's own text is what stands between
            // its children
            None if key == "texts" && node == self.anchor => {
                let runs: Vec<Value> = self
                    .gaps
                    .iter()
                    .filter_map(|g| collapsed(g))
                    .map(Value::Str)
                    .collect();
                (!runs.is_empty()).then(|| Value::list(runs))
            }
            None => self.prefix.metadata(node, key),
        }
    }

    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let value = self.property(node, property)?;
        let Value::Str(s) = value else {
            return None;
        };
        let target = s.strip_prefix('#').unwrap_or(&s).to_string();
        // in the same child first, as the whole read finds it
        if let Some((i, local)) = Self::unpack(node)
            && let Some(Some(r)) = self.with_child(i, |a| a.resolve(local, property, hint))
        {
            return Some(Self::pack(i, r));
        }
        let j = *self.ids.get(&target)?;
        self.with_child(j, |a| a.node_by_id(&target).map(|r| Self::pack(j, r)))
            .flatten()
    }
}

#[cfg(test)]
mod declared_tests {
    #[test]
    fn prologue_declaration() {
        assert_eq!(
            super::declared_encoding(b"<?xml version='1.0' encoding = 'Windows-1251' ?><a/>"),
            Some("Windows-1251".into())
        );
        assert_eq!(
            super::declared_encoding(b"<?xml version=\"1.0\"?><a/>"),
            None
        );
        assert_eq!(super::declared_encoding(b"<a encoding=\"x\"/>"), None);
        assert_eq!(
            super::declared_encoding(b"<?xml-stylesheet href=\"a.xsl\" encoding=\"koi8-r\"?><r/>"),
            None
        );
        assert_eq!(
            super::declared_encoding(b"\xef\xbb\xbf<?xml encoding=\"utf-8\"?>"),
            None
        );
    }
}

#[cfg(test)]
mod stream_tests {
    //! The document with one repeated child streamed reads as the
    //! whole file does, node for node.
    use super::*;

    const DOC: &str = "<?xml version=\"1.0\"?>\n<dictionary xmlns:x=\"urn:x\" lang=\"grc\">\n  <entry id=\"e1\" x:n=\"1\"><form>λόγος</form><sense>word</sense><see ref=\"e3\"/></entry>\n  <entry id=\"e2\"><form>μῆνις</form><sense n=\"a\">wrath &amp; rage</sense></entry>\n  <note id=\"n1\">a note</note>\n  <entry id=\"e3\"><form>ἀοιδός</form><sense>singer</sense><see ref=\"#e1\"/></entry>\n</dictionary>\n";

    fn file(name: &str, text: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("quarb-xml-stream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    fn walk(s: &XmlStream, sn: NodeId, w: &XmlAdapter, wn: NodeId, depth: usize) {
        assert!(depth < 10);
        assert_eq!(s.name(sn), w.name(wn), "name at {}", w.locator(wn));
        assert_eq!(s.traits(sn), w.traits(wn), "traits at {}", w.locator(wn));
        assert_eq!(
            s.property_names(sn),
            w.property_names(wn),
            "attrs at {}",
            w.locator(wn)
        );
        for a in w.property_names(wn) {
            assert_eq!(
                s.property(sn, &a),
                w.property(wn, &a),
                "{a} at {}",
                w.locator(wn)
            );
        }
        assert_eq!(
            s.default_value(sn),
            w.default_value(wn),
            "text at {}",
            w.locator(wn)
        );
        assert_eq!(s.locator(sn), w.locator(wn), "locator");
        for k in ["tag", "local-name", "ns-prefix", "n-attrs", "texts"] {
            assert_eq!(
                s.metadata(sn, k),
                w.metadata(wn, k),
                "{k} at {}",
                w.locator(wn)
            );
        }
        let sc = s.children(sn);
        let wc = w.children(wn);
        assert_eq!(sc.len(), wc.len(), "children at {}", w.locator(wn));
        for (a, b) in sc.iter().zip(wc.iter()) {
            assert_eq!(s.parent(*a), Some(sn), "parent at {}", w.locator(*b));
            walk(s, *a, w, *b, depth + 1);
        }
    }

    #[test]
    fn the_children_read_as_the_whole_file_does() {
        let p = file("dict.xml", DOC);
        let s = XmlStream::open(&p)
            .unwrap()
            .expect("a document worth streaming");
        assert_eq!(s.n_children(), 4);
        let w = XmlAdapter::parse(DOC).unwrap();
        walk(&s, s.root(), &w, w.root(), 0);
        // the plan hooks, by name and bare
        let dict = s.children(s.root())[0];
        let mut plan = quarb::StepPlan {
            traits: Vec::new(),
            cap: quarb::Cap {
                nodes: None,
                bytes: None,
            },
            axis: quarb::PlanAxis::Child,
            name: Some("entry"),
            conjuncts: Vec::new(),
            limit: Some(2),
            order: None,
        };
        assert_eq!(s.count(dict, &plan), Some(3));
        assert_eq!(s.select(dict, &plan).map(|sel| sel.nodes.len()), Some(2));
        plan.name = None;
        assert_eq!(s.count(dict, &plan), Some(4));
        // a reference across children, bare and with the fragment sigil
        let e1 = s.children(dict)[0];
        let see = s.children(e1)[2];
        let target = s.resolve(see, "ref", None).expect("resolves");
        assert_eq!(s.property(target, "id"), Some(Value::Str("e3".into())));
        let e3 = s.children(dict)[3];
        let back = s.resolve(s.children(e3)[2], "ref", None).expect("resolves");
        assert_eq!(s.property(back, "id"), Some(Value::Str("e1".into())));
    }

    #[test]
    fn a_document_with_text_beside_its_children_reads_whole() {
        let p = file("mixed.xml", "<r>text <a>1</a> more <a>2</a></r>");
        assert!(XmlStream::open(&p).unwrap().is_none());
        let p = file(
            "one.xml",
            "<r><a>only one child, under half the file by its text? no, over</a></r>",
        );
        assert!(XmlStream::open(&p).unwrap().is_none());
    }
}
