//! Model files: derived arbor structure over any Quarb source.
//!
//! A [`Model`] (parsed from a `--model` file, see [`parse_model`])
//! declares *derived* containers, references, and edges over a
//! *base* arbor — the top rung of the reference-provenance ladder,
//! generalizing the SQLite views-and-`--refs` construction to any
//! substrate (a CSV, a JSON export, a log stream).
//!
//! [`ModelAdapter`] wraps a base adapter and presents the base's own
//! nodes unchanged, adding the derived containers as new root
//! children in a reserved node-id band (bit 63). Derived structure
//! materializes lazily: a `node` constructor runs its query over the
//! *base* (never over `self`, so no recursion) on first touch, and
//! the distinct string-keyed values are cached. Declared references
//! and edges build forward/reverse value indexes on first use.

mod parse;

pub use parse::{parse_model, resolve_mount_target, EdgeDecl, Model, Mount, NodeDecl, RefDecl};

use quarb::{AstAdapter, NodeId, QueryResult, Value};
use std::cell::OnceCell;
use std::collections::HashMap;

/// Derived nodes carry this tag bit; the base's own ids never set it
/// (the mount layer's index rides bits 56–63 but never reaches 128,
/// so bit 63 stays clear for realistic inputs). Below it: the
/// container index (bits 44–62) and the value index (bits 0–43).
const MODEL_TAG: u64 = 1 << 63;
const CIDX_SHIFT: u64 = 44;
const VAL_MASK: u64 = (1 << CIDX_SHIFT) - 1;

/// A derived container's materialized value set.
struct Container {
    name: String,
    /// The distinct values, in first-appearance order (value index
    /// `v` is `values[v]`, and its node carries value index `v+1`;
    /// `0` names the container node itself).
    values: Vec<Value>,
    /// value string → value index (0-based into `values`).
    by_str: HashMap<String, usize>,
}

/// The reference and edge fabric, built together on first use: it
/// needs the containers materialized and one pass over each scope.
struct Fabric {
    /// (base node, field) → derived value node it resolves to.
    resolve: HashMap<(NodeId, String), NodeId>,
    /// derived value node → base nodes pointing at it, with the
    /// field label (the backlink of a declared ref).
    ref_back: HashMap<NodeId, Vec<(String, NodeId)>>,
    /// base node → its outgoing declared refs (label, target node).
    ref_fwd: HashMap<NodeId, Vec<(String, NodeId)>>,
    /// derived value node → container-labeled neighbours (the pair
    /// edges; parallel edges collapsed, undirected so stored both
    /// ways).
    edges: HashMap<NodeId, Vec<(String, NodeId)>>,
}

/// A base arbor enriched with a model's derived structure.
pub struct ModelAdapter<A: AstAdapter> {
    base: A,
    model: Model,
    containers: OnceCell<Vec<Container>>,
    fabric: OnceCell<Fabric>,
}

impl<A: AstAdapter> ModelAdapter<A> {
    pub fn new(base: A, model: Model) -> Self {
        ModelAdapter {
            base,
            model,
            containers: OnceCell::new(),
            fabric: OnceCell::new(),
        }
    }

    pub fn base(&self) -> &A {
        &self.base
    }

    /// A human-readable locator, composing the base's own renderer
    /// for base nodes: `/container/value` for derived nodes.
    pub fn locator(&self, node: NodeId, base_locator: impl Fn(NodeId) -> String) -> String {
        match self.decode(node) {
            None => base_locator(node),
            Some((c, 0)) => format!("/{}", self.containers()[c].name),
            Some((c, v)) => {
                let cont = &self.containers()[c];
                format!("/{}/{}", cont.name, cont.values[v - 1])
            }
        }
    }

    fn seeded_defs(&self) -> quarb::Defs {
        quarb::parse_defs(&self.model.defs_text).unwrap_or_default()
    }

