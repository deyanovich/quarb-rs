//! JSON adapter for Quarb.
//!
//! Maps a JSON document onto the arbor model, following the
//! specification's recommended mapping:
//!
//! - Objects, arrays, and primitives are all nodes.
//! - A node's *name* is the object key or array index that leads to
//!   it, so `/users/0/name` navigates by key and index just like a
//!   file path. The root is unnamed.
//! - A node's *trait* is its JSON type (`<object>`, `<array>`,
//!   `<string>`, `<number>`, `<boolean>`, `<null>`), so the value
//!   type is available for filtering alongside the key.
//! - A primitive's default projection (`::`) is its value.
//! - Adapter metadata exposes `::;type` and `::;length`.
//! - A `$ref`-style property resolves (`::'$ref'~>`) by treating its
//!   string value as a JSON Pointer (`#/definitions/address`).
//!
//! Object keys are visited in document order: the workspace builds
//! serde_json with its `preserve_order` feature, so keys keep the order
//! they appear in the source text rather than being sorted.

use quarb::{AstAdapter, NodeId, Value};
use serde_json::Value as Json;

/// The JSON type of a node, used as its trait.
#[derive(Clone, Copy)]
enum Kind {
    Object,
    Array,
    String,
    Number,
    Boolean,
    Null,
}

impl Kind {
    fn of(value: &Json) -> Kind {
        match value {
            Json::Object(_) => Kind::Object,
            Json::Array(_) => Kind::Array,
            Json::String(_) => Kind::String,
            Json::Number(_) => Kind::Number,
            Json::Bool(_) => Kind::Boolean,
            Json::Null => Kind::Null,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Object => "object",
            Kind::Array => "array",
            Kind::String => "string",
            Kind::Number => "number",
            Kind::Boolean => "boolean",
            Kind::Null => "null",
        }
    }
}

struct Node {
    name: Option<String>,
    kind: Kind,
    /// The scalar value for a primitive; `Value::Null` for containers.
    scalar: Value,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
}

/// A Quarb adapter over a parsed JSON document.
pub struct JsonAdapter {
    nodes: Vec<Node>,
    root: NodeId,
}

impl JsonAdapter {
    /// Parse `text` as JSON and build the adapter.
    pub fn parse(text: &str) -> serde_json::Result<Self> {
        Ok(Self::from_json_value(serde_json::from_str(text)?))
    }

    /// Build the adapter from an already-parsed value. YAML and
    /// TOML share the JSON data model, so their adapters route
    /// their parse through here.
    pub fn from_json_value(value: Json) -> Self {
        let mut nodes = Vec::new();
        build(&value, None, None, &mut nodes);
        JsonAdapter {
            nodes,
            root: NodeId(0),
        }
    }

    /// Resolve a JSON Pointer (e.g. `#/definitions/address` or
    /// `/users/0`) from the root to a node.
    fn resolve_pointer(&self, pointer: &str) -> Option<NodeId> {
        let path = pointer.strip_prefix('#').unwrap_or(pointer);
        let path = path.strip_prefix('/').unwrap_or(path);
        if path.is_empty() {
            return Some(self.root);
        }
        let mut cur = self.root;
        for part in path.split('/') {
            let token = part.replace("~1", "/").replace("~0", "~");
            cur = self
                .children(cur)
                .into_iter()
                .find(|&c| self.name(c).as_deref() == Some(token.as_str()))?;
        }
        Some(cur)
    }

    /// A JSON Pointer path to `node` (`/users/0/name`), for rendering.
    pub fn pointer(&self, node: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(node);
        while let Some(id) = cur {
            let n = &self.nodes[id.0 as usize];
            if let Some(name) = &n.name {
                parts.push(name.clone());
            }
            cur = n.parent;
        }
        parts.reverse();
        if parts.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", parts.join("/"))
        }
    }
}

/// Recursively intern `value` (reached under `name` from `parent`).
fn build(
    value: &Json,
    name: Option<String>,
    parent: Option<NodeId>,
    nodes: &mut Vec<Node>,
) -> NodeId {
    let id = nodes.len();
    nodes.push(Node {
        name,
        kind: Kind::of(value),
        scalar: scalar_of(value),
        parent,
        children: Vec::new(),
    });
    let this = NodeId(id as u64);

    let child_ids = match value {
        Json::Object(map) => map
            .iter()
            .map(|(k, v)| build(v, Some(k.clone()), Some(this), nodes))
            .collect(),
        Json::Array(arr) => arr
            .iter()
            .enumerate()
            .map(|(i, v)| build(v, Some(i.to_string()), Some(this), nodes))
            .collect(),
        _ => Vec::new(),
    };
    nodes[id].children = child_ids;
    this
}

/// The scalar value of a primitive; `Null` for containers.
fn scalar_of(value: &Json) -> Value {
    match value {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => n
            .as_i64()
            .map(Value::Int)
            .or_else(|| n.as_f64().map(Value::Float))
            .unwrap_or(Value::Null),
        Json::String(s) => Value::Str(s.clone()),
        _ => Value::Null,
    }
}

impl AstAdapter for JsonAdapter {
    fn root(&self) -> NodeId {
        self.root
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.nodes[node.0 as usize].children.clone()
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.nodes[node.0 as usize].name.clone()
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes[node.0 as usize].parent
    }

    /// The node's JSON type: `<object>`, `<string>`, `<number>`, …
    fn traits(&self, node: NodeId) -> Vec<String> {
        vec![self.nodes[node.0 as usize].kind.name().to_string()]
    }

    /// A primitive projects to its value; a container has no default
    /// projection (navigate into it instead).
    fn default_value(&self, node: NodeId) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        match n.kind {
            Kind::Object | Kind::Array => None,
            _ => Some(n.scalar.clone()),
        }
    }

    /// `::;type` (the JSON type) and `::;length` (element count for a
    /// container, character count for a string).
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        match key {
            "type" => Some(Value::Str(n.kind.name().to_string())),
            "length" => match n.kind {
                Kind::Object | Kind::Array => Some(Value::Int(n.children.len() as i64)),
                Kind::String => {
                    if let Value::Str(s) = &n.scalar {
                        Some(Value::Int(s.chars().count() as i64))
                    } else {
                        None
                    }
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Resolve `node`'s `property` child\,---\,whose string value is a
    /// JSON Pointer (`#/definitions/address`)\,---\,to its target node.
    /// The hint is unused (JSON references carry no relation type).
    fn resolve(&self, node: NodeId, property: &str, _hint: Option<&str>) -> Option<NodeId> {
        let child = self
            .children(node)
            .into_iter()
            .find(|&c| self.name(c).as_deref() == Some(property))?;
        if let Value::Str(pointer) = &self.nodes[child.0 as usize].scalar {
            self.resolve_pointer(pointer)
        } else {
            None
        }
    }
}
