//! Mount adapter for Quarb.
//!
//! Composes any set of adapters under one synthetic root, each as a
//! named child — so one query spans several documents, and the
//! correlation operator (`<=>`) joins *across* them:
//!
//! ```text
//! //measurements/row <=> //stations/row[::location = $*1::location]
//! ```
//!
//! The mounts may be heterogeneous (a CSV beside a JSON beside an
//! XML document), which makes cross-format joins ordinary queries.
//!
//! Node identities are kept apart by packing a mount index into the
//! high byte of the `NodeId` (each inner adapter may use ids up to
//! 2^56 - 1). The synthetic root is id 0 and unnamed; each mount's
//! root carries the mount name.

use quarb::{AstAdapter, NodeId, Value};

/// One mounted adapter: a name (the root child's name) and the
/// adapter itself.
pub struct Mount {
    pub name: String,
    pub adapter: Box<dyn AstAdapter>,
}

/// A Quarb adapter over a set of named mounts.
pub struct MountAdapter {
    mounts: Vec<Mount>,
}

// The mount index rides the high byte (bits 56–63); an inner adapter
// gets the low 56 bits. Every inner adapter — including a
// `ComposeAdapter`, whose graft tag therefore sits below bit 56, not
// at bit 63 — must keep its ids within `INNER_MASK`, or `encode`
// would spill into the mount byte and `decode` mis-route the node.
const SHIFT: u32 = 56;
const INNER_MASK: u64 = (1 << SHIFT) - 1;

impl MountAdapter {
    pub fn new(mounts: Vec<Mount>) -> Self {
        assert!(
            mounts.len() < 255,
            "at most 254 mounts (mount index rides the id's high byte)"
        );
        MountAdapter { mounts }
    }

    /// Which mount a node belongs to, with the inner id — `None` for
    /// the synthetic root.
    pub fn decode(&self, node: NodeId) -> Option<(usize, NodeId)> {
        let m = (node.0 >> SHIFT) as usize;
        if m == 0 {
            None
        } else {
            Some((m - 1, NodeId(node.0 & INNER_MASK)))
        }
    }

    /// The name of the `idx`-th mount.
    pub fn mount_name(&self, idx: usize) -> &str {
        &self.mounts[idx].name
    }

    fn encode(&self, mount: usize, inner: NodeId) -> NodeId {
        debug_assert!(inner.0 <= INNER_MASK, "inner id exceeds 2^56 - 1");
        NodeId(((mount as u64 + 1) << SHIFT) | inner.0)
    }
}

impl AstAdapter for MountAdapter {
    fn root(&self) -> NodeId {
        NodeId(0)
    }

    /// The first mount that answers wins: every adapter shares the
    /// built-in table, so the order only decides collisions between
    /// unit-aware mounts' custom namespaces.
    fn unit_scale(&self, expr: &str) -> Option<(f64, String)> {
        self.mounts.iter().find_map(|m| m.adapter.unit_scale(expr))
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        match self.decode(node) {
            None => self
                .mounts
                .iter()
                .enumerate()
                .map(|(m, mount)| self.encode(m, mount.adapter.root()))
                .collect(),
            Some((m, inner)) => self.mounts[m]
                .adapter
                .children(inner)
                .into_iter()
                .map(|c| self.encode(m, c))
                .collect(),
        }
    }