    /// The derived containers, materialized on first touch by running
    /// each constructor over the base. A later constructor sees the
    /// containers declared before it (chaining), so this fills the
    /// vector incrementally.
    fn containers(&self) -> &[Container] {
        self.containers.get_or_init(|| {
            let defs = self.seeded_defs();
            let mut built: Vec<Container> = Vec::new();
            for decl in &self.model.nodes {
                let values = self.distinct_values(&decl.query, &defs, &built);
                let by_str = values
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (v.to_string(), i))
                    .collect();
                built.push(Container {
                    name: decl.name.clone(),
                    values,
                    by_str,
                });
            }
            built
        })
    }

    /// Run one constructor query and collect its distinct values, in
    /// first-appearance order, string-keyed. `prior` is the
    /// containers already built (so a constructor may navigate an
    /// earlier derived container) — exposed by wrapping the base in a
    /// partial [`ModelAdapter`] over those.
    fn distinct_values(
        &self,
        query: &str,
        defs: &quarb::Defs,
        prior: &[Container],
    ) -> Vec<Value> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        let mut push = |vs: Vec<Value>| {
            for v in vs {
                if seen.insert(v.to_string()) {
                    out.push(v);
                }
            }
        };
        // A constructor over the base alone is the common case; one
        // that reaches an earlier derived container runs against a
        // scratch enrichment holding just those.
        let result = if prior.is_empty() {
            quarb::run_with_defs(query, defs, &self.base)
        } else {
            let scratch = PriorView {
                base: &self.base,
                prior,
            };
            quarb::run_with_defs(query, defs, &scratch)
        };
        match result {
            Ok(QueryResult::Values(vs)) => push(vs),
            Ok(QueryResult::Nodes(ns)) => {
                // A node result contributes each node's name as a
                // value (a bare `/container/*` constructor).
                let named = ns
                    .into_iter()
                    .filter_map(|n| self.base.name(n).map(Value::Str))
                    .collect();
                push(named)
            }
            Err(_) => {}
        }
        out
    }

    fn container_node(c: usize) -> NodeId {
        NodeId(MODEL_TAG | (c as u64) << CIDX_SHIFT)
    }

    fn value_node(c: usize, v: usize) -> NodeId {
        NodeId(MODEL_TAG | (c as u64) << CIDX_SHIFT | (v as u64 + 1))
    }

    /// Decode a derived node into `(container, value-slot)` where
    /// slot 0 is the container node and slot `v` is value `v-1`.
    /// `None` for a base node.
    fn decode(&self, node: NodeId) -> Option<(usize, usize)> {
        if node.0 & MODEL_TAG == 0 {
            return None;
        }
        let c = ((node.0 & !MODEL_TAG) >> CIDX_SHIFT) as usize;
        let v = (node.0 & VAL_MASK) as usize;
        (c < self.containers().len()).then_some((c, v))
    }

    fn container_by_name(&self, name: &str) -> Option<usize> {
        self.containers().iter().position(|c| c.name == name)
    }

    /// The value node in `container` whose string equals `value`.
    fn find_value(&self, container: usize, value: &Value) -> Option<NodeId> {
        let idx = *self.containers()[container].by_str.get(&value.to_string())?;
        Some(Self::value_node(container, idx))
    }

    /// The reference and edge fabric, built on first use.
    fn fabric(&self) -> &Fabric {
        self.fabric.get_or_init(|| {
            let defs = self.seeded_defs();
            let mut f = Fabric {
                resolve: HashMap::new(),
                ref_back: HashMap::new(),
                ref_fwd: HashMap::new(),
                edges: HashMap::new(),
            };
            // References: for each scoped base node, resolve its
            // field value into the target container.
            for decl in &self.model.refs {
                let Some(container) = self.container_by_name(&decl.container) else {
                    continue;
                };
                for node in self.scope_nodes(&decl.scope, &defs) {
                    let Some(value) = self.base.property(node, &decl.field) else {
                        continue;
                    };
                    if matches!(value, Value::Null) {
                        continue;
                    }
                    let Some(target) = self.find_value(container, &value) else {
                        continue;
                    };
                    f.resolve.insert((node, decl.field.clone()), target);
                    f.ref_fwd
                        .entry(node)
                        .or_default()
                        .push((decl.field.clone(), target));
                    f.ref_back
                        .entry(target)
                        .or_default()
                        .push((decl.field.clone(), node));
                }
            }
            // Edges: read the two fields per scoped node, connect the
            // two derived value nodes, labelled by the container each
            // reaches; collapse parallel edges.
            for decl in &self.model.edges {
                let ca = self.field_container(&decl.field_a);
                let cb = self.field_container(&decl.field_b);
                let (Some((ca, la)), Some((cb, lb))) = (ca, cb) else {
                    continue;
                };
                let mut seen = std::collections::HashSet::new();
                for node in self.scope_nodes(&decl.scope, &defs) {
                    let (Some(va), Some(vb)) = (
                        self.base.property(node, &decl.field_a),
                        self.base.property(node, &decl.field_b),
                    ) else {
                        continue;
                    };
                    if matches!(va, Value::Null) || matches!(vb, Value::Null) {
                        continue;
                    }
                    let (Some(na), Some(nb)) =
                        (self.find_value(ca, &va), self.find_value(cb, &vb))
                    else {
                        continue;
                    };
                    if seen.insert((na, nb)) {
                        f.edges.entry(na).or_default().push((lb.clone(), nb));
                        f.edges.entry(nb).or_default().push((la.clone(), na));
                    }
                }
            }
            f
        })
    }

    /// The container a `ref`-declared field points into, and the
    /// container's name (the edge label reaching it).
    fn field_container(&self, field: &str) -> Option<(usize, String)> {
        let decl = self.model.refs.iter().find(|r| r.field == field)?;
        let c = self.container_by_name(&decl.container)?;
        Some((c, decl.container.clone()))
    }

    /// The base nodes selected by a scope path.
    fn scope_nodes(&self, scope: &str, defs: &quarb::Defs) -> Vec<NodeId> {
        match quarb::run_with_defs(scope, defs, &self.base) {
            Ok(QueryResult::Nodes(ns)) => ns,
            _ => Vec::new(),
        }
    }
}

