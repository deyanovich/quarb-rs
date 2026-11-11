//! When two keys are one key (the code review of 2026-11-10, the
//! stdlib rows B1 and B2, the value rows B3 and B12, the graph row
//! T3). The spec has two rules and the engine compared four ways:
//!
//! - a record's keys and an indexed table's labels are equal AS
//!   VALUES (spec: Record): `1` and `1.0` one key, `1` and `"1"`
//!   two, one instant under two offsets one, `1_km` and `1000_m`
//!   one;
//! - `correlate`'s key and a join condition's equality compare AS
//!   `=` COMPARES (spec: Concatenation, correlated tables): the
//!   above, and a number with its text.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-keys-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("inst.csv"),
        "d,v\n2013-01-01T00:00:00Z,1\n2013-01-02T00:00:00Z,2\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("legs.json"),
        r#"{"orders": [{"id": 1, "km": "1 km"}, {"id": 2, "km": "2 km"}, {"id": 4, "km": "4 km"}],
            "customers": [{"name": "Xenophon", "d": "1000 m"}, {"name": "Cyrus", "d": "2000 m"},
                          {"name": "Clearchus", "d": "4 km"}]}"#,
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

/// `correlate` matches its key as `=` compares. The key was a
/// double or the print text: an id past 2^53 joined its neighbour's
/// row, `true` joined "true", and one instant under two offsets did
/// not join.
#[test]
fn correlate_matches_its_key_as_equals_compares() {
    let d = fixture("correlate");
    // two integers compare exactly: a 64-bit id is not a double
    assert_eq!(
        ok(
            &d,
            &[
                "= #(@(id; who); %(9007199254740993; 'Achilles'); %(9007199254740992; 'Hector')) \
               | correlate((#(@(id; city); %(9007199254740992; 'Troy')) | #(-i=:id)); --on=:id) \
               | $_:city"
            ]
        ),
        "@(null; \"Troy\")"
    );
    // a declared index wins over the undeclared key (ruling
    // 2026-11-10): under an index declared text the key is read as
    // its text, the boolean too, and joins. (Compared as values the
    // boolean was not its text, and the row found nothing.)
    assert_eq!(
        ok(
            &d,
            &["= #(@(k; who); %(true; 'Achilles')) \
               | correlate((#(@(k<text>; city); %('true'; 'Troy')) | #(-i=:k)); --on=:k) | $_:city"]
        ),
        "@(\"Troy\")"
    );
    // a number meets its text, as `1 = \"1\"` holds
    assert_eq!(
        ok(
            &d,
            &["= #(@(k; who); %(1; 'Achilles')) \
               | correlate((#(@(k; city); %('1'; 'Troy')) | #(-i=:k)); --on=:k) | $_:city"]
        ),
        "@(\"Troy\")"
    );
    // one instant under two written offsets
    assert_eq!(
        ok(
            &d,
            &["= #(@(k; who); %(2024-01-01T10:00:00+01:00; 'Achilles')) \
               | correlate((#(@(k; city); %(2024-01-01T09:00:00Z; 'Troy')) | #(-i=:k)); --on=:k) \
               | $_:city"]
        ),
        "@(\"Troy\")"
    );
    // one magnitude under two units
    assert_eq!(
        ok(
            &d,
            &["= #(@(k; who); %(1_km; 'Hector')) \
               | correlate((#(@(k; city); %(1000_m; 'Ilium')) | #(-i=:k)); --on=:k) | $_:city"]
        ),
        "@(\"Ilium\")"
    );
    // a null key matches nothing, and a label `=` tells apart from
    // no other is not a repeated one
    assert_eq!(
        ok(
            &d,
            &["= #(@(k; who); %(null; 'Achilles')) \
               | correlate((#(@(k; city); %(1; 'Troy'); %('a'; 'Ilium')) | #(-i=:k)); --on=:k) \
               | $_:city"]
        ),
        "@(null)"
    );
    // the operand's index repeats a label when `=` holds between
    // two of them, however they are written
    let e = refused(
        &d,
        &["= #(@(k; who); %(1; 'Achilles')) \
           | correlate((#(@(k; city); %(1; 'Troy'); %(1.0; 'Ilium')) | #(-i=:k)); --on=:k)"],
    );
    assert!(e.contains("is not unique"), "{e}");
}

