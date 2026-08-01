//! `default` as the coalesce idiom (ruling #21): expression
//! fallbacks evaluate against the entry's own node, and an
//! unmatched path seeds the null that `default` exists to
//! replace — `(a) | default((b))` never starves.

use quarb::{QueryResult, run};

#[test]
fn default_coalesces_expression_fallbacks() {
    let doc = r#"[
      {"new": {"city": "Lyon"}, "total": 1},
      {"old_city": "Porto", "total": 2},
      {"total": 3}
    ]"#;
    let adapter = quarb_json::JsonAdapter::parse(doc).unwrap();
    let got = match run(
        "/* | ((/new/city::) | default((/old_city::)) | default('nowhere'))",
        &adapter,
    )
    .unwrap()
    {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        _ => panic!("expected values"),
    };
    assert_eq!(got, ["Lyon", "Porto", "nowhere"]);
}
