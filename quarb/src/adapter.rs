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
//! supports. The reference cluster (`->`, `<-`, `-->`, `<--`,
//! `=>`), the search fast paths and provenance are documented on
//! the trait, group by group. See `doc/impl.tex`.

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

/// What one navigation step asks of an adapter, lowered for
/// [`AstAdapter::select`]: the axis, the name matcher, the
/// predicate conjuncts the engine can express as plain comparisons,
/// and — when the command after the step consumes them — a limit and
/// an order. An adapter answers from an index what it can and names
/// what it applied; the engine tests the rest.
#[derive(Debug, Clone)]
pub struct StepPlan<'a> {
    /// The axis: a child hop or a descendant search.
    pub axis: PlanAxis,
    /// The name the step matches, when it is one literal name.
    pub name: Option<&'a str>,
    /// The leading predicates lowered to comparisons, in order.
    pub conjuncts: Vec<Conjunct<'a>>,
    /// The step's `<trait>` filters, each one literal trait name
    /// (a clause with alternatives or a negation is not lowered).
    pub traits: Vec<&'a str>,
    /// How many results the command after the step keeps, when it
    /// keeps a prefix (`[..n]`, `@| first(n)`).
    pub limit: Option<usize>,
    /// Where an estimator may stop counting: past this many nodes
    /// or bytes the answer already exceeds the run's budget, so a
    /// listing, a header walk or an arena count stops there and
    /// reports `(counted, u64::MAX)`. Unbounded when no budget.
    pub cap: Cap,
    /// The property the command after the step orders by, and whether
    /// ascending.
    pub order: Option<(&'a str, bool)>,
}

/// Where counting may stop: the most nodes and bytes a budget
/// allows, so an estimator short-circuits at the first entry past
/// either ([`StepPlan::cap`], [`Catalog`] producers).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cap {
    pub nodes: Option<u64>,
    pub bytes: Option<u64>,
}

impl Cap {
    /// No cap: count everything (within the walk's own budget).
    pub const NONE: Cap = Cap {
        nodes: None,
        bytes: None,
    };

    /// Whether `nodes` counted or `bytes` summed so far already
    /// pass the cap: the walk may stop.
    pub fn past(&self, nodes: u64, bytes: u64) -> bool {
        self.nodes.is_some_and(|max| nodes > max) || self.bytes.is_some_and(|max| bytes > max)
    }
}

/// The axis of a [`StepPlan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanAxis {
    /// `/name`: the children.
    Child,
    /// `//name`: the descendants, each with its depth.
    Descendant,
}

/// One lowered predicate of a [`StepPlan`].
#[derive(Debug, Clone)]
pub enum Conjunct<'a> {
    /// `[::prop = value]` under the engine's standard equality, with
    /// a text value: what an index or a keyed read answers.
    Equals(&'a str, &'a Value),
    /// `[::prop OP literal]`: a comparison on one property, lowered
    /// for pricing (a listing's sizes, a store's statistics). An
    /// adapter never applies it in [`AstAdapter::select`]; the
    /// engine runs it. `Cmp::Eq` here is an equality on a
    /// non-text literal.
    Compare(&'a str, Cmp, &'a Value),
}

/// A comparison a [`Conjunct::Compare`] lowers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// `*=`: the property's text contains the literal.
    Contains,
}

impl Cmp {
    /// The operator as written.
    pub fn text(self) -> &'static str {
        match self {
            Cmp::Eq => "=",
            Cmp::Ne => "!=",
            Cmp::Lt => "<",
            Cmp::Le => "<=",
            Cmp::Gt => ">",
            Cmp::Ge => ">=",
            Cmp::Contains => "*=",
        }
    }

    /// Whether `value` stands to `literal` as the operator says,
    /// under the engine's value ordering.
    pub fn holds(self, value: &Value, literal: &Value) -> bool {
        use std::cmp::Ordering::*;
        match self {
            Cmp::Contains => match (value, literal) {
                (Value::Str(a), Value::Str(b)) => a.contains(b.as_str()),
                _ => false,
            },
            _ => {
                let o = value.compare(literal);
                match self {
                    Cmp::Eq => o == Equal,
                    Cmp::Ne => o != Equal,
                    Cmp::Lt => o == Less,
                    Cmp::Le => o != Greater,
                    Cmp::Gt => o == Greater,
                    Cmp::Ge => o != Less,
                    Cmp::Contains => unreachable!(),
                }
            }
        }
    }
}

