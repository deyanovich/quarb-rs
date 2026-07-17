//! Reflection: the query arbor.
//!
//! A Quarb query is itself a tree — an arbor — so a parsed query can
//! be exposed through the same [`AstAdapter`] surface every data
//! substrate implements, and Quarb can query Quarb:
//!
//! ```text
//! qua '//func::name' report.quarb
//! qua '//step[::axis = "//"] @| count' report.quarb
//! ```
//!
//! Node names are the reflection vocabulary (see the spec's
//! "Reflection: The Query Arbor"): `query`, `branch`, `step`,
//! `pipeline`, the stage kinds (`func`, `agg`, `push`, `expr`,
//! `expr-push`, `subcontext`, `select`, `filter`, `recall`), the
//! predicate/operand kinds (`predicate`, `and`, `or`, `not`,
//! `compare`, `path`, `literal`, `arith`, `neg`, `parens`, `topic`,
//! `ordinal`, `context`), and `projection` / `trait`. Where a node
//! has a natural syntax spelling (an axis, an operator, a register
//! reference), the property carries the *spelling* — `::axis` is
//! `"//"`, `::op` is `"+"` — so introspection reads in the language's
//! own vocabulary.
//!
//! The vocabulary is **locked (v1)** and is a compatibility surface:
//! node kinds, property keys, and spellings keep their meaning;
//! growth is additive only (new kinds and keys may appear, existing
//! ones never change or vanish). The root carries the version as
//! `::;vocabulary`, and [`QueryArbor::inventory`] exposes the
//! kind → property-key map for conformance checking (the
//! `vocabulary` test locks it). `param` is *reserved*: it names a
//! fragment parameter in the walker, but reflection sees expanded
//! queries, so it is never emitted.

use std::cell::RefCell;

use crate::adapter::{AstAdapter, NodeId};
use crate::ast::{
    Arg, Axis, Branch, Group, Matcher, Operand, PathElem, PredExpr, Predicate, Projection,
    PushBody, Query, Reach, RegRef,
    Stage, Step, TraitClause,
};
use crate::error::Result;
use crate::value::Value;
use crate::{lexer, parser};

/// One node of the reflected query tree.
struct RNode {
    /// The node kind — the edge name navigation matches.
    name: Option<String>,
    /// Named scalar facts (`::key`), spelled in query syntax where
    /// one exists.
    props: Vec<(String, Value)>,
    parent: Option<usize>,
    children: Vec<usize>,
}

/// A parsed query exposed as an arbor.
pub struct QueryArbor {
    nodes: Vec<RNode>,
    /// The original query text (root `::;source`).
    source: String,
}

impl QueryArbor {
    /// Parse `text` as a Quarb query and reflect it.
    pub fn parse(text: &str) -> Result<Self> {
        let tokens = lexer::lex(text.trim())?;
        let query = parser::parse(&tokens)?;
        let mut arbor = QueryArbor {
            nodes: Vec::new(),
            source: text.trim().to_string(),
        };
        // A synthetic unnamed root wrapping the top-level query node,
        // matching the other document adapters.
        arbor.intern(None, Vec::new(), None);
        arbor.walk_query(&query, 0, None);
        Ok(arbor)
    }

    fn intern(
        &mut self,
        name: Option<&str>,
        props: Vec<(String, Value)>,
        parent: Option<usize>,
    ) -> usize {
        let id = self.nodes.len();
        self.nodes.push(RNode {
            name: name.map(str::to_string),
            props,
            parent,
            children: Vec::new(),
        });
        if let Some(p) = parent {
            self.nodes[p].children.push(id);
        }
        id
    }

    fn walk_query(&mut self, q: &Query, parent: usize, role: Option<&str>) -> usize {
        let mut props = Vec::new();
        if let Some(role) = role {
            props.push(("role".to_string(), Value::Str(role.to_string())));
        }
        if q.outer {
            props.push(("outer".to_string(), Value::Bool(true)));
        }
        let id = self.intern(Some("query"), props, Some(parent));
        for corr in &q.correlations {
            self.walk_query(corr, id, Some("correlation"));
        }
        for branch in &q.branches {
            self.walk_branch(branch, id);
        }
        if !q.pipeline.is_empty() {
            let pipe = self.intern(Some("pipeline"), Vec::new(), Some(id));
            for stage in &q.pipeline {
                self.walk_stage(stage, pipe);
            }
        }
        id
    }

