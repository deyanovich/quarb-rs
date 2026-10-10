//! The JSON adapter estimates from its arena: exact counts, no read.

use quarb::{Confidence, PlanAxis};
use quarb_json::JsonAdapter;

#[test]
fn the_arena_counts_what_a_step_reaches() {
    let doc =
        r#"{"books": [{"t": "Emma", "p": 12}, {"t": "Iliad", "p": 9}, {"t": "Odyssey", "p": 9}]}"#;
    let a = JsonAdapter::parse(doc).unwrap();
    let plan = quarb::Query::parse("/books/*[::t = \"Iliad\"]", &a)
        .unwrap()
        .plan(&a);
    let s = &plan.branches[0].steps[1];
    assert_eq!(s.scope, Some(vec!["books".into()]));
    let e = s.estimate.clone().unwrap();
    assert_eq!(e.nodes, (0, 3));
    assert_eq!(e.confidence, Confidence::Bound);
    let plan = quarb::Query::parse("//t", &a).unwrap().plan(&a);
    let s = &plan.branches[0].steps[0];
    assert_eq!(s.axis, Some(PlanAxis::Descendant));
    let e = s.estimate.clone().unwrap();
    assert_eq!(e.nodes, (3, 3));
    assert_eq!(e.confidence, Confidence::Exact);
}

#[test]
fn the_walk_stops_at_the_first_node_past_the_budget() {
    let doc =
        r#"{"books": [{"t": "Emma"}, {"t": "Iliad"}, {"t": "Odyssey"}, {"t": "Persuasion"}]}"#;
    let a = JsonAdapter::parse(doc).unwrap();
    let budget = quarb::plan::Budget {
        nodes: Some(2),
        ..Default::default()
    };
    let plan = quarb::Query::parse("/books/*", &a)
        .unwrap()
        .plan_within(&a, &budget);
    let e = plan.branches[0].steps[1].estimate.clone().unwrap();
    assert_eq!(e.nodes, (3, u64::MAX));
    assert_eq!(e.confidence, Confidence::Bound);
    assert!(
        budget
            .exceeded_by(&plan)
            .unwrap()
            .contains("at least 3 nodes")
    );
}
