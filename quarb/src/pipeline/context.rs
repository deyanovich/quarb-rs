//! Part of the executor: see `exec/mod.rs`.

use crate::pipeline::*;

/// The provenance of a capsa's effective topic: the topic's, else
/// the node read at this command.
pub(crate) fn effective_prov<H: Host>(host: &H, c: &Thread<H::Ext>) -> Prov {
    match &c.topic {
        Some(_) => c.prov.clone(),
        None => host.prov_at(&c.ext),
    }
}

/// A thread's topic as `$_` reads it: the topic if set; on a node
/// of a reflected query, the quarq of its form; else null. A thread
/// that never projected has a null topic — a node's name is not its
/// value — and `@_`, the seed after `@|`, a group's topic and every
/// reduction over the context read the threads' topics through this
/// one function, so `@_` is the list of the `$_` by construction
/// (ruling 2026-11-05).
pub(crate) fn effective_topic<H: Host>(c: &Thread<H::Ext>, host: &H) -> Value {
    c.topic
        .clone()
        .or_else(|| host.node_form(&c.ext).map(Value::quarq_form))
        .unwrap_or(Value::Null)
}

/// Partition the context for `window` / `shift`: for each capsa, the
/// ordered list of context indices it may reach and its own position
/// within that list. Without a key there is one list — the whole
/// context. With a key, capsae whose keys compare equal share a
/// list, in original context order; a null key matches nothing, so
/// its capsa stands alone (the ordering counterpart of "a missing
/// key does not compete").
pub(crate) fn peer_lists<H: Host>(
    caps: &[Thread<H::Ext>],
    key: Option<&Operand>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> (Vec<Vec<usize>>, Vec<(usize, usize)>) {
    let Some(key) = key else {
        let all: Vec<usize> = (0..caps.len()).collect();
        let of = (0..caps.len()).map(|i| (0, i)).collect();
        return (vec![all], of);
    };
    // One probe per capsa: the partition key's sort key hashes, so
    // 8,000 string keys cost 8,000 lookups, not a scan of every
    // partition formed so far per capsa.
    let mut lists: Vec<Vec<usize>> = Vec::new();
    let mut slots: HashMap<crate::value::SortKey, usize> = HashMap::new();
    let mut of = Vec::with_capacity(caps.len());
    for (i, c) in caps.iter().enumerate() {
        let scope = c.scope(Some(i + 1), peers, outer);
        let k = operand_scalar(host, &c.ext, key, trace, scope);
        // A null key partitions alone (no two nulls share a list).
        let slot = if matches!(k, Value::Null) {
            None
        } else {
            slots.get(&k.sort_key_with(&|e| host.scale_of(e))).copied()
        };
        match slot {
            Some(li) => {
                lists[li].push(i);
                of.push((li, lists[li].len() - 1));
            }
            None => {
                if !matches!(k, Value::Null) {
                    slots.insert(k.sort_key_with(&|e| host.scale_of(e)), lists.len());
                }
                lists.push(vec![i]);
                of.push((lists.len() - 1, 0));
            }
        }
    }
    (lists, of)
}

/// The span and optional partition key of a `window` / `shift` call
/// (shapes validated at parse time). A count `window(n)` is the
/// trailing span `-(n-1)..0`.
pub(crate) fn window_args(call: &FnCall) -> (Option<i64>, Option<i64>, Option<&Operand>) {
    let key = match call.args.get(1) {
        Some(Arg::Expr(e)) => Some(e),
        _ => None,
    };
    match call.args.first() {
        Some(Arg::Range(a, b)) => (*a, *b, key),
        Some(Arg::Lit(Value::Int(n))) => (Some(-(n - 1)), Some(0), key),
        _ => (None, None, key),
    }
}

/// `@| window(span[, key])`: populate each capsa's members with its
/// reachable neighbors at offsets within the span (0 = self; an open
/// end runs to the partition edge; a short edge window holds what
/// exists), and set the list topic from their effective topics.
pub(crate) fn window_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    let (from, to, key) = window_args(call);
    let (lists, of) = peer_lists(&caps, key, host, trace, outer, peers);
    let windows: Vec<Vec<usize>> = of
        .iter()
        .map(|&(li, p)| {
            let list = &lists[li];
            let p = p as i64;
            // Saturating: span ends come from query text unclamped,
            // so `window(0..i64::MAX)` must not overflow the adds.
            let lo = from.map_or(0, |a| p.saturating_add(a).max(0)) as usize;
            let hi = to.map_or(list.len() as i64 - 1, |b| {
                p.saturating_add(b).min(list.len() as i64 - 1)
            });
            if hi < lo as i64 {
                return Vec::new();
            }
            list[lo..=hi as usize].to_vec()
        })
        .collect();
    let members: Vec<Vec<Thread<H::Ext>>> = windows
        .iter()
        .map(|w| w.iter().map(|&j| caps[j].clone()).collect())
        .collect();
    caps.into_iter()
        .zip(members)
        .map(|(mut c, members)| {
            c.topic = Some(Value::list(
                members.iter().map(|m| effective_topic(m, host)).collect(),
            ));
            c.prov = Prov::list(members.iter().map(|m| effective_prov(host, m)).collect());
            c.members = members;
            c
        })
        .collect()
}

