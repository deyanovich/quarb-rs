//! Finding 19: counts and levels taken from data cannot exhaust
//! memory — `repeat` / `indent` past 64 MiB and an outline level
//! absurdly below the shallowest are refused, never built.

use quarb::QuarbError;
use quarb_json::JsonAdapter;

fn refusal(q: &str, json: &str) -> String {
    let adapter = JsonAdapter::parse(json).unwrap();
    match quarb::run(q, &adapter) {
        Err(QuarbError::Refused(msg)) => msg,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn values(q: &str, json: &str) -> Vec<String> {
    let adapter = JsonAdapter::parse(json).unwrap();
    match quarb::run(q, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_huge_repeat_is_refused() {
    assert_eq!(values(r#"= "ab" | repeat(3)"#, "{}"), ["ababab"]);
    let msg = refusal(r#"= "ab" | repeat(9223372036854775807)"#, "{}");
    assert!(msg.starts_with("repeat(9223372036854775807)"), "{msg}");
    let msg = refusal(r#"= "x" | indent(99999999999)"#, "{}");
    assert!(msg.starts_with("indent(99999999999)"), "{msg}");
}

#[test]
fn an_absurd_outline_level_is_refused() {
    let rows = r#"[{"d": 1, "t": "a"}, {"d": 2, "t": "b"}]"#;
    assert_eq!(values("/* @| outline(::d; ::t)", rows), ["a", "  b"]);
    let deep = r#"[{"d": 1, "t": "a"}, {"d": 1000000000000, "t": "b"}]"#;
    let msg = refusal("/* @| outline(::d; ::t)", deep);
    assert!(msg.contains("level 1000000000000"), "{msg}");
}
