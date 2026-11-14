//! The thread tree (spec: The Pipeline, `@| threads`): the lineage a
//! run records when the query asks for it, and its drawing — every
//! step of the expanded query a level, the forks children, box-drawn
//! as `tree` draws, an aggregation a paragraph of its own.
//!
//! Lineage is opt-in: a query holding `threads` turns the run's
//! recording on, and every thread then carries a trail — a shared
//! linked list, one node per step, forks sharing their prefix — of
//! the steps it took with its cursor after each, the threads that
//! merged into it, and the aggregation it came out of (the member
//! trails kept for the paragraph above). The threads a step ended
//! keep their trails in the run's pruned list, so their last line
//! prints. A query without `threads` records nothing and pays nothing.

use crate::graph::adapter::AstAdapter;
use crate::graph::identity::name_path;
use crate::graph::navigate::GraphExt;
use crate::pipeline::with_run;
use crate::syntax::ast::visit::{self, Visitor};
use crate::syntax::ast::{Arg, Command, FnCall, Operand, Query};
use crate::thread::{Reg, Thread};
use crate::{NodeId, Value};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

type Link = Option<Arc<Trail>>;

thread_local! {
    /// The order trail nodes were made in, for the drawing's sibling
    /// order (a hop's landings as the hop made them).
    static SEQ: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// One event of a thread's lineage.
pub(crate) enum TrailEvent {
    /// A step the thread took — a hop (its landing, one event per
    /// landing thread) or a pipe command — and its cursor after it.
    Step {
        text: String,
        hop: bool,
        /// A hop's literal part — its leading elements up to the
        /// first wildcard (`/books` of `/books/*`): what `…` stands
        /// for on the landings' lines, with the position above.
        lead: String,
        node: Option<NodeId>,
        topic: Option<Value>,
        /// The trails of the alike threads that merged into this one
        /// at the step.
        merged: Vec<Link>,
    },
    /// An aggregation the thread came out of: the stage's text (`@|
    /// sum`, `@|{.g} count`), its ordinal among the query's
    /// commands, the answer's label (a fold's regulae; empty
    /// otherwise), its cursor, and the trails of the threads that
    /// fed the stage — the paragraph above.
    Agg {
        text: String,
        stage: u32,
        label: String,
        node: Option<NodeId>,
        topic: Option<Value>,
        members: Vec<Link>,
    },
}

/// A thread's lineage, newest event first; forks share their prefix.
pub(crate) struct Trail {
    pub(crate) event: TrailEvent,
    pub(crate) prev: Link,
    /// The node's rank in the run's making order.
    seq: u64,
}

fn link(event: TrailEvent, prev: Link) -> Link {
    let seq = SEQ.with(|s| {
        let n = s.get() + 1;
        s.set(n);
        n
    });
    Some(Arc::new(Trail { event, prev, seq }))
}

thread_local! {
    /// The trails of the threads a step ended, in the run.
    static PRUNED: RefCell<Vec<Link>> = const { RefCell::new(Vec::new()) };
}

/// Whether the run records lineage (the expanded query holds
/// `threads`).
pub(crate) fn recording() -> bool {
    with_run(|s| s.trail)
}

/// Start a run's lineage: on when the query asks for the tree.
pub(crate) fn begin(query: &Query) {
    let on = wants_threads(query);
    with_run(|s| s.trail = on);
    PRUNED.with(|p| p.borrow_mut().clear());
    SEQ.with(|s| s.set(0));
}

/// Whether a query asks for the thread tree anywhere.
pub(crate) fn wants_threads(query: &Query) -> bool {
    struct Wants(bool);
    impl Visitor for Wants {
        fn command(&mut self, c: &Command) {
            if let Command::Agg(call) = c
                && call.name == "threads"
            {
                self.0 = true;
            }
            visit::walk_command(self, c);
        }
    }
    let mut w = Wants(false);
    w.query(query);
    w.0
}

/// The trail after a step.
pub(crate) fn step(
    prev: &Link,
    text: &str,
    hop: bool,
    node: Option<NodeId>,
    topic: Option<&Value>,
) -> Link {
    if !recording() {
        return prev.clone();
    }
    link(
        TrailEvent::Step {
            text: text.trim().to_string(),
            hop,
            lead: String::new(),
            node,
            topic: topic.cloned(),
            merged: Vec::new(),
        },
        prev.clone(),
    )
}

/// Whether a path element's text is literal — one child step by a
/// plain name (`/books`, `/0`), no wildcard, glob, predicate or other
/// axis — and so part of what `…` stands for.
pub(crate) fn literal_elem(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('/') else {
        return false;
    };
    !rest.is_empty()
        && rest
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && rest != ".."
        && rest != "."
}

/// The trail after one more element of a hop within a branch: the
/// hop the parent's last event stands for, extended to this landing
/// — one level for `/books/*`, not one per element — its merges
/// carried; `first` marks the run's first element, which starts the
/// hop instead.
pub(crate) fn hop(parent: &Link, first: bool, text: &str, lead: &str, node: NodeId) -> Link {
    if !recording() {
        return parent.clone();
    }
    let (base, merged) = match parent.as_deref() {
        Some(Trail {
            event: TrailEvent::Step {
                hop: true, merged, ..
            },
            prev,
            ..
        }) if !first => (prev.clone(), merged.clone()),
        _ => (parent.clone(), Vec::new()),
    };
    link(
        TrailEvent::Step {
            text: text.trim().to_string(),
            hop: true,
            lead: lead.to_string(),
            node: Some(node),
            topic: None,
            merged,
        },
        base,
    )
}

/// A thread a hop exhausted (no landing): its trail kept for the
/// drawing, its last line the step before the hop — the hop's
/// elements walked so far (`first` false) are not a step of its own.
pub(crate) fn ended(trail: &Link, first: bool) {
    if !recording() {
        return;
    }
    let kept = match trail.as_deref() {
        Some(Trail {
            event: TrailEvent::Step { hop: true, .. },
            prev,
            ..
        }) if !first => prev.clone(),
        _ => trail.clone(),
    };
    if kept.is_none() {
        return;
    }
    PRUNED.with(|p| p.borrow_mut().push(kept));
}

/// An alike thread merged into `kept` at its last step: its trail
/// joins the step's merged list.
pub(crate) fn merge_into(kept: &mut Link, dropped: Link) {
    if !recording() {
        return;
    }
    let Some(head) = kept.take() else {
        return;
    };
    let rebuilt = match &head.event {
        TrailEvent::Step {
            text,
            hop,
            lead,
            node,
            topic,
            merged,
        } => {
            let mut merged = merged.clone();
            merged.push(dropped);
            Trail {
                event: TrailEvent::Step {
                    text: text.clone(),
                    hop: *hop,
                    lead: lead.clone(),
                    node: *node,
                    topic: topic.clone(),
                    merged,
                },
                prev: head.prev.clone(),
                seq: head.seq,
            }
        }
        TrailEvent::Agg { .. } => {
            *kept = Some(head);
            return;
        }
    };
    *kept = Some(Arc::new(rebuilt));
}

/// The step `command` just ran at the top level, from `inputs` to
/// `outputs`: every output takes the step on its trail (an
/// aggregation a fresh paragraph holding the inputs' trails), and
/// every input no output descends from is pruned, its trail kept.
pub(crate) fn record_step(
    command: &Command,
    inputs: &[GraphExt],
    outputs: &mut [Thread<GraphExt>],
) {
    if !recording() {
        return;
    }
    let full = crate::syntax::unparse::command(command);
    let stage = full.starts_with("@|");
    let text = full.strip_prefix("| ").unwrap_or(&full).to_string();
    if stage {
        let ordinal = with_run(|s| s.command);
        let members: Vec<Link> = inputs.iter().map(|e| e.trail.clone()).collect();
        let regs = match command {
            Command::Fold { regs, .. } => regs.as_slice(),
            _ => &[],
        };
        for t in outputs.iter_mut() {
            let label: Vec<String> = regs
                .iter()
                .filter_map(|r| {
                    let i = crate::pipeline::fold_index(&t.register, r)?;
                    let reg = &t.register[i];
                    let name = match &reg.name {
                        Some(n) => format!(".{n}"),
                        None => crate::syntax::unparse::fold_selector(std::slice::from_ref(r)),
                    };
                    Some(format!("{name} = {}", reg.value.to_quarb()))
                })
                .collect();
            t.ext.trail = link(
                TrailEvent::Agg {
                    text: text.clone(),
                    stage: ordinal,
                    label: label.join("; "),
                    node: (!t.ext.unplaced).then_some(t.ext.node),
                    topic: t.topic.clone(),
                    members: members.clone(),
                },
                None,
            );
        }
        return;
    }
    // a hop records its landings on the walk; every other command
    // records itself on each output
    if !matches!(command, Command::Nav(_)) {
        for t in outputs.iter_mut() {
            t.ext.trail = step(
                &t.ext.trail.clone(),
                &text,
                false,
                (!t.ext.unplaced).then_some(t.ext.node),
                t.topic.as_ref(),
            );
        }
    }
    // the inputs no output descends from were ended by the step
    let mut alive: HashSet<*const Trail> = HashSet::new();
    let heads: HashSet<*const Trail> = inputs
        .iter()
        .filter_map(|e| e.trail.as_ref().map(Arc::as_ptr))
        .collect();
    for t in outputs.iter() {
        let mut cur = t.ext.trail.clone();
        while let Some(n) = cur {
            let p = Arc::as_ptr(&n);
            if heads.contains(&p) {
                alive.insert(p);
                break;
            }
            if matches!(n.event, TrailEvent::Agg { .. }) {
                break;
            }
            cur = n.prev.clone();
        }
    }
    PRUNED.with(|p| {
        let mut pruned = p.borrow_mut();
        for e in inputs {
            if let Some(t) = &e.trail
                && !alive.contains(&Arc::as_ptr(t))
            {
                pruned.push(Some(t.clone()));
            }
        }
    });
}

/// The operands of a `threads` call.
pub(crate) struct ThreadsSpec {
    /// Steps drawn within a paragraph; None draws all.
    pub(crate) depth: Option<usize>,
    /// Every live leaf's capsa after its cursor, the regulae in
    /// filing order.
    pub(crate) capsa: bool,
}

impl ThreadsSpec {
    pub(crate) fn from_call(call: &FnCall) -> ThreadsSpec {
        let mut spec = ThreadsSpec {
            depth: None,
            capsa: false,
        };
        for arg in &call.args {
            match arg {
                Arg::Opt {
                    name,
                    value: Some(Operand::Lit(Value::Int(n))),
                } if name == "--depth" || name == "-d" => spec.depth = Some((*n).max(0) as usize),
                Arg::Opt { name, .. } if name == "--capsa" || name == "-c" => spec.capsa = true,
                _ => {}
            }
        }
        spec
    }
}

/// The lineage trie of one paragraph: the trail node each trie node
/// stands for (None at the paragraph's root), its children in
/// first-appearance order, and the trails that end at it.
struct Node {
    trail: Link,
    children: Vec<usize>,
    ends: Vec<usize>,
}

struct Trie {
    nodes: Vec<Node>,
}

impl Trie {
    /// The trie of `trails`, each read back to (not including) the
    /// paragraph's first event — an aggregation's answer event
    /// counts, a hop into it does not.
    fn build(trails: &[Link]) -> Trie {
        let mut nodes = vec![Node {
            trail: None,
            children: Vec::new(),
            ends: Vec::new(),
        }];
        let mut index: HashMap<*const Trail, usize> = HashMap::new();
        for (i, trail) in trails.iter().enumerate() {
            let mut chain = Vec::new();
            let mut cur = trail.clone();
            while let Some(t) = cur {
                let is_agg = matches!(t.event, TrailEvent::Agg { .. });
                cur = t.prev.clone();
                chain.push(t);
                if is_agg {
                    break;
                }
            }
            chain.reverse();
            let mut at = 0;
            for t in chain {
                let key = Arc::as_ptr(&t);
                at = match index.get(&key) {
                    Some(id) => *id,
                    None => {
                        let id = nodes.len();
                        nodes.push(Node {
                            trail: Some(t),
                            children: Vec::new(),
                            ends: Vec::new(),
                        });
                        nodes[at].children.push(id);
                        index.insert(key, id);
                        id
                    }
                };
            }
            nodes[at].ends.push(i);
        }
        // siblings in the order the run made them
        for id in 0..nodes.len() {
            let mut kids = std::mem::take(&mut nodes[id].children);
            kids.sort_by_key(|&k| nodes[k].trail.as_ref().map_or(0, |t| t.seq));
            nodes[id].children = kids;
        }
        Trie { nodes }
    }

    fn event(&self, id: usize) -> Option<&TrailEvent> {
        self.nodes[id].trail.as_ref().map(|t| &t.event)
    }
}

/// A trail gathered for the drawing: where it came from decides its
/// last line's tail.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// A thread of the context: its capsa prints under `--capsa`.
    Live(usize),
    /// A thread a step ended, or one an aggregation consumed.
    Ended,
    /// A thread that merged into another at its last step.
    Merged,
}

