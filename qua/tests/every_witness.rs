//! The every-witness correlation (ruling 2026-11-11): `<=>*` binds
//! every witness and forks the driver's thread once per witness,
//! SQL's JOIN; `<=>*?` keeps a driver with none; the plain `<=>`
//! stays one witness, and tells when the query reads a witness it
//! chose among several.

use quarb::{QueryResult, Value};

/// Article 08's seam: an owner per sensor, several readings per
/// sensor, one reading of a sensor nobody owns.
fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"owners":   [{"sensor": "a", "team": "core"},
                         {"sensor": "b", "team": "edge"},
                         {"sensor": "c", "team": "lab"}],
            "readings": [{"sensor": "a", "val": 5.0},
                         {"sensor": "a", "val": 1.5},
                         {"sensor": "b", "val": 7.5},
                         {"sensor": "b", "val": 3.0},
                         {"sensor": "z", "val": 9.0}]}"#,
    )
    .unwrap()
}

fn outcome(q: &str) -> (Vec<String>, Vec<String>) {
    let run = quarb::Run::ambient();
    let out = quarb::Query::parse(q, &doc())
        .unwrap_or_else(|e| panic!("{q}: {e}"))
        .run(&doc(), &run)
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    let values = match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::to_quarb).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    };
    (
        values,
        out.diagnostics.iter().map(|d| d.to_string()).collect(),
    )
}

fn values(q: &str) -> Vec<String> {
    outcome(q).0
}

const ON: &str = "/readings/*[::sensor = $$0::sensor]";

#[test]
fn the_star_binds_every_witness() {
    // one witness: the driver's cardinality, the unowned sensor's
    // owner dropped
    assert_eq!(
        values(&format!(
            "/owners/* <=> {ON} | %(team = ::team; val = $$1::val)"
        )),
        [
            r#"%(team = "core"; val = 5.0)"#,
            r#"%(team = "edge"; val = 7.5)"#
        ]
    );
    // every witness: owner by owner, then reading by reading
    let every = [
        r#"%(team = "core"; val = 5.0)"#,
        r#"%(team = "core"; val = 1.5)"#,
        r#"%(team = "edge"; val = 7.5)"#,
        r#"%(team = "edge"; val = 3.0)"#,
    ];
    assert_eq!(
        values(&format!(
            "/owners/* <=>* {ON} | %(team = ::team; val = $$1::val)"
        )),
        every
    );
    // the star needs no space, and the rounded spelling takes it
    assert_eq!(
        values(&format!(
            "/owners/* <=>*{ON} | %(team = ::team; val = $$1::val)"
        )),
        every
    );
    assert_eq!(
        values(&format!(
            "/owners/* :=:* {ON} | %(team = ::team; val = $$1::val)"
        )),
        every
    );
    // the outer form keeps the driver with no witness, once, the
    // slot unbound
    assert_eq!(
        values(&format!(
            "/owners/* <=>*? {ON} | %(team = ::team; val = $$1::val)"
        )),
        [
            every[0],
            every[1],
            every[2],
            every[3],
            r#"%(team = "lab"; val = null)"#
        ]
    );
    // no condition: the cross product
    assert_eq!(values("/owners/* <=>* /readings/* @| count"), ["15"]);
    // the marks in their one order
    let e = quarb::run(&format!("/owners/* <=>?* {ON} @| count"), &doc())
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("the star stands before the outer mark: write '<=>*?'"),
        "{e}"
    );
}

