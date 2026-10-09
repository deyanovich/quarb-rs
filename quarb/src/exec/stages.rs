//! Part of the executor: see `exec/mod.rs`.

use super::*;

/// One field of a record — or of every record in a list, so a
/// group's member list reads elementwise; anything
/// else is null.
/// The properties a stage projects directly on the capsa's node:
/// every `::name` operand in its expressions (a bare relative path
/// with no steps and a named projection).
fn projected_properties(stage: &Stage) -> Vec<String> {
    struct Props(Vec<String>);
    impl visit::Visitor for Props {
        fn operand(&mut self, o: &Operand) {
            if let Operand::Rel {
                steps,
                projection: Some(Projection::Property(Some(name))),
                anchor: Anchor::Current,
            } = o
                && steps.is_empty()
            {
                if !self.0.contains(name) {
                    self.0.push(name.clone());
                }
                return;
            }
            visit::walk_operand(self, o);
        }
        fn stage(&mut self, s: &Stage) {
            if !matches!(s, Stage::Subcontext { .. }) {
                visit::walk_stage(self, s);
            }
        }
    }
    let mut v = Props(Vec::new());
    v.stage(stage);
    v.0
}

/// Whether a stage's expressions read `@*` (directly or through a
/// pipe tail) — the trigger for materializing the input snapshot.
/// Subcontext and push bodies are excluded: their stages get
/// contexts of their own.
pub(super) fn stage_reads_context(stage: &Stage) -> bool {
    struct Reads(bool);
    impl visit::Visitor for Reads {
        fn operand(&mut self, o: &Operand) {
            if matches!(o, Operand::Capsae { .. } | Operand::PeerReg { .. }) {
                self.0 = true;
            } else {
                visit::walk_operand(self, o);
            }
        }
        fn stage(&mut self, s: &Stage) {
            if !matches!(s, Stage::Subcontext { .. }) {
                visit::walk_stage(self, s);
            }
        }
        fn elem(&mut self, e: &PathElem) {
            if !matches!(
                e,
                PathElem::Push {
                    body: PushBody::Query(_),
                    ..
                }
            ) {
                visit::walk_elem(self, e);
            }
        }
    }
    let mut v = Reads(false);
    v.stage(stage);
    v.0
}

pub(super) fn record_field(v: Value, name: &str) -> Value {
    match v {
        Value::Record(fields) => fields
            .into_iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v)
            .unwrap_or(Value::Null),
        Value::List(items) => Value::list(
            items
                .iter()
                .cloned()
                .map(|item| record_field(item, name))
                .collect(),
        ),
        _ => Value::Null,
    }
}

/// The field chain of a bare topic-field operand — `:n` is
/// `["n"]`, `:a:b` is `["a", "b"]` — or none when the operand
/// reads anything else.
pub(super) fn topic_field(op: &Operand) -> Option<Vec<String>> {
    fn chain(op: &Operand) -> Option<Vec<String>> {
        match op {
            Operand::Topic => Some(Vec::new()),
            Operand::Field { base, name } => {
                let mut names = chain(base)?;
                names.push(name.clone());
                Some(names)
            }
            _ => None,
        }
    }
    chain(op).filter(|names| !names.is_empty())
}

/// Turn the final capsa context into a query result. `projected` is
/// the query's static value-vs-node typing (see [`eval_query`]); it
/// decides the empty case, where no capsa is left to inspect.
pub(super) fn to_result(caps: Vec<Capsa>, projected: bool) -> QueryResult {
    if projected || caps.iter().any(|c| c.topic.is_some()) {
        QueryResult::Values(
            caps.into_iter()
                .map(|c| c.topic.unwrap_or(Value::Null))
                .collect(),
        )
    } else {
        QueryResult::Nodes(caps.into_iter().map(|c| c.node).collect())
    }
}

/// Apply one pipeline stage to the capsa context.
/// Run `stages` in order. `@| group(…)` directly followed by
/// `| count` is one fused step: the groups are tallied as they
/// form, and no group materializes its member list, its
/// provenance list, or its topic list only for `count` to take
/// the length. The result — topic, register, provenance — is the
/// one the two stages produce; a word count over a novel spends
/// its time on the words instead of on lists it then discards.
pub(super) fn apply_stages(
    stages: &[Stage],
    offset: usize,
    top: bool,
    mut caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
) -> Vec<Capsa> {
    let mut i = 0;
    while i < stages.len() {
        if top {
            with_run(|s| s.stage = (offset + i) as u32 + 1);
        }
        if let (Stage::Agg(call), Some(Stage::Func(next))) = (&stages[i], stages.get(i + 1)) {
            if call.name == "group" && next.name == "count" && next.args.is_empty() {
                caps = keyed_agg(call, caps, adapter, trace, outer, None, true);
                i += 2;
                continue;
            }
        }
        caps = apply_stage(&stages[i], caps, adapter, trace, outer);
        i += 1;
    }
    caps
}