/// A read-only enrichment exposing just the already-built prior
/// containers over the base — the view a chaining `node` constructor
/// navigates. It never triggers further construction.
struct PriorView<'a, A: AstAdapter> {
    base: &'a A,
    prior: &'a [Container],
}

impl<A: AstAdapter> AstAdapter for PriorView<'_, A> {
    fn root(&self) -> NodeId {
        self.base.root()
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if node.0 & MODEL_TAG != 0 {
            let c = ((node.0 & !MODEL_TAG) >> CIDX_SHIFT) as usize;
            let v = (node.0 & VAL_MASK) as usize;
            if v == 0 && c < self.prior.len() {
                return (0..self.prior[c].values.len())
                    .map(|i| NodeId(MODEL_TAG | (c as u64) << CIDX_SHIFT | (i as u64 + 1)))
                    .collect();
            }
            return Vec::new();
        }
        let mut kids = self.base.children(node);
        if node == self.base.root() {
            for c in 0..self.prior.len() {
                kids.push(NodeId(MODEL_TAG | (c as u64) << CIDX_SHIFT));
            }
        }
        kids
    }
    fn name(&self, node: NodeId) -> Option<String> {
        if node.0 & MODEL_TAG != 0 {
            let c = ((node.0 & !MODEL_TAG) >> CIDX_SHIFT) as usize;
            let v = (node.0 & VAL_MASK) as usize;
            let cont = self.prior.get(c)?;
            return Some(if v == 0 {
                cont.name.clone()
            } else {
                cont.values[v - 1].to_string()
            });
        }
        self.base.name(node)
    }
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        if node.0 & MODEL_TAG != 0 {
            return None;
        }
        self.base.property(node, name)
    }
    fn default_value(&self, node: NodeId) -> Option<Value> {
        if node.0 & MODEL_TAG != 0 {
            let c = ((node.0 & !MODEL_TAG) >> CIDX_SHIFT) as usize;
            let v = (node.0 & VAL_MASK) as usize;
            return (v > 0).then(|| self.prior.get(c).map(|k| k.values[v - 1].clone()))?;
        }
        self.base.default_value(node)
    }
}

