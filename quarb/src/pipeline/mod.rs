//! Executor: evaluate a [`Query`] against an [`AstAdapter`].
//!
//! Navigation maps a node context step by step. Once a projection
//! runs, the context becomes a list of *capsae* — each a node with a
//! register (breadcrumbs) and a current topic — that the pipeline
//! commands transform, aggregate, push, and recall.

pub(crate) use crate::stdlib;
pub(crate) use crate::stdlib::flat_cell;
pub(crate) use crate::syntax::ast::visit::{self, Visitor as _};
pub(crate) use crate::syntax::ast::{
    Anchor, Arg, Axis, Branch, CmpOp, Command, FnCall, Group, InterpSeg, Matcher, Operand,
    PathElem, PredExpr, Predicate, Projection, PushBody, Query, Reach, RefProp, RegRef, Step,
};
pub(crate) use crate::thread::{Attachment, Position, Reg, Thread};
pub(crate) use crate::value::{SortKey, Table, Value};
pub(crate) use std::cmp::Ordering;
pub(crate) use std::collections::{HashMap, HashSet};

mod commands;
mod context;
pub mod env;
mod environ;
#[cfg(feature = "kaiv")]
pub mod kaiv_out;
pub mod library;
mod machine;
mod operand;
mod register;
mod run;

// The arith helpers are reached by path outside the tests.
#[cfg(test)]
pub(crate) use crate::stdlib::arith::*;
pub(crate) use crate::stdlib::compare::*;
pub(crate) use crate::stdlib::text::*;
pub use crate::thread::prov::*;
pub(crate) use commands::*;
pub(crate) use context::*;
pub(crate) use environ::*;
pub(crate) use machine::*;
pub(crate) use operand::*;
pub(crate) use register::*;
pub use run::*;

/// The nodes an anchor starts navigation from, resolved against a
/// thread's mark array. Singular anchors yield at most one node;
/// the plural forms yield every match in push order (the caller
/// forks one thread per node); a miss yields nothing, never an
/// error.

/// A command's input context as its expressions see it (`@*`, the
/// cross-capsa read): the snapshot, and an index of who stands
/// where, built on first use — `X:.r` over a big context must not
/// scan every peer per read.
pub(crate) struct Peers<X: Attachment> {
    pub(crate) caps: Vec<Thread<X>>,
    pub(crate) by_key: std::cell::OnceCell<HashMap<X::Key, Vec<usize>>>,
}

impl<X: Attachment> Peers<X> {
    fn new(caps: Vec<Thread<X>>) -> Peers<X> {
        Peers {
            caps,
            by_key: std::cell::OnceCell::new(),
        }
    }

    /// The peers standing at `key` (the graph's node), in context
    /// order.
    pub(crate) fn at(&self, key: &X::Key) -> &[usize] {
        let index = self.by_key.get_or_init(|| {
            let mut m: HashMap<X::Key, Vec<usize>> = HashMap::new();
            for (i, c) in self.caps.iter().enumerate() {
                m.entry(c.ext.key()).or_default().push(i);
            }
            m
        });
        index.get(key).map_or(&[], |v| v.as_slice())
    }
}

/// The capsa scope a value expression may reach: the register (for
/// `$.name` recalls) and the topic (`$_`). Empty where no capsa
/// exists yet (navigation predicates).
impl<X: Attachment> Thread<X> {
    /// The scope a pipeline command evaluates this thread's
    /// expressions in: its topic, register, members, captures and
    /// the attachment's view, at `ordinal` (1-based) in the
    /// command's input, with the command's snapshot and the
    /// invoking scope.
    pub(crate) fn scope<'a>(
        &'a self,
        ordinal: Option<usize>,
        peers: Option<&'a Peers<X>>,
        outer: Option<&'a Scope<'a, X>>,
    ) -> Scope<'a, X> {
        Scope {
            prov: Some(&self.prov),
            register: &self.register,
            members: &self.members,
            topic: self.topic.as_ref(),
            ordinal,
            captures: &self.captures,
            named: &self.named,
            ext: &self.ext,
            view: self.ext.view(),
            crossing: None,
            witness: None,
            peers,
            outer,
            driver: None,
        }
    }
}

