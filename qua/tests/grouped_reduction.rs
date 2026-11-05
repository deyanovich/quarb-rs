//! The grouped-reduction round (ruling 2026-11-05): `.%(body)` files
//! one anonymous regula (the spec stands); the context accessors stand
//! as a stage after either pipe and as an operand; `--by=:col` on the
//! per-column reducers answers a table indexed by the keys, each
//! other column `col_reducer`; `++` between two tables follows the
//! header rule (the same header appends rows, disjoint headers
//! columns, by label where both are indexed); an operator over two indexed tables aligns
//! by label and keeps the index; `unstack` refuses a repeated label.
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    let test = std::thread::current()
        .name()
        .unwrap_or("t")
        .replace(':', "-");
    let dir = std::env::temp_dir().join(format!("quarb-grouped-{}-{test}", std::process::id()));
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

/// The table of the four rows, the shape every test reduces.
const T: &str = "/row/* @| #";

/// `.%(body)` files the built record as one anonymous regula (the
/// spec stands, ruling 2026-11-05), `.name%(body)` under the name;
/// `| %(body) | .%` files a constructor's fields; `.:%(body)` is no
/// push.
#[test]
fn the_record_push_files_one_anonymous_regula() {
    let d = fixture();
    assert_eq!(
        ok(&d, "/row/* @| [..2] | .%(::Sex; a = ::Age) | %.*"),
        "%(.1 = %(Sex = \"male\"; a = 22))\n%(.1 = %(Sex = \"female\"; a = 38))"
    );
    assert_eq!(
        ok(&d, "/row/* @| [..2] | .%(::Sex; ::Age) | $.Age"),
        "null\nnull"
    );
    assert_eq!(
        ok(&d, "/row/* @| [..2] | %(::Sex; ::Age) | .% | %."),
        "%(Sex = \"male\"; Age = 22)\n%(Sex = \"female\"; Age = 38)"
    );
    assert_eq!(
        ok(&d, "/row/* @| [..2] | .k%(::Sex; ::Age) | $.k"),
        "%(Sex = \"male\"; Age = 22)\n%(Sex = \"female\"; Age = 38)"
    );
    let e = err(&d, "/row/* @| [..2] | .:%(::Sex; ::Age) | :%.");
    assert!(
        e.contains("unexpected trailing input") || e.contains("attaching fields push is bare"),
        "{e}"
    );
}

/// `#.`, `#.*`, `#_` stand after `@|` as after `|`, and as operands.
#[test]
fn the_context_tables_after_the_aggregation_pipe() {
    let d = fixture();
    let pushed = "/row/* @| [..2] | .s(::Sex) | .a(::Age)";
    assert_eq!(
        ok(&d, &format!("{pushed} @| #.")),
        "#(@(s; a);\n  %(\"male\"; 22);\n  %(\"female\"; 38))"
    );
    assert_eq!(
        ok(&d, &format!("{pushed} @| #.*")),
        "#(@(s; a);\n  %(\"male\"; 22);\n  %(\"female\"; 38))"
    );
    assert_eq!(
        ok(&d, &format!("{pushed} @| #_")),
        "#(@(topic; position);\n  %(null; /row/0);\n  %(null; /row/1))"
    );
    assert_eq!(
        ok(&d, &format!("{pushed} | %(k = #.) @| first")),
        "%(k = #(@(s; a); %(\"male\"; 22); %(\"female\"; 38)))"
    );
    assert_eq!(
        ok(&d, &format!("{pushed} @| #. ++ %(s = \"x\"; a = 0)")),
        "#(@(s; a);\n  %(\"male\"; 22);\n  %(\"female\"; 38);\n  %(\"x\"; 0))"
    );
}

