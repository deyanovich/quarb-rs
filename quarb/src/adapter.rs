//! The adapter surface: how a data source plugs into the engine.
//!
//! The live methods — those the engine currently drives — are
//! the navigation set ([`root`](AstAdapter::root),
//! [`children`](AstAdapter::children), [`name`](AstAdapter::name),
//! [`parent`](AstAdapter::parent)) plus the projection set
//! ([`traits`](AstAdapter::traits), [`property`](AstAdapter::property),
//! [`default_value`](AstAdapter::default_value),
//! [`metadata`](AstAdapter::metadata)). The projection methods have
//! defaults, so an adapter can implement only what its domain
//! supports. Crosslink resolution (`-->`) and pattern search (`=>`)
//! are still planned. See `doc/impl.tex`.

use crate::value::Value;

/// An opaque handle to a node in an arbor.
///
/// The engine treats a `NodeId` as an opaque token: it is minted and
/// interpreted solely by the adapter that produced it. The `u64`
/// payload is an adapter-private index or key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u64);

/// Per-node data provenance: the three optional components behind
/// the `:::source` / `:::instant` / `:::dpid` core-metadata keys and
/// their composite `:::provenance` (`?src@ts#dpid`, kaiv's
/// spelling). A component is present only where the source genuinely
/// records it; wrapper adapters layer them ([`or`](Self::or), inner
/// wins per component).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Provenance {
    /// Where the datum came from: the document, file, store or
    /// page — the innermost source that holds the node (a grafted
    /// node's source is the file the graft opened, not the
    /// directory above it).
    pub source: Option<String>,
    /// The node's path within its source — its name-path from the
    /// source's root — so a source and a path name the node
    /// without the engine's id. Filled by the layer that knows
    /// where the source begins (a mount, a graft), else by the
    /// engine from the arbor root.
    pub path: Option<String>,
    /// When the datum was observed — `(secs, nanos, offset_min)`,
    /// the shape `Value::Instant` carries — by the rung
    /// [`instant_from`](Self::instant_from) names.
    pub instant: Option<(i64, u32, Option<i16>)>,
    /// Which rung answered the instant: the datum's own
    /// timestamp, its source's modification time, or the moment of
    /// reading. `None` with no instant.
    pub instant_from: Option<InstantFrom>,
    /// The source-assigned data-point identifier (kaiv `#dpid`).
    pub dpid: Option<String>,
}

/// The rung an instant came from — three facts that share one
/// field: a row's own timestamp, a file's mtime, the clock at the
/// read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstantFrom {
    /// The datum's own timestamp (a kaiv `@ts`, a commit date, a
    /// row's `modified`).
    Node,
    /// The source's modification time (a file's mtime).
    Source,
    /// The moment the query read it: the invocation instant.
    Access,
}

impl InstantFrom {
    pub fn name(self) -> &'static str {
        match self {
            InstantFrom::Node => "node",
            InstantFrom::Source => "source",
            InstantFrom::Access => "access",
        }
    }
}

impl Provenance {
    /// Component-wise layering: `self` (inner, more specific) wins;
    /// missing components fill from `outer`. The instant and its
    /// rung travel together.
    pub fn or(self, outer: Provenance) -> Provenance {
        let (instant, instant_from) = if self.instant.is_some() {
            (self.instant, self.instant_from)
        } else {
            (outer.instant, outer.instant_from)
        };
        Provenance {
            source: self.source.or(outer.source),
            path: self.path.or(outer.path),
            instant,
            instant_from,
            dpid: self.dpid.or(outer.dpid),
        }
    }

    /// Nothing recorded — the path does not count, it is derivable.
    pub fn is_empty(&self) -> bool {
        self.source.is_none() && self.instant.is_none() && self.dpid.is_none()
    }

    /// The instant re-labelled as the source's — what a graft does
    /// to the file's own mtime when it fills a node inside the file.
    pub fn as_source_rung(mut self) -> Provenance {
        if self.instant.is_some() {
            self.instant_from = Some(InstantFrom::Source);
        }
        self
    }

    /// The canonical text of a list: `?a@ts#x;b#y;+N` — one `?`,
    /// the entries separated by `;` (kaiv's list separator), each
    /// in its own optionality grammar, the elision marker `+N` last
    /// when `elided > 0`. `None` when no entry has a component.
    pub fn canonical_list(entries: &[Provenance], elided: u32) -> Option<String> {
        // Entries that write alike are one in the text (the path is
        // not part of it).
        let mut parts: Vec<String> = Vec::new();
        for c in entries.iter().filter_map(|e| e.canonical()) {
            let c = c.trim_start_matches('?').to_string();
            if !parts.contains(&c) {
                parts.push(c);
            }
        }
        if parts.is_empty() {
            return None;
        }
        let mut out = format!("?{}", parts.join(";"));
        if elided > 0 {
            out.push_str(&format!(";+{elided}"));
        }
        Some(out)
    }

    /// The composite canonical text `?src@ts#dpid` — kaiv's
    /// optionality grammar (`?src`, `?src@ts`, `?@ts#dpid`, …), the
    /// instant in Quarb's dashed-extended display form. `None` when
    /// fully empty.
    pub fn canonical(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut out = String::from("?");
        if let Some(src) = &self.source {
            out.push_str(src);
        }
        if let Some((secs, _, offset)) = self.instant {
            // kaiv's instant has no sub-second field: whole seconds,
            // whatever the rung recorded.
            out.push('@');
            out.push_str(&crate::temporal::format_instant(secs, 0, offset));
        }
        if let Some(dpid) = &self.dpid {
            out.push('#');
            out.push_str(dpid);
        }
        Some(out)
    }
}

/// A node's provenance as a list: several entries where the datum
/// was recorded from several sources (a kaiv leaf's `?a;b`), and how
/// many further sources the record elides (its `;+N`). The singular
/// [`AstAdapter::provenance`] is the list's anchoring first entry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProvenanceList {
    pub entries: Vec<Provenance>,
    pub elided: u32,
}

impl ProvenanceList {
    /// The one-entry list — empty when the entry is.
    pub fn single(p: Provenance) -> ProvenanceList {
        ProvenanceList {
            entries: if p.is_empty() { Vec::new() } else { vec![p] },
            elided: 0,
        }
    }

    /// The anchoring entry (the first), empty when there is none.
    pub fn first(&self) -> Provenance {
        self.entries.first().cloned().unwrap_or_default()
    }

    /// The latest instant any entry carries: a datum with several
    /// sources is as fresh as its newest.
    pub fn instant_max(&self) -> Option<(i64, u32, Option<i16>)> {
        self.entries
            .iter()
            .filter_map(|p| p.instant)
            .max_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)))
    }

    /// The canonical list text, `?a@ts#x;b#y;+N`.
    pub fn canonical(&self) -> Option<String> {
        Provenance::canonical_list(&self.entries, self.elided)
    }

    /// Component-wise layering of every entry over `outer` (see
    /// [`Provenance::or`]); an empty list takes `outer` as its one
    /// entry.
    pub fn or(self, outer: Provenance) -> ProvenanceList {
        if self.entries.is_empty() {
            return ProvenanceList {
                entries: ProvenanceList::single(outer).entries,
                elided: self.elided,
            };
        }
        ProvenanceList {
            entries: self
                .entries
                .into_iter()
                .map(|p| p.or(outer.clone()))
                .collect(),
            elided: self.elided,
        }
    }
}

/// The direction of a crosslink walk, for the batching hint
/// [`AstAdapter::prefetch_links`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkDir {
    /// `->`: the links a node carries.
    Out,
    /// `<-`: the links that reach a node.
    In,
}

