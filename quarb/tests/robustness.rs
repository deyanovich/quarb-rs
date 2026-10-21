//! The engine on inputs that used to crash or answer wrongly: deep
//! documents through the node serializers, a glob with an empty
//! literal over non-ASCII text, arithmetic that leaves the floats,
//! and the arguments that used to default silently.

use quarb::{AstAdapter, NodeId, QueryResult, Value};

/// A chain `depth` nodes deep, every node named `a`, the last one
/// carrying a value.
struct Chain(u64);

impl AstAdapter for Chain {
    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if node.0 < self.0 {
            vec![NodeId(node.0 + 1)]
        } else {
            vec![]
        }
    }
    fn name(&self, node: NodeId) -> Option<String> {
        (node.0 > 0).then(|| "a".to_string())
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        (node.0 > 0).then(|| NodeId(node.0 - 1))
    }
    fn default_value(&self, node: NodeId) -> Option<Value> {
        (node.0 == self.0).then(|| Value::Str("leaf".into()))
    }
}

fn values(q: &str, a: &impl AstAdapter) -> Vec<Value> {
    match quarb::run(q, a).unwrap() {
        QueryResult::Values(v) => v,
        other => panic!("{other:?}"),
    }
}

#[test]
fn deep_documents_serialize_without_recursion() {
    let deep = Chain(60_000);
    let json = values("/a | json", &deep);
    assert!(matches!(&json[0], Value::Str(s) if s.ends_with("\"leaf\"}") || s.contains("leaf")));
    let xml = values("/a | xml", &deep);
    assert!(matches!(&xml[0], Value::Str(s) if s.starts_with("<a><a>") && s.ends_with("</a></a>")));
}

/// A few values under one root, for the scalar cases.
struct Items(Vec<Value>);

impl AstAdapter for Items {
    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if node.0 == 0 {
            (1..=self.0.len() as u64).map(NodeId).collect()
        } else {
            vec![]
        }
    }
    fn name(&self, node: NodeId) -> Option<String> {
        (node.0 > 0 && node.0 as usize <= self.0.len()).then(|| "item".to_string())
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        (node.0 > 0 && node.0 as usize <= self.0.len()).then_some(NodeId(0))
    }
    fn default_value(&self, node: NodeId) -> Option<Value> {
        self.0.get(node.0 as usize - 1).cloned()
    }
}

#[test]
fn a_glob_with_an_empty_literal_steps_by_character() {
    let a = Items(vec![Value::Str("письма".into()), Value::Str("abc".into())]);
    assert_eq!(values("/item[:: == *\"\"] @| count", &a), [Value::Int(2)]);
}

#[test]
fn an_overflowing_float_is_inf_and_json_stays_valid() {
    // Overflow is inf, as IEEE 754 has it (spec: the float values);
    // JSON, which cannot spell it, writes null.
    let a = Items(vec![Value::Float(1e308)]);
    assert_eq!(values("= 1e308 * 10", &a), [Value::Float(f64::INFINITY)]);
    assert_eq!(
        values("= 1e308 * 10 @| json", &a),
        [Value::Str("[null]".into())]
    );
    assert_eq!(
        values("/item | %(x = :: * 10) @| json", &a),
        [Value::Str("[{\"x\": null}]".into())]
    );
}

#[test]
fn unique_dedupes_under_the_value_comparison() {
    // Distinct as sort sees them: null, "a", and 1 (which 1.0 meets).
    let a = Items(vec![
        Value::Null,
        Value::Str("a".into()),
        Value::Int(1),
        Value::Float(1.0),
    ]);
    assert_eq!(values("/item:: @| unique @| count", &a), [Value::Int(3)]);
}

#[test]
fn a_text_where_an_integer_is_due_refuses() {
    let a = Items(vec![Value::Str("x".into())]);
    let err = quarb::run("/item:: | pad(\"5\")", &a).unwrap_err();
    assert!(
        err.kind() == quarb::error::Kind::Refused && err.message().contains("must be an integer"),
        "{err}"
    );
    let n = Items(vec![Value::Float(2.345)]);
    let err = quarb::run("/item:: | round(2.0)", &n).unwrap_err();
    assert!(
        err.kind() == quarb::error::Kind::Refused && err.message().contains("digits"),
        "{err}"
    );
}