pub(crate) struct Scope<'a, X: Attachment> {
    pub(crate) register: &'a [Reg],
    pub(crate) topic: Option<&'a Value>,
    /// The topic's provenance, where the scope belongs to a capsa.
    pub(crate) prov: Option<&'a Prov>,
    /// 1-based position in the current context, when known.
    pub(crate) ordinal: Option<usize>,
    /// Regex captures from the last matching filter (`$1` …).
    pub(crate) captures: &'a [String],
    /// The named groups of the last matching filter (`%+`).
    pub(crate) named: &'a [(String, String)],
    /// A group's members: the rows the capsa stands
    /// for. A relative operand from a group reads every member's
    /// node, as `:field` reads every member's record; empty on an
    /// ordinary capsa.
    pub(crate) members: &'a [Thread<X>],
    /// The command's input context (`@*`), where one exists — the
    /// snapshot rule: a command is the transition, so "the context"
    /// during its evaluation is what it received.
    pub(crate) peers: Option<&'a Peers<X>>,
    /// The invoking capsa's scope, one query body out (`$$__`);
    /// `None` at the top level.
    pub(crate) outer: Option<&'a Scope<'a, X>>,
    /// The driver's scope inside a join condition (`$$0`); `None`
    /// everywhere else.
    pub(crate) driver: Option<&'a Scope<'a, X>>,
    /// The attachment the scope stands in: the thread's own, or the
    /// walk's state where no thread exists yet.
    pub(crate) ext: &'a X,
    /// The attachment's view: what the scope says of it beyond the
    /// attachment itself.
    pub(crate) view: X::View,
    /// The move being evaluated, before it is the thread's state (the
    /// graph: the edge a hop is crossing).
    pub(crate) crossing: Option<&'a X::Crossing>,
    /// The witness under trial, where a correlation search evaluates
    /// a candidate binding; `None` reads the thread's own.
    pub(crate) witness: Option<&'a X::Witness>,
}

// Copy by hand: a derive would demand `X: Copy`, and the scope holds
// only borrows and the attachment's copyable view.
impl<'a, X: Attachment> Clone for Scope<'a, X> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<'a, X: Attachment> Copy for Scope<'a, X> {}

