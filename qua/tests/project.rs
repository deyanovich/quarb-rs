//! `| project(expr)` / `| project(expr; --record)` (spec: Projection
//! Values): the operand's value with every projection value in it,
//! at any depth, replaced by its reading on the cursor; `-r` keys a
//! list by the projections' bare keys.
use quarb::QueryResult;

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"a":{"x":1,"y":"two","z":[1,2]},"r":{"t":"s"}}"#).unwrap()
}

fn rows(q: &str) -> Vec<String> {
    match quarb::run(q, &doc()).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryResult::Values(vs) => vs.iter().map(|v| v.display_form()).collect(),
        other => panic!("{q}: {other:?}"),
    }
}

fn refuse(q: &str) -> String {
    quarb::run(q, &doc()).unwrap_err().to_string()
}

#[test]
fn project_reads_projections_on_the_cursor() {
    // one projection written bare: its value, as `| :name` answers
    assert_eq!(rows("/a | project(:name)"), rows("/a | :name"));
    assert_eq!(rows("/a | project(::z)"), ["@(1; 2)"]);
    assert_eq!(rows("/a | project(:kind)"), ["object"]);
    // a list of projections: the values in order; -r the record
    // keyed by the bare keys
    assert_eq!(rows("/a | project(@(:name; ::x; :kind))"), [r#"@("a"; 1; "object")"#]);
    assert_eq!(
        rows("/a | project(@(:name; ::x; :kind); -r)"),
        [r#"%(name = "a"; x = 1; kind = "object")"#]
    );
    // projection VALUES: what :props, :fields, :meta answer, or a
    // capsa holding them
    assert_eq!(rows("/a | project(:props)"), [r#"@(1; "two"; @(1; 2))"#]);
    assert_eq!(rows("/a | project(:props; -r)"), rows("/a | %"));
    assert_eq!(rows("/a | project(:meta; -r)"), [r#"%(type = "object"; length = 3)"#]);
    assert_eq!(rows("/a | .p(:props) | project($.p; -r)"), rows("/a | %"));
    let fields = rows("/a | project(:fields; -r) | keys");
    assert!(fields[0].starts_with(r#"@("name"; "id"; "index""#), "{fields:?}");
    // the one spelling on any topic: a parenthesised operand is a
    // body on the cursor, `fields` answering the node's core fields,
    // a record's fields, a table's columns; a bare word among the
    // operands stays text
    assert_eq!(rows("/a | project((fields); -r)"), rows("/a | project(:fields; -r)"));
    assert_eq!(rows("/a | % | project((fields); -r)"), rows("/a | %"));
    assert_eq!(
        rows(r#"= #(@(name; age); %("Braund"; 22)) | project((fields); -r) | keys"#),
        [r#"@("name"; "age")"#]
    );
    assert_eq!(rows("/a | project((fields | [1]))"), ["a"]);
    assert_eq!(rows("/a | project(fields)"), ["fields"]);
    // on a record topic the bottom rung
    assert_eq!(rows("/a | % | project(:x)"), ["1"]);
    assert_eq!(rows("/a | % | project(@(:x; :y); -r)"), [r#"%(x = 1; y = "two")"#]);
    // per capsa, as every command on the simple pipe
    assert_eq!(rows("/a/* | project(:name)"), ["x", "y", "z"]);
    // a null reads null
    assert_eq!(rows("/a | project(::nothing)"), [""]);
    // projections at any depth of lists, records and tables are
    // read in place; every other value is kept, a text among them
    assert_eq!(rows("/a | project(@(:id; 1))"), [format!("@({}; 1)", rows("/a | :id")[0])]);
    assert_eq!(
        rows("/a | project(%(n = :name; k = :kind; fixed = 1))"),
        [r#"%(n = "a"; k = "object"; fixed = 1)"#]
    );
    assert_eq!(
        rows("/a | project(#(@(k; v); %(:name; 1); %(:kind; 2)))"),
        ["#(@(k; v);\n  %(\"a\"; 1);\n  %(\"object\"; 2))"]
    );
    assert_eq!(rows(r#"/a | project("::name")"#), ["::name"]);
    assert_eq!(rows("/a | project(5)"), ["5"]);
    assert_eq!(rows("/a | project(@(1; 2))"), ["@(1; 2)"]);
    // a projection map held as data
    assert_eq!(
        rows("/a | .p(:props) | project(%(all = $.p; first = ($.p | [1])))"),
        [r#"%(all = @(1; "two"; @(1; 2)); first = 1)"#]
    );
}

#[test]
fn project_refuses_what_is_no_projection() {
    let e = refuse("/a | project(@(:name; ::name); -r)");
    assert!(e.contains("share the bare key 'name' (:name and ::name)"), "{e}");
    // -r keys a list: an item with no key, a record, a scalar refuse
    let e = refuse("/a | project(@(1; 2); -r)");
    assert!(e.contains("item 1 of the list is no projection (1)"), "{e}");
    let e = refuse("/a | project(%(n = :name); -r)");
    assert!(e.contains("the record has its keys already"), "{e}");
    let e = refuse("/a | project(fields; -r)");
    assert!(e.contains("text is no list"), "{e}");
    let e = refuse("/a | project(5; -r)");
    assert!(e.contains("a number is no list"), "{e}");
    let e = refuse("/a | project()");
    assert!(e.contains("takes the value to read the projections of"), "{e}");
    let e = refuse("/a | project(:name; -x)");
    assert!(e.contains("--record"), "{e}");
}