/// The threads of a starred correlation stand on one node and
/// differ by their witness: they stay apart at the gate and after a
/// hop, and the commands of the context see every one.
#[test]
fn the_threads_differ_by_their_witness() {
    assert_eq!(values(&format!("/owners/* <=>* {ON} @| count")), ["4"]);
    assert_eq!(
        values(&format!("/owners/* <=>* {ON} | ::team")),
        [r#""core""#, r#""core""#, r#""edge""#, r#""edge""#]
    );
    // a hop from each thread keeps its witness, and the threads
    // that arrive at one node do not merge
    assert_eq!(
        values(&format!(
            "/owners/* <=>* {ON} | /team | %(t = ::; v = $$1::val)"
        )),
        [
            r#"%(t = "core"; v = 5.0)"#,
            r#"%(t = "core"; v = 1.5)"#,
            r#"%(t = "edge"; v = 7.5)"#,
            r#"%(t = "edge"; v = 3.0)"#
        ]
    );
    assert_eq!(
        values(&format!("/owners/* <=>* {ON} | ::team @| unique")),
        [r#""core""#, r#""edge""#]
    );
    assert_eq!(
        values(&format!("/owners/* <=>* {ON} | $$1::val @| sum")),
        ["17.0"]
    );
}

/// The mark is per entry: a plain entry keeps the first of its
/// candidates under which the rest of the chain binds, a starred
/// one every such.
#[test]
fn a_chain_takes_the_mark_per_entry() {
    let back = "/owners/*[::sensor = $$1::sensor]";
    // starred then plain: one thread per reading, each its owner
    assert_eq!(
        values(&format!(
            "/owners/* <=>* {ON} <=> {back} | %(v = $$1::val; o = $$2::team)"
        )),
        [
            r#"%(v = 5.0; o = "core")"#,
            r#"%(v = 1.5; o = "core")"#,
            r#"%(v = 7.5; o = "edge")"#,
            r#"%(v = 3.0; o = "edge")"#
        ]
    );
    // plain then starred: the first reading, then every reading of
    // its sensor
    let again = "/readings/*[::sensor = $$1::sensor]";
    assert_eq!(
        values(&format!(
            "/owners/* <=> {ON} <=>* {again} | %(a = $$1::val; b = $$2::val)"
        )),
        [
            "%(a = 5.0; b = 5.0)",
            "%(a = 5.0; b = 1.5)",
            "%(a = 7.5; b = 7.5)",
            "%(a = 7.5; b = 3.0)"
        ]
    );
    // both starred: every pair
    assert_eq!(
        values(&format!("/owners/* <=>* {ON} <=>* {again} @| count")),
        ["8"]
    );
}

/// A plain `<=>` that bound the first of several candidates says so
/// when the query reads the witness; a join that only filters, and
/// a starred one, are quiet.
#[test]
fn a_witness_read_among_several_is_told() {
    let (rows, diagnostics) = outcome(&format!(
        "/owners/* <=> {ON} | %(team = ::team; val = $$1::val)"
    ));
    assert_eq!(rows.len(), 2);
    assert_eq!(
        diagnostics,
        [
            "warning SEVERAL_WITNESSES ($$1): '<=>' bound the first of several candidates for 2 \
          drivers, and the query reads the witness ($$1): the answer depends on the joined \
          expression's order; '<=>*' binds every candidate"
        ]
    );
    // the outer form the same
    let (_, diagnostics) = outcome(&format!(
        "/owners/* <=>? {ON} | %(team = ::team; val = $$1::val)"
    ));
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(diagnostics[0].contains("for 2 drivers"), "{diagnostics:?}");
    // the join only filters
    let (rows, diagnostics) = outcome(&format!("/owners/* <=> {ON} | %(team = ::team)"));
    assert_eq!(rows.len(), 2);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    // every witness is bound: nothing was chosen
    let (_, diagnostics) = outcome(&format!(
        "/owners/* <=>* {ON} | %(team = ::team; val = $$1::val)"
    ));
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    // one candidate per driver: nothing was chosen either
    let (rows, diagnostics) = outcome(
        "/readings/* <=> /owners/*[::sensor = $$0::sensor] | %(team = $$1::team; val = ::val)",
    );
    assert_eq!(rows.len(), 4);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    // the bare test of the slot asks whether a witness was bound,
    // the anti-join's question: its answer is the same whichever
    // candidate was, and it is no read
    for q in [
        format!("/owners/* <=>? {ON} | [!$$1] | ::team"),
        format!("/owners/* <=> {ON} | [$$1] | ::team"),
        format!("/owners/* <=>? {ON} | [!$$1 || ::team = \"core\"] | ::team"),
        format!("/owners/* <=>? {ON} | [!(!$$1)] | ::team"),
    ] {
        let (_, diagnostics) = outcome(&q);
        assert!(diagnostics.is_empty(), "{q}: {diagnostics:?}");
    }
    assert_eq!(
        values(&format!("/owners/* <=>? {ON} | [!$$1] | ::team")),
        [r#""lab""#]
    );
    // a property of the slot tested for null is a read: another
    // candidate's could differ
    let (_, diagnostics) = outcome(&format!("/owners/* <=>? {ON} | [!$$1::val] | ::team"));
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    // a later entry's ON reading the witness is a read of it
    let (_, diagnostics) = outcome(&format!(
        "/owners/* <=> {ON} <=> /owners/*[::sensor = $$1::sensor] | ::team"
    ));
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(diagnostics[0].contains("($$1)"), "{diagnostics:?}");
}

/// The unparse and the reflection carry the mark.
#[test]
fn the_mark_is_kept_in_the_text_and_in_the_reflection() {
    let q = quarb::Query::parse(
        "/owners/* <=>* /readings/*[::sensor = $$0::sensor] <=>*? /owners/*[::sensor = $$1::sensor]",
        &doc(),
    )
    .unwrap();
    let text = q.canonical();
    assert!(text.contains(" <=>* /readings/*"), "{text}");
    assert!(text.contains(" <=>*? /owners/*"), "{text}");
}