/// The interface a data source implements to be queried by Quarb.
///
/// An adapter maps its native structure onto the arbor model: a tree
/// backbone whose edges carry *names*. The engine drives navigation
/// purely through this trait, so the same query language runs over
/// any adapter.
/// A reference to a document outside the arbor, as an adapter read
/// it from its own reference syntax: which document, and where in
/// it. The engine treats both as opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRef {
    /// The document's identity (a URL, a path, a key — whatever
    /// the mounts are registered under).
    pub document: String,
    /// The location within that document, for the adapter that
    /// owns it to resolve; `None` for the document itself.
    pub within: Option<String>,
}

impl ExternalRef {
    /// Read a URI reference: everything before the first `#` is the
    /// document, a non-empty fragment after it the location. A
    /// convenience for adapters whose references are URIs (html,
    /// the text level, a JSON `$ref`); an adapter with another
    /// syntax builds the two parts its own way.
    pub fn uri(reference: &str) -> Self {
        match reference.split_once('#') {
            Some((document, fragment)) => ExternalRef {
                document: document.to_string(),
                within: (!fragment.is_empty()).then(|| fragment.to_string()),
            },
            None => ExternalRef {
                document: reference.to_string(),
                within: None,
            },
        }
    }

    /// The reference as one URI again.
    pub fn to_uri(&self) -> String {
        match &self.within {
            Some(w) => format!("{}#{w}", self.document),
            None => self.document.clone(),
        }
    }
}

pub trait AstAdapter {
    /// The root node — the initial navigation context.
    fn root(&self) -> NodeId;

    /// The tree children of `node`, in document order.
    ///
    /// Returns an empty vector for a leaf (or an unreadable node).
    fn children(&self, node: NodeId) -> Vec<NodeId>;

    /// The name of `node` — the label of its incoming tree edge.
    ///
    /// `None` when the adapter leaves a node unnamed (typically the
    /// root; e.g. the filesystem root `/` carries no name).
    fn name(&self, node: NodeId) -> Option<String>;

    /// The parent of `node`, or `None` for the root.
    fn parent(&self, _node: NodeId) -> Option<NodeId> {
        None
    }

    /// The traits of `node` — its adapter-defined classifications,
    /// used by `<trait>` navigation filters (e.g. a filesystem
    /// adapter's `<dir>`, `<code>`, `<image>`).
    fn traits(&self, _node: NodeId) -> Vec<String> {
        Vec::new()
    }

    /// A named property of `node` — `::prop`. `None` if absent.
    fn property(&self, _node: NodeId, _name: &str) -> Option<Value> {
        None
    }

    /// The node's own properties, enumerated — name and value, in
    /// the source's order — for readings that keep values off the
    /// tree (the JSON property reading, where an object's scalar
    /// fields are no nodes). The serializers (`| json`, `| xml`,
    /// `| kaiv`) merge them with the children, a child winning a
    /// shared name. The default is none: for a reading whose values
    /// are all nodes, the children already say everything.
    fn properties(&self, _node: NodeId) -> Vec<(String, Value)> {
        Vec::new()
    }

    /// Whether a reverse resolution or a value match with no scope
    /// may walk the whole arbor to find its referrers. An adapter
    /// over a small, in-memory source opens to the walk; a store or
    /// a service does not, and such a query is refused with a message
    /// asking for a scope — an empty answer could not be told from an
    /// absence of referrers. A declared reference (a foreign key, a
    /// model `ref`) answers through [`reverse_resolve`](Self::reverse_resolve)
    /// and needs no walk.
    fn walks_for_references(&self) -> bool {
        false
    }

    /// Whether a reference *declared* on `source` — a catalog key, a
    /// model `ref`, a reference property the format defines — can
    /// land in `target`'s container. A question of the schema, never
    /// of the data; `None` where nothing is declared at the node.
    /// The default: a node with a reference property of its own can
    /// refer anywhere (an href may point at any element).
    fn can_refer_to(&self, source: NodeId, _target: NodeId) -> Option<bool> {
        self.ref_property(source).map(|_| true)
    }

    /// The reference properties declared on `source` that can land
    /// in `target`'s container: a catalog's key columns into the
    /// target's table, the fields of a model's `ref`s, the node's
    /// own reference property. What `t<--` and `t<--(SCOPE)` read
    /// when the query names no property; empty where none is
    /// declared.
    fn reference_properties(&self, source: NodeId, _target: NodeId) -> Vec<String> {
        self.ref_property(source).into_iter().collect()
    }

    /// Whether resolving `property` of `node` needs a reference
    /// label, because the property itself declares no target: a
    /// column with no key, or any property of a property graph,
    /// where a value names a node only under a node label. A
    /// label-less `::p-->` over such a property is refused with the
    /// remedy, since an empty answer would read as "points nowhere".
    fn resolve_needs_label(&self, _node: NodeId, _property: &str) -> bool {
        false
    }

    /// The reference label under which a referrer's property is
    /// also resolved when a reverse arrow asks about `target` and
    /// the query writes no label. `None` (the default): only the
    /// label-less resolution counts. An adapter whose label names a
    /// container (a table, a collection, a node label) returns the
    /// one `target` stands in, so `/authors/1<--(/books/*::author_id)`
    /// reads `author_id` as a key into `authors` without the query
    /// saying so. The engine derives no label of its own.
    fn reverse_label(&self, _target: NodeId) -> Option<String> {
        None
    }

    /// The trees of this arbor other trees refer to by name: each
    /// identity (a URL, a path, any string a reference may hold)
    /// with the node that stands for the tree. An identity ending
    /// in a separator is a base: every identity beneath it names
    /// the node reached beneath the base's node by the remainder's
    /// segments. Declared in a model (`tree IDENTITY: PATH;`) and
    /// automatic for a mount's own target; the engine reads nothing
    /// inside an identity beyond prefix and separator.
    fn tree_identities(&self) -> Vec<(String, NodeId)> {
        Vec::new()
    }

    /// The names of the properties a node has — what "any
    /// property" means for `<--(SCOPE::*)`. By default its off-tree
    /// properties, its reference property, and its children's names
    /// (a row's columns, an object's keys are properties under the
    /// same names); an adapter whose properties are not children
    /// (an element's attributes) lists them.
    fn property_names(&self, node: NodeId) -> Vec<String> {
        let mut ps: Vec<String> = self.properties(node).into_iter().map(|(k, _)| k).collect();
        if let Some(r) = self.ref_property(node)
            && !ps.contains(&r)
        {
            ps.push(r);
        }
        for c in self.children(node) {
            if let Some(name) = self.name(c)
                && !ps.contains(&name)
            {
                ps.push(name);
            }
        }
        ps
    }

    /// Whether this source resolves references at all. An adapter
    /// with no notion of a reference (a CSV file, a directory, a
    /// log stream) says no, and a reverse arrow over it is refused
    /// with the value match as the remedy, rather than answered
    /// with an empty set that would read as "nothing points here".
    fn resolves_references(&self) -> bool {
        true
    }

    /// `index SCOPE::prop;` — the nodes of the declared scope whose
    /// property holds `value`, from an index the model keeps; `None`
    /// when no declaration covers the pair (the walk answers). An
    /// index changes cost, never the answer: the caller still tests
    /// each node it names.
    fn index_lookup(&self, _scope: &str, _property: &str, _value: &Value) -> Option<Vec<NodeId>> {
        None
    }
    /// The declared-scope nodes whose `property` resolves (under the
    /// relation `label`, when given) to `target`, from the same index;
    /// `None` when no declaration covers the pair.
    fn index_referrers(
        &self,
        _scope: &str,
        _property: &str,
        _label: Option<&str>,
        _target: NodeId,
    ) -> Option<Vec<NodeId>> {
        None
    }

