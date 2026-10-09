//! Part of the executor: see `exec/mod.rs`.

use super::*;

/// Record-convention arguments with each field's provenance: every
/// field expression is evaluated under its own read frame.
/// A field's value is what its operand yields (as for
/// a computed push): one value itself, several as a list, none as
/// null — so `%(where = (//token->place::lemma @| unique))` holds
/// the list and an aggregate is not needed to keep it.
pub(super) fn record_fields_prov(
    args: &[Arg],
    adapter: &impl AstAdapter,
    node: NodeId,
    trace: &Trace,
    scope: Scope<'_>,
) -> Vec<(String, Value, Prov)> {
    let mut fields: Vec<(String, Value, Prov)> = Vec::new();
    let mut args = args.iter().peekable();
    while let Some(arg) = args.next() {
        match arg {
            Arg::Lit(Value::Str(name)) => {
                let (value, prov) = match args.next() {
                    Some(Arg::Expr(e)) => {
                        let (v, o) = capture(|| operand_value(adapter, node, e, trace, &[], scope));
                        (v, Prov::leaf(o))
                    }
                    Some(Arg::Lit(v)) => (v.clone(), Prov::default()),
                    Some(Arg::Range(_, _)) | Some(Arg::Named { .. }) | None => {
                        (Value::Null, Prov::default())
                    }
                };
                fields.push((name.clone(), value, prov));
            }
            Arg::Expr(e) => {
                let name = crate::ast::auto_field_name(e)
                    .unwrap_or_default()
                    .to_string();
                let (v, o) = capture(|| operand_value(adapter, node, e, trace, &[], scope));
                fields.push((name, v, Prov::leaf(o)));
            }
            Arg::Lit(v) => fields.push((String::new(), v.clone(), Prov::default())),
            Arg::Range(_, _) | Arg::Named { .. } => {}
        }
    }
    fields
}

/// Split provenance-bearing fields into the record's fields and
/// its provenance.
pub(super) fn split_fields(fields: Vec<(String, Value, Prov)>) -> (Vec<(String, Value)>, Prov) {
    let mut vs = Vec::with_capacity(fields.len());
    let mut ps = Vec::with_capacity(fields.len());
    for (k, v, p) in fields {
        vs.push((k.clone(), v));
        ps.push((k, p));
    }
    (vs, Prov::record(ps))
}

/// [`subcontext_scalar`] with the reduced value's provenance: one
/// value's own, several as a list, a node result the nodes'.
pub(super) fn subcontext_traced(
    body: &Query,
    adapter: &impl AstAdapter,
    node: NodeId,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
) -> (Value, Prov) {
    let (caps, projected) = eval_query_caps_outer(body, adapter, node, trace, outer);
    if projected {
        let mut vs: Vec<(Value, Prov)> = caps
            .into_iter()
            .map(|c| (c.topic.clone().unwrap_or(Value::Null), c.prov))
            .collect();
        match vs.len() {
            0 => (Value::Null, Prov::default()),
            1 => vs.pop().unwrap(),
            _ => {
                let (values, provs): (Vec<Value>, Vec<Prov>) = vs.into_iter().unzip();
                (Value::list(values), Prov::list(provs))
            }
        }
    } else {
        let mut o = Origins::default();
        for c in &caps {
            o.insert(Origin {
                node: c.node,
                stage: stage_now(),
            });
        }
        (Value::Int(caps.len() as i64), Prov::leaf(o))
    }
}

/// A node's default scalar (its name), used when an aggregation runs
/// over an unprojected context.
pub(super) fn node_scalar(adapter: &impl AstAdapter, node: NodeId) -> Value {
    adapter.name(node).map_or(Value::Null, Value::Str)
}

/// Evaluate a value expression against `node`, reduced to its first
/// value (null when it selects nothing).
pub(super) fn operand_scalar(
    adapter: &impl AstAdapter,
    node: NodeId,
    expr: &Operand,
    trace: &Trace,
    scope: Scope<'_>,
) -> Value {
    operand_scalar_bound(adapter, node, expr, trace, &[], scope)
}

/// [`operand_scalar`] under a correlation binding.
pub(super) fn operand_scalar_bound(
    adapter: &impl AstAdapter,
    node: NodeId,
    expr: &Operand,
    trace: &Trace,
    bound: &[Option<NodeId>],
    scope: Scope<'_>,
) -> Value {
    eval_operand(adapter, node, expr, trace, bound, scope)
        .into_iter()
        .next()
        .unwrap_or(Value::Null)
}

