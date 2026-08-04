//! Session-level completion: Tab candidates come from the live
//! arbor through the executor, not from a static table.

use quarb_session::{Doc, LocalExecutor, MemStore, Session};

fn session_over(json: &str) -> Session {
    let doc = Doc::parse(json, "json").unwrap();
    let exec = LocalExecutor::new(doc, (0, 0), false);
    Session::new(Box::new(exec), Box::new(MemStore))
}

#[test]
fn hop_candidates_are_the_real_children() {
    let s = session_over(r#"{"books": [], "authors": [], "prices": {}}"#);
    let text = "/";
    let names: Vec<String> = s
        .complete(text, text.len())
        .into_iter()
        .map(|c| c.text)
        .collect();
    assert!(names.contains(&"books".to_string()), "got {names:?}");
    assert!(names.contains(&"authors".to_string()), "got {names:?}");
}

#[test]
fn prefix_narrows_children() {
    let s = session_over(r#"{"books": [], "authors": [], "prices": {}}"#);
    let text = "/pr";
    let names: Vec<String> = s
        .complete(text, text.len())
        .into_iter()
        .map(|c| c.text)
        .collect();
    assert_eq!(names, vec!["prices".to_string()], "got {names:?}");
}

#[test]
fn syntax_tier_rides_along() {
    let s = session_over(r#"{"a": 1}"#);
    let text = "/a @| cou";
    let names: Vec<String> = s
        .complete(text, text.len())
        .into_iter()
        .map(|c| c.text)
        .collect();
    assert!(names.contains(&"count".to_string()), "got {names:?}");
}