    /// The nodes of the arbor that are not in the tree — joined to
    /// it by crosslinks alone, with no parent and no path. The
    /// universal search `*//` reaches them after the tree, in this
    /// order; `:::is-non-tree` is true on them. The default is
    /// none: every shipped adapter puts every node in the tree.
    fn non_tree_nodes(&self) -> Vec<NodeId> {
        Vec::new()
    }

    /// The node whose children an explicit child step from `node`
    /// reads. The default is `node` itself. An adapter whose tree
    /// holds transparent links (a filesystem's symlink to a
    /// directory) answers the link's target, so `/link/name` lands
    /// on the target's own child — the same node `/target/name`
    /// reaches. The descendant walk never consults this: it reads
    /// [`children`](Self::children) alone, so a linked subtree is
    /// walked once, under its own parent, and a cycle of links
    /// cannot trap it.
    fn child_host(&self, node: NodeId) -> NodeId {
        node
    }

    /// The children of `node` whose edge name is exactly `name` —
    /// the engine's fast path for name-matcher child hops. The
    /// default filters [`children`](Self::children); an adapter
    /// whose containers cannot be enumerated (permission-scoped or
    /// unbounded remote trees) overrides this with a direct,
    /// name-addressed lookup. Must be observationally identical to
    /// the default wherever enumeration works. The adapter owns the
    /// name test: it may deliberately *alias* — resolve a name to a
    /// node whose edge name differs (git revision syntax landing on
    /// a hash-named commit) — and the engine will not re-filter.
    /// Container-scoped resolution like that stays child-axis only;
    /// per-node spelling aliases belong in
    /// [`answers_to`](Self::answers_to), which this default
    /// consults.
    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        self.children(node)
            .into_iter()
            .filter(|&c| self.answers_to(c, name))
            .collect()
    }

    /// Whether `node` answers to `name` as a literal spelling
    ///. The default is canonical equality. An adapter
    /// may declare per-node aliases — a social feed spelling a
    /// handle `@alice` while the stripped `alice` is the hop name —
    /// and the engine consults this wherever a *literal* name is
    /// matched, on every axis, so one override keeps `/@alice` and
    /// `//@alice` in agreement. `:::name` stays canonical (locators
    /// and reflection print it; an alias is a way in, never a way
    /// out), and name patterns (`~(...)`, `*`) test the canonical
    /// name only.
    fn answers_to(&self, node: NodeId, name: &str) -> bool {
        self.name(node).as_deref() == Some(name)
    }

    /// Whether `node` bears `name` as a trait, as written in a
    /// `<...>` clause. The default is membership in
    /// [`traits`](Self::traits); an adapter may declare aliases the
    /// way [`answers_to`](Self::answers_to) does for names — a
    /// model file's `alias <chunk> <block>;` or
    /// `alias <s/^tag://i>;` — and the engine consults this
    /// wherever a written trait is matched. Reflection and
    /// serialization print the canonical traits only.
    fn has_trait(&self, node: NodeId, name: &str) -> bool {
        self.traits(node).iter().any(|t| t == name)
    }

    /// The default projection of `node` — bare `::`, adapter-specific
    /// (a filesystem adapter returns file content).
    fn default_value(&self, _node: NodeId) -> Option<Value> {
        None
    }

    /// Adapter-defined metadata — `::::key` (a filesystem adapter's
    /// `size`, `modified`, `permissions`, …). `None` if absent.
    fn metadata(&self, _node: NodeId, _key: &str) -> Option<Value> {
        None
    }

    /// Metadata keys this adapter also answers at `::` (ruling
    /// #29), for the given node — per node, because a composite
    /// adapter (a multi-mount, a graft) answers for whichever
    /// document owns it. Only an adapter whose property surface is *closed* —
    /// fixed by the adapter, never grown by document content — may
    /// declare aliases: a git commit cannot sprout a `short` field,
    /// where a JSON object can sprout anything. Data always wins:
    /// the engine consults `property` first and falls through only
    /// when it answers `None` for that node, so an alias can never
    /// shadow a document. Core metadata (`:::`) is never aliased —
    /// `name`, `id` and `index` exist on every node, and aliasing
    /// them would change what `::name` means wherever a document
    /// happens to lack that field.
    fn aliased_metadata(&self, _node: NodeId) -> &'static [&'static str] {
        &[]
    }

    /// Outgoing crosslinks from `node`, as `(label, target)` pairs,
    /// for `->` navigation (a filesystem adapter's symlinks).
    fn links(&self, _node: NodeId) -> Vec<(String, NodeId)> {
        Vec::new()
    }

    /// Incoming crosslinks to `node`, as `(label, source)` pairs, for
    /// `<-` navigation. May be expensive (an adapter that does not
    /// precompute edges must search for referrers).
    fn backlinks(&self, _node: NodeId) -> Vec<(String, NodeId)> {
        Vec::new()
    }

    /// Resolve a cross-reference: `::property-->hint` maps `node`'s
    /// `property` (a value that references another node) to its target,
    /// with an optional adapter-specific `hint` (a relation type such as
    /// an html `rel`, or a target container such as a table). A JSON
    /// adapter resolves a `$ref` JSON Pointer; `None` if unresolvable.
    fn resolve(&self, _node: NodeId, _property: &str, _hint: Option<&str>) -> Option<NodeId> {
        None
    }

    /// The external reference `node`'s `property` holds, if any: the
    /// identity of a document *outside this arbor* and, where the
    /// reference says so, the location within it — both as the
    /// adapter reads them out of its own reference syntax (for html,
    /// the anchor's absolute URL and its fragment). The engine does
    /// not look inside either string: it asks for the document among
    /// the mounts (the host's registration, then
    /// [`document_by_ref`](Self::document_by_ref)), and hands the
    /// location to [`resolve_fragment`](Self::resolve_fragment) on
    /// the document it found. Consulted by `::property-->` when
    /// in-document resolution misses; an unmounted document lands
    /// among the run's unresolved external references for the
    /// host's acquisition loop. The engine itself never fetches.
    fn external_ref(
        &self,
        _node: NodeId,
        _property: &str,
        _hint: Option<&str>,
    ) -> Option<ExternalRef> {
        None
    }

    /// The element `fragment` names inside the document `node`
    /// belongs to — html's `id` lookup. The landing rung of a
    /// fragment-carrying external reference, once its document is
    /// mounted.
    fn resolve_fragment(&self, _node: NodeId, _fragment: &str) -> Option<NodeId> {
        None
    }

    /// The property carrying `node`'s own reference, for the bare
    /// arrow (`//a-->`): an html anchor's `href`, an iframe's
    /// `src`. `None` for a node that references nothing.
    fn ref_property(&self, _node: NodeId) -> Option<String> {
        None
    }

    /// The relation name a resolution edge from `node` via
    /// `property` carries — html's `rel` attribute. `None` falls
    /// back to the property name as the edge label.
    fn ref_label(&self, _node: NodeId, _property: &str) -> Option<String> {
        None
    }

    /// A property of the crosslink `source --label--> target` — the
    /// `$-::prop` read. Adapters whose edges carry data (a property
    /// graph's relationship properties) override this; `None` if the
    /// edge is bare or unknown. Where parallel edges share source,
    /// label, and target, the adapter answers for one of them,
    /// consistently.
    fn link_property(
        &self,
        _source: NodeId,
        _label: &str,
        _target: NodeId,
        _name: &str,
    ) -> Option<Value> {
        None
    }

    /// The document this arbor holds under an external-reference
    /// identifier (a URL, fragment-stripped), when it can answer
    /// without the host's registration: the landing rung of `-->`
    /// after the mounted-document table misses. An adapter over a
    /// store of pages answers by URL lookup; the engine performs
    /// no IO either way.
    fn document_by_ref(&self, _id: &str) -> Option<NodeId> {
        None
    }

    /// Every node whose `property` (or, when `None`, own reference
    /// property) resolves to `node`, in document order — the
    /// reverse index behind `<--`. `None` keeps the engine's walk
    /// of the whole arbor; `Some` is the adapter's complete answer.
    fn reverse_resolve(
        &self,
        _node: NodeId,
        _property: Option<&str>,
        _hint: Option<&str>,
    ) -> Option<Vec<NodeId>> {
        None
    }

    /// The descendants of `node` that answer to `name`, each with
    /// its depth (children are depth 1), in document order — the
    /// fast path behind `//name` for an adapter whose substrate
    /// indexes names. `None` keeps the engine's walk; `Some` must
    /// be exactly what the walk would find.
    fn descendants_named(&self, _node: NodeId, _name: &str) -> Option<Vec<(NodeId, usize)>> {
        None
    }

    /// The descendants of `node` named `name` whose property
    /// `property` equals `value`, each with its depth, in document
    /// order — the fast path behind `//name[::property = "value"]`
    /// for an adapter that indexes a property (the corpus reading's
    /// positional index answers `//token[::lower = "tom"]` from it).
    /// `None` keeps the engine's walk and its comparison; `Some`
    /// must be exactly what the walk would find under the standard
    /// equality, so an adapter answers only the values it can
    /// compare as the engine does (text against text).
    fn descendants_where(
        &self,
        _node: NodeId,
        _name: &str,
        _property: &str,
        _value: &Value,
    ) -> Option<Vec<(NodeId, usize)>> {
        None
    }

    /// A batching hint: the engine is about to follow `dir`
    /// crosslinks from every node in `nodes`. An adapter backed by
    /// a store may fetch them in one statement; semantics are
    /// unchanged either way.
    fn prefetch_links(&self, _nodes: &[NodeId], _dir: LinkDir) {}

    /// The quantifier bound N_max: the depth to which open-ended path
    /// quantifiers (`+`, `*`, `{m,}`) expand, and the ceiling of any
    /// explicit `{m,n}` (the effective upper bound is min(n, N_max)).
    /// An adapter whose natural structures run deep may raise it; the
    /// CLI overrides it per run (`qua --quantifier-bound`).
    fn quantifier_bound(&self) -> usize {
        32
    }

    /// Whether the `sh(...)` pipeline stage may run external
    /// commands. False by default — query text stays inert data —
    /// and enabled per run by the CLI (`qua --allow-shell`) through
    /// the [`AllowShell`] wrapper.
    fn allow_shell(&self) -> bool {
        false
    }

    /// The invocation instant `now()` denotes (spec: The Temporal
    /// Fragment, Determinism): one UTC timeline point bound by the
    /// runner BEFORE evaluation begins — evaluation itself never
    /// reads a clock. None by default (a library `run` is fully
    /// deterministic; `now()` reads as null); the CLI binds it at
    /// startup — pinnable with `qua --now` — through the
    /// [`WithNow`] wrapper.
    fn invocation_instant(&self) -> Option<(i64, u32)> {
        None
    }

    /// The data provenance of `node` — the `:::source` /
    /// `:::instant` / `:::dpid` / `:::provenance` core-metadata
    /// keys. Empty by default: an adapter answers only the
    /// components its substrate genuinely records (never the
    /// invocation clock); wrapper adapters fill missing components
    /// from what they know — the mount its target, a graft its
    /// outer leaf, a model its derivation — and forward the rest
    /// inward, so resolution is nearest-ancestor per component.
    fn provenance(&self, _node: NodeId) -> Provenance {
        Provenance::default()
    }

    /// Whether `node` is an id this adapter minted — the guard
    /// behind the node-id anchor `((!N))`, which may name an id
    /// from an earlier run (ids are minted deterministically: the
    /// same source, the same ids). The default probes the id
    /// through `parent` and `name` and treats a panic as absence,
    /// so an adapter that indexes by id needs no override; one that
    /// can answer cheaply should.
    fn has_node(&self, node: NodeId) -> bool {
        if node == self.root() {
            return true;
        }
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.parent(node).is_some() || self.name(node).is_some()
        }))
        .unwrap_or(false);
        std::panic::set_hook(hook);
        ok
    }

    /// The data provenance of `node` as a list — every source the
    /// substrate records for the datum, and how many it elides.
    /// The default is the singular answer as a one-entry list; an
    /// adapter whose substrate records several (kaiv's `?a;b;+N`)
    /// overrides it, and a wrapper layers each entry.
    fn provenance_list(&self, node: NodeId) -> ProvenanceList {
        ProvenanceList::single(self.provenance(node))
    }

    /// The scale of a unit expression — (factor, canonical SI-base
    /// expansion) — for the unital reading's criterion text (spec:
    /// The Quantital Fragment). The default answers from the
    /// engine's frozen built-in table; a unit-aware adapter (kaiv)
    /// overrides it to include the mounted document's own custom
    /// units, so `[::range < '50kellicam']` resolves through the
    /// document's `.!units` imports.
    fn unit_scale(&self, expr: &str) -> Option<(f64, String)> {
        crate::quantity::scale_expr(expr)
    }
}

