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
            return ProvenanceList { entries: ProvenanceList::single(outer).entries, elided: self.elided };
        }
        ProvenanceList {
            entries: self.entries.into_iter().map(|p| p.or(outer.clone())).collect(),
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
    /// (ruling #30). The default is canonical equality. An adapter
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

    /// The external reference `node`'s `property` holds, if any: an
    /// adapter-defined identifier of a document *outside this
    /// arbor* — for html, the anchor's absolute URL, a relative
    /// `href` joined against the document's own URL. A `#fragment`
    /// part is the crossref *within* the target document and rides
    /// along (URI semantics). Consulted by `::property-->` when
    /// in-document resolution misses: a mounted document with that
    /// identifier answers (the fragment's element, else its root),
    /// an unmounted one lands among the run's unresolved external
    /// references for the host's acquisition loop. The engine
    /// itself never fetches.
    fn external_ref(&self, _node: NodeId, _property: &str, _hint: Option<&str>) -> Option<String> {
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

/// An adapter view with the quantifier bound overridden (the CLI's
/// `--quantifier-bound`); every other method forwards to the wrapped
/// adapter.
pub struct QuantifierBound<'a, A: AstAdapter> {
    pub inner: &'a A,
    pub bound: usize,
}

impl<A: AstAdapter> AstAdapter for QuantifierBound<'_, A> {
    fn root(&self) -> NodeId {
        self.inner.root()
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.inner.children(node)
    }
    fn name(&self, node: NodeId) -> Option<String> {
        self.inner.name(node)
    }

    /// Aliases (ruling #30) and trait aliases pass through: the
    /// wrapped adapter's word, not name equality on this layer.
    fn answers_to(&self, node: NodeId, name: &str) -> bool {
        self.inner.answers_to(node, name)
    }

    fn has_trait(&self, node: NodeId, name: &str) -> bool {
        self.inner.has_trait(node, name)
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.inner.parent(node)
    }
    fn traits(&self, node: NodeId) -> Vec<String> {
        self.inner.traits(node)
    }
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        self.inner.property(node, name)
    }
    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        self.inner.children_named(node, name)
    }
    fn default_value(&self, node: NodeId) -> Option<Value> {
        self.inner.default_value(node)
    }
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        self.inner.metadata(node, key)
    }
    fn aliased_metadata(&self, node: NodeId) -> &'static [&'static str] {
        self.inner.aliased_metadata(node)
    }
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.inner.links(node)
    }
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.inner.backlinks(node)
    }
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        self.inner.resolve(node, property, hint)
    }
    fn external_ref(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<String> {
        self.inner.external_ref(node, property, hint)
    }
    fn resolve_fragment(&self, node: NodeId, fragment: &str) -> Option<NodeId> {
        self.inner.resolve_fragment(node, fragment)
    }
    fn ref_property(&self, node: NodeId) -> Option<String> {
        self.inner.ref_property(node)
    }
    fn ref_label(&self, node: NodeId, property: &str) -> Option<String> {
        self.inner.ref_label(node, property)
    }
    fn link_property(
        &self,
        source: NodeId,
        label: &str,
        target: NodeId,
        name: &str,
    ) -> Option<Value> {
        self.inner.link_property(source, label, target, name)
    }
    fn document_by_ref(&self, id: &str) -> Option<NodeId> {
        self.inner.document_by_ref(id)
    }
    fn reverse_resolve(&self, n: NodeId, p: Option<&str>, h: Option<&str>) -> Option<Vec<NodeId>> {
        self.inner.reverse_resolve(n, p, h)
    }
    fn descendants_named(&self, n: NodeId, name: &str) -> Option<Vec<(NodeId, usize)>> {
        self.inner.descendants_named(n, name)
    }
    fn prefetch_links(&self, nodes: &[NodeId], dir: LinkDir) {
        self.inner.prefetch_links(nodes, dir)
    }
    fn quantifier_bound(&self) -> usize {
        self.bound
    }
    fn allow_shell(&self) -> bool {
        self.inner.allow_shell()
    }
    fn invocation_instant(&self) -> Option<(i64, u32)> {
        self.inner.invocation_instant()
    }
    fn provenance(&self, node: NodeId) -> Provenance {
        self.inner.provenance(node)
    }
    fn unit_scale(&self, expr: &str) -> Option<(f64, String)> {
        self.inner.unit_scale(expr)
    }
}

/// An adapter view with the shell stage enabled (the CLI's
/// `--allow-shell`); every other method forwards to the wrapped
/// adapter.
pub struct AllowShell<'a, A: AstAdapter> {
    pub inner: &'a A,
}

