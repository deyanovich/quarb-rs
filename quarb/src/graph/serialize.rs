//! Part of the executor: see `exec/mod.rs`.

use super::*;

/// A node's subtree as a JSON-shaped value — the encode half of
/// the node-mode json pair. Built bottom-up on an explicit stack,
/// so a document nested as deep as its source allows serializes
/// within the thread's stack.
#[cfg_attr(not(feature = "kaiv"), allow(dead_code))]
pub(crate) fn node_to_json(adapter: &impl AstAdapter, node: NodeId) -> Value {
    struct Frame {
        node: NodeId,
        children: Vec<NodeId>,
        next: usize,
        built: Vec<(String, Value)>,
    }
    let frame = |n: NodeId| Frame {
        node: n,
        children: adapter.children(n),
        next: 0,
        built: Vec::new(),
    };
    let mut stack = vec![frame(node)];
    loop {
        let top = stack.last_mut().expect("a frame");
        if top.next < top.children.len() {
            let c = top.children[top.next];
            top.next += 1;
            stack.push(frame(c));
            continue;
        }
        let done = stack.pop().expect("a frame");
        let value = node_value_from(adapter, done.node, done.built);
        match stack.last_mut() {
            Some(parent) => parent
                .built
                .push((adapter.name(done.node).unwrap_or_default(), value)),
            None => return value,
        }
    }
}

/// A node's subtree as JSON text — what `| json` in a node context
/// writes. Bottom-up on an explicit stack, each node's text
/// assembled from its children's, so the depth of the document
/// never becomes the depth of the stack (and no nested value is
/// built to be dropped).
pub(crate) fn node_to_json_text(adapter: &impl AstAdapter, node: NodeId) -> String {
    struct Frame {
        node: NodeId,
        children: Vec<NodeId>,
        next: usize,
        built: Vec<(String, String)>,
    }
    let frame = |n: NodeId| Frame {
        node: n,
        children: adapter.children(n),
        next: 0,
        built: Vec::new(),
    };
    let mut stack = vec![frame(node)];
    loop {
        let top = stack.last_mut().expect("a frame");
        if top.next < top.children.len() {
            let c = top.children[top.next];
            top.next += 1;
            stack.push(frame(c));
            continue;
        }
        let done = stack.pop().expect("a frame");
        let mut entries = done.built;
        for (k, v) in adapter.properties(done.node) {
            if !entries.iter().any(|(n, _)| *n == k) {
                entries.push((k, v.to_json()));
            }
        }
        let text = if entries.is_empty() {
            adapter
                .default_value(done.node)
                .unwrap_or(Value::Null)
                .to_json()
        } else {
            if entries.len() > 1 && entries.iter().all(|(n, _)| n.parse::<usize>().is_ok()) {
                entries.sort_by_key(|(n, _)| n.parse::<usize>().unwrap_or(usize::MAX));
            }
            let is_array = entries
                .iter()
                .enumerate()
                .all(|(i, (n, _))| n == &i.to_string());
            if is_array {
                let inner: Vec<&str> = entries.iter().map(|(_, t)| t.as_str()).collect();
                format!("[{}]", inner.join(", "))
            } else {
                let inner: Vec<String> = entries
                    .iter()
                    .map(|(k, t)| format!("{}: {t}", Value::Str(k.clone()).to_json()))
                    .collect();
                format!("{{{}}}", inner.join(", "))
            }
        };
        match stack.last_mut() {
            Some(parent) => parent
                .built
                .push((adapter.name(done.node).unwrap_or_default(), text)),
            None => return text,
        }
    }
}

/// The value of a node whose children are already built: the
/// entries (children, then the enumerated properties a child does
/// not shadow — a reading that keeps scalars off the tree) as a
/// list when every name is an index, else a record; a node with no
/// entries is its own scalar.
#[cfg_attr(not(feature = "kaiv"), allow(dead_code))]
pub(crate) fn node_value_from(
    adapter: &impl AstAdapter,
    node: NodeId,
    mut entries: Vec<(String, Value)>,
) -> Value {
    for (k, v) in adapter.properties(node) {
        if !entries.iter().any(|(n, _)| *n == k) {
            entries.push((k, v));
        }
    }
    if entries.is_empty() {
        return adapter.default_value(node).unwrap_or(Value::Null);
    }
    // Entries whose names are all indexes come in index order, so a
    // mixed array comes back in the source's order.
    if entries.len() > 1 && entries.iter().all(|(n, _)| n.parse::<usize>().is_ok()) {
        entries.sort_by_key(|(n, _)| n.parse::<usize>().unwrap_or(usize::MAX));
    }
    let is_array = entries
        .iter()
        .enumerate()
        .all(|(i, (n, _))| n == &i.to_string());
    if is_array {
        Value::list(entries.into_iter().map(|(_, v)| v).collect())
    } else {
        Value::Record(entries)
    }
}

