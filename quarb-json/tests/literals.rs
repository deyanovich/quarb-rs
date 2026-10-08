//! Value-position seams met while writing recipes: the record
//! literal as a value, a pipe in a push body, the combinations
//! family over nodes, and named csv columns over nodes.

use quarb::{QueryResult, run};
use quarb_json::JsonAdapter;

fn fares() -> JsonAdapter {
    JsonAdapter::parse(
        r#"{"riders": [
              {"name": "ambrose", "fare": 700, "class": 1},
              {"name": "beatrix", "fare": 120, "class": 3},
              {"name": "cedric",  "fare": 120, "class": 3}]}"#,
    )
    .unwrap()
}

fn values(q: &str) -> Vec<String> {
    match run(q, &fares()).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        other => panic!("expected values, got {other:?}"),
    }
}

#[test]
fn a_record_literal_is_a_value() {
    // a field of another record: built on the same node
    assert_eq!(
        values("/riders/0 | %(who = ::name; ticket = %(::fare; ::class))"),
        [r#"%(who = "ambrose"; ticket = %(fare = 700; class = 1))"#]
    );
    // an item of a list (the sigil follows whitespace, as
    // everywhere), and an expression head
    assert_eq!(values("= @( %(a = 1); %(a = 2)) | ... | :a"), ["1", "2"]);
    assert_eq!(values(r#"= %(a = 1; b = "x") | :b"#), ["x"]);
    // the topic is the enclosing one
    assert_eq!(
        values("/riders/1 | ::name | %(t = $_; r = %(x = $_))"),
        [r#"%(t = "beatrix"; r = %(x = "beatrix"))"#]
    );
}

#[test]
fn a_push_body_carries_a_pipe_without_a_second_pair() {
    assert_eq!(
        values("/riders/* | ::name @| pairs | .a($_ | first) | $.a @| unique"),
        ["ambrose", "beatrix"]
    );
    assert_eq!(
        quarb::expand("/x | .a($_ | first)", &quarb::Defs::default()).unwrap(),
        "/x | .a(($_ | first))"
    );
}

#[test]
fn the_combinations_family_over_valueless_nodes_says_what_to_write() {
    // same-named nodes would combine into nothing: refused, with the fix
    let err = run("/riders/*/name @| pairs", &fares())
        .unwrap_err()
        .to_string();
    assert!(err.contains("project a value"), "{err}");
    // projected values combine as before
    assert_eq!(values("/riders/* | ::name @| pairs @| count"), ["3"]);
}

#[test]
fn named_csv_columns_read_a_node_s_properties() {
    assert_eq!(
        values("/riders/* @| csv(name; fare)"),
        ["name,fare\nambrose,700\nbeatrix,120\ncedric,120"]
    );
    // over records the same list selects fields, as before
    assert_eq!(
        values("/riders/*[1] | %(::name; ::fare) @| csv(fare; name)"),
        ["fare,name\n700,ambrose"]
    );
}
