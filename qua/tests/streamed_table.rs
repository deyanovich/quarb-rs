//! The streamed table build (ruling 2026-11-06, the columnar round):
//! `@| #` right after a plain path builds the table as the walk goes,
//! no thread and no record held per row, and answers what the thread
//! path answers — the thread path forced here by a positional
//! predicate (`[1..]`), which the streamed walk does not take.
//! `@| #(::a; ::b)` is a one-row literal, not this build; the per-row
//! projector is `@| # | #(:a; :b)` over the streamed table.
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    let test = std::thread::current()
        .name()
        .unwrap_or("t")
        .replace(':', "-");
    let dir = std::env::temp_dir().join(format!("quarb-stable-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("t.csv"),
        "Name,Age,Fare,Note\nBraund,22,7.25,\"a, b\"\nCumings,38,71.5,x\nHeikkinen,26,8.75,\nFutrelle,35,53.1,x\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("n.json"),
        r#"{"a":[{"x":1,"y":"p","sub":{"k":1}},{"x":2,"y":"q"},{"x":3,"z":true}]}"#,
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

#[test]
fn the_streamed_table_answers_as_the_thread_path() {
    let d = fixture();
    for (streamed, threaded) in [
        ("/row @| #", "/row[1..] @| #"),
        ("/row @| # | count", "/row[1..] @| # | count"),
        (
            "/row @| # | mean(--of=:Age)",
            "/row[1..] @| # | mean(--of=:Age)",
        ),
        (
            "/row @| # | mean(--by=:Note)",
            "/row[1..] @| # | mean(--by=:Note)",
        ),
        (
            "/row[::Age > 30] @| # | [..2]",
            "/row[::Age > 30][1..] @| # | [..2]",
        ),
        ("//row @| # | header", "//row[1..] @| # | header"),
        (
            "/* @| # | sort_by(:Age) | [1]",
            "/*[1..] @| # | sort_by(:Age) | [1]",
        ),
        ("/row @| # @| count", "/row[1..] @| # @| count"),
        ("/row @| # | $_:Age", "/row[1..] @| # | $_:Age"),
        // the options: the index column(s), the name
        ("/row @| #(-i=:Name)", "/row[1..] @| #(-i=:Name)"),
        (
            "/row @| #(-i=:Name) | [[\"Cumings\"]]",
            "/row[1..] @| #(-i=:Name) | [[\"Cumings\"]]",
        ),
        (
            "/row @| #(-i=(:Note; :Name)) | [..2]",
            "/row[1..] @| #(-i=(:Note; :Name)) | [..2]",
        ),
        (
            "/row @| #(--name=\"t\") | header",
            "/row[1..] @| #(--name=\"t\") | header",
        ),
        ("/row @| #(-i=:Nope)", "/row[1..] @| #(-i=:Nope)"),
    ] {
        let (a, ea, ga) = qua(&d, &[streamed, "t.csv"]);
        let (b, eb, gb) = qua(&d, &[threaded, "t.csv"]);
        assert_eq!(ga, gb, "{streamed}: {ea} / {eb}");
        assert_eq!(a, b, "{streamed}");
    }
    let (out, err, good) = qua(&d, &["/row @| # | count", "t.csv"]);
    assert!(good, "{err}");
    assert_eq!(out, "4");
    // a nested record in a cell: the thread path's refusal, naming
    // the field and the row
    let (_, err, good) = qua(&d, &["/a/* @| #", "n.json"]);
    assert!(!good);
    assert!(err.contains("'sub' holds a record in row 1"), "{err}");
    let (out, _, good) = qua(&d, &["/a/*/sub @| #", "n.json"]);
    assert!(good);
    assert_eq!(out, "#(@(k);\n  %(1))");
    // a field some rows lack: null, the header in order of first
    // appearance, as the thread path reads it
    let (a, ea, _) = qua(&d, &["/a/*[::x > 1] @| #", "n.json"]);
    let (b, _, _) = qua(&d, &["/a/*[::x > 1][1..] @| #", "n.json"]);
    assert_eq!(a, b, "{ea}");
    assert_eq!(
        a,
        "#(@(x; y; z);\n  %(2; \"q\"; null);\n  %(3; null; true))"
    );
}
