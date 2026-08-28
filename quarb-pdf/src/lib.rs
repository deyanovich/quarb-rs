//! The PDF object-graph adapter: the document's *own* structure —
//! indirect objects as nodes, dictionary entries as properties and
//! children, and every indirect reference as a crosslink edge.
//!
//! This is the internals reading of the two-level split (the
//! document-adapter row in lang/TODO.md): plain `file.pdf` opens
//! here, the way plain `page.html` opens as the DOM, and
//! `text:file.pdf` opts into the reader's model
//! (`quarb-text-pdf`). A PDF is a graph of objects joined by
//! references — trailer → catalog → page tree → resources — which
//! is quintessential arbor material: the containment spine holds
//! the *direct* (inline) structure, and the references are `->`
//! edges, cycle-safe by construction (`->Parent` climbs,
//! `<-Parent` finds the children pages, `->Kids` fans out).
//!
//! - `/trailer` — the trailer dictionary (`->Root`, `->Info`).
//! - `/objects/*` — every indirect object, named by its declared
//!   `/Type` (lowercased: `catalog`, `pages`, `page`, `font`,
//!   `annot`, …) or `object` when it declares none; `::id` is
//!   "num gen".
//! - Scalar entries are properties (names and strings decoded —
//!   PDFDocEncoding or UTF-16BE per the spec); direct dictionaries
//!   and arrays are children named by their key (array elements
//!   are `item`); references are edges labeled by their key — a
//!   reference inside a direct array edges from the array's
//!   owner under the array's key, so `/objects/pages->Kids`
//!   fans out to the pages exactly as the PDF means it.
//! - A stream object carries `::::stream-length` (the raw length)
//!   and its dictionary like any other.
//!
//! The audience is inspection and forensics: `//font`,
//! `/objects/*[->JS]`, `//page->Contents`, `//object[::id = "3 0"]`
//! — the questions a PDF's surface never answers.

use std::collections::HashMap;

use lopdf::{Dictionary, Object};
use quarb::{AstAdapter, NodeId, Value};

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("not a PDF: {0}")]
    Parse(#[from] lopdf::Error),
}

struct Node {
    name: Option<String>,
    parent: Option<usize>,
    children: Vec<usize>,
    props: Vec<(String, Value)>,
    meta: Vec<(String, Value)>,
    value: Option<Value>,
    /// Outgoing reference edges: (key label, target node).
    links: Vec<(String, usize)>,
}

pub struct PdfAdapter {
    nodes: Vec<Node>,
    /// Incoming edges, inverted from `links` at load.
    back: Vec<Vec<(String, usize)>>,
}

impl PdfAdapter {
    pub fn load(bytes: &[u8]) -> Result<Self, PdfError> {
        let doc = lopdf::Document::load_mem(bytes)?;
        let mut a = PdfAdapter { nodes: Vec::new(), back: Vec::new() };
        let root = a.push(None, None);
        // One node per indirect object, allocated up front so
        // references can link forward.
        let mut by_id: HashMap<lopdf::ObjectId, usize> = HashMap::new();
        let trailer = a.push(Some("trailer".into()), Some(root));
        let objects = a.push(Some("objects".into()), Some(root));
        a.nodes[root].children = vec![trailer, objects];
        for id in doc.objects.keys() {
            let n = a.push(Some("object".into()), Some(objects));
            a.nodes[objects].children.push(n);
            by_id.insert(*id, n);
        }
        for (id, obj) in &doc.objects {
            let n = by_id[id];
            a.nodes[n].props.push(("id".into(), Value::Str(format!("{} {}", id.0, id.1))));
            match obj {
                Object::Dictionary(d) => {
                    if let Some(t) = type_name(d) {
                        a.nodes[n].name = Some(t);
                    }
                    a.fill_dict(n, d, &by_id);
                }
                Object::Stream(s) => {
                    if let Some(t) = type_name(&s.dict) {
                        a.nodes[n].name = Some(t);
                    }
                    a.nodes[n]
                        .meta
                        .push(("stream-length".into(), Value::Int(s.content.len() as i64)));
                    let dict = s.dict.clone();
                    a.fill_dict(n, &dict, &by_id);
                }
                Object::Array(items) => {
                    let items = items.clone();
                    a.fill_array(n, &items, &by_id, n, "item");
                }
                other => a.nodes[n].value = scalar(other),
            }
        }
        a.fill_dict(trailer, &doc.trailer.clone(), &by_id);
        a.back = vec![Vec::new(); a.nodes.len()];
        for (i, node) in a.nodes.iter().enumerate() {
            for (label, target) in &node.links {
                a.back[*target].push((label.clone(), i));
            }
        }
        Ok(a)
    }