/// `@| shift(n[, key])`: replace each capsa's topic with the
/// effective topic of the capsa `n` positions back in its partition
/// (negative `n` looks forward; null past the edge). Consumes any
/// members — they no longer correspond to the topic.
pub(crate) fn shift_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    let (n, key) = match call.args.split_first() {
        Some((Arg::Lit(Value::Int(n)), rest)) => (
            *n,
            match rest.first() {
                Some(Arg::Expr(e)) => Some(e),
                _ => None,
            },
        ),
        _ => (1, None),
    };
    let (lists, of) = peer_lists(&caps, key, host, trace, outer, peers);
    let topics: Vec<(Value, Prov)> = of
        .iter()
        .map(|&(li, p)| {
            let list = &lists[li];
            // Saturating: `shift(i64::MIN)` must not overflow.
            let idx = (p as i64).saturating_sub(n);
            if idx < 0 || idx >= list.len() as i64 {
                (Value::Null, Prov::default())
            } else {
                let src = &caps[list[idx as usize]];
                (effective_topic(src, host), effective_prov(host, src))
            }
        })
        .collect();
    caps.into_iter()
        .zip(topics)
        .map(|(mut c, (t, p))| {
            c.topic = Some(t);
            c.prov = p;
            c.members = Vec::new();
            c
        })
        .collect()
}

/// `@| timeline(EXPR[; ANCHOR][; forward])` (spec: The Timeline): walk
/// the context in order holding one period, the narrative point.
/// `EXPR` names each capsa's time expression — a `timex` record, or
/// a value with a calendar reading (a complete date). A complete
/// date is its own result; a relative expression, a time of day and
/// a date with fields left out are valued against the point; a
/// duration, a vague expression and a null stay unvalued. A result
/// becomes the point — under `forward`, only where it is not behind
/// it. `ANCHOR`, evaluated per capsa, cuts the context into runs of
/// one value: each run is a chain that starts with its anchor as
/// the point. Each topic becomes the expression's record with
/// `when` (the period, or null) and `from` (the `$ord` of the capsa
/// whose result was the point, 0 for the anchor, null for a
/// complete date or an unvalued one). Consumes any members.
pub(crate) fn timeline_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    use crate::period::{Period, Shape};
    // `--forward` is a flag (draft 203); the operands by position.
    let forward = call
        .args
        .iter()
        .any(|a| matches!(a, Arg::Opt { name, value: None } if name == "--forward"));
    let positional: Vec<Arg> = call
        .args
        .iter()
        .filter(|a| !matches!(a, Arg::Opt { .. }))
        .cloned()
        .collect();
    let args = positional.as_slice();
    let rows: Vec<(Value, Option<Value>)> = caps
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let read = |a: &Arg| match a {
                Arg::Lit(v) | Arg::Key(v) => v.clone(),
                Arg::Range(..)
                | Arg::Named { .. }
                | Arg::Opt { .. }
                | Arg::Pairs { .. }
                | Arg::Hole(_) => Value::Null,
                Arg::Expr(e) => {
                    operand_scalar(host, &c.ext, e, trace, c.scope(Some(i + 1), peers, outer))
                }
            };
            let expr = match args.first() {
                Some(a) => read(a),
                None => effective_topic(c, host),
            };
            (expr, args.get(1).map(read))
        })
        .collect();
    // The fold: the point, and the position of the capsa that set it.
    let mut point: Option<(Period, i64)> = None;
    let mut run: Option<&Value> = None;
    let mut out: Vec<(Option<Period>, Option<i64>)> = Vec::with_capacity(rows.len());
    for (i, (expr, anchor)) in rows.iter().enumerate() {
        if let Some(a) = anchor
            && (i == 0 || run != Some(a))
        {
            // A new run: the chain starts at its anchor, or with no
            // point where the anchor has no calendar reading.
            point = a.calendar_reading().map(|p| (p, 0));
            run = Some(a);
        }
        let ord = i as i64 + 1;
        let value = stdlib::timex_value(expr);
        let (when, from) = match value.as_deref().map(crate::period::shape) {
            Some(Shape::Complete(p)) => (Some(p), None),
            Some(Shape::Anchored) => {
                let at = point.as_ref().map(|(p, _)| p);
                match crate::period::anchor(value.as_deref().unwrap_or(""), at, forward) {
                    Some(p) => (Some(p), point.as_ref().map(|(_, src)| *src)),
                    None => (None, None),
                }
            }
            _ => (None, None),
        };
        if let Some(p) = when {
            let behind = point
                .as_ref()
                .is_some_and(|(at, _)| p.interval_cmp(at) == Ordering::Less);
            if !(forward && behind) {
                point = Some((p, ord));
            }
        }
        out.push((when, from));
    }
    caps.into_iter()
        .zip(rows)
        .zip(out)
        .map(|((mut c, (expr, _)), (when, from))| {
            if c.topic.is_none() {
                c.prov = host.prov_at(&c.ext);
            }
            let mut fields = match expr {
                Value::Record(fields) => fields,
                other => vec![("value".to_string(), other)].into(),
            };
            fields.retain(|(k, _)| k != "when" && k != "from");
            fields.push(("when".to_string(), when.map_or(Value::Null, Value::period)));
            fields.push(("from".to_string(), from.map_or(Value::Null, Value::Int)));
            c.topic = Some(Value::Record(fields));
            c.members = Vec::new();
            c
        })
        .collect()
}

