//! The tables round (rulings 2026-11-05, the 0.53 line): a table
//! projector's expression items evaluate per row (`$_` the table in
//! the glued form); `:*` is the rest of the columns or fields; `map`
//! over a table reads the row and `--to=:new` appends a column,
//! `--to=:::keys` renames the header; a record takes positions;
//! comparisons distribute over a list in value position, a predicate
//! reads them as any, `any` / `all` reduce them; `corr` over a table
//! value; a stage projector with a rename, `map(split)`, `map` inside
//! parentheses.
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    let test = std::thread::current()
        .name()
        .unwrap_or("t")
        .replace(':', "-");
    let dir = std::env::temp_dir().join(format!("quarb-tables-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("rows.json"),
        r#"{"row":[
            {"Sex":"male","Class":1,"Age":22,"Fare":7.25},
            {"Sex":"female","Class":1,"Age":38,"Fare":71.5},
            {"Sex":"male","Class":2,"Age":26,"Fare":8.75},
            {"Sex":"female","Class":2,"Age":35,"Fare":53.1}]}"#,
    )
    .unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_qua"))
        .current_dir(dir)
        .env("QUARB_LOCALE", "C")
        .env("HOME", dir)
        .args(args)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
        out.status.success(),
    )
}

fn ok(dir: &Path, query: &str) -> String {
    let (out, err, good) = qua(dir, &[query, "rows.json"]);
    assert!(good, "{query}: {err}");
    out
}

fn err(dir: &Path, query: &str) -> String {
    let (out, err, good) = qua(dir, &[query, "rows.json"]);
    assert!(!good, "{query} succeeded: {out}");
    err
}

/// The table of the four rows.
const T: &str = "/row/* @| #";

/// An expression item of a table projector evaluates per row, the
/// bare `:k` the row's cell; `$_` written out is the table, so a
/// column-wide read is `($_:Age | mean)`; the stage form answers as
/// the glued one, a rename included.
#[test]
fn projector_items_evaluate_per_row() {
    let d = fixture();
    assert_eq!(
        ok(
            &d,
            &format!("{T} | $_:#(:Sex; short = (:Sex = \"male\" ? \"M\" : \"F\")) | [..2]")
        ),
        "#(@(Sex; short);\n  %(\"male\"; \"M\");\n  %(\"female\"; \"F\"))"
    );
    assert_eq!(
        ok(
            &d,
            &format!(
                "{T} | $_:#(:Age; over30 = (:Age > 30); z = (:Age - ($_:Age | mean))) | [..2]"
            )
        ),
        "#(@(Age; over30; z);\n  %(22; false; -8.25);\n  %(38; true; 7.75))"
    );
    // a spreading command then a position: the piece of each row
    assert_eq!(
        ok(
            &d,
            &format!("{T} | $_:#(:Sex; head = (:Sex | split(\"a\") | [1])) | [..2]")
        ),
        "#(@(Sex; head);\n  %(\"male\"; \"m\");\n  %(\"female\"; \"fem\"))"
    );
    // the stage form, a rename first: the projector all the same
    assert_eq!(
        ok(&d, &format!("{T} | #(S = :Sex; :Age) | [..2]")),
        "#(@(S; Age);\n  %(\"male\"; 22);\n  %(\"female\"; 38))"
    );
    // a column the row lacks is no column
    let e = err(&d, &format!("{T} | $_:#(:Sex; i = (:index + 1))"));
    assert!(
        e.contains("no column 'index' (the columns: Sex, Class, Age, Fare)"),
        "{e}"
    );
}

