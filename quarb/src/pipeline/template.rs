//! A quarq template filled (design paper: the quasiquote is a
//! quarq with expansions): every `${…}` of the literal evaluated
//! where the literal is evaluated, in the scope it stands in, and
//! filled by the kind its position gives it — a literal where an
//! operand stands, a name where a name stands, a node where a
//! predicate, a step, a command or a projection stands, a sequence
//! where several of one kind may stand, a text parsed in the
//! expansion's kind. A mismatch is a refusal naming the expansion
//! and what it received.

use crate::pipeline::*;
use crate::syntax::ast::{Hole, ProjShape, name_hole};
use crate::syntax::expand::Form;
use crate::value::Quarq;

/// What a hole reached: the forms of the nodes it navigated to, or
/// the values it evaluated to.
enum Filled {
    Forms(Vec<Form>),
    Values(Vec<Value>),
}

/// The template `q` with its expansions filled on this thread.
pub(crate) fn fill_template<H: Host>(
    host: &H,
    ext: &H::Ext,
    q: &Quarq,
    trace: &H::Trace,
    scope: Scope<'_, H::Ext>,
) -> Value {
    let mut filler = Filler {
        filled: q
            .holes
            .iter()
            .map(|h| reach(host, ext, h, trace, scope))
            .collect(),
        taken: vec![false; q.holes.len()],
        refusal: None,
    };
    let mut body: Vec<Command> = (*q.body).clone();
    filler.commands(&mut body);
    // An expansion the fill did not reach is refused by name (the
    // review of 2026-11-10, pipeline F9). The fill walks commands,
    // operands, path elements and arguments; an expansion written
    // elsewhere (an index, a table's row, a fold's command) was left
    // in the quarq, which then refused where it was applied, as
    // "outside a quarq template", of a query that wrote it inside
    // one.
    if filler.refusal.is_none()
        && let Some(i) = filler.taken.iter().position(|t| !t)
        && let Some(hole) = q.holes.get(i)
    {
        filler.refusal = Some(format!(
            "expansion '${{{}}}' in a template stands where the template is not filled: an \
             expansion fills a command, an operand, a path element or an argument",
            hole.text
        ));
    }
    if let Some(why) = filler.refusal {
        record_refusal(why);
        return Value::Null;
    }
    crate::syntax::parser::quarq_literal(body)
}

/// A hole's reach: the nodes a path or the cursor stands at, as
/// forms, where the arbor answers forms; the values otherwise.
fn reach<H: Host>(
    host: &H,
    ext: &H::Ext,
    h: &Hole,
    trace: &H::Trace,
    scope: Scope<'_, H::Ext>,
) -> Filled {
    match h.expr.as_ref() {
        // the thread's own node — or, after `@|`, the context's
        // nodes gathered as the topic
        Operand::Cursor | Operand::Topic => {
            // the thread's own value first: after `@|` the gathered
            // topic is the threads' values, quarqs they built among
            // them; then the nodes the gathered values came from
            // (their provenance names them); then the thread's node
            let values = crate::pipeline::operand::eval_operand(host, ext, &h.expr, trace, scope);
            if let Some(forms) = forms_of_values(host, &values) {
                return Filled::Forms(forms);
            }
            if matches!(scope.topic, Some(Value::List(_)))
                && let Some(p) = scope.prov
                && !p.parts.is_empty()
            {
                let forms: Vec<Form> = p
                    .parts
                    .iter()
                    .filter_map(|(_, part)| {
                        let position = part.origins.known.first()?.position;
                        host.node_form(&host.ext_at(crate::NodeId::from(position)))
                    })
                    .collect();
                if !forms.is_empty() && forms.len() == p.parts.len() {
                    return Filled::Forms(forms);
                }
            }
            if let Some(f) = host.node_form(ext) {
                return Filled::Forms(vec![f]);
            }
            return Filled::Values(values);
        }
        Operand::Rel {
            steps,
            projection: None,
            anchor,
        } => {
            let threads = crate::pipeline::run::out_of_row_reach(|| {
                host.rel_threads(ext, steps, anchor, None, trace, scope)
            });
            let forms: Vec<Form> = threads
                .iter()
                .filter_map(|(x, _, _)| host.node_form(x))
                .collect();
            if !threads.is_empty() && forms.len() == threads.len() {
                return Filled::Forms(forms);
            }
        }
        _ => {}
    }
    let values = crate::pipeline::operand::eval_operand(host, ext, &h.expr, trace, scope);
    // node references — the context's nodes after `@|`, a held
    // node — reach their forms too
    match forms_of_values(host, &values) {
        Some(forms) => Filled::Forms(forms),
        None => Filled::Values(values),
    }
}

