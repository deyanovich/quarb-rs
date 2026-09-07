//! Compose forwards a graft's references: a text-level ref that
//! leaves its document lands on the sibling leaf serving that URL
//! through `with_document_urls`, fragment included, and trait
//! tests reach the graft.

use quarb::{AstAdapter, NodeId, QueryResult};
use quarb_compose::{ComposeAdapter, DocumentGraft};

/// A two-page site as an in-memory tree: root → two html leaves.
struct Pages(Vec<(&'static str, &'static str)>);

impl AstAdapter for Pages {
    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if node.0 == 0 { (1..=self.0.len() as u64).map(NodeId).collect() } else { Vec::new() }
    }
    fn name(&self, node: NodeId) -> Option<String> {
        self.0.get(node.0.checked_sub(1)? as usize).map(|(n, _)| n.to_string())
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        (node.0 > 0).then_some(NodeId(0))
    }
    fn default_value(&self, node: NodeId) -> Option<quarb::Value> {
        self.0.get(node.0.checked_sub(1)? as usize).map(|(_, h)| quarb::Value::Str(h.to_string()))
    }
}

fn url_of(a: &Pages, n: NodeId) -> Option<String> {
    a.name(n).map(|name| format!("https://example.org/{name}"))
}

#[test]
fn a_ref_lands_on_the_sibling_document() {
    let pages = Pages(vec![
        ("a.html", "<html><head><title>A</title><meta name=\"keywords\" content=\"alpha\"></head><body><p>Go to <a href=\"b.html#part\">B</a>.</p></body></html>"),
        ("b.html", "<html><head><title>B</title></head><body><h2 id=\"part\">Part</h2><p>Here.</p></body></html>"),
    ]);
    let a = ComposeAdapter::new(pages)
        .with_document_graft(DocumentGraft::Text)
        .with_document_urls(url_of);
    let run = |q: &str| match quarb::run(q, &a).unwrap() {
        QueryResult::Values(v) => v.into_iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        QueryResult::Nodes(n) => n.into_iter().map(|n| a.locator(n, |o| format!("/{}", a.outer().name(o).unwrap_or_default()))).collect(),
    };
    assert_eq!(run("/a.html//ref-->::lemma"), ["Part"]);
    assert_eq!(run("/a.html//ref-->"), ["/b.html!/section"]);
    // The outer leaf wears the graft's declared identity for the
    // trait test too, not only in `traits`.
    assert_eq!(run("/*<tag:alpha>::title"), ["A"]);
}