    fn walk_branch(&mut self, b: &Branch, parent: usize) {
        let mut props = Vec::new();
        if b.anchored {
            props.push(("anchored".to_string(), Value::Bool(true)));
        }
        if let Some(m) = &b.mark {
            props.push(("mark".to_string(), Value::Str(m.clone())));
        }
        let id = self.intern(Some("branch"), props, Some(parent));
        for elem in &b.steps {
            self.walk_elem(elem, id);
        }
        if let Some(p) = &b.projection {
            self.walk_projection(p, id);
        }
    }

    fn walk_elem(&mut self, e: &PathElem, parent: usize) {
        match e {
            PathElem::Mark(name) => {
                self.intern(
                    Some("mark"),
                    vec![("name".to_string(), Value::Str(name.clone()))],
                    Some(parent),
                );
            }
            PathElem::Step(s) => self.walk_step(s, parent),
            PathElem::Group(g) => self.walk_group(g, parent),
            // A pattern push reflects with the pipeline kinds — it
            // IS a subcontext (or computed) push, in pattern
            // position.
            PathElem::Push { name, body } => {
                let mut props = Vec::new();
                if let Some(n) = name {
                    props.push(("name".to_string(), Value::Str(n.clone())));
                }
                match body {
                    PushBody::Query(q) => {
                        let id = self.intern(Some("subcontext"), props, Some(parent));
                        self.walk_query(q, id, None);
                    }
                    PushBody::Expr(e) => {
                        let id = self.intern(Some("expr-push"), props, Some(parent));
                        self.walk_operand(e, id);
                    }
                }
            }
        }
    }

    fn walk_group(&mut self, g: &Group, parent: usize) {
        let mut props = vec![("min".to_string(), Value::Int(g.quant.min as i64))];
        if let Some(max) = g.quant.max {
            props.push(("max".to_string(), Value::Int(max as i64)));
        }
        if let Some(mark) = reach_mark(g.reach) {
            props.push(("reach".to_string(), Value::Str(mark.to_string())));
        }
        let id = self.intern(Some("group"), props, Some(parent));
        for alt in &g.alts {
            let alt_id = self.intern(Some("alt"), Vec::new(), Some(id));
            for elem in alt {
                self.walk_elem(elem, alt_id);
            }
        }
        for p in &g.predicates {
            self.walk_predicate(p, id);
        }
    }

    fn walk_step(&mut self, s: &Step, parent: usize) {
        let mut props = vec![
            ("axis".to_string(), Value::Str(axis_spelling(&s.axis))),
            ("matcher".to_string(), Value::Str(matcher_text(&s.matcher))),
            (
                "matcher-kind".to_string(),
                Value::Str(matcher_kind(&s.matcher).to_string()),
            ),
            ("leaf".to_string(), Value::Bool(s.leaf)),
        ];
        if let Axis::Resolve { property, hint } | Axis::ReverseResolve { property, hint } = &s.axis
        {
            props.push(("property".to_string(), Value::Str(property.clone())));
            if let Some(h) = hint {
                props.push(("hint".to_string(), Value::Str(h.clone())));
            }
        }
        let id = self.intern(Some("step"), props, Some(parent));
        for t in &s.traits {
            self.walk_trait(t, id);
        }
        for p in &s.predicates {
            self.walk_predicate(p, id);
        }
    }

    fn walk_trait(&mut self, t: &TraitClause, parent: usize) {
        let alts = Value::List(t.alts.iter().map(|a| Value::Str(a.clone())).collect());
        self.intern(
            Some("trait"),
            vec![("alts".to_string(), alts)],
            Some(parent),
        );
    }

    fn walk_predicate(&mut self, p: &Predicate, parent: usize) {
        match p {
            Predicate::Index(n) => {
                self.intern(
                    Some("predicate"),
                    vec![
                        ("kind".to_string(), Value::Str("index".to_string())),
                        ("value".to_string(), Value::Int(*n)),
                    ],
                    Some(parent),
                );
            }
            Predicate::Range(from, to) => {
                let mut props = vec![("kind".to_string(), Value::Str("range".to_string()))];
                if let Some(f) = from {
                    props.push(("from".to_string(), Value::Int(*f)));
                }
                if let Some(t) = to {
                    props.push(("to".to_string(), Value::Int(*t)));
                }
                self.intern(Some("predicate"), props, Some(parent));
            }
            Predicate::Expr(e) => {
                let id = self.intern(
                    Some("predicate"),
                    vec![("kind".to_string(), Value::Str("expr".to_string()))],
                    Some(parent),
                );
                self.walk_pred_expr(e, id);
            }
        }
    }