/// `@| outline[(level; text; --style="1.")]`: each capsa's topic
/// becomes its line of the outline, the level and the text read per
/// capsa (`depth` and `title` of a record without them), numbered
/// `1`, `1.1`, `1.2`, `2` under `--style` (the `@| number` reading is
/// retired: `number` is the locale cast alone).
/// `@| threads[(-d=n; -c)]` — the thread tree (spec: The Pipeline,
/// the thread tree): the lineage the run recorded for the context's
/// threads, drawn as one text — every step a level, the forks
/// children, each aggregation a paragraph. `apply_command`'s arm.
pub(crate) fn threads_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
) -> Vec<Thread<H::Ext>> {
    let spec = crate::graph::threads::ThreadsSpec::from_call(call);
    let mut origins = Origins::default();
    for c in &caps {
        origins.insert(host.origin_at(&c.ext));
    }
    match host.render_threads(&caps, &spec) {
        Ok(text) => vec![Thread::<H::Ext> {
            prov: Prov::leaf(origins),
            register: shared_register(&caps),
            topic: Some(Value::Str(text)),
            members: Vec::new(),
            captures: Vec::new(),
            named: Vec::new(),
            // the drawing's one thread has no position
            ext: host.unplaced_ext(),
        }],
        Err(why) => {
            record_refusal(why);
            Vec::new()
        }
    }
}

/// `@| tree[(operands)]` — the context drawn as a tree (spec: The
/// tree): one text value, the mount root on top, every context node
/// a starting point. A value context is refused: the tree draws
/// nodes. `apply_command`'s arm.
pub(crate) fn tree_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    if let Some(c) = caps.iter().find(|c| c.topic.is_some()) {
        record_refusal(format!(
            "tree draws nodes, and the context holds values ({}): draw the \
             nodes before projecting them",
            c.topic.as_ref().map_or(Value::Null, stdlib::type_of)
        ));
        return Vec::new();
    }
    let spec = crate::graph::tree::TreeSpec::from_call(call);
    // The scope a column expression evaluates under: the first
    // capsa's (its register is the context's); the root's where the
    // context is empty, the top line still printing.
    let anchor = caps
        .first()
        .cloned()
        .unwrap_or_else(|| Thread::bare(host.root_ext(), None));
    let column = |ext: &H::Ext, op: &Operand| {
        operand_value(host, ext, op, trace, anchor.scope(None, peers, outer))
    };
    let starts: Vec<H::Ext> = caps.iter().map(|c| c.ext.bare()).collect();
    let mut origins = Origins::default();
    for c in &caps {
        origins.insert(host.origin_at(&c.ext));
    }
    match host.render_tree(&starts, &spec, &column) {
        Ok(text) => vec![Thread::<H::Ext> {
            prov: Prov::leaf(origins),
            register: shared_register(&caps),
            topic: Some(Value::Str(text)),
            members: Vec::new(),
            captures: Vec::new(),
            named: Vec::new(),
            // the drawing's one thread has no position
            ext: host.unplaced_ext(),
        }],
        Err(why) => {
            record_refusal(why);
            Vec::new()
        }
    }
}

