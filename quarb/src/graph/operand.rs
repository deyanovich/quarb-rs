//! Part of the executor: see `exec/mod.rs`.

use super::navigate::ref_nodes;
use super::*;

/// [`subcontext_scalar`] with the reduced value's provenance: one
/// value's own, several as a list, a node result the nodes'.
pub(crate) fn subcontext_traced(
    body: &Query,
    adapter: &impl AstAdapter,
    node: NodeId,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
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
                position: (c.ext.node).into(),
                command: command_now(),
            });
        }
        (Value::Int(caps.len() as i64), Prov::leaf(o))
    }
}

/// A node's default scalar (its name), used when an aggregation runs
/// over an unprojected context.
pub(crate) fn node_scalar(adapter: &impl AstAdapter, node: NodeId) -> Value {
    adapter.name(node).map_or(Value::Null, Value::Str)
}

/// The node set a bare node operand denotes — a path from the
/// current node (the anchor's rule), a correlation context
/// reference, the invoking position — deduplicated, in
/// walk order. The node side of the cross-capsa read.
/// The nodes a relative path reaches from the operand's base: from
/// each anchor node, the steps walked under the witness in force
/// (the tuple under test in a predicate, else the capsa's), the
/// capsa's topic reachable as `$_` in the path's predicates, the
/// members cleared (a path reads the node, not the group), deduped.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rel_nodes(
    adapter: &impl AstAdapter,
    node: NodeId,
    steps: &[PathElem],
    anchor: &Anchor,
    trace: &Trace,
    bound: &[Option<NodeId>],
    scope: Scope<'_, GraphExt>,
    node_level: bool,
) -> Vec<NodeId> {
    let witness = if bound.is_empty() {
        witness_of(&scope)
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
            nodes.extend(navigate_from_level(
                steps,
                adapter,
                from,
                trace,
                nested_outer(&inner),
                witness,
                inner.register,
                node_level,
            ));
        }
    });
    // one node per walk, as the top-level walk answers (spec:
    // Expansion paths); the anchors' own duplicates folded
    nodes
}

pub(crate) fn operand_nodes(
    adapter: &impl AstAdapter,
    node: NodeId,
    at: &Operand,
    trace: &Trace,
    bound: &[Option<NodeId>],
    scope: Scope<'_, GraphExt>,
) -> Vec<NodeId> {
    match at {
        Operand::Rel { steps, anchor, .. } => {
            rel_nodes(adapter, node, steps, anchor, trace, bound, scope, false)
        }
        Operand::Ctx { index, steps, .. } => {
            let bound = if bound.is_empty() {
                witness_of(&scope)
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
                navigate_from(
                    steps,
                    adapter,
                    base,
                    trace,
                    nested_outer(&scope),
                    bound,
                    scope.register,
                )
            }
        }
        Operand::Invoking(inner) => match scope.outer {
            Some(o) => operand_nodes(
                adapter,
                o.view.node.unwrap_or(node),
                inner,
                trace,
                bound,
                *o,
            ),
            None => Vec::new(),
        },
        Operand::Position(inner) => operand_nodes(adapter, node, inner, trace, bound, scope),
        Operand::Driver(inner) => match scope.driver {
            Some(d) => operand_nodes(
                adapter,
                d.view.node.unwrap_or(node),
                inner,
                trace,
                bound,
                *d,
            ),
            None => Vec::new(),
        },
        // A record's field holding node references (`:source` on an
        // edges row): the nodes it refers to, in order; a value that
        // is no reference names no node.
        Operand::Field { .. } => eval_operand_at(adapter, node, at, trace, bound, scope)
            .iter()
            .flat_map(ref_nodes)
            .collect(),
        _ => Vec::new(),
    }
}

/// A witness other than the scope's own, tried in the scope's slot.
fn at_witness<'a>(scope: Scope<'a, GraphExt>, bound: &'a [Option<NodeId>]) -> Scope<'a, GraphExt> {
    if bound.is_empty() {
        scope
    } else {
        Scope {
            witness: Some(bound),
            ..scope
        }
    }
}

pub(crate) fn eval_operand_at(
    adapter: &impl AstAdapter,
    node: NodeId,
    operand: &Operand,
    trace: &Trace,
    bound: &[Option<NodeId>],
    scope: Scope<'_, GraphExt>,
) -> Vec<Value> {
    let scope = at_witness(scope, bound);
    eval_operand(adapter, &GraphExt::at(node), operand, trace, scope)
}

pub(crate) fn operand_value_at(
    adapter: &impl AstAdapter,
    node: NodeId,
    expr: &Operand,
    trace: &Trace,
    bound: &[Option<NodeId>],
    scope: Scope<'_, GraphExt>,
) -> Value {
    let scope = at_witness(scope, bound);
    operand_value(adapter, &GraphExt::at(node), expr, trace, scope)
}

pub(crate) fn eval_pred_expr_at(
    adapter: &impl AstAdapter,
    node: NodeId,
    e: &PredExpr,
    trace: &Trace,
    bound: &[Option<NodeId>],
    scope: Scope<'_, GraphExt>,
) -> bool {
    let scope = at_witness(scope, bound);
    eval_pred_expr(adapter, &GraphExt::at(node), e, trace, scope)
}