    fn walk_pred_expr(&mut self, e: &PredExpr, parent: usize) {
        match e {
            PredExpr::Or(a, b) => {
                let id = self.intern(Some("or"), Vec::new(), Some(parent));
                self.walk_pred_expr(a, id);
                self.walk_pred_expr(b, id);
            }
            PredExpr::And(a, b) => {
                let id = self.intern(Some("and"), Vec::new(), Some(parent));
                self.walk_pred_expr(a, id);
                self.walk_pred_expr(b, id);
            }
            PredExpr::Not(a) => {
                let id = self.intern(Some("not"), Vec::new(), Some(parent));
                self.walk_pred_expr(a, id);
            }
            PredExpr::Compare(l, op, r) => {
                let id = self.intern(
                    Some("compare"),
                    vec![("op".to_string(), Value::Str(cmp_spelling(*op).to_string()))],
                    Some(parent),
                );
                self.walk_operand(l, id);
                self.walk_operand(r, id);
            }
            // A bare truthy operand *is* the predicate; no wrapper.
            PredExpr::Truthy(o) => {
                self.walk_operand(o, parent);
            }
        }
    }

    fn walk_operand(&mut self, o: &Operand, parent: usize) {
        match o {
            Operand::Match {
                scrutinee,
                arms,
                other,
            } => {
                let id = self.intern(Some("match"), Vec::new(), Some(parent));
                self.walk_operand(scrutinee, id);
                for (test, regex, result) in arms {
                    let props = if *regex {
                        vec![("regex".to_string(), Value::Bool(true))]
                    } else {
                        Vec::new()
                    };
                    let arm = self.intern(Some("when"), props, Some(id));
                    self.walk_operand(test, arm);
                    self.walk_operand(result, arm);
                }
                self.walk_operand(other, id);
            }
            Operand::Rel {
                steps,
                projection,
                anchored,
                mark,
            } => {
                let mut props = Vec::new();
                if *anchored {
                    props.push(("anchored".to_string(), Value::Bool(true)));
                }
                if let Some(m) = mark {
                    props.push(("mark".to_string(), Value::Str(m.clone())));
                }
                let id = self.intern(Some("path"), props, Some(parent));
                for e in steps {
                    self.walk_elem(e, id);
                }
                if let Some(p) = projection {
                    self.walk_projection(p, id);
                }
            }
            Operand::Lit(v) => {
                self.intern(
                    Some("literal"),
                    vec![
                        ("value".to_string(), v.clone()),
                        ("type".to_string(), Value::Str(type_name(v).to_string())),
                    ],
                    Some(parent),
                );
            }
            Operand::Arith { op, left, right } => {
                let spelling = match op {
                    crate::ast::ArithOp::Add => "+",
                    crate::ast::ArithOp::Sub => "-",
                    crate::ast::ArithOp::Mul => "*",
                    crate::ast::ArithOp::Div => "div",
                    crate::ast::ArithOp::IDiv => "idiv",
                    crate::ast::ArithOp::Mod => "mod",
                };
                let id = self.intern(
                    Some("arith"),
                    vec![("op".to_string(), Value::Str(spelling.to_string()))],
                    Some(parent),
                );
                self.walk_operand(left, id);
                self.walk_operand(right, id);
            }
            Operand::Neg(inner) => {
                let id = self.intern(Some("neg"), Vec::new(), Some(parent));
                self.walk_operand(inner, id);
            }
            Operand::Group(e) => {
                let id = self.intern(Some("parens"), Vec::new(), Some(parent));
                self.walk_pred_expr(e, id);
            }
            Operand::Recall(r) => {
                self.intern(
                    Some("recall"),
                    vec![("ref".to_string(), Value::Str(reg_spelling(r)))],
                    Some(parent),
                );
            }
            Operand::Topic => {
                self.intern(Some("topic"), Vec::new(), Some(parent));
            }
            Operand::Now => {
                self.intern(Some("now"), Vec::new(), Some(parent));
            }
            Operand::Edge { projection } => {
                let id = self.intern(Some("edge"), Vec::new(), Some(parent));
                if let Some(p) = projection {
                    self.walk_projection(p, id);
                }
            }
            Operand::Edges { projection } => {
                let id = self.intern(Some("edges"), Vec::new(), Some(parent));
                if let Some(p) = projection {
                    self.walk_projection(p, id);
                }
            }
            Operand::Capsae { projection } => {
                let id = self.intern(Some("capsae"), Vec::new(), Some(parent));
                if let Some(p) = projection {
                    self.walk_projection(p, id);
                }
            }
            Operand::Piped { expr, stages } => {
                let id = self.intern(Some("piped"), Vec::new(), Some(parent));
                self.walk_operand(expr, id);
                for st in stages {
                    self.walk_stage(st, id);
                }
            }
            // Children in document order: condition, then, else.
            Operand::Cond { cond, then, other } => {
                let id = self.intern(Some("cond"), Vec::new(), Some(parent));
                self.walk_pred_expr(cond, id);
                self.walk_operand(then, id);
                self.walk_operand(other, id);
            }
            Operand::Ordinal => {
                self.intern(Some("ordinal"), Vec::new(), Some(parent));
            }
            Operand::Capture(n) => {
                self.intern(
                    Some("capture"),
                    vec![("group".to_string(), Value::Int(*n as i64))],
                    Some(parent),
                );
            }
            // Unreachable: reflection sees expanded queries only.
            Operand::Param(name) => {
                self.intern(
                    Some("param"),
                    vec![("name".to_string(), Value::Str(name.clone()))],
                    Some(parent),
                );
            }
            // `$$…` — the outer-scope wrapper: one `outer` node per
            // `$` step, the wrapped operand as its child.
            Operand::Outer(inner) => {
                let id = self.intern(Some("outer"), Vec::new(), Some(parent));
                self.walk_operand(inner, id);
            }
            // An interpolated string: text segments as literal
            // children, hole expressions as operand children, in
            // order.
            Operand::Interp(segs) => {
                let id = self.intern(Some("interp"), Vec::new(), Some(parent));
                for seg in segs {
                    match seg {
                        crate::ast::InterpSeg::Text(t) => {
                            self.intern(
                                Some("literal"),
                                vec![
                                    ("value".to_string(), Value::Str(t.clone())),
                                    ("type".to_string(), Value::Str("text".to_string())),
                                ],
                                Some(id),
                            );
                        }
                        crate::ast::InterpSeg::Expr(e) => self.walk_operand(e, id),
                    }
                }
            }
            Operand::Ctx {
                index,
                steps,
                projection,
            } => {
                let mut props = Vec::new();
                if let Some(i) = index {
                    props.push(("index".to_string(), Value::Int(*i as i64)));
                }
                let id = self.intern(Some("context"), props, Some(parent));
                for e in steps {
                    self.walk_elem(e, id);
                }
                if let Some(p) = projection {
                    self.walk_projection(p, id);
                }
            }
        }
    }