/// `--by=:col`: one row per distinct key in order of first
/// appearance, every other column `col_reducer`, the answer indexed
/// by the key; `--of` one column; a tuple of keys a composite index.
#[test]
fn the_grouped_reduction() {
    let d = fixture();
    assert_eq!(
        ok(&d, &format!("{T} | mean(--by=:Sex)")),
        "#(@(Sex; Class_mean; Age_mean; Fare_mean);\n  %(\"male\"; 1.5; 24; 8);\n  %(\"female\"; 1.5; 36.5; 62.3))"
    );
    assert_eq!(
        ok(&d, &format!("{T} | max(-b=:Sex)")),
        "#(@(Sex; Class_max; Age_max; Fare_max);\n  %(\"male\"; 2; 26; 8.75);\n  %(\"female\"; 2; 38; 71.5))"
    );
    assert_eq!(
        ok(&d, &format!("{T} | min(--of=:Age --by=:Sex)")),
        "#(@(Sex; Age_min);\n  %(\"male\"; 22);\n  %(\"female\"; 35))"
    );
    assert_eq!(
        ok(&d, &format!("{T} | sum(--of=:Age * 2 --by=:Sex)")),
        "#(@(Sex; sum);\n  %(\"male\"; 96);\n  %(\"female\"; 146))"
    );
    assert_eq!(
        ok(&d, &format!("{T} | count(--by=:Class)")),
        "#(@(Class; count);\n  %(1; 2);\n  %(2; 2))"
    );
    assert_eq!(
        ok(&d, &format!("{T} | percentile(50; --by=:Sex) | header")),
        "@(:Sex<*; text>; :Class_percentile50<*; float>; :Age_percentile50<*; float>; :Fare_percentile50<*; float>)"
    );
    // the composite key
    assert_eq!(
        ok(&d, &format!("{T} | mean(--by=(:Sex; :Class)) | [..2]")),
        "#(@(Sex; Class; Age_mean; Fare_mean);\n  %(\"male\"; 1; 22; 7.25);\n  %(\"female\"; 1; 38; 71.5))"
    );
    // the answer is indexed by the key: a column reads as a record
    assert_eq!(
        ok(&d, &format!("{T} | mean(--by=:Sex) | $_:Age_mean")),
        "%(male = 24; female = 36.5)"
    );
    // an index the input had is dropped unless named
    assert_eq!(
        ok(
            &d,
            &format!("{T} | #(-i=:Class) | mean(--by=:Sex) | header")
        ),
        "@(:Sex<*; text>; :Age_mean<*; float>; :Fare_mean<*; float>)"
    );
    // without the option nothing changes
    assert_eq!(
        ok(&d, &format!("{T} | mean")),
        "%(Sex = null; Class = 1.5; Age = 30.25; Fare = 35.15)"
    );
    let e = err(&d, &format!("{T} | mean(--by=:Nope)"));
    assert!(e.contains("--by names column 'Nope'"), "{e}");
    let e = err(&d, "= 3 | mean(--by=:x)");
    assert!(e.contains("--by groups the rows of a table"), "{e}");
}