    fn push(&mut self, name: Option<String>, parent: Option<usize>) -> usize {
        self.nodes.push(Node {
            name,
            parent,
            children: Vec::new(),
            props: Vec::new(),
            meta: Vec::new(),
            value: None,
            links: Vec::new(),
        });
        self.nodes.len() - 1
    }

    fn fill_dict(&mut self, n: usize, d: &Dictionary, by_id: &HashMap<lopdf::ObjectId, usize>) {
        for (k, v) in d.iter() {
            let key = String::from_utf8_lossy(k).to_string();
            match v {
                Object::Reference(id) => {
                    if let Some(t) = by_id.get(id) {
                        self.nodes[n].links.push((key, *t));
                    }
                }
                Object::Dictionary(inner) => {
                    let c = self.push(Some(key), Some(n));
                    self.nodes[n].children.push(c);
                    self.fill_dict(c, &inner.clone(), by_id);
                }
                Object::Array(items) => {
                    let c = self.push(Some(key.clone()), Some(n));
                    self.nodes[n].children.push(c);
                    self.fill_array(c, &items.clone(), by_id, n, &key);
                }
                other => {
                    if let Some(val) = scalar(other) {
                        self.nodes[n].props.push((key, val));
                    }
                }
            }
        }
    }

    /// `link_node`/`link_label`: where a reference found in this
    /// array attaches as an edge — the array's owner, under the
    /// array's key.
    fn fill_array(
        &mut self,
        n: usize,
        items: &[Object],
        by_id: &HashMap<lopdf::ObjectId, usize>,
        link_node: usize,
        link_label: &str,
    ) {
        for item in items {
            match item {
                Object::Reference(id) => {
                    if let Some(t) = by_id.get(id) {
                        self.nodes[link_node].links.push((link_label.to_string(), *t));
                    }
                }
                Object::Dictionary(inner) => {
                    let c = self.push(Some("item".into()), Some(n));
                    self.nodes[n].children.push(c);
                    self.fill_dict(c, &inner.clone(), by_id);
                }
                Object::Array(inner) => {
                    let c = self.push(Some("item".into()), Some(n));
                    self.nodes[n].children.push(c);
                    self.fill_array(c, &inner.clone(), by_id, link_node, link_label);
                }
                other => {
                    if let Some(val) = scalar(other) {
                        let c = self.push(Some("item".into()), Some(n));
                        self.nodes[n].children.push(c);
                        self.nodes[c].value = Some(val);
                    }
                }
            }
        }
    }

    pub fn locator(&self, node: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(node.0 as usize);
        while let Some(i) = cur {
            if let Some(name) = &self.nodes[i].name {
                let label = match self.nodes[i].props.iter().find(|(k, _)| k == "id") {
                    Some((_, Value::Str(id))) => format!("{name}({id})"),
                    _ => name.clone(),
                };
                parts.push(label);
            }
            cur = self.nodes[i].parent;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }
}

/// The declared /Type, lowercased, as the node's name.
fn type_name(d: &Dictionary) -> Option<String> {
    match d.get(b"Type") {
        Ok(Object::Name(n)) => Some(String::from_utf8_lossy(n).to_lowercase()),
        _ => None,
    }
}

fn scalar(o: &Object) -> Option<Value> {
    Some(match o {
        Object::Null => Value::Null,
        Object::Boolean(b) => Value::Bool(*b),
        Object::Integer(i) => Value::Int(*i),
        Object::Real(f) => Value::Float(*f as f64),
        Object::Name(n) => Value::Str(String::from_utf8_lossy(n).to_string()),
        Object::String(bytes, _) => Value::Str(text_string(bytes)),
        _ => return None,
    })
}

/// A PDF text string: UTF-16BE behind its BOM, PDFDocEncoding
/// otherwise (the ASCII range is identity; the handful of
/// PDFDoc-specific codes pass through lossily).
pub fn text_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

impl AstAdapter for PdfAdapter {
    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.nodes[node.0 as usize]
            .children
            .iter()
            .map(|&i| NodeId(i as u64))
            .collect()
    }
    fn name(&self, node: NodeId) -> Option<String> {
        self.nodes[node.0 as usize].name.clone()
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes[node.0 as usize].parent.map(|i| NodeId(i as u64))
    }
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        self.nodes[node.0 as usize]
            .props
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }
    fn default_value(&self, node: NodeId) -> Option<Value> {
        self.nodes[node.0 as usize].value.clone()
    }
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        self.nodes[node.0 as usize]
            .meta
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.nodes[node.0 as usize]
            .links
            .iter()
            .map(|(l, t)| (l.clone(), NodeId(*t as u64)))
            .collect()
    }
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.back[node.0 as usize]
            .iter()
            .map(|(l, s)| (l.clone(), NodeId(*s as u64)))
            .collect()
    }
}
