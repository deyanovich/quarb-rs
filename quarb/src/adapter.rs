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
    /// Where the datum came from (a URI, a path, a repo).
    pub source: Option<String>,
    /// The datum's own instant — `(secs, nanos, offset_min)`, the
    /// shape `Value::Instant` carries. Never the invocation clock.
    pub instant: Option<(i64, u32, Option<i16>)>,
    /// The source-assigned data-point identifier (kaiv `#dpid`).
    pub dpid: Option<String>,
}

impl Provenance {
    /// Component-wise layering: `self` (inner, more specific) wins;
    /// missing components fill from `outer`.
    pub fn or(self, outer: Provenance) -> Provenance {
        Provenance {
            source: self.source.or(outer.source),
            instant: self.instant.or(outer.instant),
            dpid: self.dpid.or(outer.dpid),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.source.is_none() && self.instant.is_none() && self.dpid.is_none()
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
        if let Some((secs, nanos, offset)) = self.instant {
            out.push('@');
            out.push_str(&crate::temporal::format_instant(secs, nanos, offset));
        }
        if let Some(dpid) = &self.dpid {
            out.push('#');
            out.push_str(dpid);
        }
        Some(out)
    }
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

    /// Resolve a cross-reference: `::property~>hint` maps `node`'s
    /// `property` (a value that references another node) to its target,
    /// with an optional adapter-specific relation `hint`. A JSON
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
