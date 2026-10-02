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

pub use parse::{
    AliasDecl, AliasKind, AliasRule, EdgeDecl, Model, Mount, NodeDecl, RefDecl, RelDecl,
    parse_model, parse_model_file, resolve_mount_target,
};

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

/// One member of a container: the two jobs a `node` constructor
/// does, kept apart because they answer differently to "what does
/// this node contain?".
#[derive(Clone)]
enum Member {
    /// A constructor yielding *values* elevates them: each distinct
    /// scalar becomes a node whose default projection is that value.
    Value(Value),
    /// A constructor yielding *nodes* aliases them: an existing node
    /// given a container and a role, creating nothing. Such a node
    /// holds everything its source holds, because it *is* the
    /// source under a role.
    Node(NodeId),
}

/// A derived container's materialized member set. A container
/// declared by several `node` statements holds several roles: each
/// statement appends its members under its own role, so
/// `/tables/class` and `/tables/schema` share one container.
struct Container {
    name: String,
    /// The members, in first-appearance order across the statements
    /// that fill the container (member index `i` is member `i`, and
    /// its node carries slot `i+1`; slot `0` names the container
    /// node itself).
    members: Vec<Member>,
    /// Per member: the role it plays — `ip` in `/ips/ip` — which
    /// names the child *and* labels every hop that lands on it, and
    /// the trait it carries, the role unless declared.
    roles: Vec<(String, String)>,
    /// member key string → member index, over the whole container;
    /// on a collision across roles the earlier statement wins.
    by_str: HashMap<String, usize>,
    /// role → (member key string → member index).
    by_role: HashMap<String, HashMap<String, usize>>,
}

impl Container {
    /// The member in node slot `v` (slot 0 is the container itself).
    fn member(&self, v: usize) -> Option<Member> {
        if v == 0 {
            return None;
        }
        self.members.get(v - 1).cloned()
    }

    /// The role of the member in slot `v` (`v >= 1`).
    fn role_of(&self, v: usize) -> &str {
        &self.roles[v - 1].0
    }

    /// The trait of the member in slot `v` (`v >= 1`).
    fn trait_of(&self, v: usize) -> &str {
        &self.roles[v - 1].1
    }

    /// The 1-based position of slot `v` among the members of its
    /// own role, for locators.
    fn ordinal_in_role(&self, v: usize) -> usize {
        let role = &self.roles[v - 1].0;
        self.roles[..v - 1]
            .iter()
            .filter(|(r, _)| r == role)
            .count()
            + 1
    }

    /// The member indexes of one role, in order.
    fn of_role<'a>(&'a self, role: &'a str) -> impl Iterator<Item = usize> + 'a {
        self.roles
            .iter()
            .enumerate()
            .filter(move |(_, (r, _))| r == role)
            .map(|(i, _)| i)
    }
}