/// The node set a bare node operand denotes — a path from the
/// current node (the anchor's rule), a correlation context
/// reference, the served node one scope out — deduplicated, in
/// walk order. The node side of the cross-capsa read.
/// The nodes a relative path reaches from the operand's base: from
/// each anchor node, the steps walked under the witness in force
/// (the tuple under test in a predicate, else the capsa's), the
/// capsa's topic reachable as `$_` in the path's predicates, the
/// members cleared (a path reads the node, not the group), deduped.
pub(super) fn rel_nodes(
    adapter: &impl AstAdapter,
    node: NodeId,
    steps: &[PathElem],
    anchor: &Anchor,
    trace: &Trace,
    bound: &[Option<NodeId>],
    scope: Scope<'_>,
) -> Vec<NodeId> {
    let witness = if bound.is_empty() {
        scope.bindings
    } else {
        bound
    };
    let mut nodes = Vec::new();
    let inner = Scope {
        members: &[],
        ..scope
    };
    with_enclosing_topic(&scope, || {
        for from in operand_from(adapter, anchor, node, trace, bound, scope) {
            nodes.extend(navigate_from(
                steps,
                adapter,
                from,
                trace,
                nested_outer(&inner),
                witness,
                inner.marks,
                inner.register,
            ));
        }
    });
    dedup(nodes)
}

pub(super) fn operand_nodes(
    adapter: &impl AstAdapter,
    node: NodeId,
    at: &Operand,
    trace: &Trace,
    bound: &[Option<NodeId>],
    scope: Scope<'_>,
) -> Vec<NodeId> {
    match at {
        Operand::Rel { steps, anchor, .. } => {
            rel_nodes(adapter, node, steps, anchor, trace, bound, scope)
        }
        Operand::Ctx { index, steps, .. } => {
            let bound = if bound.is_empty() {
                scope.bindings
            } else {
                bound
            };
            let base = match index {
                Some(k) => bound.get(k.saturating_sub(1)).copied(),
                None => Some(Some(node)),
            };
            let Some(Some(base)) = base else {
                return Vec::new();
            };
            if steps.is_empty() {
                vec![base]
            } else {
                dedup(navigate_from(
                    steps,
                    adapter,
                    base,
                    trace,
                    nested_outer(&scope),
                    bound,
                    scope.marks,
                    scope.register,
                ))
            }
        }
        Operand::Outer(inner) => match scope.outer {
            Some(o) => operand_nodes(adapter, o.node.unwrap_or(node), inner, trace, bound, *o),
            None => Vec::new(),
        },
        _ => Vec::new(),
    }
}

pub(super) fn extract_captures(
    e: &PredExpr,
    adapter: &impl AstAdapter,
    node: NodeId,
    trace: &Trace,
    scope: Scope<'_>,
) -> Option<Captured> {
    match e {
        PredExpr::Or(a, b) | PredExpr::And(a, b) => {
            let first = extract_captures(a, adapter, node, trace, scope);
            extract_captures(b, adapter, node, trace, scope).or(first)
        }
        PredExpr::Not(a) => extract_captures(a, adapter, node, trace, scope),
        PredExpr::Compare(l, CmpOp::Match, r) => {
            let pattern = operand_scalar_bound(adapter, node, r, trace, &[], scope);
            let re = compiled_regex(&pattern.to_string())?;
            if re.captures_len() <= 1 {
                return None;
            }
            eval_operand(adapter, node, l, trace, &[], scope)
                .iter()
                .find_map(|v| {
                    let text = v.to_string();
                    re.captures(&text).map(|caps| {
                        let numbered = caps
                            .iter()
                            .skip(1)
                            .map(|g| g.map_or(String::new(), |m| m.as_str().to_string()))
                            .collect();
                        let named = re
                            .capture_names()
                            .enumerate()
                            .filter_map(|(i, n)| {
                                n.map(|n| {
                                    let text = caps
                                        .get(i)
                                        .map_or(String::new(), |m| m.as_str().to_string());
                                    (n.to_string(), text)
                                })
                            })
                            .collect();
                        (numbered, named)
                    })
                })
        }
        PredExpr::Compare(..) | PredExpr::Truthy(_) => None,
    }
}

