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
        node ips:     /posts/*::ip;
        node cookies: /posts/*::cookie;
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
    assert_eq!(values(&a, "/cookies/ck-2::id"), vec!["ck-2"]);
}

#[test]
fn declared_refs_resolve_and_backlink() {
    let a = forum();
    // a base row resolves its ip into the container
    assert_eq!(values(&a, "/posts/1::ip-->::id"), vec!["ip-a"]);
    // a container node's backlinks are the rows that carried it
    assert_eq!(values(&a, "/cookies/ck-2<-cookie::id"), vec!["2", "3"]);
}

#[test]
fn container_labeled_pair_edges_close() {
    let a = forum();
    // the payoff: (--ips--cookies)+ walks the bipartite component,
    // no junction spelled twice. ck-1..ck-3 chain over ip-a/ip-b.
    let mut got = values(&a, "/cookies/ck-1(--ips--cookies)+::id");
    got.sort();
    assert_eq!(got, vec!["ck-2", "ck-3"]);
    // ck-9 shares no ip: alone
    assert_eq!(values(&a, "/cookies/ck-9(--ips--cookies)+::id @| count"), vec!["0"]);
    // parallel edge collapsed: ck-1 has exactly one ip neighbour
    assert_eq!(values(&a, "/cookies/ck-1--ips @| count"), vec!["1"]);
}

#[test]
fn derived_nodes_carry_container_traits() {
    let a = forum();
    assert_eq!(values(&a, "/ips/*<ip> @| count"), vec!["3"]);
    assert_eq!(values(&a, "/cookies/ck-1<cookie>::id"), vec!["ck-1"]);
}
