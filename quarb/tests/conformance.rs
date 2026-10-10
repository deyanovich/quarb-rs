//! The conformance suite on the engine's own small adapters: a
//! conforming tree passes clean, and each broken contract is named.

use quarb::{AstAdapter, NodeId, Selection, StepPlan, Value};

/// A fixed tree: 0 → {1 "a", 2 "b"}, 1 → {3 "a"}, with a trait and
/// a property on every node.
struct Tree;

fn kids(n: NodeId) -> Vec<NodeId> {
    match n.0 {
        0 => vec![NodeId(1), NodeId(2)],
        1 => vec![NodeId(3)],
        _ => vec![],
    }
}

impl AstAdapter for Tree {
    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, n: NodeId) -> Vec<NodeId> {
        kids(n)
    }
    fn name(&self, n: NodeId) -> Option<String> {
        match n.0 {
            1 | 3 => Some("a".into()),
            2 => Some("b".into()),
            _ => None,
        }
    }
    fn parent(&self, n: NodeId) -> Option<NodeId> {
        match n.0 {
            1 | 2 => Some(NodeId(0)),
            3 => Some(NodeId(1)),
            _ => None,
        }
    }
    fn traits(&self, _n: NodeId) -> Vec<String> {
        vec!["leaf".into()]
    }
    fn property(&self, n: NodeId, name: &str) -> Option<Value> {
        (name == "n").then(|| Value::Int(n.0 as i64))
    }
    fn properties(&self, n: NodeId) -> Vec<(String, Value)> {
        vec![("n".into(), Value::Int(n.0 as i64))]
    }
}

#[test]
fn a_conforming_tree_passes() {
    assert_eq!(quarb::conformance::check(&Tree, 100), Vec::<String>::new());
}

/// The same tree with `parent` broken for one child.
struct Orphan;

impl AstAdapter for Orphan {
    fn root(&self) -> NodeId {
        Tree.root()
    }
    fn children(&self, n: NodeId) -> Vec<NodeId> {
        Tree.children(n)
    }
    fn name(&self, n: NodeId) -> Option<String> {
        Tree.name(n)
    }
    fn parent(&self, n: NodeId) -> Option<NodeId> {
        (n.0 != 3).then(|| Tree.parent(n)).flatten()
    }
}

#[test]
fn a_broken_parent_is_named() {
    let findings = quarb::conformance::check(&Orphan, 100);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(
        findings[0].contains("parent(NodeId(3)) is None"),
        "{findings:?}"
    );
}

/// A tree whose descendant `select` answers one node short.
struct ShortIndex;

impl AstAdapter for ShortIndex {
    fn root(&self) -> NodeId {
        Tree.root()
    }
    fn children(&self, n: NodeId) -> Vec<NodeId> {
        Tree.children(n)
    }
    fn name(&self, n: NodeId) -> Option<String> {
        Tree.name(n)
    }
    fn parent(&self, n: NodeId) -> Option<NodeId> {
        Tree.parent(n)
    }
    fn select(&self, from: NodeId, plan: &StepPlan<'_>) -> Option<Selection> {
        match plan.axis {
            quarb::PlanAxis::Descendant if from.0 == 0 && plan.name == Some("a") => {
                Some(Selection {
                    nodes: vec![(NodeId(1), 1)],
                    applied: vec![false; plan.conjuncts.len()],
                })
            }
            _ => quarb::adapter::select_by_hooks(self, from, plan),
        }
    }
}

#[test]
fn a_short_index_is_named() {
    let findings = quarb::conformance::check(&ShortIndex, 100);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(
        findings[0].contains("answers 1 node(s), the walk finds 2"),
        "{findings:?}"
    );
}

#[test]
fn the_engine_runs_the_short_index_as_the_walk_would_not() {
    // The contract is the adapter's to keep: the engine trusts a
    // `Some`, so a short index is a short answer. The suite exists
    // so an adapter's tests catch it before a query does.
    let out = quarb::run("//a @| count", &ShortIndex).unwrap();
    assert!(matches!(out, quarb::QueryResult::Values(v) if v == [Value::Int(1)]));
    let out = quarb::run("//a @| count", &Tree).unwrap();
    assert!(matches!(out, quarb::QueryResult::Values(v) if v == [Value::Int(2)]));
}

/// A tree that counts for the engine: `/a @| count` answers what
/// `count` says, without the walk.
struct Counting;

impl AstAdapter for Counting {
    fn root(&self) -> NodeId {
        Tree.root()
    }
    fn children(&self, n: NodeId) -> Vec<NodeId> {
        Tree.children(n)
    }
    fn name(&self, n: NodeId) -> Option<String> {
        Tree.name(n)
    }
    fn parent(&self, n: NodeId) -> Option<NodeId> {
        Tree.parent(n)
    }
    fn count(&self, from: NodeId, plan: &StepPlan<'_>) -> Option<usize> {
        (from.0 == 0 && plan.name == Some("a") && plan.conjuncts.is_empty()).then_some(42)
    }
}

#[test]
fn a_counting_adapter_answers_count_without_the_walk() {
    let out = quarb::run("/a @| count", &Counting).unwrap();
    assert!(matches!(out, quarb::QueryResult::Values(v) if v == [Value::Int(42)]));
    // Anything beyond the bare shape walks: a predicate the plan
    // cannot carry, a projection, a second command.
    let out = quarb::run("/a[1] @| count", &Counting).unwrap();
    assert!(matches!(out, quarb::QueryResult::Values(v) if v == [Value::Int(1)]));
    let out = quarb::run("/a @| count @| sum", &Counting).unwrap();
    assert!(matches!(out, quarb::QueryResult::Values(v) if v == [Value::Int(1)]));
}
