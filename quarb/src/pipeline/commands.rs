//! Part of the executor: see `exec/mod.rs`.

use crate::pipeline::*;
use crate::syntax::ast::IndexSpec;

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
/// `#*`, `##*`; directly or through a pipe tail) — the trigger
/// for materializing the input snapshot. The node read (`X:.name`)
/// is not one: it reads the run's attachments, not the context.
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
        // A frame's column by name, as a list — on an indexed table
        // the record keyed by the labels (draft 166); an array has
        // none.
        Value::Table(t) => match t.column_value(name) {
            Ok(v) => v,
            Err(why) => {
                record_refusal(why);
                Value::Null
            }
        },
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
        // A walk's facts are in scope for the one command after it.
        for c in &mut caps {
            c.ext.end_walk();
        }
        // a trailing `session(…)` that sets answers nothing, as a push
        // does (draft 166): no thread, so no line
        if i + 1 == commands.len()
            && let Command::Func(call) = &commands[i]
            && call.name == "session"
            && !session_settings(call).is_empty()
        {
            caps = Vec::new();
        }
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
        // `| shift(n)` on a value (spec: The Table, "The index",
        // draft 166): a list's items, a record's values and an
        // indexed table's cells moved n places down (null where
        // nothing stands), the keys and the labels kept; a plain
        // table moves its rows.
        // `| copy` — the output boundary is the tool's (spec: The
        // clipboard): `qua`, the notebook and the browser hosts take
        // it off the end and copy what they would have printed.
        // Reaching the executor means the host does not copy, or
        // the command stood inside a body.
        Command::Func(call) if call.name == "copy" => {
            record_refusal(
                "copy stands last and is the tool's: the host that prints the output applies it; this host does not copy, or the command stood inside a body"
                    .into(),
            );
            Vec::new()
        }
        Command::Func(call) if call.name == "shift" => {
            let n: i64 = match call.args.first() {
                Some(Arg::Lit(Value::Int(n))) => *n,
                None => 1,
                _ => return refuse_all::<H>(caps, "shift takes the number of places, an integer: shift(2)"),
            };
            let shifted = |items: Vec<Value>| -> Vec<Value> {
                let len = items.len() as i64;
                (0..len)
                    .map(|i| {
                        let src = i - n;
                        if src < 0 || src >= len { Value::Null } else { items[src as usize].clone() }
                    })
                    .collect()
            };
            caps.into_iter()
                .map(|mut c| {
                    let Some(topic) = c.topic.take() else {
                        record_refusal("shift moves the values of a list, a record or a table; a node has none (after '@|' it moves the capsae)".into());
                        c.topic = Some(Value::Null);
                        return c;
                    };
                    c.topic = Some(match topic {
                        Value::List(items) => Value::list(shifted(items.to_vec())),
                        Value::Record(r) => {
                            let values: Vec<Value> = r.iter().map(|(_, v)| v.clone()).collect();
                            Value::Record(r.with_values(shifted(values)))
                        }
                        Value::Table(t) if t.index > 0 => {
                            let w = t.width();
                            let mut columns: Vec<Vec<Value>> = (0..w)
                                .map(|j| t.rows.iter().map(|r| r.get(j).cloned().unwrap_or(Value::Null)).collect())
                                .collect();
                            for col in columns.iter_mut().skip(t.index) {
                                *col = shifted(std::mem::take(col));
                            }
                            let rows: Vec<Vec<Value>> = (0..t.rows.len())
                                .map(|i| columns.iter().map(|c| c[i].clone()).collect())
                                .collect();
                            Value::table(t.with_rows(rows))
                        }
                        Value::Table(t) => {
                            let w = t.width();
                            let rows: Vec<Value> = t.rows.iter().map(|r| Value::list(r.clone())).collect();
                            let rows: Vec<Vec<Value>> = shifted(rows)
                                .into_iter()
                                .map(|r| r.into_items().unwrap_or_else(|| vec![Value::Null; w]))
                                .collect();
                            Value::table(t.with_rows(rows))
                        }
                        other => {
                            record_refusal(format!(
                                "shift moves the values of a list, a record or a table, not {}",
                                stdlib::kind_of(&other)
                            ));
                            Value::Null
                        }
                    });
                    c.prov = c.prov.flat();
                    c
                })
                .collect()
        }
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
        // `#(--header=(t | header))` — a header computed elsewhere
        // (spec: The Table): read in the capsa's scope, then the
        // table is built under it as under a written one. After
        // `@|` the stream's first capsa reads it.
        Command::Func(call) | Command::Agg(call)
            if call.name == "#" && matches!(call.args.iter().find(|a| !matches!(a, Arg::Opt { .. })), Some(Arg::Expr(_))) =>
        {
            let Some(Arg::Expr(expr)) = call.args.iter().find(|a| !matches!(a, Arg::Opt { .. })) else {
                unreachable!()
            };
            let agg = matches!(command, Command::Agg(_));
            let mut out = Vec::new();
            let mut built: Option<Command> = None;
            let n = caps.len();
            let mut stream = Vec::with_capacity(n);
            for (i, c) in caps.into_iter().enumerate() {
                if built.is_none() || !agg {
                    let scope = c.scope(Some(i + 1), peers, outer);
                    let value = group_value(host, &c.ext, expr, trace, scope);
                    let arg = match stdlib::computed_header(value) {
                        Ok(arg) => arg,
                        Err(e) => {
                            record_refusal(e);
                            continue;
                        }
                    };
                    let rebuilt = FnCall {
                        name: "#".into(),
                        args: std::iter::once(arg)
                            .chain(call.args.iter().filter(|a| matches!(a, Arg::Opt { .. })).cloned())
                            .collect(),
                    };
                    built = Some(if agg {
                        Command::Agg(rebuilt)
                    } else {
                        Command::Func(rebuilt)
                    });
                }
                if agg {
                    stream.push(c);
                } else if let Some(cmd) = &built {
                    out.extend(apply_command(cmd, vec![c], host, trace, outer));
                }
            }
            if agg && let Some(cmd) = &built {
                out.extend(apply_command(cmd, stream, host, trace, outer));
            }
            out
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
        // `session(--pretty=…; --precision=n)` sets the session's
        // settings and answers nothing (the threads pass, a trailing
        // call dropped by the dispatcher); `session` alone answers
        // the settings record (spec: Session settings, draft 166)
        Command::Func(call) if call.name == "session" => {
            let Some(memory) = run_option(|o| o.session.clone()) else {
                return refuse_all::<H>(
                    caps,
                    "the session is not open in this run: 'session(…)' sets a session's settings (quai, the qua REPL, a .quai file, a one-shot qua run)",
                );
            };
            let settings = session_settings(call);
            if settings.is_empty() {
                let record = memory.borrow().settings();
                return caps
                    .into_iter()
                    .map(|mut c| {
                        c.topic = Some(record.clone());
                        c.prov = c.prov.flat();
                        c.members = Vec::new();
                        c
                    })
                    .collect();
            }
            for (name, value) in settings {
                match (name.as_str(), value) {
                    ("--pretty", Value::Str(w)) => match crate::pipeline::PrettyMode::parse(&w) {
                        Some(m) => memory.borrow_mut().pretty = m,
                        None => {
                            return refuse_all::<H>(
                                caps,
                                &format!("session: --pretty takes 'always', 'structures' or 'off', not '{w}'"),
                            );
                        }
                    },
                    ("--precision" | "-p", Value::Int(n)) if n >= 0 => memory.borrow_mut().precision = n as usize,
                    ("--diagnostics" | "-d", Value::Str(w)) => match w.parse::<crate::diagnostic::Severity>() {
                        Ok(level) => memory.borrow_mut().diagnostics = level,
                        Err(_) => {
                            return refuse_all::<H>(
                                caps,
                                &format!("session: --diagnostics takes 'error', 'warning', 'info' or 'debug', not '{w}'"),
                            );
                        }
                    },
                    (n, v) => {
                        return refuse_all::<H>(
                            caps,
                            &format!(
                                "session: '{n}' is no setting{}; the settings are '--pretty=always|structures|off', '--precision=n' and '--diagnostics=error|warning|info|debug'",
                                if matches!(v, Value::Null) { String::new() } else { format!(" (given {})", crate::stdlib::kind_of(&v)) }
                            ),
                        );
                    }
                }
            }
            caps
        }
        // `project(expr)` (spec: Projectors): the operand,
        // evaluated on the cursor, is a projector or a list of
        // them, read on the same cursor; `--record` keys the values
        // by the projections' bare keys.
        Command::Func(call) if call.name == "project" => {
            project_command(call, caps, host, trace, outer, peers)
        }
        // `map(command; --to=…)` (spec: The Table, "Mapping over the
        // cells"): the command applied to every value one level deep.
        Command::Func(call) if call.name == "map" => {
            map_cells_command(call, caps, host, trace, outer, peers)
        }
        // `| [$.pos]`, `| [@(2; 4)]` — brackets that look at no
        // node, read per capsa: an integer or a list of integers
        // selects the rows or items of the topic; any other value
        // is the condition it always was, judged by truthiness.
        Command::Func(call)
            if call.name == "[]" && call.args.iter().any(|a| matches!(a, Arg::Expr(_))) =>
        {
            let Some(Arg::Expr(op)) = call.args.first() else {
                unreachable!("checked above")
            };
            caps.into_iter()
                .enumerate()
                .flat_map(|(i, c)| {
                    let value =
                        operand_scalar(host, &c.ext, op, trace, c.scope(Some(i + 1), peers, outer));
                    match crate::pipeline::positions_of(&value) {
                        Some(_) => {
                            let resolved = FnCall {
                                name: "[]".into(),
                                args: vec![Arg::Lit(value)],
                            };
                            func_command(&resolved, vec![c], host, trace, outer, peers)
                        }
                        None => filter_command(
                            &PredExpr::Truthy(op.clone()),
                            vec![c],
                            host,
                            trace,
                            outer,
                            peers,
                        ),
                    }
                })
                .collect()
        }
        // `record(...)` (alias `rec`) builds a record per capsa: expression fields
        // evaluate against the capsa's node, auto-named by their
        // projection or named by a preceding literal.
        Command::Func(call) if matches!(call.name.as_str(), "record" | "rec") => caps
            .into_iter()
            .enumerate()
            .map(|(i, mut c)| {
                let scope = c.scope(Some(i + 1), peers, outer);
                match crate::pipeline::operand::record_build(&call.args, host, &c.ext, trace, scope) {
                    Ok((record, provs)) => {
                        let (_, prov) = split_fields(
                            record.iter().cloned().zip(provs).map(|((k, v), p)| (k, v, p)).collect(),
                        );
                        c.topic = Some(Value::Record(record));
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
                // `| [$.pos]` reads its expression per capsa: the
                // scalar route resolves the argument
                && !matches!(
                    call.name.as_str(),
                    "number" | "ngrams" | "csv" | "table" | "pretty" | "[]"
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
        // `.:name(body)` — the attaching push (spec: Attached
        // Regulae): the push it wraps runs as itself, then the
        // regula it filed is attached to each thread's node; the
        // attachments commit when the command ends, so the body's
        // reads saw the previous command's.
        Command::Attach {
            name,
            command: inner,
        } => attach_command(name.as_deref(), inner, caps, host, trace, outer),
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
            out.topic = Some(Value::Record(fields.into()));
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
        // `@| collapse` — one thread per distinct node among the
        // context's threads (spec: Expansion paths), the merge the
        // navigation no longer makes.
        Command::Agg(call) if call.name == "collapse" => collapse_command(call, caps, host),
        // `@| edges(->road)` / `@| nodes` — the context's graph as a
        // table (spec: The graph as a table).
        Command::Agg(call)
            if call.name == "edges" || call.name == "nodes" =>
        {
            graph_table_command(call, caps, host)
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
        Command::Select(pred @ (Predicate::Index(_) | Predicate::Range(_, _) | Predicate::Positions(_))) => {
            positional(caps, pred)
        }
        // A computed bracket (`@| [$.pos]`, `@| [@(2; 4)]`) reads no
        // node: its value, read once against the first capsa's
        // scope, is the position or the position list; any other
        // value filters by truthiness as a condition would.
        Command::Select(Predicate::Computed(op)) => {
            let value = match caps.first() {
                Some(c) => operand_scalar(host, &c.ext, op, trace, c.scope(None, peers, outer)),
                None => return caps,
            };
            match crate::pipeline::positions_of(&value) {
                Some(pred) => positional(caps, &pred),
                None => apply_command(
                    &Command::Filter(PredExpr::Truthy(op.clone())),
                    caps,
                    host,
                    trace,
                    outer,
                ),
            }
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
        // `| ,name` — the session push (spec: Capsa Operations,
        // "Session memory", draft 166): it GATHERS — one thread files
        // its topic (a node the record of its properties), several
        // file the list of their topics, as `@| @` — and the threads
        // pass on untouched.
        Command::SessionPush(name) => {
            let Some(memory) = run_option(|o| o.session.clone()) else {
                return refuse_all::<H>(
                    caps,
                    "the session memory is not open in this run: '| ,name' files a value for a session (quai, the qua REPL, a .quai file); the capsa's push is '| .name'",
                );
            };
            let topics: Vec<Value> = caps
                .iter()
                .map(|c| match &c.topic {
                    Some(t) => t.clone(),
                    None => host.node_to_json(&c.ext),
                })
                .collect();
            let value = match topics.len() {
                1 => topics.into_iter().next().unwrap_or(Value::Null),
                _ => Value::list(topics),
            };
            memory.borrow_mut().memory.insert(name.clone(), value);
            caps
        }
        // `| &$.p` — the quarq the register holds runs here on each
        // capsa, its body the next commands (spec: Quarqs); late
        // bound, so the registers and the cursor are read now.
        Command::Splice(r) => caps
            .into_iter()
            .enumerate()
            .flat_map(|(i, mut c)| match recall(&c.register, r) {
                Value::Quarq(q) => apply_commands(&q.body, 0, false, vec![c], host, trace, outer),
                // a projector is the simplest quarq: read on the capsa
                v if v.holds_projector() => {
                    let read = {
                        let scope = c.scope(Some(i + 1), peers, outer);
                        crate::pipeline::operand::apply_held(host, &c.ext, v, trace, scope)
                    };
                    c.topic = Some(read);
                    vec![c]
                }
                other => {
                    record_refusal(format!(
                        "&{} splices a quarq, and the register holds {}",
                        crate::syntax::unparse::reg(r),
                        stdlib::kind_of(&other)
                    ));
                    c.topic = Some(Value::Null);
                    vec![c]
                }
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
            // the null guard broadcasts one level deep as `??` does
            // (draft 166): a structure's null items, fields or cells
            // take the fallback, the rest stand
            let holes = match &topic {
                Value::List(items) => items.iter().any(|v| matches!(v, Value::Null)),
                Value::Record(r) => r.iter().any(|(_, v)| matches!(v, Value::Null)),
                Value::Table(t) => t.rows.iter().flatten().any(|v| matches!(v, Value::Null)),
                _ => false,
            };
            let taken = matches!(topic, Value::Null) || holes;
            let (v, o) = capture(|| {
                if taken {
                    let fallback = match call.args.first() {
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
                                walk: None,
                                witness: None,
                            },
                        ),
                        _ => Value::Null,
                    };
                    if !holes {
                        return fallback;
                    }
                    let fill = |v: Value| {
                        if matches!(v, Value::Null) {
                            fallback.clone()
                        } else {
                            v
                        }
                    };
                    match topic {
                        Value::List(items) => {
                            Value::list(items.iter().cloned().map(fill).collect())
                        }
                        Value::Record(r) => Value::Record(r.map_values(fill)),
                        Value::Table(t) => {
                            let rows: Vec<Vec<Value>> = t
                                .rows
                                .iter()
                                .map(|row| row.iter().cloned().map(fill).collect())
                                .collect();
                            match t.rebuild(rows) {
                                Ok(nt) => Value::table(nt),
                                Err(e) => {
                                    crate::pipeline::record_refusal(format!("default: {e}"));
                                    Value::Null
                                }
                            }
                        }
                        other => other,
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
            c.topic = Some(Value::Record(fields.into()));
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
                && let Some(topic @ (Value::Record(_) | Value::Table(_) | Value::List(_))) =
                    &c.topic
                && let Some(v) = stdlib::reduce_structure(call, topic, &|e| host.scale_of(e))
            {
                c.topic = Some(v);
                c.prov = c.prov.flat();
                c.members = Vec::new();
                return c;
            }
            // `| #` on a list topic reads it as rows however many items
            // it holds, a list of one included (spec: List
            // Construction, draft 166); the column view is a bare
            // record topic's
            if call.name == "#"
                && of.is_none()
                && matches!(c.topic, Some(Value::List(_)))
                && let Some(l) = c.topic.take()
            {
                c.topic = Some(match stdlib::table_command(call, l) {
                    Ok(v) => v,
                    Err(why) => {
                        record_refusal(format!("#: {why}"));
                        Value::Null
                    }
                });
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
                    // `| type` names the value in front of it, a list
                    // of one included (spec: List Construction, draft
                    // 166: a list of one item is a list)
                    Some(l @ Value::List(_)) if call.name == "type" => vec![l],
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
/// `.:name(body)` / `.:%` — `apply_command`'s arm: the wrapped push,
/// then the attachment of what it filed to the node each thread
/// stands at — the regula under the name, or every regula the
/// fields push filed — committed as the command's (spec: Attached
/// Regulae). A thread standing nowhere (after an aggregate) has no
/// node to attach to and refuses; threads standing at one node must
/// agree (`commit`).
pub(crate) fn attach_command<H: Host>(
    name: Option<&str>,
    inner: &Command,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    let before: Vec<usize> = caps.iter().map(|c| c.register.len()).collect();
    let out = apply_command(inner, caps, host, trace, outer);
    let shown = name.map_or(".:%".to_string(), |n| format!(".:{n}"));
    for (i, c) in out.iter().enumerate() {
        if host.node_ref(&c.ext).is_none() {
            record_refusal(format!(
                "'{shown}' attaches to the node the thread stands at, and this thread stands nowhere \
                 (after an aggregate): navigate to a node before attaching"
            ));
            return Vec::new();
        }
        let filed: Vec<&Reg> = match name {
            Some(n) => c
                .register
                .iter()
                .rev()
                .find(|r| r.name.as_deref() == Some(n))
                .into_iter()
                .collect(),
            // every regula this command filed: the fields push
            // neither forks nor drops a thread, so the registers
            // line up with the input's
            None => c.register[before.get(i).copied().unwrap_or(0).min(c.register.len())..]
                .iter()
                .filter(|r| r.name.is_some())
                .collect(),
        };
        for r in filed {
            attach(
                c.ext.key(),
                Attached {
                    name: r.name.clone().expect("named"),
                    value: r.value.clone(),
                    prov: r.prov.clone(),
                },
            );
        }
    }
    match commit::<<H::Ext as Attachment>::Key>() {
        Ok(_) => out,
        Err(why) => {
            record_refusal(why);
            Vec::new()
        }
    }
}

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
        crate::value::ambient::record_refusal_coded(
            crate::diagnostic::QUANTIFIER_BOUND,
            "the loop",
            format!(
                "the loop's explicit ceiling ({n}) exceeds the quantifier \
                 bound ({n_max}); raise --quantifier-bound"
            ),
        );
        return Vec::new();
    }
    if *min > n_max {
        crate::value::ambient::record_refusal_coded(
            crate::diagnostic::QUANTIFIER_BOUND,
            "the loop",
            format!(
                "the loop's minimum ({min}) exceeds the quantifier bound \
                 ({n_max}); raise --quantifier-bound"
            ),
        );
        return Vec::new();
    }
    let limit = max.unwrap_or(n_max);
    let exact = *max == Some(*min);
    // An open loop (`{m;}`) runs to the bound; reaching it with the
    // capsae still changing is the quantifier's refusal (spec:
    // Quantifier bound), never a silent trail. An explicit ceiling
    // inside the bound is the user's ask: complete at n, silent.
    let open = max.is_none();
    let signature = |caps: &[Thread<H::Ext>]| -> Vec<RoundState<<H::Ext as Attachment>::Key>> {
        caps.iter()
            .map(|c| {
                let key = c.ext.key();
                let attached = attached_fields(&key);
                (
                    key,
                    c.topic.clone(),
                    named_register_fields(&c.register),
                    attached,
                )
            })
            .collect()
    };
    let mut round = 0;
    let mut moving = false;
    while round < limit {
        let forced = exact || round < *min;
        let before = if !forced || (open && round + 1 == limit) {
            Some(signature(&caps))
        } else {
            None
        };
        caps = apply_command(inner, caps, host, trace, outer);
        round += 1;
        if let Some(b) = before {
            let same = b == signature(&caps);
            moving = !same;
            if same && !forced {
                break;
            }
        }
    }
    if open && round == limit && moving {
        record_refusal(format!(
            "the push is still moving at the quantifier bound ({n_max}): {limit} rounds ran \
             and the last still changed the capsae; raise --quantifier-bound, or give the \
             loop a ceiling '{{{min};n}}' to stop there"
        ));
        return Vec::new();
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

/// `| project(expr)` / `| project(expr; --record)` (spec: Projection
/// Values): the operand evaluated on the cursor, every projection
/// value in it, at any depth of lists, records and tables, replaced
/// by that projection's reading on the same cursor, every other
/// value kept. `-r` keys a list operand's values by the projections'
/// bare keys. `apply_command`'s arm.
/// `map(command; --to=:col)` — the element-wise map: the command,
/// read as if a pipe preceded it, applied to every value one level
/// deep — the cells of a table, the items of a list, the fields of a
/// record, each the topic in turn — and the structure answered with
/// the results in place; a table re-types its columns. On a scalar
/// the command on that value. A cell that is a structure reaches the
/// command as one value.
pub(crate) fn map_cells_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    use crate::syntax::ast::Operand;
    let Some(body) = call.args.iter().find_map(|a| match a {
        Arg::Expr(e) => Some(e),
        _ => None,
    }) else {
        return refuse_all::<H>(
            caps,
            "map takes a command: map(round(3)), map(?? 0), map(+ 3)",
        );
    };
    // `map(<int>; --to=:D)` — a trait as the body redeclares the
    // columns or fields (spec: The Table, "Mapping over the cells",
    // draft 166): `<int>` declares and casts, `<*>` undeclares
    let redeclare: Option<crate::Typing> = match body {
        Operand::Typed { inner, typing } if matches!(**inner, Operand::Topic) => {
            Some(typing.clone())
        }
        _ => None,
    };
    let commands: Vec<Command> = match body {
        Operand::Piped { expr, commands } if matches!(**expr, Operand::Seed) => commands.clone(),
        other => vec![Command::Expr(other.clone())],
    };
    // `map(q(+ 3))` is `map(+ 3)`: a quarq written in place applies
    // to each cell, a projector it spells is read on it (spec:
    // Quarqs); a bare register (`map($.f)`) is its value, as
    // anywhere a value stands — `map(&$.f)` splices the held quarq
    let held_ref: Option<&Operand> = match commands.as_slice() {
        [Command::Expr(op @ Operand::Lit(Value::Quarq(_)))] => Some(op),
        _ => None,
    };
    // `--to=:col` / `--to=(:a; :b)`: the columns (or fields) mapped;
    // `--to=[rows]` / `--to=[rows]:col`: the cells the value index
    // names (spec: Indexing a Value)
    enum To<'a> {
        Columns(Vec<String>),
        Index {
            rows: &'a IndexSpec,
            cols: Option<&'a IndexSpec>,
            column: Option<&'a str>,
        },
        /// `--to=:::index`: the labels are the mapped part (draft 166)
        Labels,
    }
    const TO_FORMS: &str =
        "--to takes the cells to map: --to=:col, --to=(:a; :b), --to=[rows], --to=[rows]:col";
    let to: Option<To<'_>> = match call.args.iter().find_map(|a| match a {
        Arg::Opt { name, value } if name == "--to" => Some(value.as_ref()),
        _ => None,
    }) {
        None => None,
        Some(Some(v)) => {
            let name_of = |o: &Operand| match o {
                Operand::Field { base, name } if matches!(**base, Operand::Topic) => {
                    Some(name.clone())
                }
                _ => None,
            };
            match v {
                Operand::Rel {
                    steps,
                    projection: Some(Projection::AdapterMeta(k)),
                    anchor: Anchor::Current,
                } if steps.is_empty() && k == "index" => Some(To::Labels),
                Operand::Index { base, rows, cols } if matches!(**base, Operand::Topic) => {
                    Some(To::Index {
                        rows,
                        cols: cols.as_ref(),
                        column: None,
                    })
                }
                Operand::Field { base, name } => match &**base {
                    Operand::Index { base, rows, cols } if matches!(**base, Operand::Topic) => {
                        Some(To::Index {
                            rows,
                            cols: cols.as_ref(),
                            column: Some(name.as_str()),
                        })
                    }
                    Operand::Topic => Some(To::Columns(vec![name.clone()])),
                    _ => return refuse_all::<H>(caps, TO_FORMS),
                },
                Operand::List(items) => match items.iter().map(name_of).collect::<Option<Vec<_>>>()
                {
                    Some(names) => Some(To::Columns(names)),
                    None => return refuse_all::<H>(caps, TO_FORMS),
                },
                _ => return refuse_all::<H>(caps, TO_FORMS),
            }
        }
        Some(None) => return refuse_all::<H>(caps, TO_FORMS),
    };
    if redeclare.is_some() && matches!(to, Some(To::Index { .. })) {
        return refuse_all::<H>(
            caps,
            "map: --to under a trait names columns or fields, not rows: rows have no type",
        );
    }
    caps.into_iter()
        .enumerate()
        .map(|(i, mut c)| {
            // on a node `map` is refused: a node is no structure of cells
            if c.topic.is_none() {
                record_refusal("map applies to the cells of a structure, and a node is none: read a value first (| %, | @, ::prop)".into());
                c.topic = Some(Value::Null);
                return c;
            }
            let topic = c.topic.take().unwrap_or(Value::Null);
            let scope = c.scope(Some(i + 1), peers, outer);
            if let Some(typing) = &redeclare {
                let redeclared: Result<Value, String> = match &topic {
                    Value::Table(t) => {
                        let on: Vec<bool> = match &to {
                            None => vec![true; t.width()],
                            Some(To::Labels) => {
                                if t.index == 0 {
                                    Err("--to=:::index names the labels, and the table has no index".to_string())
                                } else {
                                    Ok((0..t.width()).map(|j| j < t.index).collect())
                                }
                            }
                            .unwrap_or_default(),
                            Some(To::Columns(names)) => match &t.header {
                                Some(h) => {
                                    if let Some(missing) = names.iter().find(|n| !h.contains(n)) {
                                        Err(format!("no column '{missing}' (the header: {})", h.join(", ")))
                                    } else {
                                        Ok(h.iter().map(|k| names.contains(k)).collect())
                                    }
                                }
                                None => Err("--to names columns, and the table has no header".into()),
                            }
                            .unwrap_or_default(),
                            Some(To::Index { .. }) => unreachable!("refused above"),
                        };
                        if on.len() != t.width() {
                            Err(match (&to, &t.header) {
                                (Some(To::Labels), _) => "--to=:::index names the labels, and the table has no index".to_string(),
                                (Some(To::Columns(names)), Some(h)) => {
                                    let missing = names.iter().find(|n| !h.contains(n)).cloned().unwrap_or_default();
                                    format!("no column '{missing}' (the header: {})", h.join(", "))
                                }
                                _ => "--to names columns, and the table has no header".to_string(),
                            })
                        } else {
                            let mut declared: Vec<Option<crate::ColumnType>> =
                                (0..t.width()).map(|j| t.declaration(j)).collect();
                            for (j, on) in on.iter().enumerate() {
                                if *on {
                                    declared[j] = typing.declared();
                                }
                            }
                            crate::Table::typed(t.header.clone(), t.rows.clone(), declared).map(Value::table)
                        }
                    }
                    // on a record the fields named are cast into the
                    // declaration (the declaration itself is kept
                    // from draft 166's record, to come)
                    Value::Record(fields) => {
                        let on: Vec<bool> = match &to {
                            None => vec![true; fields.len()],
                            Some(To::Columns(names)) => fields.iter().map(|(k, _)| names.contains(k)).collect(),
                            Some(To::Labels) => vec![false; fields.len()],
                            Some(To::Index { .. }) => unreachable!("refused above"),
                        };
                        let mut out = Vec::with_capacity(fields.len());
                        let mut err = None;
                        for ((k, v), on) in fields.iter().zip(on) {
                            let v = match (on, typing.declared()) {
                                (true, Some(ty)) => match crate::value::cast_cell(v, &ty) {
                                    Some(x) => x,
                                    None => {
                                        err = Some(format!(
                                            "field '{k}' is declared {}, and holds {}, which does not cast to it",
                                            ty.word(),
                                            crate::value::held_text(v)
                                        ));
                                        break;
                                    }
                                },
                                _ => v.clone(),
                            };
                            out.push((k.clone(), v));
                        }
                        match err {
                            Some(e) => Err(e),
                            None => Ok(Value::Record(out.into())),
                        }
                    }
                    other => Err(format!(
                        "a trait as the body redeclares a table's columns or a record's fields, not {}",
                        stdlib::kind_of(other)
                    )),
                };
                c.topic = Some(match redeclared {
                    Ok(v) => v,
                    Err(why) => {
                        record_refusal(format!("map({}): {why}", typing.spelling()));
                        Value::Null
                    }
                });
                return c;
            }
            let held = held_ref.map(|op| operand_scalar(host, &c.ext, op, trace, scope));
            let apply = |cell: &Value| -> Value {
                let body = match &held {
                    Some(Value::Quarq(q)) => (*q.body).clone(),
                    Some(p) if p.holds_projector() => {
                        let cscope = Scope {
                            topic: Some(cell),
                            prov: None,
                            members: &[],
                            ..scope
                        };
                        let read = |l: u8, k: &str| {
                            crate::pipeline::operand::read_projection(host, &c.ext, l, k, trace, cscope)
                        };
                        return crate::pipeline::operand::walk_projections(p.clone(), &read);
                    }
                    _ => commands.clone(),
                };
                let piped = Operand::Piped {
                    expr: Box::new(Operand::Lit(cell.clone())),
                    commands: body,
                };
                operand_scalar(host, &c.ext, &piped, trace, scope)
            };
            // the cells the index form names, read on this topic
            let mask = match &to {
                Some(To::Index { rows, cols, column }) => {
                    let resolve = |s: &IndexSpec| {
                        crate::pipeline::operand::resolve_index_spec(host, &c.ext, s, trace, scope)
                    };
                    let resolved = (|| -> Result<stdlib::IndexMask, String> {
                        let rows = resolve(rows)?;
                        let cols = cols.map(resolve).transpose()?;
                        stdlib::index_mask(&topic, &rows, cols.as_ref(), *column)
                    })();
                    match resolved {
                        Ok(m) => Some(m),
                        Err(why) => {
                            record_refusal(format!("map: {why}"));
                            c.topic = Some(Value::Null);
                            return c;
                        }
                    }
                }
                _ => None,
            };
            // `--to=:::index`: each label the topic in turn; the shape of
            // the results decides the index (spec: The Table, "The
            // index"): a list one TUPLE column named by the levels
            // joined with `_`, a record new levels named by its keys, a
            // scalar one level (renamed under one, joined under several)
            if let (Some(To::Labels), Value::Table(t)) = (&to, &topic) {
                let rebuilt = (|| -> Result<Value, String> {
                    let labels = t
                        .labels()
                        .ok_or_else(|| "--to=:::index names the labels, and the table has no index".to_string())?;
                    let results: Vec<Value> = labels.iter().map(|l| apply(l)).collect();
                    let joined = t.level_names().join("_");
                    let data: Vec<usize> = (t.index..t.width()).collect();
                    let body = |r: &Vec<Value>| -> Vec<Value> { data.iter().map(|&j| r.get(j).cloned().unwrap_or(Value::Null)).collect() };
                    let data_header: Vec<String> = data.iter().map(|&j| t.column_name(j)).collect();
                    let data_declared: Vec<Option<crate::ColumnType>> = data.iter().map(|&j| t.declaration(j)).collect();
                    if results.iter().all(|v| matches!(v, Value::List(_))) {
                        let arity = results.iter().map(|v| v.clone().into_items().map_or(0, |i| i.len())).max().unwrap_or(0);
                        // the tuple's types: each position typed as a column
                        let types: Vec<crate::ColumnType> = (0..arity)
                            .map(|p| {
                                let cells: Vec<Value> = results
                                    .iter()
                                    .map(|v| v.clone().into_items().and_then(|i| i.get(p).cloned()).unwrap_or(Value::Null))
                                    .collect();
                                crate::value::type_column(cells).0
                            })
                            .collect();
                        let tuple = crate::ColumnType::Tuple(std::sync::Arc::new(types));
                        let mut header = vec![joined];
                        header.extend(data_header);
                        let mut declared: Vec<Option<crate::ColumnType>> = vec![Some(tuple)];
                        declared.extend(data_declared);
                        let rows: Vec<Vec<Value>> = t
                            .rows
                            .iter()
                            .zip(&results)
                            .map(|(r, l)| {
                                let mut row = vec![l.clone()];
                                row.extend(body(r));
                                row
                            })
                            .collect();
                        return crate::Table::typed(Some(header), rows, declared).map(|t| Value::table(t.with_index(1)));
                    }
                    if results.iter().all(|v| matches!(v, Value::Record(_))) {
                        let mut keys: Vec<String> = Vec::new();
                        for v in &results {
                            if let Value::Record(r) = v {
                                for (k, _) in r.iter() {
                                    if !keys.contains(k) {
                                        keys.push(k.clone());
                                    }
                                }
                            }
                        }
                        let mut header = keys.clone();
                        header.extend(data_header);
                        let mut declared: Vec<Option<crate::ColumnType>> = vec![None; keys.len()];
                        declared.extend(data_declared);
                        let rows: Vec<Vec<Value>> = t
                            .rows
                            .iter()
                            .zip(&results)
                            .map(|(r, l)| {
                                let mut row: Vec<Value> = match l {
                                    Value::Record(rec) => keys.iter().map(|k| rec.get(k).cloned().unwrap_or(Value::Null)).collect(),
                                    _ => vec![Value::Null; keys.len()],
                                };
                                row.extend(body(r));
                                row
                            })
                            .collect();
                        let n = keys.len();
                        return crate::Table::typed(Some(header), rows, declared).map(|t| Value::table(t.with_index(n)));
                    }
                    if results.iter().any(|v| !crate::stdlib::flat_cell(v)) {
                        return Err("--to=:::index: the labels mapped are all lists, all records or all scalars; the results mix them".into());
                    }
                    let name = if t.index == 1 { t.column_name(0) } else { joined };
                    let mut header = vec![name];
                    header.extend(data_header);
                    let mut declared: Vec<Option<crate::ColumnType>> = vec![if t.index == 1 { t.declaration(0) } else { None }];
                    declared.extend(data_declared);
                    let rows: Vec<Vec<Value>> = t
                        .rows
                        .iter()
                        .zip(&results)
                        .map(|(r, l)| {
                            let mut row = vec![l.clone()];
                            row.extend(body(r));
                            row
                        })
                        .collect();
                    crate::Table::typed(Some(header), rows, declared).map(|t| Value::table(t.with_index(1)))
                })();
                c.topic = Some(match rebuilt {
                    Ok(v) => v,
                    Err(why) => {
                        record_refusal(format!("map: {why}"));
                        Value::Null
                    }
                });
                c.prov = c.prov.flat();
                return c;
            }
            let mapped = match topic {
                Value::Table(t) => {
                    let on_cell: Box<dyn Fn(usize, usize) -> bool> = match (&to, &mask) {
                        (None, _) => Box::new(|_, _| true),
                        (Some(To::Columns(names)), _) => match &t.header {
                            Some(h) => {
                                if let Some(missing) = names.iter().find(|n| !h.contains(n)) {
                                    record_refusal(format!(
                                        "map: no column '{missing}' (the header: {})",
                                        h.join(", ")
                                    ));
                                    c.topic = Some(Value::Null);
                                    return c;
                                }
                                let on: Vec<bool> = h.iter().map(|k| names.contains(k)).collect();
                                Box::new(move |_, j| on.get(j).copied().unwrap_or(false))
                            }
                            None => {
                                record_refusal(
                                    "map: --to names columns, and the table has no header".into(),
                                );
                                c.topic = Some(Value::Null);
                                return c;
                            }
                        },
                        (Some(To::Index { .. }), Some(stdlib::IndexMask::Cells { rows, cols })) => {
                            let (rows, cols) = (rows.clone(), cols.clone());
                            Box::new(move |i, j| rows.contains(&i) && cols.contains(&j))
                        }
                        _ => Box::new(|_, _| false),
                    };
                    let rows = t
                        .rows
                        .iter()
                        .enumerate()
                        .map(|(i, r)| {
                            r.iter()
                                .enumerate()
                                .map(|(j, cell)| if on_cell(i, j) { apply(cell) } else { cell.clone() })
                                .collect()
                        })
                        .collect();
                    // a declared column casts the results or refuses,
                    // an undeclared one re-types
                    match t.rebuild(rows) {
                        Ok(t) => Value::table(t),
                        Err(why) => {
                            record_refusal(format!("map: {why}"));
                            Value::Null
                        }
                    }
                }
                Value::List(items) => {
                    let on: Vec<bool> = match (&to, &mask) {
                        (None, _) => vec![true; items.len()],
                        (_, Some(stdlib::IndexMask::Positions(p))) => {
                            (0..items.len()).map(|i| p.contains(&i)).collect()
                        }
                        _ => {
                            record_refusal("map: --to names columns, and a list has items: --to=[rows]".into());
                            c.topic = Some(Value::Null);
                            return c;
                        }
                    };
                    Value::list(
                        items
                            .iter()
                            .zip(on)
                            .map(|(v, on)| if on { apply(v) } else { v.clone() })
                            .collect(),
                    )
                }
                Value::Record(fields) => {
                    let on: Vec<bool> = match (&to, &mask) {
                        (None, _) => vec![true; fields.len()],
                        (Some(To::Columns(names)), _) => {
                            fields.iter().map(|(k, _)| names.contains(k)).collect()
                        }
                        (_, Some(stdlib::IndexMask::Positions(p))) => {
                            (0..fields.len()).map(|i| p.contains(&i)).collect()
                        }
                        _ => vec![false; fields.len()],
                    };
                    // a declared field casts the result into its type
                    // or refuses naming the field (draft 166)
                    let mut values = Vec::with_capacity(fields.len());
                    let mut err = None;
                    for (i, ((k, v), on)) in fields.iter().zip(on).enumerate() {
                        let v = if on { apply(v) } else { v.clone() };
                        let v = match fields.declaration(i) {
                            Some(ty) if on => match crate::value::cast_cell(&v, &ty) {
                                Some(x) => x,
                                None => {
                                    err = Some(format!(
                                        "map: field '{k}' is declared {}, and the result {} does not cast to it",
                                        ty.word(),
                                        crate::value::held_text(&v)
                                    ));
                                    break;
                                }
                            },
                            _ => v,
                        };
                        values.push(v);
                    }
                    match err {
                        Some(why) => {
                            record_refusal(why);
                            Value::Null
                        }
                        None => Value::Record(fields.with_values(values)),
                    }
                }
                scalar => apply(&scalar),
            };
            c.topic = Some(mapped);
            c.prov = c.prov.flat();
            c
        })
        .collect()
}

/// Every thread refused with one message, the topic null.
fn refuse_all<H: Host>(caps: Vec<Thread<H::Ext>>, why: &str) -> Vec<Thread<H::Ext>> {
    record_refusal(why.into());
    caps.into_iter()
        .map(|mut c| {
            c.topic = Some(Value::Null);
            c
        })
        .collect()
}

pub(crate) fn project_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    use crate::syntax::ast::{Anchor, Operand, Projection};
    let as_record = call
        .args
        .iter()
        .any(|a| matches!(a, Arg::Opt { name, .. } if name == "--record" || name == "-r"));
    let Some(expr) = call.args.iter().find_map(|a| match a {
        Arg::Expr(e) => Some(e.clone()),
        Arg::Lit(v) => Some(Operand::Lit(v.clone())),
        _ => None,
    }) else {
        record_refusal(
            "project takes the value to read the projections of: project(:fields; --record), project(%(n = :name))".into(),
        );
        return Vec::new();
    };
    // A projection written bare, `:name` / `::title` / `:::size`,
    // as an item of a list operand: the evaluation reads it, and its
    // key names the record's field under -r.
    let bare = |op: &Operand| -> Option<(u8, String)> {
        match op {
            Operand::Field { base, name } if matches!(**base, Operand::Topic) => {
                Some((1, name.clone()))
            }
            Operand::Rel {
                steps,
                projection: Some(p),
                anchor: Anchor::Current,
            } if steps.is_empty() => Some(match p {
                Projection::CoreMeta(k) => (1, k.clone()),
                Projection::Property(Some(k)) => (2, k.clone()),
                Projection::Property(None) => (2, String::new()),
                Projection::AdapterMeta(k) => (3, k.clone()),
            }),
            _ => None,
        }
    };
    let written: Vec<Option<(u8, String)>> = match &expr {
        Operand::List(items) => items.iter().map(bare).collect(),
        _ => Vec::new(),
    };
    // the projection as it would be written on the cursor: `:key`
    // is the field read (a record's field, a table's column, a
    // node's core field), `::key` and `:::key` the node's ladders
    let as_operand = |ladder: u8, key: &str| match ladder {
        1 => Operand::Field {
            base: Box::new(Operand::Topic),
            name: key.to_string(),
        },
        _ => Operand::Rel {
            steps: Vec::new(),
            projection: Some(match ladder {
                2 => Projection::Property(Some(key.to_string())),
                _ => Projection::AdapterMeta(key.to_string()),
            }),
            anchor: Anchor::Current,
        },
    };
    // every projector, at any depth, read on the cursor
    fn walk(v: Value, read: &dyn Fn(u8, &str) -> Value) -> Value {
        match v {
            Value::Projection { ladder, key, .. } => read(ladder, &key),
            Value::List(items) => {
                Value::list(items.iter().cloned().map(|x| walk(x, read)).collect())
            }
            Value::Record(fields) => Value::Record(
                fields
                    .into_iter()
                    .map(|(k, x)| (k, walk(x, read)))
                    .collect(),
            ),
            Value::Table(t) => {
                let rows = t
                    .rows
                    .iter()
                    .map(|r| r.iter().cloned().map(|x| walk(x, read)).collect())
                    .collect();
                Value::table(t.with_rows(rows))
            }
            other => other,
        }
    }
    let mut out = Vec::new();
    for (i, c) in caps.into_iter().enumerate() {
        // the operand on the cursor; several values fork under the
        // standing rule, each its own capsa
        let (values, o) = capture(|| {
            eval_operand(
                host,
                &c.ext,
                &expr,
                trace,
                c.scope(Some(i + 1), peers, outer),
            )
        });
        let values = if values.is_empty() {
            vec![Value::Null]
        } else {
            values
        };
        for v in values {
            let mut k = c.clone();
            let read = |ladder: u8, key: &str| -> Value {
                let op = as_operand(ladder, key);
                capture(|| {
                    group_value(host, &k.ext, &op, trace, k.scope(Some(i + 1), peers, outer))
                })
                .0
            };
            let answer: Result<Value, String> = if !as_record {
                Ok(walk(v, &read))
            } else {
                match v {
                    // the keys: a projector's bare key, or the
                    // projection written bare at that place
                    Value::List(items) => {
                        let mut fields: Vec<(String, Value)> = Vec::with_capacity(items.len());
                        let mut err = None;
                        for (j, item) in items.iter().enumerate() {
                            let key = match (item, written.get(j)) {
                                (Value::Projection { key, .. }, _) => Some(key.clone()),
                                (_, Some(Some((_, key)))) => Some(key.clone()),
                                _ => None,
                            };
                            let Some(key) = key else {
                                err = Some(format!(
                                    "project(--record): item {} of the list is no projector ({}), so it has no key",
                                    j + 1,
                                    item.display_form()
                                ));
                                break;
                            };
                            if fields.iter().any(|(f, _)| *f == key) {
                                let spelled = |x: &Value, w: Option<&Option<(u8, String)>>| match (x, w) {
                                    (Value::Projection { ladder, key, .. }, _) => Value::projection_spelling(*ladder, key),
                                    (_, Some(Some((l, k)))) => Value::projection_spelling(*l, k),
                                    _ => String::new(),
                                };
                                let first = items
                                    .iter()
                                    .enumerate()
                                    .find(|(_, x)| match x {
                                        Value::Projection { key: k2, .. } => *k2 == key,
                                        _ => false,
                                    })
                                    .map(|(j2, x)| spelled(x, written.get(j2)))
                                    .or_else(|| {
                                        written.iter().find_map(|w| match w {
                                            Some((l, k2)) if *k2 == key => Some(Value::projection_spelling(*l, k2)),
                                            _ => None,
                                        })
                                    })
                                    .unwrap_or_default();
                                err = Some(format!(
                                    "project(--record): two projections share the bare key '{key}' ({first} and {}); the record keys by it",
                                    spelled(item, written.get(j))
                                ));
                                break;
                            }
                            fields.push((key, walk(item.clone(), &read)));
                        }
                        match err {
                            Some(e) => Err(e),
                            None => Ok(Value::Record(fields.into())),
                        }
                    }
                    Value::Record(_) => Err(
                        "project(--record) keys a list by its projections' bare keys; the record has its keys already"
                            .into(),
                    ),
                    other => Err(format!(
                        "project(--record) keys a list by its projections' bare keys, and {} is no list",
                        stdlib::kind_of(&other)
                    )),
                }
            };
            match answer {
                Ok(value) => {
                    k.topic = Some(value);
                    k.prov = Prov::leaf(o.clone());
                    out.push(k);
                }
                Err(e) => record_refusal(e),
            }
        }
    }
    out
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
                value: Value::Record(fields.into()),
            });
            c
        })
        .collect()
}

/// The order and selection family (`sort_by`, `reverse`, `first`, …): capsa-preserving, the effective topic baked in. `apply_command`'s arm.
/// `@| collapse`: one thread per distinct node among the context's
/// threads, in order of first arrival; its `@-` the union of their
/// arriving edges, its capsa the gathering the merge used to make
/// (equal regulae kept once, differing ones gathered as lists);
/// `collapse(-e)` keeps threads apart by arriving edge, so it
/// collapses only what differs by route alone (spec: Expansion
/// paths).
pub(crate) fn collapse_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
) -> Vec<Thread<H::Ext>> {
    if let Some(c) = caps.iter().find(|c| c.topic.is_some()) {
        record_refusal(format!(
            "collapse gathers the threads standing on nodes, and the context holds values ({}): \
             collapse before projecting",
            c.topic.as_ref().map_or(Value::Null, stdlib::type_of)
        ));
        return Vec::new();
    }
    let by_edge = call
        .args
        .iter()
        .any(|a| matches!(a, Arg::Opt { name, .. } if name == "-e" || name == "--edge"));
    let mut out: Vec<Thread<H::Ext>> = Vec::new();
    let mut index: HashMap<(<H::Ext as Attachment>::Key, Option<String>), usize> = HashMap::new();
    for c in caps {
        let key = (
            c.ext.key(),
            if by_edge { c.ext.crossing_key() } else { None },
        );
        match index.get(&key) {
            Some(&i) => {
                let kept = &mut out[i];
                kept.prov.origins.union(&c.prov.origins);
                kept.ext.gather(c.ext);
                // the capsa: equal regulae once, differing ones
                // gathered as a list of the distinct values
                for (a, b) in kept.register.iter_mut().zip(c.register) {
                    a.prov.origins.union(&b.prov.origins);
                    if a.value == b.value {
                        continue;
                    }
                    let mut items = match a.value.clone().into_items() {
                        Some(items) => items,
                        None => vec![a.value.clone()],
                    };
                    if !items.contains(&b.value) {
                        items.push(b.value);
                    }
                    a.value = Value::list(items);
                }
            }
            None => {
                index.insert(key, out.len());
                out.push(c);
            }
        }
    }
    let _ = host;
    out
}

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
    // One table in the context: the ordering reads its rows (spec:
    // The Table), `sort` refusing with the pointer at `sort_by`.
    if caps.len() == 1 && matches!(caps[0].topic, Some(Value::Table(_))) {
        let mut caps = caps;
        let scale = |e: &str| host.scale_of(e);
        let mut c = caps.pop().unwrap();
        let v = match &c.topic {
            Some(Value::Table(t)) => stdlib::table_order_reading(call, t, &scale),
            _ => None,
        };
        c.topic = Some(match v {
            Some(v) => v,
            None => {
                let mut r = stdlib::apply(call, vec![c.topic.take().unwrap()], &scale);
                r.pop().unwrap_or(Value::Null)
            }
        });
        return vec![c];
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
    let topic_of = |c: &Thread<H::Ext>| c.topic.clone().unwrap_or_else(|| host.node_scalar(&c.ext));
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
        _ => {}
    }
    // `sort --desc` (`-d`): the whole order flipped.
    if call.name == "sort" && stdlib::desc_flag(call) {
        caps.reverse();
    }
    match call.name.as_str() {
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
                    Arg::Lit(v) | Arg::Key(v) => v.to_string(),
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
            // A predicate on a table value filters its rows (spec: The
            // Table): each row is tested as the cursor record (`:Age`
            // its cell), and the rows that hold answer as a table
            // under the same header; a table of none is empty, never
            // the whole table passed through.
            if let Some(Value::Table(t)) = &c.topic {
                let t = t.clone();
                let mut kept: Vec<Vec<Value>> = Vec::new();
                for row in &t.rows {
                    let mut probe = c.clone();
                    // the row as the cursor record: keyed by the
                    // header, or by position on the array face (as
                    // `| %` keys an array's columns)
                    probe.topic = Some(Value::Record(match &t.header {
                        Some(h) => h.iter().cloned().zip(row.iter().cloned()).collect(),
                        None => row
                            .iter()
                            .cloned()
                            .enumerate()
                            .map(|(j, v)| ((j + 1).to_string(), v))
                            .collect(),
                    }));
                    let scope = probe.scope(Some(i + 1), peers, outer);
                    if eval_pred_expr(host, &probe.ext, cond, trace, scope) {
                        kept.push(row.clone());
                    }
                }
                c.topic = Some(Value::table(t.with_rows(kept)));
                return Some(c);
            }
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
    if call.name == "copy" {
        record_refusal(
            "copy stands last and is the tool's: the host that prints the output applies it; this host does not copy, or the command stood inside a body"
                .into(),
        );
        return Vec::new();
    }
    // `@| csv(title; year)` / `@| pretty(…)` over nodes: the
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
    // A key written with a glued minus (`-::fare`, `Operand::Desc`)
    // descends on that key alone, whatever its type; under `--desc`
    // (sort_by) such a key ascends. The key itself is the inner
    // projection.
    let desc_all = call.name == "sort_by" && stdlib::desc_flag(call);
    let dirs: Vec<bool> = key_args
        .iter()
        .filter_map(|a| match a {
            Arg::Expr(e) => Some(matches!(e, Operand::Desc(_)) != desc_all),
            Arg::Lit(Value::Quarq(_)) => Some(desc_all),
            _ => None,
        })
        .collect();
    // a quarq written as a key (`sort_by(q(::fare))`) is applied on
    // the capsa, as `sort_by(&$.key)` splices a held one; a bare
    // register is its value (spec: Quarqs)
    let key_of = |c: &Thread<H::Ext>, i: usize| -> Vec<Value> {
        let scope = c.scope(Some(i + 1), peers, outer);
        key_args
            .iter()
            .filter_map(|a| match a {
                Arg::Expr(e) => {
                    let inner = match e {
                        Operand::Desc(inner) => inner.as_ref(),
                        other => other,
                    };
                    let v = operand_scalar(host, &c.ext, inner, trace, scope);
                    Some(if matches!(inner, Operand::Lit(Value::Quarq(_))) {
                        crate::pipeline::operand::apply_held(host, &c.ext, v, trace, scope)
                    } else {
                        v
                    })
                }
                Arg::Lit(Value::Quarq(q)) => {
                    let held = q
                        .projector
                        .as_deref()
                        .cloned()
                        .unwrap_or_else(|| Value::Quarq(q.clone()));
                    Some(crate::pipeline::operand::apply_held(
                        host, &c.ext, held, trace, scope,
                    ))
                }
                Arg::Lit(_)
                | Arg::Key(_)
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
            .zip(dirs.iter())
            .map(|((x, y), down)| {
                let o = x.compare_with(y, &|e| host.scale_of(e));
                if *down { o.reverse() } else { o }
            })
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

    // One table under a keyed aggregate ranks its ROWS (spec: The
    // Table): each row is keyed as the cursor record (`:Age` its
    // cell, as the predicate reads it), and the rows that come out
    // answer as a table under the same header on the same thread.
    if caps.len() == 1 && matches!(caps[0].topic, Some(Value::Table(_))) && !counting {
        let mut caps = caps;
        let mut c = caps.pop().unwrap();
        let t: crate::Table = match c.topic.take() {
            Some(Value::Table(t)) => (*t).clone(),
            _ => unreachable!(),
        };
        // `sort_by(:::index)` keys the rows by their labels: the
        // index column stands for the descriptor; a table without an
        // index refuses, naming the switch (and the `| #` that drops
        // an index on the way)
        let names_index = call.args.iter().any(|a| {
            matches!(a, Arg::Expr(Operand::Rel {
                steps,
                projection: Some(Projection::AdapterMeta(k)),
                anchor: Anchor::Current,
            }) if steps.is_empty() && k == "index")
        });
        if names_index && t.index == 0 {
            crate::pipeline::record_refusal(format!(
                "{}(:::index) orders the rows by their labels, and the table has no index: \
                 declare one with '#(-i=:col)' (a '| #' on the way drops it)",
                call.name
            ));
            return Vec::new();
        }
        let call_by_label: Option<FnCall> = (t.index > 0).then(|| {
            let mut call = call.clone();
            let mut args = Vec::with_capacity(call.args.len() + t.index);
            for a in call.args.drain(..) {
                if let Arg::Expr(Operand::Rel {
                    steps,
                    projection: Some(Projection::AdapterMeta(k)),
                    anchor: Anchor::Current,
                }) = &a
                    && steps.is_empty()
                    && k == "index"
                {
                    // every level of the index, outer first
                    for j in 0..t.index {
                        args.push(Arg::Expr(Operand::Field {
                            base: Box::new(Operand::Topic),
                            name: t.column_name(j),
                        }));
                    }
                } else {
                    args.push(a);
                }
            }
            call.args = args;
            call
        });
        let call = call_by_label.as_ref().unwrap_or(call);
        let probes: Vec<Thread<H::Ext>> = t
            .rows
            .iter()
            .map(|row| {
                let mut probe = c.clone();
                probe.topic = Some(Value::Record(match &t.header {
                    Some(h) => h.iter().cloned().zip(row.iter().cloned()).collect(),
                    None => row
                        .iter()
                        .cloned()
                        .enumerate()
                        .map(|(j, v)| ((j + 1).to_string(), v))
                        .collect(),
                }));
                probe
            })
            .collect();
        let rows: Vec<Vec<Value>> = keyed_agg(call, probes, host, trace, outer, peers, false)
            .into_iter()
            .map(|p| match p.topic {
                Some(Value::Record(fields)) => fields.into_iter().map(|(_, v)| v).collect(),
                _ => Vec::new(),
            })
            .collect();
        c.topic = Some(Value::table(t.with_rows(rows)));
        return vec![c];
    }

    let mut keyed: Vec<(Vec<Value>, Thread<H::Ext>)> = caps
        .into_iter()
        .enumerate()
        .map(|(i, c)| (key_of(&c, i), c))
        .collect();
    match call.name.as_str() {
        // A null key sorts last, and a NaN key after every number
        // whichever way the key runs (`-$_` of NaN is NaN); `--desc`
        // (`-d`) flips the whole key list, a unary minus one key.
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

/// The settings a `session(…)` call carries: the option names with
/// their literal values (none for `session` alone).
pub(crate) fn session_settings(call: &FnCall) -> Vec<(String, Value)> {
    call.args
        .iter()
        .filter_map(|a| match a {
            Arg::Opt {
                name,
                value: Some(Operand::Lit(v)),
            } => Some((name.clone(), v.clone())),
            Arg::Opt { name, value: None } => Some((name.clone(), Value::Null)),
            _ => None,
        })
        .collect()
}
