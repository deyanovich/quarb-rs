//! Live battery, gated on QUARB_SPARQL_TEST (a reachable
//! triplestore seeded as below). Fixture: docker
//! `oxigraph/oxigraph` on localhost:17878, loaded with the
//! example.org office graph: two schema:City resources, two
//! schema:Person, typed populations, a date literal,
//! schema:homeLocation and schema:knows IRIs.

use quarb_sparql::SparqlAdapter;

fn gate() -> Option<String> {
    std::env::var("QUARB_SPARQL_TEST").ok().map(|v| {
        if v == "1" {
            "sparql:http://localhost:17878/query#key=rdfs:label".to_string()
        } else {
            v
        }
    })
}

fn values(a: &SparqlAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    }
}

#[test]
#[ignore = "needs a seeded local triplestore (QUARB_SPARQL_TEST=1)"]
fn live_sparql() {
    let Some(target) = gate() else { return };
    let a = SparqlAdapter::connect(&target).unwrap();

    // rdf:type plays the tables; the listing is complete.
    assert_eq!(values(&a, "/*:::name"), ["City", "Person"]);
    assert_eq!(values(&a, "/Person::::complete"), ["true"]);
    assert_eq!(values(&a, "/Person::::n-rows"), ["2"]);

    // ?key=rdfs:label names resources; typed literals answer as
    // properties.
    assert_eq!(values(&a, "/Person/*:::name"), ["Ada", "Bo"]);
    assert_eq!(values(&a, "/Person/Ada::jobTitle"), ["engineer"]);
    assert_eq!(
        values(&a, "/City/*[::population > 500000]::label"),
        ["Oslo"]
    );
    assert_eq!(
        values(&a, "/Person/*[::birthDate < 2000-01-01] @| count"),
        ["1"]
    );

    // IRI objects are typed crosslinks; backlinks are the
    // reverse triples.
    assert_eq!(
        values(&a, "/Person/Ada->homeLocation::addressCountry"),
        ["NO"]
    );
    assert_eq!(values(&a, "/Person/Bo<-knows::jobTitle"), ["engineer"]);

    // The full IRI rides the metadata channel.
    assert_eq!(
        values(&a, "/Person/Ada::::iri"),
        ["http://example.org/ada"]
    );
}
