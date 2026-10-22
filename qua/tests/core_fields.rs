//! Core fields (spec draft.164): the single colon reads a core
//! field on a node as on a record; `:props` / `:properties` list the
//! node's property projections, `:meta` / `:metadata` its adapter
//! metadata projections, `:fields` the core field projections —
//! lists of PROJECTION VALUES, printed as written and coercing to
//! text as the bare key; `| fields` reads the field projections of
//! any topic; an unknown core field refuses naming them; `| %` on a
//! node reads the record of its properties, `| @` is refused.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"a":{"title":"Iliad","year":-750},"b":[1,2]}"#).unwrap()
}

fn run(q: &str) -> quarb::Result<Vec<String>> {
    let run = quarb::Run::ambient();
    let out = quarb::Query::parse(q, &doc())?.run(&doc(), &run)?;
    Ok(match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::display_form).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    })
}

fn rows(q: &str) -> Vec<String> {
    run(q).unwrap_or_else(|e| panic!("{q}: {e}"))
}

fn refuse(q: &str) -> String {
    match run(q) {
        Err(e) => e.to_string(),
        Ok(out) => panic!("{q} passed: {out:?}"),
    }
}

#[test]
fn the_inspection_fields_list_the_three_ladders_as_projections() {
    assert_eq!(rows("/a:props"), ["@(::title; ::year)"]);
    assert_eq!(rows("/a:properties"), ["@(::title; ::year)"]);
    // a JSON array names its items by position, as `::` reads them
    assert_eq!(rows("/b:props"), ["@(::0; ::1)"]);
    // the adapter's `:::` keys the node answers
    let meta = rows("/a:meta");
    assert!(meta[0].contains(":::type"), "{meta:?}");
    assert_eq!(rows("/a:metadata"), meta);
    // the core fields themselves, the same list on every node
    let fields = rows("/a:fields");
    for k in [":name;", ":id;", ":index;", ":traits;", ":kind;", ":props;", ":meta;", ":fields;"] {
        assert!(fields[0].contains(k), "{k} in {fields:?}");
    }
    assert_eq!(rows("/b:fields"), fields);
    // the kind: the first trait, the word that says what the node
    // is in its adapter's model (spec: Traits); never null
    assert_eq!(rows("/a:kind"), ["object"]);
    assert_eq!(rows("/b:kind"), ["array"]);
    assert_eq!(rows("/a/*:kind"), rows("/a/* | :traits | first"));
    assert_eq!(rows("/a/*[:kind = \"number\"] | :name"), rows("/a/*<number> | :name"));
    // the canonical spellings print short
    for (q, canon) in [("/a:properties", "/a:props"), ("/a:metadata", "/a:meta")] {
        assert_eq!(quarb::Query::parse(q, &doc()).unwrap().canonical(), canon);
    }
}

#[test]
fn a_projection_value_prints_as_written_and_coerces_to_its_key() {
    assert_eq!(rows("/a:props | [1] | type"), ["projection"]);
    assert_eq!(rows("/a:props | join(\", \")"), ["title, year"]);
    assert_eq!(rows("/a:props | [1] | ($_ = \"title\")"), ["true"]);
    assert_eq!(rows("/a:props | [1] | upper"), ["TITLE"]);
    // the list pasted after the node reads what it names
    assert_eq!(rows("/a | @(::title; ::year)"), [r#"@("Iliad"; -750)"#]);
}

#[test]
fn the_fields_command_reads_any_topic() {
    assert_eq!(rows("= %(a = 1; b = 2) | fields"), ["@(:a; :b)"]);
    assert_eq!(rows("= %('my key' = 1; b = 2) | fields"), ["@(:'my key'; :b)"]);
    assert_eq!(rows("= #(%(a = 1; b = 2); %(a = 3; b = 4)) | fields"), ["@(:a; :b)"]);
    assert_eq!(rows("= #(%(1; 2)) | fields"), ["@(:1; :2)"]);
    assert_eq!(rows("/a | fields"), rows("/a:fields"));
    // over a stream of records, the union in first-seen order
    assert_eq!(
        rows("= @(%(a = 1); %(b = 2; a = 3)) | ... @| fields"),
        ["@(:a; :b)"]
    );
    // keys and header keep answering text
    assert_eq!(rows("= %(a = 1; b = 2) | keys"), [r#"@("a"; "b")"#]);
    // a list of one is a list
    assert_eq!(rows("= %(a = 1) | fields"), ["@(:a)"]);
    assert_eq!(rows("= %(a = 1) | keys"), [r#"@("a")"#]);
    assert_eq!(rows("= %(a = 1) | values"), ["@(1)"]);
    assert_eq!(rows("= #(%(a = 1)) | header"), [r#"%(a = "int")"#]);
    assert_eq!(rows("= @(@(1)) | flatten"), ["@(1)"]);
    let e = refuse("= 7 | fields");
    assert!(e.contains("fields reads a record's fields"), "{e}");
}

#[test]
fn an_unknown_core_field_is_refused_naming_the_core_fields() {
    let e = refuse("/a:zzz");
    assert!(
        e.contains("':zzz' is no core field") && e.contains("props") && e.contains("'::zzz'"),
        "{e}"
    );
    let e = refuse("/* | $$_:zzz");
    assert!(e.contains("':zzz' is no core field"), "{e}");
    // on a record the single colon is a field read: a miss is null;
    // a value that is no record, null among them, answers null too
    assert_eq!(rows("= %(a = 1) | :zzz"), [""]);
    assert_eq!(rows("= null | :zzz"), [""]);
    assert_eq!(rows(r#"= "x" | :zzz"#), [""]);
    assert_eq!(rows("= 3 | :zzz"), [""]);
    assert_eq!(rows("= 3 | %(cmos = (:: | :genus))"), ["%(cmos = null)"]);
    // the node's core field reads where the topic is the node
    assert_eq!(rows("/a | :name"), ["a"]);
    // any other value topic reads the POSITION's core field, an
    // unknown key null: the thread still stands at its node
    assert_eq!(rows("/a | :name | :index"), ["1"]);
    assert_eq!(rows("/a | ::title | :depth"), ["1"]);
    assert_eq!(rows("/* | :name | :zzz"), ["", ""]);
    // a list topic reads the field of each item
    assert_eq!(rows("= @(%(n = 1); %(n = 2)) | :n"), ["@(1; 2)"]);
    assert_eq!(rows("= %(a = 1) | :a"), ["1"]);
    // the known ones stay
    assert_eq!(rows("/a:name"), ["a"]);
    assert_eq!(rows("/a:index"), ["1"]);
}

#[test]
fn the_record_reading_of_a_node_and_no_row_reading() {
    assert_eq!(rows("/a | %"), [r#"%(title = "Iliad"; year = -750)"#]);
    assert_eq!(rows("/a | % | :title"), ["Iliad"]);
    let e = refuse("/a | @");
    assert!(e.contains("a node has no row reading") && e.contains("'| %'"), "{e}");
    // the record then reads as a record: its fields counted
    assert_eq!(rows("/a | % | count"), ["2"]);
}