/// The stage a trail belongs to: 0 before any aggregation, else the
/// ordinal of the aggregation its chain starts at.
fn stage_of(trail: &Link) -> u32 {
    let mut cur = trail.clone();
    while let Some(t) = cur {
        if let TrailEvent::Agg { stage, .. } = &t.event {
            return *stage;
        }
        cur = t.prev.clone();
    }
    0
}

struct Drawer<'a, A: AstAdapter> {
    adapter: &'a A,
    scalar: &'a dyn Fn(NodeId) -> Value,
    spec: &'a ThreadsSpec,
    trails: &'a [(Link, Kind)],
    registers: &'a [&'a [Reg]],
    out: Vec<String>,
}

enum Item {
    /// Landings of one hop under one step line.
    Hop(String, Vec<usize>),
    Node(usize),
}

impl<A: AstAdapter> Drawer<'_, A> {
    /// The cursor after a step, `(position; topic)`: the position
    /// relative to the line above's (ruling 2026-11-04) — `…` when
    /// unchanged; on a hop's landing `…` is the position above joined
    /// with the hop's literal part and the line prints what the
    /// wildcards matched (`(…/1; ∅)` for `/members/*`, `(…/1/members/1;
    /// ∅)` for `/teams/*/members/*`); the whole name path otherwise (a
    /// parent or ancestor hop, a mark jump, the first line of a
    /// paragraph); `∅` for no position.
    fn cursor(
        &self,
        node: Option<NodeId>,
        topic: Option<&Value>,
        above: Option<NodeId>,
        lead: &str,
    ) -> String {
        let pos = match node {
            None => "∅".to_string(),
            Some(n) => {
                let full = name_path(self.adapter, n);
                match above {
                    Some(a) if a == n => "…".to_string(),
                    Some(a) => {
                        let parent = name_path(self.adapter, a);
                        let base = parent.trim_end_matches('/');
                        match full.strip_prefix(base) {
                            Some(tail) if tail.starts_with('/') => {
                                if tail == lead {
                                    "…".to_string()
                                } else if !lead.is_empty()
                                    && let Some(matched) = tail.strip_prefix(lead)
                                    && matched.starts_with('/')
                                {
                                    format!("…{matched}")
                                } else {
                                    format!("…{tail}")
                                }
                            }
                            _ => full,
                        }
                    }
                    None => full,
                }
            }
        };
        let top = match topic {
            Some(v) => v.display_form(),
            None => "∅".to_string(),
        };
        format!("({pos}; {top})")
    }

    /// What follows a line's cursor for the trails ending at `id`:
    /// the merge tail, the capsa.
    fn tail(&self, trie: &Trie, id: usize) -> String {
        let mut s = String::new();
        if let Some(TrailEvent::Step { merged, .. }) = trie.event(id)
            && !merged.is_empty()
        {
            s.push_str(&format!("  ({} merged)", merged.len() + 1));
        }
        for &i in &trie.nodes[id].ends {
            match self.trails[i].1 {
                Kind::Merged => s.push_str("  ⤴ merged"),
                Kind::Live(k) if self.spec.capsa => {
                    let regs: Vec<String> = self.registers[k]
                        .iter()
                        .map(|r| {
                            let name = match &r.name {
                                Some(n) => format!(".{n}"),
                                None => ".".to_string(),
                            };
                            format!("{name} = {}", r.value.to_quarb())
                        })
                        .collect();
                    s.push_str("  [");
                    s.push_str(&regs.join("; "));
                    s.push(']');
                }
                _ => {}
            }
        }
        s
    }

    fn items(&self, trie: &Trie, children: &[usize]) -> Vec<Item> {
        let mut items: Vec<Item> = Vec::new();
        for &c in children {
            match trie.event(c) {
                Some(TrailEvent::Step {
                    text, hop: true, ..
                }) => match items.last_mut() {
                    Some(Item::Hop(t, ids)) if t == text => ids.push(c),
                    _ => items.push(Item::Hop(text.clone(), vec![c])),
                },
                _ => items.push(Item::Node(c)),
            }
        }
        items
    }

    fn tees(last: bool) -> (&'static str, &'static str) {
        if last {
            ("└─ ", "   ")
        } else {
            ("├─ ", "│  ")
        }
    }

    /// One trail node's line (a landing's cursor, a command with its
    /// cursor, an answer with its label) and what hangs under it.
    /// One line and what hangs under it; `above` is the position of
    /// the line above (None at a paragraph's first line).
    fn draw_node(
        &mut self,
        trie: &Trie,
        id: usize,
        prefix: &str,
        last: bool,
        level: usize,
        above: Option<NodeId>,
    ) {
        let (tee, below) = Self::tees(last);
        let (line, here) = match trie.event(id) {
            Some(TrailEvent::Step {
                text,
                hop,
                lead,
                node,
                topic,
                ..
            }) => {
                let cursor = self.cursor(*node, topic.as_ref(), above, lead);
                (
                    if *hop {
                        cursor
                    } else {
                        format!("{text}  {cursor}")
                    },
                    *node,
                )
            }
            Some(TrailEvent::Agg {
                label, node, topic, ..
            }) => {
                let cursor = self.cursor(*node, topic.as_ref(), above, "");
                (
                    if label.is_empty() {
                        cursor
                    } else {
                        format!("{label}  {cursor}")
                    },
                    *node,
                )
            }
            None => (String::new(), above),
        };
        self.out
            .push(format!("{prefix}{tee}{line}{}", self.tail(trie, id)));
        self.draw_children(trie, id, &format!("{prefix}{below}"), level + 1, here);
    }

    fn draw_children(
        &mut self,
        trie: &Trie,
        id: usize,
        prefix: &str,
        level: usize,
        above: Option<NodeId>,
    ) {
        if self.spec.depth.is_some_and(|d| level > d) {
            return;
        }
        let items = self.items(trie, &trie.nodes[id].children);
        let n = items.len();
        for (i, item) in items.into_iter().enumerate() {
            let last = i + 1 == n;
            match item {
                Item::Node(c) => self.draw_node(trie, c, prefix, last, level, above),
                Item::Hop(text, landings) => {
                    let (tee, below) = Self::tees(last);
                    self.out.push(format!("{prefix}{tee}{text}"));
                    let under = format!("{prefix}{below}");
                    let m = landings.len();
                    for (j, c) in landings.into_iter().enumerate() {
                        self.draw_node(trie, c, &under, j + 1 == m, level, above);
                    }
                }
            }
        }
    }
}