impl<A: AstAdapter> AstAdapter for ModelAdapter<A> {
    fn root(&self) -> NodeId {
        self.base.root()
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        match self.decode(node) {
            // A container node's children are its value nodes.
            Some((c, 0)) => (0..self.containers()[c].values.len())
                .map(|v| Self::value_node(c, v))
                .collect(),
            // A value node is a leaf.
            Some(_) => Vec::new(),
            // A base node: its own children, plus (at the root) the
            // derived containers as new siblings.
            None => {
                let mut kids = self.base.children(node);
                if node == self.base.root() {
                    for c in 0..self.containers().len() {
                        kids.push(Self::container_node(c));
                    }
                }
                kids
            }
        }
    }

    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        // The root's fast path must see the derived containers too.
        if node == self.base.root() {
            let mut out = self.base.children_named(node, name);
            if let Some(c) = self.container_by_name(name) {
                out.push(Self::container_node(c));
            }
            return out;
        }
        match self.decode(node) {
            Some((c, 0)) => {
                let cont = &self.containers()[c];
                cont.by_str
                    .get(name)
                    .map(|&v| vec![Self::value_node(c, v)])
                    .unwrap_or_default()
            }
            Some(_) => Vec::new(),
            None => self.base.children_named(node, name),
        }
    }

    fn name(&self, node: NodeId) -> Option<String> {
        match self.decode(node) {
            Some((c, 0)) => Some(self.containers()[c].name.clone()),
            Some((c, v)) => Some(self.containers()[c].values[v - 1].to_string()),
            None => self.base.name(node),
        }
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        match self.decode(node) {
            Some((_, 0)) => Some(self.base.root()),
            Some((c, _)) => Some(Self::container_node(c)),
            None => self.base.parent(node),
        }
    }

    fn traits(&self, node: NodeId) -> Vec<String> {
        match self.decode(node) {
            // Each derived value node carries its container's trait,
            // so mixed-type walk results self-describe (`[<ip>]`).
            Some((c, v)) if v > 0 => vec![singular(&self.containers()[c].name)],
            Some(_) => Vec::new(),
            None => self.base.traits(node),
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        match self.decode(node) {
            // A value node's own value answers `::id` (and any
            // name), so a derived node prints like a row with one
            // column.
            Some((c, v)) if v > 0 => Some(self.containers()[c].values[v - 1].clone()),
            Some(_) => None,
            None => self.base.property(node, name),
        }
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        match self.decode(node) {
            Some((c, v)) if v > 0 => Some(self.containers()[c].values[v - 1].clone()),
            Some(_) => None,
            None => self.base.default_value(node),
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match self.decode(node) {
            Some((c, 0)) if key == "n-rows" => {
                Some(Value::Int(self.containers()[c].values.len() as i64))
            }
            Some(_) => None,
            None => self.base.metadata(node, key),
        }
    }

    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        if self.decode(node).is_none() {
            if let Some(&target) = self.fabric().resolve.get(&(node, property.to_string())) {
                return Some(target);
            }
        }
        self.base.resolve(node, property, hint)
    }

    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        match self.decode(node) {
            // A value node's crosslinks are its pair edges.
            Some((_, v)) if v > 0 => {
                self.fabric().edges.get(&node).cloned().unwrap_or_default()
            }
            Some(_) => Vec::new(),
            // A base node: its own links plus any declared refs.
            None => {
                let mut out = self.base.links(node);
                if let Some(refs) = self.fabric().ref_fwd.get(&node) {
                    out.extend(refs.iter().cloned());
                }
                out
            }
        }
    }

    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        match self.decode(node) {
            // A value node: the base nodes whose declared ref points
            // here, plus its pair edges (undirected).
            Some((_, v)) if v > 0 => {
                let mut out = self.fabric().ref_back.get(&node).cloned().unwrap_or_default();
                if let Some(e) = self.fabric().edges.get(&node) {
                    out.extend(e.iter().cloned());
                }
                out
            }
            Some(_) => Vec::new(),
            None => self.base.backlinks(node),
        }
    }

    fn quantifier_bound(&self) -> usize {
        self.base.quantifier_bound()
    }
    fn allow_shell(&self) -> bool {
        self.base.allow_shell()
    }
    fn invocation_instant(&self) -> Option<(i64, u32)> {
        self.base.invocation_instant()
    }
    fn unit_scale(&self, expr: &str) -> Option<(f64, String)> {
        self.base.unit_scale(expr)
    }
}

