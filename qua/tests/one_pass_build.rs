//! Hash (e) of the columnar round (2026-11-06): the one-pass typed
//! build — a CSV row's cells parsed from their text straight into the
//! typed columns, the leader type moving forward as cells arrive, the
//! text kept for the widening — and what came with it: a table built
//! from a source of one shape keeps the source's column order (nulls
//! in the empty cells) on every path; `-a|--autoindex` on the table
//! constructor; `melt(:id)` keeping its named ids on a plain table.
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    let test = std::thread::current()
        .name()
        .unwrap_or("t")
        .replace(':', "-");
    let dir = std::env::temp_dir().join(format!("quarb-onepass-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("order.csv"),
        "id,title,year,shelf\na,Iliad,,A1\nb,Odyssey,,A2\nc,Aeneid,-19,B1\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("comma.csv"),
        "Name;Age;Fare\nBraund;22;7,25\nCumings;38;71,5\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("widen.csv"),
        "n,f,t,q,b\n1,1.50,1.50,3 kg,true\n2,2,x,4 kg,false\n3,,2.5,5 m,1\n",
    )
    .unwrap();
    std::fs::write(dir.join("ragged.csv"), "a,b,c\n1,2\n3,4,5,6\n7,,9\n").unwrap();
    std::fs::write(
        dir.join("u.csv"),
        "Name,Age,Fare,Note\nBraund,22,7.25,x\nCumings,,71.5,y\nHeikkinen,26,8.75,x\n",
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

fn ok(dir: &Path, args: &[&str]) -> String {
    let (out, err, good) = qua(dir, args);
    assert!(good, "{args:?}: {err}");
    out
}

#[test]
fn a_table_from_a_shaped_source_keeps_the_columns_order() {
    let d = fixture();
    let header = "@(:id<*; text>; :title<*; text>; :year<*; int>; :shelf<*; text>)";
    // the streamed build, the whole mount, the thread path, the index
    assert_eq!(ok(&d, &["/row @| # | header", "order.csv"]), header);
    assert_eq!(
        ok(&d, &["--no-stream", "/row @| # | header", "order.csv"]),
        header
    );
    assert_eq!(ok(&d, &["/row[1..] @| # | header", "order.csv"]), header);
    assert_eq!(
        ok(&d, &["/row[1..] @| #(-i=:id) | header", "order.csv"]),
        header
    );
    assert_eq!(
        ok(&d, &["/row @| #", "order.csv"]),
        "#(@(id; title; year; shelf);\n  %(\"a\"; \"Iliad\"; null; \"A1\");\n  %(\"b\"; \"Odyssey\"; null; \"A2\");\n  %(\"c\"; \"Aeneid\"; -19; \"B1\"))"
    );
    assert_eq!(
        ok(&d, &["/row[1] @| #", "order.csv"]),
        "#(@(id; title; year; shelf);\n  %(\"a\"; \"Iliad\"; null; \"A1\"))"
    );
}

#[test]
fn the_text_path_parses_the_cells_as_the_record_path_read_them() {
    let d = fixture();
    // a declared decimal comma: the canonical form on the way in
    assert_eq!(
        ok(
            &d,
            &[
                "/row @| # | header",
                "comma.csv?delimiter=semicolon&decimal=comma"
            ]
        ),
        "@(:Name<*; text>; :Age<*; int>; :Fare<*; float>)"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "/row @| # | sum(--of=:Fare)",
                "comma.csv?delimiter=semicolon&decimal=comma"
            ]
        ),
        "78.75"
    );
    // the leader widens: ints then a float, text that looked numeric
    // first keeps its spelling, quantities of two dimensions are text,
    // booleans then a digit are text
    assert_eq!(
        ok(&d, &["/row @| # | header", "widen.csv"]),
        "@(:n<*; int>; :f<*; float>; :t<*; text>; :q<*; text>; :b<*; text>)"
    );
    assert_eq!(
        ok(&d, &["/row @| # | :t", "widen.csv"]),
        "@(\"1.50\"; \"x\"; \"2.5\")"
    );
    assert_eq!(
        ok(&d, &["/row @| # | :f", "widen.csv"]),
        "@(1.5; 2.0; null)"
    );
    // ragged rows: a short row's missing cells null, a long row's
    // extra cell dropped, as the record path read them
    assert_eq!(
        ok(&d, &["/row @| #", "ragged.csv"]),
        ok(&d, &["/row[1..] @| #", "ragged.csv"])
    );
    assert_eq!(
        ok(&d, &["/row @| #", "ragged.csv"]),
        "#(@(a; b; c);\n  %(1; 2; null);\n  %(3; 4; 5);\n  %(7; null; 9))"
    );
    assert_eq!(
        ok(&d, &["/row | :::n-fields", "ragged.csv"]),
        ok(&d, &["--no-stream", "/row | :::n-fields", "ragged.csv"])
    );
    assert_eq!(
        ok(&d, &["--no-stream", "/row | :::n-fields", "ragged.csv"]),
        "2\n4\n3"
    );
    // the streamed and the whole read agree cell for cell, the ragged
    // file included (ruling 2026-11-06: a short row fills its tail with
    // null, a long one drops the fields past the header, read whole or
    // as a stream alike)
    for f in ["widen.csv", "order.csv", "ragged.csv"] {
        assert_eq!(
            ok(&d, &["/row @| #", f]),
            ok(&d, &["--no-stream", "/row @| #", f]),
            "{f}"
        );
    }
}