/// Evaluate a predicate expression under a fixed correlation binding
/// (`bound[k-1]` is the node that `$*k` resolves to). `trace` is still
/// threaded so that a nested navigation operand can carry it into its
/// own predicates.
pub(super) fn eval_pred_expr(
    adapter: &impl AstAdapter,
    node: NodeId,
    e: &PredExpr,
    trace: &Trace,
    bound: &[Option<NodeId>],
    scope: Scope<'_>,
) -> bool {
    match e {
        PredExpr::Or(a, b) => {
            eval_pred_expr(adapter, node, a, trace, bound, scope)
                || eval_pred_expr(adapter, node, b, trace, bound, scope)
        }
        PredExpr::And(a, b) => {
            eval_pred_expr(adapter, node, a, trace, bound, scope)
                && eval_pred_expr(adapter, node, b, trace, bound, scope)
        }
        PredExpr::Not(a) => !eval_pred_expr(adapter, node, a, trace, bound, scope),
        PredExpr::Compare(l, op, r) => {
            // Ruling #33: a pattern literal on `=` / `!=` glob-tests
            // the left values. The stars are syntax — no Value ever
            // carries them, so data can never glob. Null does not
            // participate (like `*=`), which for `!=` keeps the
            // holes-included reading.
            if let Operand::Pattern(segs) = r {
                let lhs = eval_operand(adapter, node, l, trace, bound, scope);
                return match op {
                    CmpOp::Ne => lhs.iter().any(|a| !pattern_test(segs, a)),
                    _ => lhs.iter().any(|a| pattern_test(segs, a)),
                };
            }
            let lhs = eval_operand(adapter, node, l, trace, bound, scope);
            let rhs = eval_operand(adapter, node, r, trace, bound, scope);
            // Existential over a node's own multi-valued projections;
            // each `$*k` operand is a single bound node.
            lhs.iter().any(|a| {
                rhs.iter()
                    .any(|b| compare(a, *op, b, &|e| adapter.unit_scale(e)))
            })
        }
        PredExpr::Truthy(o) => eval_operand(adapter, node, o, trace, bound, scope)
            .iter()
            .any(Value::is_truthy),
    }
}

