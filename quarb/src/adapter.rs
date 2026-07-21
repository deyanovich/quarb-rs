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
//! supports. Crosslink resolution (`~>`) and pattern search (`=>`)
//! are still planned. See `doc/impl.tex`.

use crate::value::Value;

/// An opaque handle to a node in an arbor.
///
/// The engine treats a `NodeId` as an opaque token: it is minted and
/// interpreted solely by the adapter that produced it. The `u64`
/// payload is an adapter-private index or key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u64);

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
    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        self.children(node)
            .into_iter()
            .filter(|&c| self.name(c).as_deref() == Some(name))
            .collect()
    }

    /// The default projection of `node` — bare `::`, adapter-specific
    /// (a filesystem adapter returns file content).
    fn default_value(&self, _node: NodeId) -> Option<Value> {
        None
    }

    /// Adapter-defined metadata — `;;;key` (a filesystem adapter's
    /// `size`, `modified`, `permissions`, …). `None` if absent.
    fn metadata(&self, _node: NodeId, _key: &str) -> Option<Value> {
        None
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
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.inner.links(node)
    }
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.inner.backlinks(node)
    }
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        self.inner.resolve(node, property, hint)
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
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.inner.links(node)
    }
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.inner.backlinks(node)
    }
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        self.inner.resolve(node, property, hint)
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
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.inner.links(node)
    }
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.inner.backlinks(node)
    }
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        self.inner.resolve(node, property, hint)
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
    fn unit_scale(&self, expr: &str) -> Option<(f64, String)> {
        self.inner.unit_scale(expr)
    }
}