/// `++` between two tables follows the header rule (ruling
/// 2026-11-05): the same header appends the right side's rows, the
/// index kept; disjoint headers append its columns, aligned by label
/// where both tables are indexed and by position otherwise, on
/// tables of one height; headers that overlap without being equal
/// are refused naming the shared columns. `++!` follows the rule and
/// overwrites where `++` would repeat or refuse: under the same
/// header a right row whose label the left has replaces that row in
/// place (the update), under overlapping headers the right side's
/// values take the shared names. The alignment follows pandas join:
/// one-level indexes by label whatever their names, a composite
/// index takes a plain one on the level of its name, composite
/// indexes on the same levels in the same order.
#[test]
fn tables_concatenate_by_the_header_rule() {
    let d = fixture();
    // disjoint data columns on one index: the columns, by label
    assert_eq!(
        ok(
            &d,
            &format!(
                "{T} | min(--of=:Age --by=:Sex) ++ max(--of=:Age --by=:Sex) ++ mean(--of=:Fare --by=:Sex)"
            )
        ),
        "#(@(Sex; Age_min; Age_max; Fare_mean);\n  %(\"male\"; 22; 26; 8);\n  %(\"female\"; 35; 38; 62.3))"
    );
    assert_eq!(
        ok(
            &d,
            &format!("{T} | (min(--of=:Age --by=:Sex) ++ ($_[..1] | max(--of=:Fare --by=:Sex)))")
        ),
        "#(@(Sex; Age_min; Fare_max);\n  %(\"male\"; 22; 7.25);\n  %(\"female\"; 35; null))"
    );
    // the same header: the rows, the index kept, a repeated label kept
    assert_eq!(
        ok(
            &d,
            &format!("{T} | (min(--by=:Sex) ++ min(--by=:Sex)) | dim")
        ),
        "@(4; 4)"
    );
    assert_eq!(
        ok(
            &d,
            &format!("{T} | (min(--by=:Sex) ++ min(--by=:Sex)) | $_:::index")
        ),
        "@(\"male\"; \"female\"; \"male\"; \"female\")"
    );
    assert_eq!(
        ok(&d, &format!("{T} | ($_[..1] ++ $_[..1]) | dim")),
        "@(2; 4)"
    );
    // headers that overlap without being equal: refused naming the shared columns
    let e = err(
        &d,
        &format!("{T} | min(--of=:Age --by=:Sex) ++ min(--by=:Sex)"),
    );
    assert!(
        e.contains("the headers share 'Age_min' but are not the same"),
        "{e}"
    );
    let e = err(
        &d,
        &format!("{T} | ($_[..1] ++ ($_[..1] | $_:#(:Sex; :Age)))"),
    );
    assert!(
        e.contains("the headers share 'Sex', 'Age' but are not the same"),
        "{e}"
    );
    // `++!` under the same header: the update — a label the left has
    // replaced in place, the rest untouched, a new label appended
    assert_eq!(
        ok(
            &d,
            &format!(
                "{T} | (mean(--by=:Sex) ++! (mean(--by=:Sex) + mean(--by=:Sex))) | $_:Age_mean"
            )
        ),
        "%(male = 48; female = 73)"
    );
    assert_eq!(
        ok(
            &d,
            &format!("{T} | (mean(--by=:Sex) ++! ($_[..1] | mean(--by=:Sex) + 100)) | $_:Age_mean")
        ),
        "%(male = 122; female = 36.5)"
    );
    assert_eq!(
        ok(
            &d,
            &format!(
                "{T} | (mean(--by=:Sex) ++! #(@(Sex; Class_mean; Age_mean; Fare_mean); %(\"child\"; 3; 5; 1))) | $_:Age_mean"
            )
        ),
        "%(male = 24; female = 36.5; child = 5)"
    );
    // `++!` under overlapping headers: the columns, the right side's under the shared names
    assert_eq!(
        ok(
            &d,
            &format!("{T} | (min(--of=:Age --by=:Sex) ++! (min(--by=:Sex) + 1)) | $_:Age_min")
        ),
        "%(male = 23; female = 36)"
    );
    // `++!` with nothing to overwrite does what `++` does
    assert_eq!(
        ok(&d, "= (#(@(1; 2); @(3; 4)) ++! #(@(5; 6); @(7; 8))) | dim"),
        "@(4; 2)"
    );
    // disjoint headers, not both indexed: the columns by position, the left's index kept
    assert_eq!(
        ok(
            &d,
            "= (%(ix = @(0; 1; 2)) | #(-i=:ix)) ++ #(@(a; b); %(1; 2); %(3; 4); %(5; 6)) | $_:::index"
        ),
        "@(0; 1; 2)"
    );
    assert_eq!(
        ok(
            &d,
            "= (%(ix = @(0; 1; 2)) | #(-i=:ix)) ++ #(@(a; b); %(1; 2); %(3; 4); %(5; 6))"
        ),
        "#(@(ix; a; b);\n  %(0; 1; 2);\n  %(1; 3; 4);\n  %(2; 5; 6))"
    );
    let e = err(
        &d,
        "= (%(ix = @(0; 1)) | #(-i=:ix)) ++ #(@(a); %(1); %(2); %(3))",
    );
    assert!(e.contains("the left table has 2 rows, the right 3"), "{e}");
    // the alignment: one-level indexes by label whatever their names,
    // the left's name kept (its declared type casting the labels)
    assert_eq!(
        ok(
            &d,
            &format!("{T} | min(--of=:Age --by=:Sex) ++ min(--of=:Fare --by=:Class) | $_:::index")
        ),
        "@(\"male\"; \"female\"; \"1\"; \"2\")"
    );
    // a composite index takes a plain one on the level of its name,
    // the plain row repeated over the other levels, either side
    assert_eq!(
        ok(
            &d,
            &format!("{T} | mean(--of=:Age --by=(:Sex; :Class)) ++ max(--of=:Fare --by=:Class)")
        ),
        "#(@(Sex; Class; Age_mean; Fare_max);\n  %(\"male\"; 1; 22; 71.5);\n  %(\"female\"; 1; 38; 71.5);\n  %(\"male\"; 2; 26; 53.1);\n  %(\"female\"; 2; 35; 53.1))"
    );
    assert_eq!(
        ok(
            &d,
            &format!("{T} | max(--of=:Fare --by=:Class) ++ mean(--of=:Age --by=(:Sex; :Class))")
        ),
        "#(@(Sex; Class; Fare_max; Age_mean);\n  %(\"male\"; 1; 71.5; 22);\n  %(\"female\"; 1; 71.5; 38);\n  %(\"male\"; 2; 53.1; 26);\n  %(\"female\"; 2; 53.1; 35))"
    );
    let e = err(
        &d,
        &format!("{T} | mean(--of=:Age --by=(:Sex; :Class)) ++ max(--of=:Fare --by=:Fare)"),
    );
    assert!(
        e.contains("no level of the index (Sex; Class) carries the name 'Fare'"),
        "{e}"
    );
    // composite indexes: the same levels in the same order
    assert_eq!(
        ok(
            &d,
            &format!(
                "{T} | mean(--of=:Age --by=(:Sex; :Class)) ++ max(--of=:Fare --by=(:Sex; :Class)) | $_:Fare_max"
            )
        ),
        "%(male = %(1 = 7.25; 2 = 8.75); female = %(1 = 71.5; 2 = 53.1))"
    );
    let e = err(
        &d,
        &format!(
            "{T} | mean(--of=:Age --by=(:Sex; :Class)) ++ max(--of=:Fare --by=(:Class; :Sex))"
        ),
    );
    assert!(
        e.contains("the composite indexes differ, (Sex; Class) against (Class; Sex)"),
        "{e}"
    );
}

