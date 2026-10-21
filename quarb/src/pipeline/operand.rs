//! The operand evaluator: a value expression at a thread, generic
//! over the host. Every arm that is a value computes here; the arms
//! that name a node (a relative path, a context reach, the arrived
//! edges, the peers as nodes) ask the host.
use crate::pipeline::*;

/// Evaluate an operand against `node` to a list of values (a
/// projection may yield several; a structural path yields one marker
/// per selected node).
pub(crate) fn eval_operand<H: Host>(
    host: &H,
    ext: &H::Ext,
    operand: &Operand,
    trace: &H::Trace,
    scope: Scope<'_, H::Ext>,
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
            let subject = discard(|| operand_scalar(host, ext, scrutinee, trace, scope));
            for (test, regex, result) in arms {
                let hit = discard(|| {
                    if *regex {
                        let pat = operand_scalar(host, ext, test, trace, scope);
                        regex_test(&subject, &pat, true)
                    } else {
                        let t = operand_scalar(host, ext, test, trace, scope);
                        value_eq(&subject, &t, &|e| host.scale_of(e))
                    }
                });
                if hit {
                    return eval_operand(host, ext, result, trace, scope);
                }
            }
            eval_operand(host, ext, other, trace, scope)
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
                    calendar: Default::default(),
                })
                .unwrap_or(Value::Null),
        ],
        // Capsa-scope operands: a register recall and the topic.
        Operand::Recall(r) => {
            note_all(&recall_origins(scope.register, r));
            vec![recall(scope.register, r)]
        }
        // The seed outside a pipe head reads as the topic.
        Operand::Topic | Operand::Seed => {
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
            note_all(&recall_field_origins(scope.register, r, name));
            vec![match recall(scope.register, r) {
                Value::Record(fields) => fields
                    .into_iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v)
                    .unwrap_or(Value::Null),
                Value::Table(t) => t.column(name).unwrap_or(Value::Null),
                _ => Value::Null,
            }]
        }
        // `value:key` — the value's provenance over the origins it
        // was read from; the answer's own origins are those.
        Operand::ValueMeta { base, key } => {
            let (_, o) = capture(|| eval_operand(host, ext, base, trace, scope));
            note_all(&o);
            vec![host.origins_meta(&o, key).unwrap_or(Value::Null)]
        }
        // A field read on a list of records reads each:
        // a group's member list projects to the members' values, so
        // `@| group(...) | :n | sum` sums a field per group.
        // A leading `:name` on a thread (spec: Field Projections,
        // draft 164), by the topic: a RECORD → its field, null when
        // absent; a LIST → the field of each item as a list (node
        // items their core field); the NODE (no value topic) → the
        // core field, an unknown key refused; any OTHER value (text,
        // number, null, …) → the POSITION's core field, since the
        // thread still stands at a node, a key that is no core
        // field answering null without a refusal (`| :name |
        // indent(:depth - 1)`, `bibref(cmos) | :genus`).
        Operand::Field { base, name } if matches!(**base, Operand::Topic) && scope.topic.is_none() => {
            note(host.origin_at(ext).position);
            vec![host.project(ext, &Projection::CoreMeta(name.clone()))]
        }
        Operand::Field { base, name }
            if matches!(**base, Operand::Topic)
                && !matches!(
                    scope.topic,
                    Some(Value::Record(_) | Value::List(_) | Value::Table(_))
                ) =>
        {
            if crate::graph::navigate::CORE_FIELDS.contains(&name.as_str()) {
                note(host.origin_at(ext).position);
                vec![host.project(ext, &Projection::CoreMeta(name.clone()))]
            } else {
                vec![Value::Null]
            }
        }
        Operand::Field { base, name } => eval_operand(host, ext, base, trace, scope)
            .into_iter()
            .map(|v| record_field(v, name))
            .collect(),
        // `@(a; b)` — the list literal: every value of every item.
        Operand::List(items) => vec![Value::list(
            items
                .iter()
                .flat_map(|item| eval_operand(host, ext, item, trace, scope))
                .collect(),
        )],
        // `a ?? b` — the null default: the left side once, the right
        // only where the left is null (false, 0 and "" stay).
        Operand::Coalesce { left, right } => {
            let v = operand_value(host, ext, left, trace, scope);
            if matches!(v, Value::Null) {
                eval_operand(host, ext, right, trace, scope)
            } else {
                vec![v]
            }
        }
        // `value[i]` / `table[i; j]` — the index of a value (spec:
        // Indexing a Value); a refusal where the value has no such
        // index.
        Operand::Index { base, rows, cols } => {
            let v = operand_value(host, ext, base, trace, scope);
            match crate::stdlib::index_value(v, rows, cols.as_ref()) {
                Ok(v) => vec![v],
                Err(why) => {
                    crate::pipeline::record_refusal(why);
                    vec![Value::Null]
                }
            }
        }
        // `#(…)` — the table literal: the rows to the header's width
        // (a short row filled with holes, a long one refused), a
        // named item by its column, a header inferred from the first
        // full record where none is written.
        // `a..b`, `a..b..step` — the range: every integer from a to
        // b, one value at a time. A step strides and its sign is the
        // direction: a step that points away from the second bound
        // yields nothing (Python's range); without a step the
        // direction is toward the second bound; a zero step is
        // refused.
        Operand::Range { from, to, step } => {
            let int = |e: &Operand, what: &str| -> Result<i64, String> {
                let v = operand_value(host, ext, e, trace, scope);
                match v.numeric_reading() {
                    Some(Value::Int(n)) => Ok(n),
                    _ => Err(format!(
                        "a range's {what} is an integer, not {}",
                        crate::stdlib::kind_of(&v)
                    )),
                }
            };
            let bounds = (|| -> Result<Vec<Value>, String> {
                let a = int(from, "start")?;
                let b = int(to, "end")?;
                let s = match step {
                    Some(s) => int(s, "step")?,
                    None => 1,
                };
                if s == 0 {
                    return Err("a range's step is not zero".into());
                }
                let s = match step {
                    // the written step is the direction
                    Some(_) => s,
                    // no step: toward the second bound
                    None if b < a => -1,
                    None => 1,
                };
                let mut out = Vec::new();
                let mut i = a;
                while if s > 0 { i <= b } else { i >= b } {
                    out.push(Value::Int(i));
                    i = match i.checked_add(s) {
                        Some(n) => n,
                        None => break,
                    };
                }
                Ok(out)
            })();
            match bounds {
                Ok(vs) => vs,
                Err(why) => {
                    crate::pipeline::record_refusal(why);
                    vec![Value::Null]
                }
            }
        }
        // `@%(…)` — the record array: a list of records in the
        // table literal's shape, the header extended by any name a
        // row adds, null where a record lacks a name.
        Operand::RecordArray { header, rows } => {
            let cell = |e: &Operand| operand_value(host, ext, e, trace, scope);
            match record_array(header, rows, cell) {
                Ok(v) => vec![v],
                Err(why) => {
                    crate::pipeline::record_refusal(why);
                    vec![Value::Null]
                }
            }
        }
        Operand::Table { header, rows } => {
            let cell = |e: &Operand| operand_value(host, ext, e, trace, scope);
            match table_literal(header.clone(), rows, cell) {
                Ok(t) => vec![Value::table(t)],
                Err(why) => {
                    crate::pipeline::record_refusal(why);
                    vec![Value::Null]
                }
            }
        }
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
            let taken = if discard(|| eval_pred_expr(host, ext, cond, trace, scope)) {
                then
            } else {
                other
            };
            eval_operand(host, ext, taken, trace, scope)
        }
        // `X:.r` — the cross-capsa read: at each node
        // `at` reaches, the regula of every capsa of the command's
        // input context standing there — the snapshot, so the read
        // is synchronous within the command. One value per (node,
        // capsa), in context order; none where nobody stands; null
        // where no context exists. The value's origins are the
        // regula's own (where it was read), not the node it is
        // read at: the navigation to `at` is not a read.
        Operand::PeerReg { at, reg } => {
            let Some(peers) = scope.peers else {
                return vec![Value::Null];
            };
            let keys = discard(|| host.operand_keys(ext, at, trace, scope));
            let mut out = Vec::new();
            for k in keys {
                for &i in peers.at(&k) {
                    let p = &peers.caps[i];
                    note_all(&recall_prov(&p.register, reg).origins);
                    out.push(recall(&p.register, reg));
                }
            }
            out
        }
        Operand::Topics => host.capsae(scope, None),
        Operand::Positions { projection } => host.positions(scope, projection.as_ref()),
        Operand::Matrix { node, full } => matrix(scope, *node, *full),
        Operand::CtxTable => ctx_table(host, scope),
        Operand::Piped { expr, commands } => {
            // A capsa of the tail: this scope's register, captures,
            // marks, witness and arrivals, standing at `n` with
            // `topic` — or none, for a row that is a node, whose
            // value is existence but which a command meets as the
            // pipeline would: as a node (`@| unique` tells two nodes
            // apart whatever they read as).
            let capsa = |x: H::Ext, topic: Option<Value>, prov: Prov| Thread::<H::Ext> {
                prov,
                register: scope.register.to_vec(),
                topic,
                members: Vec::new(),
                captures: scope.captures.to_vec(),
                named: scope.named.to_vec(),
                ext: x,
            };
            let mut caps: Vec<Thread<H::Ext>> = match expr.as_ref() {
                // A path head keeps its nodes: `(<-link | ::rank @| sum)`
                // reads each reached node, not the node the operand sits
                // on. Its values are what the path operand yields (the
                // projection, else existence), one per node, in order.
                Operand::Rel {
                    steps,
                    projection,
                    anchor,
                } => host
                    .rel_threads(ext, steps, anchor, projection.as_ref(), trace, scope)
                    .into_iter()
                    .map(|(x, topic, prov)| capsa(x, topic, prov))
                    .collect(),
                // The seed of a headless body: the enclosing capsa
                // as the pipeline hands it to a command — its topic
                // on its node, or the node itself when there is no
                // topic (`| cc` meets a node as the pipeline does).
                Operand::Seed if scope.members.is_empty() => {
                    let topic = scope
                        .topic
                        .cloned()
                        .or_else(|| with_run(|s| s.enclosing_topic.last().cloned()));
                    vec![capsa(
                        host.restate(&scope, ext),
                        topic,
                        scope.prov.cloned().unwrap_or_else(|| host.prov_at(ext)),
                    )]
                }
                // The topic of a group is its members:
                // `($_ | ::lower @| join)` runs the commands over the
                // rows, each on its own node.
                Operand::Topic | Operand::Seed if !scope.members.is_empty() => scope
                    .members
                    .iter()
                    .map(|m| match &m.topic {
                        Some(t) => capsa(
                            host.restate(&scope, &m.ext),
                            Some(t.clone()),
                            m.prov.clone(),
                        ),
                        None => capsa(
                            host.restate(&scope, &m.ext),
                            Some(host.node_scalar(&m.ext)),
                            host.prov_at(&m.ext),
                        ),
                    })
                    .collect(),
                _ => {
                    let (values, o) = capture(|| eval_operand(host, ext, expr, trace, scope));
                    let prov = Prov::leaf(o);
                    values
                        .into_iter()
                        .map(|v| capsa(host.restate(&scope, ext), Some(v), prov.clone()))
                        .collect()
                }
            };
            for command in commands.iter() {
                // Absence is `default`'s whole job: an unmatched
                // path yields no values at all, which would starve
                // the command — seed the null it exists to replace,
                // so `(a) | default$$.b` coalesces missing paths
                // too.
                if caps.is_empty() && matches!(command, Command::Func(c) if c.name == "default") {
                    caps.push(capsa(
                        host.restate(&scope, ext),
                        Some(Value::Null),
                        Prov::default(),
                    ));
                }
                // A lone list value explodes into a context for the
                // aggregating forms, each item on the list's node.
                if matches!(command, Command::Agg(_) | Command::Select(_))
                    && caps.len() == 1
                    && matches!(caps[0].topic, Some(Value::List(_)))
                {
                    let whole = caps.pop().expect("one capsa");
                    let items = whole.topic.and_then(Value::into_items).unwrap_or_default();
                    caps = items
                        .into_iter()
                        .enumerate()
                        .map(|(i, v)| {
                            capsa(
                                host.restate(&scope, &whole.ext),
                                Some(v),
                                whole.prov.item(i),
                            )
                        })
                        .collect();
                }
                // A scalar command's expression arguments read the
                // enclosing capsa, not the piped value: in
                // `%(g = loglik(:a; :na; :b; :nb))` the fields are
                // the record's, as `::alt` in `levenshtein(::name;
                // ::alt)` is the node's (the call form desugars to
                // this pipe, and the inner capsa is not visible to
                // the query).
                let resolved;
                let command = match command {
                    Command::Func(call)
                        if stdlib::known_scalar(&call.name)
                            && !matches!(call.name.as_str(), "rec" | "record")
                            && call.args.iter().any(|a| matches!(a, Arg::Expr(_))) =>
                    {
                        resolved = Command::Func(FnCall {
                            name: call.name.clone(),
                            args: call
                                .args
                                .iter()
                                .map(|a| match a {
                                    Arg::Expr(e) => {
                                        Arg::Lit(operand_scalar(host, ext, e, trace, scope))
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
                // a command over values that hands on a capsa without a
                // topic hands on null.
                let had_nodes = caps.iter().any(|c| c.topic.is_none());
                caps = apply_command(command, caps, host, trace, Some(&scope));
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
        Operand::Edges { projection } => host.edges(scope, projection.as_ref()),
        Operand::Edge { projection } => host.edge(scope, projection.as_ref()),
        Operand::Param(_) => vec![Value::Null],
        // `$$…` — the same operand, one scope out: the invoking
        // capsa of the enclosing subcontext body (or, in an ON
        // clause, the driving capsa). Node operands (`_::prop`)
        // re-anchor on the invoking capsa's node. Null at the top
        // level, where no enclosing scope exists.
        Operand::Invoking(inner) => match scope.outer {
            Some(o) if bare_cursor(inner) => host.node_ref(o.ext).into_iter().collect(),
            Some(o) => eval_operand(
                host,
                if H::Ext::standing(&o.view) {
                    o.ext
                } else {
                    ext
                },
                inner,
                trace,
                *o,
            ),
            None => vec![Value::Null],
        },
        // `$$_` — the position: bare, a reference to the thread's
        // node; navigated or projected, read from it like `.`.
        Operand::Position(inner) if bare_cursor(inner) => {
            host.node_ref(ext).into_iter().collect()
        }
        // `_` — the cursor record: the topic and the position, each
        // as its sigil reads it (null where it has none).
        Operand::Cursor => {
            let topic = operand_value(host, ext, &Operand::Topic, trace, scope);
            let position = host.node_ref(ext).unwrap_or(Value::Null);
            vec![Value::Record(vec![
                ("topic".to_string(), topic),
                ("position".to_string(), position),
            ])]
        }
        // navigated or projected from a thread with no position
        // (after a reduction): nothing, by construction
        Operand::Position(inner) if !host.placed(ext) => Vec::new(),
        Operand::Position(inner) => eval_operand(host, ext, inner, trace, scope),
        // `$$0` — the driver of the correlation whose join
        // condition this is (the parser refuses it elsewhere).
        Operand::Driver(inner) => match scope.driver {
            Some(d) if bare_cursor(inner) => host.node_ref(d.ext).into_iter().collect(),
            Some(d) => eval_operand(
                host,
                if H::Ext::standing(&d.view) {
                    d.ext
                } else {
                    ext
                },
                inner,
                trace,
                *d,
            ),
            None => vec![Value::Null],
        },
        Operand::Interp(segs) => {
            let mut out = String::new();
            for seg in segs {
                match seg {
                    InterpSeg::Text(t) => out.push_str(t),
                    InterpSeg::Expr(e) => {
                        let v = operand_scalar(host, ext, e, trace, scope);
                        out.push_str(&v.to_string());
                    }
                    // Ruling #34: the strict expansion refuses instead of
                    // splicing nothing (the ruling-#24 refusal cell,
                    // so the runner reports it as a refusal).
                    InterpSeg::Strict(e, msg) => {
                        let v = operand_scalar(host, ext, e, trace, scope);
                        if matches!(v, Value::Null) {
                            record_refusal(match msg {
                                Some(m) => m.clone(),
                                None => format!(
                                    "the strict expansion '${{{}:?}}' produced nothing",
                                    crate::syntax::unparse::operand_text(e)
                                ),
                            });
                        } else {
                            out.push_str(&v.to_string());
                        }
                    }
                    // Ruling #34: `:-` is `default(expr, fallback)`,
                    // bash-faithfully spelled.
                    InterpSeg::Default(e, f) => {
                        let v = operand_scalar(host, ext, e, trace, scope);
                        let v = if matches!(v, Value::Null) {
                            operand_scalar(host, ext, f, trace, scope)
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
        } => host.rel(ext, steps, anchor, projection.as_ref(), trace, scope),
        // An operand that reaches several values forks the
        // expression (spec: Value Expressions, draft 164): one result
        // per value, each its own row; two forking operands give the
        // direct product, the left changing slowest; an operand that
        // reaches nothing is null. Over a list, a record or a table
        // the operator broadcasts (spec: Arithmetic over structures);
        // a shape mismatch is a refusal, not a null.
        Operand::Arith { op, left, right } => {
            let scale = |e: &str| host.scale_of(e);
            let ls = forking(eval_operand(host, ext, left, trace, scope));
            let rs = forking(eval_operand(host, ext, right, trace, scope));
            let mut out = Vec::with_capacity(ls.len() * rs.len());
            for l in &ls {
                let l = retype_shaped_lit(left, l.clone(), &scale);
                for r in &rs {
                    let r = retype_shaped_lit(right, r.clone(), &scale);
                    out.push(match crate::stdlib::arith::broadcast(*op, &l, &r, &scale) {
                        Ok(v) => v,
                        Err(why) => {
                            crate::pipeline::record_refusal(why);
                            Value::Null
                        }
                    });
                }
            }
            out
        }
        // The concatenation (spec: Concatenation): the left side
        // gives the shape; a forking operand forks the expression as
        // for every binary operator; a row on the right is laid under
        // the left's header, its items gathered as a constructor's
        // are. A refusal nulls the result.
        Operand::Concat { mode, left, right } => {
            let ls = forking(eval_operand(host, ext, left, trace, scope));
            let rs: Vec<Result<Value, String>> = match right.as_ref() {
                Operand::Row(items) => {
                    let row: Vec<(Option<String>, Value)> = items
                        .iter()
                        .map(|(name, e)| {
                            (name.clone(), gathered(eval_operand(host, ext, e, trace, scope)))
                        })
                        .collect();
                    ls.iter()
                        .map(|l| crate::stdlib::concat_row(*mode, l.clone(), row.clone()))
                        .collect()
                }
                _ => {
                    let rs = forking(eval_operand(host, ext, right, trace, scope));
                    let mut out = Vec::with_capacity(ls.len() * rs.len());
                    for l in &ls {
                        for r in &rs {
                            out.push(crate::stdlib::concat(*mode, l.clone(), r.clone()));
                        }
                    }
                    out
                }
            };
            rs.into_iter()
                .map(|out| match out {
                    Ok(v) => v,
                    Err(why) => {
                        crate::pipeline::record_refusal(format!("{}: {why}", mode.symbol()));
                        Value::Null
                    }
                })
                .collect()
        }
        // A row stands only on a concatenation's right, where the
        // arm above reads it.
        Operand::Row(_) => vec![Value::Null],
        Operand::Neg(inner) => forking(eval_operand(host, ext, inner, trace, scope))
            .into_iter()
            .map(|v| match v.numeric_reading() {
                Some(Value::Int(n)) => n
                    .checked_neg()
                    .map(Value::Int)
                    .unwrap_or(Value::Float(-(n as f64))),
                Some(Value::Float(f)) => Value::Float(-f),
                _ => Value::Null,
            })
            .collect(),
        // A reducing callee's first argument: the values it reaches
        // as one list, as the command's input would be; one value as
        // it is (draft 164).
        Operand::Gathered(inner) => vec![gathered(eval_operand(host, ext, inner, trace, scope))],
        // A boolean group in operand position is its truth value.
        Operand::Group(e) => vec![Value::Bool(discard(|| {
            eval_pred_expr(host, ext, e, trace, scope)
        }))],
        Operand::Ctx {
            index,
            steps,
            projection,
        } => host.ctx(ext, *index, steps, projection.as_ref(), trace, scope),
    }
}

/// Whether a cursor operand's tail is bare — no path, no
/// projection — so the operand denotes the node itself: a reference
/// in value position.
pub(crate) fn bare_cursor(inner: &Operand) -> bool {
    matches!(
        inner,
        Operand::Rel {
            steps,
            projection: None,
            anchor: Anchor::Current,
        } if steps.is_empty()
    )
}

/// Evaluate a value expression against `node`, reduced to its first
/// value (null when it selects nothing).
/// A binary operand's values: an operand that reaches nothing is
/// null, one value (draft 164).
fn forking(values: Vec<Value>) -> Vec<Value> {
    if values.is_empty() {
        vec![Value::Null]
    } else {
        values
    }
}

/// Several values gathered into one list, as a constructor's item
/// and a reducing callee's input take them; one value as it is.
fn gathered(mut values: Vec<Value>) -> Value {
    if values.len() == 1 {
        values.pop().expect("one")
    } else {
        Value::list(values)
    }
}

pub(crate) fn operand_scalar<H: Host>(
    host: &H,
    ext: &H::Ext,
    expr: &Operand,
    trace: &H::Trace,
    scope: Scope<'_, H::Ext>,
) -> Value {
    eval_operand(host, ext, expr, trace, scope)
        .into_iter()
        .next()
        .unwrap_or(Value::Null)
}

/// [`operand_scalar`] under a correlation binding.

pub(crate) fn record_fields_prov<H: Host>(
    args: &[Arg],
    host: &H,
    ext: &H::Ext,
    trace: &H::Trace,
    scope: Scope<'_, H::Ext>,
) -> Result<Vec<(String, Value, Prov)>, String> {
    let mut fields: Vec<(String, Value, Prov)> = Vec::new();
    let mut args = args.iter().peekable();
    while let Some(arg) = args.next() {
        match arg {
            Arg::Lit(Value::Str(name)) => {
                let (value, prov) = match args.next() {
                    Some(Arg::Expr(e)) => {
                        let (v, o) = capture(|| operand_value(host, ext, e, trace, scope));
                        (v, Prov::leaf(o))
                    }
                    Some(Arg::Lit(v)) => (v.clone(), Prov::default()),
                    Some(Arg::Range(_, _))
                    | Some(Arg::Named { .. })
                    | Some(Arg::Opt { .. })
                    | Some(Arg::Pairs { .. })
                    | None => (Value::Null, Prov::default()),
                };
                fields.push((name.clone(), value, prov));
            }
            Arg::Expr(e) => {
                let name = crate::syntax::ast::auto_field_name(e)
                    .unwrap_or_default()
                    .to_string();
                let (v, o) = capture(|| operand_value(host, ext, e, trace, scope));
                fields.push((name, v, Prov::leaf(o)));
            }
            Arg::Lit(v) => fields.push((String::new(), v.clone(), Prov::default())),
            // The pairwise fields: the names, then the values, in
            // order; the counts must agree and no name may repeat.
            Arg::Pairs { keys, values } => {
                let k = operand_value(host, ext, keys, trace, scope);
                let (v, o) = capture(|| operand_value(host, ext, values, trace, scope));
                let names: Vec<String> = match &k {
                    Value::List(l) => l
                        .iter()
                        .map(|n| match n {
                            Value::Str(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .collect(),
                    other => {
                        return Err(format!(
                            "the pairwise field takes a list of names on the left of '=', not {}",
                            crate::stdlib::kind_of(other)
                        ));
                    }
                };
                let items: Vec<Value> = match &v {
                    Value::List(l) => l.to_vec(),
                    other => {
                        return Err(format!(
                            "the pairwise field takes a list of values on the right of '=', not {}",
                            crate::stdlib::kind_of(other)
                        ));
                    }
                };
                if names.len() != items.len() {
                    return Err(format!(
                        "the pairwise field's names and values differ in number, {} and {}",
                        names.len(),
                        items.len()
                    ));
                }
                for (name, value) in names.into_iter().zip(items) {
                    if fields.iter().any(|(n, _, _)| *n == name) {
                        return Err(format!("'{name}' is already a field of the record"));
                    }
                    fields.push((name, value, Prov::leaf(o.clone())));
                }
            }
            Arg::Range(_, _) | Arg::Named { .. } | Arg::Opt { .. } => {}
        }
    }
    Ok(fields)
}

pub(crate) fn extract_captures<H: Host>(
    e: &PredExpr,
    host: &H,
    ext: &H::Ext,
    trace: &H::Trace,
    scope: Scope<'_, H::Ext>,
) -> Option<Captured> {
    match e {
        PredExpr::Or(a, b) | PredExpr::And(a, b) => {
            let first = extract_captures(a, host, ext, trace, scope);
            extract_captures(b, host, ext, trace, scope).or(first)
        }
        PredExpr::Not(a) => extract_captures(a, host, ext, trace, scope),
        PredExpr::Compare(l, CmpOp::Match, r) => {
            let pattern = operand_scalar(host, ext, r, trace, scope);
            let re = compiled_regex(&pattern.to_string())?;
            if re.captures_len() <= 1 {
                return None;
            }
            eval_operand(host, ext, l, trace, scope)
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
        PredExpr::Compare(..) | PredExpr::Chain(..) | PredExpr::Truthy(_) => None,
    }
}

/// Evaluate a predicate expression under a fixed correlation binding
/// (`bound[k-1]` is the node that `$*k` resolves to). `trace` is still
/// threaded so that a nested navigation operand can carry it into its
/// own predicates.
pub(crate) fn eval_pred_expr<H: Host>(
    host: &H,
    ext: &H::Ext,
    e: &PredExpr,
    trace: &H::Trace,
    scope: Scope<'_, H::Ext>,
) -> bool {
    match e {
        PredExpr::Or(a, b) => {
            eval_pred_expr(host, ext, a, trace, scope) || eval_pred_expr(host, ext, b, trace, scope)
        }
        PredExpr::And(a, b) => {
            eval_pred_expr(host, ext, a, trace, scope) && eval_pred_expr(host, ext, b, trace, scope)
        }
        PredExpr::Not(a) => !eval_pred_expr(host, ext, a, trace, scope),
        PredExpr::Compare(l, op, r) => {
            // Ruling #33: a pattern literal on `=` / `!=` glob-tests
            // the left values. The stars are syntax — no Value ever
            // carries them, so data can never glob. Null does not
            // participate (like `*=`), which for `!=` keeps the
            // holes-included reading.
            if let Operand::Pattern(segs) = r {
                let lhs = eval_operand(host, ext, l, trace, scope);
                return match op {
                    CmpOp::Ne => lhs.iter().any(|a| !pattern_test(segs, a)),
                    _ => lhs.iter().any(|a| pattern_test(segs, a)),
                };
            }
            let lhs = eval_operand(host, ext, l, trace, scope);
            let rhs = eval_operand(host, ext, r, trace, scope);
            holds(l, &lhs, *op, r, &rhs, &|e| host.scale_of(e))
        }
        // Every operand once, then the adjacent pairs.
        PredExpr::Chain(operands, ops) => {
            let values: Vec<Vec<Value>> = operands
                .iter()
                .map(|o| eval_operand(host, ext, o, trace, scope))
                .collect();
            ops.iter().enumerate().all(|(i, op)| {
                holds(
                    &operands[i],
                    &values[i],
                    *op,
                    &operands[i + 1],
                    &values[i + 1],
                    &|e| host.scale_of(e),
                )
            })
        }
        PredExpr::Truthy(o) => eval_operand(host, ext, o, trace, scope)
            .iter()
            .any(Value::is_truthy),
    }
}

/// The record array `@%(…)` as the list of records it stands for
/// (spec: List Construction): positional items take the header's
/// names in order, named items their own; a name the header lacks
/// joins the names after it, and every record carries every name,
/// null where its row said nothing.
pub(crate) fn record_array(
    header: &[String],
    rows: &[Vec<(Option<String>, Operand)>],
    mut cell: impl FnMut(&Operand) -> Value,
) -> Result<Value, String> {
    let mut names: Vec<String> = header.to_vec();
    let mut records: Vec<Vec<(String, Value)>> = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let n = i + 1;
        let mut fields: Vec<(String, Value)> = Vec::with_capacity(row.len());
        let mut next = 0;
        for (name, e) in row {
            let v = cell(e);
            let name = match name {
                Some(name) => {
                    if !names.contains(name) {
                        names.push(name.clone());
                    }
                    name.clone()
                }
                None => {
                    if next >= header.len() {
                        return Err(format!(
                            "row {n} has more positional values than the header has names ({}); name the rest",
                            header.len()
                        ));
                    }
                    next += 1;
                    header[next - 1].clone()
                }
            };
            match fields.iter_mut().find(|(k, _)| *k == name) {
                Some((_, slot)) => *slot = v,
                None => fields.push((name, v)),
            }
        }
        records.push(fields);
    }
    Ok(Value::list(
        records
            .into_iter()
            .map(|fields| {
                Value::Record(
                    names
                        .iter()
                        .map(|k| {
                            (
                                k.clone(),
                                fields
                                    .iter()
                                    .find(|(n, _)| n == k)
                                    .map(|(_, v)| v.clone())
                                    .unwrap_or(Value::Null),
                            )
                        })
                        .collect(),
                )
            })
            .collect(),
    ))
}

pub(crate) fn table_literal(
    mut header: Option<Vec<String>>,
    rows: &[crate::syntax::ast::TableRow],
    mut cell: impl FnMut(&Operand) -> Value,
) -> Result<crate::Table, String> {
    use crate::syntax::ast::TableRow;
    let mut out: Vec<Vec<Value>> = Vec::with_capacity(rows.len());
    let mut width: Option<usize> = header.as_ref().map(Vec::len);
    for (i, row) in rows.iter().enumerate() {
        let n = i + 1;
        let mut values: Vec<Value> = match row {
            TableRow::List(items) => items.iter().map(&mut cell).collect(),
            TableRow::Record(items) => {
                // Without a header: named rows infer it from the
                // first row's names; positional rows are the array
                // face, the positions naming the columns.
                if header.is_none()
                    && out.is_empty()
                    && items.iter().any(|(name, _)| name.is_some())
                {
                    let names: Vec<String> = items
                        .iter()
                        .enumerate()
                        .map(|(j, (name, _))| {
                            name.clone().ok_or_else(|| {
                                format!("row {n}: item {} has no name while the others name their columns", j + 1)
                            })
                        })
                        .collect::<Result<_, _>>()?;
                    width = Some(names.len());
                    header = Some(names);
                }
                let Some(h) = header.as_ref() else {
                    if let Some((name, _)) = items.iter().find(|(name, _)| name.is_some()) {
                        return Err(format!(
                            "row {n}: '{}' names no column of an array (its columns are positions); write the header first",
                            name.as_deref().unwrap_or("")
                        ));
                    }
                    let values: Vec<Value> = items.iter().map(|(_, e)| cell(e)).collect();
                    let w = *width.get_or_insert(values.len());
                    if values.len() > w {
                        return Err(format!(
                            "row {n} has {} values, the table {w} columns",
                            values.len()
                        ));
                    }
                    let mut values = values;
                    values.resize(w, Value::Null);
                    out.push(values);
                    continue;
                };
                let mut values = vec![Value::Null; h.len()];
                let mut next = 0;
                for (name, e) in items {
                    let v = cell(e);
                    match name {
                        Some(name) => match h.iter().position(|c| c == name) {
                            Some(p) => values[p] = v,
                            None => {
                                return Err(format!(
                                    "row {n}: '{name}' is not a column of the table (the header: {})",
                                    h.join(", ")
                                ));
                            }
                        },
                        None => {
                            if next >= h.len() {
                                return Err(format!(
                                    "row {n} has {} values, the header {} columns",
                                    items.len(),
                                    h.len()
                                ));
                            }
                            values[next] = v;
                            next += 1;
                        }
                    }
                }
                values
            }
        };
        let w = *width.get_or_insert(values.len());
        if values.len() > w {
            return Err(format!(
                "row {n} has {} values, the table {w} columns",
                values.len()
            ));
        }
        values.resize(w, Value::Null);
        // The table is flat: a cell holds a scalar or null.
        if let Some((j, v)) = values.iter().enumerate().find(|(_, v)| !flat_cell(v)) {
            return Err(format!(
                "row {n}, column {}: a table is flat, its cell a scalar or null, not {}",
                j + 1,
                crate::stdlib::kind_of(v)
            ));
        }
        out.push(values);
    }
    Ok(crate::Table::new(header, out))
}

/// An expression's value for a push (the computed push lands a
/// list when the expression yields several values, the spec's
/// `.r(@-::prop)` rule): null, the one value, or the list.
pub(crate) fn operand_value<H: Host>(
    host: &H,
    ext: &H::Ext,
    expr: &Operand,
    trace: &H::Trace,
    scope: Scope<'_, H::Ext>,
) -> Value {
    let values = eval_operand(host, ext, expr, trace, scope);
    match values.len() {
        0 => Value::Null,
        1 => values.into_iter().next().unwrap(),
        _ => Value::list(values),
    }
}

/// The node-only view of [`navigate_paths`], for operand-position
/// paths: their pattern registers have no result capsa to land on
/// and are discarded.
/// The scope a nested path's predicates see one scope out. Inside a
/// capsa (a pipeline command's operand, `%(n = (^//p[:: < $.v]))`)
/// the path forks from that capsa — its register is seeded from it
/// and `_::x` / `$$…` read the capsa itself; in a navigation
/// predicate (no capsa of its own) the invoking scope stays what it
/// was.
/// An expression's value on a capsa: its first value, or on a
/// group every member's value as a list — one row's
/// worth per member, so `| ::lower` after a group is the members'
/// forms.
pub(crate) fn group_value<H: Host>(
    host: &H,
    ext: &H::Ext,
    expr: &Operand,
    trace: &H::Trace,
    scope: Scope<'_, H::Ext>,
) -> Value {
    if scope.members.is_empty() {
        return operand_scalar(host, ext, expr, trace, scope);
    }
    let values = eval_operand(host, ext, expr, trace, scope);
    match values.len() {
        0 => Value::Null,
        1 => values.into_iter().next().unwrap(),
        _ => Value::list(values),
    }
}

/// One comparison over two operands' values: existential over each
/// side's values (a node's own multi-valued projections; each `$*k`
/// operand is a single bound node), except that a range on the
/// container side of a containment is the one list it spreads to
/// (spec: Containment).
fn holds(
    l: &Operand,
    lhs: &[Value],
    op: CmpOp,
    r: &Operand,
    rhs: &[Value],
    scale: crate::stdlib::arith::UnitScale,
) -> bool {
    let spread = |o: &Operand, vs: &[Value]| -> Vec<Value> {
        if matches!(o, Operand::Range { .. }) {
            vec![Value::list(vs.to_vec())]
        } else {
            vs.to_vec()
        }
    };
    match op {
        CmpOp::Within => {
            let rs = spread(r, rhs);
            lhs.iter()
                .any(|a| rs.iter().any(|b| compare(a, op, b, scale)))
        }
        CmpOp::Encloses => {
            let ls = spread(l, lhs);
            ls.iter()
                .any(|a| rhs.iter().any(|b| compare(a, op, b, scale)))
        }
        _ => lhs
            .iter()
            .any(|a| rhs.iter().any(|b| compare(a, op, b, scale))),
    }
}

/// `#*` / `##*` — the context as a table (spec: Context Accessors):
/// one row per thread in context order, one column per value (or
/// node) regula named as `%..` (`%%..`) names them, a named push by
/// its name at its first-push position, an anonymous one `.N`;
/// cells absent on a thread are null; the columns are typed by the
/// table rule as the literal's are. A view built when read: `$.n`
/// keeps a pushed value's own type. Null where no context exists.
/// `#_` — the context table: one row per thread in context order,
/// the columns `topic` and `position` (spec: The Cursor); null
/// where no context is at hand, as `#*` is.
pub(crate) fn ctx_table<H: Host>(host: &H, scope: Scope<'_, H::Ext>) -> Vec<Value> {
    if scope.peers.is_none() {
        return vec![Value::Null];
    }
    let items = |vs: Vec<Value>| match vs.into_iter().next() {
        Some(v @ Value::List(_)) => v.into_items().unwrap_or_default(),
        _ => Vec::new(),
    };
    let topics = items(host.capsae(scope, None));
    let positions = items(host.positions(scope, None));
    let rows: Vec<Vec<Value>> = topics
        .into_iter()
        .zip(positions)
        .map(|(t, p)| vec![t, p])
        .collect();
    vec![Value::table(crate::value::Table::new(
        Some(vec!["topic".to_string(), "position".to_string()]),
        rows,
    ))]
}

pub(crate) fn matrix<X: crate::thread::Attachment>(
    scope: Scope<'_, X>,
    node: bool,
    full: bool,
) -> Vec<Value> {
    let Some(peers) = scope.peers else {
        return vec![Value::Null];
    };
    let part = match (node, full) {
        (false, false) => RegRef::Record,
        (false, true) => RegRef::FullRecord,
        (true, false) => RegRef::NodeRecord,
        (true, true) => RegRef::NodeFullRecord,
    };
    let spelled = match (node, full) {
        (false, false) => "#.",
        (false, true) => "#.*",
        (true, false) => "##.",
        (true, true) => "##.*",
    };
    let mut header: Vec<String> = Vec::new();
    let mut records: Vec<Vec<(String, Value)>> = Vec::new();
    for p in &peers.caps {
        let fields = match recall(&p.register, &part) {
            Value::Record(f) => f,
            _ => Vec::new(),
        };
        for (k, _) in &fields {
            if !header.iter().any(|h| h == k) {
                header.push(k.clone());
            }
        }
        records.push(fields);
    }
    let mut rows = Vec::with_capacity(records.len());
    for fields in records {
        let mut row = Vec::with_capacity(header.len());
        for h in &header {
            let v = fields
                .iter()
                .find(|(k, _)| k == h)
                .map(|(_, v)| v.clone())
                .unwrap_or(Value::Null);
            if matches!(
                v,
                Value::List(_) | Value::Record(_) | Value::Table(_) | Value::Foreign(_)
            ) {
                record_refusal(format!(
                    "'{spelled}' holds {} under '{h}': a table cell is flat (read the regula with '$.{h}' instead)",
                    crate::stdlib::kind_of(&v)
                ));
                return vec![Value::Null];
            }
            row.push(v);
        }
        rows.push(row);
    }
    vec![Value::table(crate::value::Table::new(Some(header), rows))]
}