    fn walk_projection(&mut self, p: &Projection, parent: usize) {
        let (kind, key) = match p {
            Projection::Property(k) => ("property", k.clone()),
            Projection::CoreMeta(k) => ("core", Some(k.clone())),
            Projection::AdapterMeta(k) => ("adapter", Some(k.clone())),
        };
        let mut props = vec![("kind".to_string(), Value::Str(kind.to_string()))];
        if let Some(k) = key {
            props.push(("key".to_string(), Value::Str(k)));
        }
        self.intern(Some("projection"), props, Some(parent));
    }

    fn walk_stage(&mut self, stage: &Stage, parent: usize) {
        match stage {
            Stage::Func(call) => {
                let id = self.intern(
                    Some("func"),
                    vec![("name".to_string(), Value::Str(call.name.clone()))],
                    Some(parent),
                );
                self.walk_args(&call.args, id);
            }
            Stage::Agg(call) => {
                let id = self.intern(
                    Some("agg"),
                    vec![("name".to_string(), Value::Str(call.name.clone()))],
                    Some(parent),
                );
                self.walk_args(&call.args, id);
            }
            Stage::Push(name) => {
                let mut props = Vec::new();
                if let Some(n) = name {
                    props.push(("name".to_string(), Value::Str(n.clone())));
                }
                self.intern(Some("push"), props, Some(parent));
            }
            Stage::Subcontext { name, body } => {
                let mut props = Vec::new();
                if let Some(n) = name {
                    props.push(("name".to_string(), Value::Str(n.clone())));
                }
                let id = self.intern(Some("subcontext"), props, Some(parent));
                self.walk_query(body, id, None);
            }
            Stage::Expr(e) => {
                let id = self.intern(Some("expr"), Vec::new(), Some(parent));
                self.walk_operand(e, id);
            }
            Stage::ExprPush { name, expr } => {
                let mut props = Vec::new();
                if let Some(n) = name {
                    props.push(("name".to_string(), Value::Str(n.clone())));
                }
                let id = self.intern(Some("expr-push"), props, Some(parent));
                self.walk_operand(expr, id);
            }
            Stage::Select(p) => {
                let id = self.intern(Some("select"), Vec::new(), Some(parent));
                self.walk_predicate(p, id);
            }
            Stage::Filter(e) => {
                let id = self.intern(Some("filter"), Vec::new(), Some(parent));
                self.walk_pred_expr(e, id);
            }
            Stage::Recall(r) => {
                self.intern(
                    Some("recall"),
                    vec![("ref".to_string(), Value::Str(reg_spelling(r)))],
                    Some(parent),
                );
            }
            Stage::Spread { outer } => {
                let props = if *outer {
                    vec![("outer".to_string(), Value::Bool(true))]
                } else {
                    Vec::new()
                };
                self.intern(Some("spread"), props, Some(parent));
            }
            Stage::Map(inner) => {
                let id = self.intern(Some("map"), Vec::new(), Some(parent));
                self.walk_stage(inner, id);
            }
        }
    }

