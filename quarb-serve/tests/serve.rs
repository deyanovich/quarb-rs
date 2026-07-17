//! Identity: a JSON document served over the protocol answers
//! exactly like the direct adapter.

use quarb_serve::ServeAdapter;

const DOC: &str = r#"{"books": [
  {"title": "Dune", "price": 9.99},
  {"title": "Emma", "price": 7.5},
  {"title": "Sapiens", "price": 22.5}
]}"#;

#[test]
fn served_equals_direct() {
    let dir = std::env::temp_dir().join("quarb-serve-test");
    std::fs::create_dir_all(&dir).unwrap();
    let doc = dir.join("store.json");
    std::fs::write(&doc, DOC).unwrap();

    let served = ServeAdapter::spawn(&format!(
        "{} {}",
        env!("CARGO_BIN_EXE_quarb-serve-json"),
        doc.display()
    ))
    .unwrap();
    assert_eq!(served.name, "json-file");
    let direct = quarb_json::JsonAdapter::parse(DOC).unwrap();

    for q in [
        "/books/*/title::",
        "/books/*[/price:: < 10]/title::",
        "/books/*/price:: @| sum",
        "/books/* @| count",
    ] {
        let a = match quarb::run(q, &served).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
            _ => panic!("values expected"),
        };
        let b = match quarb::run(q, &direct).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
            _ => panic!("values expected"),
        };
        assert_eq!(a, b, "served/direct divergence for {q}");
    }
}