/// Two indexed tables align on their labels as values: `1` and
/// `1.0` are one label. They were compared by structure, so `+`
/// answered four rows of nulls. Two undeclared indexes meet at
/// their common type (ruling 2026-11-10), so the labels of an
/// integer index and a float one are floats in the answer.
#[test]
fn indexed_tables_align_on_labels_equal_as_values() {
    let d = fixture("align");
    let left = "(#(@(k; v); %(1; 10); %(2; 20)) | #(-i=:k))";
    let right = "(#(@(k; v); %(1.0; 1); %(2.0; 2)) | #(-i=:k))";
    assert_eq!(
        ok(&d, &[&format!("= {left} + {right}")]),
        "#(@(k; v);\n  %(1.0; 11);\n  %(2.0; 22))"
    );
    // `++!` updates the left's rows where it appended the right's
    assert_eq!(
        ok(&d, &[&format!("= {left} ++! {right}")]),
        "#(@(k; v);\n  %(1.0; 1);\n  %(2.0; 2))"
    );
    // columns set side by side meet on the label
    assert_eq!(
        ok(
            &d,
            &["= (#(@(k; v); %(1; 10); %(2; 20)) | #(-i=:k)) \
               ++ (#(@(k; w); %(2.0; 'b'); %(1.0; 'a')) | #(-i=:k))"]
        ),
        "#(@(k; v; w);\n  %(1.0; 10; \"a\");\n  %(2.0; 20; \"b\"))"
    );
    // a number and its text stay two keys, nothing coerces them: a
    // record keyed 1 and one keyed "1" align on neither
    assert_eq!(ok(&d, &["= %(1 = 10) + %('1' = 1) | keys | count"]), "2");
    // pivot's row keys are the result's labels
    assert_eq!(
        ok(
            &d,
            &["= @(%(r = 1; c = 'a'; v = 10); %(r = 1.0; c = 'b'; v = 20)) | pivot(:r; :c; :v)"]
        ),
        "#(@(r; a; b);\n  %(1; 10; 20))"
    );
}

/// A label and a record key are found as values: the written
/// offset of an instant and the written unit of a quantity are
/// display.
#[test]
fn a_label_is_found_as_a_value() {
    let d = fixture("lookup");
    assert_eq!(ok(&d, &["= %(1_km = 1; 2_km = 2)[[1000_m]]"]), "1");
    assert_eq!(ok(&d, &["= %(1 = 'a')[[1.0]]"]), "a");
    assert_eq!(ok(&d, &["= %(1 = 'a')[['1']]"]), "null");
    // the row labelled 00:00Z, asked for under another offset and
    // as the day
    for label in ["2013-01-01T01:00:00+01:00", "2013-01-01"] {
        assert_eq!(
            ok(
                &d,
                &[&format!("/* @| #(-i=:d) | $_[[{label}]] | :v"), "inst.csv"]
            ),
            "1",
            "{label}"
        );
    }
    // two rows under one label, however written: the column is no
    // record keyed by it
    let e = refused(
        &d,
        &["= #(@(d; v); %(1_km; 1); %(1000_m; 2)) | #(-i=:d) | $_:v"],
    );
    assert!(e.contains("stands on several rows"), "{e}");
}

/// The keyed join and the value match offer every candidate `=`
/// would pick: two unit texts of one magnitude were keyed by their
/// spelling and missed each other.
#[test]
fn the_keyed_join_meets_unit_text_by_magnitude() {
    let d = fixture("join");
    assert_eq!(
        ok(
            &d,
            &[
                "/orders/* <=> /customers/*[::d = $$0::km] | ::id",
                "legs.json"
            ]
        ),
        "1\n2\n4"
    );
    assert_eq!(
        ok(
            &d,
            &["/orders/*::km=>(/customers/*::d) | ::name", "legs.json"]
        ),
        "Xenophon\nCyrus\nClearchus"
    );
}
