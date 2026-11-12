//! A record's missing field is null, and no longer in silence
//! (ruling 2026-11-11: "null with a warning"): a name that no record
//! the query read under it has is told of once, when the run ends,
//! naming the fields the records have (UNKNOWN_FIELD). Where one
//! record read under the name has the field the others' nulls are
//! the data's, and nothing is said. The answer never changes; a
//! table still refuses a column it lacks.

use quarb::{QueryResult, Value};

/// A table with the columns `name` and `floor`.
const T: &str =
    r#"= @(%(name = "Ada"; floor = 3); %(name = "Bob"; floor = 5); %(name = "Cy"; floor = 4)) | #"#;

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"limit": 3, "w": [1]}"#).unwrap()
}

/// The answers and the diagnostics of a query, each as its text.
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

fn warning(name: &str, there: &str) -> String {
    format!(
        "warning UNKNOWN_FIELD (:{name}): no record read here has the field '{name}'; the \
         fields there: {there}"
    )
}

#[test]
fn a_field_no_record_has_is_told_of() {
    let (values, diagnostics) = outcome("= %(a = 1; b = 2) | :nosuch");
    assert_eq!(values, ["null"]);
    assert_eq!(diagnostics, [warning("nosuch", "a, b")]);
    // the field the record has: nothing to tell
    let (values, diagnostics) = outcome("= %(a = 1; b = 2) | :a");
    assert_eq!(values, ["1"]);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    // a field that is there and null is there
    let (values, diagnostics) = outcome("= %(a = null) | :a");
    assert_eq!(values, ["null"]);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    // a record with no fields
    let (_, diagnostics) = outcome("= %() | :x");
    assert_eq!(diagnostics, [warning("x", "none")]);
}

