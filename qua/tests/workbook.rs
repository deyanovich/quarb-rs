//! Workbooks at the command line: the macro-enabled, binary and
//! add-in forms are workbooks (they once opened as the zip they are
//! packed in), and a worksheet the reader fails on is left out with
//! a warning, once, while a chart sheet is left out without a word
//! (ruling 2026-11-13; the 0.63 queue).

fn run(args: &[&str]) -> (String, String, bool) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
        out.status.success(),
    )
}

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn the_excel_variants_are_workbooks() {
    let dir = std::env::temp_dir().join(format!("quarb-workbook-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for ext in ["xlsm", "xlam", "XLSM"] {
        let f = dir.join(format!("ledger.{ext}"));
        std::fs::copy(fixture("ledger.xlsx"), &f).unwrap();
        let (out, err, ok) = run(&["/* | :name", f.to_str().unwrap()]);
        assert!(ok, "{ext}: {err}");
        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            ["scrolls", "loans"],
            "{ext}"
        );
        let (out, _, _) = run(&[
            r#"/scrolls/*[::shelf = "epic"] | ::title"#,
            f.to_str().unwrap(),
        ]);
        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            ["Iliad", "Odyssey", "Theogony"],
            "{ext}"
        );
    }
    // a binary workbook goes to the workbook reader as well: this
    // file is none, and it is the reader that says so, not the
    // archive listing its parts
    let f = dir.join("ledger.xlsb");
    std::fs::copy(fixture("ledger.xlsx"), &f).unwrap();
    let (out, err, ok) = run(&["/* | :name", f.to_str().unwrap()]);
    assert!(!ok && out.is_empty(), "{out}");
    assert!(err.contains("opening workbook"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_sheet_that_cannot_be_read_is_told_of_once() {
    let f = fixture("torn.xlsx");
    // the sheet that reads is there; the torn one and the chart
    // sheet are not
    let (out, err, ok) = run(&["/* | :name", &f]);
    assert!(ok, "{err}");
    assert_eq!(out.lines().collect::<Vec<_>>(), ["scrolls"]);
    let warnings: Vec<&str> = err
        .lines()
        .filter(|l| l.contains("UNREADABLE_SHEET"))
        .collect();
    assert_eq!(warnings.len(), 1, "{err}");
    assert!(
        warnings[0].starts_with(
            "warning UNREADABLE_SHEET (mount): the sheet 'torn' could not be read and is left out: "
        ),
        "{err}"
    );
    assert!(!err.contains("chart"), "{err}");
    // the answer is the readable sheet's
    let (out, _, _) = run(&["/scrolls/* | ::lines @| sum", &f]);
    assert_eq!(out.trim(), "28825");
    // not shown where warnings are not
    let (_, err, _) = run(&["--diagnostics=error", "/* | :name", &f]);
    assert!(!err.contains("UNREADABLE_SHEET"), "{err}");
}