/// Forwards every [`AstAdapter`] method to a wrapped adapter — the
/// one definition behind the decorators ([`QuantifierBound`],
/// [`AllowShell`], [`WithNow`], the boxed carrier, the decoding
/// layer, the mount and model wrappers), so a method added to the
/// trait is forwarded everywhere at once instead of silently
/// falling back to the trait default in whichever wrapper the
/// hand-written list missed.
///
/// The first argument names the wrapped adapter: a field (`inner`,
/// `0`) reached as `self.FIELD`, or `*` for a `Box<dyn AstAdapter>`
/// forwarding through `**self`. The optional `except` list names the
/// methods the wrapper defines itself, right after the invocation:
///
/// ```ignore
/// impl<A: AstAdapter> AstAdapter for Bounded<'_, A> {
///     quarb::forward_ast_adapter!(inner; except quantifier_bound);
///     fn quantifier_bound(&self) -> usize {
///         self.bound
///     }
/// }
/// ```
///
/// The method table inside the macro is checked against the trait
/// by `forwarding::the_macro_lists_every_trait_method`, and each
/// shipped wrapper against a probe adapter by
/// `forwarding::every_wrapper_forwards_every_method`.
#[macro_export]
macro_rules! forward_ast_adapter {
    ($inner:tt $(; except $($except:ident),+ $(,)?)? $(;)?) => {
        $crate::forward_ast_adapter!(@each [$inner] [$($($except)+)?]
            // Every method of `AstAdapter`, in the trait's order; the
            // coverage test reads this list from the source.
            root children name parent traits property properties
            walks_for_references can_refer_to reference_properties
            resolve_needs_label reverse_label tree_identities
            property_names resolves_references index_lookup
            index_referrers non_tree_nodes child_host children_named
            answers_to has_trait default_value metadata
            aliased_metadata links backlinks resolve external_ref
            resolve_fragment ref_property ref_label link_property
            document_by_ref reverse_resolve descendants_named
            descendants_where prefetch_links quantifier_bound
            allow_shell invocation_instant provenance has_node
            provenance_list unit_scale
        );
    };
    (@each [$inner:tt] $ex:tt $($m:ident)*) => {
        $( $crate::forward_ast_adapter!(@unless [$inner] $ex $m); )*
    };

    // The excepted method heads the list: emit nothing. One arm per
    // method, since a macro cannot compare two captured identifiers.
    (@unless [$i:tt] [root $($r:ident)*] root) => {};
    (@unless [$i:tt] [children $($r:ident)*] children) => {};
    (@unless [$i:tt] [name $($r:ident)*] name) => {};
    (@unless [$i:tt] [parent $($r:ident)*] parent) => {};
    (@unless [$i:tt] [traits $($r:ident)*] traits) => {};
    (@unless [$i:tt] [property $($r:ident)*] property) => {};
    (@unless [$i:tt] [properties $($r:ident)*] properties) => {};
    (@unless [$i:tt] [walks_for_references $($r:ident)*] walks_for_references) => {};
    (@unless [$i:tt] [can_refer_to $($r:ident)*] can_refer_to) => {};
    (@unless [$i:tt] [reference_properties $($r:ident)*] reference_properties) => {};
    (@unless [$i:tt] [resolve_needs_label $($r:ident)*] resolve_needs_label) => {};
    (@unless [$i:tt] [reverse_label $($r:ident)*] reverse_label) => {};
    (@unless [$i:tt] [tree_identities $($r:ident)*] tree_identities) => {};
    (@unless [$i:tt] [property_names $($r:ident)*] property_names) => {};
    (@unless [$i:tt] [resolves_references $($r:ident)*] resolves_references) => {};
    (@unless [$i:tt] [index_lookup $($r:ident)*] index_lookup) => {};
    (@unless [$i:tt] [index_referrers $($r:ident)*] index_referrers) => {};
    (@unless [$i:tt] [non_tree_nodes $($r:ident)*] non_tree_nodes) => {};
    (@unless [$i:tt] [child_host $($r:ident)*] child_host) => {};
    (@unless [$i:tt] [children_named $($r:ident)*] children_named) => {};
    (@unless [$i:tt] [answers_to $($r:ident)*] answers_to) => {};
    (@unless [$i:tt] [has_trait $($r:ident)*] has_trait) => {};
    (@unless [$i:tt] [default_value $($r:ident)*] default_value) => {};
    (@unless [$i:tt] [metadata $($r:ident)*] metadata) => {};
    (@unless [$i:tt] [aliased_metadata $($r:ident)*] aliased_metadata) => {};
    (@unless [$i:tt] [links $($r:ident)*] links) => {};
    (@unless [$i:tt] [backlinks $($r:ident)*] backlinks) => {};
    (@unless [$i:tt] [resolve $($r:ident)*] resolve) => {};
    (@unless [$i:tt] [external_ref $($r:ident)*] external_ref) => {};
    (@unless [$i:tt] [resolve_fragment $($r:ident)*] resolve_fragment) => {};
    (@unless [$i:tt] [ref_property $($r:ident)*] ref_property) => {};
    (@unless [$i:tt] [ref_label $($r:ident)*] ref_label) => {};
    (@unless [$i:tt] [link_property $($r:ident)*] link_property) => {};
    (@unless [$i:tt] [document_by_ref $($r:ident)*] document_by_ref) => {};
    (@unless [$i:tt] [reverse_resolve $($r:ident)*] reverse_resolve) => {};
    (@unless [$i:tt] [descendants_named $($r:ident)*] descendants_named) => {};
    (@unless [$i:tt] [descendants_where $($r:ident)*] descendants_where) => {};
    (@unless [$i:tt] [prefetch_links $($r:ident)*] prefetch_links) => {};
    (@unless [$i:tt] [quantifier_bound $($r:ident)*] quantifier_bound) => {};
    (@unless [$i:tt] [allow_shell $($r:ident)*] allow_shell) => {};
    (@unless [$i:tt] [invocation_instant $($r:ident)*] invocation_instant) => {};
    (@unless [$i:tt] [provenance $($r:ident)*] provenance) => {};
    (@unless [$i:tt] [has_node $($r:ident)*] has_node) => {};
    (@unless [$i:tt] [provenance_list $($r:ident)*] provenance_list) => {};
    (@unless [$i:tt] [unit_scale $($r:ident)*] unit_scale) => {};
    // Another method heads the list: look further down.
    (@unless [$i:tt] [$x:ident $($r:ident)*] $m:ident) => {
        $crate::forward_ast_adapter!(@unless [$i] [$($r)*] $m);
    };
    // Not excepted: forward.
    (@unless [$i:tt] [] $m:ident) => {
        $crate::forward_ast_adapter!(@fwd [$i] $m);
    };

    // The wrapped adapter, as an expression on `self`.
    (@target $s:ident *) => { (**$s) };
    (@target $s:ident $f:tt) => { $s.$f };

    (@fwd [$i:tt] root) => {
        fn root(&self) -> $crate::NodeId {
            $crate::forward_ast_adapter!(@target self $i).root()
        }
    };
    (@fwd [$i:tt] children) => {
        fn children(&self, node: $crate::NodeId) -> Vec<$crate::NodeId> {
            $crate::forward_ast_adapter!(@target self $i).children(node)
        }
    };
    (@fwd [$i:tt] name) => {
        fn name(&self, node: $crate::NodeId) -> Option<String> {
            $crate::forward_ast_adapter!(@target self $i).name(node)
        }
    };
    (@fwd [$i:tt] parent) => {
        fn parent(&self, node: $crate::NodeId) -> Option<$crate::NodeId> {
            $crate::forward_ast_adapter!(@target self $i).parent(node)
        }
    };
    (@fwd [$i:tt] traits) => {
        fn traits(&self, node: $crate::NodeId) -> Vec<String> {
            $crate::forward_ast_adapter!(@target self $i).traits(node)
        }
    };
    (@fwd [$i:tt] property) => {
        fn property(&self, node: $crate::NodeId, name: &str) -> Option<$crate::Value> {
            $crate::forward_ast_adapter!(@target self $i).property(node, name)
        }
    };
    (@fwd [$i:tt] properties) => {
        fn properties(&self, node: $crate::NodeId) -> Vec<(String, $crate::Value)> {
            $crate::forward_ast_adapter!(@target self $i).properties(node)
        }
    };
    (@fwd [$i:tt] walks_for_references) => {
        fn walks_for_references(&self) -> bool {
            $crate::forward_ast_adapter!(@target self $i).walks_for_references()
        }
    };
    (@fwd [$i:tt] can_refer_to) => {
        fn can_refer_to(&self, source: $crate::NodeId, target: $crate::NodeId) -> Option<bool> {
            $crate::forward_ast_adapter!(@target self $i).can_refer_to(source, target)
        }
    };
    (@fwd [$i:tt] reference_properties) => {
        fn reference_properties(
            &self,
            source: $crate::NodeId,
            target: $crate::NodeId,
        ) -> Vec<String> {
            $crate::forward_ast_adapter!(@target self $i).reference_properties(source, target)
        }
    };
    (@fwd [$i:tt] resolve_needs_label) => {
        fn resolve_needs_label(&self, node: $crate::NodeId, property: &str) -> bool {
            $crate::forward_ast_adapter!(@target self $i).resolve_needs_label(node, property)
        }
    };
    (@fwd [$i:tt] reverse_label) => {
        fn reverse_label(&self, target: $crate::NodeId) -> Option<String> {
            $crate::forward_ast_adapter!(@target self $i).reverse_label(target)
        }
    };
    (@fwd [$i:tt] tree_identities) => {
        fn tree_identities(&self) -> Vec<(String, $crate::NodeId)> {
            $crate::forward_ast_adapter!(@target self $i).tree_identities()
        }
    };
    (@fwd [$i:tt] property_names) => {
        fn property_names(&self, node: $crate::NodeId) -> Vec<String> {
            $crate::forward_ast_adapter!(@target self $i).property_names(node)
        }
    };
    (@fwd [$i:tt] resolves_references) => {
        fn resolves_references(&self) -> bool {
            $crate::forward_ast_adapter!(@target self $i).resolves_references()
        }
    };
    (@fwd [$i:tt] index_lookup) => {
        fn index_lookup(
            &self,
            scope: &str,
            property: &str,
            value: &$crate::Value,
        ) -> Option<Vec<$crate::NodeId>> {
            $crate::forward_ast_adapter!(@target self $i).index_lookup(scope, property, value)
        }
    };
    (@fwd [$i:tt] index_referrers) => {
        fn index_referrers(
            &self,
            scope: &str,
            property: &str,
            label: Option<&str>,
            target: $crate::NodeId,
        ) -> Option<Vec<$crate::NodeId>> {
            $crate::forward_ast_adapter!(@target self $i)
                .index_referrers(scope, property, label, target)
        }
    };
    (@fwd [$i:tt] non_tree_nodes) => {
        fn non_tree_nodes(&self) -> Vec<$crate::NodeId> {
            $crate::forward_ast_adapter!(@target self $i).non_tree_nodes()
        }
    };
    (@fwd [$i:tt] child_host) => {
        fn child_host(&self, node: $crate::NodeId) -> $crate::NodeId {
            $crate::forward_ast_adapter!(@target self $i).child_host(node)
        }
    };
    (@fwd [$i:tt] children_named) => {
        fn children_named(&self, node: $crate::NodeId, name: &str) -> Vec<$crate::NodeId> {
            $crate::forward_ast_adapter!(@target self $i).children_named(node, name)
        }
    };
    (@fwd [$i:tt] answers_to) => {
        fn answers_to(&self, node: $crate::NodeId, name: &str) -> bool {
            $crate::forward_ast_adapter!(@target self $i).answers_to(node, name)
        }
    };
    (@fwd [$i:tt] has_trait) => {
        fn has_trait(&self, node: $crate::NodeId, name: &str) -> bool {
            $crate::forward_ast_adapter!(@target self $i).has_trait(node, name)
        }
    };
    (@fwd [$i:tt] default_value) => {
        fn default_value(&self, node: $crate::NodeId) -> Option<$crate::Value> {
            $crate::forward_ast_adapter!(@target self $i).default_value(node)
        }
    };
    (@fwd [$i:tt] metadata) => {
        fn metadata(&self, node: $crate::NodeId, key: &str) -> Option<$crate::Value> {
            $crate::forward_ast_adapter!(@target self $i).metadata(node, key)
        }
    };
    (@fwd [$i:tt] aliased_metadata) => {
        fn aliased_metadata(&self, node: $crate::NodeId) -> &'static [&'static str] {
            $crate::forward_ast_adapter!(@target self $i).aliased_metadata(node)
        }
    };
    (@fwd [$i:tt] links) => {
        fn links(&self, node: $crate::NodeId) -> Vec<(String, $crate::NodeId)> {
            $crate::forward_ast_adapter!(@target self $i).links(node)
        }
    };
    (@fwd [$i:tt] backlinks) => {
        fn backlinks(&self, node: $crate::NodeId) -> Vec<(String, $crate::NodeId)> {
            $crate::forward_ast_adapter!(@target self $i).backlinks(node)
        }
    };
    (@fwd [$i:tt] resolve) => {
        fn resolve(
            &self,
            node: $crate::NodeId,
            property: &str,
            hint: Option<&str>,
        ) -> Option<$crate::NodeId> {
            $crate::forward_ast_adapter!(@target self $i).resolve(node, property, hint)
        }
    };
    (@fwd [$i:tt] external_ref) => {
        fn external_ref(
            &self,
            node: $crate::NodeId,
            property: &str,
            hint: Option<&str>,
        ) -> Option<$crate::ExternalRef> {
            $crate::forward_ast_adapter!(@target self $i).external_ref(node, property, hint)
        }
    };
    (@fwd [$i:tt] resolve_fragment) => {
        fn resolve_fragment(&self, node: $crate::NodeId, fragment: &str) -> Option<$crate::NodeId> {
            $crate::forward_ast_adapter!(@target self $i).resolve_fragment(node, fragment)
        }
    };
    (@fwd [$i:tt] ref_property) => {
        fn ref_property(&self, node: $crate::NodeId) -> Option<String> {
            $crate::forward_ast_adapter!(@target self $i).ref_property(node)
        }
    };
    (@fwd [$i:tt] ref_label) => {
        fn ref_label(&self, node: $crate::NodeId, property: &str) -> Option<String> {
            $crate::forward_ast_adapter!(@target self $i).ref_label(node, property)
        }
    };
    (@fwd [$i:tt] link_property) => {
        fn link_property(
            &self,
            source: $crate::NodeId,
            label: &str,
            target: $crate::NodeId,
            name: &str,
        ) -> Option<$crate::Value> {
            $crate::forward_ast_adapter!(@target self $i).link_property(source, label, target, name)
        }
    };
    (@fwd [$i:tt] document_by_ref) => {
        fn document_by_ref(&self, id: &str) -> Option<$crate::NodeId> {
            $crate::forward_ast_adapter!(@target self $i).document_by_ref(id)
        }
    };
    (@fwd [$i:tt] reverse_resolve) => {
        fn reverse_resolve(
            &self,
            node: $crate::NodeId,
            property: Option<&str>,
            hint: Option<&str>,
        ) -> Option<Vec<$crate::NodeId>> {
            $crate::forward_ast_adapter!(@target self $i).reverse_resolve(node, property, hint)
        }
    };
    (@fwd [$i:tt] descendants_named) => {
        fn descendants_named(
            &self,
            node: $crate::NodeId,
            name: &str,
        ) -> Option<Vec<($crate::NodeId, usize)>> {
            $crate::forward_ast_adapter!(@target self $i).descendants_named(node, name)
        }
    };
    (@fwd [$i:tt] descendants_where) => {
        fn descendants_where(
            &self,
            node: $crate::NodeId,
            name: &str,
            property: &str,
            value: &$crate::Value,
        ) -> Option<Vec<($crate::NodeId, usize)>> {
            $crate::forward_ast_adapter!(@target self $i)
                .descendants_where(node, name, property, value)
        }
    };
    (@fwd [$i:tt] prefetch_links) => {
        fn prefetch_links(&self, nodes: &[$crate::NodeId], dir: $crate::LinkDir) {
            $crate::forward_ast_adapter!(@target self $i).prefetch_links(nodes, dir)
        }
    };
    (@fwd [$i:tt] quantifier_bound) => {
        fn quantifier_bound(&self) -> usize {
            $crate::forward_ast_adapter!(@target self $i).quantifier_bound()
        }
    };
    (@fwd [$i:tt] allow_shell) => {
        fn allow_shell(&self) -> bool {
            $crate::forward_ast_adapter!(@target self $i).allow_shell()
        }
    };
    (@fwd [$i:tt] invocation_instant) => {
        fn invocation_instant(&self) -> Option<(i64, u32)> {
            $crate::forward_ast_adapter!(@target self $i).invocation_instant()
        }
    };
    (@fwd [$i:tt] provenance) => {
        fn provenance(&self, node: $crate::NodeId) -> $crate::Provenance {
            $crate::forward_ast_adapter!(@target self $i).provenance(node)
        }
    };
    (@fwd [$i:tt] has_node) => {
        fn has_node(&self, node: $crate::NodeId) -> bool {
            $crate::forward_ast_adapter!(@target self $i).has_node(node)
        }
    };
    (@fwd [$i:tt] provenance_list) => {
        fn provenance_list(&self, node: $crate::NodeId) -> $crate::ProvenanceList {
            $crate::forward_ast_adapter!(@target self $i).provenance_list(node)
        }
    };
    (@fwd [$i:tt] unit_scale) => {
        fn unit_scale(&self, expr: &str) -> Option<(f64, String)> {
            $crate::forward_ast_adapter!(@target self $i).unit_scale(expr)
        }
    };
}

