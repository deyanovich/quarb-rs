//! A document's table read with `| #` (spec: The Table, the table
//! constructor on a node; the document-tables round, rulings
//! 2026-11-08): the body rows under the header the format declares,
//! footer rows kept out, the sections readable, a nested table
//! refused by its place, every cell as text so a column of numbers
//! types and a text column keeps its cells as written, footnote marks
//! dropped and a break a space, the null vocabulary and the locale,
//! the row-header index, the options applied on the node, and the
//! warning on a column that falls to text.

use std::path::{Path, PathBuf};

const PORTS_HTML: &str = r##"<!doctype html><html><body>
<h1>Ports</h1>
<table class="wikitable">
<caption>Busiest ports</caption>
<thead><tr><th rowspan="2">Port</th><th colspan="2">Traffic</th><th rowspan="2">Country</th></tr>
<tr><th>2022</th><th>2023</th></tr></thead>
<tbody>
<tr><th>Piraeus</th><td>5,000,000</td><td>5,200,000<sup><a href="#n1">[1]</a></sup></td><td><a href="/gr">Greece</a></td></tr>
<tr><th>Koper</th><td>1,017,788</td><td>—</td><td>Slovenia</td></tr>
<tr><th>Rijeka</th><td>330.5 k</td><td>12 %</td><td></td></tr>
<tr><th>Bar</th><td>€ 1 234,50</td><td>n/a</td><td>Montenegro<br>(Crna Gora)</td></tr>
</tbody>
<tfoot><tr><td>Total</td><td>6,348,288</td><td></td><td></td></tr></tfoot>
</table>
<table><tr><td>1</td><td>2</td></tr><tr><td>3</td><td>4</td></tr></table>
<table><tr><th>k</th><th>v</th></tr><tr><td>a</td><td><table><tr><td>inner</td></tr></table></td></tr></table>
<table><tr><th>Name</th><th>Pages</th></tr><tr><td>Iliad</td><td>1.234,5</td></tr><tr><td>Odyssey</td><td>987,25</td></tr></table>
</body></html>
"##;

