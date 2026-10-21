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
//! - A scalar-valued object field is *also* a property of its
//!   object (`::port` where `port` holds a primitive) — dual
//!   exposure, mirroring record substrates. Container-valued
//!   fields answer only to navigation.
//! - Adapter metadata exposes `:::type` and `:::length`.
//! - A `$ref`-style property resolves (`::'$ref'-->`) by treating its
//!   string value as a JSON Pointer (`#/definitions/address`).
//!
//! Object keys are visited in document order: the workspace builds
//! serde_json with its `preserve_order` feature, so keys keep the order
//! they appear in the source text rather than being sorted.
//!
//! Newline-delimited JSON (`.jsonl` / `.ndjson`) parses via
//! [`JsonAdapter::parse_lines`]: the lines become one array, so the
//! same queries work over both spellings of a record stream.
//!
//! JSON is ambiguous under the arbor model, so the adapter has two
//! readings ([`Reading`]). The default is the node reading above
//! (`json`). The *property reading* (`json2`,
//! [`JsonAdapter::parse_as`]) is the Quarb-idiomatic one: objects
//! and arrays are nodes, a scalar is a value a projection reads
//! (`::port`) and never a node — `/port` and `//port` land on
//! nothing, there is no `<string>` node, a scalar leaf has no
//! locator and cannot be marked or grafted into. An array of
//! scalars under a key is one list-valued property; an array that
//! holds a container stays a node, and its scalar elements are the
//! array node's own projection `::`, a list. The root is always a
//! node: a document that is a scalar, or an array of scalars,
//! carries it as the root's `::`.

use quarb::{AstAdapter, NodeId, Value};
use serde_json::Value as Json;

/// How a document is read onto the arbor.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Reading {
    /// `json`: every value a node; a scalar field also a property.
    #[default]
    Node,
    /// `json2`: containers are nodes, scalars are properties.
    Property,
}

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

    fn of_value(v: &Value) -> Kind {
        match v {
            Value::Str(_) => Kind::String,
            Value::Int(_) | Value::Float(_) => Kind::Number,
            Value::Bool(_) => Kind::Boolean,
            Value::Null => Kind::Null,
            _ => Kind::Array,
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

/// A node's name: an object key, interned once per document, or
/// an array position. Fifty thousand records share one `book_id`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Name {
    Key(u32),
    Index(u32),
}

struct Node {
    name: Option<Name>,
    kind: Kind,
    /// The scalar value for a primitive; `Value::Null` for containers.
    /// Under the property reading: the root's own scalar when the
    /// document is one.
    scalar: Value,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    /// Property reading: an object's scalar fields (a list for an
    /// array of scalars), in document order, keyed into the table.
    fields: Vec<(u32, Value)>,
    /// Property reading: an array node's scalar elements, with
    /// their positions (the serializers put them back in place).
    items: Vec<(usize, Value)>,
}

/// A Quarb adapter over a parsed JSON document.
pub struct JsonAdapter {
    nodes: Vec<Node>,
    /// The distinct object keys of the document, in first-seen
    /// order; a node's `Name::Key` indexes it.
    keys: Vec<String>,
    key_index: std::collections::HashMap<String, u32>,
    root: NodeId,
    reading: Reading,
}

/// The arena under construction: the nodes and the key table. The
/// streaming build fills it straight from the deserializer, so no
/// `serde_json::Value` tree stands at the peak; the tree build (a
/// YAML or TOML document's, a value handed in) walks a tree into it.
struct Builder {
    nodes: Vec<Node>,
    keys: Vec<String>,
    key_index: std::collections::HashMap<String, u32>,
}

impl Builder {
    fn new() -> Builder {
        Builder {
            nodes: Vec::new(),
            keys: Vec::new(),
            key_index: std::collections::HashMap::new(),
        }
    }

    fn intern(&mut self, key: &str) -> u32 {
        if let Some(&k) = self.key_index.get(key) {
            return k;
        }
        let k = self.keys.len() as u32;
        self.keys.push(key.to_string());
        self.key_index.insert(key.to_string(), k);
        k
    }

    fn push(
        &mut self,
        name: Option<Name>,
        kind: Kind,
        scalar: Value,
        parent: Option<NodeId>,
    ) -> NodeId {
        let id = NodeId(self.nodes.len() as u64);
        self.nodes.push(Node {
            name,
            kind,
            scalar,
            parent,
            children: Vec::new(),
            fields: Vec::new(),
            items: Vec::new(),
        });
        // A placeholder parent (an array node not yet made, see
        // the property reading's seed) links nothing; `reparent`
        // sets the edge once the node exists.
        if let Some(p) = parent
            && (p.0 as usize) < self.nodes.len()
        {
            self.nodes[p.0 as usize].children.push(id);
        }
        id
    }

    fn finish(self, reading: Reading) -> JsonAdapter {
        JsonAdapter {
            nodes: self.nodes,
            keys: self.keys,
            key_index: self.key_index,
            root: NodeId(0),
            reading,
        }
    }
}

// ---------------------------------------------------------------
// The streaming build: a seed per value, fed by the deserializer.

/// What the property reading made of a value: a scalar (its
/// parent's field), a list of scalars (a field too, under a key),
/// or a node.
enum Built {
    Scalar(Value),
    List(Vec<Value>),
    Node(NodeId),
}

/// The node reading's seed: every value a node under `parent`.
struct NodeSeed<'b> {
    b: &'b mut Builder,
    name: Option<Name>,
    parent: Option<NodeId>,
}