/// The forms of node references and quarqs, when every value is one
/// (a list of them counts as its items, a null for nothing); `None`
/// otherwise. After `@|` the gathered topic is the threads' values:
/// quarqs each thread built fill a group expansion as its
/// alternatives (`q(/${:name}//*) @| q((${$_}))`).
fn forms_of_values<H: Host>(host: &H, values: &[Value]) -> Option<Vec<Form>> {
    fn one<H: Host>(host: &H, v: &Value, out: &mut Vec<Form>) -> Option<bool> {
        match v {
            Value::Node(n) => {
                out.push(host.node_form(&host.ext_at(crate::NodeId(n.id)))?);
                Some(true)
            }
            Value::Quarq(q) => {
                out.push(q.clone().into_form());
                Some(true)
            }
            Value::Null => Some(false),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut any = false;
    for v in values {
        match v {
            Value::List(items) if !items.is_empty() => {
                for i in items.iter() {
                    any |= one(host, i, &mut out)?;
                }
            }
            v => any |= one(host, v, &mut out)?,
        }
    }
    any.then_some(out)
}

struct Filler {
    filled: Vec<Filled>,
    /// Which holes the fill reached, by their place in the list.
    taken: Vec<bool>,
    refusal: Option<String>,
}

impl Filler {
    fn refuse(&mut self, hole: &Hole, why: String) {
        if self.refusal.is_none() {
            self.refusal = Some(format!(
                "expansion '${{{}}}' in a template: {why}",
                hole.text
            ));
        }
    }

    /// The hole's reach, taken: a hole fills once, found by its
    /// place in the template's list (two holes may be written alike).
    fn take(&mut self, hole: &Hole) -> Filled {
        self.take_at(hole.index)
    }

    fn take_at(&mut self, index: usize) -> Filled {
        if let Some(t) = self.taken.get_mut(index) {
            *t = true;
        }
        match self.filled.get_mut(index) {
            Some(f) => std::mem::replace(f, Filled::Values(Vec::new())),
            None => Filled::Values(Vec::new()),
        }
    }

    fn commands(&mut self, commands: &mut Vec<Command>) {
        let mut out: Vec<Command> = Vec::with_capacity(commands.len());
        for mut c in std::mem::take(commands) {
            if let Command::Hole { hole, agg } = &c {
                let agg = *agg;
                match self.take(hole) {
                    Filled::Forms(forms) => {
                        for f in forms {
                            match f.into_commands(agg) {
                                Ok(cmds) => out.extend(cmds),
                                Err(why) => self.refuse(hole, why),
                            }
                        }
                    }
                    Filled::Values(vs) => {
                        for v in vs {
                            match v {
                                Value::Null => {}
                                Value::Quarq(q) => match q.into_form().into_commands(agg) {
                                    Ok(cmds) => out.extend(cmds),
                                    Err(why) => self.refuse(hole, why),
                                },
                                Value::Str(t) => {
                                    match crate::syntax::parser::parse_commands_text(&t) {
                                        Ok(cmds) => out.extend(cmds),
                                        Err(e) => self.refuse(
                                            hole,
                                            format!("the text '{t}' holds no commands: {e}"),
                                        ),
                                    }
                                }
                                other => self.refuse(
                                    hole,
                                    format!(
                                        "{} stands where a command stands",
                                        crate::stdlib::kind_of(&other)
                                    ),
                                ),
                            }
                        }
                    }
                }
                continue;
            }
            self.command(&mut c);
            out.push(c);
        }
        *commands = out;
    }

    fn command(&mut self, c: &mut Command) {
        match c {
            Command::Func(call)
            | Command::Agg(call)
            | Command::RecordWith(call)
            | Command::RecordPush { call, .. }
            | Command::AggRecord { call, .. } => self.args(&mut call.args),
            Command::Expr(e)
            | Command::AggExpr(e)
            | Command::ExprPush { expr: e, .. }
            | Command::AggPush { body: e, .. } => self.operand(e),
            Command::Subcontext { body, .. } => self.query(body),
            Command::Filter(e) => self.pred(e),
            Command::Select(Predicate::Expr(e)) => self.pred(e),
            Command::Nav(b) => self.branch(b),
            Command::Map(inner)
            | Command::Repeat { command: inner, .. }
            | Command::Attach { command: inner, .. } => self.command(inner),
            _ => {}
        }
        // a name expansion on the command's own name
        match c {
            Command::Push(Some(n))
            | Command::Subcontext { name: Some(n), .. }
            | Command::ExprPush { name: Some(n), .. }
            | Command::RecordPush { name: Some(n), .. }
            | Command::AggPush { name: Some(n), .. }
            | Command::Attach { name: Some(n), .. }
            | Command::SessionPush(n) => self.name(n),
            _ => {}
        }
    }

    fn query(&mut self, q: &mut Query) {
        for corr in &mut q.correlations {
            self.query(corr);
        }
        for b in &mut q.branches {
            self.branch(b);
        }
        self.commands(&mut q.pipeline);
    }

    fn branch(&mut self, b: &mut Branch) {
        if let Anchor::Id(op) = &mut b.anchor {
            self.operand(op);
        }
        self.elems(&mut b.steps, &mut b.projection);
    }

    /// The elements of a walk with their holes filled: steps
    /// continue it, a predicate refines the element before, a
    /// projection ends it (and must stand last); a group alternative
    /// that is one hole filled by several branches becomes the
    /// group's alternatives.
    fn elems(&mut self, elems: &mut Vec<PathElem>, projection: &mut Option<Projection>) {
        let mut out: Vec<PathElem> = Vec::with_capacity(elems.len());
        for mut e in std::mem::take(elems) {
            if let PathElem::Hole(hole) = &e {
                let hole = hole.clone();
                let walks: Vec<Result<crate::syntax::expand::Walk, String>> = match self.take(&hole)
                {
                    Filled::Forms(forms) => forms.into_iter().map(Form::into_walk).collect(),
                    Filled::Values(vs) => vs
                        .into_iter()
                        .filter(|v| !matches!(v, Value::Null))
                        .map(|v| match v {
                            Value::Quarq(q) => q.into_form().into_walk(),
                            Value::Str(t) => crate::syntax::parser::parse_elems_text(&t)
                                .map(|(elems, projection)| crate::syntax::expand::Walk {
                                    elems,
                                    predicates: Vec::new(),
                                    projection,
                                })
                                .map_err(|e| format!("the text '{t}' holds no walk: {e}")),
                            other => Err(format!(
                                "{} stands where a path element stands",
                                crate::stdlib::kind_of(&other)
                            )),
                        })
                        .collect(),
                };
                for w in walks {
                    match w {
                        Ok(w) => {
                            if projection.is_some()
                                && (!w.elems.is_empty() || w.projection.is_some())
                            {
                                self.refuse(
                                    &hole,
                                    "a projection ended the branch before it".to_string(),
                                );
                                continue;
                            }
                            out.extend(w.elems);
                            for p in w.predicates {
                                match out.last_mut() {
                                    Some(PathElem::Step(s)) => s.predicates.push(p),
                                    Some(PathElem::Group(g)) if matches!(p, Predicate::Expr(_)) => g.predicates.push(p),
                                    _ => self.refuse(&hole, "a predicate refines the step before it; nothing precedes it here".to_string()),
                                }
                            }
                            if w.projection.is_some() {
                                *projection = w.projection;
                            }
                        }
                        Err(why) => self.refuse(&hole, why),
                    }
                }
                continue;
            }
            self.elem(&mut e);
            out.push(e);
        }
        *elems = out;
        if let Some(p) = projection {
            self.projection(p);
        }
    }

    fn elem(&mut self, e: &mut PathElem) {
        match e {
            PathElem::Hole(_) | PathElem::Param(_) => {}
            PathElem::Mark(Some(n)) => self.name(n),
            PathElem::Mark(None) => {}
            PathElem::Step(s) => self.step(s),
            PathElem::Group(g) => {
                // `(${…})` — the one-hole alternative filled by
                // several branches: the alternatives (design paper:
                // the group expansion)
                let single = g.alts.len() == 1
                    && g.alts[0].len() == 1
                    && matches!(g.alts[0][0], PathElem::Hole(_));
                if single {
                    let PathElem::Hole(hole) = g.alts[0][0].clone() else {
                        unreachable!("checked");
                    };
                    let forms: Vec<Form> = match self.take(&hole) {
                        Filled::Forms(forms) => forms,
                        Filled::Values(vs) => vs
                            .into_iter()
                            .filter_map(|v| match v {
                                Value::Quarq(q) => Some(q.into_form()),
                                _ => None,
                            })
                            .collect(),
                    };
                    let mut alts: Vec<Vec<PathElem>> = Vec::new();
                    for f in forms {
                        match f.into_walk() {
                            Ok(w) if w.predicates.is_empty() && w.projection.is_none() => {
                                alts.push(w.elems)
                            }
                            Ok(_) => self.refuse(
                                &hole,
                                "a group's alternative is a walk without a projection".to_string(),
                            ),
                            Err(why) => self.refuse(&hole, why),
                        }
                    }
                    if !alts.is_empty() {
                        g.alts = alts;
                    }
                    return;
                }
                for alt in &mut g.alts {
                    let mut none = None;
                    self.elems(alt, &mut none);
                }
                for p in &mut g.predicates {
                    if let Predicate::Expr(e) = p {
                        self.pred(e);
                    }
                }
            }
            PathElem::Push { name, body } => {
                if let Some(n) = name {
                    self.name(n);
                }
                match body {
                    PushBody::Query(q) => self.query(q),
                    PushBody::Expr(e) => self.operand(e),
                }
            }
        }
    }

    fn step(&mut self, s: &mut Step) {
        if let crate::syntax::ast::Matcher::Name(n) = &mut s.matcher {
            self.name(n);
        }
        for p in &mut s.predicates {
            if let Predicate::Expr(e) = p {
                self.pred(e);
            }
        }
    }

    fn pred(&mut self, e: &mut PredExpr) {
        match e {
            PredExpr::Or(a, b) | PredExpr::And(a, b) => {
                self.pred(a);
                self.pred(b);
            }
            PredExpr::Not(a) => self.pred(a),
            PredExpr::Compare(l, _, r) => {
                self.operand(l);
                self.operand(r);
            }
            PredExpr::Chain(os, _) => {
                for o in os {
                    self.operand(o);
                }
            }
            PredExpr::Truthy(o) => self.operand(o),
        }
    }

    fn args(&mut self, args: &mut Vec<Arg>) {
        let mut out: Vec<Arg> = Vec::with_capacity(args.len());
        for mut a in std::mem::take(args) {
            if let Arg::Hole(hole) = &a {
                let hole = hole.clone();
                match self.take(&hole) {
                    Filled::Forms(forms) => {
                        for f in forms {
                            match f.into_arg() {
                                Ok(a) => out.push(a),
                                Err(why) => self.refuse(&hole, why),
                            }
                        }
                    }
                    Filled::Values(vs) => {
                        for v in vs {
                            match v {
                                Value::Null => {}
                                Value::Str(t) => {
                                    match crate::syntax::parser::parse_record_items_text(&t) {
                                        Ok(items) => out.extend(items),
                                        Err(e) => self.refuse(
                                            &hole,
                                            format!("the text '{t}' holds no items: {e}"),
                                        ),
                                    }
                                }
                                Value::Quarq(q) => match q.into_form().into_arg() {
                                    Ok(a) => out.push(a),
                                    Err(why) => self.refuse(&hole, why),
                                },
                                v => out.push(Arg::Lit(v)),
                            }
                        }
                    }
                }
                continue;
            }
            match &mut a {
                Arg::Expr(e) | Arg::Opt { value: Some(e), .. } => self.operand(e),
                _ => {}
            }
            out.push(a);
        }
        *args = out;
    }

    /// An operand with its holes filled: a literal expansion — one
    /// value as its literal, several as a list literal, a reached
    /// form as the operand it is.
    fn operand(&mut self, o: &mut Operand) {
        if let Operand::Hole(hole) = o {
            let hole = hole.clone();
            let filled = match self.take(&hole) {
                Filled::Forms(forms) => {
                    if forms.len() == 1 {
                        forms.into_iter().next().expect("one").into_operand()
                    } else {
                        // several reached literals: the list of their
                        // values; anything else has no operand form
                        let mut items = Vec::with_capacity(forms.len());
                        let mut bad = None;
                        for f in forms {
                            match f.into_operand() {
                                Ok(Operand::Lit(v)) => items.push(v),
                                Ok(other) => {
                                    bad = Some(format!(
                                        "several forms stand where one operand stands ('{}' among them)",
                                        crate::syntax::unparse::operand_text(&other)
                                    ));
                                }
                                Err(why) => bad = Some(why),
                            }
                        }
                        match bad {
                            Some(why) => Err(why),
                            None => {
                                Ok(Operand::List(items.into_iter().map(Operand::Lit).collect()))
                            }
                        }
                    }
                }
                // a list fills as the list constructor, its items the
                // literals (`[$col <<= ${$.vals}]`), whatever its length
                Filled::Values(mut vs) => Ok(match vs.len() {
                    0 => Operand::Lit(Value::Null),
                    1 => match vs.pop().expect("one") {
                        Value::List(items) => {
                            Operand::List(items.iter().cloned().map(Operand::Lit).collect())
                        }
                        v => Operand::Lit(v),
                    },
                    _ => Operand::List(vs.into_iter().map(Operand::Lit).collect()),
                }),
            };
            match filled {
                Ok(op) => *o = op,
                Err(why) => self.refuse(&hole, why),
            }
            return;
        }
        match o {
            Operand::Rel {
                steps,
                projection,
                anchor,
            } => {
                if let Anchor::Id(op) = anchor {
                    self.operand(op);
                }
                self.elems(steps, projection);
            }
            Operand::Ctx {
                steps, projection, ..
            } => {
                self.elems(steps, projection);
            }
            // a nested template keeps its own holes: it fills when it
            // is evaluated in its turn
            Operand::Lit(Value::Quarq(_)) => {}
            Operand::Arith { left, right, .. }
            | Operand::Concat { left, right, .. }
            | Operand::Coalesce { left, right } => {
                self.operand(left);
                self.operand(right);
            }
            Operand::Row(items) => {
                for (_, i) in items {
                    self.operand(i);
                }
            }
            Operand::Neg(i)
            | Operand::Desc(i)
            | Operand::Gathered(i)
            | Operand::Spread(i)
            | Operand::Invoking(i)
            | Operand::Position(i)
            | Operand::Driver(i)
            | Operand::Typed { inner: i, .. }
            | Operand::InUnit { inner: i, .. }
            | Operand::InOffset { inner: i, .. } => self.operand(i),
            Operand::Through { base, projection } => {
                self.operand(base);
                self.projection(projection);
            }
            Operand::Field { base, .. } | Operand::ValueMeta { base, .. } => self.operand(base),
            Operand::Projected { base, shape } => {
                self.operand(base);
                match shape {
                    ProjShape::Value(v) => self.operand(v),
                    ProjShape::Columns { items, index, .. } => {
                        for (_, item) in items {
                            self.operand(item);
                        }
                        if let Some(i) = index {
                            self.operand(i);
                        }
                    }
                }
            }
            Operand::LabelIndex { base, labels } => {
                self.operand(base);
                for l in labels {
                    self.operand(l);
                }
            }
            Operand::List(items) => {
                for i in items {
                    self.operand(i);
                }
            }
            Operand::Range { from, to, step } => {
                self.operand(from);
                self.operand(to);
                if let Some(s) = step {
                    self.operand(s);
                }
            }
            Operand::Index { base, .. } => self.operand(base),
            Operand::Piped { expr, commands } => {
                self.operand(expr);
                self.commands(commands);
            }
            Operand::Cond { cond, then, other } => {
                self.pred(cond);
                self.operand(then);
                self.operand(other);
            }
            Operand::Group(e) => self.pred(e),
            Operand::Match {
                scrutinee,
                arms,
                other,
            } => {
                self.operand(scrutinee);
                for (t, _, r) in arms {
                    self.operand(t);
                    self.operand(r);
                }
                self.operand(other);
            }
            Operand::Interp(segs) => {
                for seg in segs {
                    match seg {
                        crate::syntax::ast::InterpSeg::Text(_) => {}
                        crate::syntax::ast::InterpSeg::Expr(e)
                        | crate::syntax::ast::InterpSeg::Strict(e, _) => self.operand(e),
                        crate::syntax::ast::InterpSeg::Default(e, d) => {
                            self.operand(e);
                            self.operand(d);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn projection(&mut self, p: &mut Projection) {
        match p {
            Projection::Property(Some(k))
            | Projection::CoreMeta(k)
            | Projection::AdapterMeta(k) => self.name(k),
            Projection::Property(None) => {}
        }
    }

    /// A name position: the hole's one value spelled as the name
    /// (design paper: a name expansion is always a whole token).
    fn name(&mut self, n: &mut String) {
        let Some((index, text)) = name_hole(n) else {
            return;
        };
        let hole = Hole {
            text: text.to_string(),
            expr: Box::new(Operand::Topic),
            index,
        };
        let spelled: Result<String, String> = match self.take_at(index) {
            Filled::Forms(mut forms) => {
                if forms.len() == 1 {
                    forms.pop().expect("one").into_name()
                } else {
                    Err(format!("{} forms stand where one name stands", forms.len()))
                }
            }
            Filled::Values(mut vs) => match vs.len() {
                1 => match vs.pop().expect("one") {
                    Value::Str(s) => Ok(s),
                    Value::Null => Err("null spells no name".to_string()),
                    v @ (Value::Int(_) | Value::Float(_) | Value::Bool(_)) => Ok(v.to_string()),
                    other => Err(format!("{} spells no name", crate::stdlib::kind_of(&other))),
                },
                n => Err(format!("{n} values stand where one name stands")),
            },
        };
        match spelled {
            Ok(s) => *n = s,
            Err(why) => self.refuse(&hole, why),
        }
    }
}