/// `| tree[(operands)]` — each thread's node drawn as its own tree
/// (ruling 2026-11-11): the drawing is the thread's topic, a text,
/// and the thread keeps its node, its capsa and its place. With one
/// thread the text is the one `@| tree` draws. A thread whose topic
/// is a value has no node to draw, and the run is refused with the
/// pointer. `apply_command`'s arm.
pub(crate) fn tree_thread_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    if let Some(c) = caps.iter().find(|c| c.topic.is_some()) {
        record_refusal(format!(
            "tree draws a node, and the thread holds a value ({}): draw the \
             node before projecting it ('/path | tree')",
            c.topic.as_ref().map_or(Value::Null, stdlib::type_of)
        ));
        return Vec::new();
    }
    let spec = crate::graph::tree::TreeSpec::from_call(call);
    let mut out = Vec::with_capacity(caps.len());
    for (i, mut c) in caps.into_iter().enumerate() {
        let drawn = {
            // a column expression reads under the thread's own capsa
            let column = |ext: &H::Ext, op: &Operand| {
                operand_value(host, ext, op, trace, c.scope(Some(i + 1), peers, outer))
            };
            host.render_tree(&[c.ext.bare()], &spec, &column)
        };
        match drawn {
            Ok(text) => {
                c.prov = effective_prov(host, &c);
                c.topic = Some(Value::Str(text));
                out.push(c);
            }
            Err(why) => {
                record_refusal(why);
                return Vec::new();
            }
        }
    }
    out
}

/// `@| edges(->road; --weight=km)` / `@| nodes`: the context's graph
/// as one table value (spec: The graph as a table).
pub(crate) fn graph_table_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
) -> Vec<Thread<H::Ext>> {
    // The positions are tabled whatever the topic: a thread in
    // value mode still stands on its node (spec: The graph as a
    // table); only a thread standing nowhere (after the aggregation pipe)
    // has nothing to table.
    if caps.iter().any(|c| host.node_ref(&c.ext).is_none()) {
        record_refusal(format!(
            "{} draws the context's nodes, and a thread of the context stands nowhere \
             (after the aggregation pipe): navigate to the nodes before tabling them",
            call.name
        ));
        return Vec::new();
    }
    let what = match crate::graph::edges::GraphTable::from_call(call) {
        Ok(w) => w,
        Err(why) => {
            record_refusal(why);
            return Vec::new();
        }
    };
    let starts: Vec<H::Ext> = caps.iter().map(|c| c.ext.bare()).collect();
    let mut origins = Origins::default();
    for c in &caps {
        origins.insert(host.origin_at(&c.ext));
    }
    match host.graph_table(&starts, &what, trace) {
        Ok(table) => vec![Thread::<H::Ext> {
            prov: Prov::leaf(origins),
            register: shared_register(&caps),
            topic: Some(Value::Table(std::sync::Arc::new(table))),
            members: Vec::new(),
            captures: Vec::new(),
            named: Vec::new(),
            ext: host.unplaced_ext(),
        }],
        Err(why) => {
            record_refusal(why);
            Vec::new()
        }
    }
}

