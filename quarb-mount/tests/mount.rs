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
            target: Some("data/people.csv".into()),
            adapter: Box::new(people),
        },
        Mount {
            name: "cities".into(),
            target: None,
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
    assert_eq!(values("/*:name"), vec!["people", "cities"]);
    assert_eq!(values("/people/row::name"), vec!["Ada", "Bo"]);
    assert_eq!(values("/cities/cities/*/country::"), vec!["FR", "UK"]);
}

/// The round-2 pandas gap: a merge across two documents — here even
/// across two *formats* — as a correlation.
#[test]
fn cross_document_join() {
    // Driver-first: the rows drive, each joined to its city (the
    // ON clause reaching the driver as `$$`), and the witness's
    // country projects in the pipeline.
    assert_eq!(
        values(r#"//people/row <=> //cities/cities/*[/city:: = $$0::city] | $$1/country::"#),
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
        target: None,
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
    assert_eq!(graph_values("/g/alias:name"), vec!["a"]);
}

/// A mounted adapter's edge properties (`$-::prop`) must survive the
/// mount: without forwarding, the predicate reads null and the edge is
/// dropped.
#[test]
fn mount_forwards_link_property() {
    assert_eq!(
        graph_values("/g/a->knows[$-::since = 2016]:name"),
        vec!["b"]
    );
}

/// The mount layer fills a missing `:source` — target where one
/// was recorded, mount name otherwise — and an inner adapter's own
/// components always win.
#[test]
fn mount_fills_provenance_source() {
    // Target recorded → the real-world address, not the alias.
    assert_eq!(values("/people/row[1]:source"), vec!["data/people.csv"]);
    // No target → the mount name stands in.
    assert_eq!(values("/cities:source"), vec!["cities"]);
    // No inner instant/dpid to inherit, so the composite is source-only.
    assert_eq!(
        values("/people/row[1]:provenance"),
        vec!["?data/people.csv"]
    );

    // Inner wins: an adapter that answers its own source keeps it.
    struct Sourced;
    impl AstAdapter for Sourced {
        fn root(&self) -> NodeId {
            NodeId(0)
        }
        fn children(&self, node: NodeId) -> Vec<NodeId> {
            if node.0 == 0 { vec![NodeId(1)] } else { vec![] }
        }
        fn name(&self, node: NodeId) -> Option<String> {
            (node.0 == 1).then(|| "leaf".to_string())
        }
        fn parent(&self, node: NodeId) -> Option<NodeId> {
            (node.0 == 1).then_some(NodeId(0))
        }
        fn provenance(&self, node: NodeId) -> quarb::Provenance {
            quarb::Provenance {
                source: (node.0 == 1).then(|| "sensor://23".to_string()),
                ..Default::default()
            }
        }
    }
    let adapter = MountAdapter::new(vec![Mount {
        name: "s".into(),
        target: Some("bundle.tar".into()),
        adapter: Box::new(Sourced),
    }]);
    let got = match quarb::run("/s/leaf:source", &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        _ => panic!("expected values"),
    };
    assert_eq!(got, vec!["sensor://23"]);
    // The mount root itself has no inner source — the target fills.
    let got = match quarb::run("/s:source", &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        _ => panic!("expected values"),
    };
    assert_eq!(got, vec!["bundle.tar"]);
}

/// Finding 19: the mount index rides the id's high byte, so more
/// than 254 mounts are refused by `try_new` (and `new` panics)
/// rather than mis-routing nodes.
#[test]
fn too_many_mounts_are_refused() {
    let mounts = |n: usize| {
        (0..n)
            .map(|i| Mount {
                name: format!("m{i}"),
                target: None,
                adapter: Box::new(CsvAdapter::parse("a\n1\n").unwrap()),
            })
            .collect::<Vec<_>>()
    };
    assert!(MountAdapter::try_new(mounts(254)).is_ok());
    let err = MountAdapter::try_new(mounts(255)).err().unwrap();
    assert!(err.contains("255 mounts"), "{err}");
}

/// A source declaring a graph over its pages with a stored measure.
struct Declaring;

impl AstAdapter for Declaring {
    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, n: NodeId) -> Vec<NodeId> {
        match n.0 {
            0 => vec![NodeId(1), NodeId(2)],
            _ => Vec::new(),
        }
    }
    fn name(&self, n: NodeId) -> Option<String> {
        match n.0 {
            1 => Some("a".into()),
            2 => Some("b".into()),
            _ => None,
        }
    }
    fn parent(&self, n: NodeId) -> Option<NodeId> {
        (n.0 > 0).then_some(NodeId(0))
    }
    fn answers_to(&self, n: NodeId, name: &str) -> bool {
        (n.0 == 1 || n.0 == 2) && name == "page" || self.name(n).as_deref() == Some(name)
    }
    fn links(&self, n: NodeId) -> Vec<(String, NodeId)> {
        match n.0 {
            1 => vec![("link".into(), NodeId(2))],
            _ => Vec::new(),
        }
    }
    fn graph_declarations(&self) -> Vec<quarb::GraphDeclaration> {
        vec![quarb::GraphDeclaration {
            scope: "//page".into(),
            hop: "->link".into(),
            measures: vec!["pagerank".into()],
        }]
    }
    fn graph_measure(
        &self,
        scope: &str,
        hop: &str,
        measure: &str,
        nodes: &[NodeId],
    ) -> Option<Vec<Value>> {
        assert_eq!((scope, hop, measure), ("//page", "->link", "pagerank"));
        Some(
            nodes
                .iter()
                .map(|n| Value::Float(if n.0 == 2 { 0.75 } else { 0.25 }))
                .collect(),
        )
    }
}

#[test]
fn declared_graphs_read_through_a_mount() {
    let m = MountAdapter::new(vec![Mount {
        name: "site".into(),
        target: None,
        adapter: Box::new(Declaring),
    }]);
    // the mount's root must be opened for the declaration to show
    let _ = m.children(m.root());
    let decls = m.graph_declarations();
    assert_eq!(decls.len(), 1);
    assert_eq!(decls[0].scope, "/site//page");
    assert_eq!(decls[0].hop, "->link");
    // the stored measure, through the mount's ids
    let pages: Vec<NodeId> = m.children(m.children(m.root())[0]);
    assert_eq!(pages.len(), 2);
    let got = m
        .graph_measure("/site//page", "->link", "pagerank", &pages)
        .unwrap();
    assert_eq!(got, vec![Value::Float(0.25), Value::Float(0.75)]);
    // another scope is nobody's
    assert!(
        m.graph_measure("//page", "->link", "pagerank", &pages)
            .is_none()
    );
    // the engine reads the stored column under the mount
    let a = m;
    let out = quarb::run("/site//page @| graph:pagerank(->link)", &a).unwrap();
    let vs = match out {
        quarb::QueryResult::Values(v) => v,
        _ => panic!(),
    };
    assert_eq!(vs, vec![Value::Float(0.25), Value::Float(0.75)]);
}
