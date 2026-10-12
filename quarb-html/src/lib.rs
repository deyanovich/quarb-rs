//! HTML adapter for Quarb.
//!
//! Maps an HTML document onto the arbor model, parsing with
//! html5ever (via `scraper`) so malformed markup is handled per the
//! HTML5 standard.
//!
//! - Elements are the nodes; text and comment nodes are not navigated
//!   (text is exposed as a projection instead).
//! - A node's *name* is its tag (`html`, `body`, `div`, `h1`, `a`),
//!   so `/html/body//p` navigates by element type. The document root
//!   is unnamed; the `<html>` element is its single child.
//! - A node's *traits* are structural classes: `<block>` or
//!   `<inline>`, plus `<heading>` for `h1`–`h6` and `<link>` for an
//!   anchor with an `href`.
//! - Attributes are properties: `::href`, `::class`, `::id`. The
//!   default projection (`::`) is the element's text content;
//!   `::::tag`, `::::classes`, `::::attrs`, and `::::n-attrs`
//!   expose adapter metadata.
//! - An anchor's fragment `href` resolves: `::href-->` follows
//!   `#section` to the element with `id="section"`.

use quarb::names::Names;
use quarb::{AstAdapter, NodeId, Value};
mod sink;
use std::collections::HashMap;

struct Node {
    /// The tag name, in the document's name table; `None` for the
    /// document root.
    tag: Option<u32>,
    /// The attributes: the name in the table, the value.
    attrs: Vec<(u32, String)>,
    /// The element's content in document order: its own text
    /// children as written, its element children by ordinal in
    /// `children`. The subtree text (the bare projection) is
    /// concatenated on first use and kept — storing it for every
    /// element up front cost the depth of the tree per element.
    pieces: Vec<Piece>,
    text: std::cell::OnceCell<String>,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
}