/// Every trail the drawing shows, with its kind: the context's
/// threads, the threads steps ended, the threads aggregations
/// consumed, and the threads that merged into others.
fn gather(live: &[Link]) -> Vec<(Link, Kind)> {
    let mut seen: HashSet<*const Trail> = HashSet::new();
    let mut out: Vec<(Link, Kind)> = Vec::new();
    let mut queue: std::collections::VecDeque<(Link, Kind)> = std::collections::VecDeque::new();
    for (i, t) in live.iter().enumerate() {
        queue.push_back((t.clone(), Kind::Live(i)));
    }
    PRUNED.with(|p| {
        for t in p.borrow().iter() {
            queue.push_back((t.clone(), Kind::Ended));
        }
    });
    while let Some((trail, kind)) = queue.pop_front() {
        if let Some(t) = &trail {
            if !seen.insert(Arc::as_ptr(t)) {
                continue;
            }
            let mut cur = Some(t.clone());
            while let Some(n) = cur {
                match &n.event {
                    TrailEvent::Step { merged, .. } => {
                        for m in merged {
                            queue.push_back((m.clone(), Kind::Merged));
                        }
                    }
                    TrailEvent::Agg { members, .. } => {
                        for m in members {
                            queue.push_back((m.clone(), Kind::Ended));
                        }
                        break;
                    }
                }
                cur = n.prev.clone();
            }
        }
        out.push((trail, kind));
    }
    out.sort_by_key(|(_, k)| match k {
        Kind::Live(i) => *i,
        _ => usize::MAX,
    });
    out
}

