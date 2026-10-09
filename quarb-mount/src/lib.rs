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
//!
//! A mount may be *latent*: declared with its name, its target and
//! an opener ([`Latent`]), and opened the first time a query reaches
//! it — a hop by its name, a listing of the root's children, a
//! reference landing on its declared identity. Laziness is
//! unobservable: an unanchored `//x` or `/*` lists the root's
//! children and so activates every declared mount before the
//! engine memoizes anything; a question about "every mount"
//! (`quantifier_bound`, `unit_scale`, `document_by_ref`) consults
//! the active ones, since an inactive mount has not been reached.
//! [`AstAdapter::tree_identities`] answers for a latent mount from
//! its declaration, with a stand-in root id that resolves to the
//! real root (opening the mount) when the engine touches it.

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
        return Some(quarb::canonical_identity_at_open(t));
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
    let mut identity = quarb::canonical_identity_at_open(path);
    if p.is_dir() && !identity.ends_with('/') {
        identity.push('/');
    }
    Some(identity)
}

/// What opens a latent mount when it is first reached.
pub type Opener = Box<dyn FnOnce() -> Result<Box<dyn AstAdapter>, String>>;

/// A declared mount, opened when first reached
/// ([`MountAdapter::declare`]).
pub struct Latent {
    pub name: String,
    pub target: Option<String>,
    pub opener: Opener,
}

/// One entry of the table: active (its adapter in the cell) or
/// latent (its opener waiting).
struct Entry {
    name: String,
    target: Option<String>,
    /// Identities declared for the root beyond the target's own (a
    /// page's URL for a file fetched from it).
    identities: Vec<String>,
    cell: std::cell::OnceCell<Box<dyn AstAdapter>>,
    opener: std::cell::RefCell<Option<Opener>>,
}

impl Entry {
    fn active(mount: Mount) -> Entry {
        let cell = std::cell::OnceCell::new();
        let _ = cell.set(mount.adapter);
        Entry {
            name: mount.name,
            target: mount.target,
            identities: Vec::new(),
            cell,
            opener: std::cell::RefCell::new(None),
        }
    }
}

/// The adapter a latent mount stands on when its opener failed: an
/// unnamed root with nothing under it (the refusal was recorded).
struct Empty;

impl AstAdapter for Empty {
    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, _node: NodeId) -> Vec<NodeId> {
        Vec::new()
    }
    fn name(&self, _node: NodeId) -> Option<String> {
        None
    }
}

/// The inner id standing for a latent mount's root before the
/// mount is opened (no inner adapter uses it: ids stop at
/// 2^56 - 2). [`MountAdapter::decode`] resolves it to the real
/// root, opening the mount.
const ROOT_ALIAS: u64 = INNER_MASK;

