//! Live integration tests, gated on a running server: set
//! `QUARB_NEO4J` to a target (e.g. `neo4j://localhost/neo4j?key=id`)
//! for a database loaded with `tests/fixture.cypher` and run with
//! `cargo test -p quarb-neo4j -- --ignored`.

use quarb_neo4j::Neo4jAdapter;

fn adapter() -> Neo4jAdapter {
    let target = std::env::var("QUARB_NEO4J").expect("QUARB_NEO4J target");
    Neo4jAdapter::connect(&target).unwrap()
}

fn values(query: &str) -> Vec<String> {
    let a = adapter();
    match quarb::run(query, &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    }
}

fn locators(query: &str) -> Vec<String> {
    let a = adapter();
    match quarb::run(query, &a).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns.into_iter().map(|n| a.locator(n)).collect(),
        quarb::QueryResult::Values(_) => panic!("expected nodes"),
    }
}

#[test]
#[ignore = "needs QUARB_NEO4J pointing at a fixture-loaded server"]
fn catalog_and_rows() {
    // labels, sorted
    assert_eq!(locators("/*"), vec!["/Employee", "/Part", "/Person"]);
    assert_eq!(values("/Person;;;n-rows"), vec!["5"]);
    assert_eq!(
        values("/Person/*::name"),
        vec!["Alice", "Bob", "Carol", "Dan", "Eve"]
    );
    assert_eq!(values("/Person/*[::title = 'CTO']::name"), vec!["Bob"]);
}

#[test]
#[ignore = "needs QUARB_NEO4J pointing at a fixture-loaded server"]
fn relationships_are_crosslinks() {
    // one hop, chain, and the reverse direction
    assert_eq!(values("/Person/3->REPORTS_TO::name"), vec!["Bob"]);
    assert_eq!(
        values("/Person/3->REPORTS_TO->REPORTS_TO::name"),
        vec!["Alice"]
    );
    assert_eq!(
        values("/Person/2<-REPORTS_TO::name"),
        vec!["Carol", "Dan"]
    );
    // wildcard edge: Bob's outgoing = REPORTS_TO Alice + FRIEND Carol
    assert_eq!(values("/Person/2->*::name"), vec!["Carol", "Alice"]);
    // BOM descent
    assert_eq!(
        values("/Part/100->CONTAINS->CONTAINS::name"),
        vec!["Tooth"]
    );
}

#[test]
#[ignore = "needs QUARB_NEO4J pointing at a fixture-loaded server"]
fn quantified_paths() {
    // the whole management chain, and its extremes
    assert_eq!(
        values("/Person/3(->REPORTS_TO)+::name"),
        vec!["Bob", "Alice"]
    );
    assert_eq!(values("/Person/3(->REPORTS_TO)+!::name"), vec!["Alice"]);
    // everyone under Alice, transitively
    assert_eq!(
        values("/Person/1(<-REPORTS_TO)+::name @| count"),
        vec!["3"]
    );
}

#[test]
#[ignore = "needs QUARB_NEO4J pointing at a fixture-loaded server"]
fn multi_label_nodes() {
    // Employee excludes plain-Person Eve
    assert_eq!(values("/Employee;;;n-rows"), vec!["4"]);
    // the same node appears under both labels, interned once
    assert_eq!(
        values("/Employee/1::name"),
        values("/Person/1::name")
    );
    // trait filters see every label from any path
    assert_eq!(
        values("/Person/*<Employee>::name @| count"),
        vec!["4"]
    );
    // canonical parent is the first label in storage order
    assert_eq!(values("/Person/1:::name"), vec!["1"]);
    assert_eq!(values("/Person/1;;;labels @| count"), vec!["1"]);
}

#[test]
#[ignore = "needs QUARB_NEO4J pointing at a fixture-loaded server"]
fn edge_properties() {
    // filter BY the relationship's own property
    assert_eq!(
        values("/Person/*->FRIEND[$-::since < 2016]::name"),
        vec!["Carol"]
    );
    // bare $- reads the type
    assert_eq!(values("/Person/2->*[$- = FRIEND]::name"), vec!["Carol"]);
    // incoming direction sees the same edge
    assert_eq!(
        values("/Person/5<-FRIEND[$-::since = 2020]::name"),
        vec!["Carol"]
    );
}

#[test]
#[ignore = "needs QUARB_NEO4J pointing at a fixture-loaded server"]
fn pattern_breadcrumbs() {
    // the BOM explosion over native relationship properties
    assert_eq!(
        values("/Part/100(->CONTAINS.($-::qty))+ | [::name = 'Tooth'] | @. | product"),
        vec!["48"]
    );
    // depth as a value: the management chain's length
    assert_eq!(
        values("/Person/3(->REPORTS_TO.(1))+! | @. | count"),
        vec!["2"]
    );
    // the walked name-path
    assert_eq!(
        values("/Person/3(->REPORTS_TO.(::name))+! | @. | join(' < ')"),
        vec!["Bob < Alice"]
    );
}

#[test]
#[ignore = "needs QUARB_NEO4J pointing at a fixture-loaded server"]
fn metadata_and_degrees() {
    assert_eq!(
        values("^;;;rel-types"),
        vec!["CONTAINS, FRIEND, REPORTS_TO"]
    );
    // Bob: out = REPORTS_TO + FRIEND, in = two reports
    assert_eq!(values("/Person/2;;;out-degree"), vec!["2"]);
    assert_eq!(values("/Person/2;;;in-degree"), vec!["2"]);
    assert!(values("/Person/2;;;element-id")[0].contains(':'));
}