    fn walk_args(&mut self, args: &[Arg], parent: usize) {
        for arg in args {
            match arg {
                Arg::Lit(v) => {
                    self.intern(
                        Some("literal"),
                        vec![
                            ("value".to_string(), v.clone()),
                            ("type".to_string(), Value::Str(type_name(v).to_string())),
                        ],
                        Some(parent),
                    );
                }
                Arg::Expr(e) => self.walk_operand(e, parent),
                Arg::Range(from, to) => {
                    let mut props = vec![("kind".to_string(), Value::Str("range".to_string()))];
                    if let Some(f) = from {
                        props.push(("from".to_string(), Value::Int(*f)));
                    }
                    if let Some(t) = to {
                        props.push(("to".to_string(), Value::Int(*t)));
                    }
                    self.intern(Some("span"), props, Some(parent));
                }
            }
        }
    }

    /// The vocabulary surface of this arbor: every node kind that
    /// occurs, with the sorted union of its property keys — for
    /// conformance checking against the locked vocabulary.
    pub fn inventory(&self) -> Vec<(String, Vec<String>)> {
        let mut map: Vec<(String, Vec<String>)> = Vec::new();
        for node in &self.nodes {
            let Some(kind) = &node.name else { continue };
            let entry = match map.iter_mut().find(|(k, _)| k == kind) {
                Some((_, keys)) => keys,
                None => {
                    map.push((kind.clone(), Vec::new()));
                    &mut map.last_mut().expect("just pushed").1
                }
            };
            for (key, _) in &node.props {
                if !entry.contains(key) {
                    entry.push(key.clone());
                }
            }
        }
        map.sort();
        for (_, keys) in &mut map {
            keys.sort();
        }
        map
    }

    /// A human-readable locator: the node's kind path with `[n]`
    /// disambiguation among same-kind siblings (as the document
    /// adapters do).
    pub fn locator(&self, node: NodeId) -> String {
        let idx = node.0 as usize;
        if idx == 0 || idx >= self.nodes.len() {
            return "/".to_string();
        }
        let mut segments = Vec::new();
        let mut cur = idx;
        while cur != 0 {
            segments.push(self.segment(cur));
            cur = match self.nodes[cur].parent {
                Some(p) => p,
                None => break,
            };
        }
        segments.reverse();
        format!("/{}", segments.join("/"))
    }

    fn segment(&self, idx: usize) -> String {
        let name = self.nodes[idx].name.clone().unwrap_or_default();
        let Some(parent) = self.nodes[idx].parent else {
            return name;
        };
        let siblings: Vec<usize> = self.nodes[parent]
            .children
            .iter()
            .copied()
            .filter(|&c| self.nodes[c].name == self.nodes[idx].name)
            .collect();
        if siblings.len() > 1 {
            let pos = siblings.iter().position(|&c| c == idx).unwrap_or(0) + 1;
            format!("{name}[{pos}]")
        } else {
            name
        }
    }
}

/// A macro parameter binding for the expansion arbor: one argument
/// form, or the collected remainder (`@rest`).
pub(crate) enum MacroBinding {
    One(Operand),
    Rest(Vec<Operand>),
}

/// Build a macro's expansion arbor: an unnamed root with one child
/// per parameter, named after it. Each argument form's reflected
/// subtree hangs under its parameter node (a `@rest` node carries
/// one subtree per collected argument), and every form's subtree
/// root carries `::form` — its unparsed text — so a macro body can
/// splice arguments back into query text.
pub(crate) fn expansion_arbor(bindings: &[(String, MacroBinding)]) -> QueryArbor {
    let mut arbor = QueryArbor {
        nodes: Vec::new(),
        source: String::new(),
    };
    arbor.intern(None, Vec::new(), None);
    for (name, binding) in bindings {
        let forms: &[Operand] = match binding {
            MacroBinding::One(op) => std::slice::from_ref(op),
            MacroBinding::Rest(ops) => ops,
        };
        let mut props = Vec::new();
        if let MacroBinding::One(op) = binding {
            props.push((
                "form".to_string(),
                Value::Str(crate::unparse::operand_text(op)),
            ));
        }
        let pid = arbor.intern(Some(name), props, Some(0));
        for op in forms {
            let before = arbor.nodes[pid].children.len();
            arbor.walk_operand(op, pid);
            if let Some(&child) = arbor.nodes[pid].children.get(before) {
                arbor.nodes[child].props.push((
                    "form".to_string(),
                    Value::Str(crate::unparse::operand_text(op)),
                ));
            }
        }
    }
    arbor
}

