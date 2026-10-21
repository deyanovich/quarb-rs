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
//!   `:::n-attrs`, and any `:::attr` expose metadata.
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

struct Node {
    /// The tag name as written, in the document's name table;
    /// `None` for the document root.
    tag: Option<u32>,
    /// The attributes: the name in the table, the value.
    attrs: Vec<(u32, String)>,
    /// The element's text content (all descendant text and CDATA,
    /// concatenated in document order).
    text: String,
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
    nodes.push(Node {
        tag: Some(tag),
        attrs,
        text: String::new(),
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

    /// A `table` element (TEI, DocBook, HTML in XML) carries
    /// `tabula`, the one trait a table has in every substrate (spec:
    /// The Table): `//*<tabula>` finds it, `| #` reads it.
    fn traits(&self, node: NodeId) -> Vec<String> {
        let local = self
            .name(node)
            .map(|n| n.rsplit(':').next().unwrap_or("").to_string())
            .unwrap_or_default();
        if local == "table" {
            vec!["tabula".to_string()]
        } else {
            Vec::new()
        }
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
    /// The `:::` keys this adapter answers (`:meta` lists the ones
    /// a node answers).
    fn metadata_catalogue(&self) -> &'static [&'static str] {
        &["tag", "local-name", "ns-prefix", "n-attrs"]
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        match key {
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
