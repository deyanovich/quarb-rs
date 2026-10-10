//! `@| count` over a plain path is counted as the walk goes, the
//! matches never collected: every shape agrees with the list the
//! materializing path produces.

use quarb::{AstAdapter, NodeId, Query, QueryResult, Run, Value};
use quarb_json::JsonAdapter;

fn doc() -> JsonAdapter {
    let mut books = String::new();
    for i in 0..300 {
        if i > 0 {
            books.push(',');
        }
        books.push_str(&format!(
            r#"{{"t": "Book {i}", "p": {}, "g": "{}", "tags": ["a", "b"]}}"#,
            (i * 7) % 100,
            if i % 3 == 0 { "verse" } else { "prose" }
        ));
    }
    JsonAdapter::parse(&format!(r#"{{"books": [{books}], "note": {{"p": 5}}}}"#)).unwrap()
}

fn count(a: &impl AstAdapter, q: &str) -> i64 {
    match Query::parse(q, a)
        .unwrap()
        .run(a, &Run::ambient())
        .unwrap()
        .result
    {
        QueryResult::Values(v) => match v.as_slice() {
            [Value::Int(n)] => *n,
            other => panic!("{q}: {other:?}"),
        },
        other => panic!("{q}: {other:?}"),
    }
}

fn listed(a: &impl AstAdapter, q: &str) -> i64 {
    match Query::parse(q, a)
        .unwrap()
        .run(a, &Run::ambient())
        .unwrap()
        .result
    {
        QueryResult::Nodes(ns) => ns.len() as i64,
        QueryResult::Values(vs) => vs.len() as i64,
    }
}

#[test]
fn every_counting_shape_agrees_with_the_list() {
    let a = doc();
    for path in [
        "/books/*",
        "/books/*[::p > 50]",
        "/books/*[::g = \"verse\"]",
        "/books/*[::g = \"verse\"][::p < 30]",
        "//p",
        "//p[. > 90]",
        "//*",
        "//tags/*",
        "/books/*[::p > 50]/tags/*",
        "/books/*/tags/*[. = \"a\"]",
        "/note/*",
        "/missing/*",
        "/books/*[3]",
        "/books/*[..10][::p > 50]",
        "//books/*[::p > 50]",
    ] {
        assert_eq!(
            count(&a, &format!("{path} @| count")),
            listed(&a, path),
            "{path}"
        );
    }
}

/// An adapter that refuses to be asked for its descendants in bulk
/// shows the count never collects: it answers `children` alone.
#[test]
fn the_count_walks_through_children_alone() {
    struct Chain(usize);
    impl AstAdapter for Chain {
        fn root(&self) -> NodeId {
            NodeId(0)
        }
        fn children(&self, n: NodeId) -> Vec<NodeId> {
            if (n.0 as usize) < self.0 {
                vec![NodeId(n.0 + 1)]
            } else {
                Vec::new()
            }
        }
        fn name(&self, n: NodeId) -> Option<String> {
            Some(if n.0 % 2 == 0 { "even" } else { "odd" }.to_string())
        }
        fn descendants_where(
            &self,
            _: NodeId,
            _: &str,
            _: &str,
            _: &Value,
        ) -> Option<Vec<(NodeId, usize)>> {
            panic!("collected")
        }
    }
    let a = Chain(2000);
    assert_eq!(count(&a, "//odd @| count"), 1000);
    assert_eq!(count(&a, "//* @| count"), 2000);
    assert_eq!(count(&a, "/odd/even/odd @| count"), 1);
}
