//! End-to-end tests: one query spanning several mounted documents.

use quarb::{AstAdapter, NodeId, Value};
use quarb_csv::CsvAdapter;
use quarb_json::JsonAdapter;
use quarb_mount::{Mount, MountAdapter, Shared};

fn mounted() -> MountAdapter {
    let people = CsvAdapter::parse("name,city\nAda,Paris\nBo,London\n").unwrap();
    let cities = JsonAdapter::parse(
        r#"{"cities": [
            {"city": "Paris", "country": "FR"},
            {"city": "London", "country": "UK"}
        ]}"#,
    )
    .unwrap();
    MountAdapter::new(vec![
        Mount {
            name: "people".into(),
            adapter: Box::new(people),
        },
        Mount {
            name: "cities".into(),
            adapter: Box::new(cities),
        },
    ])
}

fn values(query: &str) -> Vec<String> {
    let adapter = mounted();
    match quarb::run(query, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    }
}

#[test]
fn mounts_are_named_children() {
    assert_eq!(values("/*:::name"), vec!["people", "cities"]);
    assert_eq!(values("/people/row::name"), vec!["Ada", "Bo"]);
    assert_eq!(values("/cities/cities/*/country::"), vec!["FR", "UK"]);
}

/// The round-2 pandas gap: a merge across two documents — here even
/// across two *formats* — as a correlation.
#[test]
fn cross_document_join() {
    assert_eq!(
        values(r#"//people/row <=> //cities/cities/*[/city:: = $*1::city]/country::"#),
        vec!["FR", "UK"]
    );
}

/// A tiny property graph exercising the two forwards a mount must not
/// drop: an aliasing `children_named` fast path (node `a` also answers
/// to `alias`, a name no child edge carries) and an edge property
/// (`a --knows--> b` with `since = 2016`). Nodes: root 0, a 1, b 2.
struct Graph;

impl AstAdapter for Graph {
    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if node.0 == 0 {
            vec![NodeId(1), NodeId(2)]
        } else {
            Vec::new()
        }
    }
    fn name(&self, node: NodeId) -> Option<String> {
        match node.0 {
            1 => Some("a".into()),
            2 => Some("b".into()),
            _ => None,
        }
    }
    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        // The fast-path alias the default (name-filtered) enumeration
        // could never produce.
        if node.0 == 0 && name == "alias" {
            return vec![NodeId(1)];
        }
        self.children(node)
            .into_iter()
            .filter(|&c| self.name(c).as_deref() == Some(name))
            .collect()
    }
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        if node.0 == 1 {
            vec![("knows".into(), NodeId(2))]
        } else {
            Vec::new()
        }
    }
    fn link_property(
        &self,
        source: NodeId,
        label: &str,
        target: NodeId,
        name: &str,
    ) -> Option<Value> {
        if source.0 == 1 && label == "knows" && target.0 == 2 && name == "since" {
            Some(Value::Int(2016))
        } else {
            None
        }
    }
}

fn graph_values(query: &str) -> Vec<String> {
    // Wrapped in `Shared` to mirror how the CLI mounts a concrete
    // adapter (`open_mount`), so both `Shared` and `MountAdapter` must
    // forward the calls.
    let adapter = MountAdapter::new(vec![Mount {
        name: "g".into(),
        adapter: Box::new(Shared(std::rc::Rc::new(Graph))),
    }]);
    match quarb::run(query, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    }
}

/// A mounted adapter's aliasing `children_named` must survive the
/// mount: the default name-filtered enumeration would find nothing
/// named `alias`.
#[test]
fn mount_forwards_children_named_alias() {
    assert_eq!(graph_values("/g/alias:::name"), vec!["a"]);
}

/// A mounted adapter's edge properties (`$-::prop`) must survive the
/// mount: without forwarding, the predicate reads null and the edge is
/// dropped.
#[test]
fn mount_forwards_link_property() {
    assert_eq!(graph_values("/g/a->knows[$-::since = 2016]:::name"), vec!["b"]);
}
