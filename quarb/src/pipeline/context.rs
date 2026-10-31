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

/// A capsa's effective topic: the topic if set, else the node's
/// default scalar.
pub(crate) fn effective_topic<H: Host>(c: &Thread<H::Ext>, host: &H) -> Value {
    c.topic.clone().unwrap_or_else(|| host.node_scalar(&c.ext))
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
    let (forward, args) = match call.args.split_last() {
        Some((Arg::Lit(Value::Str(w)), rest)) if w == "forward" => (true, rest),
        _ => (false, call.args.as_slice()),
    };
    let rows: Vec<(Value, Option<Value>)> = caps
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let read = |a: &Arg| match a {
                Arg::Lit(v) | Arg::Key(v) => v.clone(),
                Arg::Range(..) | Arg::Named { .. } | Arg::Opt { .. } | Arg::Pairs { .. } => {
                    Value::Null
                }
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

/// `@| number[(level)]`: each capsa's topic becomes its dotted number
/// (`1`, `1.1`, `1.2`, `2`) in the stream of levels — the level
/// expression read per capsa, or the value at hand without one.
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

/// `@| edges(->road; --weight=km)` / `@| nodes`: the context's graph
/// as one table value (spec: The graph as a table).
pub(crate) fn graph_table_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
) -> Vec<Thread<H::Ext>> {
    // The positions are tabled whatever the topic: a thread in
    // value mode still stands on its node (spec: The graph as a
    // table); only a thread standing nowhere (after an aggregate)
    // has nothing to table.
    if caps.iter().any(|c| host.node_ref(&c.ext).is_none()) {
        record_refusal(format!(
            "{} draws the context's nodes, and a thread of the context stands nowhere \
             (after an aggregate): navigate to the nodes before tabling them",
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
    match host.graph_table(&starts, &what) {
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
    // the positions are read whatever the topic (spec: The graph
    // library); a thread standing nowhere has none
    if caps.iter().any(|c| host.node_ref(&c.ext).is_none()) {
        record_refusal(format!(
            "graph:{name} reads the context's nodes, and a thread of the context stands nowhere \
             (after an aggregate): navigate to the nodes before ranking them"
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
    // `--from=NODES`: the seeds, an anchored node operand resolved
    // from the arbor's root (a path from the root), the shared
    // register (a mark) or an id anchor — never from one thread's
    // position, the context being many
    let seeds: Vec<<H::Ext as Attachment>::Key> = match &what {
        crate::graph::edges::GraphTable::Distance { from, .. } => {
            let seed = seed_capsa(&caps, host);
            let scope = seed.scope(None, peers, outer);
            let root = host.unplaced_ext();
            discard(|| host.operand_keys(&root, from, trace, scope))
        }
        _ => Vec::new(),
    };
    let values = match host.graph_values(&starts, &what, &seeds) {
        Ok(v) => v,
        Err(why) => {
            record_refusal(why);
            return Vec::new();
        }
    };
    let mut origins = Origins::default();
    for c in &caps {
        origins.insert(host.origin_at(&c.ext));
    }
    caps.into_iter()
        .zip(at)
        .map(|(mut c, i)| {
            c.topic = values.get(i).cloned();
            c.prov = Prov::leaf(origins.clone());
            c
        })
        .collect()
}

/// `@| outline(level; text)` prints the line instead: the text
/// indented by its depth below the shallowest level, and behind its
/// number when the style `"1."` is given. Bare `outline` reads the
/// fields `depth` and `title` of a record stream. Consumes any
/// members.
pub(crate) fn outline_command<H: Host>(
    call: &FnCall,
    caps: Vec<Thread<H::Ext>>,
    host: &H,
    trace: &H::Trace,
    outer: Option<&Scope<'_, H::Ext>>,
    peers: Option<&Peers<H::Ext>>,
) -> Vec<Thread<H::Ext>> {
    let exprs: Vec<&Operand> = call
        .args
        .iter()
        .filter_map(|a| match a {
            Arg::Expr(e) => Some(e),
            _ => None,
        })
        .collect();
    let lines = call.name == "outline";
    let numbered = !lines || call.args.iter().any(|a| matches!(a, Arg::Lit(_)));
    let field = |c: &Thread<H::Ext>, key: &str| match effective_topic(c, host) {
        Value::Record(o) => o
            .iter()
            .find(|(k, _)| k == key)
            .map_or(Value::Null, |(_, v)| v.clone()),
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
                None if lines => field(c, "depth"),
                None => effective_topic(c, host),
            };
            let text = match exprs.get(1) {
                Some(e) => read(e),
                None if lines => field(c, "title"),
                None => Value::Null,
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
            let line = if !lines {
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
        register: Vec::new(),
        topic: Some(value),
        members: Vec::new(),
        captures: Vec::new(),
        named: Vec::new(),
        ext: node.clone(),
    }]
}