const PORTS_MD: &str = "# Ports\n\n| Port | 2022 | 2023 | Country |\n|:-----|-----:|-----:|:--------|\n| Piraeus | 5,000,000 | 5,200,000 [1] | [Greece](/gr) |\n| Koper | 1,017,788 | — | Slovenia |\n| Rijeka | 330.5 k | 12 % | |\n| Bar | `1234.50` | n/a | Montenegro<br>(Crna Gora) |\n";

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-doctables-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("ports.html"), PORTS_HTML).unwrap();
    std::fs::write(dir.join("ports.md"), PORTS_MD).unwrap();
    std::fs::write(
        dir.join("na.csv"),
        "species,body_mass_g\nAdelie,3750\nAdelie,NA\nGentoo,3800\n",
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

#[test]
fn the_table_reads_its_cells_as_text_and_keeps_the_footer_out() {
    let d = fixture("read");
    let t = ok(&d, &["//table[1] | #", "ports.html"]);
    assert_eq!(
        t,
        "#(@(Port; \"2022\"; \"2023\"; Country);\n  \
           %(\"Piraeus\"; \"5,000,000\"; \"5,200,000\"; \"Greece\");\n  \
           %(\"Koper\"; \"1,017,788\"; null; \"Slovenia\");\n  \
           %(\"Rijeka\"; \"330.5 k\"; \"12 %\"; null);\n  \
           %(\"Bar\"; \"€ 1 234,50\"; null; \"Montenegro (Crna Gora)\"))"
    );
    // the two-row header resolves through the spans; the footer is a
    // node still
    assert_eq!(ok(&d, &["//table[1] | # | count", "ports.html"]), "4");
    assert_eq!(
        ok(&d, &["//table[1]/tfoot/tr/td[1] | ::", "ports.html"]),
        "Total"
    );
    // the row headers are the index
    assert_eq!(
        ok(&d, &["//table[1] | # | :::index", "ports.html"]),
        r#"@("Piraeus"; "Koper"; "Rijeka"; "Bar")"#
    );
    assert_eq!(
        ok(&d, &["//table[1] | # | [[\"Koper\"]]", "ports.html"]),
        r#"%(Port = "Koper"; "2022" = "1,017,788"; "2023" = null; Country = "Slovenia")"#
    );
    // numeric text sums where the column stayed text
    assert_eq!(
        ok(&d, &["//table[1] | # | :\"2023\" | sum", "ports.html"]),
        "5200000"
    );
    // the Markdown table lowers to the same shape
    let md = ok(&d, &["//*<tabula>[1] | #", "ports.md"]);
    assert!(
        md.contains(r#"%("Piraeus"; "5,000,000"; "5,200,000"; "Greece")"#),
        "{md}"
    );
    assert!(md.contains(r#""Montenegro (Crna Gora)""#), "{md}");
    assert!(md.contains(r#"%("Bar"; "1234.50"; null;"#), "{md}");
}

#[test]
fn the_sections_read_as_tables_of_their_rows() {
    let d = fixture("sections");
    let body = ok(&d, &["//table[1]/tbody | #", "ports.html"]);
    assert!(
        body.starts_with("#(@(Port; \"2022\"; \"2023\"; Country);"),
        "{body}"
    );
    assert_eq!(ok(&d, &["//table[1]/tbody | # | count", "ports.html"]), "4");
    assert_eq!(
        ok(&d, &["//table[1]/tfoot | #", "ports.html"]),
        "#(@(Port; \"2022\"; \"2023\"; Country);\n  %(\"Total\"; 6348288; null; null))"
    );
    assert_eq!(
        ok(&d, &["//table[1]/thead | #", "ports.html"]),
        "#(@(\"Port\"; \"Traffic\"; \"Traffic\"; \"Country\");\n  @(\"Port\"; \"2022\"; \"2023\"; \"Country\"))"
    );
    // a headerless grid is an array; a nested table refuses by its place
    assert_eq!(
        ok(&d, &["//table[2] | #", "ports.html"]),
        "#(@(1; 2);\n  @(3; 4))"
    );
    let err = refused(&d, &["//table[3] | #", "ports.html"]);
    assert!(
        err.contains(
            "a nested table at row 2, column 2 is not read with its outer one: address it on its \
             own ('//*<tabula>')"
        ),
        "{err}"
    );
}

#[test]
fn the_options_apply_on_the_node() {
    let d = fixture("options");
    assert_eq!(
        ok(
            &d,
            &["//table[1] | #(-i=:Country) | :::index", "ports.html"]
        ),
        r#"@("Greece"; "Slovenia"; null; "Montenegro (Crna Gora)")"#
    );
    // `-i=null` keeps the row headers a column
    assert_eq!(
        ok(&d, &["//table[1] | #(-i=null) | :::index", "ports.html"]),
        "null"
    );
    assert_eq!(
        ok(&d, &["//table[1] | #(-i=null) | :::header", "ports.html"]),
        "@(:Port<*; text>; :2022<*; text>; :2023<*; text>; :Country<*; text>)"
    );
    assert_eq!(
        ok(
            &d,
            &["//table[1] | #(-c=:Country<text>) | :Country", "ports.html"]
        ),
        r#"@("Greece"; "Slovenia"; null; "Montenegro (Crna Gora)")"#
    );
    // --width has no meaning on a node
    let err = refused(&d, &["//table[1] | #(--width=2)", "ports.html"]);
    assert!(
        err.contains("#(--width=n) lays a stream of values in rows; a table node has its rows"),
        "{err}"
    );
    // the null vocabulary: the default, a given list, none
    assert_eq!(
        ok(
            &d,
            &[
                "//table[1] | #(--null=@(\"—\"; \"n/a\"; \"330.5 k\"; \"12 %\")) | :::header",
                "ports.html"
            ]
        ),
        "@(:Port<*; text>; :2022<*; text>; :2023<*; int>; :Country<*; text>)"
    );
    // (a column of an indexed table reads as the record over the index)
    let bare = ok(
        &d,
        &["//table[1] | #(--null=@()) | :\"2023\"", "ports.html"],
    );
    assert_eq!(
        bare,
        r#"%(Piraeus = "5,200,000"; Koper = "—"; Rijeka = "12 %"; Bar = "n/a")"#
    );
    // the locale: numbers written the German way
    assert_eq!(
        ok(
            &d,
            &["//table[5] | #(--locale=\"de\") | :::header", "ports.html"]
        ),
        "@(:Name<*; text>; :Pages<*; float>)"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "//table[5] | #(--locale=\"de\") | :Pages | sum",
                "ports.html"
            ]
        ),
        "2221.75"
    );
    assert_eq!(
        ok(&d, &["//table[5] | # | :::header", "ports.html"]),
        "@(:Name<*; text>; :Pages<*; text>)"
    );
    // on a stream table the same options pass over the cells
    assert_eq!(
        ok(&d, &["/row @| #(--null=@(\"NA\")) | :::header", "na.csv"]),
        "@(:species<*; text>; :body_mass_g<*; int>)"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "/row @| # | #(--null=@(\"NA\")) | :body_mass_g | mean",
                "na.csv"
            ]
        ),
        "3775.0"
    );
}

#[test]
fn a_column_that_falls_to_text_is_told() {
    let d = fixture("warn");
    let (_, _, err) = qua(&d, &["//table[1] | #(--null=@()) | count", "ports.html"]);
    assert!(
        err.contains(
            "warning TABLE_TEXT (/html/body/table[1]): column '2023' falls to text: row 2 holds \
             \"—\" (a null word: --null), row 3 holds \"12 %\" (a percent), row 4 holds \"n/a\" \
             (a null word: --null); cleanup: #(--null=@(\"n/a\"; \"—\")), or map(number; \
             --to=:2023) after the suffix is split off"
        ),
        "{err}"
    );
    assert!(
        err.contains(
            "column '2022' falls to text: row 3 holds \"330.5 k\" (a scale suffix); cleanup: \
             map(number; --to=:2022) after the suffix is split off"
        ),
        "{err}"
    );
    // a column of plain text raises nothing; --diagnostics=error silences
    assert!(!err.contains("column 'Country'"), "{err}");
    assert!(!err.contains("column 'Port'"), "{err}");
    let (_, _, quiet) = qua(
        &d,
        &[
            "--diagnostics=error",
            "//table[1] | # | count",
            "ports.html",
        ],
    );
    assert!(!quiet.contains("TABLE_TEXT"), "{quiet}");
}

#[test]
fn clean_turns_a_scraped_tables_text_into_values() {
    let d = fixture("clean");
    // the value forms
    assert_eq!(ok(&d, &["= \"330.5 k\" | clean"]), "330500");
    assert_eq!(ok(&d, &["= \"12 %\" | clean"]), "0.12");
    assert_eq!(ok(&d, &["= \"5,200,000[1]\" | clean"]), "5200000");
    assert_eq!(ok(&d, &["= \"n/a\" | clean"]), "null");
    assert_eq!(ok(&d, &["= \"—\" | clean(--null=@())"]), "—");
    assert_eq!(
        ok(&d, &["= \"1.234,5\" | clean(--locale=\"de\")"]),
        "1234.5"
    );
    assert_eq!(ok(&d, &["= \"2 bn\" | clean"]), "2000000000");
    assert_eq!(ok(&d, &["= \" Koper †\" | clean"]), "Koper");
    assert_eq!(
        ok(&d, &["= @(\"330.5 k\"; \"n/a\"; \"7\") | clean"]),
        "@(330500; null; 7)"
    );
    // the table: every text column cleaned and retyped, the index kept
    let header = ok(
        &d,
        &[
            "//table[1] | #(--null=@()) | clean | :::header",
            "ports.html",
        ],
    );
    assert_eq!(
        header,
        "@(:Port<*; text>; :2022<*; text>; :2023<*; float>; :Country<*; text>)"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "//table[1] | #(--null=@()) | clean | :\"2023\"",
                "ports.html"
            ]
        ),
        // (an int and a ratio share the column: it types float)
        "%(Piraeus = 5200000.0; Koper = null; Rijeka = 0.12; Bar = null)"
    );
    // the euro cell keeps the 2022 column text; the named form
    // cleans that column alone, the rest untouched
    let one = ok(
        &d,
        &[
            "//table[1] | #(--null=@()) | clean(:\"2023\") | :::header",
            "ports.html",
        ],
    );
    assert!(
        one.contains(":2022<*; text>") && one.contains(":2023<*; float>"),
        "{one}"
    );
    let err = refused(&d, &["//table[1] | # | clean(:Traffic)", "ports.html"]);
    assert!(
        err.contains("clean names column 'Traffic', and the table has no such column"),
        "{err}"
    );
}