/// A Quarb adapter over a set of named mounts.
pub struct MountAdapter {
    mounts: Vec<Entry>,
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
        Ok(MountAdapter {
            mounts: mounts.into_iter().map(Entry::active).collect(),
        })
    }

    /// Add an opened mount to a live table (a session acquiring a
    /// document); its index, or a refusal past [`MAX_MOUNTS`] or on
    /// a name already mounted.
    pub fn push(&mut self, mount: Mount) -> Result<usize, String> {
        self.admit(&mount.name)?;
        self.mounts.push(Entry::active(mount));
        Ok(self.mounts.len() - 1)
    }

    /// Declare a latent mount: named and identified now, opened
    /// when first reached. Its index, or a refusal as for
    /// [`push`](Self::push).
    pub fn declare(&mut self, latent: Latent) -> Result<usize, String> {
        self.admit(&latent.name)?;
        self.mounts.push(Entry {
            name: latent.name,
            target: latent.target,
            identities: Vec::new(),
            cell: std::cell::OnceCell::new(),
            opener: std::cell::RefCell::new(Some(latent.opener)),
        });
        Ok(self.mounts.len() - 1)
    }

    fn admit(&self, name: &str) -> Result<(), String> {
        if self.mounts.len() >= MAX_MOUNTS {
            return Err(format!(
                "{} mounts; at most {MAX_MOUNTS} can be mounted together",
                self.mounts.len() + 1
            ));
        }
        if self.mounts.iter().any(|e| e.name == name) {
            return Err(format!("'{name}' is already mounted"));
        }
        Ok(())
    }

    /// Declare one more identity for the `idx`-th mount's root (a
    /// page's URL for a file fetched from it): a reference naming it
    /// lands on the mount, opening it if latent.
    pub fn declare_identity(&mut self, idx: usize, identity: String) {
        let e = &mut self.mounts[idx];
        if !e.identities.contains(&identity) {
            e.identities.push(identity);
        }
    }

    /// How many mounts the table holds, active or latent.
    pub fn len(&self) -> usize {
        self.mounts.len()
    }

    /// Whether the table holds no mounts.
    pub fn is_empty(&self) -> bool {
        self.mounts.is_empty()
    }

    /// Whether the `idx`-th mount has been opened.
    pub fn is_active(&self, idx: usize) -> bool {
        self.mounts[idx].cell.get().is_some()
    }

    /// Open the `idx`-th mount now (a replay plan prefetching what
    /// the cells name); the opener's error, when it fails — the
    /// mount then stands on an empty adapter.
    pub fn activate(&self, idx: usize) -> Result<(), String> {
        self.open(idx).map(|_| ())
    }

    /// The `idx`-th mount's adapter, opened on first use; a failed
    /// opener is reported as a refusal and the mount stands empty.
    fn adapter(&self, m: usize) -> &dyn AstAdapter {
        match self.open(m) {
            Ok(a) => a,
            Err(_) => self.mounts[m].cell.get().expect("set on failure").as_ref(),
        }
    }

    fn open(&self, m: usize) -> Result<&dyn AstAdapter, String> {
        let e = &self.mounts[m];
        if let Some(a) = e.cell.get() {
            return Ok(a.as_ref());
        }
        let opened = match e.opener.borrow_mut().take() {
            Some(open) => open(),
            None => Ok(Box::new(Empty) as Box<dyn AstAdapter>),
        };
        match opened {
            Ok(a) => {
                let _ = e.cell.set(a);
                Ok(e.cell.get().expect("just set").as_ref())
            }
            Err(err) => {
                quarb::record_refusal(format!("mount '{}': {err}", e.name));
                let _ = e.cell.set(Box::new(Empty));
                Err(err)
            }
        }
    }

    /// The adapter of an active mount; `None` while latent.
    fn active(&self, m: usize) -> Option<&dyn AstAdapter> {
        self.mounts[m].cell.get().map(|a| a.as_ref())
    }

    /// Which mount a node belongs to, with the inner id — `None` for
    /// the synthetic root. A latent mount's stand-in root resolves
    /// to the real one, opening the mount.
    pub fn decode(&self, node: NodeId) -> Option<(usize, NodeId)> {
        let m = (node.0 >> SHIFT) as usize;
        if m == 0 {
            None
        } else {
            let inner = NodeId(node.0 & INNER_MASK);
            if inner.0 == ROOT_ALIAS {
                Some((m - 1, self.adapter(m - 1).root()))
            } else {
                Some((m - 1, inner))
            }
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
        (0..self.mounts.len())
            .filter_map(|m| self.active(m))
            .find_map(|a| a.unit_scale(expr))
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        match self.decode(node) {
            // Listing the root reaches every mount: each opens.
            None => (0..self.mounts.len())
                .map(|m| self.encode(m, self.adapter(m).root()))
                .collect(),
            Some((m, inner)) => self
                .adapter(m)
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
            Some((m, inner)) => self.encode(m, self.adapter(m).child_host(inner)),
        }
    }

    fn properties(&self, node: NodeId) -> Vec<(String, Value)> {
        match self.decode(node) {
            None => Vec::new(),
            Some((m, inner)) => self.adapter(m).properties(inner),
        }
    }

    /// The walk is open when every mount opens to it.
    fn walks_for_references(&self) -> bool {
        (0..self.mounts.len())
            .filter_map(|m| self.active(m))
            .all(|a| a.walks_for_references())
    }
    /// Within one mount its own declarations answer; across mounts
    /// only a reference property of the source's own can reach.
    fn can_refer_to(&self, source: NodeId, target: NodeId) -> Option<bool> {
        let (m, s) = self.decode(source)?;
        match self.decode(target) {
            Some((n, t)) if n == m => self.adapter(m).can_refer_to(s, t),
            _ => self.adapter(m).ref_property(s).map(|_| true),
        }
    }
    fn reference_properties(&self, source: NodeId, target: NodeId) -> Vec<String> {
        let Some((m, s)) = self.decode(source) else {
            return Vec::new();
        };
        match self.decode(target) {
            Some((n, t)) if n == m => self.adapter(m).reference_properties(s, t),
            _ => self.adapter(m).ref_property(s).into_iter().collect(),
        }
    }
    fn resolves_references(&self) -> bool {
        (0..self.mounts.len())
            .filter_map(|m| self.active(m))
            .any(|a| a.resolves_references())
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
        let v = self
            .adapter(m)
            .index_referrers(rest, property, label, inner)?;
        Some(v.into_iter().map(|t| self.encode(m, t)).collect())
    }
    fn reverse_label(&self, target: NodeId) -> Option<String> {
        let (m, inner) = self.decode(target)?;
        self.adapter(m).reverse_label(inner)
    }
    /// Every mount's own target is an identity of its root,
    /// canonicalized: a file's absolute path, a directory as a base
    /// over its files (its path with a trailing `/`), a URL as
    /// itself. A reading prefix (`text:`, `json2:`) is not part of
    /// the identity. Then whatever the mounts declare inside.
    fn tree_identities(&self) -> Vec<(String, NodeId)> {
        let mut out = Vec::new();
        for (m, mount) in self.mounts.iter().enumerate() {
            // A latent mount answers from its declaration, its root
            // the stand-in that opens it when touched.
            let active = self.active(m);
            let root = match active {
                Some(a) => self.encode(m, a.root()),
                None => self.encode(m, NodeId(ROOT_ALIAS)),
            };
            if let Some(target) = mount.target.as_deref()
                && let Some(identity) = mount_identity(target)
            {
                out.push((identity, root));
            }
            out.extend(mount.identities.iter().map(|i| (i.clone(), root)));
            if let Some(a) = active {
                out.extend(
                    a.tree_identities()
                        .into_iter()
                        .map(|(i, n)| (i, self.encode(m, n))),
                );
            }
        }
        out
    }
    fn resolve_needs_label(&self, node: NodeId, property: &str) -> bool {
        self.decode(node)
            .is_some_and(|(m, inner)| self.adapter(m).resolve_needs_label(inner, property))
    }
    fn property_names(&self, node: NodeId) -> Vec<String> {
        match self.decode(node) {
            Some((m, inner)) => self.adapter(m).property_names(inner),
            None => Vec::new(),
        }
    }

    /// Every mount's non-tree nodes, mount by mount.
    fn non_tree_nodes(&self) -> Vec<NodeId> {
        (0..self.mounts.len())
            .filter_map(|m| self.active(m).map(|a| (m, a)))
            .flat_map(|(m, a)| {
                a.non_tree_nodes()
                    .into_iter()
                    .map(move |n| self.encode(m, n))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// At the synthetic root the children are the mount roots, keyed
    /// by mount name — matching what [`name`](Self::name) reports.
    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        match self.decode(node) {
            // A hop by name reaches that mount alone: it opens.
            None => (0..self.mounts.len())
                .filter(|&m| self.mounts[m].name == name)
                .map(|m| self.encode(m, self.adapter(m).root()))
                .collect(),
            Some((m, inner)) => self
                .adapter(m)
                .children_named(inner, name)
                .into_iter()
                .map(|c| self.encode(m, c))
                .collect(),
        }
    }

    fn name(&self, node: NodeId) -> Option<String> {
        let (m, inner) = self.decode(node)?;
        let a = self.adapter(m);
        if inner == a.root() {
            Some(self.mounts[m].name.clone())
        } else {
            a.name(inner)
        }
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        let (m, inner) = self.decode(node)?;
        let a = self.adapter(m);
        if inner == a.root() {
            Some(NodeId(0))
        } else {
            a.parent(inner).map(|p| self.encode(m, p))
        }
    }

    fn traits(&self, node: NodeId) -> Vec<String> {
        match self.decode(node) {
            None => Vec::new(),
            Some((m, inner)) => self.adapter(m).traits(inner),
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let (m, inner) = self.decode(node)?;
        self.adapter(m).property(inner, name)
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        let (m, inner) = self.decode(node)?;
        self.adapter(m).default_value(inner)
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let (m, inner) = self.decode(node)?;
        self.adapter(m).metadata(inner, key)
    }

    /// Per node, because each mount brings its own list: a git
    /// mount aliases `short`, the JSON mount beside it aliases
    /// nothing.
    fn aliased_metadata(&self, node: NodeId) -> &'static [&'static str] {
        match self.decode(node) {
            Some((m, inner)) => self.adapter(m).aliased_metadata(inner),
            None => &[],
        }
    }

    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        match self.decode(node) {
            None => Vec::new(),
            Some((m, inner)) => self
                .adapter(m)
                .links(inner)
                .into_iter()
                .map(|(l, t)| (l, self.encode(m, t)))
                .collect(),
        }
    }

    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        match self.decode(node) {
            None => Vec::new(),
            Some((m, inner)) => self
                .adapter(m)
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
        self.adapter(m).link_property(src, label, tgt, name)
    }

    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let (m, inner) = self.decode(node)?;
        // Resolution stays within the node's own mount.
        self.adapter(m)
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
        self.adapter(m).external_ref(inner, property, hint)
    }

    fn ref_property(&self, node: NodeId) -> Option<String> {
        let (m, inner) = self.decode(node)?;
        self.adapter(m).ref_property(inner)
    }

    fn ref_label(&self, node: NodeId, property: &str) -> Option<String> {
        let (m, inner) = self.decode(node)?;
        self.adapter(m).ref_label(inner, property)
    }

    fn resolve_fragment(&self, node: NodeId, fragment: &str) -> Option<NodeId> {
        // The fragment lands within the node's own mount.
        let (m, inner) = self.decode(node)?;
        self.adapter(m)
            .resolve_fragment(inner, fragment)
            .map(|t| self.encode(m, t))
    }

    /// The first mount holding a document under `id` answers.
    fn document_by_ref(&self, id: &str) -> Option<NodeId> {
        (0..self.mounts.len())
            .filter_map(|m| self.active(m).map(|a| (m, a)))
            .find_map(|(m, a)| a.document_by_ref(id).map(|t| self.encode(m, t)))
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
        let v = self.adapter(m).reverse_resolve(inner, property, hint)?;
        Some(v.into_iter().map(|t| self.encode(m, t)).collect())
    }

    fn descendants_named(&self, node: NodeId, name: &str) -> Option<Vec<(NodeId, usize)>> {
        let (m, inner) = self.decode(node)?;
        let v = self.adapter(m).descendants_named(inner, name)?;
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
        let v = self
            .adapter(m)
            .descendants_where(inner, name, property, value)?;
        Some(v.into_iter().map(|(t, d)| (self.encode(m, t), d)).collect())
    }

    fn select(&self, from: NodeId, plan: &quarb::StepPlan<'_>) -> Option<quarb::Selection> {
        // The mount root's children are the mounts themselves.
        let Some((m, inner)) = self.decode(from) else {
            return quarb::adapter::select_by_hooks(self, from, plan);
        };
        let sel = self.adapter(m).select(inner, plan)?;
        Some(quarb::Selection {
            nodes: sel
                .nodes
                .into_iter()
                .map(|(t, d)| (self.encode(m, t), d))
                .collect(),
            applied: sel.applied,
        })
    }

    /// The scope names the mount first; an inactive mount cannot
    /// say (its catalog is not open), and the plan counts the
    /// conjuncts as the engine's.
    fn select_plan(&self, scope: &[&str], plan: &quarb::StepPlan<'_>) -> Option<Vec<bool>> {
        let (first, rest) = scope.split_first()?;
        let m = (0..self.mounts.len()).find(|&m| self.mounts[m].name == *first)?;
        self.active(m)?.select_plan(rest, plan)
    }

    fn estimate(&self, scope: &[&str], plan: &quarb::StepPlan<'_>) -> Option<quarb::Estimate> {
        let (first, rest) = scope.split_first()?;
        let m = (0..self.mounts.len()).find(|&m| self.mounts[m].name == *first)?;
        self.active(m)?.estimate(rest, plan)
    }

    fn index_lookup(&self, scope: &str, property: &str, value: &Value) -> Option<Vec<NodeId>> {
        // The scope names the mount first: `/mount/rest…`.
        let rest = scope.strip_prefix('/')?;
        let m = (0..self.mounts.len()).find(|&m| {
            rest.strip_prefix(self.mounts[m].name.as_str())
                .is_some_and(|r| r.starts_with('/'))
        })?;
        let rest = &rest[self.mounts[m].name.len()..];
        let v = self.adapter(m).index_lookup(rest, property, value)?;
        Some(v.into_iter().map(|t| self.encode(m, t)).collect())
    }

    fn has_node(&self, node: NodeId) -> bool {
        match self.decode(node) {
            None => node == self.root(),
            Some((m, inner)) => self.adapter(m).has_node(inner),
        }
    }

    /// A mounted adapter's own word on names and traits (a web
    /// store's rows answer to their kind): inside a mount the
    /// adapter decides; the mount's root answers by its mount name.
    fn answers_to(&self, node: NodeId, name: &str) -> bool {
        match self.decode(node) {
            Some((m, inner)) if inner != self.adapter(m).root() => {
                self.adapter(m).answers_to(inner, name)
            }
            _ => self.name(node).as_deref() == Some(name),
        }
    }

    fn has_trait(&self, node: NodeId, name: &str) -> bool {
        match self.decode(node) {
            None => false,
            Some((m, inner)) => self.adapter(m).has_trait(inner, name),
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
                self.adapter(m).prefetch_links(&inner, dir);
            }
        }
    }

    fn quantifier_bound(&self) -> usize {
        // A query spans every mount: the most permissive inner bound
        // must not be silently cut.
        (0..self.mounts.len())
            .filter_map(|m| self.active(m))
            .map(|a| a.quantifier_bound())
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
            Some((m, inner)) => self
                .adapter(m)
                .provenance(inner)
                .or(self.source_fill(m, inner)),
        }
    }
    /// The same layering, entry by entry.
    fn provenance_list(&self, node: NodeId) -> quarb::ProvenanceList {
        match self.decode(node) {
            None => quarb::ProvenanceList::default(),
            Some((m, inner)) => self
                .adapter(m)
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
            path: Some(quarb::name_path(self.adapter(m), inner)),
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
