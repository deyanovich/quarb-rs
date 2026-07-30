//! Aliasing node constructors, and `rel` — a relation no property
//! carries, evaluated per pair.

use quarb_model::{parse_model, ModelAdapter};
use quarb_sqlite::SqliteAdapter;
use rusqlite::Connection;

fn ops(model: &str) -> ModelAdapter<SqliteAdapter> {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE deploys (id INTEGER PRIMARY KEY, at TEXT, rev TEXT);
        CREATE TABLE errors  (id INTEGER PRIMARY KEY, at TEXT, msg TEXT);
        INSERT INTO deploys VALUES
          (1,'2026-01-04','a1'), (2,'2026-01-09','b2'), (3,'2026-02-02','c3');
        INSERT INTO errors VALUES
          (7,'2026-01-06','disk full'), (8,'2026-03-01','timeout');
        "#,
    )
    .unwrap();
    let base = SqliteAdapter::load(&conn).unwrap();
    ModelAdapter::new(base, parse_model(model).unwrap())
}

fn values(a: &ModelAdapter<SqliteAdapter>, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.len().to_string().split(' ').map(String::from).collect(),
    }
}

const MODEL: &str = r#"
    node /deploys/deploy: /deploys/*;
    node /errors/error:   /errors/*;
    rel /errors/error -> /deploys/deploy [::at < $$::at];
"#;

#[test]
fn an_aliased_node_is_the_source_wearing_a_role() {
    let a = ops(MODEL);
    // Nothing was created: the container holds the rows themselves,
    // so every column of the row answers on arrival.
    assert_eq!(values(&a, "/deploys/deploy @| count"), vec!["3"]);
    assert_eq!(
        values(&a, "/deploys/deploy[::rev = 'b2']::at"),
        vec!["2026-01-09"]
    );
    // and it carries both traits: the declared role and the source's
    assert_eq!(values(&a, "/errors/error<error> @| count"), vec!["2"]);
}

#[test]
fn a_relation_walks_both_ways_from_one_declaration() {
    let a = ops(MODEL);
    // Forward: from an error, the deploys that preceded it. The hop
    // is named for what it lands on.
    let mut got = values(&a, "/errors/error[::msg = 'disk full']->deploy::rev");
    got.sort();
    assert_eq!(got, vec!["a1"]);
    // The later error follows every deploy.
    let mut got = values(&a, "/errors/error[::msg = 'timeout']->deploy::rev");
    got.sort();
    assert_eq!(got, vec!["a1", "b2", "c3"]);
    // Backward, from the same single declaration — no reverse to
    // write. From a deploy, the errors that followed it.
    let mut got = values(&a, "/deploys/deploy[::rev = 'a1']<-error::msg");
    got.sort();
    assert_eq!(got, vec!["disk full", "timeout"]);
    // February's deploy precedes only the March error.
    assert_eq!(
        values(&a, "/deploys/deploy[::rev = 'c3']<-error::msg"),
        vec!["timeout"]
    );
    // Headless, direction ignored.
    assert_eq!(
        values(&a, "/deploys/deploy[::rev = 'c3']--error::msg"),
        vec!["timeout"]
    );
}

#[test]
fn a_condition_holding_for_many_pairs_forks_threads() {
    let a = ops(MODEL);
    // three prior deploys is three threads, not a list
    assert_eq!(
        values(&a, "/errors/error[::msg = 'timeout']->deploy @| count"),
        vec!["3"]
    );
}

#[test]
fn a_second_relation_between_the_same_sets_mints_a_second_role() {
    // Two relations between errors and deploys would both land on
    // `deploy` and be indistinguishable. The answer is a second
    // role: alias the same nodes under another container, and the
    // second relation lands there.
    let a = ops(r#"
        node /deploys/deploy: /deploys/*;
        node /errors/error:   /errors/*;
        node /culprits/culprit: /deploys/*;
        rel /errors/error -> /deploys/deploy   [::at < $$::at];
        rel /errors/error -> /culprits/culprit [::at < $$::at and ::rev = 'b2'];
    "#);
    // both relations from the same error, each under its own label
    let mut got = values(&a, "/errors/error[::msg = 'timeout']->deploy::rev");
    got.sort();
    assert_eq!(got, vec!["a1", "b2", "c3"]);
    assert_eq!(
        values(&a, "/errors/error[::msg = 'timeout']->culprit::rev"),
        vec!["b2"]
    );
}

#[test]
fn an_explicit_ref_condition_matches_a_named_property() {
    // errors carry no deploy id; pretend msg encodes a rev to
    // exercise the explicit form on an aliased container
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE deploys (id INTEGER PRIMARY KEY, at TEXT, rev TEXT);
        CREATE TABLE tickets (id INTEGER PRIMARY KEY, rev TEXT, title TEXT);
        INSERT INTO deploys VALUES (1,'2026-01-04','a1'), (2,'2026-01-09','b2');
        INSERT INTO tickets VALUES (10,'b2','rollback the cache change');
        "#,
    )
    .unwrap();
    let base = SqliteAdapter::load(&conn).unwrap();
    let a = ModelAdapter::new(
        base,
        parse_model(
            r#"
            node /deploys/deploy: /deploys/*;
            ref /tickets/*::rev --> /deploys/deploy[::rev = $];
            "#,
        )
        .unwrap(),
    );
    // the ticket's rev resolves to the deploy whose ::rev matches
    assert_eq!(values(&a, "/tickets/10--deploy::at"), vec!["2026-01-09"]);
}

#[test]
fn a_membership_criterion_belongs_to_a_node_constructor() {
    // `node`, not `rel`, is where a filter lives: it says what
    // exists, and what it holds is what the row holds.
    let a = ops(r#"
        node /january/deploy: /deploys/*[::at < '2026-02-01'];
    "#);
    assert_eq!(values(&a, "/january/deploy @| count"), vec!["2"]);
    let mut got = values(&a, "/january/deploy::rev");
    got.sort();
    assert_eq!(got, vec!["a1", "b2"]);
}

#[test]
fn an_aliased_node_forwards_base_provenance() {
    let a = ops(MODEL);
    // An aliased node IS its base row under a role: it answers the
    // base's provenance (none here — sqlite records none), never a
    // model:/ derivation stamp.
    assert_eq!(
        values(&a, "/deploys/deploy[::rev = 'b2']:::source"),
        vec![""]
    );
}