/// What an adapter answers to a [`StepPlan`]: the nodes (each with
/// its depth from the step's base; children are depth 1) in
/// document order, and which conjuncts the adapter already applied
/// (the engine runs the rest).
#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub nodes: Vec<(NodeId, usize)>,
    pub applied: Vec<bool>,
}

/// What reaching one step's nodes would cost, as an adapter can
/// say before running it ([`AstAdapter::estimate`]): intervals, a
/// request count, a latency class and how sure the adapter is. A
/// host composes these along a query ([`crate::plan::Plan`]) and
/// refuses before running when a budget is exceeded.
#[derive(Debug, Clone, PartialEq)]
pub struct Estimate {
    /// The nodes the step yields, from the least to the most.
    pub nodes: (u64, u64),
    /// The bytes read to yield them, when the adapter can say (a
    /// warehouse's dry run, a file's size).
    pub bytes: Option<(u64, u64)>,
    /// Round trips to a remote, when any.
    pub requests: Option<u32>,
    pub latency: Latency,
    pub confidence: Confidence,
}

/// Where a step's cost is paid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Latency {
    /// The nodes are in memory.
    Memory,
    /// A local file or store.
    Local,
    /// A network round trip.
    Remote,
}

/// How much an [`Estimate`] is worth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    /// A guess from statistics or defaults.
    Guess,
    /// An upper bound (a clustered table's dry run, a partition's
    /// size): the actual may come in lower.
    Bound,
    /// Exact: the catalog or the store knows.
    Exact,
}

/// What a source knows about itself before it is read: how many
/// entries it holds (files under a directory, members of an
/// archive, rows of a table, documents on a shelf) and how many
/// bytes they come to — from a listing, a central directory, a
/// `stat`, a store's catalog, never from the contents. A latent
/// mount answers the plan from it; an [`Unopened`] source is only
/// it; an opened arena answers from its nodes instead.
#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    /// The entries, from the least to the most. A listing that
    /// stopped early reports `(counted, u64::MAX)`: unbounded.
    pub entries: (u64, u64),
    /// Their bytes, when the listing says.
    pub bytes: Option<(u64, u64)>,
    pub latency: Latency,
    pub confidence: Confidence,
    /// Which of the asked conjuncts the listing applied before
    /// counting (empty: none; a catalog that applies filters sets
    /// one flag per conjunct asked).
    pub applied: Vec<bool>,
    /// Whether the asked trait filters were all applied.
    pub traits_applied: bool,
}

impl Catalog {
    /// A catalog that applied nothing.
    pub fn plain(
        entries: (u64, u64),
        bytes: Option<(u64, u64)>,
        latency: Latency,
        confidence: Confidence,
    ) -> Catalog {
        Catalog {
            entries,
            bytes,
            latency,
            confidence,
            applied: Vec::new(),
            traits_applied: false,
        }
    }
}

/// What a catalog is asked for: the literal path under the source,
/// where counting may stop, and the step's filters a listing may
/// apply before counting (it says which in [`Catalog::applied`]).
pub struct Ask<'a> {
    pub scope: &'a [&'a str],
    pub cap: Cap,
    pub conjuncts: &'a [Conjunct<'a>],
    pub traits: &'a [&'a str],
}

impl<'a> Ask<'a> {
    /// A bare ask: the path and the cap, no filters.
    pub fn at(scope: &'a [&'a str], cap: Cap) -> Ask<'a> {
        Ask {
            scope,
            cap,
            conjuncts: &[],
            traits: &[],
        }
    }
}