pub(crate) fn apply_stage(
    stage: &Stage,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
) -> Vec<Capsa> {
    // The `@*` snapshot: materialized only when the stage actually
    // reads the context (the scan is cheap; the clone is not).
    let snapshot = stage_reads_context(stage).then(|| Peers::new(caps.clone()));
    let peers = snapshot.as_ref();
    // The properties this stage projects (`::x`), read on every
    // capsa's node: a remote adapter may fetch them for the whole
    // frontier first.
    let names = projected_properties(stage);
    if !names.is_empty() {
        let nodes: Vec<NodeId> = caps.iter().map(|c| c.node).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        adapter.prefetch_properties(&nodes, &names);
    }
    match stage {
        // `sh('cmd')` — the shell stage (gated: [`gate_shell`]
        // refused it long before we got here unless the adapter
        // allows it). The command — one argument, evaluated per
        // capsa, so an interpolated string parameterizes it — runs
        // under `sh -c`; the topic (as text) is its stdin, its
        // stdout (one trailing newline trimmed) the new topic; a
        // failing command yields null, propagating. stderr passes
        // through.
        // `| json` in a NODE context (no topic yet) serializes the
        // current node's subtree back to JSON — the encode half of
        // the node-mode json pair (decode(json) is the parse half).
        // With a topic present it stays the value serializer.
        // `| markdown` / `| html` / `| atrep` mirror `| json` on
        // the document side: a node context renders its subtree
        // through the koine renderer (sections, paragraphs,
        // quotes, lists, verbatim — whatever vocabulary the
        // adapter speaks); a topic value renders as text. The
        // export button and the pipe are the same verb.
        Stage::Func(call)
            if matches!(call.name.as_str(), "markdown" | "html" | "atrep" | "conllu") =>
        {
            let kind = crate::koine::Render::from_name(&call.name)
                .expect("stage names are valid render names");
            caps.into_iter()
                .map(|mut c| {
                    let v = match &c.topic {
                        Some(t) => {
                            Value::Str(crate::koine::render_values(std::slice::from_ref(t), kind))
                        }
                        None => Value::Str(crate::koine::render_node(adapter, c.node, kind)),
                    };
                    c.prov = effective_prov(&c);
                    c.topic = Some(v);
                    c
                })
                .collect()
        }
        Stage::Func(call) if call.name == "json" || call.name == "jsonl" => caps
            .into_iter()
            .map(|mut c| {
                let v = match &c.topic {
                    Some(t) => Value::Str(t.to_json()),
                    None => Value::Str(node_to_json_text(adapter, c.node)),
                };
                c.prov = effective_prov(&c);
                c.topic = Some(v);
                c
            })
            .collect(),
        // `| link`: the capsa's node as a clickable result — the
        // adapter's `::link` record (title, href, text), whatever
        // the topic was, so `::lemma | link` still links the
        // section. An adapter with no notion of a link yields null.
        Stage::Func(call) if call.name == "link" => caps
            .into_iter()
            .map(|mut c| {
                c.topic = Some(adapter.property(c.node, "link").unwrap_or(Value::Null));
                c.prov = Prov::at(c.node, stage_now());
                c
            })
            .collect(),
        // `| kaiv` mirrors `| json`: a topic value renders as a
        // one-value kaiv document; a node context renders its
        // subtree's value form. `@| kaiv` (stdlib) renders the
        // whole stream as one document.
        #[cfg(not(feature = "kaiv"))]
        Stage::Func(call) if call.name == "kaiv" => {
            record_refusal("kaiv output is not compiled in (feature kaiv)".into());
            caps
        }
        #[cfg(feature = "kaiv")]
        Stage::Func(call) if call.name == "kaiv" => caps
            .into_iter()
            .map(|mut c| {
                let doc = match &c.topic {
                    Some(t) => crate::kaiv_out::document(std::slice::from_ref(t)),
                    None => crate::kaiv_out::document(std::slice::from_ref(&node_to_json(
                        adapter, c.node,
                    ))),
                };
                c.prov = effective_prov(&c);
                c.topic = Some(Value::Str(doc.unwrap_or_else(|e| e)));
                c
            })
            .collect(),
        // `| xml` mirrors `| json`: a topic value serializes to
        // XML (record fields → elements, `@name` fields
        // attributes, `#text` the text; a list repeats an <item>);
        // a node context serializes the subtree.
        Stage::Func(call) if call.name == "xml" => caps
            .into_iter()
            .map(|mut c| {
                let xml = match &c.topic {
                    Some(t) => value_to_xml(t, "item"),
                    None => node_to_xml(adapter, c.node),
                };
                c.prov = effective_prov(&c);
                c.topic = Some(Value::Str(xml));
                c
            })
            .collect(),
        // The gate again, where the command would run: the scan
        // before evaluation is the early refusal, this is the one
        // no query shape can walk around.
        Stage::Func(call) if call.name == "sh" && !run_option(|o| o.allow_shell) => {
            record_refusal("the sh(...) stage runs external commands; pass --allow-shell".into());
            Vec::new()
        }
        Stage::Func(call) if call.name == "sh" => {
            shell_stage(call, caps, adapter, trace, outer, peers)
        }
        // `default(fallback)`: the topic when non-null, else the
        // fallback — which may be an expression evaluated against
        // the capsa's own node, making `(a) | default$$.b` the
        // coalesce idiom (chainable). Literal
        // fallbacks keep working through stdlib for adapter-free
        // callers.
        Stage::Func(call) if call.name == "default" => {
            default_stage(call, caps, adapter, trace, outer, peers)
        }
        // `record(...)` (alias `rec`) builds a record per capsa: expression fields
        // evaluate against the capsa's node, auto-named by their
        // projection or named by a preceding literal.
        Stage::Func(call) if matches!(call.name.as_str(), "record" | "rec") => caps
            .into_iter()
            .enumerate()
            .map(|(i, mut c)| {
                let scope = c.scope(Some(i + 1), peers, outer);
                let (fields, prov) = split_fields(record_fields_prov(
                    &call.args, adapter, c.node, trace, scope,
                ));
                c.topic = Some(Value::Record(fields));
                c.prov = prov;
                c
            })
            .collect(),
        // `%%(...)` — the named register view enriched with the
        // given fields: register fields first (the `%.` layout),
        // the args after in written order, an arg overriding a
        // register field that shares its name.
        Stage::RecordWith(call) => record_with_stage(call, caps, adapter, trace, outer, peers),
        // A keyed aggregate on the plain pipe works per capsa on the
        // capsa's *members* (a group's rows): reorder or filter them,
        // rebuild the list topic from the survivors. A memberless
        // capsa passes through unchanged.
        Stage::Func(call) if stdlib::known_keyed(&call.name) => caps
            .into_iter()
            .map(|mut c| {
                if c.members.is_empty() {
                    return c;
                }
                let members = keyed_agg(
                    call,
                    std::mem::take(&mut c.members),
                    adapter,
                    trace,
                    outer,
                    peers,
                    false,
                );
                c.topic = Some(Value::list(
                    members
                        .iter()
                        .map(|m| {
                            m.topic
                                .clone()
                                .unwrap_or_else(|| node_scalar(adapter, m.node))
                        })
                        .collect(),
                ));
                c.prov = Prov::list(members.iter().map(effective_prov).collect());
                c.members = members;
                c
            })
            .collect(),
        // An association aggregate on the plain pipe reads its two
        // expressions off each capsa's *members* (a group's rows) —
        // `@| group(::k) | corr(:x; :y)`. A memberless capsa has no
        // pairs to associate: null.
        Stage::Func(call) if stdlib::association(&call.name) => caps
            .into_iter()
            .map(|mut c| {
                let members = std::mem::take(&mut c.members);
                c.topic = Some(if members.is_empty() {
                    Value::Null
                } else {
                    associate_caps(call, &members, adapter, trace, outer, peers)
                });
                c.prov = c.prov.flat();
                c
            })
            .collect(),
        // A reducing aggregate on the plain pipe reduces each capsa's
        // *list* topic (a group's members) — per-capsa work, so it
        // rides `|`. A non-list topic reduces as a singleton. The
        // reduction consumes the members: they no longer correspond
        // to the topic.
        // (`number` is both: `@| number` numbers an outline, `| number(LOCALE)`
        // reads text as a number under a locale's conventions.)
        Stage::Func(call) if stdlib::known_agg(&call.name) && call.name != "number" => {
            agg_as_func_stage(call, caps, adapter)
        }
        // Per-capsa scalar transform; a function may expand one topic
        // into several (e.g. `lines`), forking the capsa.
        Stage::Func(call) => func_stage(call, caps, adapter, trace, outer, peers),
        // The map pipe: the inner stage runs once per element of
        // the topic, per capsa — capsae unchanged, the list
        // reassembled (an expanding inner stage flattens). Inside,
        // `$_` is the element and `@*` reads the element-context.
        // `.v(body){N}` — the iteration loop: the push
        // applied round after round, every round a stage of its own
        // (its own snapshot, so each capsa reads the previous
        // round's values); exactly N rounds for `{N}`, else at least
        // `min` and on until a round repoints no name and changes
        // no topic, or `max` (the quantifier bound when open).
        Stage::Repeat {
            stage: inner,
            min,
            max,
        } => repeat_stage(inner, min, max, caps, adapter, trace, outer),
        Stage::Map(inner) => map_stage(inner, caps, adapter, trace, outer),
        // The spread: fork a list topic into one thread per
        // element; null forks to nothing, a non-list scalar passes
        // through, a record passes whole. The outer form `...?`
        // differs in one case: where the plain spread would fork
        // to nothing, it emits ONE thread with a null topic — the
        // row survives its empty side (OPTIONAL MATCH).
        Stage::Spread { outer } => caps
            .into_iter()
            .flat_map(|c| {
                let mut values = match c.topic.clone() {
                    Some(l @ Value::List(_)) => l.into_items().unwrap_or_default(),
                    Some(Value::Null) | None => Vec::new(),
                    Some(other) => vec![other],
                };
                let was_list = matches!(c.topic, Some(Value::List(_)));
                if *outer && values.is_empty() {
                    values.push(Value::Null);
                }
                let provs: Vec<Prov> = (0..values.len())
                    .map(|i| {
                        if was_list {
                            c.prov.item(i)
                        } else {
                            c.prov.clone()
                        }
                    })
                    .collect();
                values.into_iter().zip(provs).map(move |(v, prov)| Capsa {
                    prov,
                    node: c.node,
                    register: c.register.clone(),
                    topic: Some(v),
                    members: Vec::new(),
                    captures: c.captures.clone(),
                    named: c.named.clone(),
                    marks: c.marks.clone(),
                    bindings: c.bindings.clone(),
                    arrived: c.arrived.clone(),
                })
            })
            .collect(),
        // A navigation stage: resume navigation from each capsa's
        // node (or from `^` / its `(name)` mark), registers and
        // marks carried forward — the pipeline spelling of a path
        // continuation. The parser admits it only in navigation
        // mode, so no live topic is dropped here. Steps behave
        // exactly as in the path: same predicate scope law (no
        // capsa of their own), same group/breadcrumb machinery,
        // same (node, register) dedup as a branch union.
        Stage::Nav(branch) => nav_stage(branch, caps, adapter, trace, outer, peers),
        // Push the topic onto the register — or, in a node context
        // (no topic), MARK the node: the shared push spelling is
        // context-typed. A scalar goes to the register, a node to
        // the mark array (anonymously when no name is given — the
        // slot stays `(N)`-addressable); the two never mix.
        Stage::Push(name) => caps
            .into_iter()
            .map(|mut c| {
                match c.topic.clone() {
                    None => c.marks.push(Mark {
                        name: name.clone(),
                        node: c.node,
                    }),
                    Some(topic) => c.register.push(Reg {
                        prov: c.prov.clone(),
                        site: Origin {
                            node: c.node,
                            stage: stage_now(),
                        },
                        name: name.clone(),
                        value: topic,
                    }),
                }
                c
            })
            .collect(),
        // Evaluate the sub-expression from each node, reduce to a
        // scalar, set it as the topic, and push it.
        Stage::Subcontext { name, body } => {
            subcontext_stage(name, body, caps, adapter, trace, outer, peers)
        }
        // A value expression against the capsa's node becomes the
        // topic.
        Stage::Expr(expr) => expr_stage(expr, caps, adapter, trace, outer, peers),
        // ... and a pushed one is a computed column.
        Stage::ExprPush { name, expr } => {
            expr_push_stage(name, expr, caps, adapter, trace, outer, peers)
        }
        // `| .%` — the fields push: each field of the topic record
        // becomes a named regula; the topic stays the record.
        Stage::FieldsPush => caps
            .into_iter()
            .map(|mut c| {
                if let Some(Value::Record(fields)) = &c.topic {
                    for (k, v) in fields.clone() {
                        c.register.push(Reg {
                            prov: c.prov.part(&k),
                            site: Origin {
                                node: c.node,
                                stage: stage_now(),
                            },
                            name: Some(k),
                            value: v,
                        });
                    }
                }
                c
            })
            .collect(),
        // `.%(...)` / `.name%(...)` — build the record and push it,
        // the topic set to the record (`.%%(...)`: the enriched
        // register view, as `%%(...)` renders it).
        Stage::RecordPush {
            name,
            call,
            enriched,
        } => record_push_stage(name, call, enriched, caps, adapter, trace, outer, peers),
        // `@| ungroup` flattens groups back to their member capsae,
        // each inheriting the group's key registers. A memberless
        // capsa passes through unchanged.
        Stage::Agg(call) if call.name == "ungroup" => caps
            .into_iter()
            .flat_map(|c| {
                if c.members.is_empty() {
                    return vec![c];
                }
                let key_regs = c.register;
                c.members
                    .into_iter()
                    .map(|mut m| {
                        m.register.extend(key_regs.iter().cloned());
                        m
                    })
                    .collect()
            })
            .collect(),
        // `@| window(span[, key])` — each capsa gains its context
        // neighbors at ordinal offsets within the span as members
        // (partial near the edges), with the list of their effective
        // topics as its topic: the group shape, so the per-capsa
        // aggregate machinery rolls for free. With a partition key,
        // neighbors are the nearest capsae whose key compares equal,
        // original order preserved.
        Stage::Agg(call) if call.name == "window" => {
            window_stage(call, caps, adapter, trace, outer, peers)
        }
        // `@| shift(n[, key])` — each capsa's topic becomes the
        // effective topic of the capsa `n` positions back (negative
        // looks forward; null where none exists). Registers and
        // captures stay the capsa's own.
        Stage::Agg(call) if call.name == "shift" => {
            shift_stage(call, caps, adapter, trace, outer, peers)
        }
        // `@| zscore([sample][, key])` — each capsa's topic becomes
        // its standard score over the context, or over its
        // partition. Row-aligned like `shift`.
        Stage::Agg(call) if call.name == "zscore" => {
            zscore_stage(call, caps, adapter, trace, outer, peers)
        }
        // `@| number[(level)]` and `@| outline[(level; text[; "1."])]`
        // — each capsa's topic becomes its dotted number in the
        // stream of levels, or its line of the outline. Row-aligned
        // like `shift`.
        Stage::Agg(call) if matches!(call.name.as_str(), "number" | "outline") => {
            outline_stage(call, caps, adapter, trace, outer, peers)
        }
        // `@| timeline(EXPR[; ANCHOR][; forward])` — the narrative
        // point carried from capsa to capsa. Row-aligned like
        // `shift`.
        Stage::Agg(call) if call.name == "timeline" => {
            timeline_stage(call, caps, adapter, trace, outer, peers)
        }
        // `@| corr(:a; :b)` and its kin reduce two per-capsa
        // expressions to one number over the whole context.
        Stage::Agg(call) if stdlib::association(&call.name) => {
            association_stage(call, caps, adapter, trace, outer, peers)
        }
        // Keyed aggregates reorder or filter the capsae themselves,
        // preserving nodes, registers, and topics.
        Stage::Agg(call) if stdlib::known_keyed(&call.name) => {
            keyed_agg(call, caps, adapter, trace, outer, peers, false)
        }
        // The order/selection family is capsa-preserving too: it
        // reorders or picks, never reduces, so nodes and registers
        // ride along (making `@| sort_by(…) @| reverse | [..n]`
        // compose). The effective topic is baked in, keeping value
        // outputs identical to a plain reduction's.
        Stage::Agg(call)
            if matches!(
                call.name.as_str(),
                "sort" | "unique" | "reverse" | "first" | "last"
            ) =>
        {
            order_stage(call, caps, adapter)
        }
        // Positional selection from the whole context, with the same
        // numbering as the navigation-side predicates.
        Stage::Select(pred @ (Predicate::Index(_) | Predicate::Range(_, _))) => {
            positional(caps, pred)
        }
        // An expression predicate in select position — reachable only
        // through an inline aggregate pipe (`(... @| [cond] ...)`),
        // which unlike the top-level `@|` does not reject it at parse
        // time — filters per capsa exactly as `| [cond]` does, never a
        // silent pass-through of every item.
        Stage::Select(Predicate::Expr(e)) => {
            apply_stage(&Stage::Filter(e.clone()), caps, adapter, trace, outer)
        }
        // Per-capsa filter: each capsa is kept iff the condition
        // holds against its node.
        Stage::Filter(cond) => filter_stage(cond, caps, adapter, trace, outer, peers),
        // Reduce the whole context's topics to a new context.
        Stage::Agg(call) => agg_stage(call, caps, adapter),
        // Recall a register value into the topic.
        Stage::Recall(r) => caps
            .into_iter()
            .map(|mut c| {
                c.prov = recall_prov(&c.register, r);
                c.topic = Some(recall(&c.register, r));
                c
            })
            .collect(),
    }
}

