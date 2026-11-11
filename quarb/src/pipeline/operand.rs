//! The operand evaluator: a value expression at a thread, generic
//! over the host. Every arm that is a value computes here; the arms
//! that name a node (a relative path, a context reach, the arrived
//! edges, the peers as nodes) ask the host.
use crate::pipeline::*;
use crate::syntax::ast::IndexSpec;

/// The most values one expression may fork: the members of a range,
/// or the direct product of two forking operands. Each is a thread,
/// and a count read from the query sized the allocation with no
/// bound (`= (1..300000) + (1..300000) @| count` aborted the
/// process). Reaching it is a refusal, as every bound's is.
pub const MAX_FORK: usize = 10_000_000;

/// The refusal of a direct product past [`MAX_FORK`], if it is.
fn product_past_bound(left: usize, right: usize) -> Option<String> {
    match left.checked_mul(right) {
        Some(n) if n <= MAX_FORK => None,
        _ => Some(format!(
            "the product of {left} and {right} values is past the {MAX_FORK} one expression may fork"
        )),
    }
}

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
    // A chain written flat, `1 + 1 + …`, is a tree as deep as it is
    // long, and this function's frame is large: 2,200 terms overflowed
    // the stack of a release build and 150 that of a debug one (the
    // code review of 2026-11-10). The stack grows into a heap segment
    // where it runs short; where it cannot be measured (wasm) the
    // evaluation runs in place.
    stacker::maybe_grow(1024 * 1024, 16 * 1024 * 1024, || {
        eval_operand_in(host, ext, operand, trace, scope)
    })
}

