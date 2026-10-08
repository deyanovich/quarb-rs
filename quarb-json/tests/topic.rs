//! `$_` inside the predicate of a path operand evaluated from a
//! value context is the enclosing capsa's topic: a lookup written
//! as `%(form = $_; country = (^//*[::city = $_]::country))` reads
//! the topic where the predicate's own scope has none.

use quarb::QueryResult;
use quarb_json::JsonAdapter;

const CITIES: &str = r#"{"cities": [
    {"city": "Paris", "country": "FR"},
    {"city": "Rome", "country": "IT"}
]}"#;

fn values(q: &str) -> Vec<String> {
    let adapter = JsonAdapter::parse(CITIES).unwrap();
    match quarb::run(q, &adapter).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_topic_reaches_a_path_operands_predicate() {
    assert_eq!(values(r#"= "Rome" | (^//*[::city = $_]::country)"#), ["IT"]);
    assert_eq!(
        values(r#"= "Paris Rome" | words | %(form = $_; country = (^//*[::city = $_]::country))"#),
        [
            r#"%(form = "Paris"; country = "FR")"#,
            r#"%(form = "Rome"; country = "IT")"#
        ]
    );
    // Nothing matches: null, as any missing value.
    assert_eq!(
        values(r#"= "Oslo" | %(country = (^//*[::city = $_]::country))"#),
        [r#"%(country = null)"#]
    );
}
