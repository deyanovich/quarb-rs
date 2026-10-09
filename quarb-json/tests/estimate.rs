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
    assert_eq!(e.rows, (0, 3));
    assert_eq!(e.confidence, Confidence::Bound);
    let plan = quarb::Query::parse("//t", &a).unwrap().plan(&a);
    let s = &plan.branches[0].steps[0];
    assert_eq!(s.axis, Some(PlanAxis::Descendant));
    let e = s.estimate.clone().unwrap();
    assert_eq!(e.rows, (3, 3));
    assert_eq!(e.confidence, Confidence::Exact);
}