#[test]
fn an_unknown_unit_operand_is_refused() {
    // once a silent null; since draft 161 the operand must name a
    // unit the query knows (spec: The conversion)
    let a = Items(vec![Value::Str("3 km".into())]);
    let e = quarb::run("/item:: | convert(\"furlongs-per-fortnight\")", &a)
        .unwrap_err()
        .to_string();
    assert!(e.contains("no unit 'furlongs-per-fortnight'"), "{e}");
}

#[test]
fn has_node_answers_false_for_a_foreign_id_without_a_panic_message() {
    let a = Items(vec![Value::Int(1)]);
    assert!(!a.has_node(NodeId(99)));
    assert!(a.has_node(NodeId(1)));
}

/// An adapter that answers through the engine: its listing runs a
/// query of its own (as the model's tree identities do).
struct Nest(Items);

impl AstAdapter for Nest {
    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if node.0 == 0 {
            // A refusal the outer run recorded before the nested
            // run begins must survive it.
            quarb::record_refusal("outer refusal".into());
            let inner = quarb::run("/item @| count", &self.0).expect("the nested run answers");
            assert!(matches!(inner, QueryResult::Values(v) if v == [Value::Int(2)]));
        }
        self.0.children(node)
    }
    fn name(&self, node: NodeId) -> Option<String> {
        self.0.name(node)
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.0.parent(node)
    }
    fn default_value(&self, node: NodeId) -> Option<Value> {
        self.0.default_value(node)
    }
}

#[test]
fn a_nested_run_leaves_the_outer_run_its_state() {
    let a = Nest(Items(vec![Value::Int(1), Value::Int(2)]));
    let err = quarb::run("/item @| count", &a).unwrap_err();
    assert!(
        err.kind() == quarb::error::Kind::Refused && err.message() == "outer refusal",
        "{err}"
    );
}

/// An adapter whose listing asks the engine what `now()` is: the
/// nested run must answer with the outer run's instant.
struct Clock(Items, std::cell::Cell<Option<Value>>);

impl AstAdapter for Clock {
    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if node.0 == 0 {
            let inner = quarb::run("= now()", &self.0).expect("the nested run answers");
            if let QueryResult::Values(v) = inner {
                self.1.set(v.into_iter().next());
            }
        }
        self.0.children(node)
    }
    fn name(&self, node: NodeId) -> Option<String> {
        self.0.name(node)
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.0.parent(node)
    }
    fn default_value(&self, node: NodeId) -> Option<Value> {
        self.0.default_value(node)
    }
}

#[test]
fn a_nested_run_inherits_the_outer_run_options() {
    let a = Clock(Items(vec![Value::Int(1)]), std::cell::Cell::new(None));
    let run = quarb::Run {
        now: Some((86_400, 0)),
        ..Default::default()
    };
    let out = quarb::Query::parse("/item | (now())", &a)
        .unwrap()
        .run(&a, &run)
        .unwrap();
    let outer = match out.result {
        QueryResult::Values(v) => v,
        _ => panic!("values"),
    };
    assert_eq!(outer[0].to_string(), "1970-01-02T00:00:00Z");
    assert_eq!(a.1.take(), Some(outer[0].clone()));
    // Outside any run, the plain entry binds no instant: null.
    assert_eq!(values("= now()", &a.0), [Value::Null]);
}

#[test]
fn a_run_overrides_the_adapter_quantifier_bound() {
    // A chain of five; `/a(/a)+` under a bound of 2
    // still has edges to take when the rounds run out, so the
    // closure would be silently incomplete: refused out loud.
    // Under the adapter's own bound (32) it completes.
    let a = Chain(5);
    let tight = quarb::Run {
        quantifier_bound: Some(2),
        ..Default::default()
    };
    let q = quarb::Query::parse("/a(/a)+ @| count", &a).unwrap();
    let err = q.run(&a, &tight).unwrap_err();
    assert!(err.to_string().contains("quantifier"), "{err}");
    let out = q.run(&a, &quarb::Run::default()).unwrap();
    assert!(matches!(out.result, QueryResult::Values(v) if v == [Value::Int(4)]));
    let shell = quarb::Query::parse("/a:: | `true`", &a).unwrap();
    let err = shell.run(&a, &quarb::Run::default()).unwrap_err();
    assert!(err.to_string().contains("allow-shell"), "{err}");
}
