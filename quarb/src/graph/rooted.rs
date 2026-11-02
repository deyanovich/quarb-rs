//! A root on a mount (spec: Mounts, the root option; ruling
//! 2026-11-01): `?root=PATH` on any source mounts the subtree at
//! PATH as the tree. Paths print relative to it, `//` sweeps it,
//! document order runs over it, a declared graph's scope is read
//! inside it. What lies above or beside the root is not mounted at
//! all, not reachable through a crosslink either: a hop that would
//! leave the root answers nothing, as a link to a dangling redirect
//! stays on the redirect. Explicit only, never inferred; a PATH no
//! node matches refuses at mount.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::graph::adapter::{AstAdapter, Provenance, ProvenanceList, Selection, StepPlan};
use crate::{NodeId, Value};

/// The adapter `inner` re-rooted at one of its nodes. Node ids are
/// the inner's own, so every reading forwards; the membership test
/// (the parent chain reaches the root) gates whatever would reach
/// out of the subtree.
pub struct Rooted<A: AstAdapter> {
    pub inner: A,
    root: NodeId,
    /// The root's path within the source, as the inner's provenance
    /// spells it, so the paths it reports come back relative.
    root_path: Option<String>,
    inside: RefCell<HashMap<NodeId, bool>>,
}

impl<A: AstAdapter> Rooted<A> {
    /// Re-root `inner` at the one node `path` names from its root
    /// (`/sites/quarb.org`, `/a/b`): each segment a child by name.
    /// A path no node matches, or several, refuses.
    pub fn at(inner: A, path: &str) -> Result<Self, String> {
        let mut cur = inner.root();
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            let mut kids = inner.children_named(cur, seg);
            if kids.is_empty() {
                kids = inner
                    .children(cur)
                    .into_iter()
                    .filter(|&c| inner.answers_to(c, seg))
                    .collect();
            }
            match kids.len() {
                1 => cur = kids[0],
                0 => {
                    return Err(format!(
                        "root: no node at {path:?} ('{seg}' names nothing under {})",
                        name_path(&inner, cur)
                    ));
                }
                n => {
                    return Err(format!(
                        "root: {path:?} names {n} nodes at '{seg}'; the root must be one node"
                    ));
                }
            }
        }
        let root_path = inner
            .provenance(cur)
            .path
            .filter(|p| p != "/")
            .or_else(|| (cur != inner.root()).then(|| name_path(&inner, cur)));
        Ok(Rooted {
            inner,
            root: cur,
            root_path,
            inside: RefCell::new(HashMap::new()),
        })
    }

    /// Whether `n` stands at or below the root.
    fn inside(&self, n: NodeId) -> bool {
        if n == self.root {
            return true;
        }
        if let Some(&b) = self.inside.borrow().get(&n) {
            return b;
        }
        let mut cur = self.inner.parent(n);
        let mut top = n;
        let mut found = false;
        let mut steps = 0usize;
        while let Some(p) = cur {
            if p == self.root {
                found = true;
                break;
            }
            top = p;
            steps += 1;
            if steps > 1_000_000 {
                break;
            }
            cur = self.inner.parent(p);
        }
        // A chain that ends short of the source's own root is a
        // graft on demand (`graft(NOTATION)`), a free root the query
        // made: it belongs to no subtree and is kept under any.
        if !found && cur.is_none() && steps <= 1_000_000 && top != self.inner.root() {
            found = true;
        }
        self.inside.borrow_mut().insert(n, found);
        found
    }

    fn keep(&self, nodes: Vec<NodeId>) -> Vec<NodeId> {
        nodes.into_iter().filter(|&n| self.inside(n)).collect()
    }

    fn keep_labelled(&self, rows: Vec<(String, NodeId)>) -> Vec<(String, NodeId)> {
        rows.into_iter().filter(|(_, n)| self.inside(*n)).collect()
    }

    fn keep_rows(
        &self,
        rows: Vec<(String, NodeId, Option<NodeId>)>,
    ) -> Vec<(String, NodeId, Option<NodeId>)> {
        rows.into_iter()
            .filter(|(_, n, _)| self.inside(*n))
            .collect()
    }

    /// A provenance path made relative to the root's.
    fn relative(&self, p: Provenance) -> Provenance {
        let Some(prefix) = &self.root_path else {
            return p;
        };
        let path = p.path.map(|path| match path.strip_prefix(prefix.as_str()) {
            Some(rest) if rest.is_empty() => "/".to_string(),
            Some(rest) if rest.starts_with('/') => rest.to_string(),
            _ => path,
        });
        Provenance { path, ..p }
    }
}