/// `:*` is the rest of the columns (the fields), in the table's
/// order, where it stands: what the items neither produce nor take
/// whole; a column only read inside an expression keeps its place.
#[test]
fn the_rest_item() {
    let d = fixture();
    assert_eq!(
        ok(&d, &format!("{T} | $_:#(:*; over30 = (:Age > 30)) | [..1]")),
        "#(@(Sex; Class; Age; Fare; over30);\n  %(\"male\"; 1; 22; 7.25; false))"
    );
    assert_eq!(
        ok(&d, &format!("{T} | $_:#(S = :Sex; :*) | [..1]")),
        "#(@(S; Class; Age; Fare);\n  %(\"male\"; 1; 22; 7.25))"
    );
    assert_eq!(
        ok(&d, &format!("{T} | $_:#(:*; Age = (:Age * 2)) | [..1]")),
        "#(@(Sex; Class; Fare; Age);\n  %(\"male\"; 1; 7.25; 44))"
    );
    assert_eq!(
        ok(&d, &format!("{T} | $_:#(:Age; :*) | [..1]")),
        "#(@(Age; Sex; Class; Fare);\n  %(22; \"male\"; 1; 7.25))"
    );
    assert_eq!(
        ok(&d, &format!("{T} | #(:*; :Age) | [..1]")),
        "#(@(Sex; Class; Fare; Age);\n  %(\"male\"; 1; 7.25; 22))"
    );
    assert_eq!(
        ok(&d, "= %(a = 1; b = 2; c = 3):%(:*; d = 4)"),
        "%(a = 1; b = 2; c = 3; d = 4)"
    );
    assert_eq!(
        ok(&d, "= %(a = 1; b = 2; c = 3):%(B = :b; :*)"),
        "%(B = 2; a = 1; c = 3)"
    );
    let e = err(&d, "= 5:%(:*)");
    assert!(
        e.contains("':*' stands in a projector over a record"),
        "{e}"
    );
}

/// `map` over a table reads the row: `:k` the row's cell in column
/// k, the mapped cell the topic; `--to=:new` appends the column last;
/// `--to=:::keys` maps the header's names; a spreading body answers a
/// list per cell; `map` inside parentheses keeps its body.
#[test]
fn map_reads_the_row_and_appends() {
    let d = fixture();
    assert_eq!(
        ok(
            &d,
            &format!("{T} | [..2] | map((:Age > 30); --to=:Age) | $_:Age")
        ),
        "@(false; true)"
    );
    assert_eq!(
        ok(&d, &format!("{T} | [..2] | map((:Age > 30); --to=:over30)")),
        "#(@(Sex; Class; Age; Fare; over30);\n  %(\"male\"; 1; 22; 7.25; false);\n  %(\"female\"; 1; 38; 71.5; true))"
    );
    assert_eq!(
        ok(
            &d,
            &format!("{T} | [..2] | map((:Fare div :Age) | round(2); --to=:Fare) | $_:Fare")
        ),
        "@(0.33; 1.88)"
    );
    let e = err(&d, &format!("{T} | [..1] | map((:index <= 2); --to=:Sex)"));
    assert!(e.contains("no column 'index'"), "{e}");
    assert_eq!(
        ok(&d, &format!("{T} | map(lower; --to=:::keys) | [..1]")),
        "#(@(sex; class; age; fare);\n  %(\"male\"; 1; 22; 7.25))"
    );
    assert_eq!(
        ok(&d, "= %(Ab = 1; Cd = 2) | map(lower; --to=:::keys)"),
        "%(ab = 1; cd = 2)"
    );
    assert_eq!(
        ok(&d, "= @(\"a,b\"; \"c,d\") | map(split(\",\"))"),
        "@(@(\"a\"; \"b\"); @(\"c\"; \"d\"))"
    );
    assert_eq!(ok(&d, "= (@(1.26; 2.31) | map(round(1)) | min)"), "1.3");
    assert_eq!(
        ok(
            &d,
            "= (@(\"2019-05-07T01:00:00+00:00\"; \"2019-05-08T01:00:00+00:00\") | map(datetime) | min)"
        ),
        "2019-05-07T01:00:00Z"
    );
}

