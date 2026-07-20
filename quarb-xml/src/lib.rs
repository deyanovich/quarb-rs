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
//!   (`//'dc:title'`); `;;;local-name` and `;;;ns-prefix` metadata
//!   allow prefix-agnostic filtering (`//*[;;;local-name =
//!   "title"]`). The document root is unnamed; the document
//!   element is its single child.
//! - Nodes have no traits: XML has no universal structural
//!   vocabulary, and leaf/container distinctions are already
//!   available as core metadata (`:::is-leaf`, `:::n-children`).
//! - Attributes are properties: `::pages`, `::href`. The default
//!   projection (`::`) and `::text` are the element's text
//!   content; `;;;tag`, `;;;id`, `;;;local-name`, `;;;ns-prefix`,
//!   `;;;n-attrs`, and any `;;;attr` expose metadata.
//! - An attribute holding an ID reference resolves: `::ref~>`
//!   follows a bare IDREF value (`book="b1"`) or a fragment
//!   (`href="#b1"`) to the element with that `id` or `xml:id`.
//! - Input is UTF-8 text; an `encoding` declaration in the XML
//!   prologue is ignored. Only the five predefined entities and
//!   numeric character references are resolved.

use quarb::{AstAdapter, NodeId, Value};
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Decoder, Reader};
use std::collections::HashMap;

struct Node {
    /// The tag name as written; `None` for the document root.
    tag: Option<String>,
    attrs: Vec<(String, String)>,
    /// The element's text content (all descendant text and CDATA,
    /// concatenated in document order).
    text: String,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
}

impl Node {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
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

/// A Quarb adapter over a parsed XML document.
pub struct XmlAdapter {
    nodes: Vec<Node>,
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

        let mut reader = Reader::from_str(xml);
        let decoder = reader.decoder();
        // Indices of the currently open elements; the synthetic
        // root stays at the bottom.
        let mut stack: Vec<usize> = vec![0];

        loop {
            match reader.read_event()? {
                Event::Start(e) => {
                    let idx = intern(&mut nodes, &mut ids, &stack, decoder, &e)?;
                    stack.push(idx);
                }
                Event::Empty(e) => {
                    intern(&mut nodes, &mut ids, &stack, decoder, &e)?;
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
            let tag = nodes[open].tag.clone().expect("open elements are tagged");
            return Err(XmlError::Unclosed(tag));
        }
        match nodes[0].children.len() {
            0 => return Err(XmlError::NoRoot),
            1 => {}
            _ => return Err(XmlError::TrailingContent),
        }
        // The synthetic root's text is the document element's, not
        // any stray whitespace around it.
        nodes[0].text = nodes[nodes[0].children[0].0 as usize].text.clone();

        Ok(XmlAdapter { nodes, ids, root })
    }

    /// A locator path to `node`, like `/library/book[2]/author`, for
    /// rendering. A `[n]` index is added only to disambiguate
    /// same-tag siblings.
    pub fn locator(&self, node: NodeId) -> String {
        let mut segments = Vec::new();
        let mut cur = Some(node);
        while let Some(id) = cur {
            let n = &self.nodes[id.0 as usize];
            if let Some(tag) = &n.tag {
                segments.push(self.segment(id, tag));
            }
            cur = n.parent;
        }
        segments.reverse();
        format!("/{}", segments.join("/"))
    }

    fn segment(&self, node: NodeId, tag: &str) -> String {
        let Some(parent) = self.nodes[node.0 as usize].parent else {
            return tag.to_string();
        };
        let siblings = &self.nodes[parent.0 as usize].children;
        let same_tag: Vec<NodeId> = siblings
            .iter()
            .copied()
            .filter(|&s| self.nodes[s.0 as usize].tag.as_deref() == Some(tag))
            .collect();
        if same_tag.len() > 1 {
            let n = same_tag.iter().position(|&s| s == node).unwrap() + 1;
            format!("{tag}[{n}]")
        } else {
            tag.to_string()
        }
    }
}

/// Intern the element behind a `Start`/`Empty` event as a child of
/// the innermost open element, returning its arena index.
fn intern(
    nodes: &mut Vec<Node>,
    ids: &mut HashMap<String, NodeId>,
    stack: &[usize],
    decoder: Decoder,
    e: &BytesStart,
) -> Result<usize, XmlError> {
    let idx = nodes.len();
    let this = NodeId(idx as u64);
    let tag = decoder
        .decode(e.name().as_ref())
        .map_err(quick_xml::Error::from)?
        .into_owned();
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
        attrs.push((key, value));
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
    fn root(&self) -> NodeId {
        self.root
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.nodes[node.0 as usize].children.clone()
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.nodes[node.0 as usize].tag.clone()
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes[node.0 as usize].parent
    }

    /// `::text` is the element's text content; any other name is an
    /// attribute value.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        if name == "text" {
            Some(Value::Str(n.text.clone()))
        } else {
            n.attr(name).map(|v| Value::Str(v.to_string()))
        }
    }

    /// The default projection of an element is its text content.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        Some(Value::Str(self.nodes[node.0 as usize].text.clone()))
    }

    /// `;;;tag`, `;;;text`, `;;;id`, `;;;local-name`, `;;;ns-prefix`,
    /// `;;;n-attrs`, and any attribute by name (`;;;pages`).
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        match key {
            "tag" => n.tag.clone().map(Value::Str),
            "text" => Some(Value::Str(n.text.clone())),
            "id" => n
                .attr("id")
                .or_else(|| n.attr("xml:id"))
                .map(|v| Value::Str(v.to_string())),
            "local-name" => n
                .tag
                .as_deref()
                .map(|t| Value::Str(split_qualified(t).1.to_string())),
            "ns-prefix" => n
                .tag
                .as_deref()
                .and_then(|t| split_qualified(t).0)
                .map(|p| Value::Str(p.to_string())),
            "n-attrs" => Some(Value::Int(n.attrs.len() as i64)),
            other => n.attr(other).map(|v| Value::Str(v.to_string())),
        }
    }

    /// Follow an attribute that is an ID reference — a bare IDREF
    /// value (`book="b1"`) or a fragment (`href="#b1"`) — to the
    /// element carrying that `id` or `xml:id`.
    fn resolve(&self, node: NodeId, property: &str, _hint: Option<&str>) -> Option<NodeId> {
        let value = self.nodes[node.0 as usize].attr(property)?;
        let target = value.strip_prefix('#').unwrap_or(value);
        self.ids.get(target).copied()
    }
}