    /// Forward the inner adapter's name-addressed fast path (git's
    /// `rev-parse`, a remote tree's direct lookup), which may alias.
    /// At the synthetic root the children are the mount roots, keyed
    /// by mount name — matching what [`name`](Self::name) reports.
    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        match self.decode(node) {
            None => self
                .mounts
                .iter()
                .enumerate()
                .filter(|(_, mount)| mount.name == name)
                .map(|(m, mount)| self.encode(m, mount.adapter.root()))
                .collect(),
            Some((m, inner)) => self.mounts[m]
                .adapter
                .children_named(inner, name)
                .into_iter()
                .map(|c| self.encode(m, c))
                .collect(),
        }
    }

    fn name(&self, node: NodeId) -> Option<String> {
        let (m, inner) = self.decode(node)?;
        let mount = &self.mounts[m];
        if inner == mount.adapter.root() {
            Some(mount.name.clone())
        } else {
            mount.adapter.name(inner)
        }
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        let (m, inner) = self.decode(node)?;
        let mount = &self.mounts[m];
        if inner == mount.adapter.root() {
            Some(NodeId(0))
        } else {
            mount.adapter.parent(inner).map(|p| self.encode(m, p))
        }
    }

    fn traits(&self, node: NodeId) -> Vec<String> {
        match self.decode(node) {
            None => Vec::new(),
            Some((m, inner)) => self.mounts[m].adapter.traits(inner),
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let (m, inner) = self.decode(node)?;
        self.mounts[m].adapter.property(inner, name)
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        let (m, inner) = self.decode(node)?;
        self.mounts[m].adapter.default_value(inner)
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let (m, inner) = self.decode(node)?;
        self.mounts[m].adapter.metadata(inner, key)
    }

    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        match self.decode(node) {
            None => Vec::new(),
            Some((m, inner)) => self.mounts[m]
                .adapter
                .links(inner)
                .into_iter()
                .map(|(l, t)| (l, self.encode(m, t)))
                .collect(),
        }
    }

    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        match self.decode(node) {
            None => Vec::new(),
            Some((m, inner)) => self.mounts[m]
                .adapter
                .backlinks(inner)
                .into_iter()
                .map(|(l, s)| (l, self.encode(m, s)))
                .collect(),
        }
    }

    fn link_property(
        &self,
        source: NodeId,
        label: &str,
        target: NodeId,
        name: &str,
    ) -> Option<Value> {
        let (m, src) = self.decode(source)?;
        let (mt, tgt) = self.decode(target)?;
        // An edge lives inside a single mount; there is no cross-mount
        // relationship to carry a property.
        if m != mt {
            return None;
        }
        self.mounts[m].adapter.link_property(src, label, tgt, name)
    }

    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let (m, inner) = self.decode(node)?;
        // Resolution stays within the node's own mount.
        self.mounts[m]
            .adapter
            .resolve(inner, property, hint)
            .map(|t| self.encode(m, t))
    }

    fn quantifier_bound(&self) -> usize {
        // A query spans every mount: the most permissive inner bound
        // must not be silently cut.
        self.mounts
            .iter()
            .map(|m| m.adapter.quantifier_bound())
            .max()
            .unwrap_or(32)
    }
}

/// A shared handle to a concrete adapter, so a host can both mount
/// it (boxed) and keep using its concrete API (locators) for
/// rendering: `let a = Rc::new(adapter); Shared(a.clone())` mounts,
/// `a` stays usable.
pub struct Shared<A>(pub std::rc::Rc<A>);

impl<A: AstAdapter> AstAdapter for Shared<A> {
    fn root(&self) -> NodeId {
        self.0.root()
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.0.children(node)
    }
    fn name(&self, node: NodeId) -> Option<String> {
        self.0.name(node)
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.0.parent(node)
    }
    fn traits(&self, node: NodeId) -> Vec<String> {
        self.0.traits(node)
    }
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        self.0.property(node, name)
    }
    fn default_value(&self, node: NodeId) -> Option<Value> {
        self.0.default_value(node)
    }
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        self.0.metadata(node, key)
    }
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.0.links(node)
    }
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.0.backlinks(node)
    }
    fn link_property(
        &self,
        source: NodeId,
        label: &str,
        target: NodeId,
        name: &str,
    ) -> Option<Value> {
        self.0.link_property(source, label, target, name)
    }
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        self.0.resolve(node, property, hint)
    }
    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        self.0.children_named(node, name)
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
