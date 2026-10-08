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
    /// The target the mount was opened from (a path, a URL, a
    /// connection string), retained as the provenance layer's
    /// `:::source` fill. `None` when the host constructed the
    /// adapter without one; the mount name then stands in.
    pub target: Option<String>,
    pub adapter: Box<dyn AstAdapter>,
}

/// The identity a mount's target gives its root: a URL as itself;
/// a path made absolute, a directory's with a trailing `/` (a base
/// over its files); a reading prefix (`text:book.atd`) stripped
/// when what follows it is a path; anything else as written.
fn mount_identity(target: &str) -> Option<String> {
    let t = target.trim();
    if t.is_empty() {
        return None;
    }
    if quarb::is_url(t) {
        return Some(quarb::canonical_identity(t));
    }
    let path = match t.split_once(':') {
        Some((prefix, rest))
            if !prefix.is_empty()
                && prefix
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
                && !rest.starts_with("//")
                && std::path::Path::new(rest).exists() =>
        {
            rest
        }
        _ => t,
    };
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Some(t.to_string());
    }
    let mut identity = quarb::canonical_identity(path);
    if p.is_dir() && !identity.ends_with('/') {
        identity.push('/');
    }
    Some(identity)
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

/// How many mounts fit the id's high byte (255 is the root's).
pub const MAX_MOUNTS: usize = 254;
const INNER_MASK: u64 = (1 << SHIFT) - 1;

impl MountAdapter {
    /// Panics past [`MAX_MOUNTS`]; a host that takes its mounts from
    /// the command line uses [`MountAdapter::try_new`].
    pub fn new(mounts: Vec<Mount>) -> Self {
        Self::try_new(mounts).expect("at most 254 mounts (mount index rides the id's high byte)")
    }