#[test]
fn the_autoindex_switch_is_the_piped_command() {
    let d = fixture();
    assert_eq!(
        ok(&d, &["/row @| #(-a)", "u.csv"]),
        ok(&d, &["/row @| # | autoindex", "u.csv"])
    );
    assert_eq!(
        ok(&d, &["/row @| # | #(-a)", "u.csv"]),
        ok(&d, &["/row @| # | autoindex", "u.csv"])
    );
    assert_eq!(
        ok(&d, &["/row @| # | #(:Name; :Age; -a)", "u.csv"]),
        ok(&d, &["/row @| # | #(:Name; :Age) | autoindex", "u.csv"])
    );
    assert_eq!(
        ok(&d, &["/row @| #(-a) | header", "u.csv"]),
        "@(:ix<*; int>; :Name<*; text>; :Age<*; int>; :Fare<*; float>; :Note<*; text>)"
    );
    assert_eq!(
        ok(&d, &["/row @| #(--autoindex) | [[2]]", "u.csv"]),
        "%(ix = 2; Name = \"Cumings\"; Age = null; Fare = 71.5; Note = \"y\")"
    );
    // never beside -i, in either form
    let (_, e, good) = qua(&d, &["/row @| #(-a -i=:Name)", "u.csv"]);
    assert!(!good);
    assert!(e.contains("names two indexes"), "{e}");
    let (_, e, good) = qua(&d, &["/row @| # | #(:Name; :Age; -a; -i=:Name)", "u.csv"]);
    assert!(!good);
    assert!(e.contains("names two indexes"), "{e}");
    let (_, e, good) = qua(&d, &["/row @| #(-a=1)", "u.csv"]);
    assert!(!good);
    assert!(e.contains("takes no value"), "{e}");
}

