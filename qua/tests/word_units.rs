//! Units written as words in a column title (the header-units round,
//! ruling 2026-11-09): `--autounits` reads a title's tail that is no
//! symbol through the engine's dictionary — `metric tonnes per year`
//! → `t/a`, `[millions of cubic feet per second]` → `ft^3/s` with the
//! cells ×1e6 (a scale is always a cell factor), `$m` → USD ×1e6,
//! `тыс. тонн в год` → `t/a` ×1000; `tons` alone is the short ton,
//! told; `pounds` alone is ambiguous and skipped with the pointer; a
//! tail that nearly reads is skipped and told. The dictionary override
//! `--units=%(word = _u)`, the column-scoped `--unit:col=%(…)` and the
//! declaration `--unit:col=_u`; the column-scoped `--null:col` and
//! `--locale:col`; `?units=` on the mount; the word road in
//! `convert`, `12_tonnes` and unit text; the 15-digit display of a
//! computed quantity.

use std::path::{Path, PathBuf};

const WORDS_HTML: &str = r##"<!doctype html><html><body>
<table>
<thead><tr><th>Port</th><th>Throughput, metric tonnes per year</th><th>Flow [millions of cubic feet per second]</th><th>Revenue, $m</th><th>Area (square kilometres)</th><th>Cargo, tons</th><th>Weight, pounds</th><th>Growth, tonnes per yer</th><th>Грузооборот, тыс. тонн в год</th><th>Share, percent</th><th>Opened, at</th></tr></thead>
<tbody>
<tr><td>Piraeus</td><td>5200000</td><td>1.5</td><td>120</td><td>12.4</td><td>300</td><td>40</td><td>3</td><td>5200</td><td>12</td><td>1700000000</td></tr>
<tr><td>Koper</td><td>1017788</td><td>0.4</td><td>80.5</td><td>3.3</td><td>120</td><td>25</td><td>2</td><td>1018</td><td>2.2</td><td>1700003600</td></tr>
</tbody>
</table>
</body></html>
"##;

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-wordunits-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("words.html"), WORDS_HTML).unwrap();
    std::fs::write(
        dir.join("words.csv"),
        "Port,Throughput (metric tonnes per year),Revenue $m,Area in square kilometres,Cargo tons\n\
         Piraeus,5200000,120,12.4,300\nKoper,1017788,80.5,3.3,120\n",
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
fn a_titles_words_name_its_unit_and_its_scale() {
    let d = fixture("words");
    assert_eq!(
        ok(
            &d,
            &["//table[1] | #(--autounits) | :::header", "words.html"]
        ),
        "@(:Port<*; text>; :Throughput<_t/a>; :Flow<_ft^3/s>; :Revenue<_USD>; :Area<_km^2>; \
         :Cargo<_ton>; :'Weight, pounds'<*; int>; :'Growth, tonnes per yer'<*; int>; \
         :Грузооборот<_t/a>; :Share<_%>; :'Opened, at'<*; int>)"
    );
    // the cells scaled by the words' factor, the unit the title names
    assert_eq!(
        ok(&d, &["//table[1] | #(--autounits) | :Flow", "words.html"]),
        "@(1500000_ft^3/s; 400000_ft^3/s)"
    );
    assert_eq!(
        ok(
            &d,
            &["//table[1] | #(--autounits) | :Revenue", "words.html"]
        ),
        "@(120000000_USD; 80500000_USD)"
    );
    assert_eq!(
        ok(
            &d,
            &["//table[1] | #(--autounits) | :Грузооборот", "words.html"]
        ),
        "@(5200000_t/a; 1018000_t/a)"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "//table[1] | #(--autounits) | :Throughput | first | convert(t/d)",
                "words.html"
            ]
        ),
        "14236.8240930869_t/d"
    );
    let (_, _, err) = qua(&d, &["--info", "//table[1] | #(--autounits)", "words.html"]);
    assert!(
        err.contains(
            "Throughput, metric tonnes per year → Throughput in t/a, Flow [millions of cubic feet \
             per second] → Flow in ft^3/s, cells ×1e6, Revenue, $m → Revenue in USD, cells ×1e6, \
             Area (square kilometres) → Area in km^2, Cargo, tons → Cargo in ton (short tons; \
             write tonnes for t), Грузооборот, тыс. тонн в год → Грузооборот in t/a, cells ×1000, \
             Share, percent → Share in %; skipped: Growth, tonnes per yer ('yer' is no unit)"
        ),
        "{err}"
    );
    // the mount reads the same words; a comma-less title takes its
    // longest tail
    let t = "words.csv?autounits";
    assert_eq!(
        ok(&d, &["/row[1] | :props", t]),
        "@(::Port; ::Throughput; ::Revenue; ::Area; ::Cargo)"
    );
    assert_eq!(ok(&d, &["/row[1] | ::Throughput", t]), "5200000_t/a");
    assert_eq!(ok(&d, &["/row[1] | ::Revenue", t]), "120000000_USD");
    assert_eq!(ok(&d, &["/row @| # | :Area", t]), "@(12.4_km^2; 3.3_km^2)");
    assert_eq!(ok(&d, &["/row[1] | ::Cargo", t]), "300_ton");
    let (_, _, err) = qua(&d, &["--info", "/row[1] | ::Area", t]);
    assert!(
        err.contains("Area in square kilometres → Area in km^2"),
        "{err}"
    );
}