/// Serialize a node's subtree as XML: the node's name is the tag
/// (an unnamed root emits its children bare); children nest, a
/// leaf carries its scalar as text.
pub(crate) fn node_to_xml(adapter: &impl AstAdapter, node: NodeId) -> String {
    match adapter.name(node) {
        Some(tag) => node_to_xml_named(adapter, node, &xml_tag(&tag)),
        None => adapter
            .children(node)
            .iter()
            .map(|&c| {
                let t = adapter
                    .name(c)
                    .map(|n| xml_tag(&n))
                    .unwrap_or_else(|| "item".into());
                node_to_xml_named(adapter, c, &t)
            })
            .collect(),
    }
}

pub(crate) fn node_to_xml_named(adapter: &impl AstAdapter, node: NodeId, tag: &str) -> String {
    // Bottom-up on an explicit stack, as the JSON form: a node's
    // element is assembled once its children's are.
    struct Frame {
        node: NodeId,
        tag: String,
        children: Vec<NodeId>,
        names: Vec<Option<String>>,
        next: usize,
        inner: String,
    }
    let frame = |n: NodeId, tag: String| {
        let children = adapter.children(n);
        let names: Vec<Option<String>> = children.iter().map(|&c| adapter.name(c)).collect();
        Frame {
            node: n,
            tag,
            children,
            names,
            next: 0,
            inner: String::new(),
        }
    };
    let mut stack = vec![frame(node, tag.to_string())];
    loop {
        let top = stack.last_mut().expect("a frame");
        if top.next < top.children.len() {
            let c = top.children[top.next];
            let t = top.names[top.next]
                .as_deref()
                .map(xml_tag)
                .unwrap_or_else(|| "item".into());
            top.next += 1;
            stack.push(frame(c, t));
            continue;
        }
        let done = stack.pop().expect("a frame");
        // A reading that keeps scalars off the tree (the JSON
        // property reading) enumerates them; they nest as elements
        // after the children, a child winning a shared name.
        let props: Vec<(String, Value)> = adapter
            .properties(done.node)
            .into_iter()
            .filter(|(k, _)| !done.names.iter().any(|n| n.as_deref() == Some(k.as_str())))
            .collect();
        let element = if done.children.is_empty() && props.is_empty() {
            let body = adapter
                .default_value(done.node)
                .map(|v| xml_escape(&v.to_string()))
                .unwrap_or_default();
            format!("<{0}>{body}</{0}>", done.tag)
        } else {
            let mut inner = done.inner;
            for (k, v) in &props {
                inner.push_str(&value_to_xml(v, &xml_tag(k)));
            }
            format!("<{0}>{inner}</{0}>", done.tag)
        };
        match stack.last_mut() {
            Some(parent) => parent.inner.push_str(&element),
            None => return element,
        }
    }
}

/// A valid XML element name, or `item` when the source name is
/// not one (e.g. a JSON array index "0" — tags cannot start with
/// a digit).
pub(crate) fn xml_tag(name: &str) -> String {
    let mut chars = name.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'));
    if ok {
        name.to_string()
    } else {
        "item".to_string()
    }
}

/// Escape the five XML predefined characters in text/attributes.
pub(crate) fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Serialize a value as XML, wrapping it in `<tag>` when it needs
/// a name (a scalar, or a list's items). A record's `@name`
/// fields become attributes, `#text` its text, the rest nested
/// elements — the inverse of the decode convention.
pub(crate) fn value_to_xml(v: &Value, tag: &str) -> String {
    match v {
        Value::Record(fields) => {
            let mut attrs = String::new();
            let mut body = String::new();
            for (k, val) in fields {
                if let Some(a) = k.strip_prefix('@') {
                    attrs.push_str(&format!(" {a}=\"{}\"", xml_escape(&val.to_string())));
                } else if k == "#text" {
                    body.push_str(&xml_escape(&val.to_string()));
                } else if let Value::List(items) = val {
                    for it in items.iter() {
                        body.push_str(&value_to_xml(it, &xml_tag(k)));
                    }
                } else {
                    body.push_str(&value_to_xml(val, &xml_tag(k)));
                }
            }
            format!("<{tag}{attrs}>{body}</{tag}>")
        }
        Value::List(items) => items.iter().map(|it| value_to_xml(it, tag)).collect(),
        other => format!("<{tag}>{}</{tag}>", xml_escape(&other.to_string())),
    }
}