/// A record takes positions as a list of its fields does, keys kept:
/// `[n]` the value, a range the record; `first` / `last` the value,
/// with a count the record.
#[test]
fn a_record_takes_positions() {
    let d = fixture();
    assert_eq!(
        ok(&d, "= %(a = 1; b = 2; c = 3) | [..2]"),
        "%(a = 1; b = 2)"
    );
    assert_eq!(ok(&d, "= %(a = 1; b = 2; c = 3) | [2]"), "2");
    assert_eq!(ok(&d, "= %(a = 1; b = 2; c = 3) | [-1]"), "3");
    assert_eq!(ok(&d, "= %(a = 1; b = 2; c = 3) | first"), "1");
    assert_eq!(ok(&d, "= %(a = 1; b = 2; c = 3) | last"), "3");
    assert_eq!(
        ok(&d, "= %(a = 1; b = 2; c = 3) | first(2)"),
        "%(a = 1; b = 2)"
    );
    assert_eq!(
        ok(&d, "= %(a = 1; b = 2; c = 3) | last(2)"),
        "%(b = 2; c = 3)"
    );
    // a list of one record stays a list, a context of one thread a
    // context: `first` answers the record (finding 2026-11-05)
    assert_eq!(ok(&d, "= @(%(a = 1; b = 2)) | first"), "%(a = 1; b = 2)");
    assert_eq!(
        ok(&d, "/row/*[::Age = 22] | %(a = ::Age; b = 2) @| first"),
        "%(a = 22; b = 2)"
    );
    assert_eq!(
        ok(&d, "/row/*[::Age = 22] | %(a = ::Age; b = 2) @| last"),
        "%(a = 22; b = 2)"
    );
    assert_eq!(ok(&d, &format!("{T} | $_:Age | [..2]")), "@(22; 38)");
}

/// In value position a comparison distributes over a list on either
/// side; a null element compares false; two lists zip under the
/// ordering comparisons and stay whole under `=`; the regex `==`
/// distributes with the list on the left only. In a predicate the
/// list of booleans reads as any. `any` and `all` reduce it.
#[test]
fn comparisons_distribute_and_reduce() {
    let d = fixture();
    assert_eq!(ok(&d, "= @(22; 38; 26) > 35"), "@(false; true; false)");
    assert_eq!(ok(&d, "= 35 < @(22; 38)"), "@(false; true)");
    assert_eq!(ok(&d, "= @(22; null) > 20"), "@(true; false)");
    assert_eq!(ok(&d, "= @(1; 2; 3) < @(2; 2; 2)"), "@(true; false; false)");
    assert_eq!(ok(&d, "= @(1; 2) = @(1; 2)"), "true");
    assert_eq!(ok(&d, "= @(1; 2) != @(1; 3)"), "true");
    assert_eq!(ok(&d, "= @(\"a\"; \"b\") = \"a\""), "@(true; false)");
    assert_eq!(
        ok(&d, "= @(\"verse\"; \"epic\") == (/vers/)"),
        "@(true; false)"
    );
    let e = err(&d, "= @(1; 2) < @(1; 2; 3)");
    assert!(e.contains("differ in length, 2 against 3"), "{e}");
    // a predicate: any
    assert_eq!(ok(&d, "/row/*[::Age = @(22; 35)] @| count"), "2");
    assert_eq!(ok(&d, &format!("{T} | [:Age > @(30; 40)] | count")), "2");
    // the reducers
    assert_eq!(ok(&d, "= (@(22; 38; 26) > 35) | any"), "true");
    assert_eq!(ok(&d, "= (@(22; 38; 26) > 35) | all"), "false");
    assert_eq!(ok(&d, "= (@(22; 38; 26) > 20) | all"), "true");
    assert_eq!(ok(&d, "= @() | any"), "false");
    assert_eq!(ok(&d, "= @() | all"), "true");
    assert_eq!(ok(&d, "= @(true; null) | all"), "false");
    assert_eq!(ok(&d, "= any(@(22; 38) > 35)"), "true");
    assert_eq!(ok(&d, "/row/*[any(::Age > @(30; 40))] @| count"), "2");
    let e = err(&d, "= @(1; 2) | all");
    assert!(e.contains("all: a number is no boolean"), "{e}");
}

/// `corr` over a table value reads its rows as the mount form reads
/// the nodes.
#[test]
fn corr_over_a_table_value() {
    let d = fixture();
    assert_eq!(
        ok(&d, &format!("{T} | corr(:Age; :Fare) | round(4)")),
        ok(&d, "/row/* @| corr(::Age; ::Fare) | round(4)")
    );
    assert_eq!(
        ok(&d, &format!("{T} | corr(:Age; :Fare) | round(4)")),
        "0.9778"
    );
}