#[test]
fn melt_keeps_the_named_ids_on_a_plain_table() {
    let d = fixture();
    assert_eq!(
        ok(
            &d,
            &[
                "/row @| # | [..2] | #(:id; :year; :shelf) | melt(:id)",
                "order.csv"
            ]
        ),
        "#(@(id; variable; value);\n  %(\"a\"; \"year\"; null);\n  %(\"a\"; \"shelf\"; \"A1\");\n  %(\"b\"; \"year\"; null);\n  %(\"b\"; \"shelf\"; \"A2\"))"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "/row @| # | [..2] | #(:id; :year; :shelf) | melt(:id; :shelf)",
                "order.csv"
            ]
        ),
        "#(@(id; shelf; variable; value);\n  %(\"a\"; \"A1\"; \"year\"; null);\n  %(\"b\"; \"A2\"; \"year\"; null))"
    );
    // the word form and the list form say the same
    assert_eq!(
        ok(
            &d,
            &[
                "/row @| # | [..2] | #(:id; :year; :shelf) | melt(id)",
                "order.csv"
            ]
        ),
        ok(
            &d,
            &[
                "/row @| # | [..2] | #(:id; :year; :shelf) | melt(:id)",
                "order.csv"
            ]
        )
    );
    assert_eq!(
        ok(
            &d,
            &[
                "/row @| # | [..2] | #(:id; :year; :shelf) | melt(@(id; shelf))",
                "order.csv"
            ]
        ),
        ok(
            &d,
            &[
                "/row @| # | [..2] | #(:id; :year; :shelf) | melt(:id; :shelf)",
                "order.csv"
            ]
        )
    );
}

/// Ruling 2026-11-06, a type rides the name in every table
/// constructor: the literal's header, the keys of the records
/// gathered by `| #`, the column view, and `-c|--cast=:col<type>`;
/// each typed form equals the bare form followed by
/// `map(<type>; --to=:col)`.
#[test]
fn a_type_rides_the_name_in_every_constructor() {
    let d = fixture();
    let typed_header = "@(:n<int>; :s<*; text>)";
    // the literal's header
    assert_eq!(
        ok(
            &d,
            &[
                "= #(@(n<int>; s); %(1; \"a\"); %(\"2\"; \"b\")) | header",
                "u.csv"
            ]
        ),
        typed_header
    );
    assert_eq!(
        ok(
            &d,
            &["= #(@(n<int>; s); %(1; \"a\"); %(\"2\"; \"b\"))", "u.csv"]
        ),
        ok(
            &d,
            &[
                "= #(@(n; s); %(1; \"a\"); %(\"2\"; \"b\")) | map(<int>; --to=:n)",
                "u.csv"
            ]
        )
    );
    let (_, e, good) = qua(
        &d,
        &["= #(@(n<int>; s); %(1; \"a\"); %(\"x\"; \"b\"))", "u.csv"],
    );
    assert!(!good);
    assert!(
        e.contains("column 'n' is declared int, and row 2 holds \"x\""),
        "{e}"
    );
    // the records' keys
    assert_eq!(
        ok(
            &d,
            &[
                "= @(%(n<int> = 1; s = \"a\"); %(n<int> = 2; s = \"b\")) | # | header",
                "u.csv"
            ]
        ),
        typed_header
    );
    assert_eq!(
        ok(
            &d,
            &[
                "= @(%(n<int> = 1; s = \"a\"); %(n = \"2\"; s = \"b\")) | # | header",
                "u.csv"
            ]
        ),
        typed_header
    );
    assert_eq!(
        ok(
            &d,
            &[
                "= @(%(n<int> = 1; s = \"a\"); %(n = \"2\"; s = \"b\")) | #",
                "u.csv"
            ]
        ),
        ok(
            &d,
            &[
                "= @(%(n = 1; s = \"a\"); %(n = \"2\"; s = \"b\")) | # | map(<int>; --to=:n)",
                "u.csv"
            ]
        )
    );
    let (_, e, good) = qua(&d, &["= @(%(n<int> = 1); %(n<text> = \"2\")) | #", "u.csv"]);
    assert!(!good);
    assert!(
        e.contains("key 'n' is declared int on one record and text on another"),
        "{e}"
    );
    // the column view, as before
    assert_eq!(
        ok(
            &d,
            &[
                "= %(n<int> = @(1; \"2\"); s = @(\"a\"; \"b\")) | # | header",
                "u.csv"
            ]
        ),
        typed_header
    );
    // the cast option: the pipe form, the shape form, the stream;
    // repeated; with -a; the refusals
    assert_eq!(
        ok(&d, &["/row @| #(-c=:Age<float>)", "u.csv"]),
        ok(&d, &["/row @| # | map(<float>; --to=:Age)", "u.csv"])
    );
    assert_eq!(
        ok(&d, &["/row @| #(-c=:Age<float>) | header", "u.csv"]),
        "@(:Name<*; text>; :Age<float>; :Fare<*; float>; :Note<*; text>)"
    );
    assert_eq!(
        ok(&d, &["/row @| # | #(:Name; :Age; -c=:Age<float>)", "u.csv"]),
        ok(
            &d,
            &[
                "/row @| # | #(:Name; :Age) | map(<float>; --to=:Age)",
                "u.csv"
            ]
        )
    );
    assert_eq!(
        ok(
            &d,
            &["/row @| #(-c=:Age<float> -c=:Fare<text>) | header", "u.csv"]
        ),
        "@(:Name<*; text>; :Age<float>; :Fare<text>; :Note<*; text>)"
    );
    assert_eq!(
        ok(&d, &["/row @| #(-c=:Age<float> -a) | header", "u.csv"]),
        "@(:ix<*; int>; :Name<*; text>; :Age<float>; :Fare<*; float>; :Note<*; text>)"
    );
    let (_, e, good) = qua(&d, &["/row @| #(-c=:Note<int>)", "u.csv"]);
    assert!(!good);
    assert!(
        e.contains("column 'Note' is declared int, and row 1 holds \"x\""),
        "{e}"
    );
    let (_, e, good) = qua(&d, &["/row @| #(-c=:Nope<int>)", "u.csv"]);
    assert!(!good);
    assert!(e.contains("--cast names column 'Nope'"), "{e}");
    let (_, e, good) = qua(
        &d,
        &["= #(@(n<int>; s); %(1; \"a\")) | #(-c=:n<text>)", "u.csv"],
    );
    assert!(!good);
    assert!(
        e.contains("declared int by the header and cast to text by -c"),
        "{e}"
    );
    let (_, e, good) = qua(&d, &["/row @| #(-c=:Age)", "u.csv"]);
    assert!(!good);
    assert!(e.contains("casts a column to a type"), "{e}");
    // on a record, map(<int>; --to=:n) declares the key (the finding)
    assert_eq!(
        ok(
            &d,
            &["= %(n = \"7\") | map(<int>; --to=:n) | :::header", "u.csv"]
        ),
        "@(:n<int>)"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "= %(n<int> = 7; s = \"a\") | map(<*>; --to=:n) | :::header",
                "u.csv"
            ]
        ),
        "@(:n<*; int>; :s<*; text>)"
    );
}