/// Every way a query reads a record's field by name.
#[test]
fn every_read_by_name_is_tallied() {
    for (q, answer) in [
        ("= %(a = 1; b = 2) | $_:zz", "null"),
        ("= %(a = 1; b = 2) | .r | $.r:zz", "null"),
        ("= %(a = 1; b = 2) | (:zz + 1)", "null"),
        (
            "= %(a = 1; b = 2) | %(p = :a; q = :zz)",
            "%(p = 1; q = null)",
        ),
        ("= %(a = 1; b = 2) | @(:a; :zz)", "@(1; null)"),
        // the index by name and the lookup of one label
        (r#"= %(a = 1; b = 2) | $_["zz"]"#, "null"),
        (r#"= %(a = 1; b = 2) | .r | $.r[["zz"]]"#, "null"),
    ] {
        let (values, diagnostics) = outcome(q);
        assert_eq!(values, [answer], "{q}");
        assert_eq!(diagnostics, [warning("zz", "a, b")], "{q}");
    }
    // a field of a field: the inner record is the one named
    let (values, diagnostics) = outcome("= %(a = %(x = 1)) | :a:y");
    assert_eq!(values, ["null"]);
    assert_eq!(diagnostics, [warning("y", "x")]);
}

/// A list of records reads each: one warning for the name, the
/// fields of the records that lack it gathered in the order met.
#[test]
fn a_list_of_records_is_one_warning() {
    let list = "= @(%(a = 1); %(b = 2); %(a = 3; c = 4))";
    let (values, diagnostics) = outcome(&format!("{list} | :zz"));
    assert_eq!(values, ["@(null; null; null)"]);
    assert_eq!(diagnostics, [warning("zz", "a, b, c")]);
    let (values, diagnostics) = outcome(&format!("{list} | map(:zz)"));
    assert_eq!(values, ["@(null; null; null)"]);
    assert_eq!(diagnostics, [warning("zz", "a, b, c")]);
    // threads of records the same
    let (values, diagnostics) = outcome(&format!("{list} | ... | :zz"));
    assert_eq!(values, ["null", "null", "null"]);
    assert_eq!(diagnostics, [warning("zz", "a, b, c")]);
    // a key no record has leaves the order as it was, and says so
    let (_, diagnostics) = outcome(&format!("{list} | ... @| sort_by(:zz)"));
    assert_eq!(diagnostics, [warning("zz", "a, b, c")]);
}

/// Where one record read under the name has the field, the nulls of
/// the others are the data's: an optional field is no mistake.
#[test]
fn a_field_some_record_has_is_quiet() {
    let list = "= @(%(a = 1); %(b = 2); %(a = 3; c = 4))";
    for (tail, answer) in [
        (":a", "@(1; null; 3)"),
        ("map(:c)", "@(null; null; 4)"),
        ("map(:b ?? 0)", "@(0; 2; 0)"),
    ] {
        let (values, diagnostics) = outcome(&format!("{list} | {tail}"));
        assert_eq!(values, [answer], "{tail}");
        assert!(diagnostics.is_empty(), "{tail}: {diagnostics:?}");
    }
    // the record that has it may come last
    let (values, diagnostics) = outcome(&format!("{list} | ... | :c"));
    assert_eq!(values, ["null", "null", "4"]);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

/// A predicate reads the field as any expression does, the bare test
/// of presence included: quiet where a record has the field, told of
/// where none has, as a node's property is.
#[test]
fn a_predicate_reads_the_field() {
    let list = "= @(%(a = 1); %(b = 2); %(a = 3; c = 4)) | ...";
    let (values, diagnostics) = outcome(&format!("{list} | [:c]"));
    assert_eq!(values, ["%(a = 3; c = 4)"]);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let (values, diagnostics) = outcome(&format!("{list} | [!:c]"));
    assert_eq!(values, ["%(a = 1)", "%(b = 2)"]);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let (values, diagnostics) = outcome(&format!("{list} | [:zz = 1]"));
    assert!(values.is_empty(), "{values:?}");
    assert_eq!(diagnostics, [warning("zz", "a, b, c")]);
    let (values, diagnostics) = outcome(&format!("{list} | [:zz]"));
    assert!(values.is_empty(), "{values:?}");
    assert_eq!(diagnostics, [warning("zz", "a, b, c")]);
    let (values, diagnostics) = outcome(&format!("{list} | [!:zz]"));
    assert_eq!(values.len(), 3);
    assert_eq!(diagnostics, [warning("zz", "a, b, c")]);
}

/// One warning per name, in the order first read, after whatever
/// else the run raised.
#[test]
fn each_name_is_told_of_once() {
    let (values, diagnostics) = outcome("= %(a = 1) | %(p = :x; q = :y; r = :x; s = :a)");
    assert_eq!(values, ["%(p = null; q = null; r = null; s = 1)"]);
    assert_eq!(diagnostics, [warning("x", "a"), warning("y", "a")]);
}

/// A record with many fields: the nearest names first, and the count.
#[test]
fn many_fields_are_named_nearest_first() {
    let fields: Vec<String> = (1..=20).map(|i| format!("f{i:02} = {i}")).collect();
    let (_, diagnostics) = outcome(&format!("= %({}) | :f21", fields.join("; ")));
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    let d = &diagnostics[0];
    assert!(d.starts_with("warning UNKNOWN_FIELD (:f21): "), "{d}");
    assert!(d.ends_with(", … (20 in all)"), "{d}");
    // the names one edit away come before those two away
    let at = |name: &str| d.find(name).unwrap_or_else(|| panic!("{name} not in {d}"));
    assert!(at("f01") < at("f02"), "{d}");
    assert!(at("f20") < at("f02"), "{d}");
}

/// A row that has left its table is a record, and reads as one: the
/// warning names the row's fields.
#[test]
fn a_row_out_of_its_table_is_a_record() {
    for tail in ["[1] | :nosuch", "first | :nosuch"] {
        let (values, diagnostics) = outcome(&format!("{T} | {tail}"));
        assert_eq!(values, ["null"], "{tail}");
        assert_eq!(diagnostics, [warning("nosuch", "name, floor")], "{tail}");
    }
    let (values, diagnostics) = outcome(&format!("{T} | @ | ... | :nosuch"));
    assert_eq!(values, ["null", "null", "null"]);
    assert_eq!(diagnostics, [warning("nosuch", "name, floor")]);
}

/// What is no record has no fields to lack, and a table refuses: a
/// refused run tells its error and nothing else.
#[test]
fn what_is_no_record_is_not_told_of() {
    for q in [
        "= null | :x",
        "= 5 | :x",
        r#"= "text" | :x"#,
        "= @(1; 2) | :x",
    ] {
        let (_, diagnostics) = outcome(q);
        assert!(diagnostics.is_empty(), "{q}: {diagnostics:?}");
    }
    let q = format!("{T} | :nosuch");
    let e = match quarb::run(&q, &doc()) {
        Ok(v) => panic!("{q}: expected a refusal, got {v:?}"),
        Err(e) => e.to_string(),
    };
    assert!(
        e.contains("no column 'nosuch' (the columns: name, floor)"),
        "{e}"
    );
    // a refusal after a record's missing field: the error alone
    let q = format!("{T} | .t | %(a = $.t[1]:zz; b = $.t:nosuch)");
    let e = match quarb::run(&q, &doc()) {
        Ok(v) => panic!("{q}: expected a refusal, got {v:?}"),
        Err(e) => e.to_string(),
    };
    assert!(e.contains("no column 'nosuch'"), "{e}");
    assert!(!e.contains("UNKNOWN_FIELD"), "{e}");
}
