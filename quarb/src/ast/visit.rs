//! One walk over the syntax tree: a [`Visitor`] sees every query,
//! command, path element, predicate expression and operand, and the
//! `walk_*` functions descend into the children — so a question
//! about a query ("does it name the shell?", "which context index
//! does it reach?") is a visitor with one or two methods, and the
//! descent is written once.
//!
//! A visitor that stops at a boundary (a nested query that opens a
//! context of its own) overrides the method for that node and does
//! not call the walk.

use super::{
    Anchor, Arg, Branch, Command, FnCall, InterpSeg, Operand, PathElem, PredExpr, Predicate,
    PushBody, Query,
};

/// Sees each node of the tree; every method descends by default.
pub trait Visitor {
    fn query(&mut self, q: &Query) {
        walk_query(self, q);
    }
    fn branch(&mut self, b: &Branch) {
        walk_branch(self, b);
    }
    fn command(&mut self, s: &Command) {
        walk_command(self, s);
    }
    fn elem(&mut self, e: &PathElem) {
        walk_elem(self, e);
    }
    fn predicate(&mut self, p: &Predicate) {
        walk_predicate(self, p);
    }
    fn pred(&mut self, e: &PredExpr) {
        walk_pred(self, e);
    }
    fn operand(&mut self, o: &Operand) {
        walk_operand(self, o);
    }
    fn call(&mut self, c: &FnCall) {
        walk_call(self, c);
    }
    fn anchor(&mut self, a: &Anchor) {
        walk_anchor(self, a);
    }
}

pub fn walk_query<V: Visitor + ?Sized>(v: &mut V, q: &Query) {
    for c in &q.correlations {
        v.query(c);
    }
    for b in &q.branches {
        v.branch(b);
    }
    for s in &q.pipeline {
        v.command(s);
    }
}

pub fn walk_branch<V: Visitor + ?Sized>(v: &mut V, b: &Branch) {
    v.anchor(&b.anchor);
    for e in &b.steps {
        v.elem(e);
    }
}

pub fn walk_anchor<V: Visitor + ?Sized>(v: &mut V, a: &Anchor) {
    if let Anchor::Id(op) = a {
        v.operand(op);
    }
}

pub fn walk_call<V: Visitor + ?Sized>(v: &mut V, c: &FnCall) {
    for a in &c.args {
        match a {
            Arg::Expr(e) => v.operand(e),
            Arg::Opt { value: Some(e), .. } => v.operand(e),
            Arg::Lit(_) | Arg::Range(_, _) | Arg::Named { .. } | Arg::Opt { value: None, .. } => {}
        }
    }
}

pub fn walk_command<V: Visitor + ?Sized>(v: &mut V, s: &Command) {
    match s {
        Command::Func(call)
        | Command::Agg(call)
        | Command::RecordWith(call)
        | Command::RecordPush { call, .. } => v.call(call),
        Command::Subcontext { body, .. } => v.query(body),
        Command::Expr(e) | Command::ExprPush { expr: e, .. } | Command::AggPush { body: e, .. } => {
            v.operand(e)
        }
        Command::AggRecord { call, .. } => v.call(call),
        Command::Select(p) => v.predicate(p),
        Command::Filter(e) => v.pred(e),
        Command::Nav(b) => v.branch(b),
        Command::Map(inner) | Command::Repeat { command: inner, .. } => v.command(inner),
        Command::Push(_)
        | Command::Recall(_)
        | Command::AggRecall(_)
        | Command::Spread { .. }
        | Command::FieldsPush => {}
    }
}

pub fn walk_elem<V: Visitor + ?Sized>(v: &mut V, e: &PathElem) {
    match e {
        PathElem::Mark(_) => {}
        PathElem::Step(st) => {
            for p in &st.predicates {
                v.predicate(p);
            }
        }
        PathElem::Group(g) => {
            for alt in &g.alts {
                for e in alt {
                    v.elem(e);
                }
            }
            for p in &g.predicates {
                v.predicate(p);
            }
        }
        PathElem::Push { body, .. } => match body {
            PushBody::Query(q) => v.query(q),
            PushBody::Expr(e) => v.operand(e),
        },
    }
}

pub fn walk_predicate<V: Visitor + ?Sized>(v: &mut V, p: &Predicate) {
    match p {
        Predicate::Expr(e) => v.pred(e),
        Predicate::Index(_) | Predicate::Range(_, _) => {}
    }
}

pub fn walk_pred<V: Visitor + ?Sized>(v: &mut V, e: &PredExpr) {
    match e {
        PredExpr::Or(a, b) | PredExpr::And(a, b) => {
            v.pred(a);
            v.pred(b);
        }
        PredExpr::Not(a) => v.pred(a),
        PredExpr::Compare(l, _, r) => {
            v.operand(l);
            v.operand(r);
        }
        PredExpr::Truthy(o) => v.operand(o),
    }
}

pub fn walk_operand<V: Visitor + ?Sized>(v: &mut V, o: &Operand) {
    match o {
        Operand::Rel { steps, anchor, .. } => {
            v.anchor(anchor);
            for e in steps {
                v.elem(e);
            }
        }
        Operand::Ctx { steps, .. } => {
            for e in steps {
                v.elem(e);
            }
        }
        Operand::Arith { left, right, .. } => {
            v.operand(left);
            v.operand(right);
        }
        Operand::Neg(inner) | Operand::Outer(inner) => v.operand(inner),
        Operand::Group(e) => v.pred(e),
        Operand::Field { base, .. } | Operand::ValueMeta { base, .. } => v.operand(base),
        Operand::PeerReg { at, .. } => v.operand(at),
        Operand::List(items) => {
            for i in items {
                v.operand(i);
            }
        }
        Operand::Piped { expr, commands } => {
            v.operand(expr);
            for s in commands {
                v.command(s);
            }
        }
        Operand::Cond { cond, then, other } => {
            v.pred(cond);
            v.operand(then);
            v.operand(other);
        }
        Operand::Match {
            scrutinee,
            arms,
            other,
        } => {
            v.operand(scrutinee);
            for (t, _, r) in arms {
                v.operand(t);
                v.operand(r);
            }
            v.operand(other);
        }
        Operand::Interp(segs) => {
            for seg in segs {
                match seg {
                    InterpSeg::Text(_) => {}
                    InterpSeg::Expr(e) | InterpSeg::Strict(e, _) => v.operand(e),
                    InterpSeg::Default(e, f) => {
                        v.operand(e);
                        v.operand(f);
                    }
                }
            }
        }
        Operand::Pattern(_)
        | Operand::Lit(_)
        | Operand::Recall(_)
        | Operand::Topic
        | Operand::Seed
        | Operand::NamedCaptures
        | Operand::Ordinal
        | Operand::Param(_)
        | Operand::Capture(_)
        | Operand::Edge { .. }
        | Operand::Capsae { .. }
        | Operand::Now
        | Operand::Edges { .. } => {}
    }
}
