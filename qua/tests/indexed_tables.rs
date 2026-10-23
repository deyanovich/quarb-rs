//! Record keys, the indexed table and the label lookup (spec: Record;
//! The Table, "The index"; Indexing a Value; Structured Values, draft
//! 166): a record's keys are scalars compared as values, a field may
//! be declared, two records align by key; `#(-i=:col)` names a
//! table's labels, `t:A` the record keyed by them, `t[[x]]` the row.
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

const D: &str = "#(@(date; A; B); %(2013-01-01; 1; 10); %(2013-01-02; 2; 20); %(2013-01-03; 3; 30)) | #(-i=:date)";

#[test]
fn record_keys_are_scalars_compared_as_values() {
    // a bare number or ISO date is a typed key, a quoted one text
    assert_eq!(one(r#"= %(1 = "a"; 2013-01-01 = 5; x = 3)"#), r#"%(1 = "a"; 2013-01-01 = 5; x = 3)"#);
    assert_eq!(one(r#"= %(1 = "a"; 2013-01-01 = 5) | keys | [1] | type"#), "int");
    assert_eq!(one(r#"= %(1 = "a"; 2013-01-01 = 5) | keys | [2] | type"#), "instant");
    assert_eq!(one(r#"= %('2013-01-01' = 5) | keys | [1] | type"#), "text");
    // 1 and "1" are two keys; records are equal item by item
    assert_eq!(one(r#"= %(1 = "a") | .a | ($.a = %("1" = "a"))"#), "false");
    assert_eq!(one(r#"= %(1 = "a") | .a | ($.a = %(1 = "a"))"#), "true");
    assert_eq!(one(r#"= %(1 = 1) ++ %("1" = 2)"#), r#"%(1 = 1; "1" = 2)"#);
    assert_eq!(one(r#"= %(1 = 1) ++! %(1 = 2)"#), "%(1 = 2)");
    // `:name` reads by the text key, `[[k]]` by any key as a value
    assert_eq!(one(r#"= %(1 = "a") | :1"#), "a");
    assert_eq!(one(r#"= %(1 = "a"; x = 2) | .r | $.r[[1]]"#), "a");
    assert_eq!(one(r#"= %(1 = "a"; x = 2) | .r | $.r[["1"]]"#), "null");
    assert_eq!(one(r#"= %(1 = "a"; 2 = "b"; 5 = "c") | .r | $.r[[1..2]]"#), r#"%(1 = "a"; 2 = "b")"#);
    // JSON writes keys as text
    assert_eq!(one(r#"= %(1 = "a") | json"#), r#"{"1": "a"}"#);
    // the constructor prints back as written
    assert_eq!(
        one(r#"= q(%(1 = "a"; price<float> = 2; 2013-01-01 = 5))"#),
        r#"q(%(1 = "a"; price<float> = 2; 2013-01-01 = 5))"#
    );
}

#[test]
fn a_declared_field_is_kept_and_the_options_declare_all() {
    assert_eq!(one(r#"= %(price<float> = "2.5"; n = 1) | :::header"#), "@(:price<float>; :n<*; int>)");
    assert_eq!(one(r#"= %(price<float> = "2.5"; n = 1) ++! %(price = "3")"#), "%(price = 3; n = 1)");
    let e = refuse(r#"= %(price<float> = "2.5"; n = 1) ++! %(price = "x")"#);
    assert!(e.contains("field 'price' is declared float, and \"x\" does not cast"), "{e}");
    assert_eq!(one(r#"= %(price<float> = 2.5; n = 1) | map(* 2) | :::header"#), "@(:price<float>; :n<*; int>)");
    assert_eq!(one(r#"= %(price<int> = 2; n = 1) | map(* 1.5; --to=:price)"#), "%(price = 3; n = 1)");
    let e = refuse(r#"= %(price<int> = 3; n = 1) | map(* 1.5; --to=:price)"#);
    assert!(e.contains("field 'price' is declared int"), "{e}");
    // --keys / --values declare every key and value
    assert_eq!(
        one(r#"= %(--keys=instant; --values=int; "2013-01-01" = "5"; "2013-01-02" = "6")"#),
        "%(2013-01-01 = 5; 2013-01-02 = 6)"
    );
    assert_eq!(one(r#"= %(--keys=instant; "2013-01-01" = 5) | keys | [1] | type"#), "instant");
    assert_eq!(one(r#"= %(--values=int; a = "5") | :::header"#), "@(:a<int>)");
    let e = refuse(r#"= %(--values=int; a = "x")"#);
    assert!(e.contains("--values=int: field 'a' holds \"x\""), "{e}");
}

#[test]
fn two_records_align_by_key_in_arithmetic() {
    assert_eq!(one("= %(a = 1; b = 2) + %(b = 10; c = 5)"), "%(a = null; b = 12; c = null)");
    assert_eq!(one("= %(1 = 1; 2 = 2) + %(2 = 10; 3 = 5)"), "%(1 = null; 2 = 12; 3 = null)");
    assert_eq!(one("= %(1 = 1; 2 = 2) + %(2 = 10; 3 = 5) | keys | [3] | type"), "int");
    assert_eq!(one("= %(a = 1; b = 2) - %(a = 1; b = 1)"), "%(a = 0; b = 1)");
}

#[test]
fn the_index_names_a_column_the_labels_of_the_rows() {
    assert_eq!(one(&format!("= {D} | :::index")), "@(2013-01-01; 2013-01-02; 2013-01-03)");
    assert_eq!(one(&format!("= {D} | header")), "@(:date<*; instant>; :A<*; int>; :B<*; int>)");
    // the column stays a column, first; a typed -i declares it
    assert_eq!(one("= #(@(a; b); %(1; \"x\")) | #(-i=:b<text>) | header"), "@(:b<text>; :a<*; int>)");
    assert_eq!(one("= @(1..4) | #(-w=2; -i=:1) | :::index"), "@(1; 3)");
    let e = refuse(&format!("= {D} | #(-i=:zip)"));
    assert!(e.contains("--index names column 'zip', and the table has no such column"), "{e}");
    // `| #` with no option drops the role; a plain table has no labels
    assert_eq!(one(&format!("= {D} | # | :::index")), "null");
    assert_eq!(one("= #(@(a); %(1)) | :::index"), "null");
    // a column is the record keyed by the labels, in the index's type
    assert_eq!(one(&format!("= {D} | :A")), "%(2013-01-01 = 1; 2013-01-02 = 2; 2013-01-03 = 3)");
    assert_eq!(one(&format!("= {D} | :A | keys | [1] | type")), "instant");
    assert_eq!(one(&format!("= {D} | .t | $.t:A | type")), "record");
    assert_eq!(one(&format!("= {D} | :A | :2013-01-02")), "2");
    let e = refuse(r#"= #(@(k; v); %("a"; 1); %("a"; 2)) | #(-i=:k) | :v"#);
    assert!(e.contains("the label \"a\" stands on several rows"), "{e}");
    // the views: `| %` a record of records, `| @` rows with the label first
    assert_eq!(
        one(&format!("= {D} | %")),
        "%(A = %(2013-01-01 = 1; 2013-01-02 = 2; 2013-01-03 = 3); B = %(2013-01-01 = 10; 2013-01-02 = 20; 2013-01-03 = 30))"
    );
    assert_eq!(one(&format!("= {D} | @ | [1]")), "%(date = 2013-01-01; A = 1; B = 10)");
    // a group's records indexed by their key
    assert_eq!(one(r#"= @(%(value = "x"; n = 2); %(value = "y"; n = 1)) | #(-i=:value) | :n"#), "%(x = 2; y = 1)");
}

#[test]
fn a_table_projector_keeps_the_index_and_may_name_another() {
    assert_eq!(one(&format!("= {D} | .t | $.t:#(:A; :B) | header")), "@(:date<*; instant>; :A<*; int>; :B<*; int>)");
    assert_eq!(one(&format!("= {D} | .t | $.t:#(:A; :B) | :::index")), "@(2013-01-01; 2013-01-02; 2013-01-03)");
    assert_eq!(one(&format!("= {D} | .t | $.t:#(:A; :B; -i=:A) | :::index")), "@(1; 2; 3)");
    assert_eq!(one(&format!("= {D} | .t | $.t:#(:A; :B; -i=:A) | header")), "@(:A<*; int>; :B<*; int>)");
    // ordering by label, the transpose exchanging index and header
    assert_eq!(one(&format!("= {D} @| sort_by(:::index; --desc) | :::index")), "@(2013-01-03; 2013-01-02; 2013-01-01)");
    assert_eq!(one(&format!("= {D} @| sort_by(:A; --desc) | :::index")), "@(2013-01-03; 2013-01-02; 2013-01-01)");
    assert_eq!(
        one(&format!("= {D} | transpose")),
        "#(@(column; \"2013-01-01\"; \"2013-01-02\"; \"2013-01-03\");\n  %(\"A\"; 1; 2; 3);\n  %(\"B\"; 10; 20; 30))"
    );
    assert_eq!(one(&format!("= {D} | transpose | :::index")), "@(\"A\"; \"B\")");
}

#[test]
fn shift_moves_the_values_and_keeps_the_labels() {
    assert_eq!(one("= @(1; 2; 3) | shift(1)"), "@(null; 1; 2)");
    assert_eq!(one("= @(1; 2; 3) | shift(-1)"), "@(2; 3; null)");
    assert_eq!(one(&format!("= {D} | :A | shift(1)")), "%(2013-01-01 = null; 2013-01-02 = 1; 2013-01-03 = 2)");
    assert_eq!(
        one(&format!("= {D} | shift(1)")),
        "#(@(date; A; B);\n  %(2013-01-01; null; null);\n  %(2013-01-02; 1; 10);\n  %(2013-01-03; 2; 20))"
    );
    // the pandas subtraction: two records aligned by key
    assert_eq!(
        one(&format!("= {D} | .t | $.t:A - ($.t:A | shift(1))")),
        "%(2013-01-01 = null; 2013-01-02 = 1; 2013-01-03 = 1)"
    );
    let e = refuse("/a | shift(1)");
    assert!(e.contains("a node has none"), "{e}");
}

#[test]
fn the_double_bracket_looks_a_label_up() {
    assert_eq!(one(&format!("= {D} | .t | $.t[[2013-01-02]]")), "%(date = 2013-01-02; A = 2; B = 20)");
    assert_eq!(one(&format!("= {D} | .t | $.t[[\"2013-01-02\"]]:A")), "2");
    assert_eq!(one(&format!("= {D} | .t | $.t:A[[2013-01-02]]")), "2");
    assert_eq!(one(&format!("= {D} | .t | $.t[[2013-01-09]]")), "null");
    assert_eq!(one(&format!("= {D} | .t | $.t[[2013-01-01; 2013-01-03]] | :::index")), "@(2013-01-01; 2013-01-03)");
    assert_eq!(one(&format!("= {D} | .t | $.t[[2013-01-02..2013-01-03]]:#(:A; :B) | :::index")), "@(2013-01-02; 2013-01-03)");
    assert_eq!(one(&format!("= {D} | .t | $.t[[@(2013-01-01; 2013-01-02)]] | dim")), "@(2; 3)");
    // a label several rows carry answers them as a table
    assert_eq!(
        one(r#"= #(@(k; v); %("a"; 1); %("a"; 2); %("b"; 3)) | #(-i=:k) | .t | $.t[["a"]] | dim"#),
        "@(2; 2)"
    );
    let e = refuse("= #(@(a); %(1)) | .t | $.t[[1]]");
    assert!(e.contains("the table has no index") && e.contains("#(-i=:col)"), "{e}");
    let e = refuse("= @(1; 2) | .l | $.l[[1]]");
    assert!(e.contains("a list is indexed by position"), "{e}");
    // the pipe form beside `| [n]`
    assert_eq!(one(&format!("= {D} | [[2013-01-02]]")), "%(date = 2013-01-02; A = 2; B = 20)");
    assert_eq!(one(&format!("= {D} | [[2013-01-02..2013-01-03]] | :::index")), "@(2013-01-02; 2013-01-03)");
    assert_eq!(one(&format!("= {D} | :A | [[2013-01-02]]")), "2");
    assert_eq!(one(r#"= %(1 = "a"; x = 2) | [["x"]]"#), "2");
    // on a record nothing coerces a label: a quoted date is text and
    // finds no instant key, where the table casts to its index's type
    assert_eq!(one(&format!("= {D} | :A | [[\"2013-01-02\"]]")), "null");
    assert_eq!(one(&format!("= {D} | [[\"2013-01-02\"]] | :A")), "2");
}

const M: &str = r#"@(%(first = "bar"; second = "one"; A = 1; B = 10); %(first = "bar"; second = "two"; A = 2; B = 20); %(first = "baz"; second = "one"; A = 3; B = 30); %(first = "baz"; second = "two"; A = 4; B = 40)) | #(-i=(:first; :second))"#;

#[test]
fn a_composite_index_nests_level_by_level() {
    assert_eq!(one(&format!("= {M} | header")), "@(:first<*; text>; :second<*; text>; :A<*; int>; :B<*; int>)");
    // a label is the record of its levels
    assert_eq!(one(&format!("= {M} | :::index | [1]")), r#"%(first = "bar"; second = "one")"#);
    // a column nests level by level, the inner record by its outer label
    assert_eq!(one(&format!("= {M} | :A")), "%(bar = %(one = 1; two = 2); baz = %(one = 3; two = 4))");
    assert_eq!(one(&format!("= {M} | :A:bar")), "%(one = 1; two = 2)");
    assert_eq!(
        one(&format!("= {M} | %")),
        "%(A = %(bar = %(one = 1; two = 2); baz = %(one = 3; two = 4)); B = %(bar = %(one = 10; two = 20); baz = %(one = 30; two = 40)))"
    );
    // the lookup goes level by level: one row, the sub-table under
    // the outer label indexed by what remains, several outer labels
    assert_eq!(one(&format!("= {M} | .t | $.t[[\"bar\"; \"one\"]]")), r#"%(first = "bar"; second = "one"; A = 1; B = 10)"#);
    assert_eq!(one(&format!("= {M} | .t | $.t[[\"bar\"]] | :::index")), r#"@("one"; "two")"#);
    assert_eq!(one(&format!("= {M} | .t | $.t[[\"bar\"]] | header")), "@(:second<*; text>; :A<*; int>; :B<*; int>)");
    assert_eq!(one(&format!("= {M} | .t | $.t[[@(\"bar\"; \"baz\")]] | dim")), "@(4; 4)");
    let e = refuse(&format!("= {M} | .t | $.t[[\"a\"; \"b\"; \"c\"]]"));
    assert!(e.contains("the index has 2 levels (first, second), and 3 labels were given"), "{e}");
    // a level alone is a column; the sort by label goes level by level
    assert_eq!(one(&format!("= {M} | [:second = \"one\"] | :A")), "%(bar = %(one = 1); baz = %(one = 3))");
    assert_eq!(one(&format!("= {M} @| sort_by(:::index; --desc) | :::index | [1]")), r#"%(first = "baz"; second = "two")"#);
    // the option in another order re-nests
    assert_eq!(one(&format!("= {M} | .t | $.t:#(:A; -i=(:second; :first)) | :A")), "%(one = %(bar = 1; baz = 3); two = %(bar = 2; baz = 4))");
    assert_eq!(one(&format!("= {M} | .t | $.t:#(:A; -i=(:second; :first)) | header")), "@(:second<*; text>; :first<*; text>; :A<*; int>)");
    let e = refuse("= #(@(a; b); %(1; 2)) | #(-i=(:a; :a))");
    assert!(e.contains("--index names column 'a' twice"), "{e}");
}

#[test]
fn the_labels_are_mapped_under_to_index_and_a_list_makes_a_tuple_column() {
    let m = r#"@(%(first = "bar"; second = "one"; A = 1); %(first = "bar"; second = "two"; A = 2); %(first = "baz"; second = "one"; A = 3)) | #(-i=(:first; :second))"#;
    // a scalar body keeps one level: renamed, or declared under a trait
    assert_eq!(one(&format!("= {D} | map(strftime(\"%Y\"); --to=:::index) | :::index")), "@(2013; 2013; 2013)");
    assert_eq!(one(&format!("= {D} | map(<text>; --to=:::index) | header")), "@(:date<text>; :A<*; int>; :B<*; int>)");
    // a record body (a scalar command broadcast over the label record,
    // draft 166) re-nests the levels; a list body makes the tuple column
    assert_eq!(one(&format!("= {m} | map(lower; --to=:::index) | header")), "@(:first<*; text>; :second<*; text>; :A<*; int>)");
    assert_eq!(one(&format!("= {m} | map(flatten; --to=:::index) | header")), "@(:first_second<(text; text)>; :A<*; int>)");
    // a list body: one tuple column named by the levels joined
    assert_eq!(
        one(&format!("= {m} | map(flatten; --to=:::index)")),
        "#(@(first_second; A);\n  %(@(\"bar\"; \"one\"); 1);\n  %(@(\"bar\"; \"two\"); 2);\n  %(@(\"baz\"; \"one\"); 3))"
    );
    assert_eq!(one(&format!("= {m} | map(flatten; --to=:::index) | header")), "@(:first_second<(text; text)>; :A<*; int>)");
    assert_eq!(one(&format!("= {m} | map(flatten; --to=:::index) | type")), r#"@("(text; text)"; "int")"#);
    assert_eq!(one(&format!("= {m} | map(flatten; --to=:::index) | .t | $.t[[@(\"bar\"; \"two\")]] | :A")), "2");
    // a record body nests again
    assert_eq!(
        one(&format!("= {m} | map(flatten; --to=:::index) | map(%(first = $_[1]; second = $_[2]); --to=:::index) | header")),
        "@(:first<*; text>; :second<*; text>; :A<*; int>)"
    );
    // the tuple type declared on a header, cast item by item, never inferred
    assert_eq!(one(r#"= @(@(1; "x"); @(2; "y")) | #(--header=@(:key<(int; text)>)) | header"#), "@(:key<(int; text)>)");
    let e = refuse(r#"= @(@(1; "x"); @(2)) | #(--header=@(:key<(int; text)>))"#);
    assert!(e.contains("column 'key' is declared (int; text), and row 2 holds @(2)"), "{e}");
    let e = refuse(r#"= #(@(key); %(@(1; "x")))"#);
    assert!(e.contains("a table is flat") || e.contains("holds a list") || e.contains("list"), "{e}");
    let e = refuse(&format!("= {D} | map(<int>; --to=:::index) | :::index"));
    assert!(e.contains("column 'date' is declared int"), "{e}");
}

#[test]
fn header_levels_stack_unstack_and_transpose() {
    let m = r#"@(%(first = "bar"; second = "one"; A = 1; B = 10); %(first = "bar"; second = "two"; A = 2; B = 20); %(first = "baz"; second = "one"; A = 3; B = 30); %(first = "baz"; second = "two"; A = 4; B = 40)) | #(-i=(:first; :second))"#;
    // stack: the header's innermost level into the index; unstack back
    assert_eq!(
        one(&format!("= {D} | stack")),
        "#(@(date; variable; value);\n  %(2013-01-01; \"A\"; 1);\n  %(2013-01-01; \"B\"; 10);\n  %(2013-01-02; \"A\"; 2);\n  %(2013-01-02; \"B\"; 20);\n  %(2013-01-03; \"A\"; 3);\n  %(2013-01-03; \"B\"; 30))"
    );
    assert_eq!(one(&format!("= {D} | stack | :::index | [1]")), r#"%(date = 2013-01-01; variable = "A")"#);
    assert_eq!(one(&format!("= {D} | stack | unstack | header")), "@(:date<*; instant>; :A<*; int>; :B<*; int>)");
    let e = refuse("= #(@(a); %(1)) | stack");
    assert!(e.contains("the table has no index"), "{e}");
    let e = refuse("= #(@(a); %(1)) | unstack");
    assert!(e.contains("no index level to move"), "{e}");
    // unstack of a composite index: the inner level's values become
    // header paths under each data column; the views nest by level
    assert_eq!(one(&format!("= {m} | unstack | header")), "@(:first<*; text>; :A:one<*; int>; :A:two<*; int>; :B:one<*; int>; :B:two<*; int>)");
    assert_eq!(one(&format!("= {m} | unstack | :A")), "#(@(first; one; two);\n  %(\"bar\"; 1; 2);\n  %(\"baz\"; 3; 4))");
    assert_eq!(one(&format!("= {m} | unstack | :A:one")), "%(bar = 1; baz = 3)");
    assert_eq!(
        one(&format!("= {m} | unstack | %")),
        "%(A = %(one = %(bar = 1; baz = 3); two = %(bar = 2; baz = 4)); B = %(one = %(bar = 10; baz = 30); two = %(bar = 20; baz = 40)))"
    );
    assert_eq!(one(&format!("= {m} | unstack | @ | [1]")), r#"%(first = "bar"; A = %(one = 1; two = 2); B = %(one = 10; two = 20))"#);
    // transpose carries the levels across
    assert_eq!(
        one(&format!("= {m} | transpose")),
        "#(@(column; \"bar:one\"; \"bar:two\"; \"baz:one\"; \"baz:two\");\n  %(\"A\"; 1; 2; 3; 4);\n  %(\"B\"; 10; 20; 30; 40))"
    );
    assert_eq!(one(&format!("= {m} | transpose | :::index")), r#"@("A"; "B")"#);
    assert_eq!(one(&format!("= {m} | unstack | transpose | :::index | [1]")), r#"%(column = "A"; column2 = "one")"#);
}

#[test]
fn melt_and_pivot_keep_the_index() {
    assert_eq!(
        one(&format!("= {D} | melt | header")),
        "@(:date<*; instant>; :variable<*; text>; :value<*; int>)"
    );
    assert_eq!(one(&format!("= {D} | melt | :::index | [1]")), r#"%(date = 2013-01-01; variable = "A")"#);
    assert_eq!(one(&format!("= {D} | melt | [[2013-01-02; \"B\"]] | :value")), "20");
    let l = r#"@(%(first = "bar"; second = "one"; C = "x"; D = 1); %(first = "bar"; second = "two"; C = "x"; D = 2); %(first = "baz"; second = "one"; C = "y"; D = 3))"#;
    assert_eq!(
        one(&format!("= {l} | # | pivot((:first; :second); C; D)")),
        "#(@(first; second; x; y);\n  %(\"bar\"; \"one\"; 1; null);\n  %(\"bar\"; \"two\"; 2; null);\n  %(\"baz\"; \"one\"; null; 3))"
    );
    assert_eq!(one(&format!("= {l} | # | pivot((:first; :second); C; D) | [[\"bar\"]] | :x")), "%(one = 1; two = 2)");
    assert_eq!(one(&format!("= {l} | pivot((:first; :second); C; D) | [1]")), r#"%(first = "bar"; second = "one"; x = 1; y = null)"#);
    // flatten opens a record to its values
    assert_eq!(one("= %(a = 1; b = 2) | flatten"), "@(1; 2)");
}

#[test]
fn describe_summarizes_the_numeric_columns() {
    let t = r#"= #(@(date; A; B; n); %(2013-01-01; 1; 10.5; "x"); %(2013-01-02; 2; 20.25; "y"); %(2013-01-03; 3; 30; "z")) | #(-i=:date)"#;
    assert_eq!(
        one(&format!("{t} | describe")),
        "#(@(statistic; A; B);\n  %(\"count\"; 3; 3);\n  %(\"mean\"; 2; 20.25);\n  %(\"std\"; 1; 9.75);\n  %(\"min\"; 1; 10.5);\n  %(\"25%\"; 1.5; 15.375);\n  %(\"50%\"; 2; 20.25);\n  %(\"75%\"; 2.5; 25.125);\n  %(\"max\"; 3; 30))"
    );
    assert_eq!(one(&format!("{t} | describe | :::index | [2]")), "mean");
    assert_eq!(one(&format!("{t} | describe | :A:std")), "1");
    // no numeric column: the empty table; a list and a record of
    // numbers: the record of the statistics; over the context too
    assert_eq!(one(r#"= #(@(n); %("x")) | describe"#), "#(@(statistic))");
    assert_eq!(one("= @(1; 2; 3; null) | describe"), r#"%(count = 3; mean = 2; std = 1; min = 1; "25%" = 1.5; "50%" = 2; "75%" = 2.5; max = 3)"#);
    assert_eq!(one("= %(a = 1; b = 3) | describe | :mean"), "2");
    assert_eq!(one("= 1..4 @| describe | :count"), "4");
}

#[test]
fn pretty_draws_floats_to_six_digits_and_aligns_numbers() {
    // the digits are the drawing's alone: the value, json and csv
    // keep them all
    assert_eq!(one("= @(1; 2.123456789; -25.19880001; 506.0) | pretty"), "@(1; 2.123457; -25.1988; 506)");
    assert_eq!(one("= @(1; 2.123456789) | pretty(-p=2)"), "@(1; 2.12)");
    assert_eq!(one("= @(2.5) | pretty(-p=0)"), "@(2)");
    assert_eq!(one("= %(v = 1.0 div 3) | pretty"), "%(v = 0.333333)");
    assert_eq!(one("= 1.0 div 3 | json"), "0.3333333333333333");
    assert_eq!(one("= 1.0 div 3"), "0.3333333333333333");
    // the grid: a numeric column right-aligned on its decimal point,
    // a text column left-aligned
    assert_eq!(
        one(r#"= #(@(name; x); %("a"; 1.5); %("bb"; 123.25); %("c"; 7)) | pretty"#),
        "name       x\n----  ------\na       1.5\nbb    123.25\nc       7"
    );
    assert_eq!(
        one(r#"= #(@(name; x); %("a"; 1.5); %("bb"; 123.25)) @| pretty"#),
        "name       x\n----  ------\na       1.5\nbb    123.25"
    );
}

#[test]
fn a_literal_of_any_kind_and_a_conditional_stand_after_a_pipe() {
    // a bare ISO date or instant is the instant literal
    assert_eq!(one("= 2013-01-01 | type"), "instant");
    assert_eq!(one("= 1 | 2013-01-01 + 1_d"), "2013-01-02");
    // the conditional stands bare after a pipe and at the head
    assert_eq!(one("= 1 | (1 = 1) ? 1 : 0"), "1");
    assert_eq!(one(r#"= 1 = 1 ? "yes" : "no""#), "yes");
    assert_eq!(one("= 1 | $_ > 0 ? $_ : null"), "1");
    // a reducer over an indexed table reduces the value columns and
    // leaves the index out; a quarq over a table projector too
    assert_eq!(
        one("= @(%(d = 1; a = 1; b = 2); %(d = 2; a = 3; b = 4)) | # | #(-i=:d) | mean"),
        "%(a = 2; b = 3)"
    );
    assert_eq!(
        one("= @(%(d = 1; a = 1; b = 2); %(d = 2; a = 3; b = 4)) | # | #(-i=:d) | .t | q(sum | $_ * 2) | .f | $.t:#(:a) | &$.f"),
        "%(a = 8)"
    );
    // sort_by(:::index) orders by the labels level by level; without
    // an index it refuses, naming the switch
    assert_eq!(
        one("= @(%(a = \"b\"; b = 2; v = 1); %(a = \"a\"; b = 2; v = 2); %(a = \"a\"; b = 1; v = 3)) | # | #(-i=(:a; :b)) @| sort_by(:::index) | :::index"),
        "@%(@(a; b); %(\"a\"; 1); %(\"a\"; 2); %(\"b\"; 2))"
    );
    let e = refuse("= @(%(a = 1; v = 2)) | # @| sort_by(:::index)");
    assert!(e.contains("has no index") && e.contains("#(-i=:col)"), "{e}");
    // truncate(d): the instant floored to a multiple of a fixed-length
    // duration from the epoch, in UTC; a calendar period refuses
    // pointing at period(GRAIN); null where nothing temporal reads
    assert_eq!(one("= 2013-01-01T10:07:23 | truncate(5_min)"), "2013-01-01T10:05:00");
    assert_eq!(one("= 2013-01-01T10:07:23 | truncate(15_s)"), "2013-01-01T10:07:15");
    assert_eq!(one("= 2013-01-01T10:07:23 | truncate(6_h)"), "2013-01-01T06:00:00");
    assert_eq!(one("= 2013-01-01T10:07:23 | truncate(1_w)"), "2012-12-27");
    assert_eq!(one("= 2013-01-01T10:07:23+02:00 | truncate(1_h)"), "2013-01-01T08:00:00");
    assert_eq!(one("= 1969-12-31T23:59:00 | truncate(1_h)"), "1969-12-31T23:00:00");
    assert_eq!(one(r#"= "x" | truncate(5_min)"#), "null");
    let e = refuse("= 2013-01-01 | truncate(1_mo)");
    assert!(e.contains("calendar period") && e.contains("period(\"month\")"), "{e}");
    let e = refuse("= 2013-01-01 | truncate(3)");
    assert!(e.contains("seconds to weeks"), "{e}");
    // a text-to-instant reader passes an instant through unchanged
    assert_eq!(one(r#"= 1881-12-25 | tp("%Y-%m-%d") | ($_ + 12_d)"#), "1882-01-06");
    // the head still refuses a union
    let e = refuse("= 2 || /a");
    assert!(e.contains("an expression head stands alone"), "{e}");
}

#[test]
fn nan_and_the_infinities_are_float_literals() {
    assert_eq!(one("= @(1; inf; -inf; nan)"), "@(1; inf; -inf; nan)");
    assert_eq!(one("= nan | type"), "float");
    assert_eq!(one("= -inf | type"), "float");
    assert_eq!(one("= @(1; nan) | mean"), "nan");
    assert_eq!(one("= nan | json"), "null");
    assert_eq!(one("= (nan = nan)"), "false");
}
