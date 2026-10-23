//! The two readings of a table and the reshapes (spec: Command
//! Libraries, "A library value"; the reshapes): `| @` the row
//! reading, `| %` the column reading, `transpose`, `pivot`, `melt`,
//! on plain values and in both pipes. No library is needed: these
//! are stdlib commands.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"rows":[
            {"book":"a","x":1,"y":2},
            {"book":"b","x":3,"y":4},
            {"book":"c","x":5,"y":6}],
          "long":[
            {"book":"a","measure":"mean","v":1},
            {"book":"a","measure":"semi","v":2},
            {"book":"b","measure":"mean","v":3},
            {"book":"b","measure":"semi","v":4}],
          "m":[[1,2,3],[4,5,6]]}"#,
    )
    .unwrap()
}

fn run(q: &str) -> quarb::Result<Vec<String>> {
    let run = quarb::Run::ambient();
    let out = quarb::Query::parse(q, &doc())?.run(&doc(), &run)?;
    Ok(match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::display_form).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    })
}

fn one(q: &str) -> String {
    let v = run(q).unwrap();
    assert_eq!(v.len(), 1, "{q}: {v:?}");
    v.into_iter().next().unwrap()
}

fn refuse(q: &str) -> String {
    run(q).unwrap_err().to_string()
}

