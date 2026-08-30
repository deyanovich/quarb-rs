//! Live battery, gated on QUARB_REDIS_TEST (a reachable Redis
//! seeded as below). Fixture: docker `redis:7` on
//! localhost:16379 — hashes user:1/user:2, list user:1:visits,
//! JSON string session:abc, plain string config:site, zset
//! leaderboard, set tags, stream events (explicit ids), and
//! ttl:token with a thirty-day expiry.

use quarb_redis::RedisAdapter;

fn gate() -> Option<String> {
    std::env::var("QUARB_REDIS_TEST").ok().map(|v| {
        if v == "1" {
            "redis://localhost:16379".to_string()
        } else {
            v
        }
    })
}

fn values(a: &RedisAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    }
}

#[test]
#[ignore = "needs a seeded local Redis (QUARB_REDIS_TEST=1)"]
fn live_redis() {
    let Some(target) = gate() else { return };
    let a = RedisAdapter::connect(&target).unwrap();

    // The colon convention is the tree.
    assert_eq!(
        values(&a, "/*:::name"),
        ["config", "events", "leaderboard", "session", "tags", "ttl", "user"]
    );
    assert_eq!(values(&a, "/user/*:::name"), ["1", "2"]);

    // Hash fields are properties AND children; a key that is
    // also a prefix carries both facets.
    assert_eq!(values(&a, "/user/1::name"), ["Ada"]);
    assert_eq!(values(&a, "/user/*[::plan = \"pro\"]::city"), ["Oslo"]);
    assert_eq!(values(&a, "/user/1/visits/* @| count"), ["3"]);
    assert_eq!(values(&a, "/user/1::::type"), ["hash"]);

    // JSON strings graft; plain strings stay leaves.
    assert_eq!(values(&a, "/session/abc::ip"), ["10.0.0.7"]);
    assert_eq!(values(&a, "/config/site::"), ["flags=beta"]);

    // zset members ride in score order, the score as metadata.
    assert_eq!(values(&a, "/leaderboard/*:::name"), ["Bo", "Ada"]);
    assert_eq!(values(&a, "/leaderboard/Ada::::score"), ["120"]);

    // Streams are bounded snapshots: id-named entries, typed
    // ;;;ts instants, fields as properties.
    assert_eq!(values(&a, "/events/* @| count"), ["2"]);
    assert_eq!(
        values(&a, "/events/*[::kind = \"buy\"]::amount"),
        ["45"]
    );
    assert_eq!(
        values(&a, "/events/*[::::ts > 2020-01-01] @| count"),
        ["2"]
    );

    // TTL is a typed duration.
    assert_eq!(values(&a, "/ttl/token[::::ttl > 1h] @| count"), ["1"]);

    // Key-convention references resolve: the stream's user field
    // holds a full key.
    assert_eq!(
        values(&a, "/events/*[::kind = \"view\"]::user-->::name"),
        ["Ada"]
    );
    // …and the session's user pointer too.
    assert_eq!(values(&a, "/session/abc::user-->::plan"), ["pro"]);
}