impl<'a, X: Attachment> Scope<'a, X> {
    /// The empty scope at an attachment: no thread exists yet.
    pub(crate) fn at(ext: &'a X) -> Scope<'a, X> {
        Scope {
            prov: None,
            register: &[],
            members: &[],
            topic: None,
            ordinal: None,
            captures: &[],
            named: &[],
            peers: None,
            outer: None,
            driver: None,
            ext,
            view: Default::default(),
            crossing: None,
            witness: None,
        }
    }
}

/// What the pipeline asks of the layer above it: the graph (or the
/// calculator, which has no graph and refuses what names one). Every
/// method takes a thread's attachment where the graph would take a
/// node, so the pipeline never names one. The method names avoid
/// the adapter's own, since an adapter is a host and both traits may
/// be in scope.
pub(crate) trait Host {
    type Ext: Attachment;
    /// The correlation trace a run carries; opaque here.
    type Trace: Default;
    /// The attachment of a thread standing at the root.
    fn root_ext(&self) -> Self::Ext;
    /// The attachment of a thread with no position (spec:
    /// Aggregating Pipe): what a reduction leaves.
    fn unplaced_ext(&self) -> Self::Ext;
    /// Whether the thread stands somewhere: false after a reduction
    /// and at the calculator.
    fn placed(&self, ext: &Self::Ext) -> bool;
    /// A unit's scale, for quantities.
    fn scale_of(&self, expr: &str) -> Option<(f64, String)>;
    fn property_of(&self, ext: &Self::Ext, name: &str) -> Option<Value>;
    fn properties_of(&self, ext: &Self::Ext) -> Vec<(String, Value)>;
    /// The node's property names (`:props`), in the adapter's order.
    fn property_names_of(&self, ext: &Self::Ext) -> Vec<String>;
    /// The properties a command will read on every thread: a remote
    /// host may fetch them for the whole frontier first.
    fn prefetch(&self, threads: &[Thread<Self::Ext>], names: &[&str]);
    /// Provenance at the thread's position, for this command.
    fn prov_at(&self, ext: &Self::Ext) -> Prov;
    fn origin_at(&self, ext: &Self::Ext) -> Origin;
    /// The quantifier bound N_max of the open-ended quantifiers.
    fn quantifier_bound(&self) -> usize;
    /// A reference to the thread's position, for `.name` in node
    /// context (`.name($$_)`) and the `$$_` operand; `None` where no
    /// arbor stands under the thread (the calculator).
    fn node_ref(&self, ext: &Self::Ext) -> Option<Value>;
    /// Whether a correlation witness admitted the thread.
    fn witnessed(&self, ext: &Self::Ext) -> bool;
    fn origins_at(&self, ext: &Self::Ext) -> Origins;
    /// The thread's position as a scalar (its name, text or value).
    fn node_scalar(&self, ext: &Self::Ext) -> Value;
    fn project(&self, ext: &Self::Ext, proj: &Projection) -> Value;
    /// The provenance metadata of an origin set (`:provenance` on a
    /// value), by key.
    fn origins_meta(&self, o: &Origins, key: &str) -> Option<Value>;
    /// A relative path operand at the thread: the values its nodes
    /// yield (the projection, else existence), one per node.
    fn rel(
        &self,
        ext: &Self::Ext,
        steps: &[PathElem],
        anchor: &Anchor,
        projection: Option<&Projection>,
        trace: &Self::Trace,
        scope: Scope<'_, Self::Ext>,
    ) -> Vec<Value>;
    /// The same path as the rows of a piped operand: one thread per
    /// node reached, standing there with the scope's state, its topic
    /// the projection, its provenance the node.
    fn rel_threads(
        &self,
        ext: &Self::Ext,
        steps: &[PathElem],
        anchor: &Anchor,
        projection: Option<&Projection>,
        trace: &Self::Trace,
        scope: Scope<'_, Self::Ext>,
    ) -> Vec<(Self::Ext, Option<Value>, Prov)>;
    /// A context reach (`$*k…`): the witness's node, or the served
    /// one, navigated and projected.
    fn ctx(
        &self,
        ext: &Self::Ext,
        index: Option<usize>,
        steps: &[PathElem],
        projection: Option<&Projection>,
        trace: &Self::Trace,
        scope: Scope<'_, Self::Ext>,
    ) -> Vec<Value>;
    /// The arrived-by edges (`@-`) and the one being crossed (`$-`).
    fn edges(&self, scope: Scope<'_, Self::Ext>, projection: Option<&Projection>) -> Vec<Value>;
    fn edge(&self, scope: Scope<'_, Self::Ext>, projection: Option<&Projection>) -> Vec<Value>;
    /// The command's input context as values: every peer's topic
    /// (`@_`, `projection` None), or its position projected
    /// (`@@_::prop`).
    fn capsae(&self, scope: Scope<'_, Self::Ext>, projection: Option<&Projection>) -> Vec<Value>;
    /// The context's positions (`@@_`): every peer's node as a
    /// reference, or projected over (`@@_::prop`).
    fn positions(&self, scope: Scope<'_, Self::Ext>, projection: Option<&Projection>) -> Vec<Value>;
    /// The keys (positions) an operand names, for a peer read.
    fn operand_keys(
        &self,
        ext: &Self::Ext,
        at: &Operand,
        trace: &Self::Trace,
        scope: Scope<'_, Self::Ext>,
    ) -> Vec<<Self::Ext as Attachment>::Key>;
    /// The scope's state standing where `at` stands: a thread made
    /// in a piped operand carries the scope's marks, witness and
    /// arrivals at the given position.
    fn restate(&self, scope: &Scope<'_, Self::Ext>, at: &Self::Ext) -> Self::Ext;
    fn exists_binding(
        &self,
        ext: &Self::Ext,
        conds: &[&PredExpr],
        trace: &Self::Trace,
        scope: Scope<'_, Self::Ext>,
    ) -> bool;
    fn subcontext_traced(
        &self,
        body: &Query,
        ext: &Self::Ext,
        trace: &Self::Trace,
        outer: Option<&Scope<'_, Self::Ext>>,
    ) -> (Value, Prov);
    /// A navigation command: every thread forks to the nodes the
    /// branch reaches from its anchor, and the results are merged
    /// once (two paths to one position with one register are one
    /// thread).
    fn navigate(
        &self,
        branch: &Branch,
        threads: Vec<Thread<Self::Ext>>,
        trace: &Self::Trace,
        outer: Option<&Scope<'_, Self::Ext>>,
        peers: Option<&Peers<Self::Ext>>,
    ) -> Vec<Thread<Self::Ext>>;
    fn node_to_json(&self, ext: &Self::Ext) -> Value;
    fn node_to_json_text(&self, ext: &Self::Ext) -> String;
    fn node_to_xml(&self, ext: &Self::Ext) -> String;
    fn node_table(&self, ext: &Self::Ext) -> Result<Table, String>;
    /// The thread's subtree rendered through the document
    /// vocabulary named (`markdown`, `html`, `atrep`, `conllu`).
    fn render_node(&self, ext: &Self::Ext, kind: &str) -> String;
    /// Values rendered through the same vocabulary.
    fn render_values(&self, values: &[Value], kind: &str) -> String;
    /// `@| tree`: the context's nodes drawn as a tree (spec: The
    /// tree); `column` evaluates a column expression at a node. Err
    /// where the host has no nodes to draw.
    fn render_tree(
        &self,
        starts: &[Self::Ext],
        spec: &crate::graph::tree::TreeSpec,
        column: &dyn Fn(&Self::Ext, &Operand) -> Value,
    ) -> Result<String, String>;
    /// A value as an XML element named `tag`.
    fn value_to_xml(&self, v: &Value, tag: &str) -> String;
}

/// Split provenance-bearing fields into the record's fields and
/// its provenance.
pub(crate) fn split_fields(fields: Vec<(String, Value, Prov)>) -> (Vec<(String, Value)>, Prov) {
    let mut vs = Vec::with_capacity(fields.len());
    let mut ps = Vec::with_capacity(fields.len());
    for (k, v, p) in fields {
        vs.push((k.clone(), v));
        ps.push((k, p));
    }
    (vs, Prov::record(ps))
}

/// Apply a positional predicate (`[n]` / `[a..b]`) to a list: keep
/// the `n`-th item (1-based; negative from the end; out of range
/// keeps nothing), or the inclusive span (ends optional, negative
/// allowed, clamped to the list).
pub(crate) fn positional<T>(items: Vec<T>, pred: &Predicate) -> Vec<T> {
    // A position list: the union of its items' selections, in the
    // items' own order, each once.
    if let Predicate::Positions(parts) = pred {
        let len = items.len();
        let mut keep = vec![false; len];
        for part in parts {
            let chosen: Vec<usize> = positional((0..len).collect(), part);
            for i in chosen {
                keep[i] = true;
            }
        }
        return items
            .into_iter()
            .zip(keep)
            .filter_map(|(it, k)| k.then_some(it))
            .collect();
    }
    match pred {
        Predicate::Index(n) => match resolve_pos(*n, items.len()) {
            Some(idx) => items.into_iter().nth(idx).into_iter().collect(),
            None => Vec::new(),
        },
        Predicate::Range(start, end) => {
            let len = items.len();
            let s = match start {
                None | Some(0) => 0,
                Some(v) if *v > 0 => (*v - 1) as usize,
                Some(v) => (len as i64 + *v).max(0) as usize,
            };
            // The exclusive upper bound of the inclusive end.
            let e = match end {
                None => len,
                Some(0) => 0,
                Some(v) if *v > 0 => (*v as usize).min(len),
                Some(v) => (len as i64 + *v + 1).max(0) as usize,
            };
            if s < e {
                items.into_iter().skip(s).take(e - s).collect()
            } else {
                Vec::new()
            }
        }
        // An expression predicate is not positional: both call sites
        // (the navigation-side `apply_predicates` and the `Command::
        // Select` dispatch) route `Predicate::Expr` to per-capsa
        // filtering before it reaches here, so this arm is defensive —
        // it must never silently pass every item through as a filter.
        Predicate::Expr(_) | Predicate::Computed(_) | Predicate::Positions(_) => items,
    }
}

/// The positional predicate a computed bracket's value spells (spec:
/// Index Predicates): an integer the position, a list of integers
/// the position list; `None` for any other value, which is then
/// taken for its truthiness.
pub(crate) fn positions_of(v: &Value) -> Option<Predicate> {
    match v {
        Value::Int(n) => Some(Predicate::Index(*n)),
        Value::List(items) => {
            let mut parts = Vec::with_capacity(items.len());
            for it in items.iter() {
                match it {
                    Value::Int(n) => parts.push(Predicate::Index(*n)),
                    _ => return None,
                }
            }
            Some(Predicate::Positions(parts))
        }
        _ => None,
    }
}

/// Walk a filter condition for `=~` comparisons and return the
/// groups of the *last* successful match (Perl's rule) — the
/// numbered groups, and the named ones as `(name, text)` pairs — or
/// `None` when no `=~` matched. Non-participating groups capture as
/// empty text.
pub(crate) type Captured = (Vec<String>, Vec<(String, String)>);

/// Resolve a 1-based, possibly negative position against a list of
/// `len` items. `None` when out of range (including 0).
pub(crate) fn resolve_pos(i: i64, len: usize) -> Option<usize> {
    if i > 0 && i as usize <= len {
        Some(i as usize - 1)
    } else if i < 0 && len as i64 + i >= 0 {
        Some((len as i64 + i) as usize)
    } else {
        None
    }
}