/// `@| graph:pagerank(->link; …)` — the graph library (spec: The
/// graph library): a row-aligned aggregate over the context's nodes
/// and the edges among them. The threads are folded to their
/// distinct positions in order of first arrival, the host answers
/// one value per position over that induced subgraph, and every
/// thread keeps its place with its position's value as the topic.
pub(crate) fn graph_library_command<H: Host>(
    name: &str,
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    use crate::graph::edges::GraphTable;
    if caps.iter().any(|c| host.node_ref(&c.ext).is_none()) {
        record_refusal(format!(
            "graph:{name} reads the context's nodes, and a thread of the context stands nowhere \
             (after the aggregation pipe): navigate to the nodes before ranking them"
        ));
        return Vec::new();
    }
    let what = match GraphTable::from_call(call) {
        Ok(w) => w,
        Err(why) => {
            record_refusal(why);
            return Vec::new();
        }
    };
    // the threads folded to their distinct positions
    let mut keys: Vec<<H::Ext as Attachment>::Key> = Vec::new();
    let mut starts: Vec<H::Ext> = Vec::new();
    let mut at: Vec<usize> = Vec::with_capacity(caps.len());
    for c in &caps {
        let k = c.ext.key();
        match keys.iter().position(|x| *x == k) {
            Some(i) => at.push(i),
            None => {
                at.push(keys.len());
                keys.push(k);
                starts.push(c.ext.bare());
            }
        }
    }
    // the cost and the bound (spec: The graph library; Other bounds):
    // the induced subgraph's nodes and edges, their product for
    // betweenness, refused before anything is computed
    let (n, e) = host.graph_size(&starts, &what, trace);
    let cost = if what.quadratic() {
        n.saturating_mul(e)
    } else {
        n.saturating_add(e)
    };
    let bound =
        crate::pipeline::run::run_option(|o| o.graph_bound).unwrap_or_else(|| host.graph_bound());
    if cost > bound {
        crate::value::ambient::record_refusal_coded(
            "GRAPH_BOUND",
            format!("graph:{name}"),
            format!(
                "graph:{name} would compute over {n} nodes and {e} edges, {cost} cost units{} past \
                 the graph bound {bound}: narrow the context, or raise the bound with --graph-bound",
                if what.quadratic() {
                    " (nodes times edges)"
                } else {
                    ""
                }
            ),
        );
        return Vec::new();
    }
    let seeds: Vec<<H::Ext as Attachment>::Key> = match &what {
        GraphTable::Distance { from, .. } | GraphTable::Route { from, .. } => {
            let seed = seed_capsa(&caps, host);
            let scope = seed.scope(Some(crate::pipeline::run::group_ordinal()), peers, outer);
            let root = host.unplaced_ext();
            discard(|| host.operand_keys(&root, from, trace, scope))
        }
        _ => Vec::new(),
    };
    // a declared graph (spec: Graph Declarations): the source answers
    // when the hop, the options and the context are the declared ones
    let declared = declared_measure(
        host, name, &what, &keys, &starts, &caps, trace, outer, peers,
    );
    let mut origins = Origins::default();
    for c in &caps {
        origins.insert(host.origin_at(&c.ext));
    }
    if what.answers_table() {
        // a table once for the context, a reducing aggregate
        let table = match declared.answer {
            Some(mut v) if v.len() == 1 && matches!(v[0], Value::Table(_)) => match v.remove(0) {
                Value::Table(t) => t,
                _ => unreachable!(),
            },
            _ => match host.graph_table(&starts, &what, trace) {
                Ok(t) => {
                    let t = std::sync::Arc::new(t);
                    if let Some((scope, hop, measure)) = &declared.key {
                        host.store_graph_measure(
                            scope,
                            hop,
                            measure,
                            &starts,
                            &[Value::Table(t.clone())],
                        );
                    }
                    t
                }
                Err(why) => {
                    record_refusal(why);
                    return Vec::new();
                }
            },
        };
        return vec![Thread::<H::Ext> {
            prov: Prov::leaf(origins),
            register: shared_register(&caps),
            topic: Some(Value::Table(table)),
            members: Vec::new(),
            captures: Vec::new(),
            named: Vec::new(),
            ext: host.unplaced_ext(),
        }];
    }
    if what.answers_one() {
        // the partition the attached regula names, one part index per
        // position, parts numbered by first appearance; a node with
        // nothing attached is a part of its own
        let GraphTable::Modularity { by, .. } = &what else {
            unreachable!()
        };
        let mut part_of: Vec<Option<usize>> = vec![None; starts.len()];
        let mut parts: Vec<Value> = Vec::new();
        for (i, c) in caps.iter().enumerate() {
            let position = at[i];
            if part_of[position].is_some() {
                continue;
            }
            let v = operand_scalar(host, &c.ext, by, trace, c.scope(Some(i + 1), peers, outer));
            part_of[position] = Some(match v {
                Value::Null => {
                    parts.push(Value::Null);
                    parts.len() - 1
                }
                v => match parts.iter().position(|p| *p == v) {
                    Some(k) => k,
                    None => {
                        parts.push(v);
                        parts.len() - 1
                    }
                },
            });
        }
        let part: Vec<usize> = part_of.into_iter().map(|p| p.unwrap_or(0)).collect();
        return match host.graph_reduce(&starts, &what, &part, trace) {
            Ok(value) => vec![Thread::<H::Ext> {
                prov: Prov::leaf(origins),
                register: shared_register(&caps),
                topic: Some(value),
                members: Vec::new(),
                captures: Vec::new(),
                named: Vec::new(),
                ext: host.unplaced_ext(),
            }],
            Err(why) => {
                record_refusal(why);
                Vec::new()
            }
        };
    }
    let values = match declared.answer {
        Some(v) if v.len() == starts.len() => v,
        _ => match host.graph_values(&starts, &what, &seeds, trace) {
            Ok(v) => {
                if let Some((scope, hop, measure)) = &declared.key {
                    host.store_graph_measure(scope, hop, measure, &starts, &v);
                }
                v
            }
            Err(why) => {
                record_refusal(why);
                return Vec::new();
            }
        },
    };
    caps.into_iter()
        .zip(at)
        .map(|(mut c, i)| {
            c.topic = values.get(i).cloned();
            c.prov = Prov::leaf(origins.clone());
            c
        })
        .collect()
}

