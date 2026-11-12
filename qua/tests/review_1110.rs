//! Single rows of the code review of 2026-11-10 that belong to no
//! larger batch: each a short fix with the query that showed it.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-r1110-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("books.json"),
        r#"{"books":[{"t":"Iliad","author":"Homer","tags":["war","gods"]},
                     {"t":"Odyssey","author":"Homer","tags":["sea","gods"]},
                     {"t":"Emma","author":"Austen","tags":["court"]}]}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("pairs.json"),
        r#"{"rows":[{"p":1,"q":2},{"p":2,"q":4},{"p":3,"q":7}]}"#,
    )
    .unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap().trim().to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let (success, out, err) = qua(dir, args);
    assert!(success, "{args:?}: {err}");
    out
}

fn refused(dir: &Path, args: &[&str]) -> String {
    let (success, out, err) = qua(dir, args);
    assert!(!success, "{args:?} answered: {out}");
    err
}

/// A quantifier's bound too long for a count is refused (syntax
/// B17): it read as absent, so the brace meant another walk.
#[test]
fn a_quantifier_bound_past_any_count_is_refused() {
    let d = fixture("quant");
    for q in [
        "/rows(/a){99999999999999999999999;3}",
        "/rows(/a){1;99999999999999999999999}",
    ] {
        let e = refused(&d, &[q, "pairs.json"]);
        assert!(e.contains("past any count"), "{q}: {e}");
    }
    // a bound a count can hold reads as before: the three rows and
    // their six fields
    assert_eq!(ok(&d, &["/rows(/*){1;2} @| count", "pairs.json"]), "9");
}

/// `@| corr(…)` keeps the regulae the context shares, as every other
/// reduction does (pipeline F8): the push before it read null after.
#[test]
fn an_association_keeps_the_shared_capsa() {
    let d = fixture("corr");
    assert_eq!(
        ok(
            &d,
            &["/rows/* | .n(1) @| sum(--of=::p) | $.n", "pairs.json"]
        ),
        "1"
    );
    assert_eq!(
        ok(
            &d,
            &["/rows/* | .n(1) @| corr(::p; ::q) | $.n", "pairs.json"]
        ),
        "1"
    );
}

/// A fragment's parameter is spliced into a value index as into any
/// other place (syntax B4): the substitution walked the index's base
/// and skipped its own expressions, so the parameter reached the
/// evaluator and the index answered `@()`. And a template's regula
/// read in an index is the template's own (B5): the hygiene renaming
/// skipped the same place, so it read the caller's.
#[test]
fn a_parameter_reaches_a_value_index() {
    let d = fixture("index");
    assert_eq!(
        ok(&d, &["def &f($n): | @(10; 20; 30)[$n]; = 0 | &f(2)"]),
        "20"
    );
    assert_eq!(
        ok(&d, &["def &f($n): | @(10; 20; 30)[$_ > $n]; = 0 | &f(15)"]),
        "@(20; 30)"
    );
    assert_eq!(
        ok(
            &d,
            &["macro &ix: q(| .k(2) | @(10; 20; 30)[$.k]); = 0 | .k(1) | &ix"]
        ),
        "20"
    );
}

/// A template's regulae are its own wherever it reads them (syntax
/// B5): the hygiene renaming skipped a fold's selector, an aggregate
/// push's selector, an applied quarq and brackets that read a regula
/// alone, so a template could not fold by its own binder.
#[test]
fn a_templates_regulae_are_its_own_in_every_position() {
    let d = fixture("hygiene");
    // the fold's selector is renamed with the push it names: it was
    // "no push '.g' before this point"
    assert_eq!(
        ok(
            &d,
            &[
                "macro &per($col): q(| .g($col) @|{.g} count); /rows/* | &per(::p)",
                "pairs.json"
            ]
        ),
        "1\n1\n1"
    );
    // the aggregate push's selector
    assert_eq!(
        ok(
            &d,
            &[
                "macro &tally: q(| .g(::p mod 2) | .n@{.g}(count) | $.n); /rows/* | &tally",
                "pairs.json"
            ]
        ),
        "2\n1\n2"
    );
    // brackets that read the template's regula alone
    assert_eq!(
        ok(
            &d,
            &[
                "macro &second: q(| .k(2) | \\/* @| [$.k] | ::p); /rows/1 | &second",
                "pairs.json"
            ]
        ),
        "2"
    );
}

/// `@| collapse` gathers the differing values of a regula as a list,
/// and a regula whose own value is a list is one value among them
/// (pipeline F6): the first thread's list was taken for the
/// gathering, so the lists came out flattened into it, and a scalar
/// beside a list was lost.
#[test]
fn collapse_gathers_a_list_valued_regula_whole() {
    let d = fixture("collapse");
    assert_eq!(
        ok(
            &d,
            &["/books/* | .x(::tags) | \\ @| collapse | $.x", "books.json"]
        ),
        "@(@(\"war\"; \"gods\"); @(\"sea\"; \"gods\"); @(\"court\"))"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "/books/* | .x((::author = \"Homer\" ? @(1; 2) : 1)) | \\ @| collapse | $.x",
                "books.json"
            ]
        ),
        "@(@(1; 2); 1)"
    );
    // scalars gather as before, a value met twice once
    assert_eq!(
        ok(
            &d,
            &[
                "/books/* | .a(::author) | \\ @| collapse | $.a",
                "books.json"
            ]
        ),
        "@(\"Homer\"; \"Austen\")"
    );
    // and one value shared by all stays the value
    assert_eq!(
        ok(
            &d,
            &["/books/* | .k(1) | \\ @| collapse | $.k", "books.json"]
        ),
        "1"
    );
}

/// A path navigated inside `map`'s body over a table stands on its
/// own nodes: a bare `:name` in its predicate is the node's core
/// field, not the cell of the row the body evaluates for (pipeline
/// F2). The row is the run's and reached into the nested predicate,
/// so the count was 0 where the table has a column `name`, and a
/// refusal ("no column 'name'") where it has none.
#[test]
fn a_nested_paths_predicate_reads_the_node_not_the_row() {
    let d = fixture("row-reach");
    // one book is named "0" (its position in the array)
    assert_eq!(
        ok(&d, &["^/books/*[:name = \"0\"] @| count", "books.json"]),
        "1"
    );
    // a table with a column `name`: the nested count is still 1
    assert_eq!(
        ok(
            &d,
            &[
                "/books/* | %(title = ::t; name = ::author) @| # \
                 | map((^/books/*[:name = \"0\"] @| count); --to=:n) | $_:n",
                "books.json"
            ]
        ),
        "@(1; 1; 1)"
    );
    // a table without one: no refusal
    assert_eq!(
        ok(
            &d,
            &[
                "/books/* | %(title = ::t) @| # \
                 | map((^/books/*[:name = \"0\"] @| count); --to=:n) | $_:n",
                "books.json"
            ]
        ),
        "@(1; 1; 1)"
    );
    // the row's own cell is still in reach of the body itself
    assert_eq!(
        ok(
            &d,
            &[
                "/books/* | %(title = ::t; pages = 2) @| # | map(:pages * 2; --to=:n) | $_:n",
                "books.json"
            ]
        ),
        "@(4; 4; 4)"
    );
}

/// The stream's record reading leaves out a field the `null=` option
/// names, as the whole reader does (front ends B10).
#[test]
fn a_streamed_record_honours_the_null_option() {
    let d = fixture("null");
    std::fs::write(d.join("na.csv"), "id,note\n1,NA\n2,ok\n").unwrap();
    let whole = ok(&d, &["--no-stream", "/row | json", "na.csv?null=NA"]);
    assert_eq!(ok(&d, &["/row | json", "na.csv?null=NA"]), whole);
    assert!(!whole.contains("NA"), "{whole}");
}

/// The streamed CSV refuses what the whole reader refuses, in its
/// words (front ends B3). Its open scan counted byte records and
/// validated nothing: a row holding a byte that is not UTF-8 read as
/// null, and the table of the rows lost it and doubled the next.
#[test]
fn a_streamed_csv_refuses_a_byte_that_is_not_utf8() {
    let d = fixture("latin1");
    std::fs::write(
        d.join("latin1.csv"),
        b"name,age\nAchilles,28\nH\xe9ctor,30\nPriam,70\nParis,25\n",
    )
    .unwrap();
    let whole = refused(&d, &["--no-stream", "/row::name", "latin1.csv"]);
    assert!(whole.contains("not valid UTF-8 at byte 22"), "{whole}");
    for q in ["/row::name", "/row @| #", "/row @| count"] {
        let streamed = refused(&d, &[q, "latin1.csv"]);
        assert!(
            streamed.contains("not valid UTF-8 at byte 22"),
            "{q}: {streamed}"
        );
    }
    // its encoding said, it reads
    assert_eq!(
        ok(&d, &["/row[2]::name", "latin1.csv?encoding=latin1"]),
        "H\u{e9}ctor"
    );
    // a file the whole reader decodes by its byte-order mark is the
    // whole reader's: the stream refused it ("invalid UTF-8 in field
    // 0"), so a file quai read was one qua refused
    let mut u16le: Vec<u8> = vec![0xFF, 0xFE];
    for unit in "a,b\n1,2\n".encode_utf16() {
        u16le.extend(unit.to_le_bytes());
    }
    std::fs::write(d.join("u16.csv"), u16le).unwrap();
    assert_eq!(ok(&d, &["/row::a", "u16.csv"]), "1");
}

/// A streamed JSON line that does not parse refuses the query that
/// reaches it, as the whole reading refuses the file (front ends
/// B4): it answered a node of nulls under the root's locator.
#[test]
fn a_streamed_json_line_that_does_not_parse_is_refused() {
    let d = fixture("jsonl");
    std::fs::write(
        d.join("bad.jsonl"),
        "{\"t\":\"Iliad\"}\n{\"t\":\"Odyssey\",,}\n{\"t\":\"Aeneid\"}\n",
    )
    .unwrap();
    let whole = refused(&d, &["--no-stream", "/*::t", "bad.jsonl"]);
    assert!(!whole.is_empty());
    let streamed = refused(&d, &["/*::t", "bad.jsonl"]);
    assert!(streamed.contains("line 2"), "{streamed}");
}

/// `?autounits` decides a column on its first cell that says
/// something, among the rows of its container (graph B9). The plan
/// was keyed by the names and sampled on whichever node asked first:
/// a CSV's root, which has no cells, so a text column converted and
/// read null; in a document, the first item asked for.
#[test]
fn autounits_decides_a_column_on_its_rows() {
    let d = fixture("autounits");
    // a text column named like a unit stays as it is
    std::fs::write(d.join("text.csv"), "len_km,b\nabc,1\nxyz,2\n").unwrap();
    assert_eq!(
        ok(&d, &["/row | %(::len; ::len_km)", "text.csv?autounits"]),
        "%(len = null; len_km = \"abc\")\n%(len = null; len_km = \"xyz\")"
    );
    // a numeric one converts, its first cell empty or not
    std::fs::write(d.join("num.csv"), "len_km,b\n,1\n5,2\n").unwrap();
    assert_eq!(
        ok(&d, &["/row[2] | %(::len; ::len_km)", "num.csv?autounits"]),
        "%(len = 5_km; len_km = null)"
    );
    // in a document the items of one container decide together,
    // whichever is asked for first: the first that says something is
    // text, so neither converts
    std::fs::write(
        d.join("items.json"),
        r#"{"items":[{"len_km":"abc"},{"len_km":5}]}"#,
    )
    .unwrap();
    let both = ok(
        &d,
        &["/items/* | %(::len; ::len_km)", "items.json?autounits"],
    );
    assert_eq!(
        both,
        "%(len = null; len_km = \"abc\")\n%(len = null; len_km = 5)"
    );
    assert_eq!(
        ok(
            &d,
            &["/items/*[2] | %(::len; ::len_km)", "items.json?autounits"]
        ),
        "%(len = null; len_km = 5)"
    );
}

/// A mounted document's own units are known to `convert` under the
/// mount's name as when the file is opened alone (front ends B8):
/// the mount forwarded the unit's scale and not its name, so the
/// parser refused "no unit 'U'".
#[test]
fn a_mounts_units_are_known_to_the_parser() {
    let d = fixture("mount-units");
    let racks = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/kaiv-units/racks.kaiv"
    );
    let q = "//*[::height]::height | convert(U)";
    let alone = ok(&d, &[q, racks]);
    assert!(alone.contains("_U"), "{alone}");
    assert_eq!(ok(&d, &[q, &format!("r={racks}")]), alone);
}

/// `pivot` refuses a null column key (ruling 2026-11-10): a column
/// has a title, and the null landed under the title "".
#[test]
fn pivot_refuses_a_null_column_key() {
    let d = fixture("pivot-null");
    let e = refused(
        &d,
        &["= @(%(r = 1; c = 'a'; v = 10); %(r = 1; c = null; v = 20)) | pivot(:r; :c; :v)"],
    );
    assert!(e.contains("null column key"), "{e}");
    assert_eq!(
        ok(
            &d,
            &[
                "= @(%(r = 1; c = 'a'; v = 10); %(r = 1; c = 'b'; v = 20)) | pivot(:r; :c; :v) | $_:a"
            ]
        ),
        "%(1 = 10)"
    );
}

