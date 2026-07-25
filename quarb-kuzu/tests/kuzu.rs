//! Always-on integration battery: Kùzu is embedded, so the
//! fixture graph is built right here in a tempdir — a writable
//! pass creates and seeds it, then the adapter opens the same
//! directory read-only.

use quarb_kuzu::KuzuAdapter;

fn build_fixture(path: &std::path::Path) {
    let db = kuzu::Database::new(path, kuzu::SystemConfig::default()).unwrap();
    let conn = kuzu::Connection::new(&db).unwrap();
    for stmt in [
        "CREATE NODE TABLE City(name STRING, country STRING, pop INT64, PRIMARY KEY(name))",
        "CREATE NODE TABLE Person(name STRING, role STRING, hired DATE, city STRING, PRIMARY KEY(name))",
        "CREATE REL TABLE LIVES_IN(FROM Person TO City, since INT64)",
        "CREATE REL TABLE MENTORS(FROM Person TO Person, hours INT64)",
        "CREATE (:City {name: 'Oslo', country: 'NO', pop: 700000})",
        "CREATE (:City {name: 'Novi Sad', country: 'RS', pop: 350000})",
        "CREATE (:Person {name: 'Ada', role: 'engineer', hired: DATE('2019-03-01'), city: 'Oslo'})",
        "CREATE (:Person {name: 'Bo', role: 'analyst', hired: DATE('2022-09-15'), city: 'Novi Sad'})",
        "CREATE (:Person {name: 'Cy', role: 'intern', hired: DATE('2024-06-01'), city: 'Oslo'})",
        "MATCH (a:Person {name:'Ada'}), (c:City {name:'Oslo'}) CREATE (a)-[:LIVES_IN {since: 2019}]->(c)",
        "MATCH (a:Person {name:'Bo'}), (c:City {name:'Novi Sad'}) CREATE (a)-[:LIVES_IN {since: 2022}]->(c)",
        "MATCH (a:Person {name:'Cy'}), (c:City {name:'Oslo'}) CREATE (a)-[:LIVES_IN {since: 2024}]->(c)",
        "MATCH (a:Person {name:'Ada'}), (b:Person {name:'Bo'}) CREATE (a)-[:MENTORS {hours: 3}]->(b)",
        "MATCH (a:Person {name:'Ada'}), (b:Person {name:'Cy'}) CREATE (a)-[:MENTORS {hours: 5}]->(b)",
    ] {
        conn.query(stmt).unwrap();
    }
}

fn values(a: &KuzuAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    }
}

#[test]
fn embedded_kuzu() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("office");
    build_fixture(&path);

    let a = KuzuAdapter::open(&format!("kuzu:{}", path.display())).unwrap();

    // Catalog: node tables at the root; rows named by primary key.
    assert_eq!(values(&a, "/*:::name"), ["City", "Person"]);
    assert_eq!(values(&a, "/Person;;;primary-key"), ["name"]);
    assert_eq!(values(&a, "/Person;;;n-rows"), ["3"]);
    assert_eq!(values(&a, "/Person/*:::name"), ["Ada", "Bo", "Cy"]);

    // Typed properties; DATE mints an instant.
    assert_eq!(values(&a, "/Person/Ada::role"), ["engineer"]);
    assert_eq!(values(&a, "/City/*[::pop > 500000]::name"), ["Oslo"]);
    assert_eq!(
        values(&a, "/Person/*[::hired > 2020-01-01]:::name"),
        ["Bo", "Cy"]
    );

    // Rel tables are typed crosslinks, both directions.
    assert_eq!(values(&a, "/Person/Ada->LIVES_IN::name"), ["Oslo"]);
    assert_eq!(values(&a, "/City/Oslo<-LIVES_IN:::name"), ["Ada", "Cy"]);
    assert_eq!(values(&a, "/Person/Bo<-MENTORS::name"), ["Ada"]);

    // Edge properties answer the $- accessor.
    assert_eq!(
        values(&a, "/Person/Ada->MENTORS[$-::hours > 4]::name"),
        ["Cy"]
    );

    // By-convention pointer resolution against a primary key.
    assert_eq!(values(&a, "/Person/Bo::city~>City::country"), ["RS"]);

    // Aggregate over an edge walk: everyone Ada mentors.
    assert_eq!(values(&a, "/Person/Ada->MENTORS @| count"), ["2"]);
}