/// A source known only by its catalog: a root with nothing under
/// it, answering the plan's estimate for a step from the root out
/// of the catalog. A front end plans and budgets a query against
/// it before paying to open the source — a directory of a hundred
/// thousand files, a tarball of books — and opens only within
/// budget.
pub struct Unopened(pub CatalogLookup);

/// A catalog by literal path from a source's root: `&[]` is the
/// whole source, `&["shelf", "emma.txt"]` one entry under it (a
/// folder's own listing, one member's header), `None` where the
/// path names nothing the catalog can see. The cap says where
/// counting may stop.
pub type CatalogLookup = Box<dyn Fn(&Ask<'_>) -> Option<Catalog>>;

impl Unopened {
    /// A source with one catalog, for its root alone.
    pub fn fixed(c: Catalog) -> Unopened {
        Unopened(Box::new(move |ask| ask.scope.is_empty().then(|| c.clone())))
    }
}

impl AstAdapter for Unopened {
    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, _: NodeId) -> Vec<NodeId> {
        Vec::new()
    }
    fn name(&self, _: NodeId) -> Option<String> {
        None
    }
    fn estimate(&self, scope: &[&str], plan: &StepPlan<'_>) -> Option<Estimate> {
        Estimate::at_catalog(&*self.0, scope, plan)
    }
}

impl Estimate {
    /// The estimate a catalog gives a step from the source's root.
    /// The entries are files or members, the step yields nodes,
    /// and the two meet only as bounds: a child step reaches at most
    /// the entries (`/*`, or a named one, a subset), an unfiltered
    /// descendant step reaches at least every entry (`//*`: each is
    /// a node, with what the reading finds under it), anything else
    /// is open. The bytes are the source's: what opening it reads.
    pub fn from_catalog(c: &Catalog, plan: &StepPlan<'_>) -> Estimate {
        // Filters the listing applied make the count a bound on the
        // matches: nothing under an entry answers a listing's
        // property, so a filtered descendant step is bounded too.
        let filtered = !plan.conjuncts.is_empty() || !plan.traits.is_empty();
        let applied = filtered
            && c.applied.len() == plan.conjuncts.len()
            && c.applied.iter().all(|&b| b)
            && (plan.traits.is_empty() || c.traits_applied);
        let rows = match (plan.axis, plan.name, filtered) {
            (PlanAxis::Child, _, _) => (0, c.entries.1),
            (PlanAxis::Descendant, None, false) => (c.entries.0, u64::MAX),
            (PlanAxis::Descendant, None, true) if applied => (0, c.entries.1),
            _ => (0, u64::MAX),
        };
        Estimate {
            nodes: rows,
            bytes: c.bytes,
            requests: Some(0),
            latency: c.latency,
            confidence: c.confidence.min(Confidence::Bound),
        }
    }