/// Ruling 2026-11-06, the option reader: a many-valued option takes
/// its values as a list — in parentheses, as the literal, or held by
/// any expression answering one — or as the option repeated, in the
/// order written; an option that takes one value, written twice, is
/// refused.
#[test]
fn a_many_valued_option_takes_a_list_or_repeats() {
    let d = fixture();
    let cast = "@(:Name<*; text>; :Age<float>; :Fare<text>; :Note<*; text>)";
    for q in [
        "/row @| #(-c=(:Age<float>; :Fare<text>)) | header",
        "/row @| #(-c=@(:Age<float>; :Fare<text>)) | header",
        "/row @| #(-c=:Age<float> -c=:Fare<text>) | header",
        "= @(:Age<float>; :Fare<text>) | .c | /row @| #(-c=$.c) | header",
        "= @(:Age<float>; :Fare<text>) | .c | /row @| # | #(-c=$.c) | header",
        "/row @| # | #(:Name; :Age; :Fare; :Note; -c=(:Age<float>; :Fare<text>)) | header",
    ] {
        assert_eq!(ok(&d, &[q, "u.csv"]), cast, "{q}");
    }
    let indexed = "@(:Note<*; text>; :Name<*; text>; :Age<*; int>; :Fare<*; float>)";
    for q in [
        "/row @| #(-i=(:Note; :Name)) | header",
        "/row @| #(-i=:Note -i=:Name) | header",
        "= @(:Note; :Name) | .k | /row @| #(-i=$.k) | header",
    ] {
        assert_eq!(ok(&d, &[q, "u.csv"]), indexed, "{q}");
    }
    let grouped = ok(
        &d,
        &["/row @| # | sum(--by=(:Note; :Name) --of=:Fare)", "u.csv"],
    );
    for q in [
        "/row @| # | sum(--by=:Note --by=:Name --of=:Fare)",
        "/row @| # | sum(--by=@(:Note; :Name) --of=:Fare)",
        "= @(:Note; :Name) | .k | /row @| # | sum(--by=$.k --of=:Fare)",
    ] {
        assert_eq!(ok(&d, &[q, "u.csv"]), grouped, "{q}");
    }
    assert_eq!(
        ok(
            &d,
            &["/row @| # | map(round; --to=:Fare --to=:Age)", "u.csv"]
        ),
        ok(&d, &["/row @| # | map(round; --to=(:Fare; :Age))", "u.csv"])
    );
    // one value, written twice
    let (_, e, good) = qua(&d, &["/row @| # | sum(--of=:Fare --of=:Age)", "u.csv"]);
    assert!(!good);
    assert!(
        e.contains("'--of' takes one value, and is written twice"),
        "{e}"
    );
    let (_, e, good) = qua(&d, &["/row @| #(-n=a -n=b)", "u.csv"]);
    assert!(!good);
    assert!(e.contains("takes one value, and is written twice"), "{e}");
}

