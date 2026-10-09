//! The static plan: steps, scopes and what the adapter would apply,
//! read off the query before anything runs.

use quarb::plan::{Plan, PlannedStep};
use quarb::{AstAdapter, Confidence, Estimate, Latency, NodeId, PlanAxis, StepPlan, Value};

/// An adapter that never answers a node but knows its catalog: under
/// the scope `["t"]` it applies every conjunct.
struct Catalog;

impl AstAdapter for Catalog {
    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, _n: NodeId) -> Vec<NodeId> {
        panic!("the plan reads no node")
    }
    fn name(&self, _n: NodeId) -> Option<String> {
        panic!("the plan reads no node")
    }
    fn select_plan(&self, scope: &[&str], plan: &StepPlan<'_>) -> Option<Vec<bool>> {
        (scope == ["t"]).then(|| vec![true; plan.conjuncts.len()])
    }
    fn estimate(&self, scope: &[&str], plan: &StepPlan<'_>) -> Option<Estimate> {
        // The root's children are catalogued; under `t`, a bound.
        match scope {
            [] => Some(Estimate {
                rows: (1, 1),
                bytes: Some((0, 0)),
                requests: Some(0),
                latency: Latency::Memory,
                confidence: Confidence::Exact,
            }),
            ["t"] => Some(Estimate {
                rows: (0, 1000 - plan.conjuncts.len() as u64 * 900),
                bytes: Some((4096, 65536)),
                requests: Some(1),
                latency: Latency::Remote,
                confidence: Confidence::Bound,
            }),
            _ => None,
        }
    }
}

fn plan(q: &str) -> Plan {
    quarb::Query::parse(q, &Catalog).unwrap().plan(&Catalog)
}

#[test]
fn a_root_anchored_branch_names_its_scope_step_by_step() {
    let p = plan("/t/*[::k = \"x\"][::n > 1]/c @| count");
    assert_eq!(p.stages, 1);
    assert_eq!(p.correlations.len(), 0);
    let steps = &p.branches[0].steps;
    assert_eq!(steps.len(), 3);
    assert_eq!(
        steps[0],
        PlannedStep {
            axis: Some(PlanAxis::Child),
            name: Some("t".into()),
            conjuncts: vec![],
            predicates: 0,
            scope: Some(vec![]),
            pushed: None,
            estimate: Some(Estimate {
                rows: (1, 1),
                bytes: Some((0, 0)),
                requests: Some(0),
                latency: Latency::Memory,
                confidence: Confidence::Exact,
            }),
        }
    );
    // The wildcard hop under `t`: the leading equality lowered and
    // claimed by the catalog, the second predicate the engine's.
    assert_eq!(steps[1].scope, Some(vec!["t".into()]));
    assert_eq!(
        steps[1].conjuncts,
        vec![("k".into(), Value::Str("x".into()))]
    );
    assert_eq!(steps[1].predicates, 2);
    assert_eq!(steps[1].pushed, Some(vec![true]));
    assert_eq!(steps[1].residual(), 1);
    // Past a predicate the base is not literal.
    assert_eq!(steps[2].scope, None);
    assert_eq!(steps[2].pushed, None);
    assert_eq!(p.roots(), vec![Some("t".into())]);
}

#[test]
fn estimates_compose_along_the_branch_and_across_branches() {
    let p = plan("/t/*[::k = \"x\"] | ::k");
    let b = p.branches[0].estimate().unwrap();
    assert_eq!(b.rows, (0, 100));
    assert_eq!(b.bytes, Some((4096, 65536)));
    assert_eq!(b.requests, Some(1));
    assert_eq!(b.latency, Latency::Remote);
    assert_eq!(b.confidence, Confidence::Bound);
    assert_eq!(p.estimate(), Some(b));
    // A step past the catalog is unknown, and so is the branch.
    assert_eq!(plan("/t/*/c").branches[0].estimate(), None);
    // Two branches add.
    let p = plan("/t/* || /t/*");
    assert_eq!(p.estimate().unwrap().rows, (0, 2000));
    assert_eq!(p.estimate().unwrap().requests, Some(2));
}

#[test]
fn an_unanchored_search_lands_anywhere() {
    let p = plan("//x[::k = 1] | ::k");
    let s = &p.branches[0].steps[0];
    assert_eq!(s.axis, Some(PlanAxis::Descendant));
    assert_eq!(s.scope, Some(vec![]));
    assert_eq!(s.pushed, None);
    assert_eq!(s.residual(), 1);
    assert_eq!(p.roots(), vec![None]);
}

#[test]
fn correlations_are_planned_too() {
    let p = plan("/u/* <=> /t/*[::id = _::id]");
    assert_eq!(p.correlations.len(), 1);
    assert_eq!(
        p.correlations[0].branches[0].steps[0].name,
        Some("t".into())
    );
    assert_eq!(p.branches[0].steps[0].name, Some("u".into()));
    assert_eq!(p.roots(), vec![Some("u".into()), Some("t".into())]);
}

#[test]
fn a_budget_refuses_before_running_and_the_outcome_carries_the_plan() {
    let q = quarb::Query::parse("/t/*", &Catalog).unwrap();
    let within = quarb::Run {
        budget: Some(quarb::plan::Budget {
            rows: Some(1000),
            ..Default::default()
        }),
        ..Default::default()
    };
    // Within the budget the run proceeds — and would read nodes,
    // which the catalog adapter refuses: the panic is the proof
    // that the budget check came first and passed.
    let beyond = quarb::Run {
        budget: Some(quarb::plan::Budget {
            rows: Some(10),
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = q.run(&Catalog, &beyond).unwrap_err();
    assert_eq!(err.kind(), quarb::ErrorKind::Refused);
    assert!(
        err.message()
            .contains("up to 1000 rows; the budget allows 10"),
        "{err}"
    );
    let strict = quarb::Run {
        budget: Some(quarb::plan::Budget {
            strict: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = quarb::Query::parse("/t/*/c", &Catalog)
        .unwrap()
        .run(&Catalog, &strict)
        .unwrap_err();
    assert!(err.message().contains("unknown"), "{err}");
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| q.run(&Catalog, &within)))
            .is_err(),
        "within the budget, the run proceeds to the adapter"
    );
}

#[test]
fn the_plan_is_a_kaiv_document() {
    let p = plan("/t/*[::k = \"x\"]");
    let k = p.to_kaiv("/t/*[::k = \"x\"]");
    assert!(k.starts_with(".!kaiv\n"), "{k}");
    assert!(k.contains("contract=quarb/plan"), "{k}");
    assert!(k.contains("[/plan/@steps]"), "{k}");
    assert!(k.contains("scope=/t"), "{k}");
    assert!(k.contains("conjunct=\"::k = \\\"x\\\" (pushed)\""), "{k}");
    assert!(k.contains("(/plan/estimate)"), "{k}");
    assert!(k.contains("rows-max=100"), "{k}");
}