/// What a declared graph gives a library command: the source's
/// answer, or the key of the declared measure the source declined
/// (the engine computes and offers the values back), or nothing.
#[derive(Default)]
struct Declared {
    key: Option<(String, String, String)>,
    answer: Option<Vec<Value>>,
}

/// `name(options)` as written in a declaration, split.
fn split_measure(m: &str) -> (&str, Option<&str>) {
    match m.split_once('(') {
        Some((n, rest)) => (n.trim(), Some(rest.trim_end_matches(')').trim())),
        None => (m.trim(), None),
    }
}

/// A declared measure read as the engine reads a command: the text
/// `graph:NAME(HOP; OPTIONS)` through the parser.
fn parse_declared(
    hop: &str,
    name: &str,
    options: Option<&str>,
) -> Result<crate::graph::edges::GraphTable, String> {
    // a head the parser accepts stands in for the context
    let text = format!(
        "/x @| graph:{name}({hop}{})",
        options
            .filter(|o| !o.is_empty())
            .map(|o| format!("; {o}"))
            .unwrap_or_default()
    );
    let tokens = crate::syntax::lexer::lex(&text).map_err(|e| e.to_string())?;
    let query = crate::syntax::parser::parse(&tokens).map_err(|e| e.to_string())?;
    let call = query
        .pipeline
        .iter()
        .find_map(|c| match c {
            crate::syntax::ast::Command::Agg(call) => Some(call),
            _ => None,
        })
        .ok_or_else(|| "no command".to_string())?;
    crate::graph::edges::GraphTable::from_call(call)
}