    /// The adapter over `mounts`, or a refusal when there are more
    /// than [`MAX_MOUNTS`] of them (the mount index rides the
    /// node id's high byte).
    pub fn try_new(mounts: Vec<Mount>) -> Result<Self, String> {
        if mounts.len() > MAX_MOUNTS {
            return Err(format!(
                "{} mounts; at most {MAX_MOUNTS} can be mounted together",
                mounts.len()
            ));
        }
        Ok(MountAdapter { mounts })
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
    fn child_host(&self, node: NodeId) -> NodeId {
        match self.decode(node) {
            None => node,
            Some((m, inner)) => self.encode(m, self.mounts[m].adapter.child_host(inner)),
        }
    }

    fn properties(&self, node: NodeId) -> Vec<(String, Value)> {
        match self.decode(node) {
            None => Vec::new(),
            Some((m, inner)) => self.mounts[m].adapter.properties(inner),
        }
    }

    /// The walk is open when every mount opens to it.
    fn walks_for_references(&self) -> bool {
        self.mounts.iter().all(|m| m.adapter.walks_for_references())
    }
    /// Within one mount its own declarations answer; across mounts
    /// only a reference property of the source's own can reach.
    fn can_refer_to(&self, source: NodeId, target: NodeId) -> Option<bool> {
        let (m, s) = self.decode(source)?;
        match self.decode(target) {
            Some((n, t)) if n == m => self.mounts[m].adapter.can_refer_to(s, t),
            _ => self.mounts[m].adapter.ref_property(s).map(|_| true),
        }
    }
    fn reference_properties(&self, source: NodeId, target: NodeId) -> Vec<String> {
        let Some((m, s)) = self.decode(source) else {
            return Vec::new();
        };
        match self.decode(target) {
            Some((n, t)) if n == m => self.mounts[m].adapter.reference_properties(s, t),
            _ => self.mounts[m].adapter.ref_property(s).into_iter().collect(),
        }
    }
    fn resolves_references(&self) -> bool {
        self.mounts.iter().any(|m| m.adapter.resolves_references())
    }
    /// A scope inside the target's own mount (`/db/books/*`) is the
    /// mount's to answer, under its own path.
    fn index_referrers(
        &self,
        scope: &str,
        property: &str,
        label: Option<&str>,
        target: NodeId,
    ) -> Option<Vec<NodeId>> {
        let (m, inner) = self.decode(target)?;
        let rest = scope
            .strip_prefix('/')?
            .strip_prefix(self.mounts[m].name.as_str())?;
        if !rest.starts_with('/') {
            return None;
        }
        let v = self.mounts[m]
            .adapter
            .index_referrers(rest, property, label, inner)?;
        Some(v.into_iter().map(|t| self.encode(m, t)).collect())
    }
    fn reverse_label(&self, target: NodeId) -> Option<String> {
        let (m, inner) = self.decode(target)?;
        self.mounts[m].adapter.reverse_label(inner)
    }
    /// Every mount's own target is an identity of its root,
    /// canonicalized: a file's absolute path, a directory as a base
    /// over its files (its path with a trailing `/`), a URL as
    /// itself. A reading prefix (`text:`, `json2:`) is not part of
    /// the identity. Then whatever the mounts declare inside.
    fn tree_identities(&self) -> Vec<(String, NodeId)> {
        let mut out = Vec::new();
        for (m, mount) in self.mounts.iter().enumerate() {
            let root = self.encode(m, mount.adapter.root());
            if let Some(target) = mount.target.as_deref()
                && let Some(identity) = mount_identity(target)
            {
                out.push((identity, root));
            }
            out.extend(
                mount
                    .adapter
                    .tree_identities()
                    .into_iter()
                    .map(|(i, n)| (i, self.encode(m, n))),
            );
        }
        out
    }
    fn resolve_needs_label(&self, node: NodeId, property: &str) -> bool {
        self.decode(node)
            .is_some_and(|(m, inner)| self.mounts[m].adapter.resolve_needs_label(inner, property))
    }
    fn property_names(&self, node: NodeId) -> Vec<String> {
        match self.decode(node) {
            Some((m, inner)) => self.mounts[m].adapter.property_names(inner),
            None => Vec::new(),
        }
    }

    /// Every mount's non-tree nodes, mount by mount.
    fn non_tree_nodes(&self) -> Vec<NodeId> {
        self.mounts
            .iter()
            .enumerate()
            .flat_map(|(m, mount)| {
                mount
                    .adapter
                    .non_tree_nodes()
                    .into_iter()
                    .map(move |n| self.encode(m, n))
            })
            .collect()
    }

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

    /// Per node, because each mount brings its own list: a git
    /// mount aliases `short`, the JSON mount beside it aliases
    /// nothing.
    fn aliased_metadata(&self, node: NodeId) -> &'static [&'static str] {
        match self.decode(node) {
            Some((m, inner)) => self.mounts[m].adapter.aliased_metadata(inner),
            None => &[],
        }
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

    fn external_ref(
        &self,
        node: NodeId,
        property: &str,
        hint: Option<&str>,
    ) -> Option<quarb::ExternalRef> {
        // The identifier is document-external by definition — no
        // id translation, plain delegation to the node's mount.
        let (m, inner) = self.decode(node)?;
        self.mounts[m].adapter.external_ref(inner, property, hint)
    }

    fn ref_property(&self, node: NodeId) -> Option<String> {
        let (m, inner) = self.decode(node)?;
        self.mounts[m].adapter.ref_property(inner)
    }

    fn ref_label(&self, node: NodeId, property: &str) -> Option<String> {
        let (m, inner) = self.decode(node)?;
        self.mounts[m].adapter.ref_label(inner, property)
    }

    fn resolve_fragment(&self, node: NodeId, fragment: &str) -> Option<NodeId> {
        // The fragment lands within the node's own mount.
        let (m, inner) = self.decode(node)?;
        self.mounts[m]
            .adapter
            .resolve_fragment(inner, fragment)
            .map(|t| self.encode(m, t))
    }

    /// The first mount holding a document under `id` answers.
    fn document_by_ref(&self, id: &str) -> Option<NodeId> {
        self.mounts
            .iter()
            .enumerate()
            .find_map(|(m, mount)| mount.adapter.document_by_ref(id).map(|t| self.encode(m, t)))
    }

    /// A reverse index answers within the node's own mount; the
    /// other mounts cannot reference into it.
    fn reverse_resolve(
        &self,
        node: NodeId,
        property: Option<&str>,
        hint: Option<&str>,
    ) -> Option<Vec<NodeId>> {
        let (m, inner) = self.decode(node)?;
        let v = self.mounts[m]
            .adapter
            .reverse_resolve(inner, property, hint)?;
        Some(v.into_iter().map(|t| self.encode(m, t)).collect())
    }

    fn descendants_named(&self, node: NodeId, name: &str) -> Option<Vec<(NodeId, usize)>> {
        let (m, inner) = self.decode(node)?;
        let v = self.mounts[m].adapter.descendants_named(inner, name)?;
        Some(v.into_iter().map(|(t, d)| (self.encode(m, t), d)).collect())
    }

    fn descendants_where(
        &self,
        node: NodeId,
        name: &str,
        property: &str,
        value: &Value,
    ) -> Option<Vec<(NodeId, usize)>> {
        let (m, inner) = self.decode(node)?;
        let v = self.mounts[m]
            .adapter
            .descendants_where(inner, name, property, value)?;
        Some(v.into_iter().map(|(t, d)| (self.encode(m, t), d)).collect())
    }

    fn has_node(&self, node: NodeId) -> bool {
        match self.decode(node) {
            None => node == self.root(),
            Some((m, inner)) => self.mounts[m].adapter.has_node(inner),
        }
    }

    /// A mounted adapter's own word on names and traits (a web
    /// store's rows answer to their kind): inside a mount the
    /// adapter decides; the mount's root answers by its mount name.
    fn answers_to(&self, node: NodeId, name: &str) -> bool {
        match self.decode(node) {
            Some((m, inner)) if inner != self.mounts[m].adapter.root() => {
                self.mounts[m].adapter.answers_to(inner, name)
            }
            _ => self.name(node).as_deref() == Some(name),
        }
    }

    fn has_trait(&self, node: NodeId, name: &str) -> bool {
        match self.decode(node) {
            None => false,
            Some((m, inner)) => self.mounts[m].adapter.has_trait(inner, name),
        }
    }

    fn prefetch_links(&self, nodes: &[NodeId], dir: quarb::LinkDir) {
        let mut by_mount: Vec<Vec<NodeId>> = vec![Vec::new(); self.mounts.len()];
        for &n in nodes {
            if let Some((m, inner)) = self.decode(n) {
                by_mount[m].push(inner);
            }
        }
        for (m, inner) in by_mount.into_iter().enumerate() {
            if !inner.is_empty() {
                self.mounts[m].adapter.prefetch_links(&inner, dir);
            }
        }
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

    /// The mount is a provenance layer: the inner adapter answers
    /// first with what its substrate records, and the mount fills a
    /// missing `:::source` with its target (the real-world address;
    /// the query-local mount name only as a fallback).
    fn provenance(&self, node: NodeId) -> quarb::Provenance {
        match self.decode(node) {
            None => quarb::Provenance::default(),
            Some((m, inner)) => self.mounts[m]
                .adapter
                .provenance(inner)
                .or(self.source_fill(m, inner)),
        }
    }
    /// The same layering, entry by entry.
    fn provenance_list(&self, node: NodeId) -> quarb::ProvenanceList {
        match self.decode(node) {
            None => quarb::ProvenanceList::default(),
            Some((m, inner)) => self.mounts[m]
                .adapter
                .provenance_list(inner)
                .or(self.source_fill(m, inner)),
        }
    }
}

impl MountAdapter {
    /// The mount's fill: `:::source` its target (the real-world
    /// address; the query-local name as a fallback), `:::path` the
    /// node's name-path inside the mounted adapter, and — for a
    /// target that is a local file — the file's modification time
    /// as the source rung of the instant ladder.
    fn source_fill(&self, m: usize, inner: NodeId) -> quarb::Provenance {
        let mount = &self.mounts[m];
        let instant = mount
            .target
            .as_deref()
            .and_then(|t| std::fs::metadata(t).ok())
            .filter(|md| md.is_file())
            .and_then(|md| md.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| (d.as_secs() as i64, d.subsec_nanos(), Some(0)));
        quarb::Provenance {
            source: mount.target.clone().or_else(|| Some(mount.name.clone())),
            path: Some(quarb::name_path(&*mount.adapter, inner)),
            instant_from: instant.map(|_| quarb::InstantFrom::Source),
            instant,
            dpid: None,
        }
    }
}

/// A shared handle to a concrete adapter, so a host can both mount
/// it (boxed) and keep using its concrete API (locators) for
/// rendering: `let a = Rc::new(adapter); Shared(a.clone())` mounts,
/// `a` stays usable.
pub struct Shared<A>(pub std::rc::Rc<A>);

impl<A: AstAdapter> AstAdapter for Shared<A> {
    quarb::forward_ast_adapter!(0);
}