impl<'de> serde::de::DeserializeSeed<'de> for NodeSeed<'_> {
    type Value = NodeId;
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<NodeId, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> serde::de::Visitor<'de> for NodeSeed<'_> {
    type Value = NodeId;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_bool<E>(self, v: bool) -> Result<NodeId, E> {
        Ok(self
            .b
            .push(self.name, Kind::Boolean, Value::Bool(v), self.parent))
    }
    fn visit_i64<E>(self, v: i64) -> Result<NodeId, E> {
        Ok(self
            .b
            .push(self.name, Kind::Number, Value::Int(v), self.parent))
    }
    fn visit_u64<E>(self, v: u64) -> Result<NodeId, E> {
        let value = i64::try_from(v)
            .map(Value::Int)
            .unwrap_or(Value::Float(v as f64));
        Ok(self.b.push(self.name, Kind::Number, value, self.parent))
    }
    fn visit_f64<E>(self, v: f64) -> Result<NodeId, E> {
        Ok(self
            .b
            .push(self.name, Kind::Number, Value::Float(v), self.parent))
    }
    fn visit_str<E>(self, v: &str) -> Result<NodeId, E> {
        Ok(self.b.push(
            self.name,
            Kind::String,
            Value::Str(v.to_string()),
            self.parent,
        ))
    }
    fn visit_string<E>(self, v: String) -> Result<NodeId, E> {
        Ok(self
            .b
            .push(self.name, Kind::String, Value::Str(v), self.parent))
    }
    fn visit_unit<E>(self) -> Result<NodeId, E> {
        Ok(self.b.push(self.name, Kind::Null, Value::Null, self.parent))
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<NodeId, E> {
        self.visit_unit()
    }
    fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<NodeId, D::Error> {
        d.deserialize_any(self)
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<NodeId, A::Error> {
        let this = self
            .b
            .push(self.name, Kind::Array, Value::Null, self.parent);
        let mut i = 0u32;
        while seq
            .next_element_seed(NodeSeed {
                b: self.b,
                name: Some(Name::Index(i)),
                parent: Some(this),
            })?
            .is_some()
        {
            i += 1;
        }
        Ok(this)
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<NodeId, A::Error> {
        let this = self
            .b
            .push(self.name, Kind::Object, Value::Null, self.parent);
        while let Some(key) = map.next_key::<String>()? {
            let k = self.b.intern(&key);
            map.next_value_seed(NodeSeed {
                b: self.b,
                name: Some(Name::Key(k)),
                parent: Some(this),
            })?;
        }
        Ok(this)
    }
}

/// The property reading's seed: a container is a node, a scalar
/// its parent's field; an array of scalars under a key is a
/// list-valued field, under an array a node of items; the root is
/// always a node.
struct PropSeed<'b> {
    b: &'b mut Builder,
    name: Option<Name>,
    parent: Option<NodeId>,
    /// The parent is an object: a scalar array here is a field.
    in_object: bool,
}

impl<'de> serde::de::DeserializeSeed<'de> for PropSeed<'_> {
    type Value = Built;
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Built, D::Error> {
        d.deserialize_any(self)
    }
}

impl PropSeed<'_> {
    fn scalar(self, v: Value) -> Built {
        if self.parent.is_none() {
            let kind = Kind::of_value(&v);
            Built::Node(self.b.push(self.name, kind, v, None))
        } else {
            Built::Scalar(v)
        }
    }
}