/// The nodes a declared scope path selects, from the root.
fn scope_keys<H: Host>(
    host: &H,
    scope_text: &str,
    caps: &[Thread<H::Ext>],
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Result<Vec<<H::Ext as Attachment>::Key>, String> {
    // the scope read as a node operand, the way `--from` takes one:
    // a path from the root, every conjunct of it applied
    let parsed = parse_declared("->*", "distance", Some(&format!("--from={scope_text}")))?;
    let crate::graph::edges::GraphTable::Distance { from: op, .. } = parsed else {
        return Err("a scope is one path".to_string());
    };
    let seed = seed_capsa(caps, host);
    let scope = seed.scope(Some(crate::pipeline::run::group_ordinal()), peers, outer);
    let root = host.unplaced_ext();
    Ok(discard(|| host.operand_keys(&root, &op, trace, scope)))
}

#[allow(clippy::too_many_arguments)]
fn declared_measure<H: Host>(
    host: &H,
    name: &str,
    what: &crate::graph::edges::GraphTable,
    keys: &[<H::Ext as Attachment>::Key],
    starts: &[H::Ext],
    caps: &[Thread<H::Ext>],
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Declared {
    use crate::diagnostic::Diagnostic;
    use crate::value::ambient::record_diagnostic;
    let decls = host.graph_declarations();
    if decls.is_empty() {
        return Declared::default();
    }
    let fingerprint = what.fingerprint();
    let mut reason: Option<String> = None;
    for d in &decls {
        for m in &d.measures {
            let (mname, mopts) = split_measure(m);
            if mname != name {
                continue;
            }
            let shown = format!(
                "{mname}({}{})",
                d.hop,
                mopts
                    .filter(|o| !o.is_empty())
                    .map(|o| format!("; {o}"))
                    .unwrap_or_default()
            );
            match parse_declared(&d.hop, mname, mopts) {
                Err(why) => {
                    reason = Some(format!(
                        "the declared {shown} on {} is not read: {why}",
                        d.scope
                    ));
                    continue;
                }
                Ok(t) if t.fingerprint() != fingerprint => {
                    reason = Some(format!(
                        "the hop or the options differ from the declared {shown} on {}",
                        d.scope
                    ));
                    continue;
                }
                Ok(_) => {}
            }
            let declared_keys = match scope_keys(host, &d.scope, caps, trace, outer, peers) {
                Ok(k) => k,
                Err(why) => {
                    reason = Some(format!("the declared scope {} is not read: {why}", d.scope));
                    continue;
                }
            };
            let inside = keys.iter().filter(|k| declared_keys.contains(k)).count();
            let entire = inside == keys.len() && declared_keys.iter().all(|k| keys.contains(k));
            if !entire {
                reason = Some(if inside == keys.len() {
                    format!(
                        "the context is {inside} of the {} nodes of {}, computed over {inside} nodes",
                        declared_keys.len(),
                        d.scope
                    )
                } else {
                    format!(
                        "the context is not the declared scope {} ({} of its nodes, {} outside it), computed over {} nodes",
                        d.scope,
                        inside,
                        keys.len() - inside,
                        keys.len()
                    )
                });
                continue;
            }
            return match host.graph_measure(&d.scope, &d.hop, m, starts) {
                Some(values) => {
                    record_diagnostic(Diagnostic::info(
                        "PUSHED",
                        "graph",
                        format!("{shown} from the declared graph on {}", d.scope),
                    ));
                    Declared {
                        key: None,
                        answer: Some(values),
                    }
                }
                None => {
                    record_diagnostic(Diagnostic::info(
                        "NOT_PUSHED",
                        "graph",
                        format!(
                            "the source has no answer for the declared {shown} on {}; computed over {} nodes",
                            d.scope,
                            keys.len()
                        ),
                    ));
                    Declared {
                        key: Some((d.scope.clone(), d.hop.clone(), m.clone())),
                        answer: None,
                    }
                }
            };
        }
    }
    if let Some(r) = reason {
        record_diagnostic(Diagnostic::info("NOT_PUSHED", "graph", r));
    }
    Declared::default()
}

pub(crate) fn outline_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    // The level and the text, each an expression or a literal.
    let exprs: Vec<Operand> = call
        .args
        .iter()
        .filter_map(|a| match a {
            Arg::Expr(e) => Some(e.clone()),
            Arg::Lit(v) => Some(Operand::Lit(v.clone())),
            _ => None,
        })
        .collect();
    // `--style="1."` numbers the lines (draft 203: an option).
    let numbered = call
        .args
        .iter()
        .any(|a| matches!(a, Arg::Opt { name, .. } if name == "--style"));
    // Without an expression a record's `depth` and `title` serve;
    // a plain value is the level itself and has no text.
    let field = |c: &Thread<H::Ext>, key: &str| match effective_topic(c, host) {
        Value::Record(o) => o
            .iter()
            .find(|(k, _)| k == key)
            .map_or(Value::Null, |(_, v)| v.clone()),
        other if key == "depth" => other,
        _ => Value::Null,
    };
    let rows: Vec<(Option<i64>, String)> = caps
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let read = |e: &Operand| {
                operand_scalar(host, &c.ext, e, trace, c.scope(Some(i + 1), peers, outer))
            };
            let level = match exprs.first() {
                Some(e) => read(e),
                None => field(c, "depth"),
            };
            let text = match exprs.get(1) {
                Some(e) => read(e),
                None => field(c, "title"),
            };
            let text = match text {
                Value::Null => String::new(),
                v => v.to_string(),
            };
            (level.numeric().map(|f| f as i64), text)
        })
        .collect();
    let levels: Vec<Option<i64>> = rows.iter().map(|(l, _)| *l).collect();
    // The levels come from data; a depth beyond any outline
    // (`MAX_OUTLINE_DEPTH` levels below the shallowest) is refused
    // rather than indented or numbered to that depth.
    if let (Some(lo), Some(hi)) = (levels.iter().flatten().min(), levels.iter().flatten().max())
        && hi.saturating_sub(*lo) > stdlib::MAX_OUTLINE_DEPTH
    {
        record_refusal(format!(
            "{}: level {hi} is more than {} levels below the shallowest ({lo})",
            call.name,
            stdlib::MAX_OUTLINE_DEPTH
        ));
        return Vec::new();
    }
    let numbers = stdlib::outline_numbers(&levels);
    caps.into_iter()
        .zip(rows)
        .zip(numbers)
        .map(|((mut c, (_, text)), (depth, number))| {
            if c.topic.is_none() {
                c.prov = host.prov_at(&c.ext);
            }
            // Without a text the numbered line is the number alone
            // (`1`, `1.1`, `2`), as `@| number` printed it.
            let line = if numbered && text.is_empty() {
                number
            } else if numbered {
                format!("{}{number} {text}", "  ".repeat(depth))
            } else {
                format!("{}{text}", "  ".repeat(depth))
            };
            c.topic = Some(Value::Str(line));
            c.members = Vec::new();
            c
        })
        .collect()
}

