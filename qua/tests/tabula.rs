//! The `tabula` trait and `| #` on a tabula node (spec: The Table):
//! a table carries the trait in every substrate, `//*<tabula>` finds
//! it, and `| #` reads it as a table — the text model's lowered table
//! as a frame keyed by the cells' lemmas, an HTML or XML grid with
//! its declared header, spans repeated, cells as CSV cells.

fn qua(args: &[&str]) -> (bool, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn ok(args: &[&str]) -> String {
    let (ok, out, err) = qua(args);
    assert!(ok, "qua {args:?}: {err}");
    out
}

#[test]
fn a_table_is_a_tabula_in_every_substrate_and_reads_as_a_table() {
    let dir = std::env::temp_dir().join(format!("quarb-tabula-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let md = dir.join("crew.md");
    std::fs::write(
        &md,
        "# Crew\n\n| name | role | age |\n|---|---|---|\n| Alice | captain | 41 |\n| Bob |  | 29 |\n",
    )
    .unwrap();
    let html = dir.join("crew.html");
    std::fs::write(
        &html,
        r#"<html><body><table><caption>Crew</caption>
<tr><th>name</th><th>role</th><th>age</th></tr>
<tr><td>Alice</td><td>captain</td><td>41</td></tr>
<tr><td>Bob</td><td colspan="2">29</td></tr>
</table><table><tr><td>1</td><td>2</td></tr><tr><td>3</td><td>4</td></tr></table></body></html>"#,
    )
    .unwrap();
    let xml = dir.join("crew.xml");
    std::fs::write(
        &xml,
        r#"<TEI xmlns="http://www.tei-c.org/ns/1.0"><text><body><table>
<row role="label"><cell>name</cell><cell>age</cell></row>
<row><cell>Alice</cell><cell>41</cell></row>
<row><cell>Bob</cell><cell/></row>
</table></body></text></TEI>"#,
    )
    .unwrap();
    let text_md = format!("text:{}", md.display());
    let html_s = html.display().to_string();
    let xml_s = xml.display().to_string();

    // the text model: the lowered table carries tabula, its cells
    // their column as lemma; the frame is keyed by them and Bob's
    // empty cell is a hole
    assert_eq!(ok(&["//*<tabula>::lemma", &text_md]), "\n");
    assert_eq!(
        ok(&["//*<tabula> | # | header", &text_md]),
        "@(\"name\"; \"role\"; \"age\")\n"
    );
    assert_eq!(
        ok(&["//*<tabula> | # | [2]", &text_md]),
        "%(name = \"Bob\"; role = null; age = 29)\n"
    );
    assert_eq!(ok(&["//*<tabula> | # | cell(1; \"age\")", &text_md]), "41\n");
    // the DOM: a table element carries tabula; a first row of th is
    // the header, a colspan repeats the cell, a number reads as one
    assert_eq!(ok(&["//*<tabula> @| count", &html_s]), "2\n");
    assert_eq!(
        ok(&["//*<tabula>[1] | # | [2]", &html_s]),
        "%(name = \"Bob\"; role = 29; age = 29)\n"
    );
    assert_eq!(ok(&["//*<tabula>[1] | # | dim", &html_s]), "@(2; 3)\n");
    // no header declared: the array face
    assert_eq!(
        ok(&["//*<tabula>[2] | #", &html_s]),
        "#(%(1; 2);\n  %(3; 4))\n"
    );
    assert_eq!(ok(&["//*<tabula>[2] | # | transpose | [1]", &html_s]), "@(1; 3)\n");
    // XML: TEI's table, its label row the header, an empty cell null
    assert_eq!(
        ok(&["//*<tabula> | # | [2]", &xml_s]),
        "%(name = \"Bob\"; age = null)\n"
    );
    // not a table: refused with the pointer
    let (ok_, _, err) = qua(&["/html/body | #", &html_s]);
    assert!(!ok_ && err.contains("is not a table") && err.contains("//*<tabula>"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
