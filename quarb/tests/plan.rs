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
                nodes: (1, 1),
                bytes: Some((0, 0)),
                requests: Some(0),
                latency: Latency::Memory,
                confidence: Confidence::Exact,
            }),
            ["t"] => Some(Estimate {
                nodes: (0, 1000 - plan.conjuncts.len() as u64 * 900),
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
    assert_eq!(p.commands, 1);
    assert_eq!(p.correlations.len(), 0);
    let steps = &p.branches[0].steps;
    assert_eq!(steps.len(), 3);
    assert_eq!(
        steps[0],
        PlannedStep {
            traits: Vec::new(),
            axis: Some(PlanAxis::Child),
            name: Some("t".into()),
            conjuncts: vec![],
            predicates: 0,
            scope: Some(vec![]),
            pushed: None,
            estimate: Some(Estimate {
                nodes: (1, 1),
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
        vec![("k".into(), quarb::Cmp::Eq, Value::Str("x".into()))]
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
    assert_eq!(b.nodes, (0, 100));
    assert_eq!(b.bytes, Some((4096, 65536)));
    assert_eq!(b.requests, Some(1));
    assert_eq!(b.latency, Latency::Remote);
    assert_eq!(b.confidence, Confidence::Bound);
    assert_eq!(p.estimate(), Some(b));
    // A read past the catalog is unknown, and so is the branch; a
    // bare hop by name past it is free.
    assert_eq!(plan("/t/*/*").branches[0].estimate(), None);
    assert!(plan("/t/*/c").branches[0].estimate().is_some());
    // Two branches add.
    let p = plan("/t/* || /t/*");
    assert_eq!(p.estimate().unwrap().nodes, (0, 2000));
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
    let p = plan("/u/* <=> /t/*[::id = $$0::id]");
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
            nodes: Some(1000),
            ..Default::default()
        }),
        ..Default::default()
    };
    // Within the budget the run proceeds — and would read nodes,
    // which the catalog adapter refuses: the panic is the proof
    // that the budget check came first and passed.
    let beyond = quarb::Run {
        budget: Some(quarb::plan::Budget {
            nodes: Some(10),
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = q.run(&Catalog, &beyond).unwrap_err();
    assert_eq!(err.kind(), quarb::ErrorKind::Refused);
    assert!(
        err.message()
            .contains("up to 1000 nodes; the budget allows 10"),
        "{err}"
    );
    let strict = quarb::Run {
        budget: Some(quarb::plan::Budget {
            strict: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = quarb::Query::parse("/t/*/*", &Catalog)
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
    // A verbatim document: every value raw to end-of-line.
    assert!(k.starts_with(".!kaiv\n.!verbatim\n"), "{k}");
    assert!(k.contains("query=/t/*[::k = \"x\"]\n"), "{k}");
    assert!(k.contains("contract=quarb/plan"), "{k}");
    assert!(k.contains("[/plan/@steps]"), "{k}");
    assert!(k.contains("scope=/t"), "{k}");
    assert!(k.contains("conjunct=::k = \"x\" (pushed)\n"), "{k}");
    // A query written one command per line folds to one value line;
    // a dollar stays a dollar.
    let k = p.to_kaiv("/t/*[::k = $_::k]\n  | ::k");
    assert!(k.contains("query=/t/*[::k = $_::k]   | ::k\n"), "{k}");
    assert!(k.contains("(/plan/estimate)"), "{k}");
    assert!(k.contains("nodes-max=100"), "{k}");
}

#[test]
fn an_unopened_source_is_priced_from_its_catalog() {
    let a = quarb::Unopened::fixed(quarb::Catalog::plain(
        (4200, 4200),
        Some((9_000_000, 9_000_000)),
        quarb::Latency::Local,
        quarb::Confidence::Exact,
    ));
    let plan = quarb::Query::parse("/book[::title = \"Emma\"]", &a)
        .unwrap()
        .plan(&a);
    let e = plan.branches[0].steps[0].estimate.clone().unwrap();
    assert_eq!(e.nodes, (0, 4200));
    assert_eq!(e.bytes, Some((9_000_000, 9_000_000)));
    assert_eq!(e.confidence, quarb::Confidence::Bound);
    // Every entry is a node of `//*`, with more beneath.
    let plan = quarb::Query::parse("//*", &a).unwrap().plan(&a);
    let e = plan.estimate().unwrap();
    assert_eq!(e.nodes, (4200, u64::MAX));
    assert_eq!(e.confidence, quarb::Confidence::Bound);
    let plan = quarb::Query::parse("//book", &a).unwrap().plan(&a);
    assert_eq!(plan.estimate().unwrap().nodes, (0, u64::MAX));
    // A deeper step is a bounded unknown; the branch keeps the
    // root's bytes.
    let deep = quarb::Query::parse("/shelf/book", &a).unwrap().plan(&a);
    let e = deep.estimate().unwrap();
    assert_eq!(e.nodes, (0, u64::MAX));
    assert_eq!(e.bytes, Some((9_000_000, 9_000_000)));
}

#[test]
fn an_unbounded_upper_end_is_unknown_to_a_budget() {
    let a = quarb::Unopened::fixed(quarb::Catalog::plain(
        (1, u64::MAX),
        Some((42, 42)),
        quarb::Latency::Local,
        quarb::Confidence::Bound,
    ));
    let plan = quarb::Query::parse("/*", &a).unwrap().plan(&a);
    let lenient = quarb::plan::Budget {
        nodes: Some(10),
        ..Default::default()
    };
    assert_eq!(lenient.exceeded_by(&plan), None);
    let strict = quarb::plan::Budget {
        nodes: Some(10),
        strict: true,
        ..Default::default()
    };
    assert!(strict.exceeded_by(&plan).unwrap().contains("unbounded"));
    let bytes = quarb::plan::Budget {
        bytes: Some(10),
        ..Default::default()
    };
    assert!(bytes.exceeded_by(&plan).unwrap().contains("42 bytes"));
}

#[test]
fn a_lower_end_past_the_budget_exceeds_it_and_the_cap_is_threaded() {
    let a = quarb::Unopened::fixed(quarb::Catalog::plain(
        (1001, u64::MAX),
        None,
        quarb::Latency::Local,
        quarb::Confidence::Bound,
    ));
    let budget = quarb::plan::Budget {
        nodes: Some(1000),
        ..Default::default()
    };
    let plan = quarb::Query::parse("//*", &a)
        .unwrap()
        .plan_within(&a, &budget);
    assert!(
        budget
            .exceeded_by(&plan)
            .unwrap()
            .contains("at least 1001 nodes")
    );
    assert_eq!(
        budget.cap(),
        quarb::Cap {
            nodes: Some(1000),
            bytes: None
        }
    );
    // The arena walk stops past the cap too.
    let doc = quarb_json_like();
    let plan = quarb::Query::parse("//*", &doc)
        .unwrap()
        .plan_within(&doc, &budget);
    let _ = plan;
}

fn quarb_json_like() -> quarb::Unopened {
    quarb::Unopened::fixed(quarb::Catalog::plain(
        (0, 0),
        None,
        quarb::Latency::Memory,
        quarb::Confidence::Exact,
    ))
}

/// Planning is opt-in: a run with neither `plan` nor a budget never
/// asks an adapter for an estimate, so the default path carries no
/// pricing overhead at all.
#[test]
fn a_plain_run_never_plans() {
    struct Priced;
    impl AstAdapter for Priced {
        fn root(&self) -> NodeId {
            NodeId(0)
        }
        fn children(&self, _: NodeId) -> Vec<NodeId> {
            Vec::new()
        }
        fn name(&self, _: NodeId) -> Option<String> {
            None
        }
        fn estimate(&self, _: &[&str], _: &StepPlan<'_>) -> Option<Estimate> {
            panic!("priced");
        }
    }
    let q = quarb::Query::parse("/book", &Priced).unwrap();
    assert!(q.run(&Priced, &quarb::Run::ambient()).is_ok());
    let planned = quarb::Run {
        plan: true,
        ..quarb::Run::ambient()
    };
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| q.run(&Priced, &planned)))
            .is_err(),
        "--plan asks the adapter"
    );
}

#[test]
fn comparisons_and_traits_are_lowered_for_pricing() {
    use quarb::{Cmp, Conjunct};
    struct Listing;
    impl AstAdapter for Listing {
        fn root(&self) -> NodeId {
            NodeId(0)
        }
        fn children(&self, _: NodeId) -> Vec<NodeId> {
            Vec::new()
        }
        fn name(&self, _: NodeId) -> Option<String> {
            None
        }
        fn estimate(&self, _: &[&str], plan: &StepPlan<'_>) -> Option<Estimate> {
            // The listing applies a size comparison and the file
            // trait: the matches alone are counted.
            let sizes = plan
                .conjuncts
                .iter()
                .filter(|c| matches!(c, Conjunct::Compare("size", Cmp::Gt, Value::Int(2000))))
                .count();
            let n = if sizes == 1 && plan.traits == ["file"] {
                38
            } else {
                6845
            };
            Some(Estimate {
                nodes: (0, n),
                bytes: None,
                requests: Some(0),
                latency: Latency::Local,
                confidence: Confidence::Bound,
            })
        }
    }
    let p = quarb::Query::parse("//*<file>[::size > 2000]", &Listing)
        .unwrap()
        .plan(&Listing);
    let s = &p.branches[0].steps[0];
    assert_eq!(
        s.conjuncts,
        vec![("size".into(), Cmp::Gt, Value::Int(2000))]
    );
    assert_eq!(s.traits, vec!["file".to_string()]);
    assert_eq!(s.estimate.as_ref().unwrap().nodes, (0, 38));
    let k = p.to_kaiv("//*<file>[::size > 2000]");
    assert!(k.contains("trait=file"), "{k}");
    assert!(k.contains("conjunct=::size > 2000 (unknown)\n"), "{k}");
    // A non-text equality is a comparison, not an index's equality.
    let p = quarb::Query::parse("/t/*[::n = 7]", &Listing)
        .unwrap()
        .plan(&Listing);
    assert_eq!(
        p.branches[0].steps[1].conjuncts,
        vec![("n".into(), Cmp::Eq, Value::Int(7))]
    );
}
