//! Ruling #30: adapter-declared name aliases answer on every axis.

use quarb::{AstAdapter, NodeId, Value};

/// A tiny social feed: the root holds handle-named accounts. The
/// canonical hop name is the stripped `alice`; the source spelling
/// `@alice` is a declared alias.
struct Feed;

impl AstAdapter for Feed {
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
            1 => Some("alice".into()),
            2 => Some("bob".into()),
            _ => None,
        }
    }
    fn default_value(&self, node: NodeId) -> Option<Value> {
        match node.0 {
            1 => Some(Value::Str("hello".into())),
            2 => Some(Value::Str("world".into())),
            _ => None,
        }
    }
    fn answers_to(&self, node: NodeId, name: &str) -> bool {
        let canon = self.name(node);
        canon.as_deref() == Some(name)
            || name
                .strip_prefix('@')
                .is_some_and(|stripped| canon.as_deref() == Some(stripped))
    }
}

fn values(query: &str) -> Vec<String> {
    match quarb::run(query, &Feed).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|n| format!("node:{}", n.0)).collect(),
    }
}

#[test]
fn canonical_name_answers() {
    assert_eq!(values("/alice::"), vec!["hello"]);
}

#[test]
fn alias_answers_on_the_child_axis() {
    assert_eq!(values("/@alice::"), vec!["hello"]);
}

#[test]
fn alias_answers_in_search_too() {
    // The asymmetry the ruling kills: before, //@alice fell
    // silent while /@alice hit.
    assert_eq!(values("//@alice::"), vec!["hello"]);
    assert_eq!(values("//alice::"), vec!["hello"]);
}

#[test]
fn quoted_alias_spelling_answers() {
    assert_eq!(values("/\"@alice\"::"), vec!["hello"]);
}

#[test]
fn patterns_stay_canonical() {
    // ~(...) ranges over canonical vocabulary: the alias spelling
    // is not in pattern space.
    assert_eq!(values("/(/^al/)::"), vec!["hello"]);
    assert!(values("/(/^@/)::").is_empty());
}

#[test]
fn core_name_stays_canonical() {
    assert_eq!(values("/@alice:::name"), vec!["alice"]);
}

/// A two-level feed: accounts under the root, posts under each
/// account. The source's compound identifier `@alice.pinned`
/// names the pinned post *of account alice* — the alias encodes
/// the parent, and answers_to validates it against real
/// ancestry.
struct Threads;

impl AstAdapter for Threads {
    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        match node.0 {
            0 => vec![NodeId(1), NodeId(2)],   // accounts alice, bob
            1 => vec![NodeId(11), NodeId(12)], // alice: pinned, reply
            2 => vec![NodeId(21)],             // bob: pinned
            _ => Vec::new(),
        }
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        match node.0 {
            1 | 2 => Some(NodeId(0)),
            11 | 12 => Some(NodeId(1)),
            21 => Some(NodeId(2)),
            _ => None,
        }
    }
    fn name(&self, node: NodeId) -> Option<String> {
        match node.0 {
            1 => Some("alice".into()),
            2 => Some("bob".into()),
            11 | 21 => Some("pinned".into()),
            12 => Some("reply-1".into()),
            _ => None,
        }
    }
    fn default_value(&self, node: NodeId) -> Option<Value> {
        (node.0 == 11).then(|| Value::Str("alice pin".into()))
            .or((node.0 == 21).then(|| Value::Str("bob pin".into())))
    }
    fn answers_to(&self, node: NodeId, name: &str) -> bool {
        if self.name(node).as_deref() == Some(name) {
            return true;
        }
        // `@<account>.pinned` — a pinned post whose parent is
        // that account.
        if let Some(rest) = name.strip_prefix('@')
            && let Some(account) = rest.strip_suffix(".pinned")
        {
            return self.name(node).as_deref() == Some("pinned")
                && self
                    .parent(node)
                    .and_then(|p| self.name(p))
                    .as_deref()
                    == Some(account);
        }
        false
    }
}

fn tvalues(query: &str) -> Vec<String> {
    match quarb::run(query, &Threads).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|n| format!("node:{}", n.0)).collect(),
    }
}

#[test]
fn parent_encoding_alias_answers_in_search() {
    // The compound identifier addresses exactly one node — the
    // pinned post of alice, not of bob.
    assert_eq!(tvalues("//@alice.pinned::"), vec!["alice pin"]);
    assert_eq!(tvalues("//@bob.pinned::"), vec!["bob pin"]);
}

#[test]
fn parent_encoding_alias_is_not_a_root_child() {
    // The alias validates ancestry; it does not navigate. From
    // the root's child axis the post is two levels down, so the
    // spelling for a global identifier is the global axis.
    assert!(tvalues("/@alice.pinned::").is_empty());
}

#[test]
fn compound_alias_composes_with_navigation() {
    // Alias lands on the node; ordinary navigation continues —
    // ascend to the account and read its canonical name.
    assert_eq!(tvalues("//@alice.pinned\\*:::name"), vec!["alice"]);
}
