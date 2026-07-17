//! End-to-end tests: jq filters translated by the importer and
//! executed through the engine against a parsed JSON document.

use quarb_json::JsonAdapter;

const DOC: &str = r#"{
  "users": [
    {"name": "Ada", "age": 36, "email": "ada@example.org", "tags": ["admin", "dev"]},
    {"name": "Bo", "age": 17, "tags": ["dev"]},
    {"name": "Cy", "age": 64, "email": "cy@example.org", "tags": []}
  ],
  "nums": [3, 1, 4, 1, 5],
  "site": {"title": "quarb", "active": true}
}"#;

fn values(jq: &str) -> Vec<String> {
    let query = quarb_jq::translate(jq).unwrap().query;
    let adapter = JsonAdapter::parse(DOC).unwrap();
    match quarb::run(&query, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values for {query}"),
    }
}

fn nodes(jq: &str) -> Vec<String> {
    let query = quarb_jq::translate_nodes(jq).unwrap().query;
    let adapter = JsonAdapter::parse(DOC).unwrap();
    match quarb::run(&query, &adapter).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns.into_iter().map(|n| adapter.pointer(n)).collect(),
        quarb::QueryResult::Values(_) => panic!("expected nodes for {query}"),
    }
}

#[test]
fn navigation() {
    assert_eq!(values(".users[0].name"), vec!["Ada"]);
    assert_eq!(values(".users[].name"), vec!["Ada", "Bo", "Cy"]);
    assert_eq!(values(".users[-1].name"), vec!["Cy"]);
    assert_eq!(values(".site.title"), vec!["quarb"]);
    assert_eq!(values(".users[0].tags[1]"), vec!["dev"]);
    assert_eq!(nodes(".users[1]"), vec!["/users/1"]);
    assert_eq!(nodes(".users[0:2]"), vec!["/users/0", "/users/1"]);
    assert_eq!(nodes(".users[-2:]"), vec!["/users/1", "/users/2"]);
}

#[test]
fn select_filters() {
    assert_eq!(
        values(".users[] | select(.age > 30) | .name"),
        vec!["Ada", "Cy"]
    );
    assert_eq!(
        values(r#".users[] | select(.name == "Bo") | .age"#),
        vec!["17"]
    );
    assert_eq!(
        values(".users[] | select(.age >= 18 and .age < 40) | .name"),
        vec!["Ada"]
    );
    assert_eq!(
        values(r#".users[] | select(has("email")) | .name"#),
        vec!["Ada", "Cy"]
    );
    assert_eq!(nodes(".users[] | select(.age < 18)"), vec!["/users/1"]);
}

#[test]
fn functions() {
    assert_eq!(values(".users | length"), vec!["3"]);
    assert_eq!(values(".users[0].tags | length"), vec!["2"]);
    assert_eq!(values(".nums | add"), vec!["14"]);
    assert_eq!(values(".nums | min"), vec!["1"]);
    assert_eq!(values(".nums | max"), vec!["5"]);
    assert_eq!(values(".nums | unique"), vec!["1", "3", "4", "5"]);
    assert_eq!(values(".nums | sort | first"), vec!["1"]);
    assert_eq!(values(r#".users[0].tags | join(", ")"#), vec!["admin, dev"]);
    assert_eq!(values(".site | keys"), vec!["active", "title"]);
    assert_eq!(values(".users | map(.age)"), vec!["36", "17", "64"]);
    assert_eq!(values(".users | map(.age) | add"), vec!["117"]);
    assert_eq!(values("[.users[].age] | length"), vec!["3"]);
    assert_eq!(values("[.users[] | select(.age > 30)] | length"), vec!["2"]);
}

#[test]
fn slice_composition() {
    // a slice streams elements, so consuming stages must not
    // re-iterate
    assert_eq!(values(".users[0:2] | map(.name)"), vec!["Ada", "Bo"]);
    assert_eq!(values(".users[1:] | length"), vec!["2"]);
    assert_eq!(values(".nums[2:4] | add"), vec!["5"]);
    assert_eq!(values(".users[1:][] | .name"), vec!["Bo", "Cy"]);
}