#[test]
fn the_dictionary_override_and_the_declaration() {
    let d = fixture("override");
    // `--units`: the words of every title, before the dictionary; the
    // reading implied
    assert_eq!(
        ok(
            &d,
            &[
                "//table[1] | #(--units=%(pounds = _lb)) | :Weight",
                "words.html"
            ]
        ),
        "@(40_lb; 25_lb)"
    );
    // `--unit:col=%(…)`: that column's words alone
    assert_eq!(
        ok(
            &d,
            &[
                "//table[1] | #(--unit:\"Cargo, tons\"=%(tons = _t)) | :Cargo",
                "words.html"
            ]
        ),
        "@(300_t; 120_t)"
    );
    let h = ok(
        &d,
        &[
            "//table[1] | #(--autounits --unit:\"Cargo, tons\"=%(tons = _t) --unit:\"Weight, pounds\"=%(pounds = _GBP)) | :::header",
            "words.html",
        ],
    );
    assert!(
        h.contains(":Cargo<_t>") && h.contains(":Weight<_GBP>"),
        "{h}"
    );
    // `--unit:col=_u`: the column declared, its unit tail dropped
    let (_, out, err) = qua(
        &d,
        &[
            "--info",
            "//table[1] | #(--unit:\"Weight, pounds\"=_lb) | :Weight",
            "words.html",
        ],
    );
    assert_eq!(out, "@(40_lb; 25_lb)");
    assert!(
        err.contains("Weight, pounds → Weight in lb (declared)"),
        "{err}"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "//table[1] | #(--unit:\"Share, percent\"=_%) | :Share",
                "words.html"
            ]
        ),
        "@(12_%; 2.2_%)"
    );
    // the same on clean
    assert_eq!(
        ok(
            &d,
            &[
                "//table[1] | # | clean(--units=%(pounds = _lb) --autounits) | :Weight",
                "words.html"
            ]
        ),
        "@(40_lb; 25_lb)"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "//table[1] | # | clean(--unit:\"Weight, pounds\"=_lb) | :Weight",
                "words.html"
            ]
        ),
        "@(40_lb; 25_lb)"
    );
    // the mount: `?units=word:_u`
    assert_eq!(
        ok(
            &d,
            &["/row[1] | ::Cargo", "words.csv?autounits&units=tons:_t"]
        ),
        "300_t"
    );
    assert_eq!(
        ok(&d, &["/row[1] | ::Cargo", "words.csv?units=tons:_t"]),
        "300_t"
    );
    // refusals
    let err = refused(&d, &["//table[1] | #(--unit=_lb)", "words.html"]);
    assert!(err.contains("'--unit' takes a column"), "{err}");
    let err = refused(
        &d,
        &[
            "//table[1] | #(--unit:\"Weight, pounds\"=_nope)",
            "words.html",
        ],
    );
    assert!(err.contains("'nope' is no unit the table knows"), "{err}");
    let err = refused(
        &d,
        &[
            "//table[1] | #(--units=%(pounds = _nope) --autounits)",
            "words.html",
        ],
    );
    assert!(err.contains("'nope' is no unit the table knows"), "{err}");
    let err = refused(
        &d,
        &[
            "//table[1] | #(--unit:\"Weight, pounds\"=%(pounds = 3))",
            "words.html",
        ],
    );
    assert!(err.contains("maps to a number, not a unit"), "{err}");
    let err = refused(&d, &["/row[1] | ::Cargo", "words.csv?units=tons"]);
    assert!(err.contains("units= takes word:unit pairs"), "{err}");
}

#[test]
fn the_column_scoped_options() {
    let d = fixture("scopes");
    assert_eq!(
        ok(
            &d,
            &[
                "//table[1] | #(--null:Port=@(\"Koper\")) | :Port",
                "words.html"
            ]
        ),
        "@(\"Piraeus\"; null)"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "//table[1] | #(--locale:\"Revenue, $m\"=\"de\") | :\"Revenue, $m\"",
                "words.html"
            ]
        ),
        "@(120.0; 80.5)"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "//table[1] | # | clean(--null:Port=@(\"Koper\")) | :Port",
                "words.html"
            ]
        ),
        "@(\"Piraeus\"; null)"
    );
    let err = refused(&d, &["//table[1] | #(--null:Nope=@(\"x\"))", "words.html"]);
    assert!(
        err.contains("'Nope' names no column: the table's columns are Port,"),
        "{err}"
    );
    let err = refused(&d, &["//table[1] | #(--index:Port=:Port)", "words.html"]);
    assert!(err.contains("'--index' takes no column scope"), "{err}");
    let err = refused(
        &d,
        &["//table[1] | # | sum(--null:Port=@(\"x\"))", "words.html"],
    );
    assert!(
        err.contains("'sum' takes no column scope on '--null'"),
        "{err}"
    );
}