/// Two indexed tables under an operator align by label and keep the
/// index; `unstack` on a repeated label refuses, naming it.
#[test]
fn indexed_tables_under_an_operator_and_unstack() {
    let d = fixture();
    assert_eq!(
        ok(
            &d,
            &format!("{T} | (mean(--by=:Sex) + mean(--by=:Sex)) | $_:Age_mean")
        ),
        "%(male = 48; female = 73)"
    );
    assert_eq!(
        ok(&d, &format!("{T} | #(-i=:Sex) | ($_ + $_) | [..1]")),
        "#(@(Sex; Class; Age; Fare);\n  %(\"male\"; 2; 44; 14.5))"
    );
    assert_eq!(
        ok(&d, &format!("{T} | mean(--by=:Sex) | stack | unstack")),
        "#(@(Sex; Class_mean; Age_mean; Fare_mean);\n  %(\"male\"; 1.5; 24; 8);\n  %(\"female\"; 1.5; 36.5; 62.3))"
    );
    let e = err(&d, &format!("{T} | #(-i=:Sex) | stack | unstack"));
    assert!(e.contains("stands on several rows"), "{e}");
}

/// The keyed thread commands on the plain pipe rank a table's rows
/// or a list's items, the table (the list) answered with its index
/// kept; `:::index` ranks by label; a node thread is refused with
/// the pointer, and after `@| window` the per-window form stands.
#[test]
fn keyed_commands_over_a_table_value() {
    let d = fixture();
    assert_eq!(
        ok(
            &d,
            &format!("{T} | mean(--by=:Sex) | sort_by(:::index) | $_:Age_mean")
        ),
        "%(female = 36.5; male = 24)"
    );
    assert_eq!(
        ok(
            &d,
            &format!("{T} | mean(--by=:Sex) | sort_by(-:::index) | $_:::index")
        ),
        "@(\"male\"; \"female\")"
    );
    assert_eq!(
        ok(&d, &format!("{T} | sort_by(-:Age) | $_:Age")),
        "@(38; 35; 26; 22)"
    );
    assert_eq!(
        ok(&d, &format!("{T} | top(2; :Fare) | $_:Fare")),
        "@(71.5; 53.1)"
    );
    assert_eq!(
        ok(&d, &format!("{T} | unique_by(:Sex) | $_:Age")),
        "@(22; 38)"
    );
    assert_eq!(ok(&d, &format!("{T} | min_by(:Age) | $_:Age")), "@(22)");
    assert_eq!(
        ok(
            &d,
            &format!("{T} | mean(--by=(:Sex; :Class)) | sort_by(:::index) | $_:Age_mean")
        ),
        "%(female = %(1 = 38; 2 = 35); male = %(1 = 22; 2 = 26))"
    );
    assert_eq!(ok(&d, "= @(3; 1; 2) | sort_by($_)"), "@(1; 2; 3)");
    assert_eq!(ok(&d, "= @(3; 1; 2) | top(2; $_)"), "@(3; 2)");
    let e = err(&d, "/row/* | sort_by(::Age)");
    assert!(e.contains("ranks a context"), "{e}");
}

