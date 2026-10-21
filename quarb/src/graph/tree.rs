//! `@| tree` (spec: The tree): the context drawn as Unix `tree`
//! draws a directory. Every context node is a starting point whose
//! subtree prints in full, solid box lines in document order; the
//! mount root is the top line always, and a starting point below it
//! hangs by a dotted stub labelled with the skipped path. A drawn
//! node's outgoing crosslinks to context nodes are listed under its
//! line, one hop deep; a node is drawn once, where first met. The
//! operands pick a depth, the traits, the property names, the
//! properties as a record, and column expressions. One text value.
use crate::graph::adapter::AstAdapter;
use crate::graph::identity::name_path;
use crate::graph::memo::links_of;
use crate::syntax::ast::{Arg, FnCall, Operand};
use crate::{NodeId, Value};
use std::collections::HashSet;

/// What a node's line carries after its label, in the operands'
/// order.
pub(crate) enum Part {
    /// `traits`: the node's traits, `<file; md>`.
    Traits,
    /// `props`: the node's property names, `::name ::size`.
    Props,
    /// `%`: the node's properties as a record.
    Record,
    /// Any other expression, evaluated at the node.
    Column(Operand),
}

/// The operands of a `tree` call.
pub(crate) struct TreeSpec {
    /// Levels drawn below each starting point; None draws all.
    pub(crate) depth: Option<usize>,
    pub(crate) parts: Vec<Part>,
}

impl TreeSpec {
    /// The call's operands, in any order (the parser admitted them).
    pub(crate) fn from_call(call: &FnCall) -> TreeSpec {
        let mut spec = TreeSpec {
            depth: None,
            parts: Vec::new(),
        };
        for arg in &call.args {
            match arg {
                Arg::Lit(Value::Int(n)) => spec.depth = Some((*n).max(0) as usize),
                Arg::Lit(Value::Str(s)) if s == "traits" => spec.parts.push(Part::Traits),
                Arg::Lit(Value::Str(s)) if s == "props" => spec.parts.push(Part::Props),
                Arg::Lit(Value::Str(s)) if s == "%" => spec.parts.push(Part::Record),
                Arg::Expr(e) => spec.parts.push(Part::Column(e.clone())),
                _ => {}
            }
        }
        spec
    }
}

/// The label of a symlink-style crosslink: the one outgoing link an
/// adapter gives a node that stands for another (the filesystem
/// adapter's symlink), drawn inline on the node's line as Unix
/// `tree` draws `CLAUDE.md -> SYNOPSIS/CLAUDE.md`.
const INLINE_LINK: &str = "target";

struct Drawer<'a, A: AstAdapter> {
    adapter: &'a A,
    spec: &'a TreeSpec,
    column: &'a dyn Fn(NodeId, &Operand) -> Value,
    /// The starting points.
    context: HashSet<NodeId>,
    /// Drawn already: a node is drawn once, where first met.
    drawn: HashSet<NodeId>,
    out: Vec<String>,
}

/// Draw the tree of `starts` (the context, in its order).
pub(crate) fn render(
    adapter: &impl AstAdapter,
    starts: &[NodeId],
    spec: &TreeSpec,
    column: &dyn Fn(NodeId, &Operand) -> Value,
) -> String {
    let mut d = Drawer {
        adapter,
        spec,
        column,
        context: starts.iter().copied().collect(),
        drawn: HashSet::new(),
        out: Vec::new(),
    };
    let root = adapter.root();
    // Several mounts under an unnamed, sourceless root: a forest,
    // one top line per mount (each mount root carries its source).
    let tops: Vec<NodeId> = {
        let children = adapter.children(root);
        let forest = adapter.name(root).is_none()
            && adapter.provenance(root).source.is_none()
            && !children.is_empty()
            && children
                .iter()
                .all(|c| adapter.provenance(*c).source.is_some());
        if forest { children } else { vec![root] }
    };
    // A starting point under no top at all (a non-tree node brought
    // in by `*//` or an arrow) hangs from the last top, without a
    // stub.
    let mut placed: HashSet<NodeId> = HashSet::new();
    for (i, top) in tops.iter().enumerate() {
        let last = i + 1 == tops.len();
        let under: Vec<NodeId> = starts
            .iter()
            .copied()
            .filter(|s| !placed.contains(s) && d.under(*s, *top))
            .collect();
        placed.extend(under.iter().copied());
        let strays: Vec<NodeId> = if last {
            starts
                .iter()
                .copied()
                .filter(|s| !placed.contains(s))
                .collect()
        } else {
            Vec::new()
        };
        d.draw_top(*top, &under, &strays);
    }
    d.out.join("\n")
}

