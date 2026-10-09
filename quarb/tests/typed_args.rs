//! Typed arguments and positioned parse errors: an argument of the
//! wrong kind refuses the call instead of defaulting, a locale no
//! collation covers refuses the sort, and a parse error says where
//! in the query it stopped.

use quarb::{AstAdapter, NodeId, Value};

/// The smallest adapter: a root with one child carrying a value.
struct One;

impl AstAdapter for One {
    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if node.0 == 0 { vec![NodeId(1)] } else { vec![] }
    }
    fn name(&self, node: NodeId) -> Option<String> {
        (node.0 == 1).then(|| "item".to_string())
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        (node.0 == 1).then_some(NodeId(0))
    }
    fn default_value(&self, node: NodeId) -> Option<Value> {
        (node.0 == 1).then(|| Value::Str("b".into()))
    }
}

#[test]
fn a_parse_error_names_the_character() {
    let err = quarb::run("/item[::n>3", &One).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("at character"), "{msg}");
}

#[test]
fn a_text_argument_of_the_wrong_kind_refuses() {
    let err = quarb::run("/item:: | join(3)", &One).unwrap_err();
    assert!(
        err.kind() == quarb::error::Kind::Refused && err.message().contains("must be text"),
        "{err}"
    );
}

#[cfg(feature = "colligo")]
#[test]
fn an_unknown_collation_refuses_the_sort() {
    let err = quarb::run("/item:: | sort(\"nonsense-locale\")", &One).unwrap_err();
    assert!(
        err.kind() == quarb::error::Kind::Refused && err.message().contains("no collation"),
        "{err}"
    );
}

#[test]
fn a_known_collation_sorts() {
    let out = quarb::run("/item:: | sort(\"en\")", &One).unwrap();
    assert!(matches!(out, quarb::QueryResult::Values(v) if v.len() == 1));
}
