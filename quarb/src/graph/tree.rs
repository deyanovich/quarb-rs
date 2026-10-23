//! `@| tree` (spec: The tree): the context drawn as Unix `tree`
//! draws a directory. Every context node is a starting point whose
//! subtree prints in full, solid box lines in document order; the
//! mount root is the top line always, and a starting point below it
//! hangs by a dotted stub labelled with the skipped path. A drawn
//! node's outgoing crosslinks to context nodes are listed under its
//! line, one hop deep; a node is drawn once, where first met. The
//! operands pick a depth, the kind, the traits, the property names,
//! the properties as a record, and column expressions. One text
//! value.
use crate::graph::adapter::AstAdapter;
use crate::graph::identity::name_path;
use crate::graph::memo::links_of;
use crate::syntax::ast::{Arg, FnCall, Operand};
use crate::{NodeId, Value};
use std::collections::HashSet;

/// What a node's line carries after its label, in the operands'
/// order.
pub(crate) enum Part {
    /// `--kind`: the node's kind, its first trait, `<file>`.
    Kind,
    /// `--traits`: the node's traits, `<file; md>`.
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
    /// `--sample=n`: the first n children of every node that has
    /// more, the rest one line "… and N more".
    pub(crate) sample: Option<usize>,
}

impl TreeSpec {
    /// The call's operands, in any order (the parser admitted them).
    pub(crate) fn from_call(call: &FnCall) -> TreeSpec {
        let mut spec = TreeSpec {
            depth: None,
            parts: Vec::new(),
            sample: None,
        };
        for arg in &call.args {
            match arg {
                Arg::Lit(Value::Int(n)) => spec.depth = Some((*n).max(0) as usize),
                // the display switches (draft 164)
                Arg::Opt { name, .. } if name == "--kind" || name == "-k" => {
                    spec.parts.push(Part::Kind)
                }
                Arg::Opt { name, .. } if name == "--traits" || name == "-t" => {
                    spec.parts.push(Part::Traits)
                }
                Arg::Opt { name, .. } if name == "--props" || name == "-p" => {
                    spec.parts.push(Part::Props)
                }
                Arg::Opt { name, .. } if name == "--record" || name == "-r" => {
                    spec.parts.push(Part::Record)
                }
                Arg::Opt { name, value: Some(Operand::Lit(Value::Int(n))) }
                    if name == "--sample" || name == "-s" =>
                {
                    spec.sample = Some((*n).max(0) as usize)
                }
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
    /// The lines as structure, for the HTML form: the depth (the
    /// top at 0), what the line is, its text without the box
    /// characters.
    events: Vec<(usize, Line, String)>,
}

/// What a drawn line is, for the HTML form.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Line {
    /// The top line, or a node's line.
    Node,
    /// A dotted stub: the skipped path.
    Stub,
    /// A crosslink listed under a node.
    Link,
}

/// The depth of a line from the prefix before its tee: four cells
/// per level below the top.
fn depth_of(prefix: &str) -> usize {
    prefix.chars().count() / 4 + 1
}

/// The HTML form of the drawing (spec: The tree): every node with
/// children a `details` element the browser folds and unfolds, its
/// line the `summary`; a leaf a plain line; a stub a dotted line;
/// a crosslink a line of its own. Dressed by `.quarb-tree` in the
/// one stylesheet.
fn html_of(events: &[(usize, Line, String)]) -> String {
    use crate::value::table::escape;
    // the controls: fold and unfold every node at once
    let mut out = String::from(
        "<div class=\"quarb-tree\"><div class=\"controls\">\
         <button type=\"button\" onclick=\"for (const d of this.closest('.quarb-tree').querySelectorAll('details')) d.open = true\">expand all</button>\
         <button type=\"button\" onclick=\"for (const d of this.closest('.quarb-tree').querySelectorAll('details')) d.open = false\">collapse all</button>\
         </div>",
    );
    // open containers, by depth
    let mut open: Vec<usize> = Vec::new();
    for (i, (depth, line, text)) in events.iter().enumerate() {
        while open.last().is_some_and(|d| *d >= *depth) {
            open.pop();
            out.push_str("</details>");
        }
        let has_children = events
            .get(i + 1)
            .is_some_and(|(d, _, _)| *d > *depth);
        let class = match line {
            Line::Node => "node",
            Line::Stub => "stub",
            Line::Link => "link",
        };
        if has_children && *line != Line::Link {
            out.push_str(&format!(
                "<details open class=\"{class}\"><summary>{}</summary>",
                escape(text)
            ));
            open.push(*depth);
        } else {
            out.push_str(&format!("<div class=\"{class}\">{}</div>", escape(text)));
        }
    }
    while open.pop().is_some() {
        out.push_str("</details>");
    }
    out.push_str("</div>");
    out
}

/// The ancestors between a top and its starting points, as a trie:
/// a node, and what hangs under it in the context's order.
struct Trie {
    node: NodeId,
    items: Vec<Item>,
}

enum Item {
    /// A starting point, its subtree drawn in place.
    Start(NodeId),
    /// An ancestor of starting points further down.
    Below(Trie),
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
        events: Vec::new(),
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
    crate::pipeline::set_drawing_html(html_of(&d.events));
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
        self.events.push((0, Line::Node, line.clone()));
        self.out.push(line);
        self.drawn.insert(top);
        // The items under the top line, in the context's order: a
        // starting point that is the top itself draws the top's
        // subtree in place; one directly below hangs without a
        // stub; one deeper hangs by the stub of its skipped path,
        // shared with its siblings. Where the skipped paths diverge,
        // their shared prefix is drawn once, as solid nodes, and
        // the stubs hang from the divergence. A starting point
        // inside another starting point's subtree is drawn there.
        if starts.contains(&top) {
            // the whole subtree, every other item inside it
            self.links_and_children(top, "", self.spec.depth);
            return;
        }
        let mut trie = Trie {
            node: top,
            items: Vec::new(),
        };
        for &s in starts {
            if self.covered(s) {
                continue;
            }
            // the ancestors below the top, top-down
            let mut chain = Vec::new();
            let mut cur = self.adapter.parent(s);
            while let Some(n) = cur
                && n != top
            {
                chain.push(n);
                cur = self.adapter.parent(n);
            }
            chain.reverse();
            let mut at = &mut trie;
            for n in chain {
                let pos = match at
                    .items
                    .iter()
                    .position(|i| matches!(i, Item::Below(t) if t.node == n))
                {
                    Some(pos) => pos,
                    None => {
                        at.items.push(Item::Below(Trie {
                            node: n,
                            items: Vec::new(),
                        }));
                        at.items.len() - 1
                    }
                };
                at = match &mut at.items[pos] {
                    Item::Below(t) => t,
                    Item::Start(_) => unreachable!(),
                };
            }
            at.items.push(Item::Start(s));
        }
        trie.items.extend(strays.iter().map(|s| Item::Start(*s)));
        self.draw_items(trie.items, "");
    }

    /// The items under a drawn line: a starting point as its own
    /// subtree, a run of ancestors that nothing else hangs from as
    /// one dotted stub, a shared ancestor as a solid node drawn
    /// once with its own items below it.
    fn draw_items(&mut self, items: Vec<Item>, prefix: &str) {
        // consecutive starting points are siblings in the drawing:
        // they sample and collapse as children do
        let mut grouped: Vec<Result<Vec<NodeId>, Trie>> = Vec::new();
        for item in items {
            match item {
                Item::Start(s) => match grouped.last_mut() {
                    Some(Ok(run)) => run.push(s),
                    _ => grouped.push(Ok(vec![s])),
                },
                Item::Below(t) => grouped.push(Err(t)),
            }
        }
        let n = grouped.len();
        for (i, item) in grouped.into_iter().enumerate() {
            let last = i + 1 == n;
            let (tee, below) = if last {
                ("└── ", "    ")
            } else {
                ("├── ", "│   ")
            };
            match item {
                Ok(starts) => self.draw_siblings(starts, prefix, self.spec.depth, last),
                Err(t) => {
                    // the run of single-child ancestors, down to the
                    // node where the paths diverge or the starting
                    // points hang
                    let mut run = vec![t.node];
                    let mut at = t;
                    while let [Item::Below(_)] = at.items.as_slice() {
                        let Some(Item::Below(next)) = at.items.pop() else {
                            unreachable!()
                        };
                        run.push(next.node);
                        at = next;
                    }
                    let shared = at.items.iter().any(|i| matches!(i, Item::Below(_)));
                    if !shared {
                        // every item a starting point: one stub
                        let path: Vec<String> = run.iter().map(|n| self.label(*n)).collect();
                        let tee = if last { "└┄┄ " } else { "├┄┄ " };
                        self.events.push((depth_of(prefix), Line::Stub, path.join("/")));
                        self.out.push(format!("{prefix}{tee}{}", path.join("/")));
                        let under = format!("{prefix}{below}");
                        self.draw_items(at.items, &under);
                        continue;
                    }
                    // the paths diverge below the run: solid nodes,
                    // each the only item under the one above
                    let mut p = prefix.to_string();
                    let mut tee = tee;
                    let mut below = below;
                    for (j, node) in run.iter().enumerate() {
                        let text = format!("{}{}", self.label(*node), self.suffix(*node));
                        self.events.push((depth_of(&p), Line::Node, text));
                        self.out.push(format!("{p}{tee}{}{}", self.label(*node), self.suffix(*node)));
                        self.drawn.insert(*node);
                        p.push_str(below);
                        if j == 0 {
                            tee = "└── ";
                            below = "    ";
                        }
                    }
                    self.draw_items(at.items, &p);
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
        let text = format!("{}{}", self.label(node), self.suffix(node));
        self.events.push((depth_of(prefix), Line::Node, text));
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
            let text = format!("->{label} {}", name_path(self.adapter, target));
            self.events.push((depth_of(prefix) , Line::Link, text.clone()));
            if self.drawn.contains(&target) {
                self.out.push(format!("{prefix}{text}"));
                continue;
            }
            self.out.push(format!("{prefix}{text}"));
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
                let text = format!("->{l2} {}{onward}", name_path(self.adapter, t2));
                self.events.push((depth_of(&deeper), Line::Link, text.clone()));
                self.out.push(format!("{deeper}{text}"));
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
        self.draw_siblings(kids, prefix, remaining.map(|r| r - 1), true);
    }

    /// Siblings drawn in order (spec: The tree): under `--sample=n`
    /// the first n and one line "… and N more"; a run of same-named
    /// leaves collapses into one line with its count (`row ×891`);
    /// the count and the more lines stand outside the depth.
    fn draw_siblings(
        &mut self,
        kids: Vec<NodeId>,
        prefix: &str,
        remaining: Option<usize>,
        last_group: bool,
    ) {
        let (shown, more) = match self.spec.sample {
            Some(n) if kids.len() > n => (kids[..n].to_vec(), kids.len() - n),
            _ => (kids, 0),
        };
        // the items: a node, or a run of same-named leaves
        enum Sib {
            One(NodeId),
            Run(String, Vec<NodeId>),
        }
        let mut items: Vec<Sib> = Vec::new();
        for kid in shown {
            let leaf = self.adapter.children(kid).is_empty() && self.listed_links(kid).is_empty();
            if leaf {
                // a run collapses when the lines would read alike:
                // the name and what follows it
                let name = format!("{}{}", self.label(kid), self.suffix(kid));
                if let Some(Sib::Run(last, run)) = items.last_mut()
                    && *last == name
                {
                    run.push(kid);
                    continue;
                }
                items.push(Sib::Run(name, vec![kid]));
                continue;
            }
            items.push(Sib::One(kid));
        }
        let total = items.len() + usize::from(more > 0);
        for (i, item) in items.into_iter().enumerate() {
            let last = i + 1 == total && last_group;
            match item {
                Sib::One(kid) => self.draw_node(kid, prefix, last, remaining),
                // a lone leaf is a node, drawn with its suffix
                Sib::Run(_, run) if run.len() == 1 => {
                    self.draw_node(run[0], prefix, last, remaining)
                }
                Sib::Run(name, run) => {
                    self.drawn.extend(run.iter().copied());
                    self.push_line(prefix, last, format!("{name} ×{}", run.len()));
                }
            }
        }
        if more > 0 {
            self.push_line(prefix, last_group, format!("… and {more} more"));
        }
    }

    /// A line that is no node: a collapsed run or the more line.
    fn push_line(&mut self, prefix: &str, last: bool, text: String) {
        let tee = if last { "└── " } else { "├── " };
        self.events.push((depth_of(prefix), Line::Node, text.clone()));
        self.out.push(format!("{prefix}{tee}{text}"));
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
                Part::Kind => {
                    if let Some(kind) = self.adapter.traits(node).into_iter().next() {
                        s.push_str(" <");
                        s.push_str(&kind);
                        s.push('>');
                    }
                }
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
                // the node's scalar properties (the leaves directly
                // under it), never a child node as a field, none
                // when there are none, six then an ellipsis
                Part::Record => {
                    // a property that is a child node with children
                    // of its own (an object, an array) is no field
                    // here; a leaf child is the scalar it holds
                    let kids: Vec<NodeId> = self.adapter.children(node);
                    let inner: Vec<String> = kids
                        .iter()
                        .filter(|c| !self.adapter.children(**c).is_empty())
                        .filter_map(|c| self.adapter.name(*c))
                        .collect();
                    let leaves: Vec<String> = kids.iter().filter_map(|c| self.adapter.name(*c)).collect();
                    let fields: Vec<(String, Value)> = self
                        .adapter
                        .property_names(node)
                        .into_iter()
                        .filter(|name| !inner.contains(name))
                        .filter_map(|name| {
                            let v = self.adapter.property(node, &name).unwrap_or(Value::Null);
                            // a child whose value is null is a member,
                            // not a datum
                            let datum = !matches!(v, Value::List(_) | Value::Record(_) | Value::Table(_) | Value::Node(_))
                                && !(matches!(v, Value::Null) && leaves.contains(&name));
                            datum.then_some((name, v))
                        })
                        .collect();
                    if fields.is_empty() {
                        continue;
                    }
                    let more = fields.len() > 6;
                    let shown: Vec<(String, Value)> = fields.into_iter().take(6).collect();
                    let mut text = Value::Record(shown.into()).display_form();
                    if more {
                        text.pop();
                        text.push_str("; …)");
                    }
                    s.push(' ');
                    s.push_str(&text);
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
