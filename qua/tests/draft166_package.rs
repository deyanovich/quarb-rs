//! The package of 2026-10-24 (spec draft 166): the optional head
//! sigil, `pretty` over records of one shape, the scalar commands'
//! broadcast, the reducer over records of one shape, `nulls`, the
//! record of columns under `| #`, and the printed null.

use quarb::QueryResult;

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"a":1}"#).unwrap()
}

fn rows(q: &str) -> Vec<String> {
    match quarb::run(q, &doc()).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryResult::Values(vs) => vs.iter().map(|v| v.display_form()).collect(),
        other => panic!("{q}: {other:?}"),
    }
}

fn one(q: &str) -> String {
    let v = rows(q);
    assert_eq!(v.len(), 1, "{q}: {v:?}");
    v.into_iter().next().unwrap()
}

fn refuse(q: &str) -> String {
    quarb::run(q, &doc()).unwrap_err().to_string()
}

#[test]
fn the_head_sigil_is_optional_before_a_value_only_token() {
    // a literal of any kind, a constructor, a quarq, a session read
    assert_eq!(one(r#""ab" | upper"#), "AB");
    assert_eq!(one("@(1; 3; 5) | mean"), "3");
    assert_eq!(one("5 | round"), "5");
    assert_eq!(one("2013-01-01 | year"), "2013");
    assert_eq!(one("5_min | type"), "duration");
    assert_eq!(one("nan | type"), "float");
    assert_eq!(one("true | type"), "bool");
    assert_eq!(one("%(a = 1) | type"), "record");
    assert_eq!(one("#(@(a); %(1)) | dim"), "@(1; 1)");
    assert_eq!(one("q(* 2) | type"), "quarq");
    // the sigil stays before a parenthesis, a minus and a name
    assert_eq!(one("= -5 | abs"), "5");
    assert_eq!(one("= (1 + 2) | type"), "int");
    assert!(refuse("-5 | abs").contains("navigation"));
    // the unparser prints the sigil
    let q = quarb::Query::parse(r#""ab" | upper"#, &doc()).unwrap();
    assert!(q.canonical().starts_with("= "), "{}", q.canonical());
}

#[test]
fn scalar_commands_broadcast_one_level_deep() {
    assert_eq!(one(r#"@("A"; "b") | lower"#), r#"@("a"; "b")"#);
    assert_eq!(one("@(1.26; 2.5) | round(1)"), "@(1.3; 2.5)");
    assert_eq!(
        one(r#"%(a = "X"; b = "y") | upper"#),
        r#"%(a = "X"; b = "Y")"#
    );
    assert_eq!(
        one(r#"#(@(a; b); %("x"; 1)) | upper"#),
        "#(@(a; b);\n  %(\"X\"; 1))"
    );
    // one level: a nested list is handed to the command whole
    assert_eq!(one("@(@(1.26); 2.5) | round(1)"), "@(null; 2.5)");
    // an index column stands
    assert_eq!(
        one(r#"#(@(d; a); %("k"; "x")) | #(-i=:d) | upper | :::index"#),
        r#"@("k")"#
    );
    // an expanding command's values gather per item
    assert_eq!(one(r#"@("a b"; "c") | words"#), r#"@(@("a"; "b"); "c")"#);
    // the null guard broadcasts as `??` does, the substitution as `lower`
    assert_eq!(one("@(1; null) | default(0)"), "@(1; 0)");
    assert_eq!(one(r#"@("a"; "b") | s/a/x/"#), r#"@("x"; "b")"#);
    // list-input commands read the structure whole
    assert_eq!(one(r#"@("a"; "b") | count"#), "2");
    assert_eq!(one("@(1; 2) | mean"), "1.5");
}

#[test]
fn a_reducer_over_records_of_one_shape_goes_column_by_column() {
    assert_eq!(
        one("@(%(a = 1; b = 2); %(a = 3; b = 4)) | mean"),
        "%(a = 2; b = 3)"
    );
    assert_eq!(
        one("@(%(a = 1; b = 2); %(a = 3; b = 4)) | sum"),
        "%(a = 4; b = 6)"
    );
    // a list it cannot read answers null, printed
    assert_eq!(one(r#"@("a"; %(x = 1)) | mean"#), "null");
    assert_eq!(one("null"), "null");
    assert_eq!(one(r#""""#), "");
}

#[test]
fn pretty_prints_records_of_one_shape_as_the_grid() {
    assert_eq!(
        one("@(%(a = 1; b = 2); %(a = 3; b = 4)) | pretty"),
        "a  b\n-  -\n1  2\n3  4"
    );
    assert_eq!(
        one("@(%(a = 1; b = 2); %(a = 3; c = 4)) | pretty"),
        "@(%(a = 1;\n    b = 2);\n  %(a = 3;\n    c = 4))"
    );
}

#[test]
fn nulls_counts_the_null_fields() {
    assert_eq!(one("%(a = 1; b = null; c = null) | nulls"), "2");
    const T: &str = "#(@(a; b); %(1; null); %(null; null); %(1; 2))";
    assert_eq!(one(&format!("{T} | nulls")), "@(1; 2; 0)");
    assert_eq!(one(&format!("{T} | [nulls = 0] | dim")), "@(1; 2)");
    assert_eq!(one(&format!("{T} | [nulls < 2] | dim")), "@(2; 2)");
    assert_eq!(
        one("@(%(a = 1; b = null); %(a = 1; b = 2)) @| nulls"),
        "@(1; 0)"
    );
}

#[test]
fn a_list_of_one_item_is_a_list() {
    // spec: List Construction (draft 166): `@(1)` is the list holding
    // 1; the record-array sugar of one row is a list; a one-item list
    // under `| #` is rows (a one-row frame, a list cell refusing), the
    // column view being a bare record topic's
    assert_eq!(one("@(1) | type"), "list");
    assert_eq!(one("@(@(1; 2)) | count"), "1");
    assert_eq!(one("@(%(a = 1)) | type"), "list");
    assert_eq!(one("@%(@(a); %(1)) | type"), "list");
    assert_eq!(one("@%(@(a); %(1)) | json"), r#"[{"a": 1}]"#);
    assert_eq!(one("@(%(a = 1; b = 2)) | # | dim"), "@(1; 2)");
    let e = refuse("@(%(a = 1; b = @(2; 3))) | #");
    assert!(e.contains("a table is flat"), "{e}");
    assert_eq!(one("%(a = 1; b = @(2; 3)) | # | dim"), "@(2; 2)");
    // the one collapse runs the other way: a value where a list is
    // expected is a list of one
    assert_eq!(one("5 | count"), "1");
    assert_eq!(one("5 | mean"), "5");
}

#[test]
fn a_descriptor_glues_to_a_value() {
    // `value:::key` reads a descriptor of a record or a table (draft
    // 166), what `| :::key` answers; any other key stays retired
    const T: &str = "#(@(d; a; b); %(1; 2; 3); %(4; 5; 6)) | #(-i=:d)";
    assert_eq!(one(&format!("{T} | $_:::index")), "@(1; 4)");
    assert_eq!(one(&format!("{T} | $_:::dim")), "@(2; 3)");
    assert_eq!(one(&format!("{T} | .t | $.t:::nrows + 1")), "3");
    assert_eq!(
        one(&format!("{T} | %(k = $_:::keys; n = $_:::ncols)")),
        r#"%(k = @("d"; "a"; "b"); n = 3)"#
    );
    assert_eq!(one("%(a = 1) | $_:::type"), "record");
    // (the session read `$,t:::header` rides the same arm; the test
    // runner has no session memory, so a register stands in)
    assert_eq!(
        one(&format!("{T} | .t | $.t:::header")),
        "@(:d<*; int>; :a<*; int>; :b<*; int>)"
    );
    let e = refuse("%(a = 1) | $_:::zzz");
    assert!(e.contains("retired"), "{e}");
    // the canonical text keeps the glued spelling
    let q = quarb::Query::parse("= %(a = 1) | $_:::type", &doc()).unwrap();
    assert!(q.canonical().contains("$_:::type"), "{}", q.canonical());
}

#[test]
fn the_matrix_form_of_the_table_literal() {
    // rows that are all lists are a headerless table (spec: Table
    // Construction, draft 166); a headerless table prints as its lists
    assert_eq!(one("#(@(1; 2); @(3; 4))"), "#(@(1; 2);\n  @(3; 4))");
    assert_eq!(one("#(@(1; 2); @(3; 4)) | :::dim"), "@(2; 2)");
    assert_eq!(
        one("#(@(1; 2); @(3; 4)) | header"),
        "@(:1<*; int>; :2<*; int>)"
    );
    assert_eq!(
        one("#(@(1; 2); @(3; 4)) | transpose"),
        "#(@(1; 3);\n  @(2; 4))"
    );
    // a single list of numbers alone is a one-row matrix
    assert_eq!(one("#(@(1; 2)) | :::dim"), "@(1; 2)");
    // positional records stay rows, printing as the matrix they are
    assert_eq!(one("#(%(1; 2); %(3; 4))"), "#(@(1; 2);\n  @(3; 4))");
    // a list followed by records is the header, as before
    assert_eq!(
        one(r#"#(@(name; age); %("Braund"; 22)) | header"#),
        "@(:name<*; text>; :age<*; int>)"
    );
    // the rows are of one sigil
    assert!(refuse("#(%(1; 2); @(3; 4))").contains("a list after a record row"));
    assert!(refuse("#(@(1; 2); @(3; 4); %(5; 6))").contains("a record after a matrix row"));
    // a lone list of names is a header of no rows, pointing at --header
    assert!(refuse("#(@(a; b))").contains("--header"));
}

#[test]
fn the_header_has_a_name_and_transpose_exchanges_it_with_the_index() {
    // `#(--header=…; --name=word)` names the header, `:::name` reads
    // it back, null where none; `column` is the no-name default
    const T: &str = "@(1; 2) | #(--header=@(:a; :b); --name=letters)";
    assert_eq!(one(&format!("{T} | :::name")), "letters");
    assert_eq!(one(&format!("{T} | $_:::name")), "letters");
    assert_eq!(one("@(1; 2) | #(--header=@(:a; :b)) | :::name"), "null");
    assert_eq!(
        one("@(1; 2) | #(--header=@(:a; :b)) | #(--name=letters) | :::name"),
        "letters"
    );
    assert_eq!(
        one("@(1; 2) | #(--header=@(:a; :b); --name=column) | :::name"),
        "null"
    );
    assert_eq!(one(&format!("{T} | [:a > 0] | :::name")), "letters");
    // transpose: the header's name becomes the new index column's,
    // `column` where it has none; the index column's name the header's
    assert_eq!(
        one(&format!("{T} | transpose | header")),
        "@(:letters<*; text>; :1<*; int>)"
    );
    const D: &str = "#(@(date; A; B); %(2013-01-01; 1; 10); %(2013-01-02; 2; 20)) | #(-i=:date)";
    assert_eq!(one(&format!("{D} | transpose | :::name")), "date");
    assert_eq!(
        one(&format!("{D} | transpose | header")),
        "@(:column<*; text>; :'2013-01-01'<*; int>; :'2013-01-02'<*; int>)"
    );
    assert_eq!(
        one(&format!("{D} | transpose | transpose | header")),
        "@(:date<*; instant>; :A<*; int>; :B<*; int>)"
    );
    assert_eq!(
        one(&format!("{D} | transpose | transpose | :::name")),
        "null"
    );
    // the canonical text keeps the option
    let q = quarb::Query::parse(&format!("= {T}"), &doc()).unwrap();
    assert!(
        q.canonical().contains("--name=letters"),
        "{}",
        q.canonical()
    );
}

#[test]
fn brackets_glued_to_a_value_take_a_condition() {
    // brackets glued to a value read as on a step (draft 166): a
    // condition keeps the rows of a table or the items of a list, each
    // judged as the topic; header and index kept
    const T: &str = "#(@(d; A; C); %(1; 1; \"x\"); %(2; 2; \"y\"); %(3; 3; \"x\")) | #(-i=:d)";
    assert_eq!(one(&format!("{T} | $_[:A > 1] | :::index")), "@(2; 3)");
    assert_eq!(
        one(&format!("{T} | $_[:A > 1] | header")),
        "@(:d<*; int>; :A<*; int>; :C<*; text>)"
    );
    assert_eq!(
        one(&format!("{T} | .t | $.t[:C <<= @(\"x\")] | :::dim")),
        "@(2; 3)"
    );
    assert_eq!(one(&format!("{T} | $_[nulls = 0] | :::dim")), "@(3; 3)");
    assert_eq!(one("@(1; 5; 3) | $_[$_ > 2]"), "@(5; 3)");
    // the index forms stand: a position, a range, a list, a cell, a name
    assert_eq!(one("@(1; 5; 3) | .v | $.v[2]"), "5");
    assert_eq!(one("@(1; 5; 3) | .v | $.v[2..3]"), "@(5; 3)");
    assert_eq!(one("@(1; 5; 3) | .v | $.v[@(1; 3)]"), "@(1; 3)");
    assert_eq!(one("#(@(a; b); %(1; 2); %(3; 4)) | $_[2; \"b\"]"), "4");
    assert_eq!(one("%(a = 1; b = 2) | $_[\"a\"]"), "1");
    // a bare variable: a position when it holds a number, a condition
    // judged once otherwise
    assert_eq!(one("2 | .n | @(1; 5; 3) | $_[$.n]"), "5");
    assert_eq!(one("true | .n | @(1; 5; 3) | $_[$.n]"), "@(1; 5; 3)");
    assert_eq!(one("false | .n | @(1; 5; 3) | $_[$.n]"), "@()");
    // a record takes no condition; a scalar has no rows or items
    assert!(refuse("%(a = 1; b = 2) | $_[:a > 0]").contains("takes no condition"));
    assert!(refuse("5 | $_[$_ > 2]").contains("not a number"));
    // the canonical text keeps the condition in the brackets
    let q = quarb::Query::parse("= @(1; 5; 3) | $_[$_ > 2]", &doc()).unwrap();
    assert!(q.canonical().contains("$_[$_ > 2]"), "{}", q.canonical());
    // two tables compare item by item: `t[cond]` and `t | [cond]` agree
    assert_eq!(
        one(&format!("{T} | ($_[:A > 1]) = ($_ | [:A > 1])")),
        "true"
    );
}

#[test]
fn two_tables_compare_item_by_item() {
    // spec: Values, equality (draft 166): headers in order, declared
    // types alike, rows cell by cell under the coercions, the index
    // declarations alike; the header's name does not enter
    assert_eq!(one("#(@(a); %(1)) = #(@(a); %(1))"), "true");
    assert_eq!(one("#(@(a); %(1)) = #(@(b); %(1))"), "false");
    assert_eq!(one("#(@(a); %(1)) = #(@(a); %(2))"), "false");
    assert_eq!(one("#(@(a); %(1)) = #(@(a); %(\"1\"))"), "true");
    assert_eq!(one("#(@(a); %(1); %(2)) = #(@(a); %(1))"), "false");
    assert_eq!(
        one("= (#(@(d; a); %(1; 2)) | #(-i=:d)) = (#(@(d; a); %(1; 2)) | #(-i=:d))"),
        "true"
    );
    assert_eq!(
        one("= (#(@(d; a); %(1; 2)) | #(-i=:d)) = #(@(d; a); %(1; 2))"),
        "false"
    );
    assert_eq!(
        one("= (@(1) | #(--header=@(:a<int>))) = (@(1) | #(--header=@(:a)))"),
        "false"
    );
    assert_eq!(
        one("= (@(1) | #(--header=@(:a); --name=x)) = (@(1) | #(--header=@(:a)))"),
        "true"
    );
    assert_eq!(one("#(@(1; 2); @(3; 4)) = #(@(1; 2); @(3; 4))"), "true");
}

#[test]
fn a_record_of_columns_builds_the_table() {
    const R: &str = r#"%(A = 1.0; B = 2013-01-02; C<float> = @(1; 1; 1; 1); D<int> = @(3; 3; 3; 3); E = @("test"; "train"; "test"; "train"); F = "foo")"#;
    assert_eq!(one(&format!("{R} | # | dim")), "@(4; 6)");
    assert_eq!(
        one(&format!("{R} | # | header")),
        "@(:A<*; float>; :B<*; instant>; :C<float>; :D<int>; :E<*; text>; :F<*; text>)"
    );
    assert_eq!(one(&format!("{R} | # | [2] | :E")), "train");
    assert_eq!(one("%(a = 1; b = 2) | # | dim"), "@(1; 2)");
    let e = refuse("%(a = @(1; 2); b = @(1; 2; 3)) | #");
    assert!(e.contains("'a' holds 2 values, 'b' holds 3"), "{e}");
}