/// The adapter a *data-aware* macro body queries: the expansion
/// arbor's parameters plus the dataset, mounted as a child of the
/// root named `data`. The two id spaces share one `NodeId`, but a
/// data adapter's ids are opaque — the contract permits any `u64`
/// (hash- or pointer-derived), so no tag bit is free to steal
/// without risking a collision with a real key. Data nodes are
/// therefore *interned* into a disjoint high range (`DATA_BASE +
/// index`, above the params arbor's small sequential ids) and
/// translated back to the adapter's real id verbatim, so navigation
/// always lands on the node the adapter returned. The dataset's root
/// node is renamed `data` and reparented under the expansion root.
pub(crate) struct ExpansionAdapter<'a> {
    params: QueryArbor,
    data: &'a dyn AstAdapter,
    /// Interning table: the synthetic data id `DATA_BASE + i` stands
    /// for the adapter's real `data_ids[i]`. Interior mutability
    /// because ids are minted lazily as nodes surface through `&self`
    /// navigation; interning dedups so a node keeps a stable
    /// synthetic id across calls, which the engine's cycle check
    /// (`NodeId` identity) relies on.
    data_ids: RefCell<Vec<NodeId>>,
}

/// The base of the interned data-id range. Params arbor ids are
/// small sequential vector indices, always far below this, so the
/// two id spaces never overlap.
const DATA_BASE: u64 = 1 << 63;

impl<'a> ExpansionAdapter<'a> {
    pub(crate) fn new(params: QueryArbor, data: &'a dyn AstAdapter) -> Self {
        ExpansionAdapter {
            params,
            data,
            data_ids: RefCell::new(Vec::new()),
        }
    }

    /// Intern a real data `NodeId` to its synthetic handle, reusing
    /// an existing slot so the same node always maps to the same id.
    fn tag(&self, n: NodeId) -> NodeId {
        let mut table = self.data_ids.borrow_mut();
        let i = match table.iter().position(|&x| x == n) {
            Some(i) => i,
            None => {
                table.push(n);
                table.len() - 1
            }
        };
        NodeId(DATA_BASE + i as u64)
    }

    /// Recover the real data `NodeId` a synthetic handle stands for,
    /// or `None` for a handle this adapter never minted.
    fn untag(&self, n: NodeId) -> Option<NodeId> {
        self.data_ids
            .borrow()
            .get((n.0 - DATA_BASE) as usize)
            .copied()
    }

    fn is_data(n: NodeId) -> bool {
        n.0 >= DATA_BASE
    }
}

