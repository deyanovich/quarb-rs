//! Ruling 2026-11-06: a table's columns are typed once, when the
//! table is built; a selection of rows — the slice, `sort_by`, `top`,
//! `bottom`, `unique_by`, `min_by`, `max_by`, a predicate, `first`/
//! `last`, a label lookup — keeps every column's type. `autocast`
//! re-types the undeclared columns from the cells present (every one,
//! or the columns named) — the named form of `map(<*>)`; a declared
//! column stays as declared; a list reads as one column; a record is
//! refused.
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    let test = std::thread::current()
        .name()
        .unwrap_or("t")
        .replace(':', "-");
    let dir = std::env::temp_dir().join(format!("quarb-autocast-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("v.csv"),
        "Name,Age,Note\nBraund,22,1\nCumings,38,2\nHeikkinen,26,a\nFutrelle,35,\n",
    )
    .unwrap();
    dir
}

fn qua(dir: &Path, q: &str) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_qua"))
        .current_dir(dir)
        .env("QUARB_LOCALE", "C")
        .env("HOME", dir)
        .args([q, "v.csv"])
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
        out.status.success(),
    )
}

const TEXT: &str = "@(:Name<*; text>; :Age<*; int>; :Note<*; text>)";

#[test]
fn a_selection_of_rows_keeps_the_columns_types() {
    let d = fixture();
    let (h, _, _) = qua(&d, "/row @| # | header");
    assert_eq!(h, TEXT);
    for q in [
        "/row @| # | [:Note != \"a\"] | header",
        "/row @| # | [..2] | header",
        "/row @| # | sort_by(:Age) | [..2] | header",
        "/row @| # | top(2; :Age) | header",
        "/row @| # | bottom(1; :Age) | header",
        "/row @| # | unique_by(:Note) | header",
        "/row @| # | min_by(:Age) | header",
        "/row @| # | max_by(:Age) | header",
        "/row @| # | last(2) | header",
        "/row @| # | sort_by($_:Age) | [..2] | header",
        "/row @| # | top(2; $_:Age) | header",
    ] {
        let (h, e, ok) = qua(&d, q);
        assert!(ok, "{q}: {e}");
        assert_eq!(h, TEXT, "{q}");
    }
    // the digit strings stay text once the word's row is gone
    let (out, _, _) = qua(&d, "/row @| # | [:Note != \"a\"] | :Note");
    assert_eq!(out, "@(\"1\"; \"2\"; null)");
    let (out, _, _) = qua(&d, "/row @| # | top(2; :Age) | :Note");
    assert_eq!(out, "@(\"2\"; null)");
    let (out, _, _) = qua(&d, "/row @| # | last(2) | :Note");
    assert_eq!(out, "@(\"a\"; null)");
}

#[test]
fn autocast_re_types_from_the_cells_present() {
    let d = fixture();
    let (h, e, ok) = qua(&d, "/row @| # | [:Note != \"a\"] | autocast | header");
    assert!(ok, "{e}");
    assert_eq!(h, "@(:Name<*; text>; :Age<*; int>; :Note<*; int>)");
    let (out, _, _) = qua(&d, "/row @| # | [:Note != \"a\"] | autocast | :Note");
    assert_eq!(out, "@(1; 2; null)");
    // the named columns alone
    let (h, _, _) = qua(
        &d,
        "/row @| # | [:Note != \"a\"] | autocast(:Note) | header",
    );
    assert_eq!(h, "@(:Name<*; text>; :Age<*; int>; :Note<*; int>)");
    let (h, _, _) = qua(&d, "/row @| # | [:Note != \"a\"] | autocast(:Age) | header");
    assert_eq!(h, TEXT);
    // nothing to narrow: the same table
    let (h, _, _) = qua(&d, "/row @| # | autocast | header");
    assert_eq!(h, TEXT);
    // a declared column stays as declared, and is refused when named
    let (h, e, ok) = qua(
        &d,
        "/row @| # | [:Note != \"a\"] | #(:Note<text>; :Age) | autocast | header",
    );
    assert!(ok, "{e}");
    assert_eq!(h, "@(:Note<text>; :Age<*; int>)");
    let (h, e, ok) = qua(
        &d,
        "/row @| # | [:Note != \"a\"] | #(:Note<text>; :Age) | autocast(:Note) | header",
    );
    assert!(ok, "{e}");
    assert_eq!(h, "@(:Note<text>; :Age<*; int>)");
    // the named form of the undeclaring map, on the undeclared columns
    let (a, _, _) = qua(&d, "/row @| # | [:Note != \"a\"] | autocast(:Note)");
    let (b, _, _) = qua(&d, "/row @| # | [:Note != \"a\"] | map(<*>; --to=:Note)");
    assert_eq!(a, b);
    let (a, _, _) = qua(&d, "/row @| # | [:Note != \"a\"] | autocast");
    let (b, _, _) = qua(&d, "/row @| # | [:Note != \"a\"] | map(<*>)");
    assert_eq!(a, b);
    // the refusals
    let (_, e, ok) = qua(&d, "/row @| # | autocast(:Nope)");
    assert!(!ok);
    assert!(
        e.contains("no such column (its columns: Name, Age, Note)"),
        "{e}"
    );
    let (_, e, ok) = qua(&d, "/row @| # | autocast(1)");
    assert!(!ok);
    assert!(e.contains("names its columns bare"), "{e}");
    // a list is one column; a record is neither
    let (out, _, _) = qua(&d, "= @(\"1\"; \"2\"; null) | autocast");
    assert_eq!(out, "@(1; 2; null)");
    let (out, _, _) = qua(&d, "= @(\"1\"; \"x\") | autocast");
    assert_eq!(out, "@(\"1\"; \"x\")");
    let (_, e, ok) = qua(&d, "= %(a = \"1\") | autocast");
    assert!(!ok);
    assert!(e.contains("a record is neither"), "{e}");
}