/// `count(--by)` counts the group's rows, the one column `count`;
/// `--values` / `--nulls` count per column, `col_count` / `col_nulls`;
/// over a table without `--by` the flags answer the per-column record.
#[test]
fn count_under_by_and_its_flags() {
    let d = fixture();
    assert_eq!(
        ok(&d, &format!("{T} | count(--by=:Sex)")),
        "#(@(Sex; count);\n  %(\"male\"; 2);\n  %(\"female\"; 2))"
    );
    assert_eq!(
        ok(&d, &format!("{T} | $_:#(:Sex) | count(--by=:Sex)")),
        "#(@(Sex; count);\n  %(\"male\"; 2);\n  %(\"female\"; 2))"
    );
    assert_eq!(
        ok(
            &d,
            "/row/* @| # | $_:#(:Sex; :Age) | ++ %(Sex = \"male\"; Age = null) | count(-v --by=:Sex)"
        ),
        "#(@(Sex; Age_count);\n  %(\"male\"; 2);\n  %(\"female\"; 2))"
    );
    assert_eq!(
        ok(
            &d,
            "/row/* @| # | $_:#(:Sex; :Age) | ++ %(Sex = \"male\"; Age = null) | count(-n --by=:Sex)"
        ),
        "#(@(Sex; Age_nulls);\n  %(\"male\"; 1);\n  %(\"female\"; 0))"
    );
    assert_eq!(
        ok(
            &d,
            "/row/* @| # | $_:#(:Sex; :Age) | ++ %(Sex = \"male\"; Age = null) | count(-v)"
        ),
        "%(Sex = 5; Age = 4)"
    );
    assert_eq!(
        ok(
            &d,
            "/row/* @| # | $_:#(:Sex; :Age) | ++ %(Sex = \"male\"; Age = null) | count(-n)"
        ),
        "%(Sex = 0; Age = 1)"
    );
    assert_eq!(ok(&d, &format!("{T} | count")), "4");
}

/// `autoindex` makes the implicit index explicit (ruling 2026-11-05):
/// a column `index` of consecutive labels prepended and named the
/// index — positions from 1, `--start` where to begin, `--step` the
/// step (1 for a number; required for a quantity or an instant).
#[test]
fn autoindex_labels_the_rows() {
    let d = fixture();
    assert_eq!(
        ok(&d, &format!("{T} | $_:#(:Sex; :Age) | [..2] | autoindex")),
        "#(@(index; Sex; Age);\n  %(1; \"male\"; 22);\n  %(2; \"female\"; 38))"
    );
    assert_eq!(
        ok(&d, &format!("{T} | autoindex(--start=0) | $_:::index")),
        "@(0; 1; 2; 3)"
    );
    assert_eq!(
        ok(
            &d,
            &format!("{T} | autoindex(--start=10 --step=5) | $_:::index")
        ),
        "@(10; 15; 20; 25)"
    );
    assert_eq!(
        ok(
            &d,
            &format!("{T} | autoindex(--start=1_m --step=0.5_m) | $_:::index")
        ),
        "@(1_m; 1.5_m; 2_m; 2.5_m)"
    );
    assert_eq!(
        ok(
            &d,
            &format!("{T} | autoindex(--start=2001-01-01 --step=1_d) | $_:::index")
        ),
        "@(2001-01-01; 2001-01-02; 2001-01-03; 2001-01-04)"
    );
    // the new index is first; an index the table had stays a column
    assert_eq!(
        ok(
            &d,
            &format!("{T} | #(-i=:Sex) | autoindex(--start=0) | [[1]]")
        ),
        "%(index = 1; Sex = \"female\"; Class = 1; Age = 38; Fare = 71.5)"
    );
    let e = err(&d, &format!("{T} | autoindex(--start=1_m)"));
    assert!(e.contains("the start 1_m takes a step"), "{e}");
    let e = err(&d, &format!("{T} | autoindex(--start=2001-01-01 --step=2)"));
    assert!(e.contains("of another kind than the start"), "{e}");
    let e = err(&d, "= 5 | autoindex");
    assert!(e.contains("labels the rows of a table"), "{e}");
}
