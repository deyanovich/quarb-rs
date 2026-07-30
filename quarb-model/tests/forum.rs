//! End-to-end: a flat posts table, no views, no schema graph — the
//! bipartite closure conjured entirely by a model file.

use quarb::AstAdapter as _;
use quarb_model::{parse_model, ModelAdapter};
use quarb_sqlite::SqliteAdapter;
use rusqlite::Connection;

fn forum() -> ModelAdapter<SqliteAdapter> {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE posts (id INTEGER PRIMARY KEY, ip TEXT, cookie TEXT);
        INSERT INTO posts VALUES
          (1,'ip-a','ck-1'), (2,'ip-a','ck-2'), (3,'ip-b','ck-2'),
          (4,'ip-b','ck-3'), (5,'ip-c','ck-9'),
          (6,'ip-a','ck-1');           -- a parallel edge (ck-1,ip-a)
        "#,
    )
    .unwrap();
    let base = SqliteAdapter::load(&conn).unwrap();
    let model = parse_model(
        r#"
        node /ips/ip:         /posts/*::ip     | unique;
        node /cookies/cookie: /posts/*::cookie | unique;
        ref /posts/*::ip     --> ips;
        ref /posts/*::cookie --> cookies;
        edge /posts/*: ::cookie -- ::ip;
        "#,
    )
    .unwrap();
    ModelAdapter::new(base, model)
}

fn values(a: &ModelAdapter<SqliteAdapter>, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.into_iter().map(|n| a.name(n).unwrap_or_default()).collect(),
    }
}

#[test]
fn derived_containers_are_root_siblings() {
    let a = forum();
    let mut roots = values(&a, "/*");
    roots.sort();
    assert_eq!(roots, vec!["cookies", "ips", "posts"]);
    // string-keyed distinct: 4 cookies, 3 ips
    assert_eq!(values(&a, "/cookies/* @| count"), vec!["4"]);
    assert_eq!(values(&a, "/ips/* @| count"), vec!["3"]);
    // Children answer to their role, never to their value: the
    // value is reached by projection or predicate.
    assert_eq!(values(&a, "/cookies/cookie @| count"), vec!["4"]);
    assert_eq!(values(&a, "/cookies/ck-2 @| count"), vec!["0"]);
    assert_eq!(values(&a, "/cookies/cookie[:: = 'ck-2']::"), vec!["ck-2"]);
    // an elevated node has no named properties: its one datum lives
    // on the bare projection, so a column name answers nothing
    assert_eq!(values(&a, "/cookies/cookie[:: = 'ck-2']::cookie"), vec![""]);
}

#[test]
fn declared_refs_resolve_and_backlink() {
    let a = forum();
    // a base row resolves its ip into the container
    assert_eq!(values(&a, "/posts/1::ip-->::"), vec!["ip-a"]);
    // Forward, the hop is named for the role it lands on.
    assert_eq!(values(&a, "/posts/1--ip::"), vec!["ip-a"]);
    // Backward it lands on a row, so it is named for the row.
    // `--cookie` from a cookie node names no relation at all: it
    // would have to land on something that is not a cookie.
    assert_eq!(values(&a, "/cookies/cookie[:: = 'ck-2']--posts::id"), vec!["2", "3"]);
    assert_eq!(
        values(&a, "/cookies/cookie[:: = 'ck-2']--cookie @| count"),
        vec!["0"]
    );
}

#[test]
fn pair_edges_close_the_walk() {
    let a = forum();
    // the payoff: every hop names the role it lands on, and the
    // junction is never spelled twice. ck-1..ck-3 chain over
    // ip-a/ip-b.
    let mut got = values(&a, "/cookies/cookie[:: = 'ck-1'](--ip--cookie)+::");
    got.sort();
    assert_eq!(got, vec!["ck-2", "ck-3"]);
    // ck-9 shares no ip: alone
    assert_eq!(
        values(&a, "/cookies/cookie[:: = 'ck-9'](--ip--cookie)+:: @| count"),
        vec!["0"]
    );
    // parallel edge collapsed: ck-1 has exactly one ip neighbour
    assert_eq!(values(&a, "/cookies/cookie[:: = 'ck-1']--ip @| count"), vec!["1"]);
}

#[test]
fn derived_nodes_carry_container_traits() {
    let a = forum();
    assert_eq!(values(&a, "/ips/*<ip> @| count"), vec!["3"]);
    assert_eq!(values(&a, "/cookies/cookie<cookie>[:: = 'ck-1']::"), vec!["ck-1"]);
}

#[test]
fn elevated_nodes_answer_derivation_provenance_only() {
    let a = forum();
    // An elevated value is something the model made: its source is
    // the derivation, and no row's components leak through it.
    assert_eq!(
        values(&a, "/cookies/cookie[:: = 'ck-1']:::source"),
        vec!["model:/cookies"]
    );
    assert_eq!(
        values(&a, "/cookies/cookie[:: = 'ck-1']:::provenance"),
        vec!["?model:/cookies"]
    );
    assert_eq!(values(&a, "/cookies/cookie[:: = 'ck-1']:::instant"), vec![""]);
    assert_eq!(values(&a, "/cookies:::source"), vec!["model:/cookies"]);
    // Base rows forward to the base adapter (which records none).
    assert_eq!(values(&a, "/posts/1:::source"), vec![""]);
}
