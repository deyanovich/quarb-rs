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
//!   default projection (`::`) and `::text` are the element's text
//!   content; `::;tag`, `::;id`, `::;classes`, and any `::;attr`
//!   expose metadata.
//! - An anchor's fragment `href` resolves: `::href~>` follows
//!   `#section` to the element with `id="section"`.

use quarb::{AstAdapter, NodeId, Value};
use scraper::{ElementRef, Html};
use std::collections::HashMap;

struct Node {
    /// The tag name; `None` for the document root.
    tag: Option<String>,
    attrs: Vec<(String, String)>,
    /// The element's text content (all descendant text, concatenated).
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

/// A Quarb adapter over a parsed HTML document.
pub struct HtmlAdapter {
    nodes: Vec<Node>,
    /// `id` attribute value → the element carrying it.
    ids: HashMap<String, NodeId>,
    root: NodeId,
}

impl HtmlAdapter {
    /// Parse `html` and build the adapter.
    pub fn parse(html: &str) -> Self {
        let document = Html::parse_document(html);
        let mut nodes = vec![Node {
            tag: None,
            attrs: Vec::new(),
            text: String::new(),
            parent: None,
            children: Vec::new(),
        }];
        let mut ids = HashMap::new();
        let root = NodeId(0);

        let html_node = build(document.root_element(), Some(root), &mut nodes, &mut ids);
        nodes[0].text = nodes[html_node.0 as usize].text.clone();
        nodes[0].children = vec![html_node];

        HtmlAdapter { nodes, ids, root }
    }

    /// A locator path to `node`, like `/html/body/div[2]/p`, for
    /// rendering. A `[n]` index is added only to disambiguate same-tag
    /// siblings.
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

/// Intern the element `el` (child of `parent`), returning its node id.
/// Text and comment children are skipped as nodes but captured in the
/// element's text content.
///
/// Iterative (an explicit work stack rather than self-recursion) so
/// pathologically deep markup cannot overflow the call stack; html5ever
/// parses such input iteratively, so a recursive interning pass was the
/// only remaining depth limit. Elements are interned in document
/// (pre-order) order, so node ids, child ordering, and the
/// first-occurrence `ids` map are identical to the recursive form.
fn build(
    el: ElementRef,
    parent: Option<NodeId>,
    nodes: &mut Vec<Node>,
    ids: &mut HashMap<String, NodeId>,
) -> NodeId {
    let root_this = NodeId(nodes.len() as u64);
    // Work stack of (element, parent id), popped in pre-order.
    let mut stack = vec![(el, parent)];

    while let Some((el, parent)) = stack.pop() {
        let this = NodeId(nodes.len() as u64);

        let tag = el.value().name().to_string();
        let attrs: Vec<(String, String)> = el
            .value()
            .attrs()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let text: String = el.text().collect();

        if let Some(id) = el.value().id() {
            ids.entry(id.to_string()).or_insert(this);
        }

        nodes.push(Node {
            tag: Some(tag),
            attrs,
            text,
            parent,
            children: Vec::new(),
        });

        // Record the child under its parent as it is interned; because
        // siblings are pushed reversed below, they pop in document order
        // and so append in document order.
        if let Some(p) = parent {
            nodes[p.0 as usize].children.push(this);
        }

        // Push element children in reverse so the leftmost is popped
        // (and interned) first — a pre-order DFS.
        let children: Vec<ElementRef> = el.children().filter_map(ElementRef::wrap).collect();
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

    /// Structural traits: `<block>` or `<inline>`, plus `<heading>`
    /// for `h1`–`h6` and `<link>` for an anchor with an `href`.
    fn traits(&self, node: NodeId) -> Vec<String> {
        let n = &self.nodes[node.0 as usize];
        let Some(tag) = &n.tag else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if BLOCK.contains(&tag.as_str()) {
            out.push("block".to_string());
        } else {
            out.push("inline".to_string());
        }
        if is_heading(tag) {
            out.push("heading".to_string());
        }
        if tag == "a" && n.attr("href").is_some() {
            out.push("link".to_string());
        }
        out
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

    /// `::;tag`, `::;text`, `::;id`, `::;classes`, `::;n-attrs`, and
    /// any attribute by name (`::;href`).
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        match key {
            "tag" => n.tag.clone().map(Value::Str),
            "text" => Some(Value::Str(n.text.clone())),
            "id" => n.attr("id").map(|v| Value::Str(v.to_string())),
            "classes" => n.attr("class").map(|c| {
                Value::List(
                    c.split_whitespace()
                        .map(|s| Value::Str(s.to_string()))
                        .collect(),
                )
            }),
            "n-attrs" => Some(Value::Int(n.attrs.len() as i64)),
            other => n.attr(other).map(|v| Value::Str(v.to_string())),
        }
    }

    /// Follow an attribute that is a fragment reference (`#section`) to
    /// the element with that `id`. Used by an anchor's `::href~>`.
    fn resolve(&self, node: NodeId, property: &str, _hint: Option<&str>) -> Option<NodeId> {
        let value = self.nodes[node.0 as usize].attr(property)?;
        let fragment = value.strip_prefix('#')?;
        self.ids.get(fragment).copied()
    }
}