#[test]
fn the_column_reading_of_records_and_the_row_reading_back() {
    // `@| %` — the stream of records as one record of columns.
    assert_eq!(
        one(r#"/rows/* | %(::book; ::x; ::y) @| %"#),
        r#"%(book = @("a"; "b"; "c"); x = @(1; 3; 5); y = @(2; 4; 6))"#
    );
    // `| @` — the record of lists back as records, one per position;
    // after `|` one list value, after `@|` the stream of them.
    assert_eq!(
        run(r#"/rows/* | %(::book; ::x; ::y) @| % | @"#).unwrap(),
        vec![r#"@%(@(book; x; y); %("a"; 1; 2); %("b"; 3; 4); %("c"; 5; 6))"#]
    );
    assert_eq!(
        run(r#"/rows/* | %(::book; ::x; ::y) @| % @| @"#).unwrap(),
        vec![
            r#"%(book = "a"; x = 1; y = 2)"#,
            r#"%(book = "b"; x = 3; y = 4)"#,
            r#"%(book = "c"; x = 5; y = 6)"#
        ]
    );
    // A field missing from one record is null in its column.
    assert_eq!(
        one(r#"/rows/*[::book != "c"] | (::book = "a" ? %(a = ::x; b = ::y) : %(a = ::x)) @| %"#),
        r#"%(a = @(1; 3); b = @(2; null))"#
    );
    // A list of lists has no names.
    let e = run(r#"/rows/* | @(::x; ::y) @| %"#)
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("no field names") && e.contains("transpose"),
        "{e}"
    );
}

#[test]
fn transpose_exchanges_rows_and_columns() {
    // After `@|`, the stream of lists is the matrix: the result is
    // an array of one type, and a second transpose restores it.
    assert_eq!(
        run(r#"/rows/* | @(::x; ::y) @| transpose"#).unwrap(),
        vec!["#(@(1; 3; 5);\n  @(2; 4; 6))"]
    );
    assert_eq!(
        run(r#"/rows/* | @(::x; ::y) @| transpose @| transpose"#).unwrap(),
        vec!["#(@(1; 2);\n  @(3; 4);\n  @(5; 6))"]
    );
    let e = run(r#"/rows/* | (::book = "a" ? @(::x) : @(::x; ::y)) @| transpose"#)
        .unwrap_err()
        .to_string();
    assert!(e.contains("row 1 holds 1 values, row 2 holds 2"), "{e}");
}

#[test]
fn pivot_goes_long_to_wide_and_melt_back() {
    // over a stream of records as over a table: ONE table indexed by
    // the row key (draft 166)
    assert_eq!(
        one(r#"/long/* | %(::book; ::measure; ::v) @| pivot(book; measure; v)"#),
        "#(@(book; mean; semi);\n  %(\"a\"; 1; 2);\n  %(\"b\"; 3; 4))"
    );
    // Without operands, the first three fields serve by position.
    assert_eq!(one(r#"/long/* | %(::book; ::measure; ::v) @| pivot | :::index"#), r#"@("a"; "b")"#);
    // A cell no record fills is null.
    assert_eq!(
        one(r#"/long/*[::v != 4] | %(::book; ::measure; ::v) @| pivot | [2]"#),
        r#"%(book = "b"; mean = 3; semi = null)"#
    );
    // Two values on one cell refuse.
    let e = quarb::Query::parse(
        r#"/long/* | %(::book; measure = "m"; ::v) @| pivot"#,
        &doc(),
    )
    .unwrap()
    .run(&doc(), &quarb::Run::ambient())
    .unwrap_err()
    .to_string();
    assert!(e.contains("two values for row 'a', column 'm'"), "{e}");
    // a predicate on the table value filters its rows, each tested
    // as the cursor record; the rows that hold answer as a table
    assert_eq!(one("/rows/* @| # | [:x > 2] | dim"), "@(2; 3)");
    assert_eq!(one("/rows/* @| # | [:x > 2] | :book"), r#"@("b"; "c")"#);
    assert_eq!(one("/rows/* @| # | [:x > 200] | dim"), "@(0; 3)");
    assert_eq!(one(r#"/rows/* @| # | [:book = "a" || :y = 6] | :x"#), "@(1; 5)");
    assert_eq!(one("= #(%(1; 2); %(3; 4)) | [:1 > 2] | @"), "@(3; 4)");
    // melt on the table value: the long table itself, the ids
    // then `variable` and `value`
    assert_eq!(
        one("/rows/* @| # | melt(book)"),
        "#(@(book; variable; value);\n  %(\"a\"; \"x\"; 1);\n  %(\"a\"; \"y\"; 2);\n  %(\"b\"; \"x\"; 3);\n  %(\"b\"; \"y\"; 4);\n  %(\"c\"; \"x\"; 5);\n  %(\"c\"; \"y\"; 6))"
    );
    // melt: one record per non-id field.
    assert_eq!(
        run(r#"/rows/* | %(::book; ::x; ::y) @| melt(book)"#).unwrap(),
        vec![
            r#"%(book = "a"; variable = "x"; value = 1)"#,
            r#"%(book = "a"; variable = "y"; value = 2)"#,
            r#"%(book = "b"; variable = "x"; value = 3)"#,
            r#"%(book = "b"; variable = "y"; value = 4)"#,
            r#"%(book = "c"; variable = "x"; value = 5)"#,
            r#"%(book = "c"; variable = "y"; value = 6)"#
        ]
    );
    assert_eq!(
        run(r#"/rows/*[::book = "a"] | %(::book; ::x) @| melt(book; --name=measure --value=v)"#)
            .unwrap(),
        vec![r#"%(book = "a"; measure = "x"; v = 1)"#]
    );
    // Round trip: the pivot's rows are the records melted.
    assert_eq!(
        one(r#"/rows/* | %(::book; ::x; ::y) @| melt(book) @| pivot | @"#),
        one(r#"/rows/* | %(::book; ::x; ::y) @| @"#)
    );
}

/// The table kind (spec: The Table Kind): `#(...)` with a header
/// and record rows is a frame, with list rows an array; the laws of
/// the literal; the two readings and the column accessor on it;
/// `| #` the table reading of a stream; the exports by rows.
#[test]
fn the_table_literal_its_laws_and_its_readings() {
    // the two faces print as their literal
    assert_eq!(one("= #(%(1; 2); %(3; 4))"), "#(@(1; 2);\n  @(3; 4))");
    assert_eq!(one("= #(@(1; 2); @(3; 4))"), "#(@(1; 2);\n  @(3; 4))");
    let frame = "#(@(name; age);\n  %(\"Braund\"; 22);\n  %(\"Allen\"; 35))";
    assert_eq!(
        one(r#"= #(@(name; age); %("Braund"; 22); %("Allen"; 35))"#),
        frame
    );
    // the header inferred from full records; a named item by its
    // column, a short row filled with holes
    assert_eq!(
        one(r#"= #(%(name = "Braund"; age = 22); %(name = "Allen"; age = 35))"#),
        frame
    );
    assert_eq!(
        one(r#"= #(@(name; age); %(age = 22; name = "Braund"); %("Allen"))"#),
        "#(@(name; age);\n  %(\"Braund\"; 22);\n  %(\"Allen\"; null))"
    );
    // the refusals: a header wants record rows, rows do not mix,
    // positional rows name no columns, a long row, a stray name
    let refuse = |q: &str| run(q).unwrap_err().to_string();
    // two lists are a matrix (draft 166), here of text: the header of a
    // frame is the list standing before RECORD rows
    assert_eq!(one(r#"= #(@(name; age); @("Braund"; 22)) | dim"#), "@(2; 2)");
    // a number in the header names its column by its text
    assert_eq!(one("= #(@(1; 2); %(3; 4)) | header"), "@(:1<*; int>; :2<*; int>)");
    assert!(refuse("= #(@(1; 2); %(a = 1))").contains("'a' is not a column"));
    // positional rows without a header are the array face
    assert_eq!(one(r#"= #(%("Braund"; 22))"#), r#"#(@("Braund"; 22))"#);
    // (the header by position on the array face)
    assert_eq!(one(r#"= #(%("Braund"; 22)) | header"#), "@(:1<*; text>; :2<*; int>)");
    assert!(refuse(r#"= #(%(1; 2); %(a = 3))"#).contains("names no column of an array"));
    assert!(refuse(r#"= #(@(name; age); %("Braund"; 22; 3))"#).contains("row 1 has 3 values"));
    assert!(
        refuse(r#"= #(@(name; age); %("Braund"; nope = 3))"#).contains("'nope' is not a column")
    );
    // the readings and the accessors
    let t = r#"= #(@(name; age); %("Braund"; 22); %("Allen"; 35))"#;
    assert_eq!(
        run(&format!("{t} @| @")).unwrap(),
        [
            r#"%(name = "Braund"; age = 22)"#,
            r#"%(name = "Allen"; age = 35)"#
        ]
    );
    assert_eq!(
        one(&format!("{t} | %")),
        r#"%(name = @("Braund"; "Allen"); age = @(22; 35))"#
    );
    assert_eq!(one(&format!("{t} | :age")), "@(22; 35)");
    assert_eq!(one(&format!("{t} | ($_ | dim)")), "@(2; 2)");
    assert_eq!(
        one("= #(%(1; 2); %(3; 4)) | transpose"),
        "#(@(1; 3);\n  @(2; 4))"
    );
    // a frame transposes to a frame: the names as the column
    // `column`, the old rows as columns by position (a mixed column
    // takes the text form)
    assert_eq!(
        one(&format!("{t} | transpose")),
        "#(@(column; \"1\"; \"2\");\n  %(\"name\"; \"Braund\"; \"Allen\");\n  %(\"age\"; \"22\"; \"35\"))"
    );
    // a scalar command broadcasts over the cells (draft 166)
    assert_eq!(one(&format!("{t} | upper")), "#(@(name; age);\n  %(\"BRAUND\"; 22);\n  %(\"ALLEN\"; 35))");
    // `| #` — the table reading of a stream: records a frame (the
    // names in order of first appearance, a hole where one lacks),
    // lists an array (a short row padded, a long one refused),
    // values a sample
    assert_eq!(
        one("/rows/* | %(::book; ::x; ::y) @| #"),
        "#(@(book; x; y);\n  %(\"a\"; 1; 2);\n  %(\"b\"; 3; 4);\n  %(\"c\"; 5; 6))"
    );
    assert_eq!(one("/rows/* | %(::book; ::x; ::y) @| # | :x"), "@(1; 3; 5)");
    assert_eq!(
        one("= @(@(1; 2; 3); @(4; 5; 6)) | #"),
        "#(@(1; 2; 3);\n  @(4; 5; 6))"
    );
    assert_eq!(one("= @(@(1; 2); @(3)) | #"), "#(@(1; 2);\n  @(3; null))");
    assert!(refuse("= @(@(1); @(3; 4)) | #").contains("row 2 has 2 values"));
    assert_eq!(one("/rows/*::x @| #"), "#(@(1);\n  @(3);\n  @(5))");
    assert_eq!(
        one(r#"= @(%(a = 1); %(b = 2)) | #"#),
        "#(@(a; b);\n  %(1; null);\n  %(null; 2))"
    );
    // the exports read the rows
    assert_eq!(
        run(&format!("{t} @| csv")).unwrap().join("\n"),
        "name,age\nBraund,22\nAllen,35"
    );
    // the ruled access: a row by position, a slice as a table,
    // `rows`/`columns` as the sigils, `header`, `dim`, the array
    // face's columns keyed by position and read with `:2`
    assert_eq!(one(&format!("{t} | [2]")), r#"%(name = "Allen"; age = 35)"#);
    assert_eq!(
        one(&format!("{t} | [-1]")),
        r#"%(name = "Allen"; age = 35)"#
    );
    assert_eq!(
        one(&format!("{t} | [..1]")),
        "#(@(name; age);\n  %(\"Braund\"; 22))"
    );
    assert_eq!(one(&format!("{t} | header")), "@(:name<*; text>; :age<*; int>)");
    assert_eq!(one(&format!("{t} | keys")), r#"@("name"; "age")"#);
    assert_eq!(one(&format!("{t} | dim")), "@(2; 2)");
    assert_eq!(one("= #(%(1; 2); %(3; 4)) | header"), "@(:1<*; int>; :2<*; int>)");
    assert_eq!(one("= #(%(1; 2); %(3; 4)) | rows"), "@(@(1; 2); @(3; 4))");
    assert_eq!(
        one("= #(%(1; 2); %(3; 4)) | columns"),
        r#"%("1" = @(1; 3); "2" = @(2; 4))"#
    );
    assert_eq!(one("= #(%(1; 2); %(3; 4)) | % | :2"), "@(2; 4)");
    // the positions are the array's column names for `:2` too
    assert_eq!(one("= #(%(1; 2); %(3; 4)) | :2"), "@(2; 4)");
    assert_eq!(one("= #(%(1; 2); %(3; 4)) | :3"), "null");
    assert_eq!(one("= #(%(1; 2); %(3; 4)) | [1]"), "@(1; 2)");
    assert!(refuse("= 5 | [2]").contains("whole-context"));
    // over nodes, each contributes the record of its properties
    assert_eq!(one("/rows/* @| # | header"), "@(:book<*; text>; :x<*; int>; :y<*; int>)");
    assert_eq!(one("/rows/* @| # | dim"), "@(3; 3)");
    assert_eq!(one("/rows/* @| # | [2]"), r#"%(book = "b"; x = 3; y = 4)"#);
    // on a list value, `| [n]` is the item and `| [a..b]` the sublist
    assert_eq!(one("= #(%(1; 2); %(3; 4)) | [2] | [1]"), "3");
    assert_eq!(one("= @(5; 6; 7) | [-1]"), "7");
    assert_eq!(one("= @(5; 6; 7) | [2..]"), "@(6; 7)");
    assert_eq!(one("= @(5; 6; 7) | [..2]"), "@(5; 6)");
    // value indexing: brackets glued to a value expression
    let t3 = r#"#(@(name; age); %("Braund"; 22); %("Allen"; 35); %("Heikkinen"; 26))"#;
    assert_eq!(one("= @(1; 2; 3)[2]"), "2");
    assert_eq!(one("= @(1; 2; 3)[-1]"), "3");
    assert_eq!(one("= @(1; 2; 3)[2..]"), "@(2; 3)");
    assert_eq!(one("= @(1; 2; 3)[..2]"), "@(1; 2)");
    assert_eq!(one(&format!("= {t3}[2]")), r#"%(name = "Allen"; age = 35)"#);
    assert_eq!(
        one(&format!("= {t3}[2..3]")),
        "#(@(name; age);\n  %(\"Allen\"; 35);\n  %(\"Heikkinen\"; 26))"
    );
    // the cell three ways, and the ranges in either position
    assert_eq!(one(&format!("= {t3}[2; 1]")), "Allen");
    assert_eq!(one(&format!("= {t3}[2; \"age\"]")), "35");
    assert_eq!(one(&format!("= {t3}[2]:age")), "35");
    assert_eq!(one(&format!("= {t3}[2][2]")), "35");
    assert_eq!(one(&format!("= {t3}[1..2; \"age\"]")), "@(22; 35)");
    assert_eq!(
        one(&format!("= {t3}[2; 1..2]")),
        r#"%(name = "Allen"; age = 35)"#
    );
    assert_eq!(
        one(&format!("= {t3}[1..2; 1..2]")),
        "#(@(name; age);\n  %(\"Braund\"; 22);\n  %(\"Allen\"; 35))"
    );
    assert_eq!(
        one("= #(%(1; 2); %(3; 4))[2; 1..2]"),
        r#"%("1" = 3; "2" = 4)"#
    );
    assert_eq!(one("= @(1; 2) | %(a = $_[2])"), "%(a = 2)");
    assert_eq!(one(&format!("= {t3} | cell(3; \"name\")")), "Heikkinen");
    assert!(refuse(r#"= "abc"[1]"#).contains("a string is not indexed"));
    // a condition in a value's brackets reads the items (draft 166):
    // none of these numbers has a property, so none holds
    assert_eq!(one("= @(1; 2)[::x = 1]"), "@()");
    assert!(refuse(&format!("= {t3}[2; \"nope\"]")).contains("no column 'nope'"));
    // the header lifted and lowered; the lowered names make their
    // columns text, so the ages take their text form
    assert_eq!(
        one(&format!("= {t3}[..2] | header-to-row")),
        "#(@(\"name\"; \"age\");\n  @(\"Braund\"; \"22\");\n  @(\"Allen\"; \"35\"))"
    );
    assert_eq!(
        one(&format!("= {t3} | header-to-row | row-to-header | keys")),
        r#"@("name"; "age")"#
    );
    // one record is one row; a record of lists the columns
    assert_eq!(
        one(r#"/rows/*[::book = "a"] | %(::book; ::x) @| #"#),
        "#(@(book; x);\n  %(\"a\"; 1))"
    );
    assert_eq!(one("= %(a = @(1; 2); b = @(3; 4)) @| # | dim"), "@(2; 2)");
    // the renderer is `pretty`: the grid after a table value or over
    // a stream, a record one field per line, a list one item per
    // line; `| csv` after a table value serializes it; `table` stays
    // an alias through 0.37
    // (a numeric column right-aligned on its decimal point, draft 166)
    let grid = "name    age\n------  ---\nBraund   22\nAllen    35";
    assert_eq!(one(&format!("{t} | pretty")), grid);
    assert_eq!(one(&format!("{t} @| pretty")), grid);
    assert_eq!(one(&format!("{t} @| pretty")), grid);
    assert_eq!(one(&format!("{t} | csv")), "name,age\nBraund,22\nAllen,35");
    // the column list selects and orders on either pipe
    assert_eq!(one(&format!("{t} | pretty(age)")), "age\n---\n 22\n 35");
    assert_eq!(
        one(&format!("{t} | csv(age; name)")),
        "age,name\n22,Braund\n35,Allen"
    );
    assert_eq!(
        one(&format!("{t} @| csv(age; name)")),
        "age,name\n22,Braund\n35,Allen"
    );
    assert_eq!(
        one(r#"= %(a = 1; b = "x") | pretty"#),
        "%(a = 1;\n  b = \"x\")"
    );
    // a list stays on one line when it fits and holds no record;
    // a record opens at every depth, two spaces per level
    assert_eq!(one("= @(1; 2) | pretty"), "@(1; 2)");
    assert_eq!(
        one(r#"= %(a = 1; b = @(1; 2; 3); c = %(d = "x"; e = null)) | pretty"#),
        "%(a = 1;\n  b = @(1; 2; 3);\n  c = %(d = \"x\";\n        e = null))"
    );
    assert_eq!(
        one("= @(%(a = 1; n = %(k = 2)); %(a = 2)) | pretty"),
        "@(%(a = 1;\n    n = %(k = 2));\n  %(a = 2))"
    );
    assert!(one("= %(long = @(1..40)) | pretty").starts_with("%(long = @(1;\n           2;"));
    assert_eq!(
        one("= #(%(1; 2); %(3; 4)) | pretty"),
        "1  2\n-  -\n1  2\n3  4"
    );
    assert!(refuse("= %(a = 1) | csv").contains("renders a table value"));
    // a list constructor names the ids of a melt
    assert_eq!(
        run("/rows/* | %(::book; ::x; ::y) @| melt(@(book; x)) @| [1]").unwrap(),
        [r#"%(book = "a"; x = 1; variable = "y"; value = 2)"#]
    );
    // the result line shows the header and each row on its own line
    assert_eq!(
        one(t),
        "#(@(name; age);\n  %(\"Braund\"; 22);\n  %(\"Allen\"; 35))"
    );
}

/// The table is flat: a cell holds a scalar or null, never a
/// structure. The literal, the table reading of records and of
/// lists refuse, naming the cell; the records keep their shape.
#[test]
fn a_table_is_flat() {
    let e = run(r#"= #(@(a; b); %(1; @(2; 3)))"#)
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("row 1, column 2") && e.contains("a table is flat") && e.contains("list"),
        "{e}"
    );
    let e = run(r#"= @(%(a = 1; b = @(2; 3))) | #"#)
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("a table is flat")
            && e.contains("'b' holds a list in row 1")
            && e.contains("keep their shape"),
        "{e}"
    );
    let e = run(r#"= @(@(1; %(k = 2))) | #"#).unwrap_err().to_string();
    assert!(e.contains("row 1, column 2 holds a record"), "{e}");
}

/// The record array `@%(...)` (spec: List Construction): sugar over
/// a list of records in the table literal's shape, and the form two
/// or more records of one shape print in.
#[test]
fn the_record_array_is_a_list_of_records() {
    // the constructor: header, then rows positional or named
    assert_eq!(
        one(r#"= @%(@(a; b); %(1; 2); %(b = 4; a = 3))"#),
        r#"@%(@(a; b); %(1; 2); %(3; 4))"#
    );
    // a name the header lacks extends it, null in every other row
    assert_eq!(
        one(r#"= @%(@(a); %(1); %(2; c = 3))"#),
        r#"@%(@(a; c); %(1; null); %(2; 3))"#
    );
    // a plain list of records of one shape prints in the form; one
    // record, or records of differing shapes, print record by record
    assert_eq!(one(r#"= @(%(a = 1); %(a = 2))"#), r#"@%(@(a); %(1); %(2))"#);
    assert_eq!(one(r#"= @(%(a = 1))"#), r#"@(%(a = 1))"#);
    assert_eq!(
        one(r#"= @(%(a = 1); %(b = 2))"#),
        r#"@(%(a = 1); %(b = 2))"#
    );
    assert_eq!(
        one(r#"= @(%(a = 1; b = 2); %(b = 3; a = 4))"#),
        r#"@(%(a = 1; b = 2); %(b = 3; a = 4))"#
    );
    // a cell holds any value: no width is promised
    assert_eq!(
        one(r#"= @%(@(a); %(@(1; 2)); %(3))"#),
        r#"@%(@(a); %(@(1; 2)); %(3))"#
    );
    // it is a list: json, the row command, the table reading
    assert_eq!(
        one(r#"= @%(@(a; b); %(1; 2); %(3; 4)) | json"#),
        r#"[{"a": 1, "b": 2}, {"a": 3, "b": 4}]"#
    );
    assert_eq!(
        one(r#"= @%(@(a; b); %(1; 2); %(3; 4)) | [2]"#),
        r#"%(a = 3; b = 4)"#
    );
    assert_eq!(
        one(r#"= @%(@(a; b); %(1; 2); %(3; 4)) | # | dim"#),
        "@(2; 2)"
    );
    assert_eq!(one(r#"= 0 | @%(@(a); %(1); %(2)) | [1]"#), r#"%(a = 1)"#);
    // the header is required; more positional values than names refuse
    let e = run(r#"= @%(%(1; 2))"#).unwrap_err().to_string();
    assert!(e.contains("header"), "{e}");
    let e = run(r#"= @%(@(a); %(1; 2))"#).unwrap_err().to_string();
    assert!(e.contains("row 1") && e.contains("positional"), "{e}");
}

/// Table columns are typed (spec: The Table): one scalar type per
/// column, the least every non-null cell coerces to, the cells
/// stored coerced; `| type` names them. The record array stays
/// untyped.
#[test]
fn table_columns_are_typed() {
    // digit strings make an integer column; the cells print bare
    assert_eq!(
        one(r#"= #(@(year; name); %("1999"; "a"); %(2001; "b"))"#),
        "#(@(year; name);\n  %(1999; \"a\");\n  %(2001; \"b\"))"
    );
    assert_eq!(
        one(r#"= #(@(year; name); %("1999"; "a"); %(2001; "b")) | type"#),
        r#"@("int"; "text")"#
    );
    assert_eq!(
        one(r#"= #(@(year; name); %("1999"; "a"); %(2001; "b")) | :year"#),
        "@(1999; 2001)"
    );
    // the lattice: boolean < integer < float
    assert_eq!(one(r#"= #(@(b); %("true"); %(false)) | type"#), "bool");
    assert_eq!(
        one(r#"= #(@(n); %(true); %(2))"#),
        "#(@(n);\n  %(1);\n  %(2))"
    );
    assert_eq!(one(r#"= #(@(x); %(1); %("2.5")) | type"#), "float");
    assert_eq!(one(r#"= #(@(x); %(1); %("2.5")) | :x"#), "@(1; 2.5)");
    // the types of their own
    assert_eq!(
        one(r#"= #(@(d); %("2020-01-02"); %("2020-01-03T10:00:00Z")) | type"#),
        "instant"
    );
    assert_eq!(one(r#"= #(@(d); %("PT1H"); %("5d")) | type"#), "duration");
    assert_eq!(
        one(r#"= #(@(p); %("1812"); %("2025-10")) | type"#),
        "period"
    );
    assert_eq!(one(r#"= #(@(q); %("5km"); %("3 m")) | type"#), "quantity");
    // text is the top: a quantity column of two dimensions, a mixed column
    assert_eq!(one(r#"= #(@(q); %("5km"); %("3kg")) | type"#), "text");
    assert_eq!(one(r#"= #(@(c); %(1); %("a")) | :c"#), r#"@("1"; "a")"#);
    assert_eq!(one(r#"= #(@(c); %(null); %(null)) | type"#), "text");
    // null is missing in any column
    assert_eq!(one(r#"= #(@(n); %("7"); %(null)) | type"#), "int");
    // the table reading of records types the same way
    assert_eq!(
        one(r#"= @(%(y = "1999"); %(y = "2001")) | # | :y"#),
        "@(1999; 2001)"
    );
    assert_eq!(
        one(r#"/rows/* | %(::book; ::x) @| # | type"#),
        r#"@("text"; "int")"#
    );
    // the record array stays untyped: values as written
    assert_eq!(
        one(r#"= @%(@(a); %("1"); %(2))"#),
        r#"@%(@(a); %("1"); %(2))"#
    );
    // the header lowered into the rows makes its column text: the
    // numbers below it take their text form
    assert_eq!(
        one(r#"= #(@(name; age); %("Braund"; 22)) | header-to-row | :2"#),
        r#"@("age"; "22")"#
    );
}

/// `| header` is the columns as typed projectors (spec: The Table,
/// draft 166): a declared column with its type, an undeclared one
/// with the star and the type inferred; `keys` and `type` are its
/// two lists. `#(--header=@(:a<int>; :b))` builds under a declared
/// header, casting the declared columns; on an existing table it is
/// the named columns in the header's order, cast (`t:#(h...)`); the
/// record form of the header is retired.
#[test]
fn the_header_is_typed_and_a_declared_header_casts() {
    let t = r#"= #(@(name; age); %("Braund"; 22); %("Allen"; 35))"#;
    assert_eq!(one(&format!("{t} | header")), "@(:name<*; text>; :age<*; int>)");
    assert_eq!(one(&format!("{t} | header | [2]")), ":age<*; int>");
    assert_eq!(one(&format!("{t} | keys")), r#"@("name"; "age")"#);
    assert_eq!(one(&format!("{t} | type")), r#"@("text"; "int")"#);
    // a list of records: the union, typed over the records
    assert_eq!(
        one(r#"= @(%(a = 1); %(a = 2.5; b = "x")) | header"#),
        "@(:a<*; float>; :b<*; text>)"
    );
    // a record: its fields, a structure under the bare star
    assert_eq!(one(r#"= %(a = 1; b = "x"; c = @(1)) | :::header"#), "@(:a<*; int>; :b<*; text>; :c<*>)");
    assert_eq!(one("= null | header"), "null");
    assert!(refuse("= 5 | header").contains("keys and type are the two lists"));
    assert!(refuse("/rows/* @| header").contains("not all records"));
    // the declared header over a flat list: the projectors name, a
    // declared one casts; a bare name or projector is undeclared
    assert_eq!(
        one(r#"= @("1"; "2.5"; "true"; "x") | #(--header=@(:n<int>; :f<float>; :b<bool>; :t<text>)) | header"#),
        "@(:n<int>; :f<float>; :b<bool>; :t<text>)"
    );
    assert_eq!(
        one(r#"= @("1"; "2"; "3"; "4") | #(-h=@(:a<text>; :b<float>))"#),
        "#(@(a; b);\n  %(\"1\"; 2);\n  %(\"3\"; 4))"
    );
    assert_eq!(
        one(r#"= @("1"; "2.5") | #(-h=@(:n<int>; f)) | header"#),
        "@(:n<int>; :f<*; float>)"
    );
    // a cell that does not cast refuses naming the column and the row
    let e = refuse(r#"= @("1"; "x"; "2"; "3") | #(-h=@(:a<int>; :b<int>))"#);
    assert!(e.contains("column 'b' is declared int, and row 1 holds \"x\""), "{e}");
    // over a table: the named columns in the header's order, cast as
    // declared; a named column the data lacks refuses
    assert_eq!(
        one(&format!("{t} | #(--header=@(:age<float>; :name)) | header")),
        "@(:age<float>; :name<*; text>)"
    );
    assert_eq!(
        one(r#"= @(%(a = "1"; b = "x"); %(a = "2"; b = "y")) | #(--header=@(:b<text>; :a<float>)) | :a"#),
        "@(1; 2)"
    );
    let e = refuse(&format!("{t} | #(--header=@(:zip<text>))"));
    assert!(e.contains("names column 'zip', and the data has no such column"), "{e}");
    // the inferred type a header reports is no request: the star
    // declares nothing
    assert_eq!(
        one(&format!("{t} | #(--header=@(:age<*; text>)) | header")),
        "@(:age<*; int>)"
    );
    // the words are checked when the query is read
    let e = refuse(r#"= @(1; 2) | #(-h=@(:n<number>))"#);
    assert!(e.contains("'number' is none"), "{e}");
    // the record form is retired, with the pointer to the list
    let e = refuse(r#"= @(1; 2) | #(-h=%(n = "int"))"#);
    assert!(e.contains("retired") && e.contains("#(-h=@(:n<int>))"), "{e}");
    let e = refuse(r#"= @(1..4) | .h(%(a = "float"; b = "int")) | @(5; 6; 7; 8) | #(-h=$.h)"#);
    assert!(e.contains("retired") && e.contains("@(:a<float>; :b<int>)"), "{e}");
    // a header computed elsewhere: the round trip through a capsa
    // keeps the declarations, and the reported types declare nothing
    assert_eq!(
        one(r#"= @(1..4) | .t(#(-h=@(:a<int>; b))) | @("5"; "6"; "7"; "8.5") | #(-h=($.t | header)) | header"#),
        "@(:a<int>; :b<*; float>)"
    );
    assert_eq!(
        one(r#"= @(1..4) | .t(#(-h=@(a; b))) | @(5; 6; 7; 8) | #(-h=($.t | keys)) | dim"#),
        "@(2; 2)"
    );
    let e = refuse(r#"= @(1..4) | .h(5) | @(5; 6) | #(-h=$.h)"#);
    assert!(e.contains("the computed one is a number"), "{e}");
    // after @| the stream builds under the declared header
    assert_eq!(
        one(r#"= 1..4 @| #(--header=@(:a<float>; :b<int>)) | header"#),
        "@(:a<float>; :b<int>)"
    );
    // the header applied as the structured projector it is: the
    // spread into the table shape narrows and casts; a list applied
    // keeps its shape, each column cast
    assert_eq!(
        one(&format!("{t} | .t | $.t | #(-h=@(:age<float>)) | header | .h | $.t:#($.h...) | header")),
        "@(:age<float>)"
    );
    assert_eq!(one(&format!("{t} | .t | $.t:@(:age<float>) | [1] | [1] | type")), "float");
}