#[test]
fn the_word_road_in_commands_and_text_and_the_fifteen_digit_display() {
    let d = fixture("road");
    assert_eq!(ok(&d, &["= 12_tonnes | convert(t)", "words.html"]), "12_t");
    assert_eq!(ok(&d, &["= 12_tonnes = 12_t", "words.html"]), "true");
    assert_eq!(ok(&d, &["= 5 | unit(tonnes)", "words.html"]), "5_tonnes");
    assert_eq!(
        ok(&d, &["= 3_t | convert(tons)", "words.html"]),
        "3.30693393277316_tons"
    );
    assert_eq!(ok(&d, &["= 2_a | convert(d)", "words.html"]), "730.5_d");
    assert_eq!(
        ok(&d, &["= \"5 metric tonnes\" | clean", "words.html"]),
        "5_t"
    );
    assert_eq!(ok(&d, &["= \"12 km\" | clean", "words.html"]), "12_km");
    assert_eq!(
        ok(&d, &["= \"12 million dollars\" | clean", "words.html"]),
        "12000000_USD"
    );
    assert_eq!(ok(&d, &["= 12_€ = 12_EUR", "words.html"]), "true");
    // a computed quantity's written magnitude rounds to 15
    // significant digits; literals and bare floats as they are
    assert_eq!(
        ok(&d, &["= @(12_cm; 2.2_cm; 0.5_cm) | mean", "words.html"]),
        "4.9_cm"
    );
    assert_eq!(
        ok(&d, &["= @(120_g; 22_g; 5_g) | mean", "words.html"]),
        "49_g"
    );
    assert_eq!(ok(&d, &["= @(12_%; 2_%) | sum", "words.html"]), "14_%");
    assert_eq!(ok(&d, &["= 3.14_rad * 2 * 10_m", "words.html"]), "62.8_m");
    assert_eq!(
        ok(&d, &["= @(39.1_mm; 39.5_mm; 46.1_mm) | mean", "words.html"]),
        "41.5666666666667_mm"
    );
    assert_eq!(
        ok(&d, &["= 41.56666666666667_mm", "words.html"]),
        "41.56666666666667_mm"
    );
    assert_eq!(
        ok(&d, &["= 0.1 + 0.2", "words.html"]),
        "0.30000000000000004"
    );
}

#[test]
fn a_rounded_parameter_is_the_parameter_and_a_dictionary_word_is_the_unit() {
    // lang's finding on dev-17878c09: `_l` in a fragment body read as
    // a bare unit and shadowed the parameter; the binding wins — the
    // suffix form is a unit only where no parameter stands (an
    // option's value, a dictionary record)
    let d = fixture("params");
    std::fs::write(
        d.join("org.json"),
        r#"{"teams": [{"members": [{"name": "di", "level": "junior"}, {"name": "ana", "level": "senior"}]}, {"members": [{"name": "fi", "level": "junior"}]}]}"#,
    )
    .unwrap();
    assert_eq!(
        ok(
            &d,
            &[
                "def (=lvl)(_l): /teams/*/members/*[::level = _l]; (=lvl)(\"junior\")::name",
                "org.json"
            ]
        ),
        "di\nfi"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "def &lvl(_l): /teams/*/members/*[::level = _l]; &lvl(\"senior\")::name",
                "org.json"
            ]
        ),
        "ana"
    );
    // a bare `_l` outside a fragment is no value
    let (success, _, _) = qua(&d, &["= _l", "org.json"]);
    assert!(!success, "a bare parameter outside a fragment is refused");
    // the dictionary record and the scoped declaration still read the
    // suffix form
    assert_eq!(
        ok(
            &d,
            &[
                "//table[1] | #(--units=%(pounds = _lb)) | :Weight",
                "words.html"
            ]
        ),
        "@(40_lb; 25_lb)"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "//table[1] | #(--unit:\"Weight, pounds\"=_lb) | :Weight",
                "words.html"
            ]
        ),
        "@(40_lb; 25_lb)"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "//table[1] | # | clean(--units=%(pounds = _lb) --autounits) | :Weight",
                "words.html"
            ]
        ),
        "@(40_lb; 25_lb)"
    );
}