    /// The estimate a catalogued source gives a step at a literal
    /// `scope`, narrowing to the path the plan records: a bare hop
    /// by name is one entry when the catalog finds it (and costs
    /// nothing; the step that reads under it is priced by that
    /// entry's own catalog) and may read the whole when it does not
    /// (a node inside a document); a reading step at a path the
    /// catalog resolves is priced by it, and one the catalog cannot
    /// place is a bounded unknown under the source. `None` when the
    /// source itself has no catalog.
    pub fn at_catalog(
        lookup: &dyn Fn(&Ask<'_>) -> Option<Catalog>,
        scope: &[&str],
        plan: &StepPlan<'_>,
    ) -> Option<Estimate> {
        let whole = |scope: &[&str]| lookup(&Ask::at(scope, plan.cap));
        let bare =
            plan.conjuncts.is_empty() && plan.traits.is_empty() && plan.axis == PlanAxis::Child;
        if bare && let Some(name) = plan.name {
            let mut sub: Vec<&str> = scope.to_vec();
            sub.push(name);
            return Some(match whole(&sub) {
                Some(c) => Estimate {
                    nodes: (1, 1),
                    bytes: None,
                    requests: Some(0),
                    latency: c.latency,
                    confidence: Confidence::Exact,
                },
                // Not an entry the catalog sees (a node inside a
                // document): reaching it from the root may read the
                // whole; deeper, the whole was charged already.
                None => {
                    let whole = whole(&[])?;
                    if scope.is_empty() {
                        Estimate::from_catalog(&whole, plan)
                    } else {
                        Estimate::beneath_catalog(&whole)
                    }
                }
            });
        }
        // A reading step asks with its filters: a listing that can
        // apply them counts the matches alone.
        let ask = Ask {
            scope,
            cap: plan.cap,
            conjuncts: &plan.conjuncts,
            traits: &plan.traits,
        };
        Some(match lookup(&ask) {
            Some(c) => Estimate::from_catalog(&c, plan),
            None => Estimate::beneath_catalog(&whole(&[])?),
        })
    }

    /// The estimate a catalog gives a step deeper than the root:
    /// nothing is known of the nodes, and the bytes were counted at
    /// the root (`None` here, so the branch keeps them).
    pub fn beneath_catalog(c: &Catalog) -> Estimate {
        Estimate {
            nodes: (0, u64::MAX),
            bytes: None,
            requests: Some(0),
            latency: c.latency,
            confidence: Confidence::Bound,
        }
    }

    /// `self` followed by `next` along one branch: the nodes are
    /// the next step's, the bytes and requests add, the latency is
    /// the slower, the confidence the weaker.
    pub fn then(&self, next: &Estimate) -> Estimate {
        Estimate {
            nodes: next.nodes,
            bytes: match (self.bytes, next.bytes) {
                (Some(a), Some(b)) => Some((a.0.saturating_add(b.0), a.1.saturating_add(b.1))),
                (a, None) => a,
                (None, b) => b,
            },
            requests: match (self.requests, next.requests) {
                (Some(a), Some(b)) => Some(a + b),
                (a, None) => a,
                (None, b) => b,
            },
            latency: self.latency.max(next.latency),
            confidence: self.confidence.min(next.confidence),
        }
    }

    /// `self` beside `other` (two branches of one query): nodes,
    /// bytes and requests add.
    pub fn beside(&self, other: &Estimate) -> Estimate {
        Estimate {
            nodes: (
                self.nodes.0.saturating_add(other.nodes.0),
                self.nodes.1.saturating_add(other.nodes.1),
            ),
            ..self.then(other)
        }
    }
}

/// Nodes an arena walk may touch while estimating before it stops
/// counting: past this many the count is a lower bound.
pub const ARENA_ESTIMATE_BUDGET: u64 = 2_000_000;

/// An estimate for an adapter whose arbor is in memory (a parsed
/// document, a walked directory, an archive's member table, a
/// shelf of documents): the plan's base found by the literal
/// `scope` from the root (one hit per name, else unknown), then the
/// step's nodes counted — the children answering to the name, or
/// the descendants — with `bytes_of` summing what the adapter knows
/// per node. Exact; a bound under a conjunct or past the walk's
/// budget. Not the trait's default: for a lazy or remote adapter a
/// walk is a read.
pub fn arena_estimate<A: AstAdapter + ?Sized>(
    adapter: &A,
    scope: &[&str],
    plan: &StepPlan<'_>,
    latency: Latency,
    bytes_of: Option<&dyn Fn(NodeId) -> Option<u64>>,
) -> Option<Estimate> {
    let mut at = adapter.root();
    for name in scope {
        let mut hits = adapter.children_named(at, name).into_iter();
        at = hits.next()?;
        if hits.next().is_some() {
            return None;
        }
    }
    let matches = |c: NodeId| plan.name.is_none_or(|n| adapter.answers_to(c, n));
    let mut n: u64 = 0;
    let mut bytes: u64 = 0;
    let mut cut = false;
    // Counts one node; `true` once the cap is passed: stop.
    let mut count = |c: NodeId| {
        if matches(c) {
            n += 1;
            if let Some(f) = bytes_of {
                bytes += f(c).unwrap_or(0);
            }
        }
        plan.cap.past(n, bytes)
    };
    match plan.axis {
        PlanAxis::Child => {
            for c in adapter.children(at) {
                if count(c) {
                    cut = true;
                    break;
                }
            }
        }
        PlanAxis::Descendant => {
            let mut visited: u64 = 0;
            let mut stack = adapter.children(at);
            while let Some(c) = stack.pop() {
                visited += 1;
                if visited > ARENA_ESTIMATE_BUDGET || count(c) {
                    cut = true;
                    break;
                }
                stack.extend(adapter.children(c));
            }
        }
    }
    let filtered = !plan.conjuncts.is_empty();
    Some(Estimate {
        nodes: if cut {
            (n, u64::MAX)
        } else if filtered {
            (0, n)
        } else {
            (n, n)
        },
        bytes: bytes_of.map(|_| if cut { (bytes, u64::MAX) } else { (0, bytes) }),
        requests: Some(0),
        latency,
        confidence: if cut || filtered {
            Confidence::Bound
        } else {
            Confidence::Exact
        },
    })
}

/// [`AstAdapter::select`]'s default, through the individual hooks:
/// a child hop by `children_named`; a descendant search by
/// `descendants_where` when the plan leads with an equality the
/// adapter indexes, else `descendants_named`. A wrapper that
/// translates node ids forwards `select` to its inner adapter and
/// falls back to this over its own hooks.
pub fn select_by_hooks<A: AstAdapter + ?Sized>(
    adapter: &A,
    from: NodeId,
    plan: &StepPlan<'_>,
) -> Option<Selection> {
    let name = plan.name?;
    match plan.axis {
        PlanAxis::Child => Some(Selection {
            nodes: adapter
                .children_named(from, name)
                .into_iter()
                .map(|n| (n, 1))
                .collect(),
            applied: vec![false; plan.conjuncts.len()],
        }),
        PlanAxis::Descendant => {
            if let Some(Conjunct::Equals(prop, value)) = plan.conjuncts.first()
                && let Some(nodes) = adapter.descendants_where(from, name, prop, value)
            {
                let mut applied = vec![false; plan.conjuncts.len()];
                applied[0] = true;
                return Some(Selection { nodes, applied });
            }
            let nodes = adapter.descendants_named(from, name)?;
            Some(Selection {
                nodes,
                applied: vec![false; plan.conjuncts.len()],
            })
        }
    }
}

/// The probe behind [`AstAdapter::has_node`]'s default: run a
/// closure that may panic on a foreign node id, answering `false`
/// when it does, without the panic reaching stderr. The process's
/// hook is wrapped once (the first probe installs the wrapper,
/// which defers to whatever hook was there) and a thread-local
/// flag silences it only on the probing thread.
mod probe {
    use std::cell::Cell;
    use std::sync::Once;

