//! The driver's estimator behind the model's `estimate`: asked with
//! the table and the plan's equalities, remembered for the run,
//! unknown when it errs.

use std::cell::Cell;
use std::rc::Rc;

use quarb::{AstAdapter, Confidence, Conjunct, Estimate, Latency, PlanAxis, StepPlan, Value};
use quarb_relational::{RelationalModel, RowSpec, TableSpec, planner_estimate, where_equals};

fn specs() -> Vec<TableSpec> {
    vec![TableSpec {
        name: "orders".into(),
        columns: vec!["id".into(), "customer".into()],
        pk: Some(0),
        fks: vec![],
    }]
}

#[test]
fn the_estimator_is_asked_with_the_equalities_and_remembered() {
    let calls = Rc::new(Cell::new(0));
    let seen = Rc::new(std::cell::RefCell::new(Vec::new()));
    let (c, s) = (calls.clone(), seen.clone());
    let model = RelationalModel::lazy(
        specs(),
        Box::new(|_, _| {
            Ok(vec![RowSpec {
                rowid: Some(1),
                values: vec![Value::Int(1), Value::Str("Emma".into())],
            }])
        }),
    )
    .with_estimator(Rc::new(move |t, spec, equals| {
        c.set(c.get() + 1);
        s.borrow_mut()
            .push(where_equals(spec, equals, quarb_relational::quote_double));
        assert_eq!((t, spec.name.as_str()), (0, "orders"));
        Ok(planner_estimate(4200, Some(64), equals.is_empty()))
    }));
    let whole = StepPlan {
        traits: Vec::new(),
        cap: Default::default(),
        axis: PlanAxis::Child,
        name: None,
        conjuncts: vec![],
        limit: None,
        order: None,
    };
    let e = model.estimate(&["orders"], &whole).unwrap();
    assert_eq!(e.nodes, (4200, 4200));
    assert_eq!(e.bytes, Some((0, 4200 * 64)));
    assert_eq!(e.confidence, Confidence::Exact);
    assert_eq!(e.latency, Latency::Remote);
    // The same plan again: remembered, not asked again.
    model.estimate(&["orders"], &whole).unwrap();
    assert_eq!(calls.get(), 1);
    // An equality travels as the driver's WHERE text.
    let v = Value::Str("Emma".into());
    let filtered = StepPlan {
        traits: Vec::new(),
        cap: Default::default(),
        conjuncts: vec![Conjunct::Equals("customer", &v)],
        ..whole.clone()
    };
    let e = model.estimate(&["orders"], &filtered).unwrap();
    assert_eq!(e.nodes, (0, 4200));
    assert_eq!(calls.get(), 2);
    assert_eq!(
        seen.borrow().as_slice(),
        [None, Some("\"customer\" = 'Emma'".to_string())]
    );
    // Not under a table: not the estimator's. A named hop is a row
    // by key: at most one, the estimator asked for the keyed read.
    assert!(model.estimate(&[], &whole).is_none());
    let named = StepPlan {
        traits: Vec::new(),
        cap: Default::default(),
        name: Some("1"),
        ..whole.clone()
    };
    let e = model.estimate(&["orders"], &named).unwrap();
    assert_eq!(e.nodes, (0, 1));
    assert_eq!(e.requests, Some(1));
    assert_eq!(calls.get(), 3);
}

#[test]
fn an_estimator_that_errs_leaves_the_step_unknown() {
    let model = RelationalModel::lazy(specs(), Box::new(|_, _| Ok(vec![])))
        .with_estimator(Rc::new(|_, _, _| Err("no statistics".into())));
    let plan = StepPlan {
        traits: Vec::new(),
        cap: Default::default(),
        axis: PlanAxis::Child,
        name: None,
        conjuncts: vec![],
        limit: None,
        order: None,
    };
    assert!(model.estimate(&["orders"], &plan).is_none());
    let _ = Estimate {
        nodes: (0, 0),
        bytes: None,
        requests: None,
        latency: Latency::Memory,
        confidence: Confidence::Guess,
    };
}