/// The reference and edge fabric, built together on first use: it
/// needs the containers materialized and one pass over each scope.
struct Fabric {
    /// base node → the derived node aliasing it, and that
    /// container's role. A hop that would land on the raw node lands
    /// on the alias instead, so the role a model declared is the one
    /// the label uses.
    alias: HashMap<NodeId, (NodeId, String)>,
    /// (base node, field) → derived value node it resolves to.
    resolve: HashMap<(NodeId, String), NodeId>,
    /// The reverse of `resolve` and of every further element of a
    /// list-valued field: per (target, field), the source nodes —
    /// each once, the alias when a `node` gave the source a role.
    sources: HashMap<(NodeId, String), Vec<NodeId>>,
    /// derived value node → base nodes pointing at it, with the
    /// field label (the backlink of a declared ref).
    ref_back: HashMap<NodeId, Vec<(String, NodeId)>>,
    /// base node → its outgoing declared refs (label, target node).
    ref_fwd: HashMap<NodeId, Vec<(String, NodeId)>>,
    /// source node → the nodes a declared `rel` relates it to, and
    /// the reverse. One declaration serves both directions: an edge
    /// is one thing, and both ends can walk it.
    rel_fwd: HashMap<NodeId, Vec<(String, NodeId)>>,
    rel_back: HashMap<NodeId, Vec<(String, NodeId)>>,
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
                // Children share a role name, so the locator carries
                // a position within the role, as a CSV row's does.
                let cont = &self.containers()[c];
                format!(
                    "/{}/{}[{}]",
                    cont.name,
                    cont.role_of(v),
                    cont.ordinal_in_role(v)
                )
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
                let members = self.members(&decl.query, &defs, &built);
                // A second statement naming an existing container
                // fills it further under its own role.
                let idx = match built.iter().position(|c| c.name == decl.name) {
                    Some(i) => i,
                    None => {
                        built.push(Container {
                            name: decl.name.clone(),
                            members: Vec::new(),
                            roles: Vec::new(),
                            by_str: HashMap::new(),
                            by_role: HashMap::new(),
                        });
                        built.len() - 1
                    }
                };
                // Keys: an elevated value by its rendering; an aliased
                // node by what it projects, so a `ref` into the
                // container resolves against the same thing `::`
                // would show.
                let keyed: Vec<(Member, Option<String>)> = members
                    .into_iter()
                    .map(|m| {
                        let key = match &m {
                            Member::Value(v) => Some(v.to_string()),
                            Member::Node(n) => self.base_key(*n),
                        };
                        (m, key)
                    })
                    .collect();
                let cont = &mut built[idx];
                // a role declared twice adds only members it does not
                // hold: an elevated value by its rendering, an aliased
                // node by its identity (rows of a table share a name,
                // so their key is no identity)
                let held_nodes: std::collections::HashSet<NodeId> = cont
                    .members
                    .iter()
                    .zip(&cont.roles)
                    .filter(|(_, (r, _))| *r == decl.role)
                    .filter_map(|(m, _)| match m {
                        Member::Node(n) => Some(*n),
                        Member::Value(_) => None,
                    })
                    .collect();
                let by_role = cont.by_role.entry(decl.role.clone()).or_default();
                for (m, key) in keyed {
                    let held = match &m {
                        Member::Node(n) => held_nodes.contains(n),
                        Member::Value(_) => key.as_deref().is_some_and(|k| by_role.contains_key(k)),
                    };
                    if held {
                        continue;
                    }
                    let i = cont.members.len();
                    cont.members.push(m);
                    cont.roles
                        .push((decl.role.clone(), decl.trait_name.clone()));
                    if let Some(k) = key {
                        by_role.entry(k.clone()).or_insert(i);
                        cont.by_str.entry(k).or_insert(i);
                    }
                }
            }
            built
        })
    }

    /// Run one constructor query and collect its members, in
    /// first-appearance order: distinct values if it projects,
    /// aliased nodes if it navigates. `prior` is the containers
    /// already built (so a constructor may navigate an earlier
    /// derived container) — exposed by wrapping the base in a partial
    /// [`ModelAdapter`] over those.
    fn members(&self, query: &str, defs: &quarb::Defs, prior: &[Container]) -> Vec<Member> {
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
            Ok(QueryResult::Values(vs)) => {
                let mut seen = std::collections::HashSet::new();
                vs.into_iter()
                    .filter(|v| seen.insert(v.to_string()))
                    .map(Member::Value)
                    .collect()
            }
            Ok(QueryResult::Nodes(ns)) => {
                let mut seen = std::collections::HashSet::new();
                ns.into_iter()
                    .filter(|n| seen.insert(*n))
                    .map(Member::Node)
                    .collect()
            }
            Err(_) => Vec::new(),
        }
    }

    /// What a node projects, as a string — the key an aliased member
    /// is found by.
    fn base_key(&self, node: NodeId) -> Option<String> {
        self.base
            .default_value(node)
            .map(|v| v.to_string())
            .or_else(|| self.base.name(node))
    }

    /// The base node an aliased member stands for, if it is one.
    fn aliased(&self, node: NodeId) -> Option<NodeId> {
        match self.decode(node) {
            Some((c, v)) if v > 0 => match self.containers()[c].member(v)? {
                Member::Node(n) => Some(n),
                Member::Value(_) => None,
            },
            _ => None,
        }
    }

    /// The role of a derived member node, the label a hop landing
    /// on it carries.
    fn role_at(&self, node: NodeId) -> Option<String> {
        match self.decode(node) {
            Some((c, v)) if v > 0 => Some(self.containers()[c].role_of(v).to_string()),
            _ => None,
        }
    }

    /// An elevated member's value, if the node is one.
    fn elevated(&self, node: NodeId) -> Option<Value> {
        match self.decode(node) {
            Some((c, v)) if v > 0 => match self.containers()[c].member(v)? {
                Member::Value(val) => Some(val),
                Member::Node(_) => None,
            },
            _ => None,
        }
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

    /// The reference and edge fabric, built on first use.
    fn fabric(&self) -> &Fabric {
        self.fabric.get_or_init(|| {
            let defs = self.seeded_defs();
            let mut f = Fabric {
                alias: HashMap::new(),
                resolve: HashMap::new(),
                sources: HashMap::new(),
                ref_back: HashMap::new(),
                ref_fwd: HashMap::new(),
                rel_fwd: HashMap::new(),
                rel_back: HashMap::new(),
                edges: HashMap::new(),
            };
            for (c, cont) in self.containers().iter().enumerate() {
                for (i, m) in cont.members.iter().enumerate() {
                    if let Member::Node(n) = m {
                        f.alias
                            .entry(*n)
                            .or_insert((Self::value_node(c, i), cont.roles[i].0.clone()));
                    }
                }
            }
            // References: for each scoped base node, resolve its
            // field value into the target container. What each
            // (node, field) reaches is kept for the edges below.
            let mut reached: HashMap<(NodeId, String), Vec<(String, NodeId)>> = HashMap::new();
            for decl in &self.model.refs {
                // The target's key index: a derived container's
                // members (within the role the path names, when it
                // names one), keyed by the explicit target property
                // or the member key; or, when no container answers to
                // the name, the nodes any path selects over the base
                // and derived view (another mount's rows), keyed by
                // the target property or their default projection.
                // The first node with a key wins a collision.
                let mut index: HashMap<String, NodeId> = HashMap::new();
                let derived = self.container_by_name(&decl.container);
                match derived {
                    Some(container) => {
                        let cont = &self.containers()[container];
                        let slots: Vec<usize> = match decl.role.as_deref() {
                            Some(role) => cont.of_role(role).map(|i| i + 1).collect(),
                            None => (1..=cont.members.len()).collect(),
                        };
                        for v in slots {
                            let n = Self::value_node(container, v - 1);
                            let key = match decl.key_field.as_deref() {
                                Some(f) => self.property(n, f).map(|k| k.to_string()),
                                None => match cont.member(v) {
                                    Some(Member::Value(val)) => Some(val.to_string()),
                                    Some(Member::Node(b)) => self.base_key(b),
                                    None => None,
                                },
                            };
                            if let Some(k) = key {
                                index.entry(k).or_insert(n);
                            }
                        }
                    }
                    None => {
                        let view = PriorView {
                            base: &self.base,
                            prior: self.containers(),
                        };
                        for n in self.view_nodes(&decl.path, &defs) {
                            let key = match decl.key_field.as_deref() {
                                Some(f) => view.property(n, f).map(|k| k.to_string()),
                                None => view
                                    .default_value(n)
                                    .map(|v| v.to_string())
                                    .or_else(|| view.name(n)),
                            };
                            if let Some(k) = key {
                                index.entry(k).or_insert(n);
                            }
                        }
                    }
                }
                // A hop to a base target is labelled by the last
                // named segment of the target path (`--кузнецова`);
                // a derived target by the role of the node it lands on.
                let path_label = scope_role(&decl.path);
                // The source may be written through a role
                // (`ref /library/book::author --> …`): the role nodes
                // stand for the base rows beneath them.
                let mut scoped = self.scope_nodes(&decl.scope, &defs);
                if scoped.is_empty() {
                    scoped = self
                        .view_nodes(&decl.scope, &defs)
                        .into_iter()
                        .filter_map(|n| self.aliased(n))
                        .collect();
                }
                for node in scoped {
                    let Some(value) = self.base.property(node, &decl.field) else {
                        continue;
                    };
                    if matches!(value, Value::Null) {
                        continue;
                    }
                    // A list-valued property (a page's tags) refers
                    // to one node per element: the first element is
                    // what `-->` resolves to, every element gets its
                    // hop and its backlink.
                    let elements: Vec<Value> = match value {
                        l @ Value::List(_) => l.into_items().unwrap_or_default(),
                        other => vec![other],
                    };
                    let mut first = true;
                    for value in elements {
                        let Some(target) = index.get(&value.to_string()).copied() else {
                            continue;
                        };
                        let fwd = self.role_at(target).unwrap_or_else(|| path_label.clone());
                        // Backward the hop lands on the source node — on
                        // the alias if a `node` gave it a role, else on
                        // the raw node, named by the path that found it.
                        let (back_node, back) = match f.alias.get(&node) {
                            Some((alias, role)) => (*alias, role.clone()),
                            None => (node, scope_role(&decl.scope)),
                        };
                        if first {
                            f.resolve.insert((node, decl.field.clone()), target);
                            first = false;
                        }
                        reached
                            .entry((node, decl.field.clone()))
                            .or_default()
                            .push((fwd.clone(), target));
                        let from = f.sources.entry((target, decl.field.clone())).or_default();
                        if !from.contains(&back_node) {
                            from.push(back_node);
                        }
                        f.ref_fwd.entry(node).or_default().push((fwd, target));
                        f.ref_back
                            .entry(target)
                            .or_default()
                            .push((back, back_node));
                    }
                }
            }
            // Relations: a condition evaluated per pair, with `$$`
            // standing for the source node. Resolution is the special
            // case where the condition is fixed (`the target whose
            // identity is this value`) and so can go unwritten; a
            // `rel` says its own, which is why it may hold for many
            // targets and fork threads like any other hop.
            for decl in &self.model.rels {
                let fwd = scope_role(&decl.target);
                let back = scope_role(&decl.source);
                for source in self.view_nodes(&decl.source, &defs) {
                    let cond = self.bind_driver(&decl.cond, source);
                    let query = format!("{}{}", decl.target, cond);
                    for target in self.view_nodes(&query, &defs) {
                        if target == source {
                            continue;
                        }
                        f.rel_fwd
                            .entry(source)
                            .or_default()
                            .push((fwd.clone(), target));
                        f.rel_back
                            .entry(target)
                            .or_default()
                            .push((back.clone(), source));
                    }
                }
            }
            // Edges: per scoped node, connect what its two fields
            // reach — the very nodes the refs resolved to, so an edge
            // follows a ref by key property, into another mount, or
            // with both ends in one role (a network of pairs) as it
            // follows one into a value container. Each end is
            // labelled by the role it lands in; parallel edges
            // collapse.
            for decl in &self.model.edges {
                let mut seen = std::collections::HashSet::new();
                for node in self.scope_nodes(&decl.scope, &defs) {
                    let (Some(ends_a), Some(ends_b)) = (
                        reached.get(&(node, decl.field_a.clone())),
                        reached.get(&(node, decl.field_b.clone())),
                    ) else {
                        continue;
                    };
                    for (ra, na) in ends_a {
                        for (rb, nb) in ends_b {
                            if na != nb && seen.insert((*na, *nb)) {
                                f.edges.entry(*na).or_default().push((rb.clone(), *nb));
                                f.edges.entry(*nb).or_default().push((ra.clone(), *na));
                            }
                        }
                    }
                }
            }
            f
        })
    }

    /// The base nodes selected by a scope path.
    fn scope_nodes(&self, scope: &str, defs: &quarb::Defs) -> Vec<NodeId> {
        match quarb::run_with_defs(scope, defs, &self.base) {
            Ok(QueryResult::Nodes(ns)) => ns,
            _ => Vec::new(),
        }
    }

    /// The nodes a path selects over the base *and* the derived
    /// containers — the view a relation's two ends navigate, since
    /// either may be derived. It never consults the fabric, so
    /// building the fabric cannot recurse into itself.
    fn view_nodes(&self, path: &str, defs: &quarb::Defs) -> Vec<NodeId> {
        let view = PriorView {
            base: &self.base,
            prior: self.containers(),
        };
        match quarb::run_with_defs(path, defs, &view) {
            Ok(QueryResult::Nodes(ns)) => ns,
            _ => Vec::new(),
        }
    }

    /// Substitute the driver operand in a relation's condition:
    /// `_::field` becomes that property of the source node, bare
    /// `$$_` its default projection (`$$` is the heritage spelling). Text inside string literals is
    /// left alone.
    fn bind_driver(&self, cond: &str, source: NodeId) -> String {
        let view = PriorView {
            base: &self.base,
            prior: self.containers(),
        };
        let b: Vec<char> = cond.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                q @ ('\'' | '"') => {
                    out.push(q);
                    i += 1;
                    while i < b.len() && b[i] != q {
                        if b[i] == '\\' && i + 1 < b.len() {
                            out.push(b[i]);
                            i += 1;
                        }
                        out.push(b[i]);
                        i += 1;
                    }
                    if i < b.len() {
                        out.push(b[i]);
                        i += 1;
                    }
                }
                // `_` — the served node (the relation's source); `$$_`
                // and the bare `$$` are its heritage spellings.
                c @ ('$' | '_')
                    if (c == '$' && b.get(i + 1) == Some(&'$'))
                        || (c == '_'
                            && !(i > 0
                                && (b[i - 1].is_alphanumeric()
                                    || b[i - 1] == '_'
                                    || b[i - 1] == '-'))
                            && !b.get(i + 1).is_some_and(|c| {
                                c.is_alphanumeric() || *c == '_' || *c == '-'
                            })) =>
                {
                    if c == '$' {
                        i += 2;
                        if b.get(i) == Some(&'_')
                            && !b
                                .get(i + 1)
                                .is_some_and(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                        {
                            i += 1;
                        }
                    } else {
                        i += 1;
                    }
                    let value = if b.get(i) == Some(&':') && b.get(i + 1) == Some(&':') {
                        i += 2;
                        let start = i;
                        while i < b.len() && (b[i].is_alphanumeric() || b[i] == '_' || b[i] == '-')
                        {
                            i += 1;
                        }
                        let field: String = b[start..i].iter().collect();
                        view.property(source, &field)
                    } else {
                        view.default_value(source)
                    };
                    out.push_str(&literal(value.unwrap_or(Value::Null)));
                }
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        out
    }
}

/// Render a value as query-source text, so a bound driver operand
/// reads back as the literal it stands for.
fn literal(v: Value) -> String {
    match v {
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "''".to_string(),
        other => format!(
            "'{}'",
            other.to_string().replace('\\', "\\\\").replace('\'', "\\'")
        ),
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
                return (0..self.prior[c].members.len())
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
            // A derived child is named for its role, as in the full
            // adapter — the value stays on `::`.
            return Some(if v == 0 {
                cont.name.clone()
            } else {
                cont.role_of(v).to_string()
            });
        }
        self.base.name(node)
    }
    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        if node == self.base.root() {
            let mut out = self.base.children_named(node, name);
            if let Some(c) = self.prior.iter().position(|k| k.name == name) {
                out.push(NodeId(MODEL_TAG | (c as u64) << CIDX_SHIFT));
            }
            return out;
        }
        if node.0 & MODEL_TAG != 0 {
            let c = ((node.0 & !MODEL_TAG) >> CIDX_SHIFT) as usize;
            let v = (node.0 & VAL_MASK) as usize;
            let Some(cont) = self.prior.get(c) else {
                return Vec::new();
            };
            if v == 0 {
                return cont
                    .of_role(name)
                    .map(|i| NodeId(MODEL_TAG | (c as u64) << CIDX_SHIFT | (i as u64 + 1)))
                    .collect();
            }
            return Vec::new();
        }
        self.base.children_named(node, name)
    }
    fn traits(&self, node: NodeId) -> Vec<String> {
        if node.0 & MODEL_TAG != 0 {
            let c = ((node.0 & !MODEL_TAG) >> CIDX_SHIFT) as usize;
            let v = (node.0 & VAL_MASK) as usize;
            return match self.prior.get(c) {
                Some(cont) if v > 0 => vec![cont.trait_of(v).to_string()],
                _ => Vec::new(),
            };
        }
        self.base.traits(node)
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        if node.0 & MODEL_TAG != 0 {
            let c = ((node.0 & !MODEL_TAG) >> CIDX_SHIFT) as usize;
            let v = (node.0 & VAL_MASK) as usize;
            return Some(if v == 0 {
                self.base.root()
            } else {
                NodeId(MODEL_TAG | (c as u64) << CIDX_SHIFT)
            });
        }
        self.base.parent(node)
    }
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        if node.0 & MODEL_TAG != 0 {
            let c = ((node.0 & !MODEL_TAG) >> CIDX_SHIFT) as usize;
            let v = (node.0 & VAL_MASK) as usize;
            return match self.prior.get(c)?.member(v)? {
                // elevated members have no named properties — the
                // value lives on the bare projection only
                Member::Value(_) => None,
                Member::Node(n) => self.base.property(n, name),
            };
        }
        self.base.property(node, name)
    }
    fn default_value(&self, node: NodeId) -> Option<Value> {
        if node.0 & MODEL_TAG != 0 {
            let c = ((node.0 & !MODEL_TAG) >> CIDX_SHIFT) as usize;
            let v = (node.0 & VAL_MASK) as usize;
            return match self.prior.get(c)?.member(v)? {
                Member::Value(val) => Some(val),
                Member::Node(n) => self.base.default_value(n),
            };
        }
        self.base.default_value(node)
    }
    // Minimal on purpose: base nodes forward; prior-container nodes
    // answer default (constructor queries never project
    // `:::provenance` mid-derivation).
    fn document_by_ref(&self, id: &str) -> Option<NodeId> {
        self.base.document_by_ref(id)
    }
    fn descendants_named(&self, node: NodeId, name: &str) -> Option<Vec<(NodeId, usize)>> {
        if node.0 & MODEL_TAG != 0 || node == self.base.root() {
            return None;
        }
        self.base.descendants_named(node, name)
    }
    fn prefetch_links(&self, nodes: &[NodeId], dir: quarb::LinkDir) {
        let base: Vec<NodeId> = nodes
            .iter()
            .copied()
            .filter(|n| n.0 & MODEL_TAG == 0)
            .collect();
        if !base.is_empty() {
            self.base.prefetch_links(&base, dir);
        }
    }
    fn provenance(&self, node: NodeId) -> quarb::Provenance {
        if node.0 & MODEL_TAG != 0 {
            return quarb::Provenance::default();
        }
        self.base.provenance(node)
    }
    fn provenance_list(&self, node: NodeId) -> quarb::ProvenanceList {
        if node.0 & MODEL_TAG != 0 {
            return quarb::ProvenanceList::default();
        }
        self.base.provenance_list(node)
    }
}