/// An adapter view with the quantifier bound overridden (the CLI's
/// `--quantifier-bound`); every other method forwards to the wrapped
/// adapter.
pub struct QuantifierBound<'a, A: AstAdapter> {
    pub inner: &'a A,
    pub bound: usize,
}

impl<A: AstAdapter> AstAdapter for QuantifierBound<'_, A> {
    crate::forward_ast_adapter!(inner; except quantifier_bound);
    fn quantifier_bound(&self) -> usize {
        self.bound
    }
}

/// An adapter view with the shell stage enabled (the CLI's
/// `--allow-shell`); every other method forwards to the wrapped
/// adapter.
pub struct AllowShell<'a, A: AstAdapter> {
    pub inner: &'a A,
}

impl<A: AstAdapter> AstAdapter for AllowShell<'_, A> {
    crate::forward_ast_adapter!(inner; except allow_shell);
    fn allow_shell(&self) -> bool {
        true
    }
}

/// An adapter view with the invocation instant bound (the CLI's
/// startup clock, pinnable with `--now`); every other method
/// forwards to the wrapped adapter. Forwarding, not synthesizing:
/// the pinned invocation instant never becomes a node's
/// `:::instant` — absence is the true answer for a source that
/// records no time.
pub struct WithNow<'a, A: AstAdapter> {
    pub inner: &'a A,
    pub secs: i64,
    pub nanos: u32,
}