/// The thread tree of `threads` as one text: the paragraphs in order,
/// `root` with its cursor on top of the first, each aggregation's
/// text on top of its own.
pub(crate) fn render<A: AstAdapter>(
    adapter: &A,
    threads: &[Thread<GraphExt>],
    spec: &ThreadsSpec,
    scalar: &dyn Fn(NodeId) -> Value,
) -> String {
    let live: Vec<Link> = threads.iter().map(|t| t.ext.trail.clone()).collect();
    let registers: Vec<&[Reg]> = threads.iter().map(|t| t.register.as_slice()).collect();
    let all = gather(&live);
    // the paragraphs: stage 0, then each aggregation by its ordinal
    let mut stages: Vec<u32> = all.iter().map(|(t, _)| stage_of(t)).collect();
    stages.sort_unstable();
    stages.dedup();
    if stages.first() != Some(&0) {
        stages.insert(0, 0);
    }
    let mut drawer = Drawer {
        adapter,
        scalar,
        spec,
        trails: &all,
        registers: &registers,
        out: Vec::new(),
    };
    let _ = drawer.scalar;
    for stage in stages {
        let members: Vec<usize> = all
            .iter()
            .enumerate()
            .filter(|(_, (t, _))| stage_of(t) == stage)
            .map(|(i, _)| i)
            .collect();
        let trails: Vec<Link> = members.iter().map(|&i| all[i].0.clone()).collect();
        // the trie's `ends` index its own input; map back to `all`
        let trie = Trie::build(&trails);
        let remap = Trie {
            nodes: trie
                .nodes
                .into_iter()
                .map(|n| Node {
                    trail: n.trail,
                    children: n.children,
                    ends: n.ends.into_iter().map(|e| members[e]).collect(),
                })
                .collect(),
        };
        let mut above = None;
        if stage == 0 {
            let root = adapter.root();
            let cursor = drawer.cursor(Some(root), None, None, "");
            drawer
                .out
                .push(format!("root  {cursor}{}", drawer.tail(&remap, 0)));
            above = Some(root);
        } else {
            let text = remap
                .nodes
                .iter()
                .find_map(|n| match n.trail.as_ref().map(|t| &t.event) {
                    Some(TrailEvent::Agg { text, .. }) => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            drawer.out.push(text);
        }
        drawer.draw_children(&remap, 0, "", 1, above);
    }
    drawer.out.join("\n")
}