/// A container name's singular trait form: `ips` → `ip`, `cookies` →
/// `cookie` (drop a trailing `s`); other names stand as-is.
fn singular(name: &str) -> String {
    name.strip_suffix('s').unwrap_or(name).to_string()
}

/// A borrowing adapter: lets a [`ModelAdapter`] enrich an adapter a
/// caller holds by reference (behind `dyn`) without taking
/// ownership — the shape both `qua`'s `run` funnel and `quai`'s
/// per-query dispatch need, since the concrete adapter is already
/// wrapped (now-binding, shell-gating) and only borrowed there.
pub struct Borrowed<'a>(pub &'a dyn AstAdapter);

impl AstAdapter for Borrowed<'_> {
    fn root(&self) -> NodeId {
        self.0.root()
    }
    fn children(&self, n: NodeId) -> Vec<NodeId> {
        self.0.children(n)
    }
    fn name(&self, n: NodeId) -> Option<String> {
        self.0.name(n)
    }
    fn parent(&self, n: NodeId) -> Option<NodeId> {
        self.0.parent(n)
    }
    fn traits(&self, n: NodeId) -> Vec<String> {
        self.0.traits(n)
    }
    fn children_named(&self, n: NodeId, name: &str) -> Vec<NodeId> {
        self.0.children_named(n, name)
    }
    fn property(&self, n: NodeId, name: &str) -> Option<Value> {
        self.0.property(n, name)
    }
    fn default_value(&self, n: NodeId) -> Option<Value> {
        self.0.default_value(n)
    }
    fn metadata(&self, n: NodeId, key: &str) -> Option<Value> {
        self.0.metadata(n, key)
    }
    fn links(&self, n: NodeId) -> Vec<(String, NodeId)> {
        self.0.links(n)
    }
    fn backlinks(&self, n: NodeId) -> Vec<(String, NodeId)> {
        self.0.backlinks(n)
    }
    fn resolve(&self, n: NodeId, p: &str, h: Option<&str>) -> Option<NodeId> {
        self.0.resolve(n, p, h)
    }
    fn link_property(&self, s: NodeId, l: &str, t: NodeId, name: &str) -> Option<Value> {
        self.0.link_property(s, l, t, name)
    }
    fn quantifier_bound(&self) -> usize {
        self.0.quantifier_bound()
    }
    fn allow_shell(&self) -> bool {
        self.0.allow_shell()
    }
    fn invocation_instant(&self) -> Option<(i64, u32)> {
        self.0.invocation_instant()
    }
    fn unit_scale(&self, expr: &str) -> Option<(f64, String)> {
        self.0.unit_scale(expr)
    }
}