impl AstAdapter for ExpansionAdapter<'_> {
    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if Self::is_data(node) {
            let Some(real) = self.untag(node) else {
                return Vec::new();
            };
            return self
                .data
                .children(real)
                .into_iter()
                .map(|n| self.tag(n))
                .collect();
        }
        let mut out = self.params.children(node);
        if node == NodeId(0) {
            out.push(self.tag(self.data.root()));
        }
        out
    }

    fn name(&self, node: NodeId) -> Option<String> {
        if Self::is_data(node) {
            let real = self.untag(node)?;
            if real == self.data.root() {
                return Some("data".to_string());
            }
            return self.data.name(real);
        }
        self.params.name(node)
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        if Self::is_data(node) {
            let real = self.untag(node)?;
            if real == self.data.root() {
                return Some(NodeId(0));
            }
            return self.data.parent(real).map(|n| self.tag(n));
        }
        self.params.parent(node)
    }

    fn traits(&self, node: NodeId) -> Vec<String> {
        if Self::is_data(node) {
            let Some(real) = self.untag(node) else {
                return Vec::new();
            };
            return self.data.traits(real);
        }
        self.params.traits(node)
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        if Self::is_data(node) {
            return self.data.property(self.untag(node)?, name);
        }
        self.params.property(node, name)
    }

    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        if Self::is_data(node) {
            // Forward to the data adapter's own name test, which the
            // contract lets it override (e.g. git revision syntax
            // landing on a hash-named commit); the default would
            // re-filter and defeat such aliasing.
            let Some(real) = self.untag(node) else {
                return Vec::new();
            };
            return self
                .data
                .children_named(real, name)
                .into_iter()
                .map(|n| self.tag(n))
                .collect();
        }
        // Params side, including the root's mounted `data` child:
        // the default enumerate-and-filter over *this* adapter's
        // children, so the mount stays reachable by name.
        self.children(node)
            .into_iter()
            .filter(|&c| self.name(c).as_deref() == Some(name))
            .collect()
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        if Self::is_data(node) {
            return self.data.default_value(self.untag(node)?);
        }
        self.params.default_value(node)
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        if Self::is_data(node) {
            return self.data.metadata(self.untag(node)?, key);
        }
        self.params.metadata(node, key)
    }

    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        if Self::is_data(node) {
            let Some(real) = self.untag(node) else {
                return Vec::new();
            };
            return self
                .data
                .links(real)
                .into_iter()
                .map(|(l, n)| (l, self.tag(n)))
                .collect();
        }
        Vec::new()
    }

    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        if Self::is_data(node) {
            let Some(real) = self.untag(node) else {
                return Vec::new();
            };
            return self
                .data
                .backlinks(real)
                .into_iter()
                .map(|(l, n)| (l, self.tag(n)))
                .collect();
        }
        Vec::new()
    }

    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        if Self::is_data(node) {
            return self
                .data
                .resolve(self.untag(node)?, property, hint)
                .map(|n| self.tag(n));
        }
        None
    }

    fn link_property(
        &self,
        source: NodeId,
        label: &str,
        target: NodeId,
        name: &str,
    ) -> Option<Value> {
        // Only crosslinks within the dataset carry edge properties;
        // the params arbor has none. Both endpoints must be data.
        if Self::is_data(source) && Self::is_data(target) {
            return self
                .data
                .link_property(self.untag(source)?, label, self.untag(target)?, name);
        }
        None
    }

    fn quantifier_bound(&self) -> usize {
        self.data.quantifier_bound()
    }

    fn allow_shell(&self) -> bool {
        // A data-aware macro body is evaluated here at expansion
        // time; its `sh(...)` stage clears the same gate the dataset
        // carries (the `--allow-shell` flag rides on `self.data`).
        self.data.allow_shell()
    }

    fn invocation_instant(&self) -> Option<(i64, u32)> {
        self.data.invocation_instant()
    }

    fn unit_scale(&self, expr: &str) -> Option<(f64, String)> {
        self.data.unit_scale(expr)
    }
}

/// The default projection of each node kind: its most characteristic
/// scalar, so bare `::` reads naturally (`//func::` is the function
/// name, `//literal::` the value).
fn default_key(kind: &str) -> Option<&'static str> {
    match kind {
        "func" | "agg" | "push" | "expr-push" | "subcontext" => Some("name"),
        "literal" | "predicate" => Some("value"),
        "step" => Some("matcher"),
        "compare" | "arith" => Some("op"),
        "recall" => Some("ref"),
        "projection" => Some("key"),
        _ => None,
    }
}

fn cmp_spelling(op: crate::ast::CmpOp) -> &'static str {
    use crate::ast::CmpOp;
    match op {
        CmpOp::Eq => "=",
        CmpOp::Ne => "!=",
        CmpOp::Lt => "<",
        CmpOp::Le => "<=",
        CmpOp::Gt => ">",
        CmpOp::Ge => ">=",
        CmpOp::Match => "=~",
        CmpOp::NotMatch => "!~",
        CmpOp::Contains => "*=",
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "text",
        Value::List(_) => "list",
        Value::Record(_) => "record",
        // Temporal and quantital values never appear as source
        // literals (they are constructed), but the names are
        // reserved additively.
        Value::Instant { .. } => "instant",
        Value::Duration { .. } => "duration",
        Value::Quantity { .. } => "quantity",
    }
}

fn axis_spelling(a: &Axis) -> String {
    match a {
        Axis::Child => "/".to_string(),
        Axis::Descendant(Reach::All) => "//".to_string(),
        Axis::Descendant(Reach::Proximal) => "//?".to_string(),
        Axis::Descendant(Reach::Distal) => "//!".to_string(),
        Axis::Parent => "\\".to_string(),
        Axis::Ancestor(Reach::All) => "\\\\".to_string(),
        Axis::Ancestor(Reach::Proximal) => "\\\\?".to_string(),
        Axis::Ancestor(Reach::Distal) => "\\\\!".to_string(),
        Axis::NextSibling => ">".to_string(),
        Axis::PrevSibling => "<".to_string(),
        Axis::FollowingSiblings(Reach::All) => ">>".to_string(),
        Axis::FollowingSiblings(Reach::Proximal) => ">>?".to_string(),
        Axis::FollowingSiblings(Reach::Distal) => ">>!".to_string(),
        Axis::PrecedingSiblings(Reach::All) => "<<".to_string(),
        Axis::PrecedingSiblings(Reach::Proximal) => "<<?".to_string(),
        Axis::PrecedingSiblings(Reach::Distal) => "<<!".to_string(),
        Axis::OutLink => "->".to_string(),
        Axis::InLink => "<-".to_string(),
        Axis::Resolve { .. } => "~>".to_string(),
        Axis::ReverseResolve { .. } => "<~".to_string(),
    }
}