/// Lang's finding 2026-11-06: a table projector naming a column the
/// table lacks is refused naming it and the columns, as `map` is —
/// never a fold of the table into one row of joined text.
#[test]
fn a_projector_over_a_column_the_table_lacks_is_refused() {
    let d = fixture();
    for q in [
        "= %(a = @(1; 2; 3); b = @(\"x\"; \"y\"; \"z\")) | # | #(:zz; :a)",
        "= %(a = @(1; 2; 3); b = @(\"x\"; \"y\"; \"z\")) | # | [..2] | #(:zz; :a)",
        "= %(a = @(1; 2; 3); b = @(\"x\"; \"y\"; \"z\")) | # | max_by(:a) | #(:zz; :a)",
    ] {
        let (_, e, good) = qua(&d, &[q, "u.csv"]);
        assert!(!good, "{q}");
        assert!(e.contains("no column 'zz' (the columns: a, b)"), "{q}: {e}");
    }
    assert_eq!(
        ok(
            &d,
            &[
                "= %(a = @(1; 2; 3); b = @(\"x\"; \"y\"; \"z\")) | # | #(:b; :a)",
                "u.csv"
            ]
        ),
        "#(@(b; a);\n  %(\"x\"; 1);\n  %(\"y\"; 2);\n  %(\"z\"; 3))"
    );
}

/// Lang's finding 2026-11-06: a CSV mounted through a model's
/// `mount` statement builds its table in the file's column order as
/// the command-line mount does — the multi-source mount hands the
/// rows' shape through for the source holding the node.
#[test]
fn a_model_mounted_csv_keeps_the_file_order_too() {
    let d = fixture();
    let model = format!("mount t: {};\n", d.join("order.csv").display());
    std::fs::write(d.join("m.model"), model).unwrap();
    let direct = ok(&d, &["/row @| # | header", "order.csv"]);
    assert_eq!(
        direct,
        "@(:id<*; text>; :title<*; text>; :year<*; int>; :shelf<*; text>)"
    );
    assert_eq!(
        ok(&d, &["--model", "m.model", "/t/row @| # | header"]),
        direct
    );
    assert_eq!(
        ok(
            &d,
            &["--no-stream", "--model", "m.model", "/t/row @| # | header"]
        ),
        direct
    );
    assert_eq!(
        ok(&d, &["--model", "m.model", "/t/row @| #"]),
        ok(&d, &["/row @| #", "order.csv"])
    );
    assert_eq!(
        ok(&d, &["--model", "m.model", "/t/row[1..] @| # | header"]),
        direct
    );
}
