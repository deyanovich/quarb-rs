//! `@| combinations(n)`, `@| pairs`, `@| triples` (lang/TODO.md):
//! every unordered n-member subset of a stream's distinct values,
//! each once, as a list in the values' own order — and an operand in
//! stage position yielding each of its values as a row, so the
//! subsets of every group flow on.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"scenes":[{"who":["levin","kitty","anna","kitty"]},
                      {"who":["anna","vronsky"]},
                      {"who":["kitty","levin"]},
                      {"who":["dolly"]},
                      {"who":[]}]}"#,
    )
    .unwrap()
}

fn values(q: &str) -> Vec<String> {
    match quarb::run(q, &doc()).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect(),
        _ => panic!("expected values"),
    }
}

#[test]
fn pairs_are_unordered_and_each_once() {
    // distinct values, in their own order whatever the stream's
    assert_eq!(
        values("/scenes/0/who/* | :: @| pairs"),
        ["anna, kitty", "anna, levin", "kitty, levin"]
    );
    assert_eq!(
        values("/scenes/0/who/* | :: @| triples"),
        ["anna, kitty, levin"]
    );
    assert_eq!(
        values("/scenes/0/who/* | :: @| combinations(1) @| count"),
        ["3"]
    );
    // fewer values than the size: no combination
    assert_eq!(values("/scenes/3/who/* | :: @| pairs @| count"), ["0"]);
}

#[test]
fn a_pair_counts_the_same_from_either_order() {
    // levin–kitty stands as (levin, kitty) in one scene and as
    // (kitty, levin) in another: one group of two
    assert_eq!(
        values(
            "/scenes/* | (/who/*:: @| pairs) | [$_] @| group(p = $_) | count \
             | \"${$.p}: ${$_}\" @| sort"
        ),
        [
            "anna, kitty: 1",
            "anna, levin: 1",
            "anna, vronsky: 1",
            "kitty, levin: 2"
        ]
    );
}

#[test]
fn an_operand_stage_yields_every_value() {
    assert_eq!(values("/scenes/1 | (/who/*::)"), ["anna", "vronsky"]);
    // one value is that value; none is null, the row kept
    assert_eq!(values("/scenes/3 | (/who/*::)"), ["dolly"]);
    assert_eq!(values("/scenes/4 | (/who/*::) @| count"), ["1"]);
}

#[test]
fn the_sizes_are_validated() {
    assert!(quarb::run("/scenes/0/who/* | :: @| combinations", &doc()).is_err());
    assert!(quarb::run("/scenes/0/who/* | :: @| combinations(0)", &doc()).is_err());
    assert!(quarb::run("/scenes/0/who/* | :: @| pairs(2)", &doc()).is_err());
}

/// `@| unique` tells nodes apart by identity: two rows that read
/// the same are two, and a value is one with every equal value.
#[test]
fn unique_compares_nodes_by_identity() {
    // every scene node reads as an empty object: still five nodes
    assert_eq!(values("/scenes/* @| unique @| count"), ["5"]);
    assert_eq!(values("/scenes/0 | (/who/* @| unique @| count)"), ["4"]);
    // as values, kitty twice is one
    assert_eq!(values("/scenes/0 | (/who/*:: @| unique @| count)"), ["3"]);
}