/// `@| zscore([sample][, key])`: each capsa's topic becomes the
/// standard score of its numeric reading over its partition (the
/// whole context without a key); a capsa with no reading, or a
/// partition with no spread, scores null. Consumes any members.
pub(crate) fn zscore_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    let sample = stdlib::sample_flag(call);
    let key = call.args.iter().find_map(|a| match a {
        Arg::Expr(e) => Some(e),
        _ => None,
    });
    let (lists, _) = peer_lists(&caps, key, host, trace, outer, peers);
    let readings: Vec<Option<f64>> = caps
        .iter()
        .map(|c| effective_topic(c, host).numeric())
        .collect();
    let mut scores = vec![Value::Null; caps.len()];
    for list in &lists {
        let xs: Vec<Option<f64>> = list.iter().map(|&i| readings[i]).collect();
        for (&i, z) in list.iter().zip(stdlib::zscores(&xs, sample)) {
            scores[i] = z;
        }
    }
    caps.into_iter()
        .zip(scores)
        .map(|(mut c, z)| {
            c.prov = effective_prov(host, &c);
            c.topic = Some(z);
            c.members = Vec::new();
            c
        })
        .collect()
}

/// The paired numeric readings of an association call's two
/// expressions, one pair per capsa that reads on both sides.
pub(crate) fn pair_readings<H: Host>(
    call: &FnCall,
    caps: &[Thread<H::Ext>],
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<(f64, f64)> {
    let exprs: Vec<&Operand> = call
        .args
        .iter()
        .filter_map(|a| match a {
            Arg::Expr(e) => Some(e),
            _ => None,
        })
        .collect();
    if exprs.len() != 2 {
        return Vec::new();
    }
    caps.iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let read = |e: &Operand| {
                operand_scalar(host, &c.ext, e, trace, c.scope(Some(i + 1), peers, outer)).numeric()
            };
            Some((read(exprs[0])?, read(exprs[1])?))
        })
        .collect()
}

/// The paired values of an agreement call's two expressions, one
/// pair per capsa that reads on both sides (a label may be any
/// value; only a null drops the pair).
pub(crate) fn pair_values<H: Host>(
    call: &FnCall,
    caps: &[Thread<H::Ext>],
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<(Value, Value)> {
    let exprs: Vec<&Operand> = call
        .args
        .iter()
        .filter_map(|a| match a {
            Arg::Expr(e) => Some(e),
            _ => None,
        })
        .collect();
    if exprs.len() != 2 {
        return Vec::new();
    }
    caps.iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let read = |e: &Operand| {
                let v = operand_scalar(host, &c.ext, e, trace, c.scope(Some(i + 1), peers, outer));
                (!matches!(v, Value::Null)).then_some(v)
            };
            Some((read(exprs[0])?, read(exprs[1])?))
        })
        .collect()
}

/// An association or agreement call reduced over a set of capsae.
pub(crate) fn associate_caps<H: Host>(
    call: &FnCall,
    caps: &[Thread<H::Ext>],
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Value {
    if stdlib::agreement(&call.name) {
        stdlib::agree(
            &call.name,
            &pair_values(call, caps, host, trace, outer, peers),
        )
    } else {
        stdlib::associate(
            &call.name,
            &pair_readings(call, caps, host, trace, outer, peers),
        )
    }
}

/// `@| corr(:a; :b)` over the whole context: one capsa whose topic
/// is the association and whose origins are every input's.
pub(crate) fn association_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    let value = associate_caps(call, &caps, host, trace, outer, peers);
    let mut origins = Origins::default();
    for c in &caps {
        match &c.topic {
            Some(_) => origins.union(&c.prov.origins),
            None => origins.insert(host.origin_at(&c.ext)),
        }
    }
    // the measure's one thread has no position (spec: Aggregating
    // Pipe)
    let node = host.unplaced_ext();
    vec![Thread::<H::Ext> {
        prov: Prov::leaf(origins),
        // the regulae every capsa shares, as every other reduction
        // keeps them (`| .n(1) @| corr(…) | $.n` answered null where
        // `@| sum(…) | $.n` answers 1)
        register: shared_register(&caps),
        topic: Some(value),
        members: Vec::new(),
        captures: Vec::new(),
        named: Vec::new(),
        ext: node.clone(),
    }]
}