impl<A: AstAdapter> AstAdapter for WithNow<'_, A> {
    crate::forward_ast_adapter!(inner; except invocation_instant);
    fn invocation_instant(&self) -> Option<(i64, u32)> {
        Some((self.secs, self.nanos))
    }
}

impl AstAdapter for Box<dyn AstAdapter> {
    crate::forward_ast_adapter!(*);
}

#[cfg(test)]
mod forwarding {
    use super::*;

    /// The names of the `fn` items at `indent` within `block`.
    fn fn_names(block: &str, indent: &str) -> Vec<String> {
        block
            .lines()
            .filter_map(|l| l.strip_prefix(indent)?.strip_prefix("fn "))
            .map(|rest| {
                rest.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    /// The macro's method table is the trait's method list — a
    /// method added to `AstAdapter` without a forwarding arm would
    /// otherwise answer with the trait default inside every wrapper.
    #[test]
    fn the_macro_lists_every_trait_method() {
        let src = include_str!("adapter.rs");
        let trait_src = &src[src.find("pub trait AstAdapter {").unwrap()..];
        let trait_src = &trait_src[..trait_src.find("\n}\n").unwrap()];
        let in_trait = fn_names(trait_src, "    ");

        let table = &src[src
            .find("coverage test reads this list from the source.")
            .unwrap()..];
        let table = &table[table.find('\n').unwrap()..table.find(");").unwrap()];
        let listed: Vec<String> = table.split_whitespace().map(str::to_string).collect();

        assert_eq!(
            listed, in_trait,
            "the forwarding table drifted from the trait"
        );

        let macro_src = &src[src.find("macro_rules! forward_ast_adapter").unwrap()..];
        let macro_src = &macro_src[..macro_src.find("\n}\n").unwrap()];
        for m in &in_trait {
            assert!(
                macro_src.contains(&format!(
                    "(@unless [$i:tt] [{m} $($r:ident)*] {m}) => {{}};"
                )),
                "no except arm for {m}"
            );
            assert!(
                macro_src.contains(&format!("(@fwd [$i:tt] {m}) => {{")),
                "no forwarding arm for {m}"
            );
        }
    }

    /// Answers every method distinctively, so a wrapper that
    /// forwards one method to another (or to the default) is caught.
    struct Probe;

    fn n(k: u64) -> NodeId {
        NodeId(k)
    }

    fn int(v: &Value) -> u64 {
        match v {
            Value::Int(i) => *i as u64,
            _ => 0,
        }
    }

    impl AstAdapter for Probe {
        fn root(&self) -> NodeId {
            n(1)
        }
        fn children(&self, node: NodeId) -> Vec<NodeId> {
            vec![n(node.0 * 10)]
        }
        fn name(&self, node: NodeId) -> Option<String> {
            Some(format!("name{}", node.0))
        }
        fn parent(&self, node: NodeId) -> Option<NodeId> {
            Some(n(node.0 + 100))
        }
        fn traits(&self, node: NodeId) -> Vec<String> {
            vec![format!("trait{}", node.0)]
        }
        fn property(&self, node: NodeId, name: &str) -> Option<Value> {
            Some(Value::Str(format!("prop:{}:{name}", node.0)))
        }
        fn properties(&self, node: NodeId) -> Vec<(String, Value)> {
            vec![(format!("p{}", node.0), Value::Int(node.0 as i64))]
        }
        fn walks_for_references(&self) -> bool {
            true
        }
        fn can_refer_to(&self, source: NodeId, target: NodeId) -> Option<bool> {
            Some(source.0 < target.0)
        }
        fn reference_properties(&self, source: NodeId, target: NodeId) -> Vec<String> {
            vec![format!("rp{}-{}", source.0, target.0)]
        }
        fn resolve_needs_label(&self, node: NodeId, property: &str) -> bool {
            node.0 == 7 && property == "x"
        }
        fn reverse_label(&self, target: NodeId) -> Option<String> {
            Some(format!("rl{}", target.0))
        }
        fn tree_identities(&self) -> Vec<(String, NodeId)> {
            vec![("tree".into(), n(42))]
        }
        fn property_names(&self, node: NodeId) -> Vec<String> {
            vec![format!("pn{}", node.0)]
        }
        fn resolves_references(&self) -> bool {
            false
        }
        fn index_lookup(&self, scope: &str, property: &str, value: &Value) -> Option<Vec<NodeId>> {
            Some(vec![
                n((scope.len() + property.len()) as u64),
                n(int(value)),
            ])
        }
        fn index_referrers(
            &self,
            scope: &str,
            property: &str,
            label: Option<&str>,
            target: NodeId,
        ) -> Option<Vec<NodeId>> {
            Some(vec![n(
                (scope.len() + property.len() + label.map_or(0, str::len)) as u64 + target.0,
            )])
        }
        fn non_tree_nodes(&self) -> Vec<NodeId> {
            vec![n(99)]
        }
        fn child_host(&self, node: NodeId) -> NodeId {
            n(node.0 + 1000)
        }
        fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
            vec![n(node.0 + name.len() as u64)]
        }
        fn answers_to(&self, node: NodeId, name: &str) -> bool {
            node.0 == 3 && name == "alias"
        }
        fn has_trait(&self, node: NodeId, name: &str) -> bool {
            node.0 == 4 && name == "talias"
        }
        fn default_value(&self, node: NodeId) -> Option<Value> {
            Some(Value::Int(-(node.0 as i64)))
        }
        fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
            Some(Value::Str(format!("meta:{}:{key}", node.0)))
        }
        fn aliased_metadata(&self, node: NodeId) -> &'static [&'static str] {
            if node.0 == 5 { &["size"] } else { &["other"] }
        }
        fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
            vec![("link".into(), n(node.0 + 2))]
        }
        fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
            vec![("back".into(), n(node.0 + 3))]
        }
        fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
            Some(n(node.0
                + property.len() as u64
                + hint.map_or(0, |h| h.len() as u64)))
        }
        fn external_ref(
            &self,
            node: NodeId,
            property: &str,
            hint: Option<&str>,
        ) -> Option<ExternalRef> {
            Some(ExternalRef {
                document: format!("doc{}:{property}", node.0),
                within: hint.map(str::to_string),
            })
        }
        fn resolve_fragment(&self, node: NodeId, fragment: &str) -> Option<NodeId> {
            Some(n(node.0 + fragment.len() as u64 + 7))
        }
        fn ref_property(&self, node: NodeId) -> Option<String> {
            Some(format!("href{}", node.0))
        }
        fn ref_label(&self, node: NodeId, property: &str) -> Option<String> {
            Some(format!("rel{}:{property}", node.0))
        }
        fn link_property(
            &self,
            source: NodeId,
            label: &str,
            target: NodeId,
            name: &str,
        ) -> Option<Value> {
            Some(Value::Str(format!(
                "{}-{label}-{}-{name}",
                source.0, target.0
            )))
        }
        fn document_by_ref(&self, id: &str) -> Option<NodeId> {
            Some(n(id.len() as u64 + 50))
        }
        fn reverse_resolve(
            &self,
            node: NodeId,
            property: Option<&str>,
            hint: Option<&str>,
        ) -> Option<Vec<NodeId>> {
            Some(vec![n(node.0
                + property.map_or(0, |p| p.len() as u64) * 2
                + hint.map_or(0, |h| h.len() as u64) * 3)])
        }
        fn descendants_named(&self, node: NodeId, name: &str) -> Option<Vec<(NodeId, usize)>> {
            Some(vec![(n(node.0 + 9), name.len())])
        }
        fn descendants_where(
            &self,
            node: NodeId,
            name: &str,
            property: &str,
            value: &Value,
        ) -> Option<Vec<(NodeId, usize)>> {
            Some(vec![(n(node.0 + int(value)), name.len() + property.len())])
        }
        fn prefetch_links(&self, nodes: &[NodeId], dir: LinkDir) {
            PREFETCHED.with(|p| p.set(nodes.len() * 10 + matches!(dir, LinkDir::In) as usize));
        }
        fn quantifier_bound(&self) -> usize {
            77
        }
        fn allow_shell(&self) -> bool {
            true
        }
        fn invocation_instant(&self) -> Option<(i64, u32)> {
            Some((12, 34))
        }
        fn provenance(&self, node: NodeId) -> Provenance {
            Provenance {
                source: Some(format!("src{}", node.0)),
                ..Default::default()
            }
        }
        fn has_node(&self, node: NodeId) -> bool {
            node.0 == 8
        }
        fn provenance_list(&self, node: NodeId) -> ProvenanceList {
            ProvenanceList {
                entries: vec![self.provenance(node)],
                elided: node.0 as u32,
            }
        }
        fn unit_scale(&self, expr: &str) -> Option<(f64, String)> {
            Some((expr.len() as f64, format!("scale:{expr}")))
        }
    }

    thread_local! {
        static PREFETCHED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    /// Every method of `w` answers as `Probe` does, except those in
    /// `except`, which the wrapper defines itself.
    fn assert_forwards(w: &dyn AstAdapter, except: &[&str]) {
        let p = Probe;
        let v = Value::Int(6);
        macro_rules! same {
            ($m:ident $(, $a:expr)*) => {
                if !except.contains(&stringify!($m)) {
                    assert_eq!(w.$m($($a),*), p.$m($($a),*), stringify!($m));
                }
            };
        }
        same!(root);
        same!(children, n(2));
        same!(name, n(2));
        same!(parent, n(2));
        same!(traits, n(2));
        same!(property, n(2), "k");
        same!(properties, n(2));
        same!(walks_for_references);
        same!(can_refer_to, n(2), n(3));
        same!(reference_properties, n(2), n(3));
        same!(resolve_needs_label, n(7), "x");
        same!(reverse_label, n(2));
        same!(tree_identities);
        same!(property_names, n(2));
        same!(resolves_references);
        same!(index_lookup, "s", "pp", &v);
        same!(index_referrers, "s", "pp", Some("lbl"), n(2));
        same!(non_tree_nodes);
        same!(child_host, n(2));
        same!(children_named, n(2), "abc");
        same!(answers_to, n(3), "alias");
        same!(has_trait, n(4), "talias");
        same!(default_value, n(2));
        same!(metadata, n(2), "key");
        same!(aliased_metadata, n(5));
        same!(links, n(2));
        same!(backlinks, n(2));
        same!(resolve, n(2), "pp", Some("h"));
        same!(external_ref, n(2), "pp", Some("frag"));
        same!(resolve_fragment, n(2), "frag");
        same!(ref_property, n(2));
        same!(ref_label, n(2), "pp");
        same!(link_property, n(2), "lbl", n(3), "nm");
        same!(document_by_ref, "id");
        same!(reverse_resolve, n(2), Some("pp"), Some("h"));
        same!(descendants_named, n(2), "abc");
        same!(descendants_where, n(2), "abc", "pp", &v);
        if !except.contains(&"prefetch_links") {
            PREFETCHED.with(|c| c.set(0));
            w.prefetch_links(&[n(1), n(2)], LinkDir::In);
            assert_eq!(PREFETCHED.with(|c| c.get()), 21, "prefetch_links");
        }
        same!(quantifier_bound);
        same!(allow_shell);
        same!(invocation_instant);
        same!(provenance, n(2));
        same!(has_node, n(8));
        same!(has_node, n(9));
        same!(provenance_list, n(2));
        same!(unit_scale, "km");
    }

    #[test]
    fn every_wrapper_forwards_every_method() {
        let p = Probe;
        let w = QuantifierBound {
            inner: &p,
            bound: 5,
        };
        assert_forwards(&w, &["quantifier_bound"]);
        assert_eq!(w.quantifier_bound(), 5);

        let w = AllowShell { inner: &p };
        assert_forwards(&w, &["allow_shell"]);

        let w = WithNow {
            inner: &p,
            secs: 1,
            nanos: 2,
        };
        assert_forwards(&w, &["invocation_instant"]);
        assert_eq!(w.invocation_instant(), Some((1, 2)));

        let b: Box<dyn AstAdapter> = Box::new(Probe);
        assert_forwards(&b, &[]);

        let s = crate::source::Sourced {
            inner: Probe,
            encoding: "utf-8".into(),
            replaced: 0,
        };
        assert_forwards(&s, &["metadata"]);
        assert_eq!(s.metadata(n(2), "key"), p.metadata(n(2), "key"));
        assert_eq!(
            s.metadata(n(1), "encoding"),
            Some(Value::Str("utf-8".into()))
        );
    }
}
