//! Part of the executor: see `exec/mod.rs`.

use crate::pipeline::*;

/// One group of a keyed aggregate: the key, its fields, its members.
type Group<X> = (Vec<Value>, Vec<(String, Value, Prov)>, Vec<Thread<X>>);

/// One field of a record — or of every record in a list, so a
/// group's member list reads elementwise; anything
/// else is null.
/// The properties a command projects directly on the capsa's node:
/// every `::name` operand in its expressions (a bare relative path
/// with no steps and a named projection).
fn projected_properties(command: &Command) -> Vec<String> {
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
        fn command(&mut self, s: &Command) {
            if !matches!(s, Command::Subcontext { .. }) {
                visit::walk_command(self, s);
            }
        }
    }
    let mut v = Props(Vec::new());
    v.command(command);
    v.0
}

/// Whether a command's expressions read the context (`@_`, `@@_`,
/// `#*`, `##*`, a peer read; directly or through a
/// pipe tail) — the trigger for materializing the input snapshot.
/// Subcontext and push bodies are excluded: their commands get
/// contexts of their own.
pub(crate) fn command_reads_context(command: &Command) -> bool {
    struct Reads(bool);
    impl visit::Visitor for Reads {
        fn operand(&mut self, o: &Operand) {
            if matches!(
                o,
                Operand::Topics
                    | Operand::Positions { .. }
                    | Operand::Matrix { .. }
                    | Operand::CtxTable
                    | Operand::PeerReg { .. }
            ) {
                self.0 = true;
            } else {
                visit::walk_operand(self, o);
            }
        }
        fn command(&mut self, s: &Command) {
            if !matches!(s, Command::Subcontext { .. }) {
                visit::walk_command(self, s);
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
    v.command(command);
    v.0
}

pub(crate) fn record_field(v: Value, name: &str) -> Value {
    match v {
        // A frame's column by name, as a list; an array has none.
        Value::Table(t) => t.column(name).unwrap_or(Value::Null),
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
pub(crate) fn topic_field(op: &Operand) -> Option<Vec<String>> {
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

/// Apply one pipeline command to the capsa context.
/// Run `commands` in order. `@| group(…)` directly followed by
/// `| count` is one fused step: the groups are tallied as they
/// form, and no group materializes its member list, its
/// provenance list, or its topic list only for `count` to take
/// the length. The result — topic, register, provenance — is the
/// one the two commands produce; a word count over a novel spends
/// its time on the words instead of on lists it then discards.
pub(crate) fn apply_commands<H: Host>(
    commands: &[Command],
    offset: usize,
    top: bool,
    mut caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    let mut i = 0;
    while i < commands.len() {
        if top {
            with_run(|s| s.command = (offset + i) as u32 + 1);
        }
        if let (Command::Agg(call), Some(Command::Func(next))) = (&commands[i], commands.get(i + 1))
        {
            if call.name == "group" && next.name == "count" && next.args.is_empty() {
                caps = keyed_agg(call, caps, host, trace, outer, None, true);
                i += 2;
                continue;
            }
        }
        caps = apply_command(&commands[i], caps, host, trace, outer);
        i += 1;
    }
    caps
}

pub(crate) fn apply_command<H: Host>(
    command: &Command,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    // The `@*` snapshot: materialized only when the command actually
    // reads the context (the scan is cheap; the clone is not).
    let snapshot = command_reads_context(command).then(|| Peers::new(caps.clone()));
    let peers = snapshot.as_ref();
    // The properties this command projects (`::x`), read on every
    // capsa's node: a remote host may fetch them for the whole
    // frontier first.
    let names = projected_properties(command);
    if !names.is_empty() {
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        host.prefetch(&caps, &names);
    }
    match command {
        // `sh('cmd')` — the shell command (gated: [`gate_shell`]
        // refused it long before we got here unless the host
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
        // host speaks); a topic value renders as text. The
        // export button and the pipe are the same verb.
        Command::Func(call)
            if matches!(call.name.as_str(), "markdown" | "html" | "atrep" | "conllu") =>
        {
            caps.into_iter()
                .map(|mut c| {
                    let v = match &c.topic {
                        Some(t) => {
                            Value::Str(host.render_values(std::slice::from_ref(t), &call.name))
                        }
                        None => Value::Str(host.render_node(&c.ext, &call.name)),
                    };
                    c.prov = effective_prov(host, &c);
                    c.topic = Some(v);
                    c
                })
                .collect()
        }
        Command::Func(call) if call.name == "json" || call.name == "jsonl" => caps
            .into_iter()
            .map(|mut c| {
                let v = match &c.topic {
                    Some(t) => Value::Str(t.to_json()),
                    None => Value::Str(host.node_to_json_text(&c.ext)),
                };
                c.prov = effective_prov(host, &c);
                c.topic = Some(v);
                c
            })
            .collect(),
        // `| link`: the capsa's node as a clickable result — the
        // host's `::link` record (title, href, text), whatever
        // the topic was, so `::lemma | link` still links the
        // section. An host with no notion of a link yields null.
        Command::Func(call) if call.name == "link" => caps
            .into_iter()
            .map(|mut c| {
                c.topic = Some(host.property_of(&c.ext, "link").unwrap_or(Value::Null));
                c.prov = host.prov_at(&c.ext);
                c
            })
            .collect(),
        // `| kaiv` mirrors `| json`: a topic value renders as a
        // one-value kaiv document; a node context renders its
        // subtree's value form. `@| kaiv` (stdlib) renders the
        // whole stream as one document.
        #[cfg(not(feature = "kaiv"))]
        Command::Func(call) if call.name == "kaiv" => {
            record_refusal("kaiv output is not compiled in (feature kaiv)".into());
            caps
        }
        #[cfg(feature = "kaiv")]
        Command::Func(call) if call.name == "kaiv" => caps
            .into_iter()
            .map(|mut c| {
                let doc = match &c.topic {
                    Some(t) => crate::pipeline::kaiv_out::document(std::slice::from_ref(t)),
                    None => crate::pipeline::kaiv_out::document(std::slice::from_ref(&host.node_to_json(&c.ext,
                    ))),
                };
                c.prov = effective_prov(host, &c);
                c.topic = Some(Value::Str(doc.unwrap_or_else(|e| e)));
                c
            })
            .collect(),
        // `| xml` mirrors `| json`: a topic value serializes to
        // XML (record fields → elements, `@name` fields
        // attributes, `#text` the text; a list repeats an <item>);
        // a node context serializes the subtree.
        Command::Func(call) if call.name == "xml" => caps
            .into_iter()
            .map(|mut c| {
                let xml = match &c.topic {
                    Some(t) => host.value_to_xml(t, "item"),
                    None => host.node_to_xml(&c.ext),
                };
                c.prov = effective_prov(host, &c);
                c.topic = Some(Value::Str(xml));
                c
            })
            .collect(),
        // The gate again, where the command would run: the scan
        // before evaluation is the early refusal, this is the one
        // no query shape can walk around.
        // An environment command: the host's namespace under the
        // alias runs it (`sh:tr`, `go:mytool`), per capsa or over
        // the context by the control operator.
        Command::Func(call) if crate::pipeline::env::split_env(&call.name).is_some() => {
            env_command(call, caps, host, trace, outer, peers)
        }
        Command::Agg(call) if crate::pipeline::env::split_env(&call.name).is_some() => {
            env_agg_command(call, caps, host, trace, outer, peers)
        }
        // `@| sum(--of=::n)` — the option's expression is read on
        // each capsa in turn, as a key is, and the reduction runs
        // over the results (draft 164): the capsae carry the results
        // as their topics into the reduction's own arm.
        Command::Agg(call) if call.of_expr().is_some() => {
            let expr = call.of_expr().expect("checked");
            let caps = of_values(expr, caps, host, trace, outer, peers);
            apply_command(&Command::Agg(call.without_of()), caps, host, trace, outer)
        }
        Command::Func(call) if call.name == "sh" && !run_option(|o| o.shell_allowed()) => {
            record_refusal("the sh(...) command runs external commands; pass --allow-shell".into());
            Vec::new()
        }
        Command::Func(call) if call.name == "sh" => {
            shell_command(call, caps, host, trace, outer, peers)
        }
        // `default(fallback)`: the topic when non-null, else the
        // fallback — which may be an expression evaluated against
        // the capsa's own node, making `(a) | default$$.b` the
        // coalesce idiom (chainable). Literal
        // fallbacks keep working through stdlib for host-free
        // callers.
        Command::Func(call) if call.name == "exchange" => {
            exchange_command(call, caps, host, trace, outer, peers)
        }
        Command::Func(call) if call.name == "default" => {
            default_command(call, caps, host, trace, outer, peers)
        }
        // `record(...)` (alias `rec`) builds a record per capsa: expression fields
        // evaluate against the capsa's node, auto-named by their
        // projection or named by a preceding literal.
        Command::Func(call) if matches!(call.name.as_str(), "record" | "rec") => caps
            .into_iter()
            .enumerate()
            .map(|(i, mut c)| {
                let scope = c.scope(Some(i + 1), peers, outer);
                match record_fields_prov(&call.args, host, &c.ext, trace, scope) {
                    Ok(f) => {
                        let (fields, prov) = split_fields(f);
                        c.topic = Some(Value::Record(fields));
                        c.prov = prov;
                    }
                    Err(why) => {
                        crate::pipeline::record_refusal(why);
                        c.topic = Some(Value::Null);
                    }
                }
                c
            })
            .collect(),
        // `%%(...)` — the named register view enriched with the
        // given fields: register fields first (the `%.` layout),
        // the args after in written order, an arg overriding a
        // register field that shares its name.
        Command::RecordWith(call) => record_with_command(call, caps, host, trace, outer, peers),
        // A keyed aggregate on the plain pipe works per capsa on the
        // capsa's *members* (a group's rows): reorder or filter them,
        // rebuild the list topic from the survivors. A memberless
        // capsa passes through unchanged.
        Command::Func(call) if stdlib::known_keyed(&call.name) => caps
            .into_iter()
            .map(|mut c| {
                if c.members.is_empty() {
                    return c;
                }
                let members = keyed_agg(
                    call,
                    std::mem::take(&mut c.members),
                    host,
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
                                .unwrap_or_else(|| host.node_scalar(&m.ext))
                        })
                        .collect(),
                ));
                c.prov = Prov::list(members.iter().map(|m| effective_prov(host, m)).collect());
                c.members = members;
                c
            })
            .collect(),
        // An association aggregate on the plain pipe reads its two
        // expressions off each capsa's *members* (a group's rows) —
        // `@| group(::k) | corr(:x; :y)`. A memberless capsa has no
        // pairs to associate: null.
        Command::Func(call) if stdlib::association(&call.name) => caps
            .into_iter()
            .map(|mut c| {
                let members = std::mem::take(&mut c.members);
                c.topic = Some(if members.is_empty() {
                    Value::Null
                } else {
                    associate_caps(call, &members, host, trace, outer, peers)
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
        // reads text as a number under a locale's conventions; so is
        // `ngrams`: `@| ngrams(n)` windows the stream, `| ngrams(n)`
        // the word n-grams of a text.)
        // `| #` on a node: a `tabula` node read as a table (spec: The
        // Table); `| %` the record of its properties (draft 164);
        // `| @` has no reading of a node and is refused with the
        // pointer. On a value, the readings of the value.
        Command::Func(call)
            if matches!(call.name.as_str(), "#" | "%" | "@" | "fields")
                && caps.iter().any(|c| c.topic.is_none()) =>
        {
            caps.into_iter()
            .map(|mut c| {
                let v = match c.topic.take() {
                    Some(t) => stdlib::apply_scalar(call, t, &|e| host.scale_of(e))
                        .into_iter()
                        .next()
                        .unwrap_or(Value::Null),
                    // the record of the node's properties: its names,
                    // each read as `::name` reads it
                    None if call.name == "%" => Value::Record(
                        host.property_names_of(&c.ext)
                            .into_iter()
                            .map(|n| {
                                let v = host.property_of(&c.ext, &n).unwrap_or(Value::Null);
                                (n, v)
                            })
                            .collect(),
                    ),
                    // `| fields` on a node: its core field projections
                    None if call.name == "fields" => Value::list(
                        crate::graph::navigate::CORE_FIELDS
                            .iter()
                            .map(|k| Value::projection(1, *k))
                            .collect(),
                    ),
                    None if call.name == "@" => {
                        record_refusal(
                            "@: a node has no row reading; '| %' reads its properties as a \
                             record, '| #' a tabula node as a table"
                                .into(),
                        );
                        Value::Null
                    }
                    None => match host.node_table(&c.ext) {
                        Ok(t) => Value::table(t),
                        Err(why) => {
                            record_refusal(format!("#: {why}"));
                            Value::Null
                        }
                    },
                };
                c.prov = effective_prov(host, &c);
                c.topic = Some(v);
                c
            })
            .collect()
        }
        Command::Func(call)
            if stdlib::known_agg(&call.name)
                && !matches!(
                    call.name.as_str(),
                    "number" | "ngrams" | "csv" | "table" | "pretty"
                ) =>
        {
            agg_as_func_command(call, caps, host, trace, outer, peers)
        }
        // Per-capsa scalar transform; a command may expand one topic
        // into several (e.g. `lines`), forking the capsa.
        Command::Func(call) => func_command(call, caps, host, trace, outer, peers),
        // The map pipe: the inner command runs once per element of
        // the topic, per capsa — capsae unchanged, the list
        // reassembled (an expanding inner command flattens). Inside,
        // `$_` is the element and `@*` reads the element-context.
        // `.v(body){N}` — the iteration loop: the push
        // applied round after round, every round a command of its own
        // (its own snapshot, so each capsa reads the previous
        // round's values); exactly N rounds for `{N}`, else at least
        // `min` and on until a round repoints no name and changes
        // no topic, or `max` (the quantifier bound when open).
        Command::Repeat {
            command: inner,
            min,
            max,
        } => repeat_command(inner, min, max, caps, host, trace, outer),
        Command::Map(inner) => map_command(inner, caps, host, trace, outer),
        // The spread: fork a list topic into one thread per
        // element; null forks to nothing, a non-list scalar passes
        // through, a record passes whole. The outer form `...?`
        // differs in one case: where the plain spread would fork
        // to nothing, it emits ONE thread with a null topic — the
        // row survives its empty side (OPTIONAL MATCH).
        Command::Spread { outer } => caps
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
                values.into_iter().zip(provs).map(move |(v, prov)| Thread::<H::Ext> {
                    prov,
                    register: c.register.clone(),
                    topic: Some(v),
                    members: Vec::new(),
                    captures: c.captures.clone(),
                    named: c.named.clone(),
                    ext: c.ext.clone(),
                })
            })
            .collect(),
        // A navigation command: resume navigation from each capsa's
        // node (or from `^` / its `(name)` mark), registers and
        // marks carried forward — the pipeline spelling of a path
        // continuation. The parser admits it only in navigation
        // mode, so no live topic is dropped here. Steps behave
        // exactly as in the path: same predicate scope law (no
        // capsa of their own), same group/breadcrumb machinery,
        // same (node, register) dedup as a branch union.
        Command::Nav(branch) => host.navigate(branch, caps, trace, outer, peers),
        // Push the topic onto the register — or, in a node context
        // (no topic), MARK the node: `.name` is `.name($$_)`, a
        // reference to the position filed as a regula like any
        // value (spec: The Capsa, one memory, one name pool).
        Command::Push(name) => caps
            .into_iter()
            .map(|mut c| {
                let (value, prov) = match c.topic.clone() {
                    None => match host.node_ref(&c.ext) {
                        Some(r) => (r, Prov::at(host.origin_at(&c.ext).position, command_now())),
                        None => return c,
                    },
                    Some(topic) => (topic, c.prov.clone()),
                };
                c.register.push(Reg {
                    prov,
                    site: host.origin_at(&c.ext),
                    name: name.clone(),
                    value,
                });
                c
            })
            .collect(),
        // Evaluate the sub-expression from each node, reduce to a
        // scalar, and push it; the topic stays.
        Command::Subcontext { name, body } => {
            subcontext_command(name, body, caps, host, trace, outer, peers)
        }
        // A value expression against the capsa's node becomes the
        // topic.
        Command::Expr(expr) => expr_command(expr, caps, host, trace, outer, peers),
        // ... and a pushed one is a computed column.
        Command::ExprPush { name, expr } => {
            expr_push_command(name, expr, caps, host, trace, outer, peers)
        }
        // `| .%` — the fields push: each field of the topic record
        // becomes a named regula; the topic stays the record.
        Command::FieldsPush => caps
            .into_iter()
            .map(|mut c| {
                match &c.topic {
                    Some(Value::Record(fields)) => {
                        for (k, v) in fields.clone() {
                            c.register.push(Reg {
                                prov: c.prov.part(&k),
                                site: host.origin_at(&c.ext),
                                name: Some(k),
                                value: v,
                            });
                        }
                    }
                    // A topic that is not a record has no fields to
                    // file: refused by name, never a silent empty push.
                    other => record_refusal(format!(
                        "the fields push '| .%' takes a record topic, not {}; a plain '| .' files any topic as one regula",
                        match other {
                            Some(Value::Null) | None => "a node with no topic".to_string(),
                            Some(Value::Str(_)) => "text".to_string(),
                            Some(Value::Int(_) | Value::Float(_)) => "a number".to_string(),
                            Some(Value::Bool(_)) => "a boolean".to_string(),
                            Some(Value::List(_)) => "a list".to_string(),
                            Some(_) => "a value".to_string(),
                        }
                    )),
                }
                c
            })
            .collect(),
        // `.%(...)` / `.name%(...)` — build the record and push it,
        // the topic left as it was (`.%%(...)`: the enriched
        // register view, as `%%(...)` renders it).
        Command::RecordPush {
            name,
            call,
            enriched,
        } => record_push_command(name, call, enriched, caps, host, trace, outer, peers),
        // `@| .name(body)` — the aggregate push: the body runs once
        // over the context as a whole, and its value is filed under
        // the name on every capsa; the context passes on unchanged.
        Command::AggPush { name, body } => {
            if caps.is_empty() {
                return caps;
            }
            let seed = seed_capsa(&caps, host);
            let scope = seed.scope(None, peers, outer);
            let (value, o) =
                capture(|| operand_value(host, &seed.ext, body, trace, scope));
            let prov = Prov::leaf(o);
            caps.into_iter()
                .map(|mut c| {
                    c.register.push(Reg {
                        prov: prov.clone(),
                        site: host.origin_at(&c.ext),
                        name: name.clone(),
                        value: value.clone(),
                    });
                    c
                })
                .collect()
        }
        // `@| %(…)` — the aggregate record: every field a body over
        // the context as a whole; one capsa, the record its topic,
        // the shared regulae its register.
        Command::AggRecord { call, enriched } => {
            let seed = seed_capsa(&caps, host);
            let scope = seed.scope(None, peers, outer);
            let built = match record_fields_prov(&call.args, host, &seed.ext, trace, scope) {
                Ok(f) => f,
                Err(why) => {
                    crate::pipeline::record_refusal(why);
                    let mut out = seed;
                    out.topic = Some(Value::Null);
                    return vec![out];
                }
            };
            let (fields, prov) = if *enriched {
                let mut fields = named_register_fields(&seed.register);
                let mut parts = named_register_parts(&seed.register);
                for (name, value, p) in built {
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
                split_fields(built)
            };
            let mut out = seed;
            out.topic = Some(Value::Record(fields));
            out.prov = prov;
            vec![out]
        }
        // `@| @(0; $_)`, `@| 0` — an expression over the context as
        // a whole: the seed capsa's topic is the list of the topics,
        // and the one capsa out holds the expression's value.
        Command::AggExpr(expr) => {
            let seed = seed_capsa(&caps, host);
            expr_command(expr, vec![seed], host, trace, outer, peers)
        }
        // `@| %.` / `@| $.name` — a register accessor after the
        // aggregation pipe reads once: the regulae every capsa
        // shares, as one capsa.
        Command::AggRecall(r) => {
            let mut out = seed_capsa(&caps, host);
            out.topic = Some(recall(&out.register, r));
            out.prov = recall_prov(&out.register, r);
            vec![out]
        }
        // `@| ungroup` flattens groups back to their member capsae,
        // each inheriting the group's key registers. A memberless
        // capsa passes through unchanged.
        Command::Agg(call) if call.name == "ungroup" => caps
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
        Command::Agg(call) if call.name == "window" => {
            window_command(call, caps, host, trace, outer, peers)
        }
        // `@| shift(n[, key])` — each capsa's topic becomes the
        // effective topic of the capsa `n` positions back (negative
        // looks forward; null where none exists). Registers and
        // captures stay the capsa's own.
        Command::Agg(call) if call.name == "shift" => {
            shift_command(call, caps, host, trace, outer, peers)
        }
        // `@| zscore([sample][, key])` — each capsa's topic becomes
        // its standard score over the context, or over its
        // partition. Row-aligned like `shift`.
        Command::Agg(call) if call.name == "zscore" => {
            zscore_command(call, caps, host, trace, outer, peers)
        }
        // `@| number[(level)]` and `@| outline[(level; text[; "1."])]`
        // — each capsa's topic becomes its dotted number in the
        // stream of levels, or its line of the outline. Row-aligned
        // like `shift`.
        Command::Agg(call) if matches!(call.name.as_str(), "number" | "outline") => {
            outline_command(call, caps, host, trace, outer, peers)
        }
        // `@| tree[(…)]` — the context drawn as a tree, one text
        // value (spec: The tree).
        Command::Agg(call) if call.name == "tree" => {
            tree_command(call, caps, host, trace, outer, peers)
        }
        // `@| timeline(EXPR[; ANCHOR][; forward])` — the narrative
        // point carried from capsa to capsa. Row-aligned like
        // `shift`.
        Command::Agg(call) if call.name == "timeline" => {
            timeline_command(call, caps, host, trace, outer, peers)
        }
        // `@| corr(:a; :b)` and its kin reduce two per-capsa
        // expressions to one number over the whole context.
        Command::Agg(call) if stdlib::association(&call.name) => {
            association_command(call, caps, host, trace, outer, peers)
        }
        // Keyed aggregates reorder or filter the capsae themselves,
        // preserving nodes, registers, and topics.
        Command::Agg(call) if stdlib::known_keyed(&call.name) => {
            keyed_agg(call, caps, host, trace, outer, peers, false)
        }
        // The order/selection family is capsa-preserving too: it
        // reorders or picks, never reduces, so nodes and registers
        // ride along (making `@| sort_by(…) @| reverse | [..n]`
        // compose). The effective topic is baked in, keeping value
        // outputs identical to a plain reduction's.
        Command::Agg(call)
            if matches!(
                call.name.as_str(),
                "sort" | "unique" | "reverse" | "first" | "last"
            ) =>
        {
            order_command(call, caps, host)
        }
        // Positional selection from the whole context, with the same
        // numbering as the navigation-side predicates.
        Command::Select(pred @ (Predicate::Index(_) | Predicate::Range(_, _))) => {
            positional(caps, pred)
        }
        // An expression predicate in select position — reachable only
        // through an inline aggregate pipe (`(... @| [cond] ...)`),
        // which unlike the top-level `@|` does not reject it at parse
        // time — filters per capsa exactly as `| [cond]` does, never a
        // silent pass-through of every item.
        Command::Select(Predicate::Expr(e)) => {
            apply_command(&Command::Filter(e.clone()), caps, host, trace, outer)
        }
        // Per-capsa filter: each capsa is kept iff the condition
        // holds against its node.
        Command::Filter(cond) => filter_command(cond, caps, host, trace, outer, peers),
        // `jaccard(EXPR)`: the other set from an expression — a
        // register holding a list, a sub-pipeline's values — read
        // once, against the first capsa's scope (every capsa of a
        // context shares the register the set was bound in).
        Command::Agg(call)
            if call.name == "jaccard" && call.args.iter().any(|a| matches!(a, Arg::Expr(_))) =>
        {
            let resolved = match caps.first() {
                Some(c) => FnCall {
                    name: call.name.clone(),
                    args: call
                        .args
                        .iter()
                        .map(|a| match a {
                            Arg::Expr(e) => {
                                let mut vs =
                                    eval_operand(host, &c.ext, e, trace, c.scope(None, peers, outer));
                                Arg::Lit(if vs.len() == 1 {
                                    vs.pop().expect("one value")
                                } else {
                                    Value::list(vs)
                                })
                            }
                            other => other.clone(),
                        })
                        .collect(),
                },
                None => call.clone(),
            };
            agg_command(&resolved, caps, host)
        }
        // Reduce the whole context's topics to a new context.
        Command::Agg(call) => agg_command(call, caps, host),
        // Recall a register value into the topic.
        Command::Recall(r) => caps
            .into_iter()
            .map(|mut c| {
                c.prov = recall_prov(&c.register, r);
                c.topic = Some(recall(&c.register, r));
                c
            })
            .collect(),
    }
}

/// `default(x)` — the topic, or `x` where it is null. `apply_command`'s arm.
pub(crate) fn default_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    caps.into_iter()
        .map(|mut c| {
            let topic = c.topic.take().unwrap_or(Value::Null);
            let taken = matches!(topic, Value::Null);
            let (v, o) = capture(|| {
                if taken {
                    match call.args.first() {
                        Some(Arg::Lit(v)) => v.clone(),
                        Some(Arg::Expr(e)) => operand_scalar(
                            host,
                            &c.ext,
                            e,
                            trace,
                            Scope {
                                prov: None,
                                register: &c.register,
                                members: &c.members,
                                topic: None,
                                ordinal: None,
                                captures: &c.captures,
                                named: &c.named,
                                peers,
                                outer,
                                driver: None,
                                ext: &c.ext,
                                view: c.ext.view(),
                                crossing: None,
                                witness: None,
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

/// `%(...)` — the record built from the topic. `apply_command`'s arm.
pub(crate) fn record_with_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    caps.into_iter()
        .enumerate()
        .map(|(i, mut c)| {
            let scope = c.scope(Some(i + 1), peers, outer);
            let built = match record_fields_prov(&call.args, host, &c.ext, trace, scope) {
                Ok(f) => f,
                Err(why) => {
                    crate::pipeline::record_refusal(why);
                    c.topic = Some(Value::Null);
                    return c;
                }
            };
            let mut fields = named_register_fields(&c.register);
            let mut parts = named_register_parts(&c.register);
            for (name, value, p) in built {
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

/// An aggregate name used as a per-capsa command over a list topic. `apply_command`'s arm.
pub(crate) fn agg_as_func_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    let stripped = call.without_of();
    let of = call.of_expr();
    let call = if of.is_some() { &stripped } else { call };
    caps.into_iter()
        .map(|mut c| {
            // The reducers over structures (draft 164): a record's
            // values, a table's columns into a record; `count` of
            // a record's fields and a table's rows.
            if of.is_none()
                && let Some(topic @ (Value::Record(_) | Value::Table(_))) = &c.topic
                && let Some(v) = stdlib::reduce_structure(call, topic, &|e| host.scale_of(e))
            {
                c.topic = Some(v);
                c.prov = c.prov.flat();
                c.members = Vec::new();
                return c;
            }
            let items = match of {
                // `| sum(--of=:n)` after `group`: the expression is
                // read on each member in turn (each its own capsa),
                // or on each item of a list topic as the topic, a
                // table's rows, a record's values (draft 164).
                Some(expr) => {
                    let members = std::mem::take(&mut c.members);
                    let threads: Vec<Thread<H::Ext>> = if !members.is_empty() {
                        c.topic.take();
                        members
                    } else {
                        let as_thread = |topic: Option<Value>, prov: Prov| Thread::<H::Ext> {
                            prov,
                            register: c.register.clone(),
                            topic,
                            members: Vec::new(),
                            captures: c.captures.clone(),
                            named: c.named.clone(),
                            ext: c.ext.clone(),
                        };
                        match c.topic.take() {
                            Some(l @ Value::List(_)) => l
                                .into_items()
                                .unwrap_or_default()
                                .into_iter()
                                .enumerate()
                                .map(|(i, v)| as_thread(Some(v), c.prov.item(i)))
                                .collect(),
                            Some(Value::Table(t)) => t
                                .as_ref()
                                .clone()
                                .into_records()
                                .into_items()
                                .unwrap_or_default()
                                .into_iter()
                                .enumerate()
                                .map(|(i, v)| as_thread(Some(v), c.prov.item(i)))
                                .collect(),
                            Some(Value::Record(fields)) => fields
                                .into_iter()
                                .map(|(k, v)| {
                                    let prov = c.prov.part(&k);
                                    as_thread(Some(v), prov)
                                })
                                .collect(),
                            Some(other) => vec![as_thread(Some(other), c.prov.clone())],
                            None => vec![as_thread(None, c.prov.clone())],
                        }
                    };
                    of_values(expr, threads, host, trace, outer, peers)
                        .into_iter()
                        .map(|t| t.topic.unwrap_or(Value::Null))
                        .collect()
                }
                None => match c.topic.take() {
                    Some(l @ Value::List(_)) => l.into_items().unwrap_or_default(),
                    Some(other) => vec![other],
                    None => vec![host.node_scalar(&c.ext)],
                },
            };
            let mut out = stdlib::apply(call, items, &|e| host.scale_of(e));
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

/// `| repeat(...)` — the iteration command. `apply_command`'s arm.
pub(crate) fn repeat_command<H: Host>(
    inner: &Command,
    min: &usize,
    max: &Option<usize>,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    let mut caps = caps;
    // The bound holds here as in a path quantifier: an
    // explicit ceiling or minimum above it is refused, an
    // open loop runs to it.
    let n_max = host.quantifier_bound();
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
    let signature = |caps: &[Thread<H::Ext>]| -> Vec<RoundState<<H::Ext as Attachment>::Key>> {
        caps.iter()
            .map(|c| {
                (
                    c.ext.key(),
                    c.topic.clone(),
                    named_register_fields(&c.register),
                )
            })
            .collect()
    };
    let mut round = 0;
    while round < limit {
        let before = if exact || round < *min {
            None
        } else {
            Some(signature(&caps))
        };
        caps = apply_command(inner, caps, host, trace, outer);
        round += 1;
        if let Some(b) = before
            && b == signature(&caps)
        {
            break;
        }
    }
    caps
}

/// `| (body)` — a subcontext run per capsa. `apply_command`'s arm.
pub(crate) fn subcontext_command<H: Host>(
    name: &Option<String>,
    body: &Query,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    caps.into_iter()
        .enumerate()
        .map(|(i, mut c)| {
            // The body sees this capsa as its enclosing scope:
            // `$.name` / `$$_` / `$$ord` reach it (correlated
            // subqueries).
            let scope = c.scope(Some(i + 1), peers, outer);
            let (value, prov) = host.subcontext_traced(body, &c.ext, trace, Some(&scope));
            c.register.push(Reg {
                prov,
                site: host.origin_at(&c.ext),
                name: name.clone(),
                value,
            });
            // The cursor is untouched: the topic that came down
            // the pipe stays, the body's value waits in the capsa
            // (spec: Def Capsa). The push still returns the thread
            // to navigation mode (see ExprPush).
            c
        })
        .collect()
}

/// `| expr` — an expression over the topic. `apply_command`'s arm.
pub(crate) fn expr_command<H: Host>(
    expr: &Operand,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    caps.into_iter()
        .enumerate()
        .flat_map(|(i, mut c)| {
            let scope = c.scope(Some(i + 1), peers, outer);
            // An operand that yields several values is a hop in
            // value position: each value is its own row, as each
            // target of a navigation is.
            if c.members.is_empty() {
                let (values, o) = capture(|| eval_operand(host, &c.ext, expr, trace, scope));
                // a range of no values is no rows, not one null row
                let empty_range = values.is_empty() && matches!(expr, Operand::Range { .. });
                if values.len() > 1 || empty_range {
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
            let (value, o) = capture(|| group_value(host, &c.ext, expr, trace, scope));
            c.topic = Some(value);
            c.prov = Prov::leaf(o);
            vec![c]
        })
        .collect()
}

/// `.name(expr)` — push an expression's value. `apply_command`'s arm.
pub(crate) fn expr_push_command<H: Host>(
    name: &Option<String>,
    expr: &Operand,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    caps.into_iter()
        .enumerate()
        .map(|(i, mut c)| {
            let scope = c.scope(Some(i + 1), peers, outer);
            let (value, o) = capture(|| operand_value(host, &c.ext, expr, trace, scope));
            c.register.push(Reg {
                prov: Prov::leaf(o),
                site: host.origin_at(&c.ext),
                name: name.clone(),
                value,
            });
            // A push appends to the capsa and leaves the cursor as
            // it is: the topic before the push is the topic after
            // it (spec: Def Capsa, draft 154). The PUSH is still
            // what returns the thread to navigation mode: a
            // following hop is legal and rebuilds the capsae, the
            // filed value safe in the register (spec: Execution
            // Modes).
            c
        })
        .collect()
}

/// `.%(...)` — build the record and push it. `apply_command`'s arm.
pub(crate) fn record_push_command<H: Host>(
    name: &Option<String>,
    call: &FnCall,
    enriched: &bool,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    caps.into_iter()
        .enumerate()
        .map(|(i, mut c)| {
            let scope = c.scope(Some(i + 1), peers, outer);
            let built = match record_fields_prov(&call.args, host, &c.ext, trace, scope) {
                Ok(f) => f,
                Err(why) => {
                    crate::pipeline::record_refusal(why);
                    return c;
                }
            };
            let (fields, prov) = if *enriched {
                let mut fields = named_register_fields(&c.register);
                let mut parts = named_register_parts(&c.register);
                for (name, value, p) in built {
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
                split_fields(built)
            };
            // Filed, the topic left as it was (spec: the record
            // push, draft 154).
            c.register.push(Reg {
                prov,
                site: host.origin_at(&c.ext),
                name: name.clone(),
                value: Value::Record(fields),
            });
            c
        })
        .collect()
}

/// The order and selection family (`sort_by`, `reverse`, `first`, …): capsa-preserving, the effective topic baked in. `apply_command`'s arm.
pub(crate) fn order_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
) -> Vec<Thread<H::Ext>> {
    // `first` / `last` over an empty context: an empty input
    // reduces to null (spec: Aggregate Commands), a thread with no
    // position.
    if caps.is_empty() && matches!(call.name.as_str(), "first" | "last") {
        return vec![Thread::bare(host.unplaced_ext(), Some(Value::Null))];
    }
    // Which capsae are still nodes (no topic yet): `unique`
    // tells nodes apart by identity, values by value. An ordering
    // reduction over nodes acts on the NODES and yields node
    // threads (spec: Lists, draft 164): a node stands by its name
    // for the ordering, but no name is materialized as its topic —
    // `@| reverse | :name` reads the core field as before.
    let is_node: Vec<bool> = caps.iter().map(|c| c.topic.is_none()).collect();
    let mut caps: Vec<Thread<H::Ext>> = caps
        .into_iter()
        .map(|mut c| {
            if c.topic.is_none() {
                c.prov = host.prov_at(&c.ext);
            }
            c
        })
        .collect();
    let topic_of = |c: &Thread<H::Ext>| {
        c.topic
            .clone()
            .unwrap_or_else(|| host.node_scalar(&c.ext))
    };
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
                let scale = |e: &str| host.scale_of(e);
                caps.sort_by(|a, b| {
                    stdlib::default_cmp(&topic_of(a), &topic_of(b), &scale, collate.as_deref())
                })
            }
        },
        "reverse" => caps.reverse(),
        "unique" => {
            // A dropped duplicate hands its origins to the
            // survivor: the value was read there too.
            let mut kept: Vec<Thread<H::Ext>> = Vec::new();
            let mut by_node: HashMap<<H::Ext as Attachment>::Key, usize> = HashMap::new();
            let mut by_value: HashMap<crate::value::SortKey, usize> = HashMap::new();
            // A node is itself, whatever it reads as: two
            // characters with the same (or no) text are two.
            // A value is one with every equal value — equal under
            // the value comparison (the sort key), the one rule
            // the reducing `unique` and `unique_by` dedupe by, so
            // the context command, a list topic and a body's
            // headless `unique` count alike.
            let scale = |e: &str| host.scale_of(e);
            for (c, node) in caps.drain(..).zip(is_node.iter()) {
                let seen = if *node {
                    by_node.get(&c.ext.key()).copied()
                } else {
                    by_value.get(&topic_of(&c).sort_key_with(&scale)).copied()
                };
                match seen {
                    Some(i) => kept[i].prov.origins.union(&c.prov.origins),
                    None => {
                        if *node {
                            by_node.insert(c.ext.key(), kept.len());
                        } else {
                            by_value.insert(topic_of(&c).sort_key_with(&scale), kept.len());
                        }
                        kept.push(c);
                    }
                }
            }
            caps = kept;
        }
        // `first` / `last`: one capsa, the first / last n, or a rank
        // band (`first(11..20)`, `last(2..3)`; a negative rank from
        // the other end).
        "first" => {
            let (s, e) = stdlib::rank_span(call.args.first(), caps.len(), false)
                .unwrap_or((0, caps.len().min(1)));
            caps.truncate(e);
            caps.drain(..s);
        }
        "last" => {
            let (s, e) = stdlib::rank_span(call.args.first(), caps.len(), true)
                .unwrap_or((caps.len().saturating_sub(1), caps.len()));
            caps.truncate(e);
            caps.drain(..s);
        }
        _ => {}
    }
    caps
}

/// `sh('cmd')` — the shell command, one command per capsa with the topic as stdin: `apply_command`'s shell arm.
pub(crate) fn shell_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    caps.into_iter()
        .enumerate()
        .map(|(i, mut c)| {
            let scope = c.scope(Some(i + 1), peers, outer);
            let (cmd, o) = capture(|| {
                call.args.first().map(|a| match a {
                    Arg::Lit(v) => v.to_string(),
                    Arg::Expr(e) => operand_scalar(host, &c.ext, e, trace, scope).to_string(),
                    Arg::Range(..) | Arg::Named { .. } | Arg::Opt { .. } | Arg::Pairs { .. } => {
                        String::new()
                    }
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

/// `| f(...)` — a per-capsa command on the topic: `apply_command`'s simple-command arm.
pub(crate) fn func_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
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
                                host,
                                &c.ext,
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
            stdlib::apply_scalar(call, topic, &|e| host.scale_of(e))
                .into_iter()
                .map(move |v| Thread::<H::Ext> {
                    prov: prov.clone(),
                    register: c.register.clone(),
                    topic: Some(v),
                    members: Vec::new(),
                    captures: c.captures.clone(),
                    named: c.named.clone(),
                    ext: c.ext.clone(),
                })
        })
        .collect()
}

/// `| f -> inner` — the mapped command over each member: `apply_command`'s map arm.
pub(crate) fn map_command<H: Host>(
    inner: &Command,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
) -> Vec<Thread<H::Ext>> {
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
            let pseudo: Vec<Thread<H::Ext>> = items
                .into_iter()
                .zip(provs)
                .map(|(v, prov)| Thread::<H::Ext> {
                    prov,
                    register: c.register.clone(),
                    topic: Some(v),
                    members: Vec::new(),
                    captures: c.captures.clone(),
                    named: c.named.clone(),
                    ext: c.ext.clone(),
                })
                .collect();
            let (mut out, mut provs): (Vec<Value>, Vec<Prov>) =
                apply_command(inner, pseudo, host, trace, outer)
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

/// `| [cond]` as a command — `apply_command`'s filter arm.
pub(crate) fn filter_command<H: Host>(
    cond: &PredExpr,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
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
            let hit = if !host.witnessed(&c.ext) {
                host.exists_binding(&c.ext, &[cond], trace, scope)
            } else {
                eval_pred_expr(host, &c.ext, cond, trace, scope)
            };
            if !hit {
                return None;
            }
            // A successful `=~` in the filter binds its groups
            // for later commands ($1 …); a filter without one
            // leaves earlier captures intact. A NAMED group is
            // a named push: `(?<year>…)` files its
            // text into the register under `year`, read back as
            // `$.year` / `(.year)` and seen by `%.`.
            // A named group is a capture, not a push: `%+:year`
            // is the match's, `$.year` the capsa's — filing is
            // explicit (`| .year(%+:year)`, or `| %+ | .%`).
            let groups = extract_captures(cond, host, &c.ext, trace, scope);
            if let Some((groups, named)) = groups {
                c.captures = groups;
                c.named = named;
            }
            Some(c)
        })
        .collect()
}

/// `@| f(...)` — an aggregate over the whole context (or each partition): `apply_command`'s aggregate arm.
/// `--of=expr` (spec: Aggregating Pipe, draft 164): the expression
/// read on each capsa in turn — its own topic, position and memory
/// in reach — as the value the reduction takes; a null result is a
/// missing value, as the reduction treats one.
fn of_values<H: Host>(
    expr: &Operand,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    caps.into_iter()
        .enumerate()
        .map(|(i, mut c)| {
            let scope = c.scope(Some(i + 1), peers, outer);
            let (value, o) = capture(|| group_value(host, &c.ext, expr, trace, scope));
            c.topic = Some(value);
            c.prov = Prov::leaf(o);
            c
        })
        .collect()
}

pub(crate) fn agg_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
) -> Vec<Thread<H::Ext>> {
    // The output boundary is the tool's: `qua`, `quai` and the
    // session take `@| encoding(LABEL)` off the end of a query
    // and encode what they print. Reaching the executor means
    // it stood somewhere else, or the host does not encode.
    if call.name == "encoding" {
        record_refusal(
            "the encoding command stands last: '… @| csv @| encoding(windows-1251)'; \
                 the tool that prints the output applies it"
                .into(),
        );
        return Vec::new();
    }
    // `@| csv(title; year)` / `@| table(…)` over nodes: the
    // named columns are the nodes' properties, as they are a
    // record's fields — a node stands for the record of the
    // columns asked of it.
    let columns: Vec<&str> = if matches!(call.name.as_str(), "csv" | "table" | "pretty")
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
            // `@| #` over nodes: a node contributes the record of its
            // properties (a CSV row its columns), so `/* @| #` is the
            // frame; a node with none stands by its scalar (a sample).
            None if call.name == "#" => host.node_to_json(&c.ext),
            None if !columns.is_empty() => Value::Record(
                columns
                    .iter()
                    .map(|col| {
                        (
                            col.to_string(),
                            host.property_of(&c.ext, col).unwrap_or(Value::Null),
                        )
                    })
                    .collect(),
            ),
            None => host.node_scalar(&c.ext),
        })
        .collect();
    // The combinations family combines values; a node
    // stands in a reduction by its name. Nodes that share one
    // name (a table's rows, a list's same-named fields) would
    // combine into a silent nothing, so the command says what
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
            None => origins.insert(host.origin_at(&c.ext)),
        }
    }
    // The reduction's one thread has no position (spec: Aggregating
    // Pipe): `$$_` and `$-` are null after it, and navigation
    // continues only where the query says from where (`$$__/…`, a
    // mark).
    let node = host.unplaced_ext();
    // A reduction that hands back one of its inputs (max,
    // min, first, last, unique, sort, reverse) keeps that
    // input's origins, so `((!$_:%origin:id))` after `@| max`
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
                    o.insert(host.origin_at(&c.ext));
                    o
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    let inputs = if selects { topics.clone() } else { Vec::new() };
    // The reduction keeps the regulae its inputs all share: a
    // single capsa keeps its whole register across an aggregate,
    // and what every row filed alike is still readable after.
    let register = shared_register(&caps);
    stdlib::apply(call, topics, &|e| host.scale_of(e))
        .into_iter()
        .map(|v| Thread::<H::Ext> {
            prov: Prov::leaf(
                inputs
                    .iter()
                    .position(|t| *t == v)
                    .map(|i| per_input[i].clone())
                    .unwrap_or_else(|| origins.clone()),
            ),
            register: register.clone(),
            topic: Some(v),
            members: Vec::new(),
            captures: Vec::new(),
            named: Vec::new(),
            ext: node.clone(),
        })
        .collect()
}

/// The regulae every capsa of the context shares — the first
/// capsa's register, kept where each other capsa filed the same
/// name with an equal value. One capsa shares its whole register;
/// none share nothing.
pub(crate) fn shared_register<X: Attachment>(caps: &[Thread<X>]) -> Vec<Reg> {
    let Some((first, rest)) = caps.split_first() else {
        return Vec::new();
    };
    first
        .register
        .iter()
        .filter(|r| {
            rest.iter().all(|c| {
                c.register
                    .iter()
                    .any(|q| q.name == r.name && q.value == r.value)
            })
        })
        .cloned()
        .collect()
}

/// The seed of a body fed by the aggregation pipe: the context as a
/// whole — its topic the list of the peers' topics (a node standing
/// by its name), its provenance theirs, its register the shared
/// regulae, standing on the first capsa's node (the root when there
/// is none).
pub(crate) fn seed_capsa<H: Host>(caps: &[Thread<H::Ext>], host: &H) -> Thread<H::Ext> {
    let (topics, provs): (Vec<Value>, Vec<Prov>) = caps
        .iter()
        .map(|c| match &c.topic {
            Some(t) => (t.clone(), c.prov.clone()),
            None => (host.node_scalar(&c.ext), host.prov_at(&c.ext)),
        })
        .unzip();
    Thread::<H::Ext> {
        prov: Prov::list(provs),
        register: shared_register(caps),
        topic: Some(Value::list(topics)),
        members: Vec::new(),
        captures: Vec::new(),
        named: Vec::new(),
        ext: caps
            .first()
            .map_or_else(|| host.root_ext(), |c| c.ext.bare()),
    }
}

/// Apply a keyed aggregate: reorder or filter the capsae by keys
/// evaluated per capsa against its node, preserving nodes,
/// registers, and topics. Sorting is stable; a composite key
/// compares lexicographically; a null key sorts like empty text
/// (consistent with `Value::compare` everywhere else).
/// `counting`: the fused `group | count` (see [`apply_commands`]) —
/// each group's topic is its member count, its provenance the
/// members' origins, and no member is kept.
pub(crate) fn keyed_agg<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
    counting: bool,
) -> Vec<Thread<H::Ext>> {
    // `top` / `bottom` carry their count — or a rank band, `top(11..20;
    // ::key)`, `top(-10; ::key)` from the far end — as a leading literal.
    let (count, key_args): (Option<&Arg>, &[Arg]) = match call.args.split_first() {
        Some((sel @ (Arg::Lit(Value::Int(_)) | Arg::Range(_, _)), rest))
            if matches!(call.name.as_str(), "top" | "bottom") =>
        {
            (Some(sel), rest)
        }
        _ => (None, &call.args),
    };
    let key_of = |c: &Thread<H::Ext>, i: usize| -> Vec<Value> {
        key_args
            .iter()
            .filter_map(|a| match a {
                Arg::Expr(e) => Some(operand_scalar(
                    host,
                    &c.ext,
                    e,
                    trace,
                    c.scope(Some(i + 1), peers, outer),
                )),
                Arg::Lit(_)
                | Arg::Range(_, _)
                | Arg::Named { .. }
                | Arg::Opt { .. }
                | Arg::Pairs { .. } => None,
            })
            .collect()
    };
    let compare_keys = |a: &[Value], b: &[Value]| -> Ordering {
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| x.compare_with(y, &|e| host.scale_of(e)))
            .find(|o| *o != Ordering::Equal)
            .unwrap_or(Ordering::Equal)
    };

    // A missing key does not compete — the ordering counterpart of
    // the numeric aggregates skipping non-numeric values. Extremes
    // and top/bottom exclude null-keyed capsae; sort_by keeps them,
    // last; unique_by treats null as its own group.
    let has_null = |k: &[Value]| k.iter().any(|v| matches!(v, Value::Null));
    let has_nan = |k: &[Value]| k.iter().any(stdlib::is_nan);

    // `group` partitions instead of reordering: one capsa per
    // distinct key (first-appearance order; null keys grouped out,
    // pandas' dropna), topic = the list of member topics, the key
    // fields pushed as named regs for later recall ($.city).
    if call.name == "group" {
        // (key, key fields, members) per group; when counting, the
        // members stay empty and `tally` holds (count, origins,
        // first node) instead.
        let mut groups: Vec<Group<H::Ext>> = Vec::new();
        let mut tally: Vec<(i64, Origins, H::Ext)> = Vec::new();
        let member_origins = |c: &Thread<H::Ext>| match &c.topic {
            Some(_) => c.prov.origins.clone(),
            None => host.origins_at(&c.ext),
        };
        // Key → position in `groups`: one probe per capsa, where a
        // scan over the groups formed so far made a word count over
        // a novel quadratic in its vocabulary.
        let mut index: HashMap<Vec<SortKey>, usize> = HashMap::new();
        for (i, c) in caps.into_iter().enumerate() {
            let scope = c.scope(Some(i + 1), peers, outer);
            let fields = match record_fields_prov(&call.args, host, &c.ext, trace, scope) {
                Ok(f) => f,
                Err(why) => {
                    crate::pipeline::record_refusal(why);
                    continue;
                }
            };
            let key: Vec<Value> = fields.iter().map(|(_, v, _)| v.clone()).collect();
            if key.iter().any(|v| matches!(v, Value::Null)) {
                continue;
            }
            let hashed: Vec<SortKey> = key
                .iter()
                .map(|v| v.sort_key_with(&|e| host.scale_of(e)))
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
                        tally.push((1, member_origins(&c), c.ext.bare()));
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
                            site: host.origin_at(&node),
                            name: Some(name),
                            value,
                        })
                        .collect();
                    Thread::<H::Ext> {
                        prov: Prov::leaf(origins),
                        register,
                        topic: Some(Value::Int(n)),
                        members: Vec::new(),
                        captures: Vec::new(),
                        named: Vec::new(),
                        ext: node.clone(),
                    }
                })
                .collect();
        }
        return groups
            .into_iter()
            .map(|(_, fields, members)| {
                let node = members[0].ext.bare();
                let register = fields
                    .into_iter()
                    .map(|(name, value, prov)| Reg {
                        prov,
                        site: host.origin_at(&node),
                        name: Some(name),
                        value,
                    })
                    .collect();
                let prov = Prov::list(
                    members
                        .iter()
                        .map(|m| match &m.topic {
                            Some(_) => m.prov.clone(),
                            None => host.prov_at(&m.ext),
                        })
                        .collect(),
                );
                let topics: Vec<Value> = members
                    .iter()
                    .map(|m| m.topic.clone().unwrap_or_else(|| host.node_scalar(&m.ext)))
                    .collect();
                Thread::<H::Ext> {
                    prov,
                    register,
                    topic: Some(Value::list(topics)),
                    members,
                    captures: Vec::new(),
                    named: Vec::new(),
                    ext: node.clone(),
                }
            })
            .collect();
    }

    let mut keyed: Vec<(Vec<Value>, Thread<H::Ext>)> = caps
        .into_iter()
        .enumerate()
        .map(|(i, c)| (key_of(&c, i), c))
        .collect();
    match call.name.as_str() {
        // A null key sorts last, and a NaN key after every number
        // whichever way the key runs (`-$_` of NaN is NaN).
        "sort_by" => {
            keyed.sort_by(|(a, _), (b, _)| {
                has_null(a)
                    .cmp(&has_null(b))
                    .then_with(|| has_nan(a).cmp(&has_nan(b)))
                    .then_with(|| compare_keys(a, b))
            });
        }
        // Descending / ascending, stable, truncated to the count. A
        // null or NaN key is never ranked.
        "top" | "bottom" => {
            keyed.retain(|(k, _)| !has_null(k) && !has_nan(k));
            if call.name == "top" {
                keyed.sort_by(|(a, _), (b, _)| compare_keys(b, a));
            } else {
                keyed.sort_by(|(a, _), (b, _)| compare_keys(a, b));
            }
            let (s, e) = stdlib::rank_span(count, keyed.len(), false).unwrap_or((0, 0));
            keyed.truncate(e);
            keyed.drain(..s);
        }
        // First capsa per distinct key, in original order.
        "unique_by" => {
            let mut seen: HashSet<Vec<SortKey>> = HashSet::new();
            keyed.retain(|(k, _)| {
                let hashed: Vec<SortKey> = k
                    .iter()
                    .map(|v| v.sort_key_with(&|e| host.scale_of(e)))
                    .collect();
                seen.insert(hashed)
            });
        }
        // Every capsa achieving the extreme key (ties included); a
        // null or NaN key is never an extreme.
        "min_by" | "max_by" => {
            keyed.retain(|(k, _)| !has_null(k) && !has_nan(k));
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

/// `exchange(RATES[; TARGET])` — the currency conversion (spec: The
/// exchange). `apply_command`'s arm: the rates are an expression
/// (a register, a record literal) evaluated per thread.
pub(crate) fn exchange_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    caps.into_iter()
        .map(|mut c| {
            let topic = c.topic.take().unwrap_or(Value::Null);
            let scope = c.scope(None, peers, outer);
            let arg = |n: usize| -> Option<Value> {
                match call.args.get(n) {
                    Some(Arg::Lit(v)) => Some(v.clone()),
                    Some(Arg::Expr(e)) => Some(operand_scalar(host, &c.ext, e, trace, scope)),
                    _ => None,
                }
            };
            let rates = arg(0).unwrap_or(Value::Null);
            let target = arg(1).map(|v| v.to_string());
            let v =
                match crate::value::quantity::exchange(&topic, &rates, target.as_deref(), &|e| {
                    host.scale_of(e)
                }) {
                    Ok(v) => v,
                    Err(why) => {
                        record_refusal(why);
                        Value::Null
                    }
                };
            c.prov = c.prov.flat();
            c.topic = Some(v);
            c
        })
        .collect()
}