impl<A: AstAdapter> Drawer<'_, A> {
    /// Whether `node` is `top` or one of its descendants.
    fn under(&self, node: NodeId, top: NodeId) -> bool {
        let mut cur = Some(node);
        while let Some(n) = cur {
            if n == top {
                return true;
            }
            cur = self.adapter.parent(n);
        }
        false
    }

    /// Whether a strict ancestor of `node` is a starting point: the
    /// node is then drawn inside that ancestor's subtree.
    fn covered(&self, node: NodeId) -> bool {
        let mut cur = self.adapter.parent(node);
        while let Some(n) = cur {
            if self.context.contains(&n) {
                return true;
            }
            cur = self.adapter.parent(n);
        }
        false
    }

    /// The names from `top`'s child down to `node`, `/`-joined: the
    /// path a stub stands for.
    fn skipped(&self, node: NodeId, top: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(node);
        while let Some(n) = cur
            && n != top
        {
            parts.push(self.label(n));
            cur = self.adapter.parent(n);
        }
        parts.reverse();
        parts.join("/")
    }

    /// A node's label: its name; a nameless node its locator
    /// segment (`[0]`).
    fn label(&self, node: NodeId) -> String {
        if let Some(name) = self.adapter.name(node) {
            return name;
        }
        match self.adapter.parent(node) {
            Some(p) => {
                let pos = self
                    .adapter
                    .children(p)
                    .iter()
                    .position(|c| *c == node)
                    .unwrap_or(0);
                format!("[{pos}]")
            }
            None => "/".to_string(),
        }
    }

    /// The top line: the mount root as its locator — `.` for a
    /// directory, a mount's name, a document's file name.
    fn top_label(&self, top: NodeId) -> String {
        if self.adapter.traits(top).iter().any(|t| t == "dir") {
            return ".".to_string();
        }
        if let Some(name) = self.adapter.name(top) {
            return name;
        }
        match self.adapter.provenance(top).source {
            Some(src) => std::path::Path::new(&src)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or(src),
            None => "/".to_string(),
        }
    }

    fn draw_top(&mut self, top: NodeId, starts: &[NodeId], strays: &[NodeId]) {
        let line = format!("{}{}", self.top_label(top), self.suffix(top));
        self.out.push(line);
        self.drawn.insert(top);
        // The items under the top line, in the context's order: a
        // starting point that is the top itself draws the top's
        // subtree in place; one directly below hangs without a
        // stub; one deeper hangs by the stub of its skipped path,
        // shared with its siblings. A starting point inside another
        // starting point's subtree is drawn there.
        enum Item {
            Whole,
            Direct(NodeId),
            Stub(String, Vec<NodeId>),
            Stray(NodeId),
        }
        let mut items: Vec<Item> = Vec::new();
        for &s in starts {
            if s == top {
                items.push(Item::Whole);
            } else if self.covered(s) {
                continue;
            } else if self.adapter.parent(s) == Some(top) {
                items.push(Item::Direct(s));
            } else {
                let parent = self.adapter.parent(s).expect("under the top");
                let path = self.skipped(parent, top);
                match items
                    .iter_mut()
                    .find(|i| matches!(i, Item::Stub(p, _) if *p == path))
                {
                    Some(Item::Stub(_, members)) => members.push(s),
                    _ => items.push(Item::Stub(path, vec![s])),
                }
            }
        }
        items.extend(strays.iter().map(|s| Item::Stray(*s)));
        if items.iter().any(|i| matches!(i, Item::Whole)) {
            // the whole subtree, every other item inside it
            self.links_and_children(top, "", self.spec.depth);
            return;
        }
        let n = items.len();
        for (i, item) in items.into_iter().enumerate() {
            let last = i + 1 == n;
            match item {
                Item::Whole => unreachable!(),
                Item::Direct(s) | Item::Stray(s) => self.draw_node(s, "", last, self.spec.depth),
                Item::Stub(path, members) => {
                    let (tee, below) = if last {
                        ("└┄┄ ", "    ")
                    } else {
                        ("├┄┄ ", "│   ")
                    };
                    self.out.push(format!("{tee}{path}"));
                    let m = members.len();
                    for (j, s) in members.into_iter().enumerate() {
                        self.draw_node(s, below, j + 1 == m, self.spec.depth);
                    }
                }
            }
        }
    }

    /// One node's line under `prefix`, then its crosslinks and its
    /// children. `remaining` is the depth still allowed below it; a
    /// starting point resets it to the call's.
    fn draw_node(&mut self, node: NodeId, prefix: &str, last: bool, remaining: Option<usize>) {
        let (tee, below) = if last {
            ("└── ", "    ")
        } else {
            ("├── ", "│   ")
        };
        self.out.push(format!(
            "{prefix}{tee}{}{}",
            self.label(node),
            self.suffix(node)
        ));
        self.drawn.insert(node);
        let remaining = if self.context.contains(&node) {
            self.spec.depth
        } else {
            remaining
        };
        let child_prefix = format!("{prefix}{below}");
        self.links_and_children(node, &child_prefix, remaining);
    }

    /// The crosslink lines of a drawn node (hop one: the target in
    /// the context, drawn under the line when not drawn yet), then
    /// its children not drawn yet, in document order.
    fn links_and_children(&mut self, node: NodeId, prefix: &str, remaining: Option<usize>) {
        for (label, target) in self.listed_links(node) {
            if self.drawn.contains(&target) {
                self.out.push(format!(
                    "{prefix}->{label} {}",
                    name_path(self.adapter, target)
                ));
                continue;
            }
            self.out.push(format!(
                "{prefix}->{label} {}",
                name_path(self.adapter, target)
            ));
            self.drawn.insert(target);
            // hop two: the target's own crosslinks by path only, and
            // its children
            let deeper = format!("{prefix}    ");
            for (l2, t2) in self.listed_links(target) {
                let onward = if links_of(self.adapter, t2)
                    .iter()
                    .any(|(_, t)| self.context.contains(t))
                {
                    " -> ..."
                } else {
                    ""
                };
                self.out.push(format!(
                    "{deeper}->{l2} {}{onward}",
                    name_path(self.adapter, t2)
                ));
            }
            self.children(target, &deeper, self.spec.depth);
        }
        self.children(node, prefix, remaining);
    }

    fn children(&mut self, node: NodeId, prefix: &str, remaining: Option<usize>) {
        if remaining == Some(0) {
            return;
        }
        let kids: Vec<NodeId> = self
            .adapter
            .children(node)
            .into_iter()
            .filter(|c| !self.drawn.contains(c))
            .collect();
        let n = kids.len();
        for (i, kid) in kids.into_iter().enumerate() {
            self.draw_node(kid, prefix, i + 1 == n, remaining.map(|r| r - 1));
        }
    }

    /// The crosslinks listed under a node's line: the outgoing links
    /// whose target is a context node, the inline one excepted.
    fn listed_links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        if self.inline_link(node).is_some() {
            return Vec::new();
        }
        links_of(self.adapter, node)
            .iter()
            .filter(|(_, t)| self.context.contains(t))
            .cloned()
            .collect()
    }

    /// The one link drawn inline (`name -> path`): a node whose only
    /// outgoing link is its symlink target.
    fn inline_link(&self, node: NodeId) -> Option<NodeId> {
        match links_of(self.adapter, node).as_slice() {
            [(label, target)] if label == INLINE_LINK => Some(*target),
            _ => None,
        }
    }

    /// What follows a node's label: the inline link, then the parts
    /// in the operands' order.
    fn suffix(&self, node: NodeId) -> String {
        let mut s = String::new();
        if let Some(target) = self.inline_link(node) {
            let path = name_path(self.adapter, target);
            s.push_str(" -> ");
            s.push_str(path.strip_prefix('/').unwrap_or(&path));
        }
        for part in &self.spec.parts {
            match part {
                Part::Traits => {
                    let traits = self.adapter.traits(node);
                    if !traits.is_empty() {
                        s.push_str(" <");
                        s.push_str(&traits.join("; "));
                        s.push('>');
                    }
                }
                Part::Props => {
                    for name in self.adapter.property_names(node) {
                        s.push_str(" ::");
                        s.push_str(&name);
                    }
                }
                // the properties by name, as `props` lists them
                Part::Record => {
                    let fields: Vec<(String, Value)> = self
                        .adapter
                        .property_names(node)
                        .into_iter()
                        .map(|name| {
                            let v = self.adapter.property(node, &name).unwrap_or(Value::Null);
                            (name, v)
                        })
                        .collect();
                    s.push(' ');
                    s.push_str(&Value::Record(fields).display_form());
                }
                // a null column value prints nothing, not a blank
                Part::Column(op) => match (self.column)(node, op) {
                    Value::Null => {}
                    v => {
                        s.push_str("  ");
                        s.push_str(&v.display_form());
                    }
                },
            }
        }
        s
    }
}