fn name_path<A: AstAdapter>(a: &A, node: NodeId) -> String {
    let mut parts = Vec::new();
    let mut cur = Some(node);
    while let Some(n) = cur {
        if n == a.root() {
            break;
        }
        parts.push(a.name(n).unwrap_or_else(|| "*".to_string()));
        cur = a.parent(n);
    }
    parts.reverse();
    format!("/{}", parts.join("/"))
}

impl<A: AstAdapter> AstAdapter for Rooted<A> {
    crate::forward_ast_adapter!(inner; except root, parent, non_tree_nodes, tree_identities,
        links, backlinks, link_rows, edge_rows, backlink_rows, resolve, resolve_fragment,
        document_by_ref, reverse_resolve, index_lookup, index_referrers, has_node,
        provenance, provenance_list, select);

    fn root(&self) -> NodeId {
        self.root
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        if node == self.root {
            None
        } else {
            self.inner.parent(node)
        }
    }
    fn non_tree_nodes(&self) -> Vec<NodeId> {
        self.keep(self.inner.non_tree_nodes())
    }
    fn tree_identities(&self) -> Vec<(String, NodeId)> {
        self.keep_labelled(self.inner.tree_identities())
    }
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.keep_labelled(self.inner.links(node))
    }
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.keep_labelled(self.inner.backlinks(node))
    }
    fn link_rows(&self, node: NodeId) -> Vec<(String, NodeId, Option<NodeId>)> {
        self.keep_rows(self.inner.link_rows(node))
    }
    fn edge_rows(&self, node: NodeId) -> Vec<(String, NodeId, Option<NodeId>)> {
        self.keep_rows(self.inner.edge_rows(node))
    }
    fn backlink_rows(&self, node: NodeId) -> Vec<(String, NodeId, Option<NodeId>)> {
        self.keep_rows(self.inner.backlink_rows(node))
    }
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        self.inner
            .resolve(node, property, hint)
            .filter(|&t| self.inside(t))
    }
    fn resolve_fragment(&self, node: NodeId, fragment: &str) -> Option<NodeId> {
        self.inner
            .resolve_fragment(node, fragment)
            .filter(|&t| self.inside(t))
    }
    fn document_by_ref(&self, id: &str) -> Option<NodeId> {
        self.inner.document_by_ref(id).filter(|&t| self.inside(t))
    }
    fn reverse_resolve(
        &self,
        node: NodeId,
        property: Option<&str>,
        hint: Option<&str>,
    ) -> Option<Vec<NodeId>> {
        self.inner
            .reverse_resolve(node, property, hint)
            .map(|v| self.keep(v))
    }
    fn index_lookup(&self, scope: &str, property: &str, value: &Value) -> Option<Vec<NodeId>> {
        self.inner
            .index_lookup(scope, property, value)
            .map(|v| self.keep(v))
    }
    fn index_referrers(
        &self,
        scope: &str,
        property: &str,
        label: Option<&str>,
        target: NodeId,
    ) -> Option<Vec<NodeId>> {
        self.inner
            .index_referrers(scope, property, label, target)
            .map(|v| self.keep(v))
    }
    fn has_node(&self, node: NodeId) -> bool {
        self.inner.has_node(node) && self.inside(node)
    }
    fn provenance(&self, node: NodeId) -> Provenance {
        self.relative(self.inner.provenance(node))
    }
    fn provenance_list(&self, node: NodeId) -> ProvenanceList {
        let list = self.inner.provenance_list(node);
        ProvenanceList {
            entries: list.entries.into_iter().map(|p| self.relative(p)).collect(),
            ..list
        }
    }
    fn select(&self, from: NodeId, plan: &StepPlan<'_>) -> Option<Selection> {
        self.inner.select(from, plan).map(|s| Selection {
            nodes: s
                .nodes
                .into_iter()
                .filter(|(n, _)| self.inside(*n))
                .collect(),
            ..s
        })
    }
}