impl<A: AstAdapter> AstAdapter for ModelAdapter<A> {
    fn root(&self) -> NodeId {
        self.base.root()
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        match self.decode(node) {
            // A container node's children are its value nodes.
            Some((c, 0)) => (0..self.containers()[c].members.len())
                .map(|v| Self::value_node(c, v))
                .collect(),
            // An elevated node is a leaf; an aliased one has the
            // children of the node it stands for.
            Some(_) => match self.aliased(node) {
                Some(base) => self.base.children(base),
                None => Vec::new(),
            },
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
            // Every child answers to the role; none answers to its
            // value (that is a predicate's job, not a name's).
            Some((c, 0)) => self.containers()[c]
                .of_role(name)
                .map(|i| Self::value_node(c, i))
                .collect(),
            Some(_) => match self.aliased(node) {
                Some(base) => self.base.children_named(base, name),
                None => Vec::new(),
            },
            None => self.base.children_named(node, name),
        }
    }

    fn name(&self, node: NodeId) -> Option<String> {
        match self.decode(node) {
            Some((c, 0)) => Some(self.containers()[c].name.clone()),
            // A derived node is named for its role, not its value:
            // a name says what a node *is* where you found it. The
            // value stays in the value space, on `::`.
            Some((c, v)) => Some(self.containers()[c].role_of(v).to_string()),
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
            Some((c, v)) if v > 0 => {
                let mut out = vec![self.containers()[c].trait_of(v).to_string()];
                // An aliased node is the source wearing a role, so it
                // keeps the traits the source already carried.
                if let Some(base) = self.aliased(node) {
                    out.extend(self.base.traits(base));
                }
                out
            }
            Some(_) => Vec::new(),
            None => self.base.traits(node),
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        match self.decode(node) {
            // An aliased node answers with the source's own
            // properties. An elevated node has exactly one datum —
            // its value, on the *bare* projection (`::`) — and no
            // named properties: answering any name with the value
            // would let `::cookie` on an ip node return the ip.
            Some((_, v)) if v > 0 => match self.aliased(node) {
                Some(base) => self.base.property(base, name),
                None => None,
            },
            Some(_) => None,
            None => self.base.property(node, name),
        }
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        match self.decode(node) {
            Some((_, v)) if v > 0 => match self.aliased(node) {
                Some(base) => self.base.default_value(base),
                None => self.elevated(node),
            },
            Some(_) => None,
            None => self.base.default_value(node),
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match self.decode(node) {
            Some((c, 0)) if key == "n-rows" => {
                Some(Value::Int(self.containers()[c].members.len() as i64))
            }
            Some(_) => None,
            None => self.base.metadata(node, key),
        }
    }

    /// Model nodes add their own annotations (`n-rows`); base nodes
    /// keep the mounted adapter's, aliases included.
    fn aliased_metadata(&self, node: NodeId) -> &'static [&'static str] {
        self.base.aliased_metadata(node)
    }

    /// Data provenance. An aliased node *is* its base node under a
    /// role, so it answers that node's provenance. An elevated node
    /// is a new value the model made: it answers a derivation source
    /// only (`model:/container`), never the components of any row it
    /// was computed from — the same no-leak discipline `property`
    /// applies to elevated values.
    fn provenance(&self, node: NodeId) -> quarb::Provenance {
        match self.decode(node) {
            Some((c, _)) => match self.aliased(node) {
                Some(base) => self.base.provenance(base),
                None => quarb::Provenance {
                    source: Some(format!("model:/{}", self.containers()[c].name)),
                    ..Default::default()
                },
            },
            None => self.base.provenance(node),
        }
    }
    fn provenance_list(&self, node: NodeId) -> quarb::ProvenanceList {
        match self.decode(node) {
            Some((_, _)) => match self.aliased(node) {
                Some(base) => self.base.provenance_list(base),
                None => quarb::ProvenanceList::single(self.provenance(node)),
            },
            None => self.base.provenance_list(node),
        }
    }

    /// A written name reaches a node under its canonical name or
    /// any `alias` the model declares for names.
    fn answers_to(&self, node: NodeId, name: &str) -> bool {
        if self.decode(node).is_none() && self.base.answers_to(node, name) {
            return true;
        }
        let Some(canonical) = self.name(node) else {
            return canonical_none(name);
        };
        if canonical == name {
            return true;
        }
        let aliases = self
            .model
            .aliases
            .iter()
            .filter(|a| a.kind == AliasKind::Name);
        for a in aliases {
            if a.admits(name, &canonical) {
                return true;
            }
            // An exact alias rewrites the written name; a base node
            // then answers under its own rules (a kind name, a stem,
            // a title), not only under its canonical spelling.
            if self.decode(node).is_none()
                && let AliasRule::Exact { alias, original } = &a.rule
                && alias == name
                && self.base.answers_to(node, original)
            {
                return true;
            }
        }
        false
    }

    /// A written trait reaches a node under a canonical trait or any
    /// `alias <...>` the model declares.
    fn has_trait(&self, node: NodeId, name: &str) -> bool {
        let traits = self.traits(node);
        if traits.iter().any(|t| t == name) {
            return true;
        }
        self.model
            .aliases
            .iter()
            .filter(|a| a.kind == AliasKind::Trait)
            .any(|a| traits.iter().any(|t| a.admits(name, t)))
    }

    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        // An aliased node resolves as the node it stands for.
        let under = self.aliased(node).unwrap_or(node);
        if let Some(&target) = self.fabric().resolve.get(&(under, property.to_string())) {
            return Some(target);
        }
        if self.decode(under).is_none() {
            return self.base.resolve(under, property, hint);
        }
        // A purely derived node (a class container, an elevated
        // value) has no base identity to resolve through — and base
        // adapters index by their own ids, so a tagged id must never
        // reach them.
        None
    }