/// Evaluate an operand against `node` to a list of values (a
/// projection may yield several; a structural path yields one marker
/// per selected node).
pub(super) fn eval_operand(
    adapter: &impl AstAdapter,
    node: NodeId,
    operand: &Operand,
    trace: &Trace,
    bound: &[Option<NodeId>],
    scope: Scope<'_>,
) -> Vec<Value> {
    match operand {
        Operand::Lit(v) => vec![v.clone()],
        // A pattern literal is not a value: the grammar admits it
        // only as a comparison's right operand, where the Compare
        // arm intercepts it before operand evaluation.
        Operand::Pattern(_) => Vec::new(),
        // The value match: scrutinee once, arms in order, first hit
        // wins, only the taken branch evaluates. A regex arm tests
        // like `=~`; a value arm compares by the standard equality.
        Operand::Match {
            scrutinee,
            arms,
            other,
        } => {
            let subject =
                discard(|| operand_scalar_bound(adapter, node, scrutinee, trace, bound, scope));
            for (test, regex, result) in arms {
                let hit = discard(|| {
                    if *regex {
                        let pat = operand_scalar_bound(adapter, node, test, trace, bound, scope);
                        regex_test(&subject, &pat, true)
                    } else {
                        let t = operand_scalar_bound(adapter, node, test, trace, bound, scope);
                        value_eq(&subject, &t, &|e| adapter.unit_scale(e))
                    }
                });
                if hit {
                    return eval_operand(adapter, node, result, trace, bound, scope);
                }
            }
            eval_operand(adapter, node, other, trace, bound, scope)
        }
        // `now()` — the invocation instant, bound by the runner
        // before evaluation (never a clock read here); displayed
        // as UTC. Null when the runner bound none.
        Operand::Now => vec![
            run_option(|o| o.now)
                .map(|(secs, nanos)| Value::Instant {
                    secs,
                    nanos,
                    offset_min: Some(0),
                })
                .unwrap_or(Value::Null),
        ],
        // Capsa-scope operands: a register recall and the topic.
        Operand::Recall(r) => {
            note_all(&recall_prov(scope.register, r).origins);
            vec![recall(scope.register, r)]
        }
        Operand::Topic => {
            if let Some(p) = scope.prov {
                note_all(&p.origins);
            }
            // Inside the predicate of a path evaluated from a value
            // context (`%(lemma = (^//entry[//inflection:: = $_]
            // ::lemma))`), the predicate's own scope has no topic:
            // `$_` is the enclosing capsa's, carried across.
            vec![
                scope
                    .topic
                    .cloned()
                    .or_else(|| with_run(|s| s.enclosing_topic.last().cloned()))
                    .unwrap_or(Value::Null),
            ]
        }
        // `base:name` — a record's field; null on a non-record. A
        // field of a recalled record keeps that field's origins.
        Operand::Field { base, name } if matches!(base.as_ref(), Operand::Recall(_)) => {
            let Operand::Recall(r) = base.as_ref() else {
                unreachable!()
            };
            note_all(&recall_prov(scope.register, r).part(name).origins);
            vec![match recall(scope.register, r) {
                Value::Record(fields) => fields
                    .into_iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v)
                    .unwrap_or(Value::Null),
                _ => Value::Null,
            }]
        }
        // `value:::key` — the value's provenance over the origins it
        // was read from; the answer's own origins are those.
        Operand::ValueMeta { base, key } => {
            let (_, o) = capture(|| eval_operand(adapter, node, base, trace, bound, scope));
            note_all(&o);
            vec![origins_meta(adapter, &o, key).unwrap_or(Value::Null)]
        }
        // A field read on a list of records reads each:
        // a group's member list projects to the members' values, so
        // `@| group(...) | :n | sum` sums a field per group.
        Operand::Field { base, name } => eval_operand(adapter, node, base, trace, bound, scope)
            .into_iter()
            .map(|v| record_field(v, name))
            .collect(),
        // `@(a; b)` — the list literal: every value of every item.
        Operand::List(items) => vec![Value::list(
            items
                .iter()
                .flat_map(|item| eval_operand(adapter, node, item, trace, bound, scope))
                .collect(),
        )],
        // `%+` — the named groups of the last match, as a record.
        Operand::NamedCaptures => vec![Value::Record(
            scope
                .named
                .iter()
                .map(|(k, v)| (k.clone(), Value::Str(v.clone())))
                .collect(),
        )],
        Operand::Ordinal => vec![
            scope
                .ordinal
                .map(|i| Value::Int(i as i64))
                .unwrap_or(Value::Null),
        ],
        // `(cond ? then : else)` — only the taken branch
        // evaluates (an untaken branch's paths never navigate).
        Operand::Cond { cond, then, other } => {
            let taken = if discard(|| eval_pred_expr(adapter, node, cond, trace, bound, scope)) {
                then
            } else {
                other
            };
            eval_operand(adapter, node, taken, trace, bound, scope)
        }
        // `X:.r` — the cross-capsa read: at each node
        // `at` reaches, the regula of every capsa of the stage's
        // input context standing there — the snapshot, so the read
        // is synchronous within the stage. One value per (node,
        // capsa), in context order; none where nobody stands; null
        // where no context exists. The value's origins are the
        // regula's own (where it was read), not the node it is
        // read at: the navigation to `at` is not a read.
        Operand::PeerReg { at, reg } => {
            let Some(peers) = scope.peers else {
                return vec![Value::Null];
            };
            let nodes = discard(|| operand_nodes(adapter, node, at, trace, bound, scope));
            let mut out = Vec::new();
            for n in nodes {
                for &i in peers.at(n) {
                    let p = &peers.caps[i];
                    note_all(&recall_prov(&p.register, reg).origins);
                    out.push(recall(&p.register, reg));
                }
            }
            out
        }
        // `@*` — the stage's input context: bare, the peers'
        // topics; projected, the projection over their nodes. Null
        // where no context exists (navigation predicates).
        Operand::Capsae { projection } => {
            let Some(peers) = scope.peers else {
                return vec![Value::Null];
            };
            let vs: Vec<Value> = peers
                .caps
                .iter()
                .map(|p| match projection {
                    None => {
                        match &p.topic {
                            Some(_) => note_all(&p.prov.origins),
                            None => note(p.node),
                        }
                        p.topic
                            .clone()
                            .unwrap_or_else(|| node_scalar(adapter, p.node))
                    }
                    Some(proj) => {
                        note(p.node);
                        project(adapter, p.node, proj)
                    }
                })
                .collect();
            vec![Value::list(vs)]
        }
        // `(expr | f @| g)` — the pipe tail: each value rides a
        // pseudo-capsa (this node and register, the value as topic)
        // through the ordinary stage machinery, so the semantics
        // mirror the pipeline by construction. `@|` sees a single
        // list value as a context of its elements.
        Operand::Piped { expr, stages } => {
            // A capsa of the tail: this scope's register, captures,
            // marks, witness and arrivals, standing at `n` with
            // `topic` — or none, for a row that is a node, whose
            // value is existence but which a stage meets as the
            // pipeline would: as a node (`@| unique` tells two nodes
            // apart whatever they read as).
            let capsa = |n: NodeId, topic: Option<Value>, prov: Prov| Capsa {
                prov,
                node: n,
                register: scope.register.to_vec(),
                topic,
                members: Vec::new(),
                captures: scope.captures.to_vec(),
                named: scope.named.to_vec(),
                marks: scope.marks.to_vec(),
                bindings: bound.to_vec(),
                arrived: scope.arrived.to_vec(),
            };
            let mut caps: Vec<Capsa> = match expr.as_ref() {
                // A path head keeps its nodes: `(<-link | ::rank @| sum)`
                // reads each reached node, not the node the operand sits
                // on. Its values are what the path operand yields (the
                // projection, else existence), one per node, in order.
                Operand::Rel {
                    steps,
                    projection,
                    anchor,
                } => rel_nodes(adapter, node, steps, anchor, trace, bound, scope)
                    .into_iter()
                    .map(|n| {
                        let topic = projection.as_ref().map(|p| project(adapter, n, p));
                        capsa(n, topic, Prov::at(n, stage_now()))
                    })
                    .collect(),
                // The topic of a group is its members:
                // `($_ | ::lower @| join)` runs the stages over the
                // rows, each on its own node.
                Operand::Topic if !scope.members.is_empty() => scope
                    .members
                    .iter()
                    .map(|m| match &m.topic {
                        Some(t) => capsa(m.node, Some(t.clone()), m.prov.clone()),
                        None => capsa(
                            m.node,
                            Some(node_scalar(adapter, m.node)),
                            Prov::at(m.node, stage_now()),
                        ),
                    })
                    .collect(),
                _ => {
                    let (values, o) =
                        capture(|| eval_operand(adapter, node, expr, trace, bound, scope));
                    let prov = Prov::leaf(o);
                    values
                        .into_iter()
                        .map(|v| capsa(node, Some(v), prov.clone()))
                        .collect()
                }
            };
            for stage in stages.iter() {
                // Absence is `default`'s whole job: an unmatched
                // path yields no values at all, which would starve
                // the stage — seed the null it exists to replace,
                // so `(a) | default$$.b` coalesces missing paths
                // too.
                if caps.is_empty() && matches!(stage, Stage::Func(c) if c.name == "default") {
                    caps.push(capsa(node, Some(Value::Null), Prov::default()));
                }
                // A lone list value explodes into a context for the
                // aggregating forms, each item on the list's node.
                if matches!(stage, Stage::Agg(_) | Stage::Select(_))
                    && caps.len() == 1
                    && matches!(caps[0].topic, Some(Value::List(_)))
                {
                    let whole = caps.pop().expect("one capsa");
                    let items = whole.topic.and_then(Value::into_items).unwrap_or_default();
                    caps = items
                        .into_iter()
                        .enumerate()
                        .map(|(i, v)| capsa(whole.node, Some(v), whole.prov.item(i)))
                        .collect();
                }
                // A scalar stage's expression arguments read the
                // enclosing capsa, not the piped value: in
                // `%(g = loglik(:a; :na; :b; :nb))` the fields are
                // the record's, as `::alt` in `levenshtein(::name;
                // ::alt)` is the node's (the call form desugars to
                // this pipe, and the inner capsa is not visible to
                // the query).
                let resolved;
                let stage = match stage {
                    Stage::Func(call)
                        if stdlib::known_scalar(&call.name)
                            && !matches!(call.name.as_str(), "rec" | "record")
                            && call.args.iter().any(|a| matches!(a, Arg::Expr(_))) =>
                    {
                        resolved = Stage::Func(FnCall {
                            name: call.name.clone(),
                            args: call
                                .args
                                .iter()
                                .map(|a| match a {
                                    Arg::Expr(e) => {
                                        Arg::Lit(operand_scalar(adapter, node, e, trace, scope))
                                    }
                                    other => other.clone(),
                                })
                                .collect(),
                        });
                        &resolved
                    }
                    other => other,
                };
                // Only a row that came in as a node can leave as one:
                // a stage over values that hands on a capsa without a
                // topic hands on null.
                let had_nodes = caps.iter().any(|c| c.topic.is_none());
                caps = apply_stage(stage, caps, adapter, trace, Some(&scope));
                if !had_nodes {
                    for c in &mut caps {
                        if c.topic.is_none() {
                            c.topic = Some(Value::Null);
                        }
                    }
                }
            }
            // What the tail finally yields is noted into the
            // enclosing frame; a row still a node reads as existence.
            for c in &caps {
                note_all(&c.prov.origins);
            }
            caps.into_iter()
                .map(|c| c.topic.unwrap_or(Value::Bool(true)))
                .collect()
        }
        // `@-` — all final-hop crossings: labels bare, each edge's
        // property projected. Empty list where none.
        Operand::Edges { projection } => {
            let vs: Vec<Value> = scope
                .arrived
                .iter()
                .map(|edge| match projection {
                    None | Some(Projection::Property(None)) => Value::Str(edge.label.clone()),
                    Some(Projection::Property(Some(prop))) => adapter
                        .link_property(edge.source, &edge.label, edge.target, prop)
                        .unwrap_or(Value::Null),
                    Some(_) => Value::Null,
                })
                .collect();
            vec![Value::list(vs)]
        }
        // `$-` — the arrived-by edge: its label bare, an edge
        // property projected. Null where no edge is in scope.
        Operand::Edge { projection } => {
            let Some(edge) = scope.edge else {
                return vec![Value::Null];
            };
            let v = match projection {
                None | Some(Projection::Property(None)) => Value::Str(edge.label.clone()),
                Some(Projection::Property(Some(prop))) => adapter
                    .link_property(edge.source, &edge.label, edge.target, prop)
                    .unwrap_or(Value::Null),
                // Refused at parse time.
                Some(_) => Value::Null,
            };
            vec![v]
        }
        // Unreachable: parameters are substituted at expansion time.
        Operand::Param(_) => vec![Value::Null],
        // `$$…` — the same operand, one scope out: the invoking
        // capsa of the enclosing subcontext body (or, in an ON
        // clause, the driving capsa). Node operands (`_::prop`)
        // re-anchor on the invoking capsa's node. Null at the top
        // level, where no enclosing scope exists.
        Operand::Outer(inner) => match scope.outer {
            Some(o) => eval_operand(adapter, o.node.unwrap_or(node), inner, trace, bound, *o),
            None => vec![Value::Null],
        },
        // An interpolated string: each hole contributes its first
        // value as text (null splices as empty).
        Operand::Interp(segs) => {
            let mut out = String::new();
            for seg in segs {
                match seg {
                    InterpSeg::Text(t) => out.push_str(t),
                    InterpSeg::Expr(e) => {
                        let v = operand_scalar_bound(adapter, node, e, trace, bound, scope);
                        out.push_str(&v.to_string());
                    }
                    // Ruling #34: the strict hole refuses instead of
                    // splicing nothing (the ruling-#24 refusal cell,
                    // so the runner reports it as a refusal).
                    InterpSeg::Strict(e, msg) => {
                        let v = operand_scalar_bound(adapter, node, e, trace, bound, scope);
                        if matches!(v, Value::Null) {
                            record_refusal(match msg {
                                Some(m) => m.clone(),
                                None => format!(
                                    "the strict hole '${{{}:?}}' produced nothing",
                                    crate::unparse::operand_text(e)
                                ),
                            });
                        } else {
                            out.push_str(&v.to_string());
                        }
                    }
                    // Ruling #34: `:-` is `default(expr, fallback)`,
                    // bash-faithfully spelled.
                    InterpSeg::Default(e, f) => {
                        let v = operand_scalar_bound(adapter, node, e, trace, bound, scope);
                        let v = if matches!(v, Value::Null) {
                            operand_scalar_bound(adapter, node, f, trace, bound, scope)
                        } else {
                            v
                        };
                        out.push_str(&v.to_string());
                    }
                }
            }
            vec![Value::Str(out)]
        }
        Operand::Capture(n) => vec![
            scope
                .captures
                .get(n - 1)
                .map(|s| Value::Str(s.clone()))
                .unwrap_or(Value::Null),
        ],
        Operand::Rel {
            steps,
            projection,
            anchor,
        } => {
            // The branch anchor's rule, in operand position: `^`
            // navigates from the root, the rounded family from the
            // marked node(s) — the plural forms unioning the
            // values gathered from every matching mark
            // (existential, like any multi-valued operand). An
            // unset anchor yields nothing.
            // In predicates `bound` is the tuple under test; in
            // pipeline stages it is empty and the capsa's witness
            // (`scope.bindings`) takes over — the same rule as `$*k`
            // itself. Either way it travels into the path, so its
            // step predicates read `$*k` per thread.
            let nodes = rel_nodes(adapter, node, steps, anchor, trace, bound, scope);
            for &n in &nodes {
                note(n);
            }
            match projection {
                Some(p) => nodes.iter().map(|&n| project(adapter, n, p)).collect(),
                None => vec![Value::Bool(true); nodes.len()],
            }
        }
        // A correlation context reference. The base is the single node
        // bound to the k-th trace context under the current tuple
        // (`$*k`), or the node being filtered (`$*`, no index). `steps`
        // then descend from that base before projecting, so a context
        // whose data lives one hop down (e.g. a JSON object's field) is
        // still reachable.
        // Arithmetic: each side contributes its first value; the
        // combination rules (numeric readings, null propagation,
        // overflow promotion) live in `arith`. A shaped LITERAL
        // operand types itself first: the author's own
        // spelling — `1900-01-01 + 12d`, `2h + 30min`, `1500m +
        // 2km` — is unambiguous, unlike data text (Rel reads never
        // retype) or comparison criteria (which stay criteria and
        // coerce at the use site).
        Operand::Arith { op, left, right } => {
            let scale = |e: &str| adapter.unit_scale(e);
            let l = operand_scalar_bound(adapter, node, left, trace, bound, scope);
            let r = operand_scalar_bound(adapter, node, right, trace, bound, scope);
            let l = retype_shaped_lit(left, l, &scale);
            let r = retype_shaped_lit(right, r, &scale);
            vec![arith(*op, &l, &r, &scale)]
        }
        Operand::Neg(inner) => {
            let v = operand_scalar_bound(adapter, node, inner, trace, bound, scope);
            vec![match v.numeric_reading() {
                Some(Value::Int(n)) => n
                    .checked_neg()
                    .map(Value::Int)
                    .unwrap_or(Value::Float(-(n as f64))),
                Some(Value::Float(f)) => Value::Float(-f),
                _ => Value::Null,
            }]
        }
        // A boolean group in operand position is its truth value.
        Operand::Group(e) => vec![Value::Bool(discard(|| {
            eval_pred_expr(adapter, node, e, trace, bound, scope)
        }))],
        Operand::Ctx {
            index,
            steps,
            projection,
        } => {
            // In predicates `bound` carries the tuple under test;
            // in pipeline stages it is empty and the capsa's
            // correlation *witness* takes over, so `$*k` projects
            // the joined side into output.
            let bound = if bound.is_empty() {
                scope.bindings
            } else {
                bound
            };
            let base = match index {
                Some(k) => bound.get(k.saturating_sub(1)).copied(),
                None => Some(Some(node)),
            };
            let base = match base {
                // Out of range: no such context.
                None => return Vec::new(),
                // An outer context no tuple admitted: null.
                Some(None) => return vec![Value::Null],
                Some(Some(n)) => n,
            };
            let nodes = if steps.is_empty() {
                vec![base]
            } else {
                navigate_from(
                    steps,
                    adapter,
                    base,
                    trace,
                    nested_outer(&scope),
                    bound,
                    scope.marks,
                    scope.register,
                )
            };
            for &n in &nodes {
                note(n);
            }
            match projection {
                Some(p) => nodes.iter().map(|&n| project(adapter, n, p)).collect(),
                None => vec![Value::Bool(true); nodes.len()],
            }
        }
    }
}

/// A unit-expression resolver for the unital reading's criterion
/// text — the adapter's [`crate::adapter::AstAdapter::unit_scale`],
/// threaded so custom units resolve where comparisons happen.
pub(crate) type UnitScale<'a> = &'a dyn Fn(&str) -> Option<(f64, String)>;