fn eval_operand_in<H: Host>(
    host: &H,
    ext: &H::Ext,
    operand: &Operand,
    trace: &H::Trace,
    scope: Scope<'_, H::Ext>,
) -> Vec<Value> {
    match operand {
        // a quarq that spells a projector answers the projector
        // (spec: Quarqs); any other literal is itself
        Operand::Lit(Value::Quarq(q)) if q.projector.is_some() => {
            vec![q.projector.as_deref().cloned().expect("checked")]
        }
        // a quarq template fills where it is evaluated: once per
        // thread, in this scope (design paper: the quasiquote)
        Operand::Lit(Value::Quarq(q)) if q.is_template() => {
            vec![crate::pipeline::template::fill_template(
                host, ext, q, trace, scope,
            )]
        }
        Operand::Lit(v) => vec![v.clone()],
        // an expansion outside a template never evaluates: the
        // parser refuses it, the fill replaces it
        Operand::Hole(h) => {
            record_refusal(format!(
                "expansion '${{{}}}' stands outside a quarq template",
                h.text
            ));
            vec![Value::Null]
        }
        // A pattern literal is not a value: the grammar admits it
        // only as a comparison's right operand, where the Compare
        // arm intercepts it before operand evaluation.
        Operand::Pattern(_) => Vec::new(),
        // The switch: scrutinee once, arms in order, first hit
        // wins, only the taken branch evaluates. A regex arm tests
        // like `=~`; a value arm compares by the standard equality.
        Operand::Match {
            scrutinee,
            arms,
            other,
        } => {
            let subject = discard(|| operand_scalar(host, ext, scrutinee, trace, scope));
            if matches!(subject, Value::Bool(_)) {
                record_refusal(
                    "the switch compares a VALUE, and its scrutinee is a boolean: \
                     the plain conditional is '(cond ? a : b)'"
                        .to_string(),
                );
                return vec![Value::Null];
            }
            for (test, regex, result) in arms {
                // A pattern arm that matches binds its groups for its
                // own result (ruling 2026-11-10): in
                // `(x ?= (/re(\d+)/) ? $1 : else)` the `$1` reads this
                // arm's match. The scope is the result's alone: the
                // thread's captures stay as they were, and the next
                // arm and the else see none of these.
                let mut groups: Option<Captured> = None;
                let hit = discard(|| {
                    if *regex {
                        let pat = operand_scalar(host, ext, test, trace, scope);
                        let hit = regex_test(&subject, &pat, true);
                        if hit {
                            groups = compiled_regex(&pat.to_string())
                                .filter(|re| re.captures_len() > 1)
                                .and_then(|re| {
                                    let text = subject.to_string();
                                    re.captures(&text).map(|caps| {
                                        let numbered = caps
                                            .iter()
                                            .skip(1)
                                            .map(|g| {
                                                g.map_or(String::new(), |m| m.as_str().to_string())
                                            })
                                            .collect();
                                        let named = re
                                            .capture_names()
                                            .enumerate()
                                            .filter_map(|(i, n)| {
                                                n.map(|n| {
                                                    let text =
                                                        caps.get(i).map_or(String::new(), |m| {
                                                            m.as_str().to_string()
                                                        });
                                                    (n.to_string(), text)
                                                })
                                            })
                                            .collect();
                                        (numbered, named)
                                    })
                                });
                        }
                        hit
                    } else {
                        let t = operand_scalar(host, ext, test, trace, scope);
                        value_eq(&subject, &t, &|e| host.scale_of(e))
                    }
                });
                if hit {
                    return match &groups {
                        Some((numbered, named)) => {
                            let arm = Scope {
                                captures: numbered,
                                named,
                                ..scope
                            };
                            eval_operand(host, ext, result, trace, arm)
                        }
                        None => eval_operand(host, ext, result, trace, scope),
                    };
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
            // On a node of a reflected query (the expansion arbor,
            // a .quarb file) the topic of a node thread is the node
            // itself, as the quarq of its form (design paper: a
            // reflected node reads as the quarq of its subtree) —
            // `$_ ++! %(…)`, `syntax:node($_; …)`, `… ?? $_`.
            vec![
                scope
                    .topic
                    .cloned()
                    .or_else(|| with_run(|s| s.enclosing_topic.last().cloned()))
                    .or_else(|| host.node_form(ext).map(Value::quarq_form))
                    .unwrap_or(Value::Null),
            ]
        }
        // `base:name` — a record's field; null on a non-record. A
        // field of a recalled record keeps that field's origins.
        Operand::Field { base, name, .. } if matches!(base.as_ref(), Operand::Recall(_)) => {
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
                // a table's column as the table answers it (the
                // record keyed by the labels on an indexed table); a
                // quarq's own core field (`$.f:unq`)
                t @ (Value::Table(_) | Value::Quarq(_)) => {
                    crate::pipeline::commands::record_field(t, name)
                }
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
        // A field read on a list of records reads each: a list of
        // records projects to the records' values, so `(@_:n) @| sum`
        // sums a field over the members.
        // A leading `:name` on a thread (spec: Field Projections,
        // draft 164), by the topic: a RECORD → its field, null when
        // absent; a LIST → the field of each item as a list (node
        // items their core field); the NODE (no value topic) → the
        // core field, an unknown key refused; any OTHER value (text,
        // number, null, …) → the POSITION's core field, since the
        // thread still stands at a node, a key that is no core
        // field answering null without a refusal (`| :name |
        // indent(:depth - 1)`, `bibref(cmos) | :genus`); a QUARQ
        // answers its own core field first (`:unq`, the canonical
        // text `--expand` prints).
        // Under a row in reach (ruling 2026-11-05: a table projector's
        // expression item, `map`'s body over a table) the bare `:name`
        // is the row's cell in that column, whatever the topic — the
        // table the item stands on, the cell being mapped; `$_:name`
        // written out keeps the topic's reading. A thread of its own
        // record or list (`($_ | sort_by(:Age))`) reads that, as
        // ever. A name no column carries is refused naming them.
        Operand::Field {
            base,
            name,
            bare: true,
        } if matches!(**base, Operand::Topic)
            && !matches!(scope.topic, Some(Value::Record(_) | Value::List(_)))
            && let Some(read) = row_field(name) =>
        {
            vec![match read {
                Ok(v) => v,
                Err(columns) => {
                    record_refusal(format!(
                        "no column '{name}' (the columns: {})",
                        columns.join(", ")
                    ));
                    Value::Null
                }
            }]
        }
        // `:name` with a node reference as the topic reads the node the
        // reference names (`map(:name)` over a route); an unknown
        // field is refused there as on a path (finding 2026-11-05:
        // `$$_:%(:Age)` answered null where `/row:%(:Age)` refused)
        Operand::Field { base, name, .. }
            if matches!(**base, Operand::Topic)
                && scope.topic.is_some_and(|t| matches!(t, Value::Node(_))) =>
        {
            let topic = scope.topic.cloned().unwrap_or(Value::Null);
            vec![through(host, &topic, &Projection::CoreMeta(name.clone()))]
        }
        Operand::Field { base, name, .. }
            if matches!(**base, Operand::Topic) && scope.topic.is_none() =>
        {
            note(host.origin_at(ext).position);
            vec![host.project(ext, &Projection::CoreMeta(name.clone()))]
        }
        Operand::Field { base, name, .. }
            if matches!(**base, Operand::Topic)
                && !matches!(
                    scope.topic,
                    Some(Value::Record(_) | Value::List(_) | Value::Table(_))
                ) =>
        {
            let own = scope.topic.and_then(|t| match t {
                Value::Quarq(q) => q.core_field(name),
                _ => None,
            });
            if let Some(v) = own {
                vec![v]
            } else if crate::graph::navigate::CORE_FIELDS.contains(&name.as_str()) {
                note(host.origin_at(ext).position);
                vec![host.project(ext, &Projection::CoreMeta(name.clone()))]
            } else {
                vec![Value::Null]
            }
        }
        Operand::Field { base, name, .. } => eval_operand(host, ext, base, trace, scope)
            .into_iter()
            .map(|v| match &v {
                // a core field read through the node a value names;
                // an unknown field is refused as it is on a path,
                // naming the core fields (finding 2026-11-05: glued to
                // a reference, `:Age` answered null where the path
                // form refused)
                Value::Node(_) | Value::List(_)
                    if !crate::graph::navigate::ref_nodes(&v).is_empty() =>
                {
                    through(host, &v, &Projection::CoreMeta(name.clone()))
                }
                _ => record_field(v, name),
            })
            .collect(),
        Operand::Through { base, projection } => eval_operand(host, ext, base, trace, scope)
            .into_iter()
            .map(|v| through(host, &v, projection))
            .collect(),
        // `t[[x]]`, `t[[x; y]]`, `t[[a..b]]`, `r[[k]]` — the lookup by
        // label (spec: Indexing a Value, draft 166)
        Operand::LabelIndex { base, labels } => {
            let specs: Vec<LabelSpec> = labels
                .iter()
                .flat_map(|l| match l {
                    Operand::Range {
                        from,
                        to,
                        step: None,
                    } => vec![LabelSpec::Range(
                        operand_value(host, ext, from, trace, scope),
                        operand_value(host, ext, to, trace, scope),
                    )],
                    other => match operand_value(host, ext, other, trace, scope) {
                        Value::List(items) => vec![LabelSpec::Many(items.to_vec())],
                        v => vec![LabelSpec::One(v)],
                    },
                })
                .collect();
            eval_operand(host, ext, base, trace, scope)
                .into_iter()
                .map(|v| match label_lookup(v, &specs, &|e| host.scale_of(e)) {
                    Ok(v) => v,
                    Err(why) => {
                        crate::pipeline::record_refusal(why);
                        Value::Null
                    }
                })
                .collect()
        }
        // `v:@(…)`, `v:%(…)`, `t:#(…)`, `v:(…)`, `v:$.proj` — the
        // projection operator (spec: The Projection Operator): the
        // shape read on each value (or node) of the base, every
        // projector in it replaced by its reading there.
        // `v&$.f`, `v&$.f!`, `v&$.f#` — the glued recall: the pipe
        // form `v | &$.f` as one operand (spec: Quarqs, the held
        // quarq's recalls)
        Operand::Apply { base, reg, shot } => {
            let command = match shot {
                Some(shot) => Command::SpliceAs {
                    reg: reg.clone(),
                    shot: *shot,
                },
                None => Command::Splice(reg.clone()),
            };
            let piped = Operand::Piped {
                expr: base.clone(),
                commands: vec![command],
            };
            eval_operand(host, ext, &piped, trace, scope)
        }
        Operand::Projected { base, shape } => {
            use crate::syntax::ast::ProjShape;
            // the probes: a path's nodes (each with its projection,
            // if any, as the topic), else each value of the base on
            // the standing node
            let probes: Vec<(H::Ext, Option<Value>)> = match base.as_ref() {
                Operand::Rel {
                    steps,
                    projection,
                    anchor,
                } => crate::pipeline::run::out_of_row_reach(|| {
                    host.rel_threads(ext, steps, anchor, projection.as_ref(), trace, scope)
                })
                .into_iter()
                .map(|(x, topic, _)| (x, topic))
                .collect(),
                other => eval_operand(host, ext, other, trace, scope)
                    .into_iter()
                    .map(|v| (ext.clone(), Some(v)))
                    .collect(),
            };
            let mut out = Vec::new();
            for (x, topic) in &probes {
                let pscope = Scope {
                    topic: topic.as_ref(),
                    prov: None,
                    members: &[],
                    ..scope
                };
                let read =
                    |ladder: u8, key: &str| read_projection(host, x, ladder, key, trace, pscope);
                match shape {
                    ProjShape::Value(v) => {
                        // a record shape builds its fields on the
                        // probe itself (a node reads its core fields
                        // there), not in a sub-capsa that would take
                        // the enclosing topic; on a list of records
                        // it reads each item (ruling 2026-11-01)
                        let values = match v.as_ref() {
                            Operand::Piped { expr, commands }
                                if matches!(**expr, Operand::Topic)
                                    && let [Command::Func(call)] = commands.as_slice()
                                    && matches!(call.name.as_str(), "rec" | "record") =>
                            {
                                let build = |pscope: Scope<'_, H::Ext>| match record_build(
                                    &call.args, host, x, trace, pscope,
                                ) {
                                    Ok((r, _)) => Value::Record(r),
                                    Err(why) => {
                                        crate::pipeline::record_refusal(why);
                                        Value::Null
                                    }
                                };
                                match topic {
                                    Some(Value::List(items))
                                        if !items.is_empty()
                                            && items
                                                .iter()
                                                .all(|i| matches!(i, Value::Record(_))) =>
                                    {
                                        vec![Value::list(
                                            items
                                                .iter()
                                                .map(|item| {
                                                    build(Scope {
                                                        topic: Some(item),
                                                        ..pscope
                                                    })
                                                })
                                                .collect(),
                                        )]
                                    }
                                    _ => vec![build(pscope)],
                                }
                            }
                            other => eval_operand(host, x, other, trace, pscope),
                        };
                        // a held register projects and nothing else
                        // (spec: Projection by the Glued Colon): a
                        // quarq that is one projection or a projector
                        // structure, or a projector structure itself;
                        // anything else refuses pointing at the sigil
                        let held = matches!(v.as_ref(), Operand::Recall(_));
                        for value in values {
                            out.push(match value {
                                Value::Quarq(q) if held => match &q.projector {
                                    Some(p) => walk_projections((**p).clone(), &read),
                                    None => {
                                        crate::pipeline::record_refusal(
                                            "the colon projects; to run the quarq write v | &$.f (or v&$.f)"
                                                .into(),
                                        );
                                        Value::Null
                                    }
                                },
                                v @ (Value::Str(_) | Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Null) if held => {
                                    crate::pipeline::record_refusal(format!(
                                        "the colon projects, and the regula holds {} with no projector in it; to run a quarq write v | &$.f",
                                        crate::stdlib::kind_of(&v)
                                    ));
                                    Value::Null
                                }
                                // a held quarq runs on the probe as
                                // the glued colon applies it
                                Value::Quarq(q) => run_quarq(host, x, &q, trace, pscope),
                                v => walk_projections(v, &read),
                            });
                        }
                    }
                    // `#(:A; :B)` — the table of the named columns:
                    // on a table the columns side by side, on a node
                    // or a record one row of the readings
                    ProjShape::Columns {
                        items,
                        index,
                        autoindex,
                        casts,
                    } => {
                        let mut header: Vec<(String, Option<crate::ColumnType>)> =
                            Vec::with_capacity(items.len());
                        let mut columns = Vec::with_capacity(items.len());
                        let mut refused = false;
                        // a table projector on an indexed table KEEPS
                        // the index (the labels are the rows'
                        // identity), unless an item names its column
                        // or `-i` names another
                        let named = |items: &[(Option<String>, Operand)], k: &str| {
                            items.iter().any(|(n, o)| {
                                n.as_deref() == Some(k)
                                    || crate::syntax::ast::auto_field_name(o) == Some(k)
                            })
                        };
                        // `-i` read first: the base's index columns stay
                        // where no other index is named, or where the
                        // new index names them (`-i=(:second; :first)`
                        // re-nests the levels)
                        let new_index: Option<
                            Result<Vec<(String, Option<crate::ColumnType>)>, String>,
                        > = index.as_ref().map(|op| {
                            match operand_value(host, x, op, trace, pscope) {
                                v @ (Value::Projection { .. } | Value::List(_) | Value::Str(_)) => {
                                    Ok(crate::stdlib::index_levels(&v))
                                }
                                other => Err(format!(
                                    "--index names a column with a projector (-i=:col), not {}",
                                    crate::stdlib::kind_of(&other)
                                )),
                            }
                        });
                        if let Some(Value::Table(t)) = topic
                            && t.index > 0
                        {
                            for j in 0..t.index {
                                let k = t.column_name(j);
                                let wanted = match &new_index {
                                    None => true,
                                    Some(Ok(levels)) => levels.iter().any(|(n, _)| *n == k),
                                    Some(Err(_)) => false,
                                };
                                if wanted && !named(items, &k) {
                                    header.push((k.clone(), t.declaration(j)));
                                    columns.push(t.column(&k).unwrap_or(Value::Null));
                                }
                            }
                        }
                        // what `:*` leaves out (ruling 2026-11-05): the
                        // columns the items produce (bare `:Age`, the key
                        // of `name = expr`) and the ones a rename takes
                        // whole (`new = :old`); a column only read inside
                        // an expression keeps its place under `:*`
                        let taken: Vec<String> = items
                            .iter()
                            .flat_map(|(n, o)| {
                                let o = match o {
                                    Operand::Typed { inner, .. } => inner.as_ref(),
                                    other => other,
                                };
                                let mut v = Vec::new();
                                if let Some(k) = n {
                                    v.push(k.clone());
                                }
                                if let Operand::Field { base, name, .. } = o
                                    && matches!(**base, Operand::Topic)
                                    && name != "*"
                                {
                                    v.push(name.clone());
                                }
                                v
                            })
                            .collect();
                        let is_rest = |o: &Operand| matches!(o, Operand::Field { base, name, .. } if matches!(**base, Operand::Topic) && name == "*");
                        for (name, item) in items {
                            // `#($.h...)` — the spread: every projector
                            // the held list holds is a column, keyed
                            // and declared as it is (draft 166)
                            if let Operand::Spread(inner) = item {
                                let held = operand_value(host, x, inner, trace, pscope);
                                let projectors = match held {
                                    Value::List(l) => l.to_vec(),
                                    p @ Value::Projection { .. } => vec![p],
                                    other => {
                                        crate::pipeline::record_refusal(format!(
                                            "the spread in a table shape takes a list of projectors, not {}",
                                            crate::stdlib::kind_of(&other)
                                        ));
                                        refused = true;
                                        break;
                                    }
                                };
                                for p in projectors {
                                    let Value::Projection {
                                        ladder,
                                        key,
                                        typing,
                                    } = p
                                    else {
                                        crate::pipeline::record_refusal(format!(
                                            "the spread in a table shape takes projectors; {} is none",
                                            p.display_form()
                                        ));
                                        refused = true;
                                        break;
                                    };
                                    header.push((
                                        key.clone(),
                                        typing.as_ref().and_then(crate::Typing::declared),
                                    ));
                                    columns.push(read(ladder, &key));
                                }
                                if refused {
                                    break;
                                }
                                continue;
                            }
                            // `:*` — the rest: the base's data columns the
                            // items neither produce nor take, in the
                            // table's order, where it stands (ruling
                            // 2026-11-05)
                            if name.is_none() && is_rest(item) {
                                match topic {
                                    Some(Value::Table(t)) => {
                                        for j in t.index..t.width() {
                                            let k = t.column_name(j);
                                            if !taken.contains(&k) {
                                                header.push((k.clone(), t.declaration(j)));
                                                columns.push(t.column(&k).unwrap_or(Value::Null));
                                            }
                                        }
                                    }
                                    _ => {
                                        crate::pipeline::record_refusal(
                                            "':*' stands in a projector over a table (t:#(:*; …)) or a record (r:%(:*; …)): the rest of its columns or fields".into(),
                                        );
                                        refused = true;
                                        break;
                                    }
                                }
                                continue;
                            }
                            // a typed item declares its column
                            let (item, declared) = match item {
                                Operand::Typed { inner, typing } => {
                                    (inner.as_ref(), typing.declared())
                                }
                                other => (other, None),
                            };
                            let key = name.clone().or_else(|| {
                                crate::syntax::ast::auto_field_name(item).map(str::to_string)
                            });
                            let Some(key) = key else {
                                crate::pipeline::record_refusal(
                                    "the table shape names its columns: a bare projection (:A) or 'name = expr'".into(),
                                );
                                refused = true;
                                break;
                            };
                            header.push((key, declared));
                            // on a table a bare `:name` is the column
                            // as a list, whatever record the indexed
                            // table answers for it elsewhere; any other
                            // item is evaluated per row (ruling
                            // 2026-11-05), the row in reach of its bare
                            // `:name` reads, `$_` the table still
                            let column = match (topic, item) {
                                (Some(Value::Table(t)), Operand::Field { base, name, .. })
                                    if matches!(**base, Operand::Topic) =>
                                {
                                    // a column the table lacks is refused naming it
                                    // and the columns, as map refuses (lang's finding
                                    // 2026-11-06), never a fold of the table
                                    match t.column(name) {
                                        Some(c) => c,
                                        None => {
                                            crate::pipeline::record_refusal(format!(
                                                "no column '{name}' (the columns: {})",
                                                (0..t.width())
                                                    .map(|j| t.column_name(j))
                                                    .collect::<Vec<_>>()
                                                    .join(", ")
                                            ));
                                            refused = true;
                                            break;
                                        }
                                    }
                                }
                                (Some(Value::Table(t)), item) => Value::list(
                                    (1..=t.n_rows())
                                        .map(|i| {
                                            let row = t.row(i).unwrap_or(Value::Null);
                                            with_row_in_reach(row, || {
                                                walk_projections(
                                                    operand_value(host, x, item, trace, pscope),
                                                    &read,
                                                )
                                            })
                                        })
                                        .collect(),
                                ),
                                _ => walk_projections(
                                    operand_value(host, x, item, trace, pscope),
                                    &read,
                                ),
                            };
                            columns.push(column);
                        }
                        // a name twice in the header — an item beside what
                        // `:*` expanded, or two items — is a duplicate field
                        if !refused
                            && let Some((k, _)) = header
                                .iter()
                                .enumerate()
                                .find(|(i, (k, _))| header[..*i].iter().any(|(h, _)| h == k))
                                .map(|(_, h)| h)
                        {
                            crate::pipeline::record_refusal(format!(
                                "'{k}' is already a column of the projector; a name stands once in a header"
                            ));
                            refused = true;
                        }
                        if refused {
                            out.push(Value::Null);
                            continue;
                        }
                        let all_lists = !columns.is_empty()
                            && columns.iter().all(|c| matches!(c, Value::List(_)));
                        let rows: Vec<Vec<Value>> = if all_lists {
                            let lists: Vec<Vec<Value>> = columns
                                .iter()
                                .map(|c| c.clone().into_items().unwrap_or_default())
                                .collect();
                            let n = lists.iter().map(Vec::len).max().unwrap_or(0);
                            (0..n)
                                .map(|i| {
                                    lists
                                        .iter()
                                        .map(|l| l.get(i).cloned().unwrap_or(Value::Null))
                                        .collect()
                                })
                                .collect()
                        } else {
                            vec![columns]
                        };
                        let kept = matches!(topic, Some(Value::Table(t)) if t.index > 0)
                            && index.is_none();
                        // the columns of a table whose names are paths of
                        // levels are still such paths
                        let levels = matches!(topic, Some(Value::Table(t)) if t.levels);
                        let built = crate::Table::declared(header, rows)
                            .map(|t| t.with_levels(levels))
                            .and_then(|t| match new_index {
                                // `-i=:A` names the index (declaring it when typed)
                                Some(Ok(levels)) => t.indexed_by_levels(&levels),
                                Some(Err(why)) => Err(why),
                                None => Ok(match topic {
                                    Some(Value::Table(base)) if kept => t.with_index(base.index),
                                    _ => t,
                                }),
                            });
                        // `-a` (ruling 2026-11-06): the positions prepended
                        // as the index, never beside `-i`
                        let built = built.and_then(|t| {
                            if !*autoindex {
                                return Ok(t);
                            }
                            if index.is_some() {
                                return Err("#(…; -a -i=:col) names two indexes, and a table has one: -a prepends the positions as the index, -i=:col makes a column the index".into());
                            }
                            crate::stdlib::autoindex_bare(&t)
                        });
                        // `-c=:col<type>` (ruling 2026-11-06): the columns cast
                        // after the build, as map(<type>; --to=:col) casts them
                        let built = built.and_then(|t| {
                            if casts.is_empty() {
                                return Ok(t);
                            }
                            let mut list: Vec<(String, crate::ColumnType)> = Vec::new();
                            for c in casts {
                                match operand_value(host, x, c, trace, pscope) {
                                    v @ (Value::Projection { .. } | Value::List(_)) => {
                                        for (name, ty) in crate::stdlib::index_levels(&v) {
                                            match ty {
                                                Some(ty) => list.push((name, ty)),
                                                None => return Err(format!("-c=:{name} names no type: -c=:{name}<int>")),
                                            }
                                        }
                                    }
                                    other => return Err(format!("--cast names a column with a typed projector (-c=:col<int>), not {}", crate::stdlib::kind_of(&other))),
                                }
                            }
                            t.cast_columns(&list)
                        });
                        out.push(match built {
                            Ok(t) => Value::table(t),
                            Err(why) => {
                                crate::pipeline::record_refusal(why);
                                Value::Null
                            }
                        });
                    }
                }
            }
            out
        }
        // a spread outside a record item reads as what it spreads
        Operand::Spread(inner) => eval_operand(host, ext, inner, trace, scope),
        // `@(a; b)` — the list literal: every value of every item. A
        // typed projector among the items is a header's or a
        // structured projector's item (draft 166): it reads its
        // projection and casts the reading.
        Operand::List(items) => vec![Value::list(
            items
                .iter()
                .flat_map(|item| match item {
                    Operand::Typed { inner, typing } => {
                        let key = crate::syntax::ast::auto_field_name(inner)
                            .unwrap_or_default()
                            .to_string();
                        eval_operand(host, ext, inner, trace, scope)
                            .into_iter()
                            .map(|v| cast_reading(v, &key, Some(typing.clone())))
                            .collect::<Vec<_>>()
                    }
                    other => eval_operand(host, ext, other, trace, scope),
                })
                .collect(),
        )],
        // `a ?? b` — the null default: the left side once, the right
        // only where the left is null (false, 0 and "" stay).
        // `a ?? b` — the null default; over a structure it
        // broadcasts one level deep, as arithmetic does (draft 166):
        // the null cells of a table, items of a list, fields of a
        // record take the default, the structure itself never being
        // null.
        Operand::Coalesce { left, right } => {
            let v = operand_value(host, ext, left, trace, scope);
            let fill = |v: &Value| -> Value {
                if matches!(v, Value::Null) {
                    operand_value(host, ext, right, trace, scope)
                } else {
                    v.clone()
                }
            };
            match v {
                Value::Null => eval_operand(host, ext, right, trace, scope),
                Value::Table(t)
                    if t.rows()
                        .flat_map(|r| r.iter())
                        .any(|c| matches!(c, Value::Null)) =>
                {
                    let rows = t
                        .rows()
                        .map(|r| r.iter().map(|v| fill(&v)).collect())
                        .collect();
                    vec![match t.rebuild(rows) {
                        Ok(t) => Value::table(t),
                        Err(why) => {
                            crate::pipeline::record_refusal(format!("??: {why}"));
                            Value::Null
                        }
                    }]
                }
                Value::List(items) if items.iter().any(|c| matches!(c, Value::Null)) => {
                    vec![Value::list(items.iter().map(fill).collect())]
                }
                Value::Record(fields) if fields.iter().any(|(_, c)| matches!(c, Value::Null)) => {
                    vec![Value::Record(
                        fields.iter().map(|(k, c)| (k.clone(), fill(c))).collect(),
                    )]
                }
                other => vec![other],
            }
        }
        // `value[i]` / `table[i; j]` — the index of a value (spec:
        // Indexing a Value); a refusal where the value has no such
        // index.
        Operand::Index { base, rows, cols } => {
            let v = operand_value(host, ext, base, trace, scope);
            // `t[cond]` / `v[cond]` (draft 166): the rows of a table or
            // the items of a list that hold, each judged as the topic,
            // as `| [cond]` judges them; a bare variable holding no
            // position is a condition judged once
            let condition: Option<Result<bool, &PredExpr>> = match rows {
                IndexSpec::Cond(e) if cols.is_none() => Some(Err(e)),
                IndexSpec::Expr(e) if cols.is_none() => {
                    let read = operand_value(host, ext, e, trace, scope);
                    match crate::pipeline::positions_of(&read) {
                        Some(_) => None,
                        None => Some(Ok(read.is_truthy())),
                    }
                }
                _ => None,
            };
            if let Some(cond) = condition {
                let holds = |topic: &Value| -> bool {
                    match cond {
                        Ok(constant) => constant,
                        Err(e) => {
                            let mut s = scope;
                            s.topic = Some(topic);
                            eval_pred_expr(host, ext, e, trace, s)
                        }
                    }
                };
                return vec![match v {
                    Value::Table(t) => {
                        let kept: Vec<Vec<Value>> = t
                            .rows()
                            .filter(|row| {
                                let record = Value::Record(match &t.header {
                                    Some(h) => h.iter().cloned().zip(row.iter()).collect(),
                                    None => row
                                        .iter()
                                        .enumerate()
                                        .map(|(j, v)| ((j + 1).to_string(), v))
                                        .collect(),
                                });
                                holds(&record)
                            })
                            .map(|r| r.cells())
                            .collect();
                        Value::table(t.keep_rows(kept))
                    }
                    Value::List(items) => {
                        Value::list(items.iter().filter(|i| holds(i)).cloned().collect())
                    }
                    Value::Record(_) => {
                        crate::pipeline::record_refusal(
                            "a record takes no condition in its brackets (it has no rows): index a field by name, '[\"age\"]', or by position".into(),
                        );
                        Value::Null
                    }
                    other => {
                        crate::pipeline::record_refusal(format!(
                            "a condition in brackets reads the rows of a table or the items of a list, not {}",
                            crate::stdlib::kind_of(&other)
                        ));
                        Value::Null
                    }
                }];
            }
            // a computed index (`[@(2; 4)]`, `[$.pos]`) is read now
            let resolve = |s: &IndexSpec| resolve_index_spec(host, ext, s, trace, scope);
            let (rows, cols) = match (resolve(rows), cols.as_ref().map(|c| resolve(c)).transpose())
            {
                (Ok(r), Ok(c)) => (r, c),
                (Err(why), _) | (_, Err(why)) => {
                    crate::pipeline::record_refusal(why);
                    return vec![Value::Null];
                }
            };
            match crate::stdlib::index_value(v, &rows, cols.as_ref()) {
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
                // every value is a thread: a range sized by a number
                // in the query must not exhaust memory
                // (`= 1..9000000000 @| count` aborted the process)
                let count = ((b as i128 - a as i128) / s as i128).max(-1) + 1;
                if count > MAX_FORK as i128 {
                    return Err(format!(
                        "the range {a}..{b} holds {count} values, past the {MAX_FORK} one expression may fork"
                    ));
                }
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
        Operand::Table {
            header,
            declared,
            rows,
            levels,
        } => {
            // a projector literal stays the projector, as a list item
            // or a record field does (a held projector table)
            let cell = |e: &Operand| match e {
                Operand::Lit(v @ Value::Projection { .. }) => v.clone(),
                _ => operand_value(host, ext, e, trace, scope),
            };
            match table_literal(header.clone(), declared, rows, cell) {
                Ok(t) => vec![Value::table(t.with_levels(*levels))],
                Err(why) => {
                    crate::pipeline::record_refusal(why);
                    vec![Value::Null]
                }
            }
        }
        // `%+` — the named groups of the last match, as a record.
        Operand::NamedCaptures => {
            let record = |named: &[(String, String)]| {
                Value::Record(
                    named
                        .iter()
                        .map(|(k, v)| (k.clone(), Value::Str(v.clone())))
                        .collect(),
                )
            };
            // a predicate that made no capture reads the thread's
            // that navigates its path
            vec![if scope.captures.is_empty() && scope.named.is_empty() {
                crate::pipeline::run::enclosing_captures(|c| {
                    record(c.map_or(&[][..], |(_, named)| named))
                })
            } else {
                record(scope.named)
            }]
        }
        // `$ord`: the thread's position in its context; in a
        // reducing command the group's ordinal, the place of the
        // group in a regula fold's order and 1 without a fold (spec:
        // the fold by a regula; ruling 2026-11-10). The scope carries
        // either: a reducing command builds its seed's scope at the
        // group's ordinal. (It fell back to a run-wide slot, which
        // answered by where `$ord` was written and reached a nested
        // pipeline's arguments.)
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
        // `X:.name` / `:.name` / `X:%.` — the node read (spec:
        // Attached Regulae): at each node `at` reaches (the current
        // node when there is no node side), what the previous
        // command left attached there — by the node's identity,
        // whoever attached it and whether anyone stands there now.
        // One value per node, in context order; a node with nothing
        // attached under the name yields none (null as a scalar,
        // nothing to an aggregate); `%.` yields the record, empty
        // where nothing is attached. The value's origins are the
        // attached regula's own (where it was read), not the node
        // it is read at: the navigation to `at` is not a read.
        Operand::PeerReg { at, reg } => {
            let keys = match at {
                Some(at) => discard(|| host.operand_keys(ext, at, trace, scope)),
                None => vec![ext.key()],
            };
            let mut out = Vec::new();
            for k in keys {
                match reg {
                    RegRef::Record => {
                        let all = attached_all(&k);
                        for r in &all {
                            note_all(&r.prov.origins);
                        }
                        out.push(Value::record(
                            all.into_iter().map(|r| (r.name, r.value)).collect(),
                        ));
                    }
                    RegRef::Named(name) => {
                        if let Some((v, prov)) = attached(&k, name) {
                            note_all(&prov.origins);
                            out.push(v);
                        }
                    }
                    _ => {}
                }
            }
            out
        }
        Operand::Topics => host.capsae(scope, None),
        Operand::Positions { projection } => host.positions(scope, projection.as_ref()),
        Operand::Matrix { node, full } => matrix(scope, *node, *full),
        Operand::CtxTable => ctx_table(host, scope),
        // A record constructor as a value (`%(y = %(n = (@@_ @|
        // count)))`) builds its fields on this very scope — the
        // context's peers in reach, as the flat record has them —
        // not in a sub-capsa of its own, which would stand alone
        // (finding 2026-11-04).
        Operand::Piped { expr, commands }
            if matches!(**expr, Operand::Topic)
                && let [Command::Func(call)] = commands.as_slice()
                && matches!(call.name.as_str(), "rec" | "record") =>
        {
            match record_build(&call.args, host, ext, trace, scope) {
                Ok((r, _)) => vec![Value::Record(r)],
                Err(why) => {
                    crate::pipeline::record_refusal(why);
                    vec![Value::Null]
                }
            }
        }
        Operand::Piped { expr, commands } => {
            // the nested pipeline stands on lone values, or on the
            // cell `map` marked it with, whatever encloses it
            let _own_cells = crate::value::ambient::cells_ground();
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
                } => crate::pipeline::run::out_of_row_reach(|| {
                    host.rel_threads(ext, steps, anchor, projection.as_ref(), trace, scope)
                })
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
                // A reducing command over a group's seed takes the
                // members together, as `| count` after the group does:
                // `%(n = count)`, `($_ | sum(--of=:x))` reduce the
                // group, never each member.
                Operand::Topic | Operand::Seed
                    if !scope.members.is_empty()
                        && matches!(commands.first(), Some(Command::Func(c)) if crate::syntax::registry::takes_of(&c.name)) =>
                {
                    let topics: Vec<Value> = scope
                        .members
                        .iter()
                        .map(|m| crate::pipeline::effective_topic(m, host))
                        .collect();
                    vec![Thread::<H::Ext> {
                        prov: scope.prov.cloned().unwrap_or_else(|| host.prov_at(ext)),
                        register: scope.register.to_vec(),
                        topic: Some(Value::list(topics)),
                        members: scope.members.to_vec(),
                        captures: scope.captures.to_vec(),
                        named: scope.named.to_vec(),
                        ext: host.restate(&scope, ext),
                    }]
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
                            Some(crate::pipeline::effective_topic(m, host)),
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
            let mut mode = crate::pipeline::commands::start_mode(&caps);
            // the nested pipeline reduces its own context: an
            // enclosing fold's group is not its commands' group
            let _own_ground = crate::pipeline::run::fresh_group();
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
                // the query). An option's expression value reads the
                // same scope (`duration("…"; --to=:a)`: `:a` is the
                // record's, not the piped literal's).
                let outer_expr = |a: &Arg| {
                    matches!(a, Arg::Expr(_))
                        || matches!(a, Arg::Opt { value: Some(v), .. } if !matches!(v, Operand::Lit(_)))
                };
                // `(:Name | split(",") | [1])` — a positional bracket
                // after a command that spread one value into several
                // picks among what it spread, as `@| [n]` reads the
                // context (ruling 2026-11-05: a projector item answers
                // as written); a bracket over one value stays that
                // value's own index
                let selecting;
                let command = match command {
                    Command::Func(call)
                        if call.name == "[]"
                            && caps.len() > 1
                            && caps.iter().all(|c| {
                                !matches!(
                                    c.topic,
                                    None | Some(
                                        Value::List(_) | Value::Table(_) | Value::Record(_)
                                    )
                                )
                            })
                            && let Some(pred) = match call.args.as_slice() {
                                [Arg::Lit(Value::Int(n))] => {
                                    Some(crate::syntax::ast::Predicate::Index(*n))
                                }
                                [Arg::Range(a, b)] => {
                                    Some(crate::syntax::ast::Predicate::Range(*a, *b))
                                }
                                _ => None,
                            } =>
                    {
                        selecting = Command::Select(pred);
                        &selecting
                    }
                    other => other,
                };
                let resolved;
                // (`map` keeps its arguments: the first is the body it
                // applies, `--to=` names its cells)
                let command = match command {
                    Command::Func(call)
                        if stdlib::known_scalar(&call.name)
                            && !matches!(call.name.as_str(), "rec" | "record" | "map")
                            && call.args.iter().any(|a| {
                                outer_expr(a) && !stdlib::names_columns(&call.name, a)
                            }) =>
                    {
                        resolved = Command::Func(FnCall {
                            headless: false,
                            name: call.name.clone(),
                            args: call
                                .args
                                .iter()
                                .map(|a| match a {
                                    // a column's name stays a name, as
                                    // in the pipe itself
                                    a if stdlib::names_columns(&call.name, a) => a.clone(),
                                    Arg::Expr(e) => {
                                        Arg::Lit(operand_scalar(host, ext, e, trace, scope))
                                    }
                                    Arg::Opt {
                                        name,
                                        value: Some(e),
                                    } if !matches!(e, Operand::Lit(_)) => Arg::Opt {
                                        name: name.clone(),
                                        value: Some(Operand::Lit(operand_scalar(
                                            host, ext, e, trace, scope,
                                        ))),
                                    },
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
                caps = crate::pipeline::commands::apply_command_in(
                    command,
                    mode,
                    caps,
                    host,
                    trace,
                    Some(&scope),
                );
                mode = crate::syntax::parser::command_mode_out(command, mode);
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
        Operand::Position(inner) if bare_cursor(inner) => host.node_ref(ext).into_iter().collect(),
        // `_` — the cursor record: the topic and the position, each
        // as its sigil reads it (null where it has none).
        Operand::Cursor => {
            let topic = operand_value(host, ext, &Operand::Topic, trace, scope);
            let position = host.node_ref(ext).unwrap_or(Value::Null);
            vec![Value::record(vec![
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
            // `$$0` in the driver's own pipeline after a join is the
            // driver's own node, what `$$_` is there (ruling
            // 2026-11-06); never a silent null
            None if bare_cursor(inner) => host.node_ref(ext).into_iter().collect(),
            None => eval_operand(host, ext, inner, trace, scope),
        },
        Operand::Interp(segs) => {
            let mut out = String::new();
            for seg in segs {
                match seg {
                    InterpSeg::Text(t) => out.push_str(t),
                    InterpSeg::Expr(e) => {
                        // a forking expression is refused (ruling
                        // 2026-11-06): one string wants `@| join`, the
                        // per-thread form its own interpolation
                        let vs = eval_operand(host, ext, e, trace, scope);
                        if vs.len() > 1 {
                            record_refusal(format!(
                                "'${{…}}' takes one value, and the expression forks into {}: write \
                                 '${{… @| join(\", \")}}' for one string, or interpolate per thread",
                                vs.len()
                            ));
                            return vec![Value::Null];
                        }
                        let v = vs.into_iter().next().unwrap_or(Value::Null);
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
                                    "the strict expansion '${{{}:?}}' is null",
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
        Operand::Capture(n) => {
            let own = |captures: &[String]| {
                captures
                    .get(n - 1)
                    .map(|s| Value::Str(s.clone()))
                    .unwrap_or(Value::Null)
            };
            // a predicate that made no capture reads the thread's
            // that navigates its path (ruling 2026-11-10)
            vec![if scope.captures.is_empty() && scope.named.is_empty() {
                crate::pipeline::run::enclosing_captures(|c| {
                    own(c.map_or(&[][..], |(numbered, _)| numbered))
                })
            } else {
                own(scope.captures)
            }]
        }
        // The descriptors (spec: Field Projections): on a record, a
        // table or a list of records, `:::key` reads a descriptor of
        // the value — fields, keys, header, type, dim, nrows, ncols,
        // what the commands of those names answer — any other key
        // null. `::key` stays the position's property whatever the
        // topic (the double colon picks up from navigation), and a
        // record's field has the single colon alone.
        // a bare projection with one node reference as the topic reads
        // the node the reference names (`map(::code)` over a route, a
        // cell's reference in a subcontext); a list as the topic keeps
        // the thread's own node, so `.:r | [::code = "PPT"]` after an
        // attach still filters by the node the thread stands at
        Operand::Rel {
            steps,
            projection: Some(projection @ (Projection::Property(_) | Projection::CoreMeta(_))),
            anchor: Anchor::Current,
        } if steps.is_empty() && scope.topic.is_some_and(|t| matches!(t, Value::Node(_))) => {
            let topic = scope.topic.cloned().unwrap_or(Value::Null);
            vec![through(host, &topic, projection)]
        }
        Operand::Rel {
            steps,
            projection: Some(Projection::AdapterMeta(key)),
            anchor: Anchor::Current,
        } if steps.is_empty()
            && (matches!(
                scope.topic,
                Some(Value::Record(_) | Value::Table(_) | Value::Projection { .. })
            ) || matches!(scope.topic, Some(Value::List(l)) if !l.is_empty() && l.iter().all(|x| matches!(x, Value::Record(_))))) =>
        {
            let topic = scope.topic.cloned().unwrap_or(Value::Null);
            if let Some(prov) = scope.prov {
                note_all(&prov.origins);
            }
            use crate::stdlib as sl;
            // on a projector, `:::type` is its declared type (draft
            // 166), null where it carries none; no other descriptor
            if let Value::Projection { .. } = &topic {
                return vec![match key.as_str() {
                    "type" => topic
                        .declared_type()
                        .map_or(Value::Null, |t| Value::Str(t.word().to_string())),
                    _ => Value::Null,
                }];
            }
            vec![match key.as_str() {
                "fields" => sl::fields_reading(topic).unwrap_or(Value::Null),
                "keys" => sl::keys_reading(topic).unwrap_or(Value::Null),
                "header" => sl::table_meta(topic, "header").unwrap_or(Value::Null),
                // the labels of an indexed table (draft 166), null
                // on a table with no index and on a record
                "index" => match &topic {
                    Value::Table(t) => t.labels().map_or(Value::Null, Value::list),
                    _ => Value::Null,
                },
                // the header's name (draft 166), null where none
                "name" => match &topic {
                    Value::Table(t) => t.name.clone().map_or(Value::Null, Value::Str),
                    _ => Value::Null,
                },
                "type" => sl::type_of(&topic),
                "dim" => sl::table_meta(topic, "dim").unwrap_or(Value::Null),
                "nrows" | "ncols" => match sl::table_meta(topic, "dim") {
                    Ok(Value::List(d)) => d
                        .get(usize::from(key == "ncols"))
                        .cloned()
                        .unwrap_or(Value::Null),
                    _ => Value::Null,
                },
                _ => Value::Null,
            }]
        }
        Operand::Rel {
            steps,
            projection,
            anchor,
        } => crate::pipeline::run::out_of_row_reach(|| {
            host.rel(ext, steps, anchor, projection.as_ref(), trace, scope)
        }),
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
            if let Some(refusal) = product_past_bound(ls.len(), rs.len()) {
                crate::pipeline::record_refusal(refusal);
                return vec![Value::Null];
            }
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
            let row_of = |items: &[(Option<String>, Operand)]| -> Vec<(Option<String>, Value)> {
                items
                    .iter()
                    .map(|(name, e)| match e {
                        // a declared field casts its value
                        Operand::Typed { inner, typing } => {
                            let v = gathered(eval_operand(host, ext, inner, trace, scope));
                            let key = name.clone().unwrap_or_default();
                            (name.clone(), cast_reading(v, &key, Some(typing.clone())))
                        }
                        e => (
                            name.clone(),
                            gathered(eval_operand(host, ext, e, trace, scope)),
                        ),
                    })
                    .collect()
            };
            // `$_ ++! %(axis = "//")` on a reflected node: the node
            // copied with the named properties replaced (`++` added),
            // its children kept, answered as a quarq (design paper:
            // the node overwrite — the constructor applied to the
            // node itself)
            if matches!(left.as_ref(), Operand::Cursor | Operand::Topic)
                && let Operand::Row(items) = right.as_ref()
                && let Some(form) = host.node_form(ext)
            {
                let row = row_of(items);
                return vec![match form.rewrite(*mode, &row) {
                    Ok(f) => Value::quarq_form(f),
                    Err(why) => {
                        crate::pipeline::record_refusal(format!("{}: {why}", mode.symbol()));
                        Value::Null
                    }
                }];
            }
            let ls = forking(eval_operand(host, ext, left, trace, scope));
            let rs: Vec<Result<Value, String>> = match right.as_ref() {
                Operand::Row(items) => {
                    let row = row_of(items);
                    ls.iter()
                        .map(|l| crate::stdlib::concat_row(*mode, l.clone(), row.clone()))
                        .collect()
                }
                _ => {
                    let rs = forking(eval_operand(host, ext, right, trace, scope));
                    if let Some(refusal) = product_past_bound(ls.len(), rs.len()) {
                        crate::pipeline::record_refusal(refusal);
                        return vec![Value::Null];
                    }
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
        // A typed projector stands where a field is declared — a
        // header item, a record key, a structured projector's item,
        // `map`'s body — never as a value (spec: Projectors, draft
        // 166): a trait describes a field, not a value. The unit
        // trait is the exception (ruling 2026-11-08): `::time<_h>`
        // as a value is the reading under the unit, null where it
        // does not read.
        Operand::Typed {
            inner,
            typing: crate::Typing::Declared(crate::ColumnType::Unit(unit)),
        } => forking(eval_operand(host, ext, inner, trace, scope))
            .into_iter()
            .map(|v| crate::value::unit_reading(v, unit))
            .collect(),
        Operand::InUnit { inner, unit } => forking(eval_operand(host, ext, inner, trace, scope))
            .into_iter()
            .map(|v| crate::value::unit_reading(v, unit))
            .collect(),
        Operand::InOffset { inner, minutes } => {
            forking(eval_operand(host, ext, inner, trace, scope))
                .into_iter()
                .map(|v| crate::value::offset_reading_at(v, *minutes))
                .collect()
        }
        // The offset trait likewise (ruling 2026-11-10): `::at<+02:00>`
        // as a value is the instant declared or displayed at the
        // offset, null on what is no instant.
        Operand::Typed {
            inner,
            typing: crate::Typing::Declared(crate::ColumnType::Offset(minutes)),
        } => forking(eval_operand(host, ext, inner, trace, scope))
            .into_iter()
            .map(|v| crate::value::offset_reading_at(v, *minutes))
            .collect(),
        Operand::Typed { inner, typing } => {
            let spelled = crate::syntax::unparse::operand(operand);
            let cast = match typing.declared() {
                Some(t) => format!(
                    "; cast a value with {}({})",
                    t.word(),
                    crate::syntax::unparse::operand(inner)
                ),
                None => String::new(),
            };
            crate::pipeline::record_refusal(format!(
                "'{spelled}' declares a field, and stands where a value does: a trait describes a field, never a value{cast}"
            ));
            vec![Value::Null]
        }
        Operand::Neg(inner) | Operand::Desc(inner) => {
            forking(eval_operand(host, ext, inner, trace, scope))
                .into_iter()
                .map(|v| match v.numeric_reading() {
                    Some(Value::Int(n)) => n
                        .checked_neg()
                        .map(Value::Int)
                        .unwrap_or(Value::Float(-(n as f64))),
                    Some(Value::Float(f)) => Value::Float(-f),
                    _ => Value::Null,
                })
                .collect()
        }
        // A reducing callee's first argument: the values it reaches
        // as one list, as the command's input would be; one value as
        // it is (draft 164).
        Operand::Gathered(inner) => vec![gathered(eval_operand(host, ext, inner, trace, scope))],
        // A boolean group in operand position is its truth value —
        // and a comparison whose sides are one value each distributes
        // over a list on either side (ruling 2026-11-05: `@(22; 38) >
        // 35` → `@(false; true)`); several values on a side keep the
        // existential reading a predicate has.
        Operand::Group(e) => {
            if let PredExpr::Compare(l, op, r) = e.as_ref()
                && !matches!(r, Operand::Pattern(_))
            {
                let lhs = discard(|| eval_operand(host, ext, l, trace, scope));
                let rhs = discard(|| eval_operand(host, ext, r, trace, scope));
                if lhs.len() == 1
                    && rhs.len() == 1
                    && (matches!(lhs[0], Value::List(_)) || matches!(rhs[0], Value::List(_)))
                {
                    return vec![match crate::stdlib::compare::compare_value(
                        &lhs[0],
                        *op,
                        &rhs[0],
                        &|e| host.scale_of(e),
                    ) {
                        Ok(v) => v,
                        Err(why) => {
                            record_refusal(why);
                            Value::Null
                        }
                    }];
                }
            }
            vec![Value::Bool(discard(|| {
                eval_pred_expr(host, ext, e, trace, scope)
            }))]
        }
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

/// A projector read on the standing node or topic: `:key`
/// the field read (a record's field, a table's column, a node's
/// core field), `::key` and `:::key` the node's ladders.
pub(crate) fn read_projection<H: Host>(
    host: &H,
    ext: &H::Ext,
    ladder: u8,
    key: &str,
    trace: &H::Trace,
    scope: Scope<'_, H::Ext>,
) -> Value {
    use crate::syntax::ast::{Anchor, Projection};
    let op = match ladder {
        1 => Operand::Field {
            base: Box::new(Operand::Topic),
            name: key.to_string(),
            bare: true,
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
    group_value(host, ext, &op, trace, scope)
}

/// Every projector in `v`, at any depth of lists, records
/// and tables, replaced by `read`'s reading of it; every other
/// value kept (spec: The Projection Operator).
pub(crate) fn walk_projections(v: Value, read: &dyn Fn(u8, &str) -> Value) -> Value {
    match v {
        // a projector that holds a level (`:bar:@(:A; :B)`, a
        // header's nested reading): the name read, and the level
        // below read on that reading, as the glued colon applies a
        // list to a value
        Value::Projection {
            ladder,
            key,
            typing: Some(crate::Typing::Levels(below)),
        } => {
            let base = read(ladder, &key);
            let under = |l: u8, k: &str| match l {
                1 => crate::pipeline::commands::record_field(base.clone(), k),
                _ => Value::Null,
            };
            Value::list(
                below
                    .into_iter()
                    .map(|x| walk_projections(x, &under))
                    .collect(),
            )
        }
        // a typed projector casts what it reads (draft 166): the
        // column's cells, or the one value, into its declaration
        Value::Projection {
            ladder,
            key,
            typing,
        } => cast_reading(read(ladder, &key), &key, typing),
        Value::List(items) => Value::list(
            items
                .iter()
                .cloned()
                .map(|x| walk_projections(x, read))
                .collect(),
        ),
        Value::Record(fields) => Value::Record(
            fields
                .into_iter()
                .map(|(k, x)| (k, walk_projections(x, read)))
                .collect(),
        ),
        Value::Table(t) => {
            let rows = t
                .rows()
                .map(|r| r.iter().map(|x| walk_projections(x, read)).collect())
                .collect();
            Value::table(t.with_rows(rows))
        }
        other => other,
    }
}

/// One label of a double-bracket lookup: a value, or a range with
/// both ends in.
pub(crate) enum LabelSpec {
    One(Value),
    /// `[[@(x; y)]]` — several labels from a list
    Many(Vec<Value>),
    Range(Value, Value),
}

/// `v[[…]]` (spec: Indexing a Value, draft 166): on a record the
/// field under the key, compared as a value (one label the value,
/// several a record of the fields found); on an indexed table the
/// rows carrying the label, compared against the index's type (one
/// label on one row the row record, else the rows as a table under
/// the same index); a table with no index, a list and any other value
/// refuse, each with its pointer.
pub(crate) fn label_lookup(
    v: Value,
    specs: &[LabelSpec],
    scale: crate::stdlib::arith::UnitScale,
) -> Result<Value, String> {
    use crate::stdlib::compare::value_cmp;
    use std::cmp::Ordering;
    let within = |x: &Value, a: &Value, b: &Value| {
        matches!(
            value_cmp(a, x, scale),
            Some(Ordering::Less | Ordering::Equal)
        ) && matches!(
            value_cmp(x, b, scale),
            Some(Ordering::Less | Ordering::Equal)
        )
    };
    match v {
        Value::Record(r) => {
            // a label meets a key as a value, and nothing coerces
            // them (a record is free-form where a table is typed): a
            // text finds a text key only, the literal `2013-01-02`
            // the instant key
            let find = |k: &Value| r.find_key(k);
            let mut hits: Vec<usize> = Vec::new();
            for s in specs {
                match s {
                    LabelSpec::One(k) => {
                        if let Some(i) = find(k) {
                            hits.push(i);
                        } else if specs.len() == 1 {
                            return Ok(Value::Null);
                        }
                    }
                    LabelSpec::Many(ks) => hits.extend(ks.iter().filter_map(find)),
                    LabelSpec::Range(a, b) => {
                        hits.extend((0..r.len()).filter(|&i| within(&r.key(i), a, b)));
                    }
                }
            }
            if let [LabelSpec::One(_)] = specs {
                return Ok(hits.first().map(|&i| r[i].1.clone()).unwrap_or(Value::Null));
            }
            Ok(Value::Record(r.filter_fields(|i, _| hits.contains(&i))))
        }
        Value::Table(t) if t.index > 1 => {
            // a composite index: the labels given are the levels,
            // outer first (`t[["bar"; "one"]]` one row, `t[["bar"]]`
            // the sub-table under the outer label indexed by what
            // remains); a list of labels (`t[[@(x; y)]]`) several
            // outer labels, as a table
            if let [LabelSpec::Many(many)] = specs {
                let rows: Vec<usize> = many.iter().flat_map(|l| t.rows_at_levels(std::slice::from_ref(l))).collect();
                return Ok(Value::table(t.take_rows(&rows)));
            }
            let levels: Vec<Value> = specs
                .iter()
                .map(|s| match s {
                    LabelSpec::One(v) => Ok(v.clone()),
                    LabelSpec::Many(_) => Err("a list of labels stands alone in the double bracket: t[[@(x; y)]]".to_string()),
                    LabelSpec::Range(_, _) => Err("a range reads one level of a composite index at a time: t[[a..b]] on the outer level alone is not yet a form".to_string()),
                })
                .collect::<Result<_, _>>()?;
            if levels.len() > t.index {
                return Err(format!(
                    "the index has {} levels ({}), and {} labels were given",
                    t.index,
                    t.level_names().join(", "),
                    levels.len()
                ));
            }
            let rows = t.rows_at_levels(&levels);
            if levels.len() == t.index {
                return Ok(if rows.len() == 1 {
                    t.row(rows[0] + 1).unwrap_or(Value::Null)
                } else if rows.is_empty() {
                    Value::Null
                } else {
                    Value::table(t.take_rows(&rows))
                });
            }
            Ok(Value::table(t.sub_table(&rows, levels.len())))
        }
        Value::Table(t) => {
            if t.index == 0 {
                return Err(
                    "the table has no index to look a label up in: name one with #(-i=:col), or read a row by position, t[n]"
                        .into(),
                );
            }
            let labels = t.labels().unwrap_or_default();
            let ty = t.types().first().cloned();
            let cast = |x: &Value| match &ty {
                Some(ty) => crate::value::cast_cell(x, ty).unwrap_or_else(|| x.clone()),
                None => x.clone(),
            };
            // on a tuple index a list written in the brackets is ONE
            // label, the tuple itself
            let tuple_index = matches!(ty, Some(crate::ColumnType::Tuple(_)));
            let specs: Vec<LabelSpec> = specs
                .iter()
                .map(|s| match s {
                    LabelSpec::Many(ks) if tuple_index => LabelSpec::One(Value::list(ks.clone())),
                    LabelSpec::One(v) => LabelSpec::One(v.clone()),
                    LabelSpec::Many(ks) => LabelSpec::Many(ks.clone()),
                    LabelSpec::Range(a, b) => LabelSpec::Range(a.clone(), b.clone()),
                })
                .collect();
            let specs = specs.as_slice();
            let mut rows: Vec<usize> = Vec::new();
            for s in specs {
                match s {
                    LabelSpec::One(k) => {
                        let k = cast(k);
                        rows.extend(labels.iter().enumerate().filter(|(_, l)| l.same_key(&k)).map(|(i, _)| i));
                    }
                    LabelSpec::Many(ks) => {
                        for k in ks {
                            let k = cast(k);
                            rows.extend(labels.iter().enumerate().filter(|(_, l)| l.same_key(&k)).map(|(i, _)| i));
                        }
                    }
                    LabelSpec::Range(a, b) => {
                        let (a, b) = (cast(a), cast(b));
                        rows.extend(labels.iter().enumerate().filter(|(_, l)| within(l, &a, &b)).map(|(i, _)| i));
                    }
                }
            }
            if let [LabelSpec::One(_)] = specs
                && rows.len() <= 1
            {
                return Ok(rows.first().and_then(|&i| t.row(i + 1)).unwrap_or(Value::Null));
            }
            Ok(Value::table(t.take_rows(&rows)))
        }
        Value::List(_) => Err(
            "a list is indexed by position, [n]; the double bracket looks a label up on an indexed table, or a key on a record"
                .into(),
        ),
        other => Err(format!(
            "the double bracket looks a label up on an indexed table or a key on a record, not {}",
            crate::stdlib::kind_of(&other)
        )),
    }
}

/// A projector's reading cast into its declared type (spec:
/// Projectors, draft 166): a list cell by cell, a scalar as one; a
/// value that does not cast refuses naming the field, and the
/// reading is null. A bare projector or the star casts nothing.
pub(crate) fn cast_reading(v: Value, key: &str, typing: Option<crate::Typing>) -> Value {
    let Some(ty) = typing.as_ref().and_then(crate::Typing::declared) else {
        return v;
    };
    let cast_one = |c: &Value| crate::value::cast_cell(c, &ty);
    match &v {
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (i, c) in items.iter().enumerate() {
                match cast_one(c) {
                    Some(x) => out.push(x),
                    None => {
                        crate::pipeline::record_refusal(format!(
                            "'{key}' is declared {}, and item {} holds {}, which does not cast to it",
                            ty.word(),
                            i + 1,
                            crate::value::held_text(c)
                        ));
                        return Value::Null;
                    }
                }
            }
            Value::list(out)
        }
        Value::Record(_) | Value::Table(_) | Value::Foreign(_) => v,
        c => match cast_one(c) {
            Some(x) => x,
            None => {
                crate::pipeline::record_refusal(format!(
                    "'{key}' is declared {}, and holds {}, which does not cast to it",
                    ty.word(),
                    crate::value::held_text(c)
                ));
                Value::Null
            }
        },
    }
}

/// A quarq run on the capsa (spec: Quarqs): its body as the tail of
/// a pipe on the scope's topic — or its node, when there is no topic
/// — late bound, the registers and the cursor read here.
pub(crate) fn run_quarq<H: Host>(
    host: &H,
    ext: &H::Ext,
    q: &crate::value::Quarq,
    trace: &H::Trace,
    scope: Scope<'_, H::Ext>,
) -> Value {
    let piped = Operand::Piped {
        expr: Box::new(Operand::Seed),
        commands: (*q.body).clone(),
    };
    operand_scalar(host, ext, &piped, trace, scope)
}

/// A value a command takes a command through — a key of a keyed
/// aggregate, the body of `map` — resolved where it is a held quarq
/// or projector (spec: Quarqs): the quarq runs on the capsa, a
/// structure of projectors is read on it, anything else is itself.
pub(crate) fn apply_held<H: Host>(
    host: &H,
    ext: &H::Ext,
    v: Value,
    trace: &H::Trace,
    scope: Scope<'_, H::Ext>,
) -> Value {
    match v {
        Value::Quarq(q) => run_quarq(host, ext, &q, trace, scope),
        v if v.holds_projector() => {
            let read =
                |ladder: u8, key: &str| read_projection(host, ext, ladder, key, trace, scope);
            walk_projections(v, &read)
        }
        v => v,
    }
}

/// A value index's spec with its expression read (`[@(2; 4)]`,
/// `[$.pos]`): an integer the position, a list of integers the
/// position list; any other spec as written.
pub(crate) fn resolve_index_spec<H: Host>(
    host: &H,
    ext: &H::Ext,
    spec: &IndexSpec,
    trace: &H::Trace,
    scope: Scope<'_, H::Ext>,
) -> Result<IndexSpec, String> {
    match spec {
        IndexSpec::Expr(e) => {
            match crate::pipeline::positions_of(&operand_value(host, ext, e, trace, scope)) {
                Some(crate::syntax::ast::Predicate::Index(n)) => Ok(IndexSpec::At(n)),
                Some(crate::syntax::ast::Predicate::Positions(parts)) => Ok(IndexSpec::List(
                    parts
                        .iter()
                        .map(|p| match p {
                            crate::syntax::ast::Predicate::Index(n) => IndexSpec::At(*n),
                            _ => IndexSpec::At(0),
                        })
                        .collect(),
                )),
                _ => Err(
                    "an index read from an expression is an integer or a list of integers".into(),
                ),
            }
        }
        other => Ok(other.clone()),
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
    let (record, provs) = record_build(args, host, ext, trace, scope)?;
    Ok(record
        .into_fields()
        .into_iter()
        .zip(provs)
        .map(|((k, v), p)| (k, v, p))
        .collect())
}

/// The record constructor's build (spec: Record, draft 166): the
/// record with its typed keys and its declarations, and one
/// provenance per field. `record_fields_prov` is the text-keyed view
/// for the callers that read the fields as a key.
pub(crate) fn record_build<H: Host>(
    args: &[Arg],
    host: &H,
    ext: &H::Ext,
    trace: &H::Trace,
    scope: Scope<'_, H::Ext>,
) -> Result<(crate::value::Record, Vec<Prov>), String> {
    use crate::value::{ColumnType, Record};
    let mut fields: Vec<(String, Value, Prov)> = Vec::new();
    // the typed key and the declaration per field, in step
    let mut keys: Vec<Option<Value>> = Vec::new();
    let mut declared: Vec<Option<ColumnType>> = Vec::new();
    let mut keys_type: Option<ColumnType> = None;
    let mut values_type: Option<ColumnType> = None;
    // what `:*` leaves out (ruling 2026-11-05): the fields the items
    // produce (a bare `:k`, the key of `name = expr`) and the ones a
    // rename takes whole (`new = :old`)
    let taken: Vec<String> = {
        let mut v = Vec::new();
        let mut it = args.iter().peekable();
        while let Some(a) = it.next() {
            let produced = match a {
                Arg::Lit(Value::Str(name)) => Some(name.clone()),
                Arg::Key(k) => Some(Record::key_text(k)),
                _ => None,
            };
            if let Some(name) = produced {
                v.push(name);
                if let Some(Arg::Expr(Operand::Field {
                    base, name: old, ..
                })) = it.peek()
                    && matches!(**base, Operand::Topic)
                    && old != "*"
                {
                    v.push(old.clone());
                }
                continue;
            }
            match a {
                Arg::Expr(Operand::Field { base, name, .. })
                    if matches!(**base, Operand::Topic) && name != "*" =>
                {
                    v.push(name.clone());
                }
                Arg::Expr(Operand::Typed { inner, .. }) => {
                    if let Some(k) = crate::syntax::ast::auto_field_name(inner) {
                        v.push(k.to_string());
                    }
                }
                _ => {}
            }
        }
        v
    };
    let mut args = args.iter().peekable();
    while let Some(arg) = args.next() {
        let before = fields.len();
        match arg {
            // a template hole never reaches a record build: the fill
            // replaces it before anything runs
            Arg::Hole(_) => {}
            Arg::Lit(Value::Str(_)) | Arg::Key(_) => {
                let (name, key) = match arg {
                    Arg::Lit(Value::Str(name)) => (name.clone(), None),
                    Arg::Key(k) => (Record::key_text(k), Some(k.clone())),
                    _ => unreachable!(),
                };
                let mut decl = None;
                let (value, prov) = match args.next() {
                    // `price<float> = …` — the declared field: its
                    // value cast into the key's type (draft 166)
                    Some(Arg::Expr(Operand::Typed { inner, typing })) => {
                        let (v, o) = capture(|| operand_value(host, ext, inner, trace, scope));
                        decl = typing.declared();
                        (cast_reading(v, &name, Some(typing.clone())), Prov::leaf(o))
                    }
                    Some(Arg::Expr(e)) => {
                        let (v, o) = capture(|| operand_value(host, ext, e, trace, scope));
                        (v, Prov::leaf(o))
                    }
                    Some(Arg::Lit(v)) => (v.clone(), Prov::default()),
                    Some(Arg::Range(_, _))
                    | Some(Arg::Key(_))
                    | Some(Arg::Named { .. })
                    | Some(Arg::Opt { .. })
                    | Some(Arg::Pairs { .. })
                    | Some(Arg::Hole(_))
                    | None => (Value::Null, Prov::default()),
                };
                fields.push((name, value, prov));
                keys.push(key);
                declared.push(decl);
            }
            // `%(--keys=instant; --values=int)`: every key, every
            // value declared (either alone)
            Arg::Opt {
                name,
                value: Some(Operand::Lit(Value::Str(word))),
            } if matches!(name.as_str(), "--keys" | "--values") => {
                let ty = ColumnType::from_word(word);
                if name == "--keys" {
                    keys_type = ty;
                } else {
                    values_type = ty;
                }
            }
            // `%(:props...)` — the spread: the projections the item
            // reads spread into fields keyed by their bare keys,
            // each read on the cursor; two with one key refuse
            Arg::Expr(Operand::Spread(inner)) => {
                let (v, o) = capture(|| operand_value(host, ext, inner, trace, scope));
                let items = match v {
                    Value::List(items) => items.to_vec(),
                    one @ Value::Projection { .. } => vec![one],
                    other => {
                        return Err(format!(
                            "the spread takes a list of projections, not {}",
                            crate::stdlib::kind_of(&other)
                        ));
                    }
                };
                for item in items {
                    let Value::Projection {
                        ladder,
                        key,
                        typing,
                    } = item
                    else {
                        return Err(format!(
                            "the spread takes projections; {} is none",
                            item.display_form()
                        ));
                    };
                    if let Some(first) = fields.iter().find(|(f, _, _)| *f == key) {
                        let _ = first;
                        return Err(format!(
                            "two projections share the bare key '{key}' ({}); the record keys by it",
                            Value::projection_spelling(ladder, &key)
                        ));
                    }
                    let value = read_projection(host, ext, ladder, &key, trace, scope);
                    let value = cast_reading(value, &key, typing);
                    fields.push((key, value, Prov::leaf(o.clone())));
                }
            }
            // `%(::price<float>)` — the bare typed projector keys its
            // own field and declares it: `%(price<float> = ::price)`
            Arg::Expr(Operand::Typed { inner, typing }) => {
                let name = crate::syntax::ast::auto_field_name(inner)
                    .unwrap_or_default()
                    .to_string();
                let (v, o) = capture(|| operand_value(host, ext, inner, trace, scope));
                fields.push((
                    name.clone(),
                    cast_reading(v, &name, Some(typing.clone())),
                    Prov::leaf(o),
                ));
                keys.push(None);
                declared.push(typing.declared());
            }
            // `%(:*; …)` — the rest: the topic record's fields the
            // items neither produce nor take, in its order, where it
            // stands (ruling 2026-11-05)
            Arg::Expr(Operand::Field { base, name, .. })
                if matches!(**base, Operand::Topic) && name == "*" =>
            {
                let Some(Value::Record(r)) = scope.topic else {
                    return Err(
                        "':*' stands in a projector over a record (r:%(:*; …)) or a table (t:#(:*; …)): the rest of its fields or columns".into(),
                    );
                };
                for (k, v) in r.iter() {
                    if !taken.iter().any(|t| t == k) {
                        if fields.iter().any(|(f, _, _)| f == k) {
                            return Err(format!("'{k}' is already a field of the record"));
                        }
                        fields.push((
                            k.clone(),
                            v.clone(),
                            scope.prov.cloned().unwrap_or_default(),
                        ));
                    }
                }
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
            Arg::Pairs {
                keys: key_list,
                values,
            } => {
                let k = operand_value(host, ext, key_list, trace, scope);
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
                let typed_keys: Vec<Option<Value>> = match &k {
                    Value::List(l) => l
                        .iter()
                        .map(|n| (!matches!(n, Value::Str(_))).then(|| n.clone()))
                        .collect(),
                    _ => Vec::new(),
                };
                for ((name, value), key) in names.into_iter().zip(items).zip(typed_keys) {
                    if fields.iter().any(|(n, _, _)| *n == name) {
                        return Err(format!("'{name}' is already a field of the record"));
                    }
                    fields.push((name, value, Prov::leaf(o.clone())));
                    keys.push(key);
                    declared.push(None);
                }
            }
            Arg::Range(_, _) | Arg::Named { .. } | Arg::Opt { .. } => {}
        }
        let _ = before;
        keys.resize(fields.len(), None);
        declared.resize(fields.len(), None);
    }
    // `--keys=T`: every key cast into T; `--values=T`: every value
    // cast and every field declared T
    if let Some(ty) = keys_type {
        for (i, (name, _, _)) in fields.iter().enumerate() {
            let k = keys[i].clone().unwrap_or_else(|| Value::Str(name.clone()));
            match crate::value::cast_cell(&k, &ty) {
                Some(cast) => keys[i] = Some(cast),
                None => {
                    return Err(format!(
                        "--keys={}: the key {} does not cast to it",
                        ty.word(),
                        crate::value::held_text(&k)
                    ));
                }
            }
        }
    }
    if let Some(ty) = values_type {
        for (i, (name, v, _)) in fields.iter_mut().enumerate() {
            match crate::value::cast_cell(v, &ty) {
                Some(cast) => *v = cast,
                None => {
                    return Err(format!(
                        "--values={}: field '{name}' holds {}, which does not cast to it",
                        ty.word(),
                        crate::value::held_text(v)
                    ));
                }
            }
            declared[i] = Some(ty.clone());
        }
    }
    let provs: Vec<Prov> = fields.iter().map(|(_, _, p)| p.clone()).collect();
    let mut record = if keys.iter().any(Option::is_some) {
        Record::keyed(
            fields
                .iter()
                .zip(&keys)
                .map(|((name, v, _), k)| {
                    (
                        k.clone().unwrap_or_else(|| Value::Str(name.clone())),
                        v.clone(),
                    )
                })
                .collect(),
        )
    } else {
        Record::new(fields.into_iter().map(|(k, v, _)| (k, v)).collect())
    };
    for (i, d) in declared.into_iter().enumerate() {
        if d.is_some() {
            record.declare(i, d);
        }
    }
    Ok((record, provs))
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
    // a long `a || b || …` is a tree as deep as it is long: the
    // stack grows where it runs short (see `eval_operand`)
    stacker::maybe_grow(512 * 1024, 8 * 1024 * 1024, || {
        eval_pred_expr_in(host, ext, e, trace, scope)
    })
}

fn eval_pred_expr_in<H: Host>(
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
        // A bare path is an existence test: nodes alone are read
        PredExpr::Truthy(Operand::Rel {
            steps,
            projection,
            anchor,
        }) if !steps.is_empty() => {
            host.rel_exists(ext, steps, anchor, projection.as_ref(), trace, scope)
        }
        // a list of booleans — a comparison that distributed over a
        // list — holds when any element does (ruling 2026-11-05)
        PredExpr::Truthy(o) => {
            // a property alone is a test of presence: the name is
            // the question, and a surface that lacks it is asked
            // nothing (the read is the same, the warning is not)
            let read = || eval_operand(host, ext, o, trace, scope);
            let values = match o {
                Operand::Rel {
                    projection: Some(Projection::Property(Some(name))),
                    ..
                } => {
                    let values = crate::value::ambient::with_presence(read);
                    if values.iter().any(|v| !matches!(v, Value::Null)) {
                        crate::value::ambient::presence_found(name);
                    }
                    values
                }
                _ => read(),
            };
            values.iter().any(|v| match v {
                Value::List(l)
                    if !l.is_empty()
                        && l.iter().all(|x| matches!(x, Value::Bool(_) | Value::Null)) =>
                {
                    l.iter().any(|x| matches!(x, Value::Bool(true)))
                }
                v => v.is_truthy(),
            })
        }
    }
}

/// A join condition (spec: Correlation; ruling 2026-11-10): the ON
/// clause read in three-valued logic. A comparison one of whose
/// operands reads the other side of the join — the driver or an
/// earlier joined node — is a key comparison, and a null on either
/// side of it is unknown whatever the operator: `=` does not pair
/// two absent keys, and `!=` does not pair an absent key with every
/// node. Unknown passes through `!`, yields to a true under `||` and
/// to a false under `&&`; only a condition that is true admits the
/// candidate. Every other part of the clause reads as a predicate
/// does anywhere (`[::status = null]` holds for a null).
pub(crate) fn eval_join_condition<H: Host>(
    host: &H,
    ext: &H::Ext,
    e: &PredExpr,
    trace: &H::Trace,
    scope: Scope<'_, H::Ext>,
) -> Option<bool> {
    use crate::syntax::parser::operand_is_correlated as keyed;
    let known = |o: &Operand| -> Vec<Value> {
        eval_operand(host, ext, o, trace, scope)
            .into_iter()
            .filter(|v| !matches!(v, Value::Null))
            .collect()
    };
    match e {
        PredExpr::Or(a, b) => match eval_join_condition(host, ext, a, trace, scope) {
            Some(true) => Some(true),
            left => match (left, eval_join_condition(host, ext, b, trace, scope)) {
                (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            },
        },
        PredExpr::And(a, b) => match eval_join_condition(host, ext, a, trace, scope) {
            Some(false) => Some(false),
            left => match (left, eval_join_condition(host, ext, b, trace, scope)) {
                (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            },
        },
        PredExpr::Not(a) => eval_join_condition(host, ext, a, trace, scope).map(|b| !b),
        // a parenthesized condition is the condition: `!(a = b)`
        PredExpr::Truthy(Operand::Group(inner)) => {
            eval_join_condition(host, ext, inner, trace, scope)
        }
        PredExpr::Compare(l, op, r) if keyed(l) || keyed(r) => {
            let lhs = known(l);
            if lhs.is_empty() {
                return None;
            }
            if let Operand::Pattern(segs) = r {
                return Some(match op {
                    CmpOp::Ne => lhs.iter().any(|a| !pattern_test(segs, a)),
                    _ => lhs.iter().any(|a| pattern_test(segs, a)),
                });
            }
            let rhs = known(r);
            if rhs.is_empty() {
                return None;
            }
            Some(holds(l, &lhs, *op, r, &rhs, &|e| host.scale_of(e)))
        }
        PredExpr::Chain(operands, ops) if operands.iter().any(keyed) => {
            let values: Vec<Vec<Value>> = operands.iter().map(known).collect();
            if values.iter().any(Vec::is_empty) {
                return None;
            }
            Some(ops.iter().enumerate().all(|(i, op)| {
                holds(
                    &operands[i],
                    &values[i],
                    *op,
                    &operands[i + 1],
                    &values[i + 1],
                    &|e| host.scale_of(e),
                )
            }))
        }
        other => Some(eval_pred_expr(host, ext, other, trace, scope)),
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
    declared: &[Option<crate::ColumnType>],
    rows: &[crate::syntax::ast::TableRow],
    mut cell: impl FnMut(&Operand) -> Value,
) -> Result<crate::Table, String> {
    use crate::syntax::ast::TableRow;
    let mut out: Vec<Vec<Value>> = Vec::with_capacity(rows.len());
    let mut width: Option<usize> = header.as_ref().map(Vec::len);
    for (i, row) in rows.iter().enumerate() {
        let n = i + 1;
        // a quoted key's marker dropped: a row's names are column names
        let unmarked: Vec<(Option<String>, Operand)>;
        let row = match row {
            TableRow::Record(items)
                if items
                    .iter()
                    .any(|(n, _)| n.as_deref().is_some_and(|n| n.starts_with('\u{0}'))) =>
            {
                unmarked = items
                    .iter()
                    .map(|(n, e)| {
                        (
                            n.as_deref()
                                .map(|n| crate::syntax::ast::row_key(n).0.to_string()),
                            e.clone(),
                        )
                    })
                    .collect();
                &TableRow::Record(unmarked)
            }
            other => other,
        };
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
    Ok(crate::Table::typed(header, out, declared.to_vec())?)
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
            _ => Vec::new().into(),
        };
        for (k, _) in &fields {
            if !header.iter().any(|h| h == k) {
                header.push(k.clone());
            }
        }
        records.push(fields.to_vec());
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

/// A projection read through the node a value names: on a node
/// reference the node's reading, on a list each item's, null for
/// anything else (spec: reading through a reference).
pub(crate) fn through<H: Host>(host: &H, v: &Value, projection: &Projection) -> Value {
    match v {
        Value::Node(n) => host.project(&host.ext_at(crate::NodeId(n.id)), projection),
        Value::List(items) => {
            Value::list(items.iter().map(|x| through(host, x, projection)).collect())
        }
        // a quarq's own core field (`:unq`)
        Value::Quarq(q) => match projection {
            Projection::CoreMeta(key) => q.core_field(key).unwrap_or(Value::Null),
            _ => Value::Null,
        },
        _ => Value::Null,
    }
}
