//! The unit trait `<_h>` in query expressions (spec: Projectors, the
//! unit trait; ruling 2026-11-08): the model's `<_km>` admitted on a
//! projection — a declaration where a typed projector stands, a value
//! right of `=`, as an operand or as a body; convert's rule plus the
//! minting of bare numbers.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-unit-trait-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("leg.json"),
        r#"{"time": "110 min", "n": 12, "fare": "155 EUR", "km": 1235, "legs": ["90 min", "2 h", 30]}"#,
    )
    .unwrap();
    let db = rusqlite::Connection::open(dir.join("flights.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE airports (code TEXT PRIMARY KEY, city TEXT);
         CREATE TABLE flights (id INTEGER PRIMARY KEY, carrier TEXT, origin TEXT, dest TEXT,
                               duration TEXT, fare TEXT, km INTEGER);
         INSERT INTO airports VALUES ('LJU','Ljubljana'),('IST','Istanbul');
         INSERT INTO flights VALUES (1,'swissair','LJU','IST','130 min','155 EUR',1235);",
    )
    .unwrap();
    std::fs::write(
        dir.join("flights.model"),
        "ref /flights/*::origin --> /airports/*[::code = $];\n\
         ref /flights/*::dest --> /airports/*[::code = $];\n\
         edge /flights/*: ::origin -- ::dest as ::carrier weights(::duration; ::fare; ::km<_km>);\n",
    )
    .unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap().trim().to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let (success, out, err) = qua(dir, args);
    assert!(success, "{args:?}: {err}");
    out
}

fn refused(dir: &Path, args: &[&str]) -> String {
    let (success, out, err) = qua(dir, args);
    assert!(!success, "{args:?} answered: {out}");
    err
}

#[test]
fn a_unit_trait_stands_as_a_value() {
    let d = fixture("value");
    let q = |query: &str| ok(&d, &[query, "leg.json"]);
    // a quantity of the dimension, written in the unit, convert's rule
    assert_eq!(q("^ | ::time<_h>"), "1.83333333333333_h");
    assert_eq!(q("^ | ::time<_min>"), "110_min");
    // a bare number is minted in the unit, not converted
    assert_eq!(q("^ | ::n<_h>"), "12_h");
    assert_eq!(q("^ | ::km<_mi>"), "1235_mi");
    assert_eq!(q("^ | ::km<_km>"), "1235_km");
    // another dimension does not read: null, as convert answers
    assert_eq!(q("^ | ::fare<_h>"), "null");
    assert_eq!(q("^ | ::fare<_USD>"), "null");
    assert_eq!(q("^ | ::fare<_EUR>"), "155_EUR");
    // a list item by item
    assert_eq!(q("^ | ::legs<_h>"), "@(1.5_h; 2_h; 30_h)");
    // right of `=` the key stays undeclared, the value converted
    assert_eq!(q("^ | %(h = ::time<_h>)"), "%(h = 1.83333333333333_h)");
    // the header reports the inferred type under the star, as for
    // any undeclared field; the declared key reads back its unit
    assert_eq!(q("^ | %(h = ::time<_h>) | :::header"), "@(:h<*; duration>)");
    assert_eq!(q("^ | %(h<_h> = ::time) | :::header"), "@(:h<_h>)");
    // as an operand
    assert_eq!(q("^ | (::time<_h> | magnitude)"), "1.83333333333333");
    assert_eq!(q("^ | ::n<_h> + 1_h"), "13_h");
    // in a predicate: legal and idle, the criterion converting anyway
    assert_eq!(q("^ | [::time<_h> > 1_h] | ::n"), "12");
    assert_eq!(q("^ | [::time<_h> > 2_h] | ::n"), "");
    // a type trait as a value stays refused, with the cast pointer —
    // on a bare projection at the walk, on any other projection
    // spelling at parse (`$.x<int>`, `$-::price<float>`)
    let e = refused(&d, &["^ | ::n<int> + 1", "leg.json"]);
    assert!(
        e.contains("a trait describes a field, never a value"),
        "{e}"
    );
    let e = refused(&d, &["^ | .n(::n) | $.n<int> + 1", "leg.json"]);
    assert!(
        e.contains("'$.n<int>' declares a field, and stands where a value does")
            && e.contains("cast a value with int($.n)"),
        "{e}"
    );
    let e = refused(&d, &["^ | %(k = ::n<int>)", "leg.json"]);
    assert!(e.contains("right of '=' declares nothing"), "{e}");
}