/// One piece of an element's content.
enum Piece {
    Text(String),
    Child(usize),
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

/// The encoding an HTML document declares in its head — a `<meta
/// charset=…>` or a `<meta http-equiv="content-type"
/// content="…; charset=…">` within the first 1024 bytes, as the
/// HTML standard's prescan reads them — reported for the engine's
/// source boundary. The adapter decodes nothing. An XML prologue
/// on an XHTML page counts too (the XML adapter's rule, applied
/// here so a page served as text/html still declares). A
/// `charset=` inside any other attribute's value (a description)
/// declares nothing.
pub fn declared_encoding(bytes: &[u8]) -> Option<String> {
    if let Some(label) = xml_prologue_encoding(bytes) {
        return Some(label);
    }
    let head = &bytes[..bytes.len().min(1024)];
    let lower: Vec<u8> = head.iter().map(|b| b.to_ascii_lowercase()).collect();
    let mut at = 0;
    while let Some(i) = find(&lower[at..], b"<meta") {
        let start = at + i + 5;
        let end = start + find(&lower[start..], b">").unwrap_or(lower.len() - start);
        let attrs = meta_attributes(&lower[start..end]);
        let get = |name: &str| {
            attrs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        };
        if let Some(label) = get("charset").filter(|l| !l.is_empty()) {
            return Some(label.to_string());
        }
        if get("http-equiv") == Some("content-type")
            && let Some(content) = get("content")
            && let Some(p) = content.find("charset=")
        {
            let label = content[p + 8..]
                .trim_matches(|c: char| c == '"' || c == '\'' || c.is_ascii_whitespace())
                .split([';', ' ', '"', '\''])
                .next()
                .unwrap_or("");
            if !label.is_empty() {
                return Some(label.to_string());
            }
        }
        at = end;
    }
    None
}

/// `<?xml … encoding="…"?>` at the very start — the one XML rule
/// the HTML reporter shares, kept local so the crates stay apart.
/// The target must be `xml` itself (`<?xml-stylesheet …?>` is
/// another instruction and declares nothing).
fn xml_prologue_encoding(bytes: &[u8]) -> Option<String> {
    let head = bytes.strip_prefix(b"<?xml")?;
    if !head.first().is_some_and(|b| b.is_ascii_whitespace()) {
        return None;
    }
    let end = find(head, b"?>")?;
    let prologue = std::str::from_utf8(&head[..end]).ok()?;
    let rest = prologue
        .split_once("encoding")?
        .1
        .trim_start()
        .strip_prefix('=')?
        .trim_start();
    let quote = rest.chars().next()?;
    (quote == '"' || quote == '\'')
        .then(|| {
            rest[1..]
                .split(quote)
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .filter(|l| !l.is_empty())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The attributes of a lowercased tag body, as (name, value): a
/// value quoted or bare, a name without one given the empty value.
/// Quoted values are skipped as wholes, so nothing inside one is
/// taken for an attribute.
fn meta_attributes(tag: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    let n = tag.len();
    while i < n {
        while i < n && (tag[i].is_ascii_whitespace() || tag[i] == b'/') {
            i += 1;
        }
        let name_start = i;
        while i < n && !tag[i].is_ascii_whitespace() && tag[i] != b'=' && tag[i] != b'/' {
            i += 1;
        }
        if i == name_start {
            break;
        }
        let name = String::from_utf8_lossy(&tag[name_start..i]).into_owned();
        while i < n && tag[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < n && tag[i] == b'=' {
            i += 1;
            while i < n && tag[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < n && (tag[i] == b'"' || tag[i] == b'\'') {
                let q = tag[i];
                i += 1;
                let v = i;
                while i < n && tag[i] != q {
                    i += 1;
                }
                value = String::from_utf8_lossy(&tag[v..i]).trim().to_string();
                i = (i + 1).min(n);
            } else {
                let v = i;
                while i < n && !tag[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = String::from_utf8_lossy(&tag[v..i]).to_string();
            }
        }
        out.push((name, value));
    }
    out
}

/// A Quarb adapter over a parsed HTML document.
pub struct HtmlAdapter {
    nodes: Vec<Node>,
    /// The document's tag and attribute names, each once.
    names: Names,
    /// `id` attribute value → the element carrying it.
    ids: HashMap<String, NodeId>,
    root: NodeId,
    /// The document's own URL, when the mount knows it — the base
    /// a relative `href` joins against for `::href-->` across
    /// documents. `None` for pasted or fileless html: only
    /// absolute hrefs reference out then.
    document_url: Option<url::Url>,
}

impl HtmlAdapter {
    /// Does `node`'s `rel` admit the relation `hint`? No hint and
    /// the `*` wildcard admit everything; a named hint needs the
    /// space-separated `rel` list to contain it (`rel="nofollow
    /// noopener"` answers to either word). An element without a
    /// `rel` answers only the wildcard forms — the spec's
    /// `-->stylesheet` reads "only the stylesheet relation".
    fn rel_ok(&self, node: NodeId, hint: Option<&str>) -> bool {
        match hint {
            None | Some("*") => true,
            Some(h) => self.nodes[node.0 as usize]
                .attr(&self.names, "rel")
                .is_some_and(|rel| rel.split_whitespace().any(|w| w.eq_ignore_ascii_case(h))),
        }
    }

    /// Declare the document's own URL — the base a relative `href`
    /// joins against when `::href-->` reaches across documents.
    pub fn set_document_url(&mut self, url: &str) {
        self.document_url = url::Url::parse(url).ok();
    }

    /// The subtree text of `node` — every descendant text node in
    /// document order — concatenated on first use and kept. An
    /// explicit stack, so a document nested twenty thousand
    /// elements deep does not recurse.
    fn text_of(&self, node: NodeId) -> &str {
        self.nodes[node.0 as usize].text.get_or_init(|| {
            let mut out = String::new();
            // (element, next piece, where its text starts in `out`):
            // an element whose pieces are spent keeps its own text,
            // so a filter over every element of a deep tree reads
            // each subtree once.
            let mut stack: Vec<(usize, usize, usize)> = vec![(node.0 as usize, 0, 0)];
            while let Some(top) = stack.last_mut() {
                let (cur, at, start) = *top;
                let n = &self.nodes[cur];
                if at >= n.pieces.len() {
                    if cur != node.0 as usize {
                        let _ = n.text.set(out[start..].to_string());
                    }
                    stack.pop();
                    continue;
                }
                top.1 += 1;
                match &n.pieces[at] {
                    Piece::Text(t) => out.push_str(t),
                    Piece::Child(k) => {
                        let c = n.children[*k].0 as usize;
                        match self.nodes[c].text.get() {
                            Some(t) => out.push_str(t),
                            None => stack.push((c, 0, out.len())),
                        }
                    }
                }
            }
            out
        })
    }

    /// Parse `html` and build the adapter.
    pub fn parse(html: &str) -> Self {
        use html5ever::tendril::TendrilSink;
        let sink =
            html5ever::driver::parse_document(sink::Sink::new(), Default::default()).one(html);
        let mut nodes = vec![Node {
            tag: None,
            attrs: Vec::new(),
            pieces: vec![Piece::Child(0)],
            text: std::cell::OnceCell::new(),
            parent: None,
            children: Vec::new(),
        }];
        let mut ids = HashMap::new();
        let root = NodeId(0);
        let sink::Sink {
            nodes: mut sunk,
            mut names,
        } = sink;
        // The document element: the first element child of the
        // sink's document.
        let html_el = sunk[0].kids.iter().find_map(|k| match k {
            sink::Kid::Node(n) if !sunk[*n].skip => Some(*n),
            _ => None,
        });
        if let Some(html_el) = html_el {
            let html_node = build(
                html_el,
                Some(root),
                &mut sunk,
                &mut nodes,
                &mut names,
                &mut ids,
            );
            nodes[0].children = vec![html_node];
        }

        HtmlAdapter {
            nodes,
            names,
            ids,
            root,
            document_url: None,
        }
    }

    /// A locator path to `node`, like `/html/body/div[2]/p`, for
    /// rendering. A `[n]` index is added only to disambiguate same-tag
    /// siblings.
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

/// Intern the sink's element `el` and its subtree into the arena:
/// text pieces and element children in order, the strings moved
/// out of the sink. Iterative (an explicit work stack) so deep
/// markup cannot overflow the call stack; elements are interned in
/// document (pre-order) order, so node ids, child ordering and the
/// first-occurrence `ids` map follow the document.
fn build(
    el: usize,
    parent: Option<NodeId>,
    sunk: &mut [sink::SinkNode],
    nodes: &mut Vec<Node>,
    names: &mut Names,
    ids: &mut HashMap<String, NodeId>,
) -> NodeId {
    let root_this = NodeId(nodes.len() as u64);
    let mut stack = vec![(el, parent)];
    while let Some((el, parent)) = stack.pop() {
        let this = NodeId(nodes.len() as u64);
        let s = &mut sunk[el];
        let tag = names.intern(&s.name.as_ref().expect("an element").local);
        let attrs = std::mem::take(&mut s.attrs);
        let kids = std::mem::take(&mut s.kids);
        let mut pieces = Vec::new();
        let mut children: Vec<usize> = Vec::new();
        let mut ordinal = 0;
        for k in kids {
            match k {
                sink::Kid::Text(t) => pieces.push(Piece::Text(t)),
                sink::Kid::Node(n) => {
                    if sunk[n].skip {
                        continue;
                    }
                    pieces.push(Piece::Child(ordinal));
                    ordinal += 1;
                    children.push(n);
                }
            }
        }
        if let Some(k) = names.key("id")
            && let Some((_, id)) = attrs.iter().find(|(a, _)| *a == k)
        {
            ids.entry(id.clone()).or_insert(this);
        }
        nodes.push(Node {
            tag: Some(tag),
            attrs,
            pieces,
            text: std::cell::OnceCell::new(),
            parent,
            children: Vec::new(),
        });
        if let Some(p) = parent {
            nodes[p.0 as usize].children.push(this);
        }
        for child in children.into_iter().rev() {
            stack.push((child, Some(this)));
        }
    }
    root_this
}

/// Block-level HTML element tags.
const BLOCK: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "body",
    "details",
    "dialog",
    "dd",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "html",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "ul",
];

fn is_heading(tag: &str) -> bool {
    matches!(tag, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

impl AstAdapter for HtmlAdapter {
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

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes[node.0 as usize].parent
    }

    /// Structural traits: `<block>` or `<inline>`, plus `<heading>`
    /// for `h1`–`h6` and `<link>` for an anchor with an `href`.
    fn traits(&self, node: NodeId) -> Vec<String> {
        let n = &self.nodes[node.0 as usize];
        let Some(tag) = n.tag.map(|t| self.names.get(t)) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if BLOCK.contains(&tag) {
            out.push("block".to_string());
        } else {
            out.push("inline".to_string());
        }
        if is_heading(tag) {
            out.push("heading".to_string());
        }
        if tag == "a" && n.attr(&self.names, "href").is_some() {
            out.push("link".to_string());
        }
        // A table in every substrate carries `tabula` (spec: The
        // Table): `//*<tabula>` finds it, `| #` reads it.
        if tag == "table" {
            out.push("tabula".to_string());
        }
        out
    }

    /// Attributes, by name (`::href`). Element text is the bare
    /// projection (`::`) — no attribute name is shadowed.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        self.nodes[node.0 as usize]
            .attr(&self.names, name)
            .map(|v| Value::Str(v.to_string()))
    }

    /// The default projection of an element is its text content.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        Some(Value::Str(self.text_of(node).to_string()))
    }

    /// `::::tag`, `::::classes` (the class attribute, split),
    /// `::::attrs` (the attribute names, sorted — the parser does
    /// not preserve source order), and `::::n-attrs` — facts about
    /// the element, never its data: attribute values are properties
    /// (`::href`), text is the bare projection. `attrs` is what
    /// makes a renamed attribute discoverable: enumerate the names,
    /// probe the values by shape.
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        match key {
            "tag" => n.tag.map(|t| Value::Str(self.names.get(t).to_string())),
            "classes" => n.attr(&self.names, "class").map(|c| {
                Value::list(
                    c.split_whitespace()
                        .map(|s| Value::Str(s.to_string()))
                        .collect(),
                )
            }),
            "attrs" => {
                let mut names: Vec<&str> =
                    n.attrs.iter().map(|(k, _)| self.names.get(*k)).collect();
                names.sort_unstable();
                Some(Value::list(
                    names
                        .into_iter()
                        .map(|k| Value::Str(k.to_string()))
                        .collect(),
                ))
            }
            "n-attrs" => Some(Value::Int(n.attrs.len() as i64)),
            _ => None,
        }
    }

    /// Follow an attribute that is a fragment reference (`#section`) to
    /// the element with that `id`. Used by an anchor's `::href-->`.
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        if property == "href" && !self.rel_ok(node, hint) {
            return None;
        }
        let value = self.nodes[node.0 as usize].attr(&self.names, property)?;
        let fragment = value.strip_prefix('#')?;
        self.ids.get(fragment).copied()
    }

    /// The external reference an `href`-like attribute holds: the
    /// absolute URL, relative values joined against the document's
    /// own URL (when the mount declared one). A bare `#fragment`
    /// stays in-document (that is `resolve`'s job); a fragment on
    /// a cross-document URL rides along — it is the crossref
    /// within the target, landed by `resolve_fragment` once that
    /// document is mounted. Non-web schemes reference nothing
    /// followable. Feeds `::href-->` across documents — the
    /// acquisition arrow.
    fn external_ref(
        &self,
        node: NodeId,
        property: &str,
        hint: Option<&str>,
    ) -> Option<quarb::ExternalRef> {
        if property == "href" && !self.rel_ok(node, hint) {
            return None;
        }
        let value = self.nodes[node.0 as usize].attr(&self.names, property)?;
        let t = value.trim();
        if t.is_empty() || t.starts_with('#') {
            return None;
        }
        let u = match url::Url::parse(t) {
            Ok(u) => u,
            // A relative reference: joined against the document's
            // URL where one is known; else handed over as written,
            // for the engine to make absolute against the tree the
            // page stands in (a file beside it in a directory).
            Err(url::ParseError::RelativeUrlWithoutBase) => match &self.document_url {
                Some(base) => base.join(t).ok()?,
                None => return Some(quarb::ExternalRef::uri(t)),
            },
            Err(_) => return None,
        };
        if u.scheme() != "http" && u.scheme() != "https" {
            return None;
        }
        Some(quarb::ExternalRef::uri(u.as_str()))
    }

    /// html's fragment landing: the element whose `id` is
    /// `fragment`, anywhere in this document.
    fn resolve_fragment(&self, _node: NodeId, fragment: &str) -> Option<NodeId> {
        self.ids.get(fragment).copied()
    }

    /// The bare arrow's property: an anchor-family element's
    /// `href`, a frame's `src`.
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

    fn ref_property(&self, node: NodeId) -> Option<String> {
        let n = &self.nodes[node.0 as usize];
        let (prop, tags): (&str, &[&str]) = match n.tag.map(|t| self.names.get(t)) {
            Some(t) if ["a", "link", "area"].contains(&t) => ("href", &["a", "link", "area"]),
            Some(t) if ["iframe", "frame"].contains(&t) => ("src", &["iframe", "frame"]),
            _ => return None,
        };
        let _ = tags;
        n.attr(&self.names, prop).map(|_| prop.to_string())
    }

    /// The relation an href edge carries: the `rel` attribute —
    /// `<link rel="stylesheet">`, `<a rel="nofollow">`. The spec's
    /// `-->stylesheet` hint filters on it; absent, the edge label
    /// falls back to the property name.
    fn ref_label(&self, node: NodeId, property: &str) -> Option<String> {
        if property != "href" {
            return None;
        }
        let rel = self.nodes[node.0 as usize].attr(&self.names, "rel")?;
        let rel = rel.trim();
        (!rel.is_empty()).then(|| rel.to_string())
    }
}

#[cfg(test)]
mod declared_tests {
    use super::declared_encoding;

    #[test]
    fn meta_charset_forms() {
        assert_eq!(
            declared_encoding(
                b"<!DOCTYPE html><html><head><meta charset=\"windows-1251\"><title>x</title>"
            ),
            Some("windows-1251".into())
        );
        assert_eq!(
            declared_encoding(b"<html><head><META http-equiv=\"Content-Type\" content=\"text/html; charset=koi8-r\">"),
            Some("koi8-r".into())
        );
        assert_eq!(
            declared_encoding(b"<html><head><meta charset=utf-8></head>"),
            Some("utf-8".into())
        );
        assert_eq!(
            declared_encoding(b"<html><head><meta name=\"charset-note\" content=\"x\"></head>"),
            None
        );
        assert_eq!(
            declared_encoding(b"<?xml version=\"1.0\" encoding=\"ISO-8859-5\"?><html/>"),
            Some("ISO-8859-5".into())
        );
        // a charset= inside another attribute's value declares nothing
        assert_eq!(
            declared_encoding(
                b"<meta name=\"description\" content=\"why charset=koi8-r was used\"><title>x</title>"
            ),
            None
        );
        // another processing instruction is no prologue
        assert_eq!(
            declared_encoding(b"<?xml-stylesheet href=\"a.xsl\" encoding=\"koi8-r\"?><html/>"),
            None
        );
        let mut late = vec![b' '; 1100];
        late.extend_from_slice(b"<meta charset=\"koi8-r\">");
        assert_eq!(declared_encoding(&late), None);
    }
}
