//! Live-broker battery, gated on QUARB_KAFKA_TEST (a reachable
//! broker seeded as below). Fixture: single-node KRaft Kafka on
//! localhost:19092 with
//!   - `events` (2 partitions): 5 JSON messages keyed e1..e5,
//!     fields user_id/action/amount + a nested geo.city
//!   - `users` (compacted): u1@basic, u2@gold, then u1@gold —
//!     two live versions of u1, latest wins
//!   - `logs`: 3 plain-text unkeyed lines
//! See the cloud-adapters runbook for the docker seed commands.

use quarb_kafka::KafkaAdapter;

fn gate() -> Option<String> {
    std::env::var("QUARB_KAFKA_TEST").ok().map(|v| {
        if v == "1" {
            "kafka://localhost:19092".to_string()
        } else {
            v
        }
    })
}

fn values(a: &KafkaAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    }
}

#[test]
#[ignore = "needs a seeded local Kafka broker (QUARB_KAFKA_TEST=1)"]
fn live_kafka() {
    let Some(target) = gate() else { return };
    let a = KafkaAdapter::connect(&target).unwrap();

    // Topics list, internal ones hidden.
    assert_eq!(values(&a, "/*:::name"), ["events", "logs", "users"]);
    assert_eq!(values(&a, "/events::::partitions"), ["2"]);

    // The bounded window sees every message, across partitions.
    assert_eq!(values(&a, "/events/* @| count"), ["5"]);
    let mut keys = values(&a, "/events/*::::key");
    keys.sort();
    assert_eq!(keys, ["e1", "e2", "e3", "e4", "e5"]);

    // JSON payloads are attribute trees: filters, dual exposure,
    // nested descent.
    assert_eq!(
        values(&a, "/events/*[::action = \"buy\"]::amount @| sum"),
        ["172"]
    );
    assert_eq!(
        values(&a, "/events/*[::amount = 120]/geo::city"),
        ["Novi Sad"]
    );

    // Record timestamps are typed instants.
    assert_eq!(
        values(&a, "/events/*[::::ts > 2020-01-01] @| count"),
        ["5"]
    );

    // Key-named messages repeat: u1's history, in time order.
    assert_eq!(values(&a, "/users/'u1' @| count"), ["2"]);
    assert_eq!(values(&a, "/users/'u1'[-1]::tier"), ["gold"]);
    assert_eq!(values(&a, "/users/'u1'[1]::tier"), ["basic"]);

    // The stream-table join: resolve lands on the LATEST message
    // with the key — the compacted topic's current row.
    assert_eq!(
        values(&a, "/events/'e3'::user_id-->users::name"),
        ["Bo"]
    );
    assert_eq!(values(&a, "/events/'e2'::user_id-->users::tier"), ["gold"]);
    // Hint-less: user_id → the `users` topic by convention.
    assert_eq!(values(&a, "/events/'e2'::user_id-->::name"), ["Ada"]);

    // Plain-text payloads stay leaf values.
    assert_eq!(values(&a, "/logs/* @| count"), ["3"]);
    assert!(values(&a, "/logs/*::").contains(&"warn: low disk".to_string()));

    // Window narrowing: everything is older than 2030, nothing
    // predates 2020.
    let sep = if target.contains('?') { '&' } else { '?' };
    let till = KafkaAdapter::connect(&format!("{target}{sep}until=2020-01-01")).unwrap();
    assert_eq!(values(&till, "/events/* @| count"), ["0"]);
    let since = KafkaAdapter::connect(&format!("{target}{sep}from=2020-01-01")).unwrap();
    assert_eq!(values(&since, "/events/* @| count"), ["5"]);

    // Topic selection at the target.
    let only = KafkaAdapter::connect(&format!("{target}{sep}topics=logs")).unwrap();
    assert_eq!(values(&only, "/*:::name"), ["logs"]);
}
