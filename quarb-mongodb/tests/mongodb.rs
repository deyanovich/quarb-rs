//! Live tests, gated on `QUARB_MONGODB` (a target whose database
//! holds the quarb-fs music fixture: `artists` / `albums` /
//! `tracks` with DBRef `album` / `artist` fields, string `_id`s).
//! Run with `-- --ignored`.
use quarb_mongodb::MongodbAdapter;

#[test]
#[ignore = "needs QUARB_MONGODB and a live server"]
fn native_references() {
    let Ok(t) = std::env::var("QUARB_MONGODB") else {
        return;
    };
    let a = MongodbAdapter::connect(&t).unwrap();
    let v = |q: &str| match quarb::run(q, &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    };
    assert_eq!(v("/tracks/* @| count"), ["3"]);
    assert_eq!(v("/tracks/mars::album~>::artist~>::name"), ["Holst"]);
    assert_eq!(v("/tracks/venus->album::title"), ["The Planets"]);
    assert_eq!(v("/tracks/*[::price < 1] @| count"), ["2"]);
}