    fn external_ref(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<String> {
        // An aliased node references as the node it stands for; a
        // base id passes straight through, and a purely derived
        // node (tagged id) references nothing external.
        let under = self.aliased(node).unwrap_or(node);
        if self.decode(under).is_none() {
            return self.base.external_ref(under, property, hint);
        }
        None
    }

    fn resolve_fragment(&self, node: NodeId, fragment: &str) -> Option<NodeId> {
        let under = self.aliased(node).unwrap_or(node);
        if self.decode(under).is_none() {
            return self.base.resolve_fragment(under, fragment);
        }
        None
    }

    fn ref_property(&self, node: NodeId) -> Option<String> {
        let under = self.aliased(node).unwrap_or(node);
        if self.decode(under).is_none() {
            return self.base.ref_property(under);
        }
        None
    }

    fn ref_label(&self, node: NodeId, property: &str) -> Option<String> {
        let under = self.aliased(node).unwrap_or(node);
        if self.decode(under).is_none() {
            return self.base.ref_label(under, property);
        }
        None
    }

    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let f = self.fabric();
        match self.decode(node) {
            // A value node's crosslinks are its pair edges and any
            // relation declared from it.
            Some((_, v)) if v > 0 => {
                let mut out = f.edges.get(&node).cloned().unwrap_or_default();
                if let Some(rels) = f.rel_fwd.get(&node) {
                    out.extend(rels.iter().cloned());
                }
                if let Some(base) = self.aliased(node) {
                    out.extend(self.base.links(base));
                    if let Some(refs) = f.ref_fwd.get(&base) {
                        out.extend(refs.iter().cloned());
                    }
                }
                out
            }
            Some(_) => Vec::new(),
            // A base node: its own links plus any declared refs and
            // relations.
            None => {
                let mut out = self.base.links(node);
                if let Some(refs) = f.ref_fwd.get(&node) {
                    out.extend(refs.iter().cloned());
                }
                if let Some(rels) = f.rel_fwd.get(&node) {
                    out.extend(rels.iter().cloned());
                }
                out
            }
        }
    }

    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let f = self.fabric();
        match self.decode(node) {
            // A value node: the base nodes whose declared ref points
            // here, its pair edges (undirected), and the far end of
            // any relation declared toward it.
            Some((_, v)) if v > 0 => {
                let mut out = f.ref_back.get(&node).cloned().unwrap_or_default();
                if let Some(e) = f.edges.get(&node) {
                    out.extend(e.iter().cloned());
                }
                if let Some(rels) = f.rel_back.get(&node) {
                    out.extend(rels.iter().cloned());
                }
                out
            }
            Some(_) => Vec::new(),
            // A base node: its own backlinks, the declared refs that
            // resolve to it (a ref may target any path, so another
            // mount's row is a target too), and the relations toward it.
            None => {
                let mut out = self.base.backlinks(node);
                if let Some(refs) = f.ref_back.get(&node) {
                    out.extend(refs.iter().cloned());
                }
                if let Some(rels) = f.rel_back.get(&node) {
                    out.extend(rels.iter().cloned());
                }
                out
            }
        }
    }

    /// The document the base holds under a URL — base ids pass
    /// straight through this layer.
    fn document_by_ref(&self, id: &str) -> Option<NodeId> {
        self.base.document_by_ref(id)
    }

    /// The base's reverse index answers only when the model adds
    /// no reference fabric of its own; otherwise the walk sees
    /// both.
    fn reverse_resolve(
        &self,
        node: NodeId,
        property: Option<&str>,
        hint: Option<&str>,
    ) -> Option<Vec<NodeId>> {
        // A field the model declares a ref for: the fabric knows its
        // sources, each once — the walk would meet a source twice,
        // as the base row and as the role node over it.
        if let Some(field) = property
            && self.model.refs.iter().any(|r| r.field == field)
        {
            let f = self.fabric();
            let target = f.alias.get(&node).map_or(node, |(alias, _)| *alias);
            let mut out = f
                .sources
                .get(&(target, field.to_string()))
                .cloned()
                .unwrap_or_default();
            if target != node
                && let Some(more) = f.sources.get(&(node, field.to_string()))
            {
                for n in more {
                    if !out.contains(n) {
                        out.push(*n);
                    }
                }
            }
            return Some(out);
        }
        if self.decode(node).is_some()
            || !self.model.refs.is_empty()
            || !self.model.rels.is_empty()
            || !self.model.edges.is_empty()
        {
            return None;
        }
        self.base.reverse_resolve(node, property, hint)
    }

    /// The base's name index answers below any base node but the
    /// root (the model's containers hang there) and only when no
    /// name alias could widen the match.
    fn descendants_named(&self, node: NodeId, name: &str) -> Option<Vec<(NodeId, usize)>> {
        if self.decode(node).is_some()
            || node == self.base.root()
            || self.model.aliases.iter().any(|a| a.kind == AliasKind::Name)
        {
            return None;
        }
        self.base.descendants_named(node, name)
    }

    fn prefetch_links(&self, nodes: &[NodeId], dir: quarb::LinkDir) {
        let base: Vec<NodeId> = nodes
            .iter()
            .copied()
            .filter(|n| self.decode(*n).is_none())
            .collect();
        if !base.is_empty() {
            self.base.prefetch_links(&base, dir);
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

/// An unnamed node answers to no written name.
fn canonical_none(_name: &str) -> bool {
    false
}

/// The role of the nodes a scope path selects: the last *named*
/// segment of the path (`/posts/*` -> `posts`, `/row` -> `row`).
/// A base node's own name cannot serve — a relational row is named
/// by its primary key, which is an identity, not a role — so the
/// path that selected it supplies the word instead.
fn scope_role(scope: &str) -> String {
    scope
        .split('/')
        .filter(|seg| {
            !seg.is_empty()
                && !seg.starts_with('*')
                && !seg.starts_with('[')
                && !seg.chars().next().is_some_and(|c| c.is_ascii_digit())
        })
        .next_back()
        .unwrap_or("")
        .split(['[', ':'])
        .next()
        .unwrap_or("")
        .to_string()
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

    /// Aliases (ruling #30) and trait aliases pass through: the
    /// wrapped adapter's word, not name equality on this layer.
    fn answers_to(&self, n: NodeId, name: &str) -> bool {
        self.0.answers_to(n, name)
    }

    fn has_trait(&self, n: NodeId, name: &str) -> bool {
        self.0.has_trait(n, name)
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
    fn external_ref(&self, n: NodeId, p: &str, h: Option<&str>) -> Option<String> {
        self.0.external_ref(n, p, h)
    }
    fn resolve_fragment(&self, n: NodeId, f: &str) -> Option<NodeId> {
        self.0.resolve_fragment(n, f)
    }
    fn ref_property(&self, n: NodeId) -> Option<String> {
        self.0.ref_property(n)
    }
    fn ref_label(&self, n: NodeId, p: &str) -> Option<String> {
        self.0.ref_label(n, p)
    }
    fn link_property(&self, s: NodeId, l: &str, t: NodeId, name: &str) -> Option<Value> {
        self.0.link_property(s, l, t, name)
    }
    fn document_by_ref(&self, id: &str) -> Option<NodeId> {
        self.0.document_by_ref(id)
    }
    fn reverse_resolve(&self, n: NodeId, p: Option<&str>, h: Option<&str>) -> Option<Vec<NodeId>> {
        self.0.reverse_resolve(n, p, h)
    }
    fn descendants_named(&self, n: NodeId, name: &str) -> Option<Vec<(NodeId, usize)>> {
        self.0.descendants_named(n, name)
    }
    fn prefetch_links(&self, nodes: &[NodeId], dir: quarb::LinkDir) {
        self.0.prefetch_links(nodes, dir)
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
    fn provenance(&self, n: NodeId) -> quarb::Provenance {
        self.0.provenance(n)
    }
    fn provenance_list(&self, n: NodeId) -> quarb::ProvenanceList {
        self.0.provenance_list(n)
    }
    fn unit_scale(&self, expr: &str) -> Option<(f64, String)> {
        self.0.unit_scale(expr)
    }
}
