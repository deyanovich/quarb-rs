//! `quarb-cypher` — what the Cypher-speaking adapters share: the
//! literal and identifier spellings, the statements every store
//! answers the same way, and the per-node relationship cache a
//! hop fills in one statement. The transports stay with the
//! adapters (HTTP, Bolt, the Redis protocol, SQL, an embedded
//! engine); this crate holds the Cypher.

use quarb::{NodeId, Value};
use std::cell::RefCell;
use std::collections::HashMap;

/// A Cypher literal for a Quarb value: numbers and booleans as
/// written, everything else a single-quoted string with the
/// backslash and the quote escaped.
pub fn literal(v: &Value) -> String {
    match v {
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        other => format!(
            "'{}'",
            other.to_string().replace('\\', "\\\\").replace('\'', "\\'")
        ),
    }
}

/// A label, type or property name in backticks, a backtick inside
/// doubled.
pub fn bt(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// The relationship pattern for a direction: `-[r]->` out of the
/// node, `<-[r]-` into it.
pub fn arrow(incoming: bool) -> &'static str {
    if incoming { "<-[r]-" } else { "-[r]->" }
}

/// The relationships of one node (`id_expr` names it, e.g.
/// `id(n) = 42`), relationship and other node per row, in a
/// stable order.
pub fn edges_stmt(id_expr: &str, incoming: bool) -> String {
    format!(
        "MATCH (n){}(m) WHERE {id_expr} RETURN r, m ORDER BY type(r), id(m)",
        arrow(incoming)
    )
}

/// The relationships of many nodes at once (`ids_expr` names the
/// set, e.g. `id(n) IN [1, 2, 3]`), with the source node's id first
/// so the rows group back per node.
pub fn edges_batch_stmt(ids_expr: &str, incoming: bool) -> String {
    format!(
        "MATCH (n){}(m) WHERE {ids_expr} RETURN id(n), r, m ORDER BY id(n), type(r), id(m)",
        arrow(incoming)
    )
}

/// Every node under a label, ordered by `order` (a key expression
/// or `id(m)`).
pub fn label_listing_stmt(label: &str, order: &str) -> String {
    format!("MATCH (m:{}) RETURN m ORDER BY {order}", bt(label))
}

/// A node's relationships once read, per direction — `links` and
/// `backlinks` answer from here, and a hop's prefetch fills it for
/// every node of the hop in one statement per chunk.
#[derive(Default)]
pub struct EdgeCache {
    edges: RefCell<HashMap<(NodeId, bool), Vec<(String, NodeId)>>>,
}

/// How many nodes one prefetch statement names.
pub const PREFETCH_CHUNK: usize = 500;

impl EdgeCache {
    pub fn get(&self, node: NodeId, incoming: bool) -> Option<Vec<(String, NodeId)>> {
        self.edges.borrow().get(&(node, incoming)).cloned()
    }

    pub fn put(&self, node: NodeId, incoming: bool, edges: Vec<(String, NodeId)>) {
        self.edges.borrow_mut().insert((node, incoming), edges);
    }

    pub fn has(&self, node: NodeId, incoming: bool) -> bool {
        self.edges.borrow().contains_key(&(node, incoming))
    }

    /// The nodes of `nodes` not yet cached for `incoming`, in
    /// chunks of [`PREFETCH_CHUNK`]; `fetch` answers one chunk as
    /// `(node, edges)` pairs (a node with no relationships answers
    /// with an empty list) or `None` when the store refused, in
    /// which case nothing is cached and the per-node reads stand.
    pub fn fill<F>(&self, nodes: &[NodeId], incoming: bool, mut fetch: F)
    where
        F: FnMut(&[NodeId]) -> Option<Vec<(NodeId, Vec<(String, NodeId)>)>>,
    {
        let wanted: Vec<NodeId> = nodes
            .iter()
            .copied()
            .filter(|&n| !self.has(n, incoming))
            .collect();
        if wanted.len() < 2 {
            return;
        }
        for chunk in wanted.chunks(PREFETCH_CHUNK) {
            let Some(found) = fetch(chunk) else {
                return;
            };
            let mut by_node: HashMap<NodeId, Vec<(String, NodeId)>> = found.into_iter().collect();
            let mut cache = self.edges.borrow_mut();
            for &n in chunk {
                cache.insert((n, incoming), by_node.remove(&n).unwrap_or_default());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_and_names() {
        assert_eq!(literal(&Value::Int(3)), "3");
        assert_eq!(literal(&Value::Str("it's".into())), "'it\\'s'");
        assert_eq!(bt("a`b"), "`a``b`");
        assert_eq!(
            edges_stmt("id(n) = 7", false),
            "MATCH (n)-[r]->(m) WHERE id(n) = 7 RETURN r, m ORDER BY type(r), id(m)"
        );
    }

    #[test]
    fn cache_fills_in_chunks_and_keeps_empties() {
        let c = EdgeCache::default();
        let nodes: Vec<NodeId> = (0..3).map(NodeId).collect();
        c.fill(&nodes, false, |chunk| {
            Some(
                chunk
                    .iter()
                    .map(|&n| {
                        (
                            n,
                            if n.0 == 1 {
                                vec![("r".to_string(), NodeId(9))]
                            } else {
                                vec![]
                            },
                        )
                    })
                    .collect(),
            )
        });
        assert_eq!(c.get(NodeId(1), false).unwrap().len(), 1);
        assert_eq!(c.get(NodeId(2), false).unwrap().len(), 0);
        assert!(c.get(NodeId(2), true).is_none());
    }
}
