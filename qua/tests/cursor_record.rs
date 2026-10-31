//! The cursor as the input (spec: The Cursor): `_` the cursor
//! record, `_:topic` and `_:position` its fields, `#_` the context
//! table; and the signed quantity literal.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"a":[{"k":1,"name":"x"},{"k":2,"name":"y"}]}"#).unwrap()
}

fn run(q: &str) -> quarb::Result<Vec<String>> {
    let run = quarb::Run::ambient();
    let out = quarb::Query::parse(q, &doc())?.run(&doc(), &run)?;
    Ok(match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::display_form).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    })
}

fn all(q: &str) -> String {
    run(q).unwrap().join(" | ")
}

fn refused(q: &str) -> String {
    match run(q) {
        Ok(v) => panic!("{q}: expected a refusal, got {v:?}"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn the_cursor_is_a_record_of_the_topic_and_the_position() {
    assert_eq!(
        all("/a/* | _"),
        "%(topic = null; position = /a/0) | %(topic = null; position = /a/1)"
    );
    assert_eq!(
        all("/a/*::k | _"),
        "%(topic = 1; position = /a/0) | %(topic = 2; position = /a/1)"
    );
    assert_eq!(all("/a/*::k | _ | type"), "record | record");
    // the fields read as the sigils do
    assert_eq!(all("/a/*::k | _:topic"), "1 | 2");
    assert_eq!(all("/a/* | _:position::k"), "1 | 2");
    assert_eq!(all("/a/* | _:position/name::"), "x | y");
    assert_eq!(all("/a/*[_:position::k = 2]::name"), "y");
    assert_eq!(all("= @(1; 2) $| (_:topic * 2)"), "@(2; 4)");
    assert_eq!(all("= 3 | _:topic"), "3");
    // any other field misses, as on any record
    assert_eq!(all("= 3 | _:x | type"), "null");
    // a node projection on the cursor says where to go
    let e = refused("/a/* | _::k");
    assert!(e.contains("'_' is the cursor"), "{e}");
    assert!(e.contains("'_:position::k' or '$$_::k'"), "{e}");
    let e = refused("/a/* | %(up = _::k)");
    assert!(e.contains("'_' is the cursor"), "{e}");
}

#[test]
fn the_context_table_is_the_cursors_plural() {
    assert_eq!(
        all("/a/*::k | #_ @| first"),
        // (a table column of references is a `node` column: the
        // cells stay references and print as the path, as the
        // `node` column of `@| nodes` does)
        "#(@(topic; position);\n  %(1; /a/0);\n  %(2; /a/1))"
    );
    assert_eq!(all("/a/*::k | #_:topic @| first"), "@(1; 2)");
    assert_eq!(all("/a/*::k | @_ @| first"), "@(1; 2)");
    assert_eq!(all("/a/* | #_:position @| first"), "@(/a/0; /a/1)");
    assert_eq!(all("/a/* | @@_ @| first"), "@(/a/0; /a/1)");
}

#[test]
fn a_signed_quantity_literal_is_a_quantity() {
    assert_eq!(all("= -1_km | type"), "quantity");
    assert_eq!(all("= +1_km | type"), "quantity");
    assert_eq!(all("= -1_km"), "-1_km");
    assert_eq!(all("= ((0_km - 1_km) = -1_km)"), "true");
    assert_eq!(all("= -1_km + 1_km"), "0_km");
    assert_eq!(all("= @(-1_km; 2_km) | sum"), "1_km");
    assert_eq!(all("= -1.5_km | type"), "quantity");
}

#[test]
fn a_reduction_leaves_a_thread_with_no_position() {
    // spec: Aggregating Pipe — after a reduction `$$_` is null, the
    // cursor's position is null, and navigation from the position
    // yields nothing by construction
    assert_eq!(all("/a/* @| count | $$_ | type"), "null");
    assert_eq!(all("/a/* @| count | _"), "%(topic = 2; position = null)");
    // (a navigation in value position that reaches nothing is null,
    // as `$$__/x` is where no x stands)
    assert_eq!(all("/a/* @| count | $$_/a/*::k | type"), "null");
    assert_eq!(
        all("/a/* | %(x = ::k; y = ::k) @| corr(:x; :y) | $$_ | type"),
        "null"
    );
    assert_eq!(all("/a/* @| json | $$_ | type"), "null");
    assert_eq!(all("/a/* @| tree | $$_ | type"), "null");
    assert_eq!(all("/a/*[::k > 9] @| first | $$_ | type"), "null");
    // the invoking position still says from where: the opener
    // inside a body, nothing at the top level
    assert_eq!(all("/a | .w(/a/* @| count | $$__:name) | $.w"), "a");
    assert_eq!(
        all("/a | .w(/a/* @| count | $$__/*::k @| count) | $.w"),
        "2"
    );
    assert_eq!(all("/a/* @| count | $$__ | type"), "null");
    // selecting and keyed aggregates keep their threads' positions
    assert_eq!(all("/a/* @| first | $$_"), "/a/0");
    assert_eq!(all("/a/* @| sort_by(::k) @| last | $$_"), "/a/1");
    assert_eq!(all("/a/* @| unique | $$_"), "/a/0 | /a/1");
}

#[test]
fn the_capsa_counts_back_and_names_the_penultimate() {
    // spec draft 163: `$.-N` the N-th regula back from the latest,
    // `$..` the penultimate, on both faces, rounded too
    let q = "/a/* | .x(::k) | .y(::k * 10) | .z(::k * 100) | ";
    assert_eq!(all(&format!("{q}$.-1")), "100 | 200");
    assert_eq!(all(&format!("{q}$.-2")), "10 | 20");
    assert_eq!(all(&format!("{q}$..")), "10 | 20");
    assert_eq!(all(&format!("{q}$.-3")), "1 | 2");
    assert_eq!(all(&format!("{q}$.-4 | type")), "null | null");
    assert_eq!(all(&format!("{q}(.-2)")), "10 | 20");
    assert_eq!(all(&format!("{q}(..)")), "10 | 20");
    // spaced, the minus is subtraction
    assert_eq!(all(&format!("{q}$. - 2")), "98 | 198");
    // the node face: the marks counted back
    let m = "/a/0 | .p | /name | .q | \\ | .r | ";
    assert_eq!(all(&format!("{m}$$.-1:name")), "0");
    assert_eq!(all(&format!("{m}$$.-2:name")), "name");
    assert_eq!(all(&format!("{m}$$..:name")), "name");
    assert_eq!(all(&format!("{m}((.-3)):name")), "0");
    assert_eq!(all(&format!("{m}((..)):name")), "name");
    // a malformed capsa read refuses with the forms
    for bad in ["$.+1", "$.x.y", "$.-0", "$.-x"] {
        let e = refused(&format!("{q}{bad}"));
        assert!(e.contains("is no capsa read"), "{bad}: {e}");
    }
}

#[test]
fn the_full_views_and_the_matrices_take_the_star_and_the_dot() {
    let q = "/a/* | .x(::k) | .(::name) | ";
    assert_eq!(all(&format!("{q}%.")), "%(x = 1) | %(x = 2)");
    assert_eq!(
        all(&format!("{q}%.*")),
        "%(x = 1; .2 = \"x\") | %(x = 2; .2 = \"y\")"
    );
    let e = refused(&format!("{q}%.."));
    assert!(e.contains("'%..' is retired") && e.contains("'%.*'"), "{e}");
    let e = refused(&format!("{q}%%.."));
    assert!(e.contains("'%%..' is retired"), "{e}");
    // the matrices: `#.` the named regulae, `#.*` every one
    assert_eq!(all(&format!("{q}#. @| first")), "#(@(x);\n  %(1);\n  %(2))");
    assert_eq!(
        all(&format!("{q}#.* @| first")),
        "#(@(x; .2);\n  %(1; \"x\");\n  %(2; \"y\"))"
    );
    assert_eq!(all(&format!("{q}#.:x @| first")), "@(1; 2)");
    assert_eq!(all(&format!("{q}#.*:2 @| first")), "@(\"x\"; \"y\")");
    assert_eq!(
        all("/a/* | .A | .k(::k) | ##. @| first"),
        "#(@(A);\n  %(/a/0);\n  %(/a/1))"
    );
    for bad in ["#*", "##*"] {
        let e = refused(&format!("{q}{bad} @| first"));
        assert!(
            e.contains("are retired") && e.contains("'#.*'"),
            "{bad}: {e}"
        );
    }
}
