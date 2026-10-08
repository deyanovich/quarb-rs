//! The in-engine join's hash side: where the ON holds an equality
//! between a property of the joined node and a value of the driver,
//! the joined context is indexed by that property and only the
//! matching candidates are tested. The answer must be the nested
//! loop's: each query here is run twice, once in the indexable
//! spelling and once with the equality hidden behind arithmetic or
//! a disjunction (which the index does not read), and the two must
//! agree.

use quarb::{QueryResult, run};
use quarb_json::JsonAdapter;

fn shop() -> JsonAdapter {
    JsonAdapter::parse(
        r#"{"authors": [
              {"id": 1,   "name": "Austen", "tags": ["novel", "regency"]},
              {"id": 2,   "name": "Twain",  "tags": ["novel", "river"]},
              {"id": "3", "name": "Homer",  "tags": ["epic"]},
              {"id": 2,   "name": "Clemens","tags": []},
              {"id": 4.0, "name": "Gibbon", "tags": ["history"]}],
            "books": [
              {"title": "Emma",        "author_id": 1,    "tag": "novel"},
              {"title": "Tom Sawyer",  "author_id": 2,    "tag": "river"},
              {"title": "The Iliad",   "author_id": 3,    "tag": "epic"},
              {"title": "Anonymous",   "author_id": null, "tag": "none"},
              {"title": "Orphan",      "author_id": 99,   "tag": "novel"},
              {"title": "Decline",     "author_id": "4",  "tag": "history"}]}"#,
    )
    .unwrap()
}

fn values(q: &str) -> Vec<String> {
    match run(q, &shop()).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        other => panic!("expected values, got {other:?}"),
    }
}

/// The same join, indexable and not.
fn both(hashed: &str, looped: &str) -> Vec<String> {
    let a = values(hashed);
    assert_eq!(a, values(looped), "hashed and looped disagree on {hashed}");
    a
}

#[test]
fn the_inner_join_keeps_the_first_witness_in_document_order() {
    // Two authors carry id 2: the first in document order is the
    // witness. 3 and "3", 4.0 and "4" are equal, as `=` has them.
    assert_eq!(
        both(
            "/books/* <=> /authors/*[::id = _::author_id] | %(::title; a = $$1::name)",
            "/books/* <=> /authors/*[::id + 0 = _::author_id + 0] | %(::title; a = $$1::name)",
        ),
        [
            r#"%(title = "Emma"; a = "Austen")"#,
            r#"%(title = "Tom Sawyer"; a = "Twain")"#,
            r#"%(title = "The Iliad"; a = "Homer")"#,
            r#"%(title = "Decline"; a = "Gibbon")"#,
        ]
    );
}

#[test]
fn the_equality_may_stand_either_way_round_and_beside_other_conditions() {
    assert_eq!(
        both(
            "/books/* <=> /authors/*[_::author_id = ::id && ::name != \"Twain\"] \
             | %(::title; a = $$1::name)",
            "/books/* <=> /authors/*[(_::author_id = ::id || 1 = 2) && ::name != \"Twain\"] \
             | %(::title; a = $$1::name)",
        ),
        [
            r#"%(title = "Emma"; a = "Austen")"#,
            r#"%(title = "Tom Sawyer"; a = "Clemens")"#,
            r#"%(title = "The Iliad"; a = "Homer")"#,
            r#"%(title = "Decline"; a = "Gibbon")"#,
        ]
    );
}

#[test]
fn the_outer_join_keeps_the_unmatched_with_a_null_witness() {
    assert_eq!(
        both(
            "/books/* <=>? /authors/*[::id = _::author_id] | %(::title; a = $$1::name)",
            "/books/* <=>? /authors/*[::id + 0 = _::author_id + 0] | %(::title; a = $$1::name)",
        ),
        [
            r#"%(title = "Emma"; a = "Austen")"#,
            r#"%(title = "Tom Sawyer"; a = "Twain")"#,
            r#"%(title = "The Iliad"; a = "Homer")"#,
            r#"%(title = "Anonymous"; a = null)"#,
            r#"%(title = "Orphan"; a = null)"#,
            r#"%(title = "Decline"; a = "Gibbon")"#,
        ]
    );
}

#[test]
fn a_list_valued_property_joins_by_membership() {
    // `::tags` is a list: a value with no key, tested every time.
    assert_eq!(
        both(
            "/books/* <=> /authors/*[::tags = _::tag] | %(::title; a = $$1::name)",
            "/books/* <=> /authors/*[(::tags = _::tag || 1 = 2)] | %(::title; a = $$1::name)",
        ),
        [
            r#"%(title = "Emma"; a = "Austen")"#,
            r#"%(title = "Tom Sawyer"; a = "Twain")"#,
            r#"%(title = "The Iliad"; a = "Homer")"#,
            r#"%(title = "Orphan"; a = "Austen")"#,
            r#"%(title = "Decline"; a = "Gibbon")"#,
        ]
    );
}
