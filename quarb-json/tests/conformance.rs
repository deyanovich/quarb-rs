//! The engine's conformance suite over a JSON document.

use quarb_json::JsonAdapter;

#[test]
fn a_json_document_conforms() {
    let doc = r#"{"books": [{"title": "Emma", "price": 12}, {"title": "Iliad", "price": 9}],
                 "shop": {"name": "Arcade", "tags": ["old", "new"]}}"#;
    let adapter = JsonAdapter::parse(doc).unwrap();
    let findings = quarb::conformance::check(&adapter, 10_000);
    assert!(findings.is_empty(), "{findings:#?}");
}