impl<'de> serde::de::Visitor<'de> for PropSeed<'_> {
    type Value = Built;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_bool<E>(self, v: bool) -> Result<Built, E> {
        Ok(self.scalar(Value::Bool(v)))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Built, E> {
        Ok(self.scalar(Value::Int(v)))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Built, E> {
        Ok(self.scalar(
            i64::try_from(v)
                .map(Value::Int)
                .unwrap_or(Value::Float(v as f64)),
        ))
    }
    fn visit_f64<E>(self, v: f64) -> Result<Built, E> {
        Ok(self.scalar(Value::Float(v)))
    }
    fn visit_str<E>(self, v: &str) -> Result<Built, E> {
        Ok(self.scalar(Value::Str(v.to_string())))
    }
    fn visit_string<E>(self, v: String) -> Result<Built, E> {
        Ok(self.scalar(Value::Str(v)))
    }
    fn visit_unit<E>(self) -> Result<Built, E> {
        Ok(self.scalar(Value::Null))
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Built, E> {
        self.visit_unit()
    }
    fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<Built, D::Error> {
        d.deserialize_any(self)
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Built, A::Error> {
        // Under a key the array is a field until a container
        // shows: its scalars wait. Under an array, or at the root,
        // it is a node from the start.
        let root = self.parent.is_none();
        let mut node: Option<NodeId> = if self.in_object && !root {
            None
        } else {
            Some(
                self.b
                    .push(self.name, Kind::Array, Value::Null, self.parent),
            )
        };
        let mut pending: Vec<(usize, Value)> = Vec::new();
        let mut i = 0usize;
        loop {
            let built = seq.next_element_seed(PropSeed {
                b: self.b,
                name: Some(Name::Index(i as u32)),
                // The element's parent: the array node once it exists.
                // An element built before the node exists is a scalar
                // (a container makes the node first, below).
                parent: node.or(Some(NodeId(u64::MAX))),
                in_object: false,
            });
            // A container element needs the array node before it
            // is built: the seed above cannot make it, so a
            // placeholder parent marks the case and the element is
            // re-parented here.
            let Some(built) = built? else {
                break;
            };
            match built {
                Built::Scalar(v) => pending.push((i, v)),
                Built::List(vs) => {
                    // An array of scalars inside an array: a node of
                    // items (never a field).
                    let parent = *node.get_or_insert_with(|| {
                        self.b
                            .push(self.name, Kind::Array, Value::Null, self.parent)
                    });
                    let child = self.b.push(
                        Some(Name::Index(i as u32)),
                        Kind::Array,
                        Value::Null,
                        Some(parent),
                    );
                    self.b.nodes[child.0 as usize].items = vs.into_iter().enumerate().collect();
                }
                Built::Node(child) => {
                    let parent = *node.get_or_insert_with(|| {
                        self.b
                            .push(self.name, Kind::Array, Value::Null, self.parent)
                    });
                    self.b.reparent(child, parent);
                }
            }
            i += 1;
        }
        match node {
            None => Ok(Built::List(pending.into_iter().map(|(_, v)| v).collect())),
            Some(n) => {
                // A scalar-array root carries its list as the
                // scalar; any other array keeps its scalars as items.
                if root && self.b.nodes[n.0 as usize].children.is_empty() {
                    self.b.nodes[n.0 as usize].scalar =
                        Value::list(pending.into_iter().map(|(_, v)| v).collect());
                } else {
                    self.b.nodes[n.0 as usize].items = pending;
                }
                Ok(Built::Node(n))
            }
        }
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Built, A::Error> {
        let this = self
            .b
            .push(self.name, Kind::Object, Value::Null, self.parent);
        while let Some(key) = map.next_key::<String>()? {
            let k = self.b.intern(&key);
            let built = map.next_value_seed(PropSeed {
                b: self.b,
                name: Some(Name::Key(k)),
                parent: Some(this),
                in_object: true,
            })?;
            match built {
                Built::Scalar(v) => self.b.nodes[this.0 as usize].fields.push((k, v)),
                Built::List(vs) => self.b.nodes[this.0 as usize]
                    .fields
                    .push((k, Value::list(vs))),
                Built::Node(_) => {}
            }
        }
        Ok(Built::Node(this))
    }
}

impl Builder {
    /// Move `child` under `parent` (a container element built
    /// before its array node existed).
    fn reparent(&mut self, child: NodeId, parent: NodeId) {
        let old = self.nodes[child.0 as usize].parent;
        if old == Some(parent) {
            return;
        }
        if let Some(o) = old
            && (o.0 as usize) < self.nodes.len()
        {
            self.nodes[o.0 as usize].children.retain(|&c| c != child);
        }
        self.nodes[child.0 as usize].parent = Some(parent);
        self.nodes[parent.0 as usize].children.push(child);
    }
}

/// Decode the percent-escapes of a URI fragment (`%7E` → `~`); a
/// malformed escape is kept as written.
fn percent_decode(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(byte) = text
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl JsonAdapter {
    /// Parse `text` as JSON and build the adapter.
    pub fn parse(text: &str) -> serde_json::Result<Self> {
        Self::parse_as(text, Reading::Node)
    }

    /// Parse `text` under the given reading (`json2` is
    /// [`Reading::Property`]).
    pub fn parse_as(text: &str, reading: Reading) -> serde_json::Result<Self> {
        use serde::de::DeserializeSeed;
        let mut b = Builder::new();
        let mut d = serde_json::Deserializer::from_str(text);
        match reading {
            Reading::Node => {
                NodeSeed {
                    b: &mut b,
                    name: None,
                    parent: None,
                }
                .deserialize(&mut d)?;
            }
            Reading::Property => {
                PropSeed {
                    b: &mut b,
                    name: None,
                    parent: None,
                    in_object: false,
                }
                .deserialize(&mut d)?;
            }
        }
        d.end()?;
        Ok(b.finish(reading))
    }

    /// [`parse_lines`](Self::parse_lines) under the given reading.
    pub fn parse_lines_as(text: &str, reading: Reading) -> serde_json::Result<Self> {
        use serde::de::DeserializeSeed;
        // The lines are the elements of one array: the root node
        // first, then each line streamed under it.
        let mut b = Builder::new();
        let root = b.push(None, Kind::Array, Value::Null, None);
        let mut i = 0u32;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let mut d = serde_json::Deserializer::from_str(line);
            match reading {
                Reading::Node => {
                    NodeSeed {
                        b: &mut b,
                        name: Some(Name::Index(i)),
                        parent: Some(root),
                    }
                    .deserialize(&mut d)?;
                }
                Reading::Property => {
                    let built = PropSeed {
                        b: &mut b,
                        name: Some(Name::Index(i)),
                        parent: Some(root),
                        in_object: false,
                    }
                    .deserialize(&mut d)?;
                    match built {
                        Built::Scalar(v) => b.nodes[root.0 as usize].items.push((i as usize, v)),
                        Built::List(vs) => {
                            let child =
                                b.push(Some(Name::Index(i)), Kind::Array, Value::Null, Some(root));
                            b.nodes[child.0 as usize].items = vs.into_iter().enumerate().collect();
                        }
                        Built::Node(_) => {}
                    }
                }
            }
            d.end()?;
            i += 1;
        }
        Ok(b.finish(reading))
    }

    /// The reading this adapter took.
    pub fn reading(&self) -> Reading {
        self.reading
    }

    /// Parse newline-delimited JSON (JSONL / NDJSON): every
    /// non-empty line is one value, and the document is their
    /// array — so `/*` iterates the lines, exactly as it would
    /// the equivalent JSON array. This is also the shape `qua`
    /// itself emits, so results pipe back in.
    pub fn parse_lines(text: &str) -> serde_json::Result<Self> {
        Self::parse_lines_as(text, Reading::Node)
    }

    /// Build the adapter from an already-parsed value. YAML and
    /// TOML share the JSON data model, so their adapters route
    /// their parse through here.
    pub fn from_json_value(value: Json) -> Self {
        Self::from_json_value_as(value, Reading::Node)
    }

    /// Build the adapter from a parsed value under the given reading.
    pub fn from_json_value_as(value: Json, reading: Reading) -> Self {
        let mut b = Builder::new();
        match reading {
            Reading::Node => {
                build(&value, None, None, &mut b);
            }
            Reading::Property => {
                build_property(&value, None, None, &mut b);
            }
        }
        b.finish(reading)
    }

    /// The text of a node's name: the key from the table, or the
    /// array position.
    fn name_text(&self, name: Name) -> String {
        match name {
            Name::Key(k) => self.keys[k as usize].clone(),
            Name::Index(i) => i.to_string(),
        }
    }

    /// The table's key for `name`, when the document has it.
    fn key(&self, name: &str) -> Option<u32> {
        self.key_index.get(name).copied()
    }

    /// Resolve a reference value by the standards JSON has for
    /// one: a JSON Pointer (RFC 6901 — a path whose every token is
    /// led by `/`) and its URI fragment form (`#/a/b`, `#` for the
    /// whole document, percent-encoded; RFC 6901 section 6, the
    /// form a `$ref` holds). Any other string is not a reference
    /// into this document. Quarb reads the conventions data is
    /// written in and adds none of its own, so a bare word
    /// (`"users"`) points nowhere, however it happens to match a
    /// key.
    fn resolve_pointer(&self, reference: &str) -> Option<NodeId> {
        match reference.strip_prefix('#') {
            Some(fragment) => self.walk_pointer(&percent_decode(fragment)),
            None if reference.starts_with('/') => self.walk_pointer(reference),
            None => None,
        }
    }

    /// Evaluate a JSON Pointer (RFC 6901): the empty pointer is the
    /// whole document; otherwise each `/`-led token names a member
    /// (or an array index), with `~1` for `/` and `~0` for `~`.
    fn walk_pointer(&self, pointer: &str) -> Option<NodeId> {
        if pointer.is_empty() {
            return Some(self.root);
        }
        let path = pointer.strip_prefix('/')?;
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

    /// The string a node holds under `property`, in either reading.
    fn string_property(&self, node: NodeId, property: &str) -> Option<String> {
        if self.reading == Reading::Property {
            let k = self.key(property)?;
            return match self.nodes[node.0 as usize]
                .fields
                .iter()
                .find(|(f, _)| *f == k)
            {
                Some((_, Value::Str(s))) => Some(s.clone()),
                _ => None,
            };
        }
        let child = self
            .children(node)
            .into_iter()
            .find(|&c| self.name(c).as_deref() == Some(property))?;
        match &self.nodes[child.0 as usize].scalar {
            Value::Str(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// A JSON Pointer path to `node` (`/users/0/name`), for rendering.
    pub fn pointer(&self, node: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(node);
        while let Some(id) = cur {
            let n = &self.nodes[id.0 as usize];
            if let Some(name) = n.name {
                parts.push(self.name_text(name));
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

/// Recursively intern `value` (reached under `name` from `parent`):
/// the node reading over a parsed tree.
fn build(value: &Json, name: Option<Name>, parent: Option<NodeId>, b: &mut Builder) -> NodeId {
    let this = b.push(name, Kind::of(value), scalar_of(value), parent);
    match value {
        Json::Object(map) => {
            for (k, v) in map {
                let k = b.intern(k);
                build(v, Some(Name::Key(k)), Some(this), b);
            }
        }
        Json::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                build(v, Some(Name::Index(i as u32)), Some(this), b);
            }
        }
        _ => {}
    }
    this
}

/// Whether `value` is a scalar, or an array of scalars — the shapes
/// the property reading answers as a property, never as a node.
fn is_property_shaped(value: &Json) -> bool {
    match value {
        Json::Object(_) => false,
        Json::Array(arr) => arr
            .iter()
            .all(|v| !matches!(v, Json::Object(_) | Json::Array(_))),
        _ => true,
    }
}

/// A scalar, or an array of scalars as a list.
fn property_value(value: &Json) -> Value {
    match value {
        Json::Array(arr) => Value::list(arr.iter().map(scalar_of).collect()),
        other => scalar_of(other),
    }
}

/// The property reading over a parsed tree: intern `value` as a
/// node when it is a container (or the root), its scalar fields
/// and elements kept as values on it.
fn build_property(
    value: &Json,
    name: Option<Name>,
    parent: Option<NodeId>,
    b: &mut Builder,
) -> NodeId {
    let kind = Kind::of(value);
    // A scalar root carries its value; a scalar-array root its list.
    let scalar = if parent.is_none() && is_property_shaped(value) {
        property_value(value)
    } else {
        Value::Null
    };
    let this = b.push(name, kind, scalar, parent);
    match value {
        Json::Object(map) => {
            for (k, v) in map {
                let k = b.intern(k);
                if is_property_shaped(v) {
                    b.nodes[this.0 as usize].fields.push((k, property_value(v)));
                } else {
                    build_property(v, Some(Name::Key(k)), Some(this), b);
                }
            }
        }
        Json::Array(arr) => {
            let root_list = parent.is_none() && is_property_shaped(value);
            for (i, v) in arr.iter().enumerate() {
                if matches!(v, Json::Object(_) | Json::Array(_)) {
                    build_property(v, Some(Name::Index(i as u32)), Some(this), b);
                } else if !root_list {
                    b.nodes[this.0 as usize].items.push((i, scalar_of(v)));
                }
            }
        }
        _ => {}
    }
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
    /// The arena knows exactly how many nodes a step reaches; a
    /// conjunct makes that an upper bound. Nothing is read.
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
        self.nodes[node.0 as usize].name.map(|n| self.name_text(n))
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes[node.0 as usize].parent
    }

    /// The node's JSON type: `<object>`, `<string>`, `<number>`, …
    /// The value type, and `scalar` over the primitive types — the
    /// ones dual exposure answers as properties (string, number,
    /// boolean, and null: a null field is exposed too).
    fn traits(&self, node: NodeId) -> Vec<String> {
        let kind = self.nodes[node.0 as usize].kind;
        let mut out = vec![kind.name().to_string()];
        if !matches!(kind, Kind::Object | Kind::Array) {
            out.push("scalar".to_string());
        }
        out
    }

    /// Dual exposure: a scalar-valued object field answers as a
    /// property too, so `::port` reads a flat field the way a CSV
    /// column or an XML attribute reads. The child spelling
    /// (`/port::`) always works and is the shape-stable one — a
    /// field that holds an object in *this* document answers only
    /// to navigation, never as a property. An array element is not
    /// a property of its array (an index is a position, not a
    /// field); an array whose elements are all scalars is one
    /// *list-valued* property of the object — the shape a page's
    /// tags have — read whole by `::marks`, spread by `| ...`, and
    /// resolved per element by a declared ref. An array holding a
    /// container answers only to navigation.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        if !matches!(n.kind, Kind::Object) {
            return None;
        }
        let key = self.key(name)?;
        if self.reading == Reading::Property {
            // The scalar fields live on the object itself.
            return n
                .fields
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.clone());
        }
        let child = n
            .children
            .iter()
            .find(|&&c| self.nodes[c.0 as usize].name == Some(Name::Key(key)))?;
        let c = &self.nodes[child.0 as usize];
        match c.kind {
            Kind::Object => None,
            Kind::Array => {
                let items = c.children.iter().map(|&i| &self.nodes[i.0 as usize]);
                if items
                    .clone()
                    .any(|e| matches!(e.kind, Kind::Object | Kind::Array))
                {
                    None
                } else {
                    Some(Value::list(items.map(|e| e.scalar.clone()).collect()))
                }
            }
            _ => Some(c.scalar.clone()),
        }
    }

    /// Property reading: an object's scalar fields, an array's
    /// scalar elements under their index — what the serializers
    /// merge with the children. The node reading has none: every
    /// value is a child there.
    fn properties(&self, node: NodeId) -> Vec<(String, Value)> {
        if self.reading != Reading::Property {
            return Vec::new();
        }
        let n = &self.nodes[node.0 as usize];
        match n.kind {
            Kind::Object => n
                .fields
                .iter()
                .map(|(k, v)| (self.keys[*k as usize].clone(), v.clone()))
                .collect(),
            Kind::Array => n
                .items
                .iter()
                .map(|(i, v)| (i.to_string(), v.clone()))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// A primitive projects to its value; a container has no default
    /// projection (navigate into it instead).
    fn default_value(&self, node: NodeId) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        if self.reading == Reading::Property {
            // A mixed array projects its scalar elements as a list;
            // a scalar (or scalar-array) root projects its value.
            return match n.kind {
                Kind::Array if !n.items.is_empty() => Some(Value::list(
                    n.items.iter().map(|(_, v)| v.clone()).collect(),
                )),
                Kind::Object => None,
                Kind::Array => {
                    (n.parent.is_none() && n.scalar != Value::Null).then(|| n.scalar.clone())
                }
                _ => Some(n.scalar.clone()),
            };
        }
        match n.kind {
            Kind::Object | Kind::Array => None,
            _ => Some(n.scalar.clone()),
        }
    }

    /// `:::type` (the JSON type) and `:::length` (element count for a
    /// container, character count for a string).
    /// The `:::` keys this adapter answers (`:meta` lists the ones
    /// a node answers).
    fn metadata_catalogue(&self) -> &'static [&'static str] {
        &["type", "length"]
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        match key {
            "type" => Some(Value::Str(n.kind.name().to_string())),
            "length" => match n.kind {
                Kind::Object | Kind::Array => Some(Value::Int(
                    (n.children.len() + n.fields.len() + n.items.len()) as i64,
                )),
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
    /// Any string value may be a JSON Pointer, into any part of the
    /// document: the format declares no narrower reference.
    fn can_refer_to(&self, _source: NodeId, _target: NodeId) -> Option<bool> {
        Some(true)
    }

    /// JSON's own reference syntax: an object's `$ref`. The bare
    /// arrows (`//x-->`, `t<--`) read it.
    fn ref_property(&self, node: NodeId) -> Option<String> {
        let has = if self.reading == Reading::Property {
            self.key("$ref").is_some_and(|key| {
                self.nodes[node.0 as usize]
                    .fields
                    .iter()
                    .any(|(k, v)| *k == key && matches!(v, Value::Str(_)))
            })
        } else {
            self.children(node).into_iter().any(|c| {
                self.name(c).as_deref() == Some("$ref")
                    && matches!(self.nodes[c.0 as usize].scalar, Value::Str(_))
            })
        };
        has.then(|| "$ref".to_string())
    }

    /// The node a property's string refers to, by the two standards
    /// JSON has. `$ref` holds a URI reference: only its fragment —
    /// `#/a/b`, `#`, and the empty reference — points into this
    /// document (anything else is another resource, see
    /// [`external_ref`](Self::external_ref)). Any other property is
    /// read as a JSON Pointer, in the plain form (`/a/b`) or the
    /// fragment form (`#/a/b`, `#`); an empty string there is an
    /// empty value, and a bare word is text.
    fn resolve(&self, node: NodeId, property: &str, _hint: Option<&str>) -> Option<NodeId> {
        let reference = self.string_property(node, property)?;
        if property == "$ref" {
            if reference.is_empty() {
                return Some(self.root);
            }
            let fragment = reference.strip_prefix('#')?;
            return self.walk_pointer(&percent_decode(fragment));
        }
        if reference.is_empty() {
            return None;
        }
        self.resolve_pointer(&reference)
    }

    /// A `$ref` to another resource (`other.json#/definitions/x`,
    /// `/schemas/x.json`): the URI read into its document and its
    /// fragment, for the engine to look up among the mounts. Only
    /// the reference keyword is read this way; any other property
    /// holding such a string is plain text.
    fn external_ref(
        &self,
        node: NodeId,
        property: &str,
        _hint: Option<&str>,
    ) -> Option<quarb::ExternalRef> {
        if property != "$ref" {
            return None;
        }
        let reference = self.string_property(node, property)?;
        (!reference.is_empty() && !reference.starts_with('#'))
            .then(|| quarb::ExternalRef::uri(&reference))
    }

    /// The location of a reference that landed in this document: a
    /// JSON Pointer in its URI fragment form.
    fn resolve_fragment(&self, _node: NodeId, fragment: &str) -> Option<NodeId> {
        self.walk_pointer(&percent_decode(fragment))
    }
}