#[test]
fn a_unit_trait_declares_a_field_where_a_typed_projector_stands() {
    let d = fixture("declare");
    let q = |query: &str| ok(&d, &[query, "leg.json"]);
    // the record key and the bare item
    assert_eq!(
        q("^ | %(hours<_h> = ::time)"),
        "%(hours = 1.83333333333333_h)"
    );
    assert_eq!(q("^ | %(::time<_h>)"), "%(time = 1.83333333333333_h)");
    assert_eq!(q("^ | %(::n<_h>; ::km<_km>)"), "%(n = 12_h; km = 1235_km)");
    // the header item, the bare typed projector over the topic's
    // fields: the declaration read back, the type listed; a value
    // right of `=` leaves the column undeclared
    assert_eq!(
        q("^ | %(::time) | #(:time<_h>) | :::header"),
        "@(:time<_h>)"
    );
    assert_eq!(q("^ | %(::time) | #(:time<_h>) | type"), "_h");
    assert_eq!(
        q("^ | %(::time; ::n) | #(:time<_h>; :n<_mi>) | :time"),
        "@(1.83333333333333_h)"
    );
    assert_eq!(q("^ | #(t = ::time<_h>) | :::header"), "@(:t<*; duration>)");
    // a value that does not read refuses on build, naming the field
    let e = refused(&d, &["^ | %(h<_h> = ::fare)", "leg.json"]);
    assert!(
        e.contains("'h' is declared _h, and holds \"155 EUR\", which does not cast to it"),
        "{e}"
    );
    let e = refused(&d, &["^ | %(::fare) | #(:fare<_h>)", "leg.json"]);
    assert!(
        e.contains(
            "column 'fare' is declared _h, and row 1 holds \"155 EUR\", which does not cast to it"
        ),
        "{e}"
    );
    // the star reports a type, never a unit
    let e = refused(&d, &["^ | %(::time<*; _h>)", "leg.json"]);
    assert!(
        e.contains("the star reports a type, and '_h' is a unit"),
        "{e}"
    );
}

#[test]
fn a_unit_trait_names_a_unit_the_query_knows_and_glues_to_a_projection() {
    let d = fixture("admit");
    // an unknown or refused spelling, with convert's pointer
    let e = refused(&d, &["^ | ::time<_KB>", "leg.json"]);
    assert!(
        e.contains("'<_KB>': no unit 'KB' (kB is 1000 B, KiB is 1024 B)"),
        "{e}"
    );
    let e = refused(&d, &["^ | %(q<_quux> = ::n)", "leg.json"]);
    assert!(e.contains("'<_quux>': no unit 'quux'"), "{e}");
    // a unit without its underscore is a type word, and none
    let e = refused(&d, &["^ | %(::time<h>)", "leg.json"]);
    assert!(
        e.contains("a unit is written with its underscore, '<_h>'"),
        "{e}"
    );
    // on a literal or a parenthesized expression: the road is convert
    let e = refused(&d, &["^ | %(x = 12<_h>)", "leg.json"]);
    assert!(
        e.contains("puts a unit trait on a value") && e.contains("| convert(h)"),
        "{e}"
    );
    let e = refused(&d, &["^ | %(x = (::n + 1)<_h>)", "leg.json"]);
    assert!(e.contains("puts a unit trait on a value"), "{e}");
}

#[test]
fn the_edge_cursor_and_the_register_take_the_unit_trait() {
    let d = fixture("edge");
    let m = ["--model", "flights.model"];
    let q = |query: &str| ok(&d, &[&m[..], &[query, "flights.db"]].concat());
    // lang's six lines on the LJU edge ('130 min', 1235 km, '155 EUR')
    assert_eq!(
        q("/airports/LJU | ->*[..1] | %($-::carrier; hours<_h> = $-::duration)"),
        "%(carrier = \"swissair\"; hours = 2.16666666666667_h)"
    );
    assert_eq!(
        q("/airports/LJU | ->*[..1] | $-::duration<_h>"),
        "2.16666666666667_h"
    );
    assert_eq!(q("/airports/LJU | ->*[..1] | $-::km<_km>"), "1235_km");
    assert_eq!(q("/airports/LJU | ->*[..1] | $-::km<_mi>"), "1235_mi");
    assert_eq!(q("/airports/LJU | ->*[..1] | $-::fare<_h>"), "null");
    let e = refused(
        &d,
        &[
            &m[..],
            &[
                "/airports/LJU | ->*[..1] | %(h<_h> = $-::fare)",
                "flights.db",
            ],
        ]
        .concat(),
    );
    assert!(e.contains("'h' is declared _h, and holds"), "{e}");
    // the register and the walk's facts
    assert_eq!(
        q("/airports/LJU | ->*[..1] | .d($-::duration) | $.d<_h>"),
        "2.16666666666667_h"
    );
    assert_eq!(
        q("/airports/LJU(->*){;5_h} | $-:total<_min>"),
        "null\n130_min"
    );
}