/// Whether a file is written under another delimiter is decided on
/// the same first lines by both readers (ruling 2026-11-10): the
/// whole reader looked at every line and the stream at thirty-two,
/// so a file with one odd line further down was refused by one and
/// read, as one column, by the other.
#[test]
fn both_readers_decide_the_delimiter_on_the_same_lines() {
    let d = fixture("delimiter");
    let mut text = String::from("a;b\n");
    for i in 0..40 {
        text.push_str(if i == 35 { "x\n" } else { "1;2\n" });
    }
    std::fs::write(d.join("semi.csv"), text).unwrap();
    let streamed = refused(&d, &["/row @| count", "semi.csv"]);
    let whole = refused(&d, &["--no-stream", "/row @| count", "semi.csv"]);
    for e in [&streamed, &whole] {
        assert!(e.contains("?delimiter=semicolon"), "{e}");
    }
    assert_eq!(
        ok(&d, &["/row @| count", "semi.csv?delimiter=semicolon"]),
        "40"
    );
}

/// A numeric command over what it does not take (ruling 2026-11-10):
/// in a structure a cell of another kind stays as it is, where
/// `sqrt`, the logarithms, `exp` and `pow` made it null; a cell the
/// command applies to and fails on is null; a lone value of another
/// kind answers null, `round` included (it kept the text), and the
/// run warns once for the command, counting the values.
#[test]
fn a_numeric_command_leaves_a_cell_and_nulls_a_lone_value() {
    let d = fixture("numeric");
    // the cells of a list, of a record and of a table
    let (success, out, err) = qua(&d, &[r#"= @(16; "abc"; -1; true; null) | sqrt"#]);
    assert!(success, "{err}");
    assert_eq!(out, r#"@(4.0; "abc"; null; true; null)"#);
    assert!(!err.contains("NOT_NUMERIC"), "{err}");
    assert_eq!(
        ok(&d, &[r#"= %(a = 16; b = "abc") | sqrt"#]),
        r#"%(a = 4.0; b = "abc")"#
    );
    assert_eq!(
        ok(&d, &[r#"= @(2.5; "n/a"; 5_km) | exp | round(2)"#]),
        r#"@(12.18; "n/a"; 5_km)"#
    );
    for q in [
        r#"= #(@(t; n); %("Iliad"; 16); %("Emma"; "n/a")) | sqrt | $_[2]:n"#,
        r#"= #(@(t; n); %("Iliad"; 16); %("Emma"; "n/a")) | map(sqrt) | $_[2]:n"#,
        r#"= @(16; "n/a") | map(sqrt) | $_[2]"#,
    ] {
        let (success, out, err) = qua(&d, &[q]);
        assert!(success, "{q}: {err}");
        assert_eq!(out, "n/a", "{q}");
        assert!(!err.contains("NOT_NUMERIC"), "{q}: {err}");
    }
    // a lone value: null, and one line naming the command
    for (q, line) in [
        (
            r#"= "abc" | sqrt"#,
            r#"sqrt takes a number: 1 value is of another kind (text "abc") and answers null"#,
        ),
        (
            r#"= "n/a" | round"#,
            "round takes a number or a quantity: 1 value is of another kind (text \"n/a\") and \
             answers null",
        ),
        (
            "= 5_km | sqrt",
            "sqrt takes a number: 1 value is of another kind (a quantity 5_km) and answers null",
        ),
        (
            r#"= "abc" | map(sqrt)"#,
            r#"sqrt takes a number: 1 value is of another kind (text "abc") and answers null"#,
        ),
    ] {
        let (success, out, err) = qua(&d, &[q]);
        assert!(success, "{q}: {err}");
        assert_eq!(out, "null", "{q}");
        assert_eq!(err.matches("NOT_NUMERIC").count(), 1, "{q}: {err}");
        assert!(err.contains(line), "{q}: {err}");
    }
    // every thread is a lone value: one line for the stream
    let (success, out, err) = qua(&d, &["/books/* | ::t | sqrt", "books.json"]);
    assert!(success, "{err}");
    assert_eq!(out, "null\nnull\nnull");
    assert_eq!(err.matches("NOT_NUMERIC").count(), 1, "{err}");
    assert!(
        err.contains(
            "sqrt takes a number: 3 values are of another kind (the first: text \"Iliad\") and \
             answer null"
        ),
        "{err}"
    );
    // null is no kind, and a number that fails is no warning
    for q in ["= null | sqrt", "= -1 | sqrt"] {
        let (success, out, err) = qua(&d, &[q]);
        assert!(success, "{q}: {err}");
        assert_eq!(out, "null", "{q}");
        assert!(!err.contains("NOT_NUMERIC"), "{q}: {err}");
    }
    // `pow` with no exponent is refused whatever stands before it
    assert!(refused(&d, &[r#"= "abc" | pow"#]).contains("pow takes the exponent"));
}

/// A positional predicate in a hop operand counts per source node
/// (ruling 2026-11-10), as the hop does in a path: `edges(->*[1])`
/// kept the first edge of all the nodes together.
#[test]
fn a_hop_operands_position_counts_per_source_node() {
    let d = fixture("edges-position");
    std::fs::write(
        d.join("air.json"),
        r#"{"airports":[{"code":"ATH"},{"code":"ROM"},{"code":"ALX"}],
            "flights":[{"origin":"ATH","dest":"ROM"},{"origin":"ATH","dest":"ALX"},
                       {"origin":"ROM","dest":"ATH"},{"origin":"ROM","dest":"ALX"},
                       {"origin":"ALX","dest":"ATH"}]}"#,
    )
    .unwrap();
    std::fs::write(
        d.join("air.model"),
        "ref /flights/*::origin --> /airports/*[::code = $];\n\
         ref /flights/*::dest --> /airports/*[::code = $];\n\
         edge /flights/*: ::origin -- ::dest;\n",
    )
    .unwrap();
    let q = |query: &str| ok(&d, &["--model", "air.model", query, "air.json"]);
    assert_eq!(
        q("/airports/* @| edges(->*[1]) | #(:source; :target) @| csv"),
        "source,target\n/airports/0,/airports/1\n/airports/1,/airports/0\n/airports/2,/airports/0"
    );
    assert_eq!(q("/airports/* @| edges(->*[..2]) | dim"), "@(5; 6)");
    assert_eq!(q("/airports/*->*[1] @| count"), "3");
    // a predicate on the node arrived at is the same at every size
    assert_eq!(
        q(r#"/airports/* @| edges(->*[::code = "ALX"]) | dim"#),
        "@(2; 6)"
    );
}

/// An expression command evaluates its operand once: an operand of
/// one value ran a second time for that value, so a warning raised
/// in it was counted twice.
#[test]
fn an_expression_command_evaluates_its_operand_once() {
    let d = fixture("once");
    let (success, out, err) = qua(&d, &["/books/0 | (::t | sqrt)", "books.json"]);
    assert!(success, "{err}");
    assert_eq!(out, "null");
    assert!(err.contains("1 value is of another kind"), "{err}");
}

/// A colon in a source's column title is one name until a command
/// makes levels (ruling 2026-11-10). Any colon was a level of the
/// header, so the title `Time (HH:MM)` nested every row under
/// `Time (HH`, and `dc:title` under `dc`. The levels are a mark on
/// the table, set by the commands that make them.
#[test]
fn a_colon_in_a_sources_title_is_one_name() {
    let d = fixture("titles");
    std::fs::write(
        d.join("dc.csv"),
        "dc:title,dc:creator,Time (HH:MM),note\nIliad,Homer,10:30,x\nOdyssey,Homer,11:45,y\n",
    )
    .unwrap();
    let q = |query: &str| ok(&d, &[query, "dc.csv"]);
    // the rows and the columns are flat, with an index or without
    assert_eq!(
        q("/* @| # | json"),
        "[{\"dc:title\": \"Iliad\", \"dc:creator\": \"Homer\", \"Time (HH:MM)\": \"10:30\", \
         \"note\": \"x\"}, {\"dc:title\": \"Odyssey\", \"dc:creator\": \"Homer\", \
         \"Time (HH:MM)\": \"11:45\", \"note\": \"y\"}]"
    );
    assert_eq!(
        q("/* @| # | %"),
        "%(\"dc:title\" = @(\"Iliad\"; \"Odyssey\"); \"dc:creator\" = @(\"Homer\"; \"Homer\"); \
         \"Time (HH:MM)\" = @(\"10:30\"; \"11:45\"); note = @(\"x\"; \"y\"))"
    );
    assert_eq!(
        q("/* @| #(-i=:note) | %"),
        "%(\"dc:title\" = %(x = \"Iliad\"; y = \"Odyssey\"); \"dc:creator\" = %(x = \"Homer\"; \
         y = \"Homer\"); \"Time (HH:MM)\" = %(x = \"10:30\"; y = \"11:45\"))"
    );
    // the title reads whole, and its first part names no sub-table
    assert_eq!(q(r#"/* @| # | $_:"dc:title""#), r#"@("Iliad"; "Odyssey")"#);
    // (a read of a column the table lacks is refused, ruling 2026-11-11)
    let e = refused(&d, &["/* @| # | $_:dc", "dc.csv"]);
    assert!(
        e.contains("no column 'dc' (the columns: dc:title, dc:creator, Time (HH:MM), note)"),
        "{e}"
    );
    // `stack` and `transpose` take a title for one name: one `value`
    // column, one label column
    assert_eq!(q("/* @| #(-i=:note) | stack | dim"), "@(6; 3)");
    assert_eq!(q("/* @| #(-i=:note) | transpose | dim"), "@(3; 3)");
    // a label with a colon is one name in the transposed header
    assert_eq!(
        q(r#"/* @| #(-i=:"Time (HH:MM)") | transpose | @ | $_[1]"#),
        r#"%(column = "dc:title"; "10:30" = "Iliad"; "11:45" = "Odyssey")"#
    );

    // The commands that make levels: `unstack` over several columns,
    let long = r#"#(@(k; m; a; b); %("x"; "p"; 1; 2); %("x"; "q"; 3; 4); %("y"; "p"; 5; 6))"#;
    let wide = format!("= {long} | #(-i=@(:k; :m)) | unstack");
    let v = |tail: &str| ok(&d, &[&format!("{wide} {tail}")]);
    assert_eq!(
        v("| @"),
        "@%(@(k; a; b); %(\"x\"; %(p = 1; q = 3); %(p = 2; q = 4)); %(\"y\"; %(p = 5; q = null); \
         %(p = 6; q = null)))"
    );
    assert_eq!(v("| $_:a | dim"), "@(2; 3)");
    // kept by a selection of rows, by a selection of columns and by
    // `stack`, which undoes it
    assert_eq!(
        v("| $_[2..2] | @"),
        "%(k = \"y\"; a = %(p = 5; q = null); b = %(p = 6; q = null))"
    );
    assert_eq!(
        v(r#"| #(:k; :"a:p") | @"#),
        r#"@%(@(k; a); %("x"; %(p = 1)); %("y"; %(p = 5)))"#
    );
    assert_eq!(v("| stack | dim"), "@(4; 4)");
    // the transpose of a composite index, whose labels become paths,
    assert_eq!(
        ok(
            &d,
            &[&format!("= {long} | #(-i=@(:k; :m)) | transpose | @")]
        ),
        "@%(@(column; x; y); %(\"a\"; %(p = 1; q = 3); %(p = 5)); %(\"b\"; %(p = 2; q = 4); \
         %(p = 6)))"
    );
    // and a literal whose header writes a path (the user, the same
    // day: "bare path such as toplevel:col2"). A quoted name is one
    // name whatever it holds.
    assert_eq!(
        ok(&d, &["= #(@(bar:A; bar:B; baz); %(1; 2; 3)) | @"]),
        "%(bar = %(A = 1; B = 2); baz = 3)"
    );
    assert_eq!(
        ok(&d, &[r#"= #(@("bar:A"; "bar:B"; baz); %(1; 2; 3)) | @"#]),
        r#"%("bar:A" = 1; "bar:B" = 2; baz = 3)"#
    );
    // The printed table reads back as it was: a levelled column as
    // its path, a segment that is no bare word quoted, a title with
    // a colon quoted whole.
    for (written, printed) in [
        (
            "#(@(bar:A; bar:B; baz); %(1; 2; 3))",
            "#(@(bar:A; bar:B; baz);\n  %(1; 2; 3))",
        ),
        (
            r#"#(@(sales:2019; sales:"Q 1"; "New York":pop); %(1; 2; 3))"#,
            "#(@(sales:\"2019\"; sales:\"Q 1\"; \"New York\":pop);\n  %(1; 2; 3))",
        ),
        (
            r#"#(@("Time (HH:MM)"; note); %("10:30"; "x"))"#,
            "#(@(\"Time (HH:MM)\"; note);\n  %(\"10:30\"; \"x\"))",
        ),
    ] {
        assert_eq!(ok(&d, &[&format!("= {written}")]), printed, "{written}");
        assert_eq!(ok(&d, &[&format!("= {printed}")]), printed, "{written}");
    }
    assert_eq!(
        v(""),
        "#(@(k; a:p; a:q; b:p; b:q);\n  %(\"x\"; 1; 3; 2; 4);\n  %(\"y\"; 5; null; 6; null))"
    );
    // the canonical text keeps the path and the quotes
    assert_eq!(
        ok(&d, &[r#"= q(#(@(bar:A; baz<int>); %(1; 2))) | :unq"#]),
        "(#(@(bar:A; baz<int>); %(1; 2)))"
    );
    assert_eq!(
        ok(&d, &[r#"= q(#(@("Time (HH:MM)"; n); %(1; 2))) | :unq"#]),
        r#"(#(@("Time (HH:MM)"; n); %(1; 2)))"#
    );

    // A header has levels or names with colons, not both: the levels
    // are read off every colon, so the three makers refuse a colon
    // inside a segment, naming it.
    let e = refused(&d, &[r#"= #(@(bar:A; "Time (HH:MM)"); %(1; 2))"#]);
    assert!(
        e.contains(r#"the quoted name "Time (HH:MM)" holds one of its own"#),
        "{e}"
    );
    // (the way out a title's refusal names is the constructor's slug
    // option, which respells the names, as the user ruled; a label
    // is a cell, which the slugs do not touch, and is mapped)
    assert!(e.contains("| #(--slugs)"), "{e}");
    let e = refused(
        &d,
        &[r#"/* @| #(-i=@(:note; :"dc:creator")) | unstack"#, "dc.csv"],
    );
    assert!(
        e.contains("the title 'dc:title' holds a colon")
            && e.contains("the constructor's slug option (| #(--slugs))"),
        "{e}"
    );
    assert_eq!(
        q("/* @| #(--slugs) | #(-i=@(:note; :dc_creator)) | unstack | dim"),
        "@(2; 3)"
    );
    let e = refused(
        &d,
        &[
            r#"/* @| #(-i=@(:note; :"Time (HH:MM)")) | transpose"#,
            "dc.csv",
        ],
    );
    assert!(
        e.contains("the label '10:30' holds a colon")
            && e.contains("respell the labels first (map(…; --to=:::index))"),
        "{e}"
    );
    // a record array's header names fields, and takes no path
    let e = refused(&d, &["= @%(@(bar:A; baz); %(1; 2))"]);
    assert!(
        e.contains("a record array's header names the fields"),
        "{e}"
    );
}

/// The towns and legs of the walk tests: fares, hours and capacities
/// on each leg, two cheapest legs out of S at one fare, and a town V
/// behind X that the cheap way into X is too slow for.
fn legs(tag: &str) -> PathBuf {
    let d = fixture(tag);
    std::fs::write(
        d.join("legs.json"),
        r#"{"towns":[{"code":"S"},{"code":"X"},{"code":"A"},{"code":"T"},{"code":"B"},{"code":"V"}],
 "legs":[{"id":1,"a":"S","b":"X","fare":10,"duration":10,"cap":10},
         {"id":2,"a":"X","b":"T","fare":10,"duration":10,"cap":1},
         {"id":3,"a":"S","b":"A","fare":50,"duration":3,"cap":5},
         {"id":4,"a":"A","b":"T","fare":60,"duration":4,"cap":10},
         {"id":5,"a":"A","b":"X","fare":50,"duration":1,"cap":2},
         {"id":6,"a":"S","b":"B","fare":10,"duration":1,"cap":3},
         {"id":7,"a":"X","b":"V","fare":5,"duration":6,"cap":4}]}"#,
    )
    .unwrap();
    std::fs::write(
        d.join("legs.model"),
        "ref /legs/*::a --> /towns/*[::code = $];\n\
         ref /legs/*::b --> /towns/*[::code = $];\n\
         edge /legs/*: ::a -- ::b as leg weights(::fare<_EUR>; ::duration<_h>; ::cap<_t>);\n",
    )
    .unwrap();
    d
}

/// Under a budget the proximal marker keeps the survivors of least
/// total, the tie set, as the distal keeps those of greatest (ruling
/// 2026-11-10: "proximal should give multiple results only in case
/// of a tie"). Every node was answered at its own least total.
#[test]
fn the_proximal_marker_under_a_budget_keeps_the_lightest() {
    let d = legs("lightest");
    let q = |query: &str| {
        ok(
            &d,
            &[
                "--model",
                "legs.model",
                &format!(r#"/towns/*[::code = "S"]{query}"#),
                "legs.json",
            ],
        )
    };
    // two towns at the least fare: both, and no dearer one
    assert_eq!(q(r#"(->leg){*_EUR}[::code != "S"]? | ::code"#), "X\nB");
    assert_eq!(
        q(r#"(->leg){*_EUR}[::code != "S"]? | $-:total"#),
        "10_EUR\n10_EUR"
    );
    // the start stands at zero, and is the lightest where the guard
    // admits it
    assert_eq!(q("(->leg){*_EUR}? | ::code"), "S");
    // one town named: its lightest walk, as before
    assert_eq!(q(r#"(->leg){*_EUR}[::code = "T"]? | $-:total"#), "20_EUR");
    // two walks tie at the greatest (S > A > T and S > A > X > T):
    // the reach never merges, so both answer
    assert_eq!(
        q(r#"(->leg){*_EUR}[::code = "T"]! | $-:total"#),
        "110_EUR\n110_EUR"
    );
}

/// The search by total is exact for one brace alone (the review of
/// 2026-11-10, graph B1): it settled a node at its first arrival and
/// looked at no second brace, no count brace, no floor and no rule.
/// Every other form takes the exact walk and keeps its lightest.
#[test]
fn the_lightest_walk_honours_every_brace() {
    let d = legs("braces");
    let q = |query: &str| {
        ok(
            &d,
            &[
                "--model",
                "legs.model",
                &format!(r#"/towns/*[::code = "S"]{query}"#),
                "legs.json",
            ],
        )
    };
    // the cheap way into X is too slow to go on to V; the dear way
    // fits, and was never tried
    assert_eq!(
        q(r#"(->leg){;15_h}{*_EUR}[::code = "V"]? | $-:total"#),
        "105_EUR"
    );
    // a count brace: no walk of one hop reaches T (it answered the
    // walk of two)
    assert_eq!(q(r#"(->leg){;1}{*_EUR}[::code = "T"]? | $-:total"#), "");
    // a floor: the walk into X under it is not the answer
    assert_eq!(
        q(r#"(->leg){15_EUR;}[::code = "X"]? | $-:total"#),
        "100_EUR"
    );
    // `min:` falls along a walk: the least over the walks, by X > T
    // and by A > X > T alike, two walks and two answers
    assert_eq!(
        q(r#"(->leg){min:*_t}[::code = "T"]? | $-:total"#),
        "1_t\n1_t"
    );
    // At the quantifier bound a walk that could have gone on to a
    // lighter survivor refuses (the search ended in silence); an
    // answer nothing cut could undercut stands.
    let bound = |query: &str| {
        qua(
            &d,
            &[
                "--quantifier-bound=1",
                "--model",
                "legs.model",
                &format!(r#"/towns/*[::code = "S"]{query}"#),
                "legs.json",
            ],
        )
    };
    let (success, _, err) = bound(r#"(->leg){*_EUR}[::code = "T"]? | $-:total"#);
    assert!(!success);
    assert!(
        err.contains("the lightest walk would be silently incomplete"),
        "{err}"
    );
    let (success, out, err) = bound(r#"(->leg){*_EUR}[::code != "S"]? | ::code"#);
    assert!(success, "{err}");
    assert_eq!(out, "X\nB");
}

/// The set walk keeps one arrival per node, so it runs only where no
/// arrival can be told from another (the review, graph B2 and B3): a
/// guard that reads the walk makes the walk exact, and so does an
/// alternative of several hops.
#[test]
fn the_set_walk_is_for_guards_and_groups_it_cannot_change() {
    let d = legs("setwalk");
    // T is first reached by leg 2; the guard asks for leg 4, and the
    // one arrival kept was not it
    assert_eq!(
        ok(
            &d,
            &[
                "--model",
                "legs.model",
                r#"/towns/*[::code = "S"](->leg)+[$-::id = 4]? | ::code"#,
                "legs.json"
            ]
        ),
        "T"
    );
    // S -a-> P -b-> Q -a-> R -b-> P -a-> T -b-> U: the second
    // repetition comes back to P, which the first passed through, so
    // no acyclic walk goes past Q
    std::fs::write(
        d.join("ab.json"),
        r#"{"towns":[{"code":"S"},{"code":"P"},{"code":"Q"},{"code":"R"},{"code":"T"},{"code":"U"}],
 "legs":[{"a":"S","b":"P","kind":"a"},{"a":"P","b":"Q","kind":"b"},{"a":"Q","b":"R","kind":"a"},
         {"a":"R","b":"P","kind":"b"},{"a":"P","b":"T","kind":"a"},{"a":"T","b":"U","kind":"b"}]}"#,
    )
    .unwrap();
    std::fs::write(
        d.join("ab.model"),
        "ref /legs/*::a --> /towns/*[::code = $];\n\
         ref /legs/*::b --> /towns/*[::code = $];\n\
         edge /legs/*: ::a -- ::b as ::kind;\n",
    )
    .unwrap();
    let q = |query: &str| ok(&d, &["--model", "ab.model", query, "ab.json"]);
    let open = q(r#"/towns/*[::code = "S"](->a->b)+ @| collapse | ::code"#);
    assert_eq!(open, "Q");
    assert_eq!(
        open,
        q(r#"/towns/*[::code = "S"](->a->b){1;5} @| collapse | ::code"#)
    );
    assert_eq!(
        q(r#"/towns/*[::code = "S"](->a->b)+[::code = "U"]? | ::code"#),
        ""
    );
    assert_eq!(
        q(r#"/towns/*[::code = "S"][(->a->b)+[::code = "U"]] | ::code"#),
        ""
    );
}

/// Two rows on one leg are two edges, whatever shortcut the walk
/// takes (the review, graph B4 and B8). `@| collapse` gathers a
/// node's arriving edges into `@-`, and `collapse(-e)` is one thread
/// per node and arriving edge: the walk expanded by node for either
/// and kept one arrival. Under the acyclic promise the paths of a
/// round merged by source, label and target, the row left out.
#[test]
fn parallel_legs_stay_two_edges() {
    let d = fixture("parallel");
    std::fs::write(
        d.join("par.json"),
        r#"{"towns":[{"code":"S"},{"code":"T"},{"code":"U"}],
 "legs":[{"id":1,"a":"S","b":"T"},{"id":2,"a":"S","b":"T"},{"id":3,"a":"T","b":"U"}]}"#,
    )
    .unwrap();
    for (file, promise) in [("par.model", ""), ("par-acyclic.model", " acyclic")] {
        std::fs::write(
            d.join(file),
            format!(
                "ref /legs/*::a --> /towns/*[::code = $];\n\
                 ref /legs/*::b --> /towns/*[::code = $];\n\
                 edge /legs/*: ::a -- ::b as leg{promise};\n"
            ),
        )
        .unwrap();
    }
    let q = |model: &str, query: &str| {
        ok(
            &d,
            &[
                "--model",
                model,
                &format!(r#"/towns/*[::code = "S"]{query}"#),
                "par.json",
            ],
        )
    };
    for model in ["par.model", "par-acyclic.model"] {
        // the open walk and the bounded one gather the same edges
        for walk in ["(->leg)+", "(->leg){1;5}"] {
            assert_eq!(
                q(model, &format!("{walk} @| collapse | @-::id")),
                "@(1; 2)\n@(3)",
                "{model} {walk}"
            );
            assert_eq!(
                q(model, &format!("{walk} @| collapse(-e) @| count")),
                "3",
                "{model} {walk}"
            );
        }
        // a collapse nothing reads the edges of is still one thread
        // per node
        assert_eq!(q(model, "(->leg)+ @| collapse | ::code"), "T\nU", "{model}");
        // the promise is cost, not answers: the guard meets the second
        // row of the parallel leg under it too
        assert_eq!(
            q(model, "(->leg){1;2}[$-::id = 2] @| collapse | ::code"),
            "T",
            "{model}"
        );
    }
}

/// `&$.f!` and `&$.f#` apply the held quarq on the capsa as it stood
/// before the push (spec: Quarqs, the held quarq's appliers), through
/// every command of the body, and the body sees a regula it pushed
/// itself (ruling 2026-11-10; the review, pipeline F3 and F4). The
/// window was a mark on the thread that every command but an
/// expression dropped.
#[test]
fn a_held_quarq_applies_on_the_capsa_before_its_push() {
    let d = fixture("closure");
    let held = |body: &str, topic: &str, applier: &str| {
        ok(
            &d,
            &[&format!(
                "= 5 | .k(2) | .f(q({body})) | .k(3) | {topic} | &$.f{applier}"
            )],
        )
    };
    assert_eq!(held("$_ * $.k", "7", "!"), "14");
    // a command before the read (it read the applying capsa: 21, 15)
    assert_eq!(held("abs | $_ * $.k", "7", "!"), "14");
    assert_eq!(held("abs | $_ * $.k", "7", "#"), "10");
    assert_eq!(held("$_ @| sum | $_ * $.k", "7", "!"), "14");
    // the commands that read the register whole (3, and both regulae)
    assert_eq!(held("default($.k)", "null", "!"), "2");
    assert_eq!(held("%.", "7", "!"), "%(k = 2)");
    // the body's own push is in its reach (it read null), and lands
    // in the applying thread after its capsa
    assert_eq!(held(".g($_ * $.k) | $.g", "7", "!"), "14");
    assert_eq!(
        held(".g($_ * $.k) | $.g", "7", "! | %."),
        "%(k = 3; f = q(.g($_ * $.k) | $.g); g = 14)"
    );
    // the bare splice is the body written in place: the applying capsa
    assert_eq!(held("$_ * $.k", "7", ""), "21");

    // The photograph: a body that walks leaves its threads where its
    // walk ended (ruling 2026-11-10; every thread was put back on the
    // applying position, so the tags answered nulls) ...
    let q = |query: &str| ok(&d, &[query, "books.json"]);
    assert_eq!(
        q("/books/* | .f(q(/tags/*)) | &$.f# | ::"),
        "war\ngods\nsea\ngods\ncourt"
    );
    // ... and one that does not walk returns to where it applied
    assert_eq!(
        q("/books/*[1] | .f(q(::t | upper)) | /tags/* | &$.f# | %(v = $_; at = $$_)"),
        "%(v = \"ILIAD\"; at = /books/0/tags/0)\n%(v = \"ILIAD\"; at = /books/0/tags/1)"
    );
    // a push that stood on no node photographs no position (the root
    // was read off it)
    assert_eq!(q("/books/* | ::t @| count | .f(q($$_)) | &$.f#"), "null");
}

/// A spliced body runs as if written in place (ruling 2026-11-10):
/// on the whole context where every thread holds the same quarq, so
/// `@_` and an aggregation inside it see what they would see written
/// there. It ran thread by thread always.
#[test]
fn a_spliced_body_runs_in_place() {
    let d = fixture("splice");
    let q = |query: &str| ok(&d, &[query]);
    assert_eq!(q("= 1..3 | .f(q($_ + (@_ | count))) | &$.f"), "4\n5\n6");
    assert_eq!(q("= 1..3 | .f(q($_ @| sum)) | &$.f"), "6");
    assert_eq!(
        ok(
            &d,
            &["/books/* | .f(q(/tags/* @| count)) | &$.f", "books.json"]
        ),
        "5"
    );
    // threads that hold different quarqs are read each on its own
    assert_eq!(
        q("= 1..2 | .f(($_ = 1 ? q($_ @| count) : q($_ @| sum))) | &$.f"),
        "1\n2"
    );
    // and the appliers apply per thread, each on its own capsa
    assert_eq!(q("= 1..3 | .f(q($_ + (@_ | count))) | &$.f!"), "2\n3\n4");
    assert_eq!(q("= 1..3 | .f(q($_ @| sum)) | &$.f!"), "1\n2\n3");
}

/// The seven rulings of 2026-11-10 on the review's rows that waited
/// for one (stdlib B10, value T4a, T4b, T5a and their kin).
#[test]
fn the_seven_rulings_on_tables_and_casts() {
    let d = fixture("seven");
    let q = |query: &str| ok(&d, &[query]);
    let t = r#"#(@(date; k; A; B); %(2013-01-01; "x"; 1; 10); %(2013-01-02; "y"; 2; 20)) | #(-i=:date)"#;

    // (1) `melt` on an indexed table: an id named beside the index
    // is an id too (it was melted as one more variable)
    assert_eq!(
        q(&format!("= {t} | melt(k)")),
        "#(@(date; variable; k; value);\n  %(2013-01-01; \"A\"; \"x\"; 1);\n  \
         %(2013-01-01; \"B\"; \"x\"; 10);\n  %(2013-01-02; \"A\"; \"y\"; 2);\n  \
         %(2013-01-02; \"B\"; \"y\"; 20))"
    );
    assert_eq!(
        q(&format!("= {t} | melt(k) | :::index | [1]")),
        r#"%(date = 2013-01-01; variable = "A")"#
    );

    // (2) `clean`: an uppercase K is the thousand, as the lowercase
    // is; the kelvin under the scientific reading alone (the ruling
    // revised the same day: "with ?sci K is kelvin, without it it's
    // the current behavior"), read there by the unit rung
    assert_eq!(
        q(r#"= @("300K"; "300 K"; "300k"; "2M"; "5 m") | clean"#),
        "@(300000; 300000; 300000; 2000000; 5_m)"
    );
    assert_eq!(
        q(r#"= @("300K"; "300 K"; "300k"; "2M"; "5 m") | clean(--sci)"#),
        "@(300_K; 300_K; 300000; 2000000; 5_m)"
    );

    // (3) `| #(…)` keeps the index unless `-i` says otherwise; the
    // bare `| #` drops the role
    let labels = "@(2013-01-01; 2013-01-02)";
    assert_eq!(q(&format!("= {t} | #(-c=:A<float>) | :::index")), labels);
    assert_eq!(q(&format!("= {t} | #(--name=tbl) | :::index")), labels);
    assert_eq!(q(&format!("= {t} | #(-i=:k) | :::index")), r#"@("x"; "y")"#);
    assert_eq!(q(&format!("= {t} | #(-i=null) | :::index")), "null");
    assert_eq!(q(&format!("= {t} | # | :::index")), "null");

    // (4) text of integer form past the integer bound stays text (as
    // a float two identifiers that differ in the last digit were
    // equal); a literal stays the float, a text within the bound the
    // integer
    assert_eq!(
        q(r#"= "12345678901234567890" = "12345678901234567891""#),
        "false"
    );
    assert_eq!(q(r#"= #(@(a); %("12345678901234567890")) | type"#), "text");
    assert_eq!(q("= 12345678901234567890 | type"), "float");
    assert_eq!(q(r#"= "9223372036854775807" + 0"#), "9223372036854775807");

    // (5) a declared `<int>` takes the text of a whole-valued float,
    // as it takes the float; an undeclared column of such text is a
    // float column still
    assert_eq!(
        q(r#"= #(@(a<int>); %("3.0"); %(" 4 "); %(5.0)) | $_:a"#),
        "@(3; 4; 5)"
    );
    assert!(refused(&d, &[r#"= #(@(a<int>); %("3.5"))"#]).contains("declared int"));
    assert_eq!(q(r#"= #(@(a); %("3.0")) | type"#), "float");

    // (6) a declared time unit reads ISO duration text, as
    // `<duration>` does; another dimension does not
    assert_eq!(
        q(r#"= #(@(a<_h>); %("PT1H"); %("90 min"); %(2)) | $_:a"#),
        "@(1_h; 1.5_h; 2_h)"
    );
    assert!(refused(&d, &[r#"= #(@(a<_km>); %("PT1H"))"#]).contains("declared _km"));

    // (7) the fifteen-digit rule takes conversion noise out of a
    // computed magnitude and leaves an exact whole one alone
    assert_eq!(
        q("= (1234567890123450_B + 7_B) | magnitude"),
        "1234567890123457"
    );
    assert_eq!(q("= 1234567890123450_B + 7_B"), "1234567890123457_B");
    assert_eq!(q("= 0.1_km + 0.2_km"), "0.3_km");
    assert_eq!(q("= 10_m div 3"), "3.33333333333333_m");
}

/// A nested predicate inside a subquery sees the outer thread's
/// captures, as it sees that thread's regulae (ruling 2026-11-10),
/// and the captures a nested navigation makes are its own (the
/// review, graph B13 and pipeline F7). The captures were one map by
/// node for the whole run: a nested predicate read the stale capture
/// of its candidate node, and a nested path's captures landed on the
/// outer step's threads.
#[test]
fn a_nested_predicate_reads_the_outer_threads_captures() {
    let d = fixture("captures");
    let q = |query: &str| ok(&d, &[query, "books.json"]);
    // wherever the match was written, in the step or after a pipe
    for matched in [
        r"/books/*[::t == (/^(\w)/)]",
        r"/books/* | [::t == (/^(\w)/)]",
    ] {
        assert_eq!(
            q(&format!(r#"{matched} | (^/books/*[$1 = "O"] @| count)"#)),
            "0\n3\n0",
            "{matched}"
        );
    }
    assert_eq!(
        q(r#"/books/*[::author == (/^(\w)/)] | (^/books/*[$1 = "H"] @| count)"#),
        "3\n3\n0"
    );
    // a named group, by the record of them
    assert_eq!(
        q(r#"/books/*[::t == (/^(?<first>\w)/)] | (^/books/*[%+:first = "O"] @| count)"#),
        "0\n3\n0"
    );
    // a path from the thread's own node, as an operand and as the
    // navigation command after a pipe
    assert_eq!(
        q(r#"/books/*[::t == (/^(\w)/)] | (/tags/*[$1 = "I"] @| count)"#),
        "2\n0\n0"
    );
    assert_eq!(
        q(r#"/books/*[::t == (/^(\w)/)] | /tags/*[$1 = "I"] | ::"#),
        "war\ngods"
    );
    // the captures of a nested path stay in it: the outer step made
    // none, and the outer thread keeps its own
    assert_eq!(
        q(r"/books/*[(^/books/*[::t == (/^(Il|Od)/)] @| count) > 0] | $1"),
        "null\nnull\nnull"
    );
    assert_eq!(
        q(r"/books/*[::t == (/^(\w)/)] | (^/books/*[::t == (/^(O)/)] @| count) | $1"),
        "I\nO\nE"
    );
    // a step's later predicate reads its own capture, across a
    // positional one, as before
    assert_eq!(
        q(r#"/books/*[::t == (/^(\w)/)][1][$1 = "I"] | ::t"#),
        "Iliad"
    );
}

/// The table of every node at its own least total asks the question
/// once per node: the reach is taken per starting thread, so the
/// query forks first and each thread reaches for the one target
/// (the user's form for what the bare `{*_km}?` answered before the
/// ruling of 2026-11-10). One target per thread, over a directed
/// link walked back and under a cost function too. Where two walks
/// from one start tie, both answer (the reach never merges): from A
/// the leg to T and the way by X are 60 EUR each, two rows.
#[test]
fn a_fork_then_a_reach_is_the_table_of_least_totals() {
    let d = legs("fork");
    let q = |query: &str| ok(&d, &["--model", "legs.model", query, "legs.json"]);
    assert_eq!(
        q(r#"/towns/* | .t(::code) | (<-leg){*_EUR}[::code = "S"]? | %(to = $.t; d = $-:total)"#),
        "%(to = \"S\"; d = null)\n%(to = \"X\"; d = 10_EUR)\n%(to = \"A\"; d = 50_EUR)\n\
         %(to = \"T\"; d = 20_EUR)\n%(to = \"B\"; d = 10_EUR)\n%(to = \"V\"; d = 15_EUR)"
    );
    assert_eq!(
        q(r#"/towns/* | .t(::code) | (->leg){*_EUR}[::code = "T"]? | %(from = $.t; d = $-:total)"#),
        "%(from = \"S\"; d = 20_EUR)\n%(from = \"X\"; d = 10_EUR)\n%(from = \"A\"; d = 60_EUR)\n\
         %(from = \"A\"; d = 60_EUR)\n%(from = \"T\"; d = null)"
    );
    // the widest way to each, the least capacity along it
    assert_eq!(
        q(r#"/towns/* | .t(::code) | (<-leg){min:*_t}[::code = "S"]! | %(to = $.t; d = $-:total)"#),
        "%(to = \"S\"; d = null)\n%(to = \"X\"; d = 10_t)\n%(to = \"A\"; d = 5_t)\n\
         %(to = \"T\"; d = 5_t)\n%(to = \"B\"; d = 3_t)\n%(to = \"V\"; d = 4_t)"
    );
}

/// The fold by a regula pushes nothing on the folded thread (ruling
/// 2026-11-10; the review, pipeline F18): the folding regula stays
/// where it was pushed, once, and a reduction keeps it as it keeps
/// what its inputs share. It was pushed again on top, so the capsa
/// showed it twice and held more regulae than the text files.
#[test]
fn a_fold_leaves_its_regula_where_it_was_pushed() {
    let d = fixture("fold-key");
    let q = |query: &str| ok(&d, &[query]);
    // a regula pushed after the key stays on top; the key is read by
    // its name
    assert_eq!(
        q("= 1..4 | .g($_ mod 2) | .h(5) @|{.g} count | @."),
        "@(1; 5)\n@(0; 5)"
    );
    assert_eq!(
        q("= 1..4 | .g($_ mod 2) | .h(5) @|{.g} count | %(n = $_; g = $.g; top = $.)"),
        "%(n = 2; g = 1; top = 5)\n%(n = 2; g = 0; top = 5)"
    );
    // the same under a record constructor, which never doubled it
    assert_eq!(
        q("= 1..4 | .g($_ mod 2) | .h(5) @|{.g} %(all = @.)"),
        "%(all = @(1; 5))\n%(all = @(0; 5))"
    );
    // under `@|{.}` the key is the top by construction
    assert_eq!(
        q("= 1..4 | .g($_ mod 2) @|{.} count | %(n = $_; top = $.)"),
        "%(n = 2; top = 1)\n%(n = 2; top = 0)"
    );
    // a command that keeps its threads: each keeps its one regula
    assert_eq!(
        q("= 1..4 | .g($_ mod 2) @|{.g} top(1; $_) | %(v = $_; all = @.)"),
        "%(v = 3; all = @(1))\n%(v = 4; all = @(0))"
    );
    // the tuple fold pushes neither again
    assert_eq!(
        q("= 1..4 | .who($_ mod 2) | .w($_ > 2) @|{.who; .w} count | %."),
        "%(who = 1; w = false)\n%(who = 0; w = false)\n%(who = 1; w = true)\n%(who = 0; w = true)"
    );
    // keys that are one key written two ways are one group, and the
    // group holds the first of them
    assert_eq!(
        q("= 1..4 | .g(($_ = 1 ? 1 : ($_ = 2 ? 1.0 : 2))) @|{.g} count | %(n = $_; g = $.g)"),
        "%(n = 2; g = 1)\n%(n = 2; g = 2)"
    );
}

/// The refusal of a conversion between two currencies points at the
/// way on, a stated rate, as the other refusals point at their
/// remedy; any other pair of unlike dimensions is named and no more.
#[test]
fn a_conversion_between_currencies_points_at_exchange() {
    let d = fixture("currencies");
    let e = refused(&d, &["= 12_EUR | convert(USD)"]);
    assert!(
        e.contains(
            "convert(USD): a quantity converts within its own dimension, and these are unlike \
             (EUR and USD): two currencies have no fixed rate; state one, as in \
             exchange(%(EUR = 1.08_USD); USD)"
        ),
        "{e}"
    );
    // the form it points at answers
    assert_eq!(
        ok(&d, &["= 12_EUR | exchange(%(EUR = 1.08_USD); USD)"]),
        "12.96_USD"
    );
    // a price per mass: the exchange takes the currency, the
    // conversion follows it
    let e = refused(&d, &["= 12_EUR/kg | convert(USD/lb)"]);
    assert!(e.contains("exchange(%(EUR = 1.08_USD); USD)"), "{e}");
    assert_eq!(
        ok(
            &d,
            &["= 12_EUR/kg | exchange(%(EUR = 1.08_USD); USD) | convert(USD/kg)"]
        ),
        "12.96_USD/kg"
    );
    let e = refused(&d, &["= 12_km | convert(kg)"]);
    assert!(
        e.contains("are unlike (m and kg)") && !e.contains("exchange"),
        "{e}"
    );
}

/// Two indexes that meet agree on a type before their labels are
/// compared (the user's ruling of 2026-11-10): a declared index
/// wins over an undeclared one and refuses a label that does not
/// cast; two undeclared ones are upcast to their common type; two
/// declared ones are the same type, or an integer with a float.
/// The labels were compared as values across the two columns' types,
/// so 1 and "1" were two labels and a declaration bound one side.
#[test]
fn two_indexes_agree_on_a_type_where_they_meet() {
    let d = fixture("index-types");
    let q = |query: &str| ok(&d, &[query]);
    let no = |query: &str| refused(&d, &[query]);
    let a = r#"(#(@(k; a); %(1; 10); %(2; 20)) | #(-i=:k))"#;
    let text = r#"(#(@(k; b); %("1"; "x"); %("z"; "y")) | #(-i=:k))"#;
    let int = r#"(#(@(k; c); %(1; 100); %(3; 300)) | #(-i=:k<int>))"#;
    let float = r#"(#(@(k; d); %(1.0; 0.5); %(2.5; 0.7)) | #(-i=:k<float>))"#;
    let declared_text = r#"(#(@(k; e); %("1"; "p"); %("q"; "r")) | #(-i=:k<text>))"#;

    // two undeclared: integers and text meet as text, so 1 is "1"
    assert_eq!(
        q(&format!("= {a} ++ {text}")),
        "#(@(k; a; b);\n  %(\"1\"; 10; \"x\");\n  %(\"2\"; 20; null);\n  %(\"z\"; null; \"y\"))"
    );
    // a declared one wins, and the result's index is declared
    assert_eq!(
        q(&format!("= {a} ++ {int} | header")),
        "@(:k<int>; :a<*; int>; :c<*; int>)"
    );
    let e = no(&format!("= {text} ++ {int}"));
    assert!(
        e.contains(
            "the indexes meet as int, the declared one, and the label \"z\" of 'k' does not \
             cast to it"
        ),
        "{e}"
    );
    // two declared: an integer with a float are floats
    assert_eq!(
        q(&format!("= {int} ++ {float}")),
        "#(@(k; c; d);\n  %(1.0; 100; 0.5);\n  %(3.0; 300; null);\n  %(2.5; null; 0.7))"
    );
    assert_eq!(
        q(&format!("= {int} ++ {float} | header")),
        "@(:k<float>; :c<*; int>; :d<*; float>)"
    );
    let e = no(&format!("= {int} ++ {declared_text}"));
    assert!(
        e.contains(
            "the indexes meet as int ('k') and text ('k'): two declared indexes align where \
             their types are the same, or an integer with a float"
        ),
        "{e}"
    );

    // the same wherever two indexes meet: rows appended under one
    // header and the update by label,
    let more = r#"(#(@(k; a); %("1"; 30); %("z"; 40)) | #(-i=:k))"#;
    assert_eq!(
        q(&format!("= {a} ++! {more}")),
        "#(@(k; a);\n  %(\"1\"; 30);\n  %(\"2\"; 20);\n  %(\"z\"; 40))"
    );
    assert_eq!(
        q(&format!(
            "= (#(@(k; a); %(1; 10)) | #(-i=:k<int>)) ++ (#(@(k; a); %(2.5; 30)) | \
             #(-i=:k<float>)) | header"
        )),
        "@(:k<float>; :a<*; int>)"
    );
    // an operator between two tables,
    assert_eq!(
        q(&format!("= {a} + {more}")),
        "#(@(k; a);\n  %(\"1\"; 40);\n  %(\"2\"; null);\n  %(\"z\"; null))"
    );
    // and `correlate`, whose key column stands as the side it is
    let e = no(&format!(
        r#"= {int} | .u | #(@(id; n); %("1"; "a"); %("zz"; "b")) | correlate($.u; --on=:id)"#
    ));
    assert!(
        e.contains("the label \"zz\" of 'id' does not cast to it"),
        "{e}"
    );
    assert_eq!(
        q(&format!(
            r#"= {int} | .u | #(@(id; n); %(1; "a"); %(2; "b")) | correlate($.u; --on=:id)"#
        )),
        "#(@(id; n; c);\n  %(1; \"a\"; 100);\n  %(2; \"b\"; null))"
    );
}

/// A model declaration whose query is wrong raises an error (the
/// user, 2026-11-10: "wrong query should raise an error"): at load
/// where the query does not parse or names what the engine does not
/// have, and at the query that reaches it where it is refused when
/// it runs. Every place that ran a declaration's query took its
/// failure for an empty answer, so the model loaded in silence and
/// the container, the scope or the index was empty.
#[test]
fn a_models_wrong_query_is_refused() {
    let d = fixture("model-queries");
    std::fs::write(
        d.join("t.json"),
        r#"{"towns":[{"code":"S"},{"code":"T"}],"legs":[{"a":"S","b":"T"}]}"#,
    )
    .unwrap();
    let with = |model: &str, query: &str| {
        std::fs::write(d.join("m.model"), model).unwrap();
        qua(&d, &["--model", "m.model", query, "t.json"])
    };
    // at load, naming the statement, whatever the query asked is
    for (model, named) in [
        // (the statement's `;` is not the query's: it was quoted
        // inside it where the bracket was left open)
        (
            "node /codes/code: /towns/*[::code = ;\n",
            "node /codes/code: the query '/towns/*[::code =' is refused",
        ),
        (
            "node /codes/code: /towns/* | ::code | nosuchcmd;\n",
            "node /codes/code: the query '/towns/* | ::code | nosuchcmd' is refused",
        ),
        (
            "index /towns/*[::code;\n",
            "the query '/towns/*[' is refused",
        ),
    ] {
        let (success, out, err) = with(model, "/towns/* @| count");
        assert!(!success, "{model} answered {out}");
        assert!(err.contains(named), "{model}: {err}");
    }
    // a query that parses and is refused when it runs refuses the
    // query that builds the model's containers, in its words: the
    // first navigation over the model, whatever it asks for
    let model = "node /codes/code: /towns/* | (5_km + 5);\n";
    for query in ["/codes/* @| count", "/towns/* @| count"] {
        let (success, out, err) = with(model, query);
        assert!(!success, "{query} answered {out}");
        assert!(
            err.contains(
                "the model's query '/towns/* | (5_km + 5)' is refused: the sum of a quantity \
                 and a number without a unit"
            ),
            "{query}: {err}"
        );
    }
    // a sound model is untouched
    let (success, out, err) = with(
        "ref /legs/*::a --> /towns/*[::code = $];\n\
         ref /legs/*::b --> /towns/*[::code = $];\n\
         edge /legs/*: ::a -- ::b as leg;\n",
        "/towns/*->leg @| count",
    );
    assert!(success, "{err}");
    assert_eq!(out, "1");
}

/// Under a reach selector every walk tied at the best total is an
/// answer, two walks to one node included (the user, 2026-11-10:
/// "reporting one result when two give the same cost is wrong"; a
/// plain range brace breaks no tie, and no internal order does). A
/// group that records its way is searched like any other: a route
/// query `(->leg .(::code)){*_EUR}[P]?` was sent to the exhaustive
/// walk by the batch before and refused at the frontier bound over
/// the flights.
#[test]
fn the_lightest_reach_answers_every_tied_walk() {
    let d = legs("tied-walks");
    let q = |query: &str| ok(&d, &["--model", "legs.model", query, "legs.json"]);
    // a recorded route under one brace: the search, the one walk
    assert_eq!(
        q(
            r#"/towns/*[::code = "S"](->leg .(::code)){*_EUR}[::code = "T"]? | %(route = (@. | join(" > ")); t = $-:total)"#
        ),
        r#"%(route = "X > T"; t = 20_EUR)"#
    );
    // two walks of one total to one node
    std::fs::write(
        d.join("tie.json"),
        r#"{"towns":[{"code":"S"},{"code":"P"},{"code":"Q"},{"code":"T"}],
 "legs":[{"a":"S","b":"P","fare":1,"duration":1,"cap":1},
         {"a":"S","b":"Q","fare":1,"duration":1,"cap":1},
         {"a":"P","b":"T","fare":1,"duration":1,"cap":1},
         {"a":"Q","b":"T","fare":1,"duration":1,"cap":1}]}"#,
    )
    .unwrap();
    let tie = |query: &str| ok(&d, &["--model", "legs.model", query, "tie.json"]);
    let route = r#"| %(route = (@. | join(" > ")); t = $-:total)"#;
    let both = "%(route = \"P > T\"; t = 2_h)\n%(route = \"Q > T\"; t = 2_h)";
    // under two braces (the exact walk) and under one (the search by
    // total), the same two routes
    for braces in ["{;10_EUR}{*_h}", "{*_h}"] {
        assert_eq!(
            tie(&format!(
                r#"/towns/*[::code = "S"](->leg .(::code)){braces}[::code = "T"]? {route}"#
            )),
            both,
            "{braces}"
        );
    }
    // the form without a marker answers every walk, as before
    assert_eq!(
        tie(&format!(
            r#"/towns/*[::code = "S"](->leg .(::code)){{;10_EUR}}{{*_h}}[::code = "T"] {route}"#
        )),
        both
    );
    // two nodes at the least total are two answers
    assert_eq!(
        tie(r#"/towns/*[::code = "S"](->leg .(::code)){;10_EUR}{*_h}[::code != "S"]? | ::code"#),
        "P\nQ"
    );
    // the reach never merges (the user, the same day): two walks
    // that record nothing are two threads still, as the plain walk
    // has them, and print alike
    for braces in ["{;10_EUR}{*_h}", "{*_h}"] {
        assert_eq!(
            tie(&format!(
                r#"/towns/*[::code = "S"](->leg){braces}[::code = "T"]? | ::code"#
            )),
            "T\nT",
            "{braces}"
        );
    }
}

/// The casts of an alignment and of a transposition are told (the
/// user, 2026-11-10: "make sure the upcasting and the index merging
/// … is always relayed to the user via an info diagnostics", "the
/// upcasting on transpose too"): one info per command, naming what
/// met, as what, and what it became; nothing where nothing is cast.
#[test]
fn the_casts_of_an_alignment_and_a_transposition_are_told() {
    let d = fixture("cast-infos");
    let infos = |query: &str| {
        let (success, _, err) = qua(&d, &["--info", query]);
        assert!(success, "{query}: {err}");
        err.lines()
            .filter(|l| l.starts_with("info INDEX_CAST") || l.starts_with("info TRANSPOSE_CAST"))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let a = r#"(#(@(k; a); %(1; 10); %(2; 20)) | #(-i=:k))"#;
    let text = r#"(#(@(k; b); %("1"; "x"); %("z"; "y")) | #(-i=:k))"#;
    let int = r#"(#(@(k; c); %(1; 100); %(3; 300)) | #(-i=:k<int>))"#;
    let float = r#"(#(@(k; d); %(1.0; 0.5); %(2.5; 0.7)) | #(-i=:k<float>))"#;
    // two undeclared indexes upcast to their common type
    assert_eq!(
        infos(&format!("= {a} ++ {text}")),
        [
            "info INDEX_CAST (pipeline): ++: the indexes 'k' (int) and 'k' (text) met as text: 2 \
          labels cast"
        ]
    );
    // a declared one wins
    assert_eq!(
        infos(&format!(
            "= (#(@(k; a); %(1.0; 1); %(2.0; 2)) | #(-i=:k)) ++ {int}"
        )),
        [
            "info INDEX_CAST (pipeline): ++: the index 'k' (float) met the declared index 'k' (int) \
          and was cast to it: 2 labels cast"
        ]
    );
    // two declared, an integer with a float
    assert_eq!(
        infos(&format!("= {int} ++ {float}")),
        [
            "info INDEX_CAST (pipeline): ++: the declared indexes 'k' (int) and 'k' (float) met as \
          float: 2 labels cast"
        ]
    );
    // every aligning form names itself
    assert_eq!(
        infos(&format!(
            "= {a} + (#(@(k; a); %(1.0; 1); %(2.5; 2)) | #(-i=:k))"
        )),
        [
            "info INDEX_CAST (pipeline): +: the indexes 'k' (int) and 'k' (float) met as float: 2 \
          labels cast"
        ]
    );
    assert!(
        infos(&format!(
            r#"= {a} ++! (#(@(k; a); %("1"; 30); %("z"; 40)) | #(-i=:k))"#
        ))[0]
            .starts_with("info INDEX_CAST (pipeline): ++!: ")
    );
    assert!(
        infos(&format!(
            r#"= {text} | .u | #(@(id; n); %(1; "a"); %(2; "b")) | correlate($.u; --on=:id)"#
        ))[0]
            .starts_with(
                "info INDEX_CAST (pipeline): correlate: the indexes 'id' (int) and 'k' (text)"
            )
    );
    // told once for a command that runs on every thread
    assert_eq!(infos(&format!("= 1..3 | ({a} ++ {text}) | dim")).len(), 1);
    // nothing is cast, nothing is told: one type, or a declaration
    // the labels already satisfy
    assert!(infos(&format!("= {a} ++ (#(@(k; z); %(1; 5)) | #(-i=:k))")).is_empty());
    assert!(infos(&format!("= {a} ++ {int}")).is_empty());

    // a transposition brings the cells of columns of several kinds
    // to their common type, named in the columns' order
    assert_eq!(
        infos(r#"= #(@(n; x; s); %(1; 2.5; "a"); %(2; 3.5; "b")) | transpose"#),
        [
            "info TRANSPOSE_CAST (pipeline): transpose: the cells of 3 columns (int, float, text) \
          were brought to text"
        ]
    );
    assert_eq!(
        infos("= #(@(b; n); %(true; 2); %(false; 3)) | transpose"),
        [
            "info TRANSPOSE_CAST (pipeline): transpose: the cells of 2 columns (bool, int) were \
          brought to int"
        ]
    );
    assert!(infos("= #(@(n; m); %(1; 2); %(3; 4)) | transpose").is_empty());
    // an array too, and the index stays out of the count
    assert_eq!(infos(r#"= #(@(1; "a"); @(2; "b")) | transpose"#).len(), 1);
    assert_eq!(
        infos(r#"= #(@(k; n; s); %("p"; 1; "a"); %("q"; 2; "b")) | #(-i=:k) | transpose"#),
        [
            "info TRANSPOSE_CAST (pipeline): transpose: the cells of 2 columns (int, text) were \
          brought to text"
        ]
    );
}

/// The user's answer of 2026-11-10 on the header's reading ("agreed"
/// to the nested list per level): `| header` on a header with levels
/// answers the columns under an outer name in a list of their own,
/// `--header=` takes that shape back, `fields` nests as `header`
/// does without the types and `keys` names the outer names. A table
/// without levels answers the flat list it did.
#[test]
fn a_header_with_levels_reads_nested() {
    let d = fixture("nested-header");
    let t = "#(@(bar:A; bar:B; baz); %(1; 2; 3); %(4; 5; 6))";
    assert_eq!(
        ok(&d, &[&format!("= {t} | header")]),
        "@(:bar:@(:A<*; int>; :B<*; int>); :baz<*; int>)"
    );
    assert_eq!(
        ok(&d, &[&format!("= {t} | fields")]),
        "@(:bar:@(:A; :B); :baz)"
    );
    assert_eq!(ok(&d, &[&format!("= {t} | keys")]), r#"@("bar"; "baz")"#);
    // the outer names are those of the nested record the columns give
    assert_eq!(
        ok(&d, &[&format!("= {t} | % | keys")]),
        r#"@("bar"; "baz")"#
    );
    // deeper levels nest level by level
    assert_eq!(
        ok(
            &d,
            &["= #(@(a:x:p; a:x:q; a:y; b); %(1; 2; 3; 4)) | header"]
        ),
        "@(:a:@(:x:@(:p<*; int>; :q<*; int>); :y<*; int>); :b<*; int>)"
    );
    // a header lists its columns in their order: the columns of one
    // outer name that do not stand side by side stay two groups, and
    // `keys` names the outer name once
    let apart = "#(@(a:x; b; a:y); %(1; 2; 3))";
    assert_eq!(
        ok(&d, &[&format!("= {apart} | header")]),
        "@(:a:@(:x<*; int>); :b<*; int>; :a:@(:y<*; int>))"
    );
    assert_eq!(
        ok(&d, &[&format!("= {apart} | fields")]),
        "@(:a:@(:x); :b; :a:@(:y))"
    );
    assert_eq!(ok(&d, &[&format!("= {apart} | keys")]), r#"@("a"; "b")"#);
    // a table without levels: the flat list, a quoted name with a
    // colon one name
    assert_eq!(
        ok(&d, &["= #(@(a; b); %(1; 2)) | header"]),
        "@(:a<*; int>; :b<*; int>)"
    );
    assert_eq!(ok(&d, &["= #(@(a; b); %(1; 2)) | fields"]), "@(:a; :b)");
    assert_eq!(
        ok(&d, &[r#"= #(@("bar:A"; baz); %(1; 2)) | header"#]),
        "@(:'bar:A'<*; int>; :baz<*; int>)"
    );

    // `--header=` takes the shape back: written, over a flat list
    assert_eq!(
        ok(
            &d,
            &["= @(1; 2; 3; 4; 5; 6) | #(--header=@(:bar:@(:A<int>; :B<float>); :baz))"]
        ),
        "#(@(bar:A; bar:B; baz);\n  %(1; 2.0; 3);\n  %(4; 5.0; 6))"
    );
    assert_eq!(
        ok(
            &d,
            &["= @(1; 2; 3) | #(--header=@(:bar:@(:A<int>; :B<float>); :baz)) | @"]
        ),
        "%(bar = %(A = 1; B = 2.0); baz = 3)"
    );
    // written, over a table: its columns chosen and cast, the
    // declarations read back as written
    assert_eq!(
        ok(
            &d,
            &[&format!(
                "= {t} | #(--header=@(:bar:@(:A<int>; :B<float>); :baz<text>)) | header"
            )]
        ),
        "@(:bar:@(:A<int>; :B<float>); :baz<text>)"
    );
    // computed: what `header` answers builds the same table, the
    // columns apart included
    assert_eq!(
        ok(
            &d,
            &[&format!(
                "= {t} | .h(header) | @(7; 8; 9) | #(--header=$.h)"
            )]
        ),
        "#(@(bar:A; bar:B; baz);\n  %(7; 8; 9))"
    );
    assert_eq!(
        ok(
            &d,
            &[&format!(
                "= {apart} | .h(header) | @(7; 8; 9) | #(--header=$.h)"
            )]
        ),
        "#(@(a:x; b; a:y);\n  %(7; 8; 9))"
    );
    // the option reads back as written
    assert_eq!(
        ok(
            &d,
            &["= q(#(--header=@(:bar:@(:A<int>; :B); :baz))) | :unq"]
        ),
        "#(--header=@(:bar:@(:A<int>; :B); :baz))"
    );
    // applied as the structured projector it is, the level below is
    // read on what the outer name reads
    assert_eq!(
        ok(&d, &[&format!("= {t} | .t | header | .h | $.t:$.h")]),
        "@(@(@(1; 4); @(2; 5)); @(3; 6))"
    );

    // a header has levels or names with colons, not both
    let e = refused(&d, &["= @(1; 2) | #(--header=@(:bar:@(:A); :'x:y'))"]);
    assert!(
        e.contains("a header that writes levels")
            && e.contains("'x:y' holds one of its own")
            && e.contains("| #(--slugs)"),
        "{e}"
    );
    // an empty level names nothing
    let e = refused(&d, &["= @(1; 2) | #(--header=@(:bar:@(); :z))"]);
    assert!(e.contains("a projector holding the level below it"), "{e}");
}

/// The walks tied at the best total (the user, 2026-11-10). They
/// answer in the walk's order, by round and then by the edges'
/// order, under the search by total as under the exact walk. The
/// reach never merges: it selects among the threads the plain walk
/// answers, so two tied walks over two edges are two answers, alike
/// or not, and `$-` reads each walk's own edge. And a reach selector
/// written directly after a brace before the last breaks the ties
/// the last brace's leaves ("last brace first, first brace
/// tie-breaker"): `?` the least of that brace's cost, `!` the
/// greatest, right to left; a brace without one orders nothing.
#[test]
fn a_reach_selector_per_brace_breaks_the_ties() {
    let d = fixture("brace-markers");
    // two ways to T, three hours each, two and three euros; the way
    // by P is listed first and is met second by a search by total
    std::fs::write(
        d.join("fork.json"),
        r#"{"towns":[{"code":"S"},{"code":"P"},{"code":"Q"},{"code":"T"}],
 "legs":[{"a":"S","b":"P","fare":1,"duration":2,"cap":1},
         {"a":"S","b":"Q","fare":2,"duration":1,"cap":1},
         {"a":"P","b":"T","fare":1,"duration":1,"cap":1},
         {"a":"Q","b":"T","fare":1,"duration":2,"cap":1}],
 "rails":[{"a":"S","b":"T","duration":3}]}"#,
    )
    .unwrap();
    std::fs::write(
        d.join("fork.model"),
        "ref /legs/*::a --> /towns/*[::code = $];\n\
         ref /legs/*::b --> /towns/*[::code = $];\n\
         ref /rails/*::a --> /towns/*[::code = $];\n\
         ref /rails/*::b --> /towns/*[::code = $];\n\
         edge /legs/*: ::a -- ::b as leg weights(::fare<_EUR>; ::duration<_h>; ::cap<_t>);\n\
         edge /rails/*: ::a -- ::b as rail weights(::duration<_h>);\n",
    )
    .unwrap();
    let from_s = |tail: &str| format!(r#"/towns/*[::code = "S"]{tail}"#);
    let q = |tail: &str| ok(&d, &["--model", "fork.model", &from_s(tail), "fork.json"]);
    let no = |tail: &str| refused(&d, &["--model", "fork.model", &from_s(tail), "fork.json"]);
    let route = r#"| %(route = (@. | join(" > ")); c = $-:costs)"#;
    let by_p = r#"%(route = "P > T"; c = @(2_EUR; 3_h))"#;
    let by_q = r#"%(route = "Q > T"; c = @(3_EUR; 3_h))"#;

    // the walk's order, under the search by total and the exact walk
    assert_eq!(
        q(&format!(
            r#"(->leg .(::code)){{*_h}}[::code = "T"]? {route}"#
        )),
        "%(route = \"P > T\"; c = @(3_h))\n%(route = \"Q > T\"; c = @(3_h))"
    );
    assert_eq!(
        q(&format!(
            r#"(->leg .(::code)){{;10_EUR}}{{*_h}}[::code = "T"]? {route}"#
        )),
        format!("{by_p}\n{by_q}")
    );

    // walks that record nothing are as many threads: two that
    // differ in a cost, two that print alike and read each its own
    // arriving edge
    assert_eq!(
        q(r#"(->leg){;10_EUR}{*_h}[::code = "T"]? | %(::code; c = $-:costs)"#),
        "%(code = \"T\"; c = @(2_EUR; 3_h))\n%(code = \"T\"; c = @(3_EUR; 3_h))"
    );
    assert_eq!(
        q(r#"(->leg){*_h}[::code = "T"]? | %(::code; t = $-:total)"#),
        "%(code = \"T\"; t = 3_h)\n%(code = \"T\"; t = 3_h)"
    );
    assert_eq!(
        q(r#"(->leg){*_h}[::code = "T"]? | %(::code; fare = $-::fare; from = $-:source::code)"#),
        "%(code = \"T\"; fare = 1; from = \"P\")\n%(code = \"T\"; fare = 1; from = \"Q\")"
    );
    // by round first: the rail in one, then the two walks by the legs
    assert_eq!(
        q(r#"(->*){*_h}[::code = "T"]? | %(::code; via = $-; t = $-:total)"#),
        "%(code = \"T\"; via = \"rail\"; t = 3_h)\n%(code = \"T\"; via = \"leg\"; t = 3_h)\n\
         %(code = \"T\"; via = \"leg\"; t = 3_h)"
    );

    // a reach selector on the brace before the last breaks the tie
    assert_eq!(
        q(&format!(
            r#"(->leg .(::code)){{;10_EUR}}?{{*_h}}[::code = "T"]? {route}"#
        )),
        by_p
    );
    assert_eq!(
        q(&format!(
            r#"(->leg .(::code)){{;10_EUR}}!{{*_h}}[::code = "T"]? {route}"#
        )),
        by_q
    );
    // the two selectors mix
    assert_eq!(
        q(&format!(
            r#"(->leg .(::code)){{;10_EUR}}!{{*_h}}[::code = "T"]! {route}"#
        )),
        by_q
    );
    // a count brace with a selector selects by the hops
    let hops = r#"| %(route = (@. | join(" > ")); via = $-; h = $-:hops)"#;
    assert_eq!(
        q(&format!(
            r#"(->* .(::code)){{1;3}}?{{*_h}}[::code = "T"]? {hops}"#
        )),
        r#"%(route = "T"; via = "rail"; h = 1)"#
    );
    assert_eq!(
        q(&format!(
            r#"(->* .(::code)){{1;3}}!{{*_h}}[::code = "T"]? {hops}"#
        )),
        "%(route = \"P > T\"; via = \"leg\"; h = 2)\n%(route = \"Q > T\"; via = \"leg\"; h = 2)"
    );
    // the query reads back as written, each selector after its brace
    assert_eq!(
        ok(
            &d,
            &[r#"= q(/towns/S(->leg .(::code)){;10_EUR}!{1;3}?{*_h}[::code = "T"]!) | :unq"#]
        ),
        r#"/towns/S(->leg .(::code)){;10_EUR}!{1;3}?{*_h}[::code = "T"]!"#
    );

    // a tie-breaker with nothing to break
    let e = no(r#"(->leg){;10_EUR}?{*_h}[::code = "T"] | ::code"#);
    assert!(
        e.contains("a reach selector on a brace before the last")
            && e.contains("the stack ends in none"),
        "{e}"
    );
    // a brace after a marked '+', as after a bare one
    let e = no(r#"(->leg)+?{*_h}[::code = "T"]? | ::code"#);
    assert!(e.contains("a brace never follows '+' or '*'"), "{e}");
    // a filter after the reach selector: the pointer at the order
    for tail in [
        r#"(->leg){*_h}?[::code = "T"] | ::code"#,
        r#"(->leg)+?[::code = "T"] | ::code"#,
        r#"(->leg){;10_EUR}?{*_h}?[::code = "T"] | ::code"#,
    ] {
        let e = no(tail);
        assert!(
            e.contains("a group's predicates stand between its quantifier and its reach selector"),
            "{tail}: {e}"
        );
    }
}

/// The scientific reading (the user, 2026-11-10: "approved"). Under
/// either reading a superscript after a unit's letter, after `× 10`
/// and a signed one are exponents, and a letter, a symbol and a
/// bracketed number in superscript are footnote marks, dropped;
/// every superscript that was a number was dropped as a mark
/// (`5 m<sup>2</sup>` read five metres). The one case the text does
/// not settle, a bare digit right after a plain number, is a
/// footnote mark, and an exponent under `?sci` at the mount, `--sci`
/// on `| #` and on `clean`; there an uppercase K is the kelvin too.
#[test]
fn the_scientific_reading_of_superscripts_and_k() {
    let d = fixture("sci");
    std::fs::write(
        d.join("bodies.html"),
        r#"<html><body><table>
<tr><th>Body</th><th>Area (km<sup>2</sup>)</th><th>Mass</th><th>Gravity<sup>1</sup></th><th>Count</th></tr>
<tr><td>Earth<sup>a</sup></td><td>5.1 × 10<sup>8</sup></td><td>5.97 × 10<sup>24</sup> kg</td><td>9.8 m/s<sup>2</sup></td><td>10<sup>3</sup></td></tr>
<tr><td>Moon<sup>[2]</sup></td><td>3.8 × 10<sup>7</sup></td><td>7.35 × 10<sup>22</sup> kg</td><td>1.6 m/s<sup>2</sup></td><td>10<sup>-3</sup></td></tr>
<tr><td>Mars<sup>*</sup></td><td>1.4 × 10<sup>8</sup></td><td>6.42 × 10<sup>23</sup> kg</td><td>3.7 m/s<sup>2</sup></td><td>12<sup>2</sup></td></tr>
</table></body></html>"#,
    )
    .unwrap();
    let on = |query: &str, target: &str| ok(&d, &[query, target]);

    // the table as read: exponents written with their caret, marks
    // dropped, the bare digit after a plain number a mark
    assert_eq!(
        on("//table | #", "bodies.html"),
        "#(@(Body; \"Area (km^2)\"; Mass; Gravity; Count);\n  \
         %(\"Earth\"; \"5.1 × 10^8\"; \"5.97 × 10^24 kg\"; 9.8_m/s^2; \"10\");\n  \
         %(\"Moon\"; \"3.8 × 10^7\"; \"7.35 × 10^22 kg\"; 1.6_m/s^2; \"10^-3\");\n  \
         %(\"Mars\"; \"1.4 × 10^8\"; \"6.42 × 10^23 kg\"; 3.7_m/s^2; \"12\"))"
    );
    // cleaned: the powers of ten numbers, with their unit quantities
    let cleaned = |count: &str| {
        format!(
            "#(@(Body; \"Area (km^2)\"; Mass; Gravity; Count);\n  \
             %(\"Earth\"; 510000000.0; 5.97e+24_kg; 9.8_m/s^2; {});\n  \
             %(\"Moon\"; 38000000.0; 7.35e+22_kg; 1.6_m/s^2; 0.001);\n  \
             %(\"Mars\"; 140000000.0; 6.42e+23_kg; 3.7_m/s^2; {}))",
            count.split(';').next().unwrap(),
            count.split(';').nth(1).unwrap()
        )
    };
    assert_eq!(
        on("//table | # | clean", "bodies.html"),
        cleaned("10.0;12.0")
    );
    // the scientific reading: the bare digit an exponent, by the
    // constructor's option and by the mount's
    assert_eq!(
        on("//table | #(--sci) | clean", "bodies.html"),
        cleaned("1000.0;144.0")
    );
    assert_eq!(
        on("//table | # | clean", "bodies.html?sci"),
        cleaned("1000.0;144.0")
    );
    assert_eq!(on("//table | :::sci", "bodies.html?sci"), "true");
    // the warning names the powers as what they nearly are
    let (_, _, err) = qua(&d, &["//table | #", "bodies.html"]);
    assert!(
        err.contains("row 1 holds \"5.1 × 10^8\" (a power of ten)"),
        "{err}"
    );

    // a text cleaned: superscript digits read as a document's are
    assert_eq!(
        on(
            r#"= @("5 m²"; "9.8 m/s²"; "10⁻³"; "6.02×10²³"; "1.5 × 10³ kg"; "2^10"; "France¹"; "10³"; "12²") | clean"#,
            "bodies.html"
        ),
        r#"@(5_m^2; 9.8_m/s^2; 0.001; 6.02e+23; 1500_kg; 1024; "France"; 10; 12)"#
    );
    assert_eq!(
        on(
            r#"= @("10³"; "12²"; "France¹"; "5 m²"; "300 K"; "300k") | clean(--sci)"#,
            "bodies.html"
        ),
        r#"@(1000; 144; "France"; 5_m^2; 300_K; 300000)"#
    );
    // `clean` over a node of a mount opened `?sci` reads as
    // `clean(--sci)` does
    std::fs::write(d.join("temp.json"), r#"{"t": "300 K"}"#).unwrap();
    assert_eq!(on("/t | :: | clean", "temp.json"), "300000");
    assert_eq!(on("/t | :: | clean", "temp.json?sci"), "300_K");

    // the three flags take no value
    let e = refused(&d, &["//table | #(--sci=1)", "bodies.html"]);
    assert!(e.contains("'#(--sci)' takes no value"), "{e}");
    let e = refused(&d, &[r#"= "300 K" | clean(--sci=1)"#]);
    assert!(e.contains("option '--sci' of 'clean' is a switch"), "{e}");
    let e = refused(&d, &["//table | #", "bodies.html?sci=1"]);
    assert!(e.contains("sci=1: sci takes no value"), "{e}");
}

/// Two rows of the review's syntax table. The short and the long
/// spelling of one option are one option: `pretty(-p=1 --precision=4)`
/// took the first written where `--precision` twice is refused (B16).
/// And the refusal of a header that is no list pointed at the record
/// form, which is retired (B19).
#[test]
fn one_option_under_two_spellings_is_written_twice() {
    let d = fixture("spellings");
    for q in [
        "= 3.14159 | pretty(-p=1 --precision=4)",
        "= 3.14159 | pretty(--precision=4 -p=1)",
    ] {
        let e = refused(&d, &[q]);
        assert!(
            e.contains(
                "are one option, which takes one value, and it is written twice in 'pretty'"
            ),
            "{q}: {e}"
        );
    }
    let e = refused(&d, &["= 3.14159 | pretty(--precision=4 --precision=1)"]);
    assert!(
        e.contains("'--precision' takes one value, and is written twice in 'pretty'"),
        "{e}"
    );
    // one spelling once, as ever
    assert_eq!(ok(&d, &["= 3.14159 | pretty(-p=1)"]), "3.1");

    let e = refused(&d, &["= #(@(a); %(1)) | #(--header=5)"]);
    assert!(
        e.contains("the typed header as a list of projectors, '#(--header=@(:a<int>; :b<text>))'")
            && !e.contains("%(a ="),
        "{e}"
    );
}

/// Two findings of lang's on the messages of 2026-11-10. A parse
/// message showed the token it stopped at in its Rust form
/// (`found Some(RBracket)`, `at token Name { text: "s", … }`): it
/// shows it as written. And a model's statement whose query leaves
/// a bracket open kept its own terminator, and every statement
/// after it, inside the query it quoted.
#[test]
fn a_message_shows_a_token_as_written() {
    let d = fixture("tokens");
    let e = refused(&d, &["/books/*[::t = ]", "books.json"]);
    assert!(
        e.contains("expected a value or path in a predicate, found ']'"),
        "{e}"
    );
    let e = refused(&d, &["= 1 2"]);
    assert!(e.contains("unexpected trailing input at '2'"), "{e}");
    let e = refused(&d, &[r#"= 1 "s""#]);
    assert!(e.contains(r#"unexpected trailing input at "s""#), "{e}");
    for q in ["/books/*[::t = ]", "= 1 2", "/books/* )"] {
        let e = refused(&d, &[q, "books.json"]);
        assert!(!e.contains("Some(") && !e.contains("Name {"), "{q}: {e}");
    }

    // the statement's terminator is not the query's, and the
    // statement after it is read as one
    std::fs::write(
        d.join("open.model"),
        "node /novels/novel: /books/*[::author = \"Austen\";\n\
         ref /books/*::author --> /books/*;\n",
    )
    .unwrap();
    let e = refused(&d, &["--model", "open.model", "/novels/*", "books.json"]);
    assert!(
        e.contains(r#"node /novels/novel: the query '/books/*[::author = "Austen"' is refused"#)
            && e.contains("expected ']' to close a predicate"),
        "{e}"
    );
    // a `;` that ends a line inside a balanced list is the list's
    std::fs::write(d.join("list.model"), "property /books/*::k = @(1;\n  2);\n").unwrap();
    assert_eq!(
        ok(
            &d,
            &["--model", "list.model", "/books/*[1] | ::k", "books.json"]
        ),
        "@(1; 2)"
    );
}

/// A column with levels is named by its path wherever a column is
/// named (lang's reading of the levels ruling, 2026-11-10), in the
/// table constructor's options too: `-i=:bar:A` read the path as a
/// field of a field and named no column, and `-c=:bar:A<int>` was a
/// parse error.
#[test]
fn a_levelled_column_is_named_by_its_path_in_the_options() {
    let d = fixture("path-options");
    let q = |query: &str| ok(&d, &[query]);
    let t = "#(@(bar:A; bar:B; baz); %(1; 2; 3); %(4; 5; 6))";
    assert_eq!(q(&format!("= {t} | #(-i=:bar:A) | :::index")), "@(1; 4)");
    assert_eq!(
        q(&format!("= {t} | #(-i=(:bar:A; :baz)) | :::index")),
        r#"@%(@("bar:A"; baz); %(1; 3); %(4; 6))"#
    );
    assert_eq!(
        q(&format!("= {t} | #(-c=:bar:A<float>)")),
        "#(@(bar:A; bar:B; baz);\n  %(1.0; 2; 3);\n  %(4.0; 5; 6))"
    );
    assert_eq!(
        q(&format!(
            "= {t} | #(-c=(:bar:A<float>; :baz<text>)) | header"
        )),
        "@(:bar:@(:A<float>; :B<*; int>); :baz<text>)"
    );
    // in the header option the path writes the levels
    assert_eq!(
        q("= @(1; 2; 3) | #(--header=@(:bar:A<int>; :bar:B; :baz)) | header"),
        "@(:bar:@(:A<int>; :B<*; int>); :baz<*; int>)"
    );
    assert_eq!(
        q(&format!(
            "= {t} | #(--header=@(:bar:B<float>; :baz)) | header"
        )),
        "@(:bar:@(:B<float>); :baz<*; int>)"
    );
    // a path that names no column is refused by its path
    let e = refused(&d, &[&format!("= {t} | #(-i=:bar:C)")]);
    assert!(
        e.contains("--index names column 'bar:C'") && e.contains("its columns: bar:A, bar:B, baz"),
        "{e}"
    );
    // outside the options a trait on a field of a field is a trait
    // on a value, as before
    let e = refused(&d, &["= %(a = %(b = 5)) | :a:b<int>"]);
    assert!(
        e.contains("declares a field, and stands where a value does"),
        "{e}"
    );
}

/// The review's syntax B13: a compound unit as the unit trait's word.
/// `header` answers `:Speed<_km/h>`, and that text was refused
/// ("close with '>': '<_km>'"), the slash read as a hop. It is one
/// word, where the unit trait stands; and a unit the query does not
/// know is refused by name in a literal's header, where the list was
/// read again as a row.
#[test]
fn a_compound_unit_is_the_unit_traits_word() {
    let d = fixture("compound-unit");
    let q = |query: &str| ok(&d, &[query]);
    assert_eq!(q("= #(@(v<_km/h>); %(5))"), "#(@(v);\n  %(5_km/h))");
    assert_eq!(q("= #(@(v<_km/h>); %(5)) | header"), "@(:v<_km/h>)");
    // what `header` answers reads back
    assert_eq!(
        q("= #(@(v<_km/h>); %(5)) | .h(header) | @(7) | #(--header=$.h)"),
        "#(@(v);\n  %(7_km/h))"
    );
    assert_eq!(
        q("= q(#(--header=@(:v<_km/h>))) | :unq"),
        "#(--header=@(:v<_km/h>))"
    );
    assert_eq!(q("= %(v = 5) | :v<_m/s^2>"), "5_m/s^2");
    assert_eq!(q("= %(v = 5) | %(a<_t/a> = :v)"), "%(a = 5_t/a)");
    assert_eq!(
        q("= #(@(v); %(5)) | map(<_m^2>; --to=:v)"),
        "#(@(v);\n  %(5_m^2))"
    );
    assert_eq!(
        q("= #(@(n); %(5)) | #(-c=:n<_km/h>) | header"),
        "@(:n<_km/h>)"
    );
    for (query, unit) in [
        ("= #(@(v<_zz/h>); %(5))", "<_zz/h>"),
        ("= #(@(v<_zz>); %(5))", "<_zz>"),
    ] {
        let e = refused(&d, &[query]);
        assert!(
            e.contains(&format!("'{unit}': no unit 'zz'")),
            "{query}: {e}"
        );
    }
    // a comparison is still a comparison
    assert_eq!(q("= %(a = 5) | (:a < 7)"), "true");
}

/// Two rows of the review's syntax table. `sh(` was refused as the
/// retired command wherever it stood: `sh` as a hop's name before a
/// group and as a fragment's name are names like any other (B12).
/// And a constructor right after a comment was refused as glued,
/// the comment's `)` standing before it: a comment is whitespace
/// (B17c).
#[test]
fn sh_is_a_name_and_a_comment_is_whitespace() {
    let d = fixture("sh");
    std::fs::write(
        d.join("bin.json"),
        r#"{"bin":{"sh":{"b":1,"c":2},"ls":{"b":3}}}"#,
    )
    .unwrap();
    assert_eq!(ok(&d, &["/bin/sh(/b|/c) | ::", "bin.json"]), "1\n2");
    assert_eq!(
        ok(
            &d,
            &[
                r#"def &sh($x): /bin/*[:name = $x]; &sh("ls") | :name"#,
                "bin.json"
            ]
        ),
        "ls"
    );
    // the retired command is still refused where a command stands
    for q in ["= 1 | sh(echo hi)", "= sh(echo hi)"] {
        let e = refused(&d, &[q]);
        assert!(e.contains("the sh(cmd) command is retired"), "{q}: {e}");
    }

    for q in [
        "= 1 | (!! c )@(1; 2)",
        "= 1 | (!! c ) @(1; 2)",
        "= 1 |(!! c )@(1; 2)",
    ] {
        assert_eq!(ok(&d, &[q]), "@(1; 2)", "{q}");
    }
    assert_eq!(ok(&d, &["= (!! c )%(a = 1)"]), "%(a = 1)");
    // glued to a value it is still refused
    let e = refused(&d, &["= 1@(1; 2)"]);
    assert!(e.contains("a list literal follows whitespace"), "{e}");
}

/// The review's syntax B18: a form built with the `syntax:` library
/// skipped the shape checks a written one gets. A group built with
/// no alternative expanded to `()`, one built with its most below
/// its least to `{9;3}`, and both answered nothing; a rewrite that
/// leaves a range no range likewise.
#[test]
fn a_constructed_group_gets_the_shape_checks() {
    let d = fixture("constructed");
    std::fs::write(d.join("r.json"), r#"{"rows":[{"a":1},{"a":2}]}"#).unwrap();
    let e = refused(&d, &["macro &g: ^ | syntax:group(); /rows&g", "r.json"]);
    assert!(
        e.contains("a group node holds one alternative or more"),
        "{e}"
    );
    let e = refused(
        &d,
        &["macro &g: ^ | syntax:group(9; 3; q(/rows)); ^&g", "r.json"],
    );
    assert!(e.contains("quantifier {9;3} has max below min"), "{e}");
    let e = refused(
        &d,
        &[
            "macro &g: ^ | syntax:group(1; 2; q(/rows)) | ($_ ++! %(min = 5)); ^&g | :name",
            "r.json",
        ],
    );
    assert!(e.contains("quantifier {5;2} has max below min"), "{e}");
    // a group that is one still expands and answers
    assert_eq!(
        ok(
            &d,
            &[
                "macro &g: ^ | syntax:group(1; 2; q(/rows)); ^&g | :name",
                "r.json"
            ]
        ),
        "rows"
    );
}

/// The review's graph B7: the navop wildcard merged parallel edges.
/// `~x` is `(/x|\x|->x|<-x)`, one thread per edge; the step had each
/// edge in hand and answered nodes, and the edge was derived again
/// from the node, the first that matched. Two legs between two towns
/// were one thread with the first one's fare, so the lightest reach
/// over `~` answered a dearer total than over `--`.
#[test]
fn the_navop_wildcard_hands_its_edges_over() {
    let d = fixture("navop-edges");
    // two legs from S to T, the dearer one listed first
    std::fs::write(
        d.join("pair.json"),
        r#"{"towns":[{"code":"S"},{"code":"T"}],
 "legs":[{"id":1,"a":"S","b":"T","fare":9,"duration":1,"cap":1},
         {"id":2,"a":"S","b":"T","fare":4,"duration":1,"cap":1}]}"#,
    )
    .unwrap();
    std::fs::write(
        d.join("legs.model"),
        "ref /legs/*::a --> /towns/*[::code = $];\n\
         ref /legs/*::b --> /towns/*[::code = $];\n\
         edge /legs/*: ::a -- ::b as leg weights(::fare<_EUR>; ::duration<_h>; ::cap<_t>);\n",
    )
    .unwrap();
    let q = |tail: &str| {
        ok(
            &d,
            &[
                "--model",
                "legs.model",
                &format!(r#"/towns/*[::code = "S"]{tail}"#),
                "pair.json",
            ],
        )
    };
    // one thread per edge, as the link hop has them
    assert_eq!(q(r#"--leg[::code = "T"] | $-::id"#), "1\n2");
    assert_eq!(q(r#"~leg[::code = "T"] | $-::id"#), "1\n2");
    assert_eq!(q(r#"~*[::code = "T"] | $-::fare"#), "9\n4");
    // and the lightest reach reads the cheaper leg
    assert_eq!(q(r#"(--leg){*_EUR}[::code = "T"]? | $-:total"#), "4_EUR");
    assert_eq!(q(r#"(~leg){*_EUR}[::code = "T"]? | $-:total"#), "4_EUR");
    assert_eq!(q(r#"(~*){*_EUR}[::code = "T"]? | $-:total"#), "4_EUR");
}

/// Five rows of the review's pipeline table, each a short fix.
#[test]
fn five_rows_of_the_pipeline_review() {
    let d = fixture("pipeline-rows");
    std::fs::write(
        d.join("two.json"),
        r#"{"books":[{"title":"Iliad","price":12},{"title":"Odyssey","price":15}]}"#,
    )
    .unwrap();
    let on = |query: &str| ok(&d, &[query, "two.json"]);

    // F16: a projector is the simplest quarq, and the two appliers
    // read one held as the bare sigil does (both refused "the regula
    // holds a projector"): `!` on the applying cursor, `#` on the
    // node the push stood at
    assert_eq!(on(r"/books/1 | .f(q(::title)) | \ | /0 | &$.f"), "Iliad");
    assert_eq!(on(r"/books/1 | .f(q(::title)) | \ | /0 | &$.f!"), "Iliad");
    assert_eq!(on(r"/books/1 | .f(q(::title)) | \ | /0 | &$.f#"), "Odyssey");
    assert_eq!(
        on(r"/books/1 | .f(q(%(::title; ::price))) | \ | /0 | &$.f#"),
        r#"%(title = "Odyssey"; price = 15)"#
    );
    let e = refused(&d, &["/books/*[1] | .f(5) | &$.f!", "two.json"]);
    assert!(
        e.contains("&$.f! applies a quarq, and the regula holds a number"),
        "{e}"
    );

    // F12: `%%(…)` is `%.` with the given fields; a mark stood in it
    assert_eq!(
        on("/books/*[1] | .m | .t(::title) | %."),
        r#"%(t = "Iliad")"#
    );
    assert_eq!(
        on("/books/*[1] | .m | .t(::title) | %%(a = 1)"),
        r#"%(t = "Iliad"; a = 1)"#
    );

    // F11b: a `session(…)` that ends a body hands its thread back; at
    // the end of the query it answers nothing, as before
    assert_eq!(
        on("= 5 | .f(q(session(--precision=2))) | &$.f | $_ + 1"),
        "6"
    );
    assert_eq!(on("= 5 | session(--precision=2)"), "");

    // F17: the per-thread shift saturates, as the context's does
    assert_eq!(
        on("= @(1; 2; 3) | shift(-9223372036854775808)"),
        "@(null; null; null)"
    );
    assert_eq!(on("= @(1; 2; 3) | shift(1)"), "@(null; 1; 2)");

    // F10: a recalled field's provenance is the recalled regula's,
    // whatever was pushed before it (a mark first, and `$.1:a` took
    // the mark's origin)
    for q in [
        "/books/*[2] | .r(%(a = ::title)) | $.1:a",
        "^ | .m | /books/*[2] | .r(%(a = ::title)) | $.1:a",
        "^ | .m | /books/*[2] | .r(%(a = ::title)) | $.r:a",
    ] {
        let out = ok(&d, &["--kaiv", q, "two.json"]);
        assert!(out.contains("#books-1"), "{q}: {out}");
    }
}

/// The review's pipeline F9: an expansion the template's fill does
/// not reach was left in the quarq, which then refused where it was
/// applied as "outside a quarq template", of a query that wrote it
/// inside one. It is refused at the fill, by name.
#[test]
fn an_expansion_the_fill_does_not_reach_is_refused() {
    let d = fixture("unfilled");
    for q in [
        "= 2 | q(@(5;6;7)[${$_}]) | :unq",
        "= 2 | q(#(@(a; b); %(${$_}; 9))) | :unq",
    ] {
        let e = refused(&d, &[q]);
        assert!(
            e.contains("expansion '${$_}' in a template stands where the template is not filled"),
            "{q}: {e}"
        );
    }
    // where the fill reaches, it fills as before
    assert_eq!(
        ok(&d, &["= 2 | q(@(5; 6; 7) | [${$_}]) | :unq"]),
        "@(5; 6; 7) | [2]"
    );
    assert_eq!(ok(&d, &["= 2 | q($_ + ${$_}) | :unq"]), "$_ + 2");
}

/// The review's pipeline F14: the iteration pipe ran its command in
/// navigation mode, where a bare projection over a null topic reads
/// the node, so `@(null; 1) $| ::title` answered the title twice
/// where `map(::title)` answers null for the null. Each item is its
/// thread's topic: value mode.
#[test]
fn the_iteration_pipe_runs_in_value_mode() {
    let d = fixture("iteration-mode");
    std::fs::write(
        d.join("two.json"),
        r#"{"books":[{"title":"Iliad","price":12},{"title":"Odyssey","price":15}]}"#,
    )
    .unwrap();
    let on = |query: &str| ok(&d, &[query, "two.json"]);
    assert_eq!(
        on("/books/*[1] | @(null; 1) $| ::title"),
        r#"@(null; "Iliad")"#
    );
    assert_eq!(
        on("/books/*[1] | @(null; 1) | map(::title)"),
        r#"@(null; "Iliad")"#
    );
    // the rest as before
    assert_eq!(on("/books/*[1] | @(1; 2) $| ($_ + 1)"), "@(2; 3)");
    assert_eq!(on("= @(1; null; 3) $| ($_ ?? 0)"), "@(1; 0; 3)");
    assert_eq!(
        on("/books/*[1] | @(1; 2) $| %(n = $_; t = ::title)"),
        r#"@%(@(n; t); %(1; "Iliad"); %(2; "Iliad"))"#
    );
}