    thread_local! {
        static SILENT: Cell<bool> = const { Cell::new(false) };
    }
    static INSTALL: Once = Once::new();

    pub(super) fn silently<R: Default>(f: impl FnOnce() -> R) -> R {
        INSTALL.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                if !SILENT.with(|s| s.get()) {
                    previous(info);
                }
            }));
        });
        SILENT.with(|s| s.set(true));
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_default();
        SILENT.with(|s| s.set(false));
        out
    }
}

/// The interface a data source implements to be queried by Quarb.
///
/// An adapter maps its native structure onto the arbor model: a tree
/// backbone whose edges carry *names*. The engine drives navigation
/// purely through this trait, so the same query language runs over
/// any adapter. Three methods are required (`root`, `children`,
/// `name`); every other one has a default an adapter overrides
/// only where its substrate has more to say. The methods fall into
/// groups:
///
/// - **The tree**: `root`, `children`, `name`, `parent`,
///   `child_host` (a transparent link lends its target's
///   children), `non_tree_nodes` (joined by crosslinks alone),
///   `has_node`.
/// - **Naming and traits**: `answers_to` (per-node spelling
///   aliases, consulted wherever a literal name is matched),
///   `traits`, `has_trait`.
/// - **Properties and metadata**: `property`, `properties`,
///   `property_names`, `default_value`, `metadata`,
///   `aliased_metadata`.
/// - **References**, three protocols the engine consults in this
///   order. *Materialized edges*: `links` / `backlinks` (the `->` /
///   `<-` hops) with `link_property` and the `prefetch_links`
///   batching hint. *Property resolution* (`::prop-->`, `-->`,
///   `<--`): `resolve`, `ref_property`, `ref_label`,
///   `resolve_needs_label`, `reverse_resolve`, `reverse_label`,
///   `can_refer_to`, `reference_properties`,
///   `walks_for_references`, `resolves_references`. *Cross-document*
///   (a reference naming another document): `external_ref`,
///   `resolve_fragment`, `document_by_ref`, `tree_identities`; the
///   engine joins the identity against the tree the reference was
///   written in and lands through the host's registered targets,
///   then `document_by_ref`, then the identities, and records what
///   it could not reach. The engine performs no I/O for any of it.
/// - **Declared indexes** (a model's `index SCOPE::prop;`):
///   `index_lookup`, `index_referrers`.
/// - **Search fast paths**: [`select`](Self::select) — one plan
///   per step (axis, name matcher, the conjuncts the engine can
///   lower, a limit, an order) an adapter may answer from an index;
///   its default folds `children_named`, `descendants_named` and
///   `descendants_where`, which an adapter may still implement
///   individually. A fast path changes cost, never the answer.
/// - **Evaluation hints**: `quantifier_bound` (the depth the
///   adapter's own structures need; a run may override it),
///   `unit_scale`.
/// - **Provenance**: `provenance`, `provenance_list`.
///
/// Pairs the engine relies on being observationally equal, so an
/// override must keep them so: `children_named(n, x)` is
/// `children(n)` filtered by `answers_to(_, x)` (except deliberate
/// container-scoped aliasing, documented on the method);
/// `has_trait(n, t)` is `t ∈ traits(n)`; `property_names(n)` covers
/// the names of `properties(n)`, the `ref_property`, and the child
/// names; `provenance_list(n)`'s first entry is `provenance(n)`;
/// `select` answers exactly what the walk would find. The
/// conformance suite ([`crate::conformance::check`]) tests these
/// over a sample of an adapter's nodes.
///
/// Optional capabilities with a single implementor each — fine to
/// leave at the default: `unit_scale` (kaiv's units),
/// `descendants_where` (the corpus reading's positional index),
/// `child_host` (filesystem symlinks), `aliased_metadata` (git's
/// closed surface), `provenance_list` (kaiv's layered sources),
/// `index_lookup` / `index_referrers` (a model's declared indexes).
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

    /// The search fast path: the nodes one step reaches from `from`
    /// under `plan`, answered from an index — `None` keeps the
    /// engine's walk (the default folds `children_named`,
    /// `descendants_named` and `descendants_where`:
    /// [`select_by_hooks`]). `Some` must be exactly what the walk
    /// would find for the conjuncts it marks applied, under the
    /// engine's standard equality; the engine tests the remaining
    /// conjuncts and every other predicate on the nodes returned. A
    /// child plan's answer is never re-filtered by name (the adapter
    /// owns the name test, as `children_named` does); a descendant
    /// plan's nodes still pass the nameless tests.
    fn select(&self, from: NodeId, plan: &StepPlan<'_>) -> Option<Selection> {
        select_by_hooks(self, from, plan)
    }
    /// The dry-run form of [`select`](Self::select): which of the
    /// plan's conjuncts the adapter would apply, decided from its
    /// catalog alone (a column's existence and type, an index's
    /// presence), with no node in hand. `scope` is the literal path
    /// from the root to the step's base (`["invoices"]` for
    /// `/invoices/*[...]`), empty at the root. `None` when the
    /// adapter cannot say — the static plan then counts every
    /// conjunct as the engine's.
    fn select_plan(&self, _scope: &[&str], _plan: &StepPlan<'_>) -> Option<Vec<bool>> {
        None
    }
    /// What reaching one step's nodes would cost, said from the
    /// catalog with no node in hand (`scope` as for
    /// [`select_plan`](Self::select_plan)): a warehouse's dry run,
    /// a table's statistics, a file's size, an arena's count.
    /// `None` where the adapter has no estimator — the plan then
    /// reads "unknown" for the step.
    fn estimate(&self, _scope: &[&str], _plan: &StepPlan<'_>) -> Option<Estimate> {
        None
    }
    /// How many nodes one step reaches from `from` under `plan`,
    /// without materializing them — `@| count` right after the step
    /// asks before walking. `None` walks; `Some` must be exactly the
    /// walk's count for the conjuncts the plan carries (every one of
    /// them, or none: a count cannot be partially applied). SQL:
    /// `SELECT COUNT(*)`; a document index: the bucket's length.
    fn count(&self, _from: NodeId, _plan: &StepPlan<'_>) -> Option<usize> {
        None
    }
    /// A batching hint, the projection twin of `prefetch_links`: the
    /// engine is about to read the properties `names` of every node
    /// in `nodes` (the next command projects them). A remote adapter
    /// fetches the columns for the frontier in one round trip;
    /// semantics are unchanged either way.
    fn prefetch_properties(&self, _nodes: &[NodeId], _names: &[&str]) {}
    /// A batching hint: the engine is about to follow `dir`
    /// crosslinks from every node in `nodes`. An adapter backed by
    /// a store may fetch them in one statement; semantics are
    /// unchanged either way.
    fn prefetch_links(&self, _nodes: &[NodeId], _dir: LinkDir) {}

    /// The quantifier bound N_max: the depth to which open-ended path
    /// quantifiers (`+`, `*`, `{m,}`) expand, and the ceiling of any
    /// explicit `{m,n}` (the effective upper bound is min(n, N_max)).
    /// An adapter whose natural structures run deep may raise it; a
    /// run may override it (`Run::quantifier_bound`, the CLI's
    /// `--quantifier-bound`). The shell gate and the invocation
    /// instant are the run's alone (`Run::allow_shell`, `Run::now`).
    fn quantifier_bound(&self) -> usize {
        32
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
        // An adapter that indexes its arena panics on a foreign id;
        // the probe catches that. The panic hook is installed once
        // for the process and silenced per thread while probing —
        // never swapped, so other threads keep theirs.
        probe::silently(|| self.parent(node).is_some() || self.name(node).is_some())
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
/// one definition behind the wrappers (the boxed carrier, the
/// decoding layer, the mount and model wrappers), so a method added to the
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
            descendants_where select select_plan estimate count
            prefetch_properties
            prefetch_links quantifier_bound
            provenance has_node
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
    (@unless [$i:tt] [select $($r:ident)*] select) => {};
    (@unless [$i:tt] [select_plan $($r:ident)*] select_plan) => {};
    (@unless [$i:tt] [estimate $($r:ident)*] estimate) => {};
    (@unless [$i:tt] [count $($r:ident)*] count) => {};
    (@unless [$i:tt] [prefetch_properties $($r:ident)*] prefetch_properties) => {};
    (@unless [$i:tt] [prefetch_links $($r:ident)*] prefetch_links) => {};
    (@unless [$i:tt] [quantifier_bound $($r:ident)*] quantifier_bound) => {};
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
    (@fwd [$i:tt] select) => {
        fn select(
            &self,
            from: $crate::NodeId,
            plan: &$crate::adapter::StepPlan<'_>,
        ) -> Option<$crate::adapter::Selection> {
            $crate::forward_ast_adapter!(@target self $i).select(from, plan)
        }
    };
    (@fwd [$i:tt] select_plan) => {
        fn select_plan(
            &self,
            scope: &[&str],
            plan: &$crate::adapter::StepPlan<'_>,
        ) -> Option<Vec<bool>> {
            $crate::forward_ast_adapter!(@target self $i).select_plan(scope, plan)
        }
    };
    (@fwd [$i:tt] estimate) => {
        fn estimate(
            &self,
            scope: &[&str],
            plan: &$crate::adapter::StepPlan<'_>,
        ) -> Option<$crate::adapter::Estimate> {
            $crate::forward_ast_adapter!(@target self $i).estimate(scope, plan)
        }
    };
    (@fwd [$i:tt] count) => {
        fn count(&self, from: $crate::NodeId, plan: &$crate::adapter::StepPlan<'_>) -> Option<usize> {
            $crate::forward_ast_adapter!(@target self $i).count(from, plan)
        }
    };
    (@fwd [$i:tt] prefetch_properties) => {
        fn prefetch_properties(&self, nodes: &[$crate::NodeId], names: &[&str]) {
            $crate::forward_ast_adapter!(@target self $i).prefetch_properties(nodes, names)
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
        fn select(&self, from: NodeId, plan: &StepPlan<'_>) -> Option<Selection> {
            Some(Selection {
                nodes: vec![(n(from.0 + plan.conjuncts.len() as u64 + 1), 3)],
                applied: vec![true; plan.conjuncts.len()],
            })
        }
        fn select_plan(&self, scope: &[&str], plan: &StepPlan<'_>) -> Option<Vec<bool>> {
            Some(vec![scope.len() == 1; plan.conjuncts.len()])
        }
        fn estimate(&self, scope: &[&str], plan: &StepPlan<'_>) -> Option<Estimate> {
            Some(Estimate {
                nodes: (scope.len() as u64, plan.conjuncts.len() as u64 + 9),
                bytes: None,
                requests: Some(1),
                latency: Latency::Local,
                confidence: Confidence::Bound,
            })
        }
        fn count(&self, from: NodeId, plan: &StepPlan<'_>) -> Option<usize> {
            Some(from.0 as usize + plan.conjuncts.len() + 40)
        }
        fn prefetch_properties(&self, nodes: &[NodeId], names: &[&str]) {
            PREFETCHED.with(|p| p.set(nodes.len() * 100 + names.len()));
        }
        fn prefetch_links(&self, nodes: &[NodeId], dir: LinkDir) {
            PREFETCHED.with(|p| p.set(nodes.len() * 10 + matches!(dir, LinkDir::In) as usize));
        }
        fn quantifier_bound(&self) -> usize {
            77
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
        let plan = StepPlan {
            traits: Vec::new(),
            cap: Default::default(),
            axis: PlanAxis::Descendant,
            name: Some("abc"),
            conjuncts: vec![Conjunct::Equals("pp", &v)],
            limit: None,
            order: None,
        };
        if !except.contains(&"select") {
            assert_eq!(
                w.select(n(2), &plan).map(|s| s.nodes),
                p.select(n(2), &plan).map(|s| s.nodes),
                "select"
            );
        }
        same!(count, n(2), &plan);
        same!(select_plan, &["t"], &plan);
        same!(estimate, &["t"], &plan);
        if !except.contains(&"prefetch_properties") {
            PREFETCHED.with(|p| p.set(0));
            w.prefetch_properties(&[n(1), n(2), n(3)], &["a", "b"]);
            assert_eq!(PREFETCHED.with(|p| p.get()), 302, "prefetch_properties");
        }
        if !except.contains(&"prefetch_links") {
            PREFETCHED.with(|c| c.set(0));
            w.prefetch_links(&[n(1), n(2)], LinkDir::In);
            assert_eq!(PREFETCHED.with(|c| c.get()), 21, "prefetch_links");
        }
        same!(quantifier_bound);
        same!(provenance, n(2));
        same!(has_node, n(8));
        same!(has_node, n(9));
        same!(provenance_list, n(2));
        same!(unit_scale, "km");
    }

    #[test]
    fn every_wrapper_forwards_every_method() {
        let p = Probe;
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
