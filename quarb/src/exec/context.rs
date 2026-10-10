//! Part of the executor: see `exec/mod.rs`.

use super::*;

/// The provenance of a capsa's effective topic: the topic's, else
/// the node read at this command.
pub(super) fn effective_prov(c: &Capsa) -> Prov {
    match &c.topic {
        Some(_) => c.prov.clone(),
        None => Prov::at(c.node, command_now()),
    }
}

/// A capsa's effective topic: the topic if set, else the node's
/// default scalar.
pub(super) fn effective_topic(c: &Capsa, adapter: &impl AstAdapter) -> Value {
    c.topic
        .clone()
        .unwrap_or_else(|| node_scalar(adapter, c.node))
}

/// Partition the context for `window` / `shift`: for each capsa, the
/// ordered list of context indices it may reach and its own position
/// within that list. Without a key there is one list — the whole
/// context. With a key, capsae whose keys compare equal share a
/// list, in original context order; a null key matches nothing, so
/// its capsa stands alone (the ordering counterpart of "a missing
/// key does not compete").
pub(super) fn peer_lists(
    caps: &[Capsa],
    key: Option<&Operand>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
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
        let k = operand_scalar(adapter, c.node, key, trace, scope);
        // A null key partitions alone (no two nulls share a list).
        let slot = if matches!(k, Value::Null) {
            None
        } else {
            slots
                .get(&k.sort_key_with(&|e| adapter.unit_scale(e)))
                .copied()
        };
        match slot {
            Some(li) => {
                lists[li].push(i);
                of.push((li, lists[li].len() - 1));
            }
            None => {
                if !matches!(k, Value::Null) {
                    slots.insert(k.sort_key_with(&|e| adapter.unit_scale(e)), lists.len());
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
pub(super) fn window_args(call: &FnCall) -> (Option<i64>, Option<i64>, Option<&Operand>) {
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
pub(super) fn window_command(
    call: &FnCall,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Vec<Capsa> {
    let (from, to, key) = window_args(call);
    let (lists, of) = peer_lists(&caps, key, adapter, trace, outer, peers);
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
    let members: Vec<Vec<Capsa>> = windows
        .iter()
        .map(|w| w.iter().map(|&j| caps[j].clone()).collect())
        .collect();
    caps.into_iter()
        .zip(members)
        .map(|(mut c, members)| {
            c.topic = Some(Value::list(
                members
                    .iter()
                    .map(|m| effective_topic(m, adapter))
                    .collect(),
            ));
            c.prov = Prov::list(members.iter().map(effective_prov).collect());
            c.members = members;
            c
        })
        .collect()
}

/// `@| shift(n[, key])`: replace each capsa's topic with the
/// effective topic of the capsa `n` positions back in its partition
/// (negative `n` looks forward; null past the edge). Consumes any
/// members — they no longer correspond to the topic.
pub(super) fn shift_command(
    call: &FnCall,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Vec<Capsa> {
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
    let (lists, of) = peer_lists(&caps, key, adapter, trace, outer, peers);
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
                (effective_topic(src, adapter), effective_prov(src))
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
pub(super) fn timeline_command(
    call: &FnCall,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Vec<Capsa> {
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
                Arg::Lit(v) => v.clone(),
                Arg::Range(..) | Arg::Named { .. } | Arg::Opt { .. } => Value::Null,
                Arg::Expr(e) => operand_scalar(
                    adapter,
                    c.node,
                    e,
                    trace,
                    c.scope(Some(i + 1), peers, outer),
                ),
            };
            let expr = match args.first() {
                Some(a) => read(a),
                None => effective_topic(c, adapter),
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
                c.prov = Prov::at(c.node, command_now());
            }
            let mut fields = match expr {
                Value::Record(fields) => fields,
                other => vec![("value".to_string(), other)],
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
/// `@| outline(level; text)` prints the line instead: the text
/// indented by its depth below the shallowest level, and behind its
/// number when the style `"1."` is given. Bare `outline` reads the
/// fields `depth` and `title` of a record stream. Consumes any
/// members.
pub(super) fn outline_command(
    call: &FnCall,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Vec<Capsa> {
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
    let field = |c: &Capsa, key: &str| match effective_topic(c, adapter) {
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
                operand_scalar(
                    adapter,
                    c.node,
                    e,
                    trace,
                    c.scope(Some(i + 1), peers, outer),
                )
            };
            let level = match exprs.first() {
                Some(e) => read(e),
                None if lines => field(c, "depth"),
                None => effective_topic(c, adapter),
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
                c.prov = Prov::at(c.node, command_now());
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
pub(super) fn zscore_command(
    call: &FnCall,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Vec<Capsa> {
    let sample = stdlib::sample_flag(call);
    let key = call.args.iter().find_map(|a| match a {
        Arg::Expr(e) => Some(e),
        _ => None,
    });
    let (lists, _) = peer_lists(&caps, key, adapter, trace, outer, peers);
    let readings: Vec<Option<f64>> = caps
        .iter()
        .map(|c| effective_topic(c, adapter).numeric())
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
            c.prov = effective_prov(&c);
            c.topic = Some(z);
            c.members = Vec::new();
            c
        })
        .collect()
}

/// The paired numeric readings of an association call's two
/// expressions, one pair per capsa that reads on both sides.
pub(super) fn pair_readings(
    call: &FnCall,
    caps: &[Capsa],
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
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
                operand_scalar(
                    adapter,
                    c.node,
                    e,
                    trace,
                    c.scope(Some(i + 1), peers, outer),
                )
                .numeric()
            };
            Some((read(exprs[0])?, read(exprs[1])?))
        })
        .collect()
}

/// The paired values of an agreement call's two expressions, one
/// pair per capsa that reads on both sides (a label may be any
/// value; only a null drops the pair).
pub(super) fn pair_values(
    call: &FnCall,
    caps: &[Capsa],
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
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
                let v = operand_scalar(
                    adapter,
                    c.node,
                    e,
                    trace,
                    c.scope(Some(i + 1), peers, outer),
                );
                (!matches!(v, Value::Null)).then_some(v)
            };
            Some((read(exprs[0])?, read(exprs[1])?))
        })
        .collect()
}

/// An association or agreement call reduced over a set of capsae.
pub(super) fn associate_caps(
    call: &FnCall,
    caps: &[Capsa],
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Value {
    if stdlib::agreement(&call.name) {
        stdlib::agree(
            &call.name,
            &pair_values(call, caps, adapter, trace, outer, peers),
        )
    } else {
        stdlib::associate(
            &call.name,
            &pair_readings(call, caps, adapter, trace, outer, peers),
        )
    }
}

/// `@| corr(:a; :b)` over the whole context: one capsa whose topic
/// is the association and whose origins are every input's.
pub(super) fn association_command(
    call: &FnCall,
    caps: Vec<Capsa>,
    adapter: &impl AstAdapter,
    trace: &Trace,
    outer: Option<&Scope<'_>>,
    peers: Option<&Peers>,
) -> Vec<Capsa> {
    let value = associate_caps(call, &caps, adapter, trace, outer, peers);
    let mut origins = Origins::default();
    for c in &caps {
        match &c.topic {
            Some(_) => origins.union(&c.prov.origins),
            None => origins.insert(Origin {
                node: c.node,
                command: command_now(),
            }),
        }
    }
    let node = caps.first().map_or(adapter.root(), |c| c.node);
    vec![Capsa {
        prov: Prov::leaf(origins),
        node,
        register: Vec::new(),
        topic: Some(value),
        members: Vec::new(),
        captures: Vec::new(),
        named: Vec::new(),
        marks: Vec::new(),
        bindings: Vec::new(),
        arrived: Vec::new(),
    }]
}