impl<A: AstAdapter> AstAdapter for AllowShell<'_, A> {
    fn root(&self) -> NodeId {
        self.inner.root()
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.inner.children(node)
    }
    fn name(&self, node: NodeId) -> Option<String> {
        self.inner.name(node)
    }

    /// Aliases (ruling #30) and trait aliases pass through: the
    /// wrapped adapter's word, not name equality on this layer.
    fn answers_to(&self, node: NodeId, name: &str) -> bool {
        self.inner.answers_to(node, name)
    }

    fn has_trait(&self, node: NodeId, name: &str) -> bool {
        self.inner.has_trait(node, name)
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.inner.parent(node)
    }
    fn traits(&self, node: NodeId) -> Vec<String> {
        self.inner.traits(node)
    }
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        self.inner.property(node, name)
    }
    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        self.inner.children_named(node, name)
    }
    fn default_value(&self, node: NodeId) -> Option<Value> {
        self.inner.default_value(node)
    }
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        self.inner.metadata(node, key)
    }
    fn aliased_metadata(&self, node: NodeId) -> &'static [&'static str] {
        self.inner.aliased_metadata(node)
    }
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.inner.links(node)
    }
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.inner.backlinks(node)
    }
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        self.inner.resolve(node, property, hint)
    }
    fn external_ref(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<String> {
        self.inner.external_ref(node, property, hint)
    }
    fn resolve_fragment(&self, node: NodeId, fragment: &str) -> Option<NodeId> {
        self.inner.resolve_fragment(node, fragment)
    }
    fn ref_property(&self, node: NodeId) -> Option<String> {
        self.inner.ref_property(node)
    }
    fn ref_label(&self, node: NodeId, property: &str) -> Option<String> {
        self.inner.ref_label(node, property)
    }
    fn link_property(
        &self,
        source: NodeId,
        label: &str,
        target: NodeId,
        name: &str,
    ) -> Option<Value> {
        self.inner.link_property(source, label, target, name)
    }
    fn document_by_ref(&self, id: &str) -> Option<NodeId> {
        self.inner.document_by_ref(id)
    }
    fn reverse_resolve(&self, n: NodeId, p: Option<&str>, h: Option<&str>) -> Option<Vec<NodeId>> {
        self.inner.reverse_resolve(n, p, h)
    }
    fn descendants_named(&self, n: NodeId, name: &str) -> Option<Vec<(NodeId, usize)>> {
        self.inner.descendants_named(n, name)
    }
    fn prefetch_links(&self, nodes: &[NodeId], dir: LinkDir) {
        self.inner.prefetch_links(nodes, dir)
    }
    fn quantifier_bound(&self) -> usize {
        self.inner.quantifier_bound()
    }
    fn allow_shell(&self) -> bool {
        true
    }
    fn invocation_instant(&self) -> Option<(i64, u32)> {
        self.inner.invocation_instant()
    }
    fn provenance(&self, node: NodeId) -> Provenance {
        self.inner.provenance(node)
    }
    fn unit_scale(&self, expr: &str) -> Option<(f64, String)> {
        self.inner.unit_scale(expr)
    }
}

/// An adapter view with the invocation instant bound (the CLI binds
/// it at startup, `--now` pins it); every other method forwards to
/// the wrapped adapter.
pub struct WithNow<'a, A: AstAdapter> {
    pub inner: &'a A,
    pub secs: i64,
    pub nanos: u32,
}

impl<A: AstAdapter> AstAdapter for WithNow<'_, A> {
    fn root(&self) -> NodeId {
        self.inner.root()
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.inner.children(node)
    }
    fn name(&self, node: NodeId) -> Option<String> {
        self.inner.name(node)
    }

    /// Aliases (ruling #30) and trait aliases pass through: the
    /// wrapped adapter's word, not name equality on this layer.
    fn answers_to(&self, node: NodeId, name: &str) -> bool {
        self.inner.answers_to(node, name)
    }

    fn has_trait(&self, node: NodeId, name: &str) -> bool {
        self.inner.has_trait(node, name)
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.inner.parent(node)
    }
    fn traits(&self, node: NodeId) -> Vec<String> {
        self.inner.traits(node)
    }
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        self.inner.property(node, name)
    }
    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        self.inner.children_named(node, name)
    }
    fn default_value(&self, node: NodeId) -> Option<Value> {
        self.inner.default_value(node)
    }
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        self.inner.metadata(node, key)
    }
    fn aliased_metadata(&self, node: NodeId) -> &'static [&'static str] {
        self.inner.aliased_metadata(node)
    }
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.inner.links(node)
    }
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.inner.backlinks(node)
    }
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        self.inner.resolve(node, property, hint)
    }
    fn external_ref(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<String> {
        self.inner.external_ref(node, property, hint)
    }
    fn resolve_fragment(&self, node: NodeId, fragment: &str) -> Option<NodeId> {
        self.inner.resolve_fragment(node, fragment)
    }
    fn ref_property(&self, node: NodeId) -> Option<String> {
        self.inner.ref_property(node)
    }
    fn ref_label(&self, node: NodeId, property: &str) -> Option<String> {
        self.inner.ref_label(node, property)
    }
    fn link_property(
        &self,
        source: NodeId,
        label: &str,
        target: NodeId,
        name: &str,
    ) -> Option<Value> {
        self.inner.link_property(source, label, target, name)
    }
    fn document_by_ref(&self, id: &str) -> Option<NodeId> {
        self.inner.document_by_ref(id)
    }
    fn reverse_resolve(&self, n: NodeId, p: Option<&str>, h: Option<&str>) -> Option<Vec<NodeId>> {
        self.inner.reverse_resolve(n, p, h)
    }
    fn descendants_named(&self, n: NodeId, name: &str) -> Option<Vec<(NodeId, usize)>> {
        self.inner.descendants_named(n, name)
    }
    fn prefetch_links(&self, nodes: &[NodeId], dir: LinkDir) {
        self.inner.prefetch_links(nodes, dir)
    }
    fn quantifier_bound(&self) -> usize {
        self.inner.quantifier_bound()
    }
    fn allow_shell(&self) -> bool {
        self.inner.allow_shell()
    }
    fn invocation_instant(&self) -> Option<(i64, u32)> {
        Some((self.secs, self.nanos))
    }
    // Forwarding, not synthesizing: the pinned invocation instant
    // never becomes a node's `:::instant` — absence is the true
    // answer for a source that records no time.
    fn provenance(&self, node: NodeId) -> Provenance {
        self.inner.provenance(node)
    }
    fn unit_scale(&self, expr: &str) -> Option<(f64, String)> {
        self.inner.unit_scale(expr)
    }
}