/// `default(x)` — the topic, or `x` where it is null. `apply_stage`'s arm.
pub(super) fn default_stage(
    call: &FnCall,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Vec<Capsa> {
    caps.into_iter()
        .map(|mut c| {
            let topic = c.topic.take().unwrap_or(Value::Null);
            let taken = matches!(topic, Value::Null);
            let (v, o) = capture(|| {
                if taken {
                    match call.args.first() {
                        Some(Arg::Lit(v)) => v.clone(),
                        Some(Arg::Expr(e)) => operand_scalar(
                            adapter,
                            c.node,
                            e,
                            trace,
                            Scope {
                                prov: None,
                                node: Some(c.node),
                                register: &c.register,
                                members: &c.members,
                                topic: None,
                                ordinal: None,
                                captures: &c.captures,
                                named: &c.named,
                                marks: &c.marks,
                                bindings: &c.bindings,
                                edge: None,
                                arrived: &c.arrived,
                                peers,
                                outer,
                            },
                        ),
                        _ => Value::Null,
                    }
                } else {
                    topic
                }
            });
            c.prov = if taken { Prov::leaf(o) } else { c.prov.flat() };
            c.topic = Some(v);
            c
        })
        .collect()
}

/// `%(...)` — the record built from the topic. `apply_stage`'s arm.
pub(super) fn record_with_stage(
    call: &FnCall,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Vec<Capsa> {
    caps.into_iter()
        .enumerate()
        .map(|(i, mut c)| {
            let scope = c.scope(Some(i + 1), peers, outer);
            let mut fields = named_register_fields(&c.register);
            let mut parts = named_register_parts(&c.register);
            for (name, value, p) in record_fields_prov(&call.args, adapter, c.node, trace, scope) {
                match fields.iter_mut().find(|(n, _)| n == &name) {
                    Some((_, v)) => *v = value,
                    None => fields.push((name.clone(), value)),
                }
                match parts.iter_mut().find(|(n, _)| n == &name) {
                    Some((_, q)) => *q = p,
                    None => parts.push((name, p)),
                }
            }
            c.topic = Some(Value::Record(fields));
            c.prov = Prov::record(parts);
            c
        })
        .collect()
}

/// An aggregate name used as a per-capsa function over a list topic. `apply_stage`'s arm.
pub(super) fn agg_as_func_stage(
    call: &FnCall,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
) -> Vec<Capsa> {
    caps.into_iter()
        .map(|mut c| {
            let mut items = match c.topic.take() {
                Some(l @ Value::List(_)) => l.into_items().unwrap_or_default(),
                Some(other) => vec![other],
                None => vec![node_scalar(adapter, c.node)],
            };
            // `sum(:n)` over a group of records: a
            // leading topic-field argument projects the members
            // before the reduction — `top(3; :n)`'s spelling on
            // the reducing side.
            let projected;
            let call = match call.args.first() {
                Some(Arg::Expr(op)) if topic_field(op).is_some() => {
                    let names = topic_field(op).expect("checked");
                    items = items
                        .into_iter()
                        .map(|v| names.iter().fold(v, |v, n| record_field(v, n)))
                        .collect();
                    projected = FnCall {
                        name: call.name.clone(),
                        args: call.args[1..].to_vec(),
                    };
                    &projected
                }
                _ => call,
            };
            let mut out = stdlib::apply(call, items, &|e| adapter.unit_scale(e));
            c.topic = Some(match out.len() {
                1 => out.pop().expect("len checked"),
                _ => Value::list(out),
            });
            c.prov = c.prov.flat();
            c.members = Vec::new();
            c
        })
        .collect()
}

/// `| repeat(...)` — the iteration stage. `apply_stage`'s arm.
pub(super) fn repeat_stage(
    inner: &Stage,
    min: &usize,
    max: &Option<usize>,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
) -> Vec<Capsa> {
    let mut caps = caps;
    // The bound holds here as in a path quantifier: an
    // explicit ceiling or minimum above it is refused, an
    // open loop runs to it.
    let n_max = quantifier_bound(adapter);
    if let Some(n) = max
        && *n > n_max
    {
        record_refusal(format!(
            "the loop's explicit ceiling ({n}) exceeds the quantifier \
                 bound ({n_max}); raise --quantifier-bound"
        ));
        return Vec::new();
    }
    if *min > n_max {
        record_refusal(format!(
            "the loop's minimum ({min}) exceeds the quantifier bound \
                 ({n_max}); raise --quantifier-bound"
        ));
        return Vec::new();
    }
    let limit = max.unwrap_or(n_max);
    let exact = *max == Some(*min);
    let signature = |caps: &[Capsa]| -> Vec<RoundState> {
        caps.iter()
            .map(|c| (c.node, c.topic.clone(), named_register_fields(&c.register)))
            .collect()
    };
    let mut round = 0;
    while round < limit {
        let before = if exact || round < *min {
            None
        } else {
            Some(signature(&caps))
        };
        caps = apply_stage(inner, caps, adapter, trace, outer);
        round += 1;
        if let Some(b) = before
            && b == signature(&caps)
        {
            break;
        }
    }
    caps
}

/// `| (body)` — a subcontext run per capsa. `apply_stage`'s arm.
pub(super) fn subcontext_stage(
    name: &Option<String>,
    body: &Query,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Vec<Capsa> {
    caps.into_iter()
        .enumerate()
        .map(|(i, mut c)| {
            // The body sees this capsa as its enclosing scope:
            // `$.name` / `$$_` / `$$ord` reach it (correlated
            // subqueries).
            let scope = c.scope(Some(i + 1), peers, outer);
            let (value, prov) = subcontext_traced(body, adapter, c.node, trace, Some(&scope));
            c.register.push(Reg {
                prov: prov.clone(),
                site: Origin {
                    node: c.node,
                    stage: stage_now(),
                },
                name: name.clone(),
                value: value.clone(),
            });
            c.prov = prov;
            // Visible as the topic; the push still returns
            // the thread to navigation mode (see ExprPush).
            c.topic = Some(value);
            c
        })
        .collect()
}

/// `| expr` — an expression over the topic. `apply_stage`'s arm.
pub(super) fn expr_stage(
    expr: &Operand,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Vec<Capsa> {
    caps.into_iter()
        .enumerate()
        .flat_map(|(i, mut c)| {
            let scope = c.scope(Some(i + 1), peers, outer);
            // An operand that yields several values is a hop in
            // value position: each value is its own row, as each
            // target of a navigation is.
            if c.members.is_empty() {
                let (values, o) =
                    capture(|| eval_operand(adapter, c.node, expr, trace, &[], scope));
                if values.len() > 1 {
                    return values
                        .into_iter()
                        .map(|v| {
                            let mut k = c.clone();
                            k.topic = Some(v);
                            k.prov = Prov::leaf(o.clone());
                            k
                        })
                        .collect::<Vec<_>>();
                }
            }
            let (value, o) = capture(|| group_value(adapter, c.node, expr, trace, scope));
            c.topic = Some(value);
            c.prov = Prov::leaf(o);
            vec![c]
        })
        .collect()
}

/// `.name(expr)` — push an expression's value. `apply_stage`'s arm.
pub(super) fn expr_push_stage(
    name: &Option<String>,
    expr: &Operand,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Vec<Capsa> {
    caps.into_iter()
        .enumerate()
        .map(|(i, mut c)| {
            let scope = c.scope(Some(i + 1), peers, outer);
            let (value, o) = capture(|| operand_value(adapter, c.node, expr, trace, &[], scope));
            c.prov = Prov::leaf(o);
            c.register.push(Reg {
                prov: c.prov.clone(),
                site: Origin {
                    node: c.node,
                    stage: stage_now(),
                },
                name: name.clone(),
                value: value.clone(),
            });
            // The pushed value stays visible as the topic —
            // but the PUSH is what returns the thread to
            // navigation mode: a following hop is legal and
            // rebuilds the capsae, the filed value safe in
            // the register (spec: Execution Modes).
            c.topic = Some(value);
            c
        })
        .collect()
}

/// `.%(...)` — build the record and push it. `apply_stage`'s arm.
pub(super) fn record_push_stage(
    name: &Option<String>,
    call: &FnCall,
    enriched: &bool,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Vec<Capsa> {
    caps.into_iter()
        .enumerate()
        .map(|(i, mut c)| {
            let scope = c.scope(Some(i + 1), peers, outer);
            let (fields, prov) = if *enriched {
                let mut fields = named_register_fields(&c.register);
                let mut parts = named_register_parts(&c.register);
                for (name, value, p) in
                    record_fields_prov(&call.args, adapter, c.node, trace, scope)
                {
                    match fields.iter_mut().find(|(n, _)| n == &name) {
                        Some((_, v)) => *v = value,
                        None => fields.push((name.clone(), value)),
                    }
                    match parts.iter_mut().find(|(n, _)| n == &name) {
                        Some((_, q)) => *q = p,
                        None => parts.push((name, p)),
                    }
                }
                (fields, Prov::record(parts))
            } else {
                split_fields(record_fields_prov(
                    &call.args, adapter, c.node, trace, scope,
                ))
            };
            let value = Value::Record(fields);
            c.register.push(Reg {
                prov: prov.clone(),
                site: Origin {
                    node: c.node,
                    stage: stage_now(),
                },
                name: name.clone(),
                value: value.clone(),
            });
            c.prov = prov;
            c.topic = Some(value);
            c
        })
        .collect()
}

/// The order and selection family (`sort_by`, `reverse`, `first`, …): capsa-preserving, the effective topic baked in. `apply_stage`'s arm.
pub(super) fn order_stage(
    call: &FnCall,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
) -> Vec<Capsa> {
    // Which capsae are still nodes (no topic yet): `unique`
    // tells nodes apart by identity, values by value.
    let is_node: Vec<bool> = caps.iter().map(|c| c.topic.is_none()).collect();
    let mut caps: Vec<Capsa> = caps
        .into_iter()
        .map(|mut c| {
            if c.topic.is_none() {
                c.prov = Prov::at(c.node, stage_now());
            }
            c.topic = Some(
                c.topic
                    .take()
                    .unwrap_or_else(|| node_scalar(adapter, c.node)),
            );
            c
        })
        .collect();
    let topic_of = |c: &Capsa| c.topic.clone().unwrap_or(Value::Null);
    match call.name.as_str() {
        // With a locale argument, topics sort by their text
        // under that locale's collation (colligo; codepoint
        // fallback when the feature is compiled out); with
        // `atergo`, in the a tergo order.
        "sort" => match stdlib::text_order(call) {
            Some(cmp) => {
                caps.sort_by(|a, b| cmp(&topic_of(a).to_string(), &topic_of(b).to_string()))
            }
            None => {
                let collate = stdlib::default_text_order();
                let scale = |e: &str| adapter.unit_scale(e);
                caps.sort_by(|a, b| {
                    stdlib::default_cmp(&topic_of(a), &topic_of(b), &scale, collate.as_deref())
                })
            }
        },
        "reverse" => caps.reverse(),
        "unique" => {
            // A dropped duplicate hands its origins to the
            // survivor: the value was read there too.
            let mut kept: Vec<Capsa> = Vec::new();
            let mut index: HashMap<String, usize> = HashMap::new();
            // A node is itself, whatever it reads as: two
            // characters with the same (or no) text are two.
            // A value is one with every equal value.
            for (c, node) in caps.drain(..).zip(is_node.iter()) {
                let key = if *node {
                    format!("\u{0}node:{}", c.node.0)
                } else {
                    topic_of(&c).to_string()
                };
                match index.get(&key) {
                    Some(&i) => kept[i].prov.origins.union(&c.prov.origins),
                    None => {
                        index.insert(key, kept.len());
                        kept.push(c);
                    }
                }
            }
            caps = kept;
        }
        "first" => caps.truncate(1),
        "last" => {
            let n = caps.len().saturating_sub(1);
            caps.drain(..n);
        }
        _ => {}
    }
    caps
}

/// `sh('cmd')` — the shell stage, one command per capsa with the topic as stdin: `apply_stage`'s shell arm.
pub(super) fn shell_stage(
    call: &FnCall,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Vec<Capsa> {
    caps.into_iter()
        .enumerate()
        .map(|(i, mut c)| {
            let scope = c.scope(Some(i + 1), peers, outer);
            let (cmd, o) = capture(|| {
                call.args.first().map(|a| match a {
                    Arg::Lit(v) => v.to_string(),
                    Arg::Expr(e) => operand_scalar(adapter, c.node, e, trace, scope).to_string(),
                    Arg::Range(..) | Arg::Named { .. } => String::new(),
                })
            });
            c.topic = Some(match cmd {
                Some(cmd) if !cmd.is_empty() => {
                    run_shell(&cmd, c.topic.as_ref()).unwrap_or(Value::Null)
                }
                _ => Value::Null,
            });
            let mut prov = c.prov.flat();
            prov.origins.union(&o);
            c.prov = prov;
            c
        })
        .collect()
}

/// `| f(...)` — a per-capsa function on the topic: `apply_stage`'s function arm.
pub(super) fn func_stage(
    call: &FnCall,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Vec<Capsa> {
    caps.into_iter()
        .flat_map(|c| {
            let topic = c.topic.clone().unwrap_or(Value::Null);
            let prov = c.prov.flat();
            // Expression arguments evaluate per capsa, against
            // its node and register (the spec's Expression
            // Arguments): `levenshtein(::alt)`, `lpad(::width)`.
            let resolved;
            let call = if call.args.iter().any(|a| matches!(a, Arg::Expr(_))) {
                resolved = FnCall {
                    name: call.name.clone(),
                    args: call
                        .args
                        .iter()
                        .map(|a| match a {
                            Arg::Expr(e) => Arg::Lit(operand_scalar(
                                adapter,
                                c.node,
                                e,
                                trace,
                                c.scope(None, peers, outer),
                            )),
                            other => other.clone(),
                        })
                        .collect(),
                };
                &resolved
            } else {
                call
            };
            stdlib::apply_scalar(call, topic, &|e| adapter.unit_scale(e))
                .into_iter()
                .map(move |v| Capsa {
                    prov: prov.clone(),
                    node: c.node,
                    register: c.register.clone(),
                    topic: Some(v),
                    members: Vec::new(),
                    captures: c.captures.clone(),
                    named: c.named.clone(),
                    marks: c.marks.clone(),
                    bindings: c.bindings.clone(),
                    arrived: c.arrived.clone(),
                })
        })
        .collect()
}

/// `| f -> inner` — the mapped stage over each member: `apply_stage`'s map arm.
pub(super) fn map_stage(
    inner: &Stage,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
) -> Vec<Capsa> {
    caps.into_iter()
        .map(|mut c| {
            let (items, was_list) = match c.topic.take() {
                Some(l @ Value::List(_)) => (l.into_items().unwrap_or_default(), true),
                Some(Value::Null) | None => {
                    // Null maps to null: nothing to map over.
                    c.topic = Some(Value::Null);
                    return c;
                }
                Some(other) => (vec![other], false),
            };
            let provs: Vec<Prov> = (0..items.len())
                .map(|i| {
                    if was_list {
                        c.prov.item(i)
                    } else {
                        c.prov.clone()
                    }
                })
                .collect();
            let pseudo: Vec<Capsa> = items
                .into_iter()
                .zip(provs)
                .map(|(v, prov)| Capsa {
                    prov,
                    node: c.node,
                    register: c.register.clone(),
                    topic: Some(v),
                    members: Vec::new(),
                    captures: c.captures.clone(),
                    named: c.named.clone(),
                    marks: c.marks.clone(),
                    bindings: c.bindings.clone(),
                    arrived: c.arrived.clone(),
                })
                .collect();
            let (mut out, mut provs): (Vec<Value>, Vec<Prov>) =
                apply_stage(inner, pseudo, adapter, trace, outer)
                    .into_iter()
                    .map(|p| (p.topic.unwrap_or(Value::Null), p.prov))
                    .unzip();
            if was_list || out.len() != 1 {
                c.prov = Prov::list(provs);
                c.topic = Some(Value::list(out));
            } else {
                c.prov = provs.pop().unwrap_or_default();
                c.topic = Some(out.pop().expect("len checked"));
            }
            c
        })
        .collect()
}

/// `| hop` — a navigation stage from each capsa's pockets: `apply_stage`'s nav arm.
pub(super) fn nav_stage(
    branch: &Branch,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Vec<Capsa> {
    let mut out = Vec::new();
    for c in &caps {
        // The thread's own pockets; an unset anchor yields
        // nothing, like every anchor miss. `(@)` re-seeds
        // the thread from every pocket — one fork per
        // marked node, registers and marks carried.
        let anchor_scope = c.scope(None, peers, outer);
        for from in anchor_nodes(adapter, &branch.anchor, c.node, trace, &[], anchor_scope) {
            for (node, register, marks, arrived) in navigate_paths(
                &branch.steps,
                adapter,
                from,
                trace,
                outer,
                // The thread's witness travels with the hop, so
                // step predicates on the way read `$*k` per
                // thread (the results carry it on, below).
                &c.bindings,
                &c.marks,
                &c.register,
            ) {
                out.push(Capsa {
                    prov: if branch.projection.is_some() {
                        Prov::at(node, stage_now())
                    } else {
                        Prov::default()
                    },
                    node,
                    register,
                    marks,
                    arrived,
                    topic: branch
                        .projection
                        .as_ref()
                        .map(|p| project(adapter, node, p)),
                    members: Vec::new(),
                    captures: c.captures.clone(),
                    named: c.named.clone(),
                    bindings: c.bindings.clone(),
                });
            }
        }
    }
    dedup_merge(out)
}

/// `| [cond]` as a stage — `apply_stage`'s filter arm.
pub(super) fn filter_stage(
    cond: &PredExpr,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Vec<Capsa> {
    caps.into_iter()
        .enumerate()
        .filter_map(|(i, mut c)| {
            let scope = c.scope(Some(i + 1), peers, outer);
            // A capsa that carries a correlation witness is
            // tested UNDER it — the pipeline filter is the
            // WHERE clause, null-propagating over a null
            // witness slot (so `[not $*1::x]` is the anti-
            // join), never a re-search of the trace. Without
            // a witness, the existential runs as in
            // navigation position.
            let hit = if c.bindings.is_empty() {
                exists_binding(adapter, c.node, &[cond], trace, &mut Vec::new(), scope)
            } else {
                eval_pred_expr(adapter, c.node, cond, trace, &c.bindings, scope)
            };
            if !hit {
                return None;
            }
            // A successful `=~` in the filter binds its groups
            // for later stages ($1 …); a filter without one
            // leaves earlier captures intact. A NAMED group is
            // a named push: `(?<year>…)` files its
            // text into the register under `year`, read back as
            // `$.year` / `(.year)` and seen by `%.`.
            // A named group is a capture, not a push: `%+:year`
            // is the match's, `$.year` the capsa's — filing is
            // explicit (`| .year(%+:year)`, or `| %+ | .%`).
            let groups = extract_captures(cond, adapter, c.node, trace, scope);
            if let Some((groups, named)) = groups {
                c.captures = groups;
                c.named = named;
            }
            Some(c)
        })
        .collect()
}

/// `@| f(...)` — an aggregate over the whole context (or each partition): `apply_stage`'s aggregate arm.
pub(super) fn agg_stage(call: &FnCall, caps: Vec<Capsa>, adapter: &impl AstAdapter) -> Vec<Capsa> {
    // The output boundary is the tool's: `qua`, `quai` and the
    // session take `@| encoding(LABEL)` off the end of a query
    // and encode what they print. Reaching the executor means
    // it stood somewhere else, or the host does not encode.
    if call.name == "encoding" {
        record_refusal(
            "the encoding stage stands last: '… @| csv @| encoding(windows-1251)'; \
                 the tool that prints the output applies it"
                .into(),
        );
        return Vec::new();
    }
    // `@| csv(title; year)` / `@| table(…)` over nodes: the
    // named columns are the nodes' properties, as they are a
    // record's fields — a node stands for the record of the
    // columns asked of it.
    let columns: Vec<&str> = if matches!(call.name.as_str(), "csv" | "table")
        || call.name == stdlib::ROWS_FOR_RENDERING
    {
        stdlib::split_csv_delimiter(call)
            .1
            .iter()
            .filter_map(|a| match a {
                Arg::Lit(Value::Str(s)) => Some(s.as_str()),
                _ => None,
            })
            .collect()
    } else {
        Vec::new()
    };
    let topics: Vec<Value> = caps
        .iter()
        .map(|c| match &c.topic {
            Some(t) => t.clone(),
            None if !columns.is_empty() => Value::Record(
                columns
                    .iter()
                    .map(|col| {
                        (
                            col.to_string(),
                            adapter.property(c.node, col).unwrap_or(Value::Null),
                        )
                    })
                    .collect(),
            ),
            None => node_scalar(adapter, c.node),
        })
        .collect();
    // The combinations family combines values; a node
    // stands in a reduction by its name. Nodes that share one
    // name (a table's rows, a list's same-named fields) would
    // combine into a silent nothing, so the stage says what
    // to write instead.
    if matches!(call.name.as_str(), "combinations" | "pairs" | "triples")
        && caps.len() > 1
        && caps.iter().all(|c| c.topic.is_none())
        && topics
            .iter()
            .filter(|t| !matches!(t, Value::Null))
            .map(|t| t.to_string())
            .collect::<std::collections::HashSet<_>>()
            .len()
            <= 1
    {
        record_refusal(format!(
            "'{0}' combines values, and these nodes have none to tell \
                 them apart (a node stands by its name): project a value \
                 first — '| ::title @| {0}'",
            call.name
        ));
        return Vec::new();
    }
    // The reduction's origins: every reduced value's, bounded.
    let mut origins = Origins::default();
    for c in &caps {
        match &c.topic {
            Some(_) => origins.union(&c.prov.origins),
            None => origins.insert(Origin {
                node: c.node,
                stage: stage_now(),
            }),
        }
    }
    let node = caps.first().map_or(adapter.root(), |c| c.node);
    // A reduction that hands back one of its inputs (max,
    // min, first, last, unique, sort, reverse) keeps that
    // input's origins, so `((!$_:::origin))` after `@| max`
    // lands on the winner; a computed reduction (sum, count,
    // avg) keeps the union.
    let selects = matches!(
        call.name.as_str(),
        "max" | "min" | "first" | "last" | "unique" | "sort" | "reverse"
    );
    let per_input: Vec<Origins> = if selects {
        caps.iter()
            .map(|c| match &c.topic {
                Some(_) => c.prov.origins.clone(),
                None => {
                    let mut o = Origins::default();
                    o.insert(Origin {
                        node: c.node,
                        stage: stage_now(),
                    });
                    o
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    let inputs = if selects { topics.clone() } else { Vec::new() };
    stdlib::apply(call, topics, &|e| adapter.unit_scale(e))
        .into_iter()
        .map(|v| Capsa {
            prov: Prov::leaf(
                inputs
                    .iter()
                    .position(|t| *t == v)
                    .map(|i| per_input[i].clone())
                    .unwrap_or_else(|| origins.clone()),
            ),
            node,
            register: Vec::new(),
            topic: Some(v),
            members: Vec::new(),
            captures: Vec::new(),
            named: Vec::new(),
            marks: Vec::new(),
            bindings: Vec::new(),
            arrived: Vec::new(),
        })
        .collect()
}

/// Apply a keyed aggregate: reorder or filter the capsae by keys
/// evaluated per capsa against its node, preserving nodes,
/// registers, and topics. Sorting is stable; a composite key
/// compares lexicographically; a null key sorts like empty text
/// (consistent with `Value::compare` everywhere else).
/// `counting`: the fused `group | count` (see [`apply_stages`]) —
/// each group's topic is its member count, its provenance the
/// members' origins, and no member is kept.
pub(super) fn keyed_agg(
    call: &FnCall,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
    counting: bool,
) -> Vec<Capsa> {
    // `top` / `bottom` carry their count as a leading literal.
    let (count, key_args): (Option<usize>, &[Arg]) = match call.args.split_first() {
        Some((Arg::Lit(Value::Int(n)), rest)) if matches!(call.name.as_str(), "top" | "bottom") => {
            (Some((*n).max(0) as usize), rest)
        }
        _ => (None, &call.args),
    };
    let key_of = |c: &Capsa, i: usize| -> Vec<Value> {
        key_args
            .iter()
            .filter_map(|a| match a {
                Arg::Expr(e) => Some(operand_scalar(
                    adapter,
                    c.node,
                    e,
                    trace,
                    c.scope(Some(i + 1), peers, outer),
                )),
                Arg::Lit(_) | Arg::Range(_, _) | Arg::Named { .. } => None,
            })
            .collect()
    };
    let compare_keys = |a: &[Value], b: &[Value]| -> Ordering {
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| x.compare_with(y, &|e| adapter.unit_scale(e)))
            .find(|o| *o != Ordering::Equal)
            .unwrap_or(Ordering::Equal)
    };

    // A missing key does not compete — the ordering counterpart of
    // the numeric aggregates skipping non-numeric values. Extremes
    // and top/bottom exclude null-keyed capsae; sort_by keeps them,
    // last; unique_by treats null as its own group.
    let has_null = |k: &[Value]| k.iter().any(|v| matches!(v, Value::Null));

    // `group` partitions instead of reordering: one capsa per
    // distinct key (first-appearance order; null keys grouped out,
    // pandas' dropna), topic = the list of member topics, the key
    // fields pushed as named regs for later recall ($.city).
    if call.name == "group" {
        // (key, key fields, members) per group; when counting, the
        // members stay empty and `tally` holds (count, origins,
        // first node) instead.
        type Group = (Vec<Value>, Vec<(String, Value, Prov)>, Vec<Capsa>);
        let mut groups: Vec<Group> = Vec::new();
        let mut tally: Vec<(i64, Origins, NodeId)> = Vec::new();
        let member_origins = |c: &Capsa| match &c.topic {
            Some(_) => c.prov.origins.clone(),
            None => Origins::at(c.node, stage_now()),
        };
        // Key → position in `groups`: one probe per capsa, where a
        // scan over the groups formed so far made a word count over
        // a novel quadratic in its vocabulary.
        let mut index: HashMap<Vec<SortKey>, usize> = HashMap::new();
        for (i, c) in caps.into_iter().enumerate() {
            let scope = c.scope(Some(i + 1), peers, outer);
            let fields = record_fields_prov(&call.args, adapter, c.node, trace, scope);
            let key: Vec<Value> = fields.iter().map(|(_, v, _)| v.clone()).collect();
            if key.iter().any(|v| matches!(v, Value::Null)) {
                continue;
            }
            let hashed: Vec<SortKey> = key
                .iter()
                .map(|v| v.sort_key_with(&|e| adapter.unit_scale(e)))
                .collect();
            match index.get(&hashed) {
                Some(&at) => {
                    let (_, kf, members) = &mut groups[at];
                    // The key was read from this member too.
                    for ((_, _, p), (_, _, q)) in kf.iter_mut().zip(&fields) {
                        p.origins.union(&q.origins);
                    }
                    if counting {
                        let (n, acc, _) = &mut tally[at];
                        *n += 1;
                        acc.union(&member_origins(&c));
                    } else {
                        members.push(c)
                    }
                }
                None => {
                    index.insert(hashed, groups.len());
                    if counting {
                        tally.push((1, member_origins(&c), c.node));
                        groups.push((key, fields, Vec::new()))
                    } else {
                        groups.push((key, fields, vec![c]))
                    }
                }
            }
        }
        if counting {
            return groups
                .into_iter()
                .zip(tally)
                .map(|((_, fields, _), (n, origins, node))| {
                    let register = fields
                        .into_iter()
                        .map(|(name, value, prov)| Reg {
                            prov,
                            site: Origin {
                                node,
                                stage: stage_now(),
                            },
                            name: Some(name),
                            value,
                        })
                        .collect();
                    Capsa {
                        prov: Prov::leaf(origins),
                        node,
                        register,
                        topic: Some(Value::Int(n)),
                        members: Vec::new(),
                        captures: Vec::new(),
                        named: Vec::new(),
                        marks: Vec::new(),
                        bindings: Vec::new(),
                        arrived: Vec::new(),
                    }
                })
                .collect();
        }
        return groups
            .into_iter()
            .map(|(_, fields, members)| {
                let node = members[0].node;
                let register = fields
                    .into_iter()
                    .map(|(name, value, prov)| Reg {
                        prov,
                        site: Origin {
                            node,
                            stage: stage_now(),
                        },
                        name: Some(name),
                        value,
                    })
                    .collect();
                let prov = Prov::list(
                    members
                        .iter()
                        .map(|m| match &m.topic {
                            Some(_) => m.prov.clone(),
                            None => Prov::at(m.node, stage_now()),
                        })
                        .collect(),
                );
                let topics: Vec<Value> = members
                    .iter()
                    .map(|m| {
                        m.topic
                            .clone()
                            .unwrap_or_else(|| node_scalar(adapter, m.node))
                    })
                    .collect();
                Capsa {
                    prov,
                    node,
                    register,
                    topic: Some(Value::list(topics)),
                    members,
                    captures: Vec::new(),
                    named: Vec::new(),
                    marks: Vec::new(),
                    bindings: Vec::new(),
                    arrived: Vec::new(),
                }
            })
            .collect();
    }

    let mut keyed: Vec<(Vec<Value>, Capsa)> = caps
        .into_iter()
        .enumerate()
        .map(|(i, c)| (key_of(&c, i), c))
        .collect();
    match call.name.as_str() {
        "sort_by" => {
            keyed.sort_by(|(a, _), (b, _)| {
                has_null(a)
                    .cmp(&has_null(b))
                    .then_with(|| compare_keys(a, b))
            });
        }
        // Descending / ascending, stable, truncated to the count.
        "top" => {
            keyed.retain(|(k, _)| !has_null(k));
            keyed.sort_by(|(a, _), (b, _)| compare_keys(b, a));
            keyed.truncate(count.unwrap_or(0));
        }
        "bottom" => {
            keyed.retain(|(k, _)| !has_null(k));
            keyed.sort_by(|(a, _), (b, _)| compare_keys(a, b));
            keyed.truncate(count.unwrap_or(0));
        }
        // First capsa per distinct key, in original order.
        "unique_by" => {
            let mut seen: HashSet<Vec<SortKey>> = HashSet::new();
            keyed.retain(|(k, _)| {
                let hashed: Vec<SortKey> = k
                    .iter()
                    .map(|v| v.sort_key_with(&|e| adapter.unit_scale(e)))
                    .collect();
                seen.insert(hashed)
            });
        }
        // Every capsa achieving the extreme key (ties included).
        "min_by" | "max_by" => {
            keyed.retain(|(k, _)| !has_null(k));
            let want = if call.name == "min_by" {
                Ordering::Less
            } else {
                Ordering::Greater
            };
            let extreme = keyed
                .iter()
                .map(|(k, _)| k.clone())
                .reduce(|a, b| if compare_keys(&b, &a) == want { b } else { a });
            if let Some(extreme) = extreme {
                keyed.retain(|(k, _)| compare_keys(k, &extreme) == Ordering::Equal);
            }
        }
        _ => {}
    }
    keyed.into_iter().map(|(_, c)| c).collect()
}