fn matcher_text(m: &Matcher) -> String {
    match m {
        Matcher::Name(n) => n.clone(),
        Matcher::Glob(g) => g.glob().glob().to_string(),
        Matcher::Regex(r) => r.as_str().to_string(),
        Matcher::Any => "*".to_string(),
        Matcher::Dot => ".".to_string(),
    }
}

fn matcher_kind(m: &Matcher) -> &'static str {
    match m {
        Matcher::Name(_) => "name",
        Matcher::Glob(_) => "glob",
        Matcher::Regex(_) => "regex",
        Matcher::Any => "any",
        Matcher::Dot => "dot",
    }
}

/// The `?` / `!` reach mark of a path-pattern group, `None` for the
/// keep-all default.
fn reach_mark(r: Reach) -> Option<&'static str> {
    match r {
        Reach::All => None,
        Reach::Proximal => Some("?"),
        Reach::Distal => Some("!"),
    }
}

fn reg_spelling(r: &RegRef) -> String {
    match r {
        RegRef::Top => "$.".to_string(),
        RegRef::Index(n) => format!("$.{n}"),
        RegRef::Named(n) => format!("$.{n}"),
        RegRef::Whole => "@.".to_string(),
        RegRef::Record => "%.".to_string(),
    }
}

impl AstAdapter for QueryArbor {
    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.nodes
            .get(node.0 as usize)
            .map(|n| n.children.iter().map(|&c| NodeId(c as u64)).collect())
            .unwrap_or_default()
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.nodes.get(node.0 as usize).and_then(|n| n.name.clone())
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes
            .get(node.0 as usize)
            .and_then(|n| n.parent)
            .map(|p| NodeId(p as u64))
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let n = self.nodes.get(node.0 as usize)?;
        n.props
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        let n = self.nodes.get(node.0 as usize)?;
        let key = default_key(n.name.as_deref()?)?;
        self.property(node, key)
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        if node.0 == 0 {
            return match key {
                "source" => Some(Value::Str(self.source.clone())),
                // The locked vocabulary version (growth is additive
                // within a version; a break would bump it).
                "vocabulary" => Some(Value::Int(1)),
                _ => None,
            };
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny data adapter whose node ids deliberately set the top
    /// bit — permitted by the `NodeId` contract (opaque, hash- or
    /// pointer-derived). The old bit-63 tag scheme corrupted such
    /// ids: `untag` stripped the bit, so `name`/`children` landed on
    /// a different node than the adapter returned.
    struct HighBitData;

    impl AstAdapter for HighBitData {
        fn root(&self) -> NodeId {
            NodeId(1 << 63)
        }
        fn children(&self, node: NodeId) -> Vec<NodeId> {
            if node == NodeId(1 << 63) {
                vec![NodeId(u64::MAX)]
            } else {
                Vec::new()
            }
        }
        fn name(&self, node: NodeId) -> Option<String> {
            if node == NodeId(u64::MAX) {
                Some("leaf".to_string())
            } else {
                None
            }
        }
        fn property(&self, node: NodeId, name: &str) -> Option<Value> {
            if node == NodeId(u64::MAX) && name == "kind" {
                Some(Value::Str("commit".to_string()))
            } else {
                None
            }
        }
    }

    #[test]
    fn data_ids_with_top_bit_round_trip() {
        let params = expansion_arbor(&[]);
        let data = HighBitData;
        let ad = ExpansionAdapter::new(params, &data);

        // The dataset mounts as a `data` child of the expansion root.
        let root = ad.root();
        let data_node = ad
            .children(root)
            .into_iter()
            .find(|&c| ad.name(c).as_deref() == Some("data"))
            .expect("data mount reachable by name");

        // Its child's real id is u64::MAX (top bit set); name and
        // property must read the *right* underlying node, not one
        // with the top bit stripped.
        let kids = ad.children(data_node);
        assert_eq!(kids.len(), 1);
        let leaf = kids[0];
        assert_eq!(ad.name(leaf).as_deref(), Some("leaf"));
        assert_eq!(
            ad.property(leaf, "kind"),
            Some(Value::Str("commit".to_string()))
        );

        // Synthetic ids are stable: re-surfacing a node yields the
        // same id, which the engine's cycle check relies on.
        assert_eq!(ad.children(data_node), kids);
    }
}
