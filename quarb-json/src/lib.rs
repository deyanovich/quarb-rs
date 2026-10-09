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
//! - Adapter metadata exposes `;;;type` and `;;;length`.
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
    /// Under the property reading: the root's own scalar when the
    /// document is one.
    scalar: Value,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    /// Property reading: an object's scalar fields (a list for an
    /// array of scalars), in document order.
    fields: Vec<(String, Value)>,
    /// Property reading: an array node's scalar elements, with
    /// their positions (the serializers put them back in place).
    items: Vec<(usize, Value)>,
}

/// A Quarb adapter over a parsed JSON document.
pub struct JsonAdapter {
    nodes: Vec<Node>,
    root: NodeId,
    reading: Reading,
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
        Ok(Self::from_json_value(serde_json::from_str(text)?))
    }

    /// Parse `text` under the given reading (`json2` is
    /// [`Reading::Property`]).
    pub fn parse_as(text: &str, reading: Reading) -> serde_json::Result<Self> {
        Ok(Self::from_json_value_as(
            serde_json::from_str(text)?,
            reading,
        ))
    }

    /// [`parse_lines`](Self::parse_lines) under the given reading.
    pub fn parse_lines_as(text: &str, reading: Reading) -> serde_json::Result<Self> {
        let values = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<serde_json::Result<Vec<Json>>>()?;
        Ok(Self::from_json_value_as(Json::Array(values), reading))
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
        let values = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<serde_json::Result<Vec<Json>>>()?;
        Ok(Self::from_json_value(Json::Array(values)))
    }

    /// Build the adapter from an already-parsed value. YAML and
    /// TOML share the JSON data model, so their adapters route
    /// their parse through here.
    pub fn from_json_value(value: Json) -> Self {
        Self::from_json_value_as(value, Reading::Node)
    }

    /// Build the adapter from a parsed value under the given reading.
    pub fn from_json_value_as(value: Json, reading: Reading) -> Self {
        let mut nodes = Vec::new();
        match reading {
            Reading::Node => {
                build(&value, None, None, &mut nodes);
            }
            Reading::Property => {
                build_property(&value, None, None, &mut nodes);
            }
        }
        JsonAdapter {
            nodes,
            root: NodeId(0),
            reading,
        }
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
            return match self.nodes[node.0 as usize]
                .fields
                .iter()
                .find(|(k, _)| k == property)
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
        fields: Vec::new(),
        items: Vec::new(),
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

/// The property reading: intern `value` as a node when it is a
/// container (or the root), its scalar fields and elements kept
/// as values on it.
fn build_property(
    value: &Json,
    name: Option<String>,
    parent: Option<NodeId>,
    nodes: &mut Vec<Node>,
) -> NodeId {
    let id = nodes.len();
    let kind = Kind::of(value);
    nodes.push(Node {
        name,
        kind,
        // A scalar root carries its value; a scalar-array root its list.
        scalar: if parent.is_none() && is_property_shaped(value) {
            property_value(value)
        } else {
            Value::Null
        },
        parent,
        children: Vec::new(),
        fields: Vec::new(),
        items: Vec::new(),
    });
    let this = NodeId(id as u64);
    let mut children = Vec::new();
    let mut fields = Vec::new();
    let mut items = Vec::new();
    match value {
        Json::Object(map) => {
            for (k, v) in map {
                if is_property_shaped(v) {
                    fields.push((k.clone(), property_value(v)));
                } else {
                    children.push(build_property(v, Some(k.clone()), Some(this), nodes));
                }
            }
        }
        Json::Array(arr) => {
            // Only an array holding a container is reached here
            // under a key (a scalar array is its parent's property);
            // the root may be either, and a scalar-array root
            // already carries its list as the scalar.
            let root_list = parent.is_none() && is_property_shaped(value);
            for (i, v) in arr.iter().enumerate() {
                if matches!(v, Json::Object(_) | Json::Array(_)) {
                    children.push(build_property(v, Some(i.to_string()), Some(this), nodes));
                } else if !root_list {
                    items.push((i, scalar_of(v)));
                }
            }
        }
        _ => {}
    }
    let n = &mut nodes[id];
    n.children = children;
    n.fields = fields;
    n.items = items;
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

impl JsonAdapter {
    /// The node the literal `scope` names from the root, by child
    /// names; `None` when a name is missing or ambiguous.
    fn scope_node(&self, scope: &[&str]) -> Option<NodeId> {
        let mut at = self.root;
        for name in scope {
            let mut hits = self.nodes[at.0 as usize]
                .children
                .iter()
                .copied()
                .filter(|&c| self.nodes[c.0 as usize].name.as_deref() == Some(*name));
            at = hits.next()?;
            if hits.next().is_some() {
                return None;
            }
        }
        Some(at)
    }

    /// How many nodes `plan` reaches under `at`, from the arena.
    fn reach(&self, at: NodeId, plan: &quarb::StepPlan<'_>) -> u64 {
        let matches = |c: NodeId| {
            plan.name
                .is_none_or(|n| self.nodes[c.0 as usize].name.as_deref() == Some(n))
        };
        match plan.axis {
            quarb::PlanAxis::Child => self.nodes[at.0 as usize]
                .children
                .iter()
                .filter(|&&c| matches(c))
                .count() as u64,
            quarb::PlanAxis::Descendant => {
                let mut n = 0;
                let mut stack = self.nodes[at.0 as usize].children.clone();
                while let Some(c) = stack.pop() {
                    if matches(c) {
                        n += 1;
                    }
                    stack.extend(self.nodes[c.0 as usize].children.iter().copied());
                }
                n
            }
        }
    }
}

impl AstAdapter for JsonAdapter {
    /// The arena knows exactly how many nodes a step reaches; a
    /// conjunct makes that an upper bound. Nothing is read.
    fn estimate(&self, scope: &[&str], plan: &quarb::StepPlan<'_>) -> Option<quarb::Estimate> {
        let at = self.scope_node(scope)?;
        let n = self.reach(at, plan);
        Some(quarb::Estimate {
            rows: if plan.conjuncts.is_empty() {
                (n, n)
            } else {
                (0, n)
            },
            bytes: None,
            requests: Some(0),
            latency: quarb::Latency::Memory,
            confidence: if plan.conjuncts.is_empty() {
                quarb::Confidence::Exact
            } else {
                quarb::Confidence::Bound
            },
        })
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
        self.nodes[node.0 as usize].name.clone()
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
        if self.reading == Reading::Property {
            // The scalar fields live on the object itself.
            return n
                .fields
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone());
        }
        let child = n
            .children
            .iter()
            .find(|&&c| self.nodes[c.0 as usize].name.as_deref() == Some(name))?;
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
            Kind::Object => n.fields.clone(),
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

    /// `;;;type` (the JSON type) and `;;;length` (element count for a
    /// container, character count for a string).
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
            self.nodes[node.0 as usize]
                .fields
                .iter()
                .any(|(k, v)| k == "$ref" && matches!(v, Value::Str(_)))
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
