//! A read of a column a table lacks is refused (ruling 2026-11-11:
//! "refuse all inexistant column accessors"), naming the columns the
//! table has: the plain read, a row's read in a predicate, a keyed
//! command's key, an expression read on the rows. A record's missing
//! field stays null, and a row that has left its table is a record.

use quarb::{QueryResult, Value};

/// A table with the columns `name` and `floor`, built on a document
/// whose root has the property `limit` and no `floor`.
const T: &str =
    r#"= @(%(name = "Ada"; floor = 3); %(name = "Bob"; floor = 5); %(name = "Cy"; floor = 4)) | #"#;

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"limit": 3, "w": [1]}"#).unwrap()
}

fn values(q: &str) -> Vec<String> {
    match quarb::run(q, &doc()).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryResult::Values(vs) => vs.iter().map(Value::to_quarb).collect(),
        other => panic!("{q}: expected values, got {other:?}"),
    }
}

fn one(q: &str) -> String {
    let v = values(q);
    assert_eq!(v.len(), 1, "{q}: {v:?}");
    v.into_iter().next().unwrap()
}

fn refused(q: &str) -> String {
    match quarb::run(q, &doc()) {
        Ok(v) => panic!("{q}: expected a refusal, got {v:?}"),
        Err(e) => e.to_string(),
    }
}

const NO_COLUMN: &str = "no column 'nosuch' (the columns: name, floor)";

#[test]
fn a_read_of_a_missing_column_is_refused() {
    for tail in [
        // the table's own read, bare and written out, alone and in a
        // constructor, and through the capsa
        ":nosuch",
        "$_:nosuch",
        "%(a = :nosuch)",
        "@(:nosuch)",
        ".t | $.t:nosuch",
        // the row predicate, the bare tests among them: a table's
        // columns are known
        r#"[:nosuch = "x"]"#,
        "[:nosuch]",
        "[!:nosuch]",
        "[:floor > 3 && :nosuch > 1]",
        // the keyed commands
        "sort_by(:nosuch)",
        "unique_by(:nosuch)",
        "min_by(:nosuch)",
        "max_by(:nosuch)",
        "top(1; :nosuch)",
        "bottom(1; :nosuch)",
        // an expression read on the rows
        "sum(--of=:nosuch)",
        "sum(--of=:floor + :nosuch)",
        // refused before the ruling, the same words
        "map(:nosuch)",
        "#(:name; :nosuch)",
    ] {
        let q = format!("{T} | {tail}");
        let e = refused(&q);
        assert!(e.contains(NO_COLUMN), "{tail}: {e}");
    }
    // the keyed commands after the aggregation pipe, where the one
    // thread's table is ranked by its rows as well
    for tail in [
        "sort_by(:nosuch)",
        "unique_by(:nosuch)",
        "min_by(:nosuch)",
        "max_by(:nosuch)",
        "top(1; :nosuch)",
        "bottom(1; :nosuch)",
        "sum(--of=:nosuch)",
    ] {
        let q = format!("{T} @| {tail}");
        let e = refused(&q);
        assert!(e.contains(NO_COLUMN), "@| {tail}: {e}");
    }
    // an operand that names a column: `melt`'s ids
    for tail in ["melt(:nosuch)", "melt(:name; :nosuch)"] {
        let e = refused(&format!("{T} | {tail}"));
        assert!(e.contains(&format!("melt: {NO_COLUMN}")), "{tail}: {e}");
    }
    // an indexed table, read and filtered
    for tail in [":nosuch", "[:nosuch > 1]"] {
        let e = refused(&format!("{T} | #(-i=:name) | {tail}"));
        assert!(e.contains(NO_COLUMN), "{tail}: {e}");
    }
}

#[test]
fn the_columns_a_table_has_answer_as_before() {
    assert_eq!(one(&format!("{T} | :name")), r#"@("Ada"; "Bob"; "Cy")"#);
    assert_eq!(
        one(&format!("{T} | [:floor > 3] | :name")),
        r#"@("Bob"; "Cy")"#
    );
    assert_eq!(
        one(&format!("{T} | sort_by(-:floor) | :name")),
        r#"@("Bob"; "Cy"; "Ada")"#
    );
    assert_eq!(one(&format!("{T} | sum(--of=:floor)")), "12");
    assert_eq!(one(&format!("{T} | melt(:name) | :value")), "@(3; 5; 4)");
    // after the aggregation pipe the same rows, the same answers
    assert_eq!(
        one(&format!("{T} @| sort_by(-:floor) | :name")),
        r#"@("Bob"; "Cy"; "Ada")"#
    );
    assert_eq!(
        one(&format!("{T} @| top(1; :floor) | :name")),
        r#"@("Bob")"#
    );
    assert_eq!(
        one(&format!("{T} | #(-i=:name) | :floor")),
        "%(Ada = 3; Bob = 5; Cy = 4)"
    );
    // a pipe tail in the predicate reads its own topic, not the row
    assert_eq!(
        one(&format!(r#"{T} | [(:name | upper) = "ADA"] | :floor"#)),
        "@(3)"
    );
}

/// A property read asks the thread's node, not the row. Where the
/// table has a column of that name and the node no such property,
/// the row's cell was meant and the refusal points at the one-colon
/// read; a node that has the property keeps the comparison of a
/// column with it.
#[test]
fn two_colons_on_a_row_point_at_the_column() {
    let e = refused(&format!("{T} | [::floor > 3]"));
    assert!(
        e.contains("'::floor' reads a property of the thread's node")
            && e.contains("the row's column is ':floor'"),
        "{e}"
    );
    // the root has `limit`: each row's cell against the node's property
    assert_eq!(
        one(&format!("{T} | [:floor > ::limit] | :name")),
        r#"@("Bob"; "Cy")"#
    );
    // a name that is no column is a property read like any other
    assert_eq!(one(&format!("{T} | [::nosuch > 3]")), "#(@(name; floor))");
}

/// On a table without a header the columns are positions.
#[test]
fn a_header_less_table_names_its_positions() {
    let t = "= @(@(1; 2); @(3; 4)) | #";
    assert_eq!(one(&format!("{t} | :1")), "@(1; 3)");
    assert_eq!(one(&format!("{t} | [:1 = 3]")), "#(@(3; 4))");
    for tail in [":x", "[:x = 1]"] {
        let e = refused(&format!("{t} | {tail}"));
        assert!(
            e.contains(
                "'x' names no column: the table has no header, its columns are positions (1 to 2)"
            ),
            "{tail}: {e}"
        );
    }
}

/// A record's missing field is null, and a row that has left its
/// table is a record: taken out by position, as the first row, or
/// with the rows as a list. (The null is told of when no record has
/// the field: unknown_field.rs.)
#[test]
fn a_record_s_missing_field_stays_null() {
    assert_eq!(one("= %(a = 1) | :nosuch"), "null");
    assert!(values("= @(%(a = 1); %(a = 2)) | [:nosuch = 1]").is_empty());
    assert_eq!(one(&format!("{T} | [1] | :nosuch")), "null");
    assert_eq!(one(&format!("{T} | first | :nosuch")), "null");
    assert_eq!(
        values(&format!("{T} | @ | ... | :nosuch")),
        ["null", "null", "null"]
    );
    // several rows are still a table
    let e = refused(&format!("{T} | first(2) | :nosuch"));
    assert!(e.contains(NO_COLUMN), "{e}");
}
