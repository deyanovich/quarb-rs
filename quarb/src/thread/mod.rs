//! The thread layer: the forking machine. A query runs as a list of
//! threads; the machine has three moves, fork (one thread to m),
//! terminate (to zero), and the whole-list map, and every forking
//! and joining construct of the language is one of them, from the
//! hops of the graph to the spread and the iteration pipe of the
//! values. Both layers above reach the machine directly for forking.
//!
//! A thread carries a topic (the value it works on) and a register
//! (the values pushed on the walk) of its own, and one attachment,
//! the per-thread state a layer above hangs on it: the graph's
//! position, marks, correlation witness and arrived edges; nothing
//! for the calculator. The one contract: on a fork every attached
//! state is copied into each child and never shared afterwards
//! (the fork is the clone), threads never communicate, and the
//! only whole-machine operations are the whole-list maps. The spec
//! calls everything a thread carries at one moment its capsa; the
//! engine has no such type, each layer keeps its own part.
use crate::value::Value;

pub mod prov;
use prov::{Origin, Prov};

/// Where a thread stood, as provenance records it: a number whose
/// meaning the layer above gives (the graph: its node id). The
/// thread and the pipeline carry it and compare it; only the graph
/// reads it as a place.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Position(pub u64);

/// What a layer above hangs on every thread: copied on a fork,
/// never shared. A scope borrows the attachment whole; beside it
/// the attachment's view is the little an expression may see that
/// the attachment itself does not say (the graph: whether the scope
/// belongs to a thread standing on its node), and its crossing is
/// the move being evaluated before it is the thread's state (the
/// graph: the edge a hop is crossing, for `$-`), as is the witness a
/// correlation search tries. Its key is what a context is indexed by
/// (the graph's node).
pub(crate) trait Attachment: Clone {
    type View: Copy + Default;
    type Crossing;
    /// What a correlation binds and tries: the witness under trial,
    /// read in place of the thread's own while a search evaluates.
    type Witness: ?Sized;
    type Key: std::hash::Hash + Eq + Clone + 'static;
    /// What a walk knows of the edge it crossed (the graph: the
    /// round, the hops, the budget's total — spec: Edge facts).
    type Walk;
    fn view(&self) -> Self::View;
    fn key(&self) -> Self::Key;
    /// The walk that ended at this attachment, if one did and no
    /// command has read it yet: the last edge crossed and its facts,
    /// in scope for the one command after the walk.
    fn walk_exit(&self) -> Option<(&Self::Crossing, &Self::Walk)> {
        None
    }
    /// A command has run: the walk's facts leave the scope.
    fn end_walk(&mut self) {}
    /// The edge the thread most recently walked, where no walk's
    /// exit stands in scope (the graph: a plain hop's arrived-by
    /// edge, read by `$-` in the command after the hop).
    fn crossing(&self) -> Option<&Self::Crossing> {
        None
    }
    /// `@| collapse`: fold another thread's attachment into this one
    /// (the graph: the arriving edges united).
    fn gather(&mut self, _other: Self) {}
    /// The identity of the edge this thread arrived by, for
    /// `collapse(-e)`; `None` where none.
    fn crossing_key(&self) -> Option<String> {
        None
    }
    /// A thread standing where this one stands, carrying nothing.
    fn bare(&self) -> Self;
    /// Whether a scope with this view belongs to a thread standing
    /// somewhere (the graph: on a node), as against a navigation
    /// scope with no thread yet.
    fn standing(view: &Self::View) -> bool;
}

/// The calculator's attachment: nothing.
impl Attachment for () {
    type View = ();
    type Crossing = ();
    type Witness = ();
    type Key = ();
    type Walk = ();
    fn view(&self) -> Self::View {}
    fn key(&self) -> Self::Key {}
    fn bare(&self) -> Self {}
    fn standing(_: &()) -> bool {
        false
    }
}

/// One entry in a capsa's register: the value, where it was read
/// (`prov`), and where it was pushed (`site`).
#[derive(Clone)]
pub(crate) struct Reg {
    pub(crate) name: Option<String>,
    pub(crate) value: Value,
    pub(crate) prov: Prov,
    /// Where the push happened. Unread: the node read goes by the
    /// node a regula is attached to (spec: Attached Regulae), not
    /// by where its push stood.
    #[allow(dead_code)]
    pub(crate) site: Origin,
}

/// One execution path: its topic, its register, the members of a
/// group it stands for, the captures of its last match, and the
/// attachment of the layer above.
#[derive(Clone)]
pub(crate) struct Thread<X: Attachment> {
    pub(crate) register: Vec<Reg>,
    pub(crate) topic: Option<Value>,
    /// Where the topic's value was read (per field for a record).
    pub(crate) prov: Prov,
    /// A group's member threads (empty everywhere else). Carried by
    /// `@| group` alongside the list topic so keyed aggregates on
    /// `|` can work per group and `@| ungroup` can flatten back.
    pub(crate) members: Vec<Thread<X>>,
    /// The groups of the last successful `=~` match in a filter
    /// command (`$1` … `$9`); empty until one matches.
    pub(crate) captures: Vec<String>,
    /// The named groups of that match (`%+`), as `(name, text)`.
    pub(crate) named: Vec<(String, String)>,
    /// The layer above's state for this thread.
    pub(crate) ext: X,
}

impl<X: Attachment> Thread<X> {
    /// A thread carrying nothing but its attachment and a topic.
    pub(crate) fn bare(ext: X, topic: Option<Value>) -> Thread<X> {
        Thread {
            register: Vec::new(),
            topic,
            prov: Prov::default(),
            members: Vec::new(),
            captures: Vec::new(),
            named: Vec::new(),
            ext,
        }
    }
}
