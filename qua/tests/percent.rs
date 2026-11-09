//! The percent unit and the dimension-one rule (ruling 2026-11-09):
//! `%` is a built-in unit of dimension one with factor 1/100 of the
//! ratio — `12_%` holds 0.12 and prints as written; a quantity of
//! dimension one (a percent, an angle) computes with a bare number
//! and with a dimensioned quantity as the number it is, two of one
//! unit add to that unit, a product or quotient of two is a bare
//! number; it orders against a bare number in the base and never
//! equals one; `convert(%)` dresses a number again. A `%` column
//! title declares the column in `%`, `clean` makes `12_%` of "12 %",
//! and a column mixing percents and bare numbers types as numbers.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-percent-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("shares.html"),
        "<!doctype html><table><thead><tr><th>Port</th><th>Share, %</th><th>2023</th></tr></thead>\
         <tbody><tr><td>Piraeus</td><td>12</td><td>5,200,000</td></tr>\
         <tr><td>Koper</td><td>2.2</td><td>12 %</td></tr>\
         <tr><td>Rijeka</td><td>0.5</td><td>n/a</td></tr></tbody></table>\n",
    )
    .unwrap();
    std::fs::write(dir.join("one.json"), "{\"x\": 1}").unwrap();
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
fn the_percent_is_a_unit_of_dimension_one() {
    let d = fixture("unit");
    assert_eq!(ok(&d, &["= 12_%", "one.json"]), "12_%");
    assert_eq!(ok(&d, &["= 12_% | type", "one.json"]), "quantity");
    assert_eq!(ok(&d, &["= 12_% | dimension", "one.json"]), "1");
    assert_eq!(ok(&d, &["= 12_% | magnitude", "one.json"]), "12");
    assert_eq!(ok(&d, &["= 12_% | unit", "one.json"]), "%");
    // the base form of dimension one is the bare ratio
    assert_eq!(ok(&d, &["= 12_% | convert", "one.json"]), "0.12");
    // a number dressed again, bare `%` or quoted
    assert_eq!(ok(&d, &["= 0.12 | convert(%)", "one.json"]), "12_%");
    assert_eq!(ok(&d, &["= 0.12 | convert(\"%\")", "one.json"]), "12_%");
    assert_eq!(ok(&d, &["= 12 | unit(%)", "one.json"]), "12_%");
    assert_eq!(ok(&d, &["= 12_% | magnitude(%)", "one.json"]), "12");
    assert_eq!(ok(&d, &["= 0.12 | convert(rad)", "one.json"]), "0.12_rad");
    // unit text, with a locale
    assert_eq!(ok(&d, &["= \"12 %\" | clean", "one.json"]), "12_%");
    assert_eq!(
        ok(&d, &["= \"2,5 %\" | clean(--locale=de)", "one.json"]),
        "2.5_%"
    );
    // the record sigil and the modulo are untouched
    assert_eq!(ok(&d, &["= %(a = 1) | :a", "one.json"]), "1");
}

#[test]
fn a_quantity_of_dimension_one_computes_as_the_number_it_is() {
    let d = fixture("arith");
    // with a bare number: plain arithmetic in the base
    assert_eq!(ok(&d, &["= 12_% * 50", "one.json"]), "6.0");
    assert_eq!(ok(&d, &["= 50 * 12_%", "one.json"]), "6.0");
    assert_eq!(ok(&d, &["= 1 - 12_%", "one.json"]), "0.88");
    assert_eq!(ok(&d, &["= 12_% + 1", "one.json"]), "1.12");
    assert_eq!(ok(&d, &["= 12_% div 4", "one.json"]), "0.03");
    assert_eq!(ok(&d, &["= 100 div 50_%", "one.json"]), "200.0");
    assert_eq!(ok(&d, &["= 2_rad * 3", "one.json"]), "6.0");
    // two of one unit keep it; a product or quotient of two is a number
    assert_eq!(ok(&d, &["= 12_% + 3_%", "one.json"]), "15_%");
    assert_eq!(ok(&d, &["= 1_rad + 1_rad", "one.json"]), "2_rad");
    assert_eq!(ok(&d, &["= 12_% * 3_%", "one.json"]), "0.0036");
    assert_eq!(ok(&d, &["= 12_% div 3_%", "one.json"]), "4.0");
    // with a dimensioned quantity: the partner's unit, scaled
    assert_eq!(ok(&d, &["= 12_% * 50_EUR", "one.json"]), "6_EUR");
    assert_eq!(ok(&d, &["= 50_EUR * 12_%", "one.json"]), "6_EUR");
    assert_eq!(ok(&d, &["= 50_EUR div 50_%", "one.json"]), "100_EUR");
    assert_eq!(ok(&d, &["= 2_rad * 3_m", "one.json"]), "6_m");
    // a sum with a dimensioned quantity stays the refusal
    let (_, out, err) = qua(&d, &["= 12_% + 3_m", "one.json"]);
    assert!(out.is_empty() || out == "null", "{out}");
    assert!(err.contains("unlike dimensions (1 and m)"), "{err}");
    // ordering in the base, never equal to a bare number
    assert_eq!(ok(&d, &["= 12_% > 0.1", "one.json"]), "true");
    assert_eq!(ok(&d, &["= 12_% = 0.12", "one.json"]), "false");
    assert_eq!(ok(&d, &["= 12_% = 12_%", "one.json"]), "true");
    // the folds: percents alone keep the unit, beside numbers they
    // are the numbers they are
    assert_eq!(ok(&d, &["= @(12_%; 3) | sum", "one.json"]), "3.12");
    assert_eq!(ok(&d, &["= @(1_rad; 2) | sum", "one.json"]), "3.0");
    assert_eq!(ok(&d, &["= @(1_rad; 2_rad) | sum", "one.json"]), "3_rad");
    assert_eq!(ok(&d, &["= @(12_%; 3_%) | sum", "one.json"]), "15_%");
}

#[test]
fn a_percent_title_declares_the_column_and_mixed_columns_are_numbers() {
    let d = fixture("table");
    assert_eq!(
        ok(
            &d,
            &["//table[1] | #(--autounits) | :::header", "shares.html"]
        ),
        "@(:Port<*; text>; :Share<_%>; :2023<*; text>)"
    );
    assert_eq!(
        ok(&d, &["//table[1] | #(--autounits) | :Share", "shares.html"]),
        "@(12_%; 2.2_%; 0.5_%)"
    );
    assert_eq!(
        ok(
            &d,
            &["//table[1] | #(--autounits) | :Share | sum", "shares.html"]
        ),
        "14.7_%"
    );
    let (_, _, err) = qua(
        &d,
        &["--info", "//table[1] | #(--autounits)", "shares.html"],
    );
    assert!(err.contains("Share, % → Share in %"), "{err}");
    // the trait and the cast
    assert_eq!(
        ok(
            &d,
            &[
                "//table[1] | #(--slugs) | #(-c=:share<_%>) | :share",
                "shares.html"
            ]
        ),
        "@(12_%; 2.2_%; 0.5_%)"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "= @(%(share = 12)) | #(--header=@(:share<_%>)) | :share",
                "one.json"
            ]
        ),
        "@(12_%)"
    );
    // `clean` makes `12_%` of "12 %"; a column mixing it with counts
    // types as numbers, the percent as its ratio
    assert_eq!(
        ok(&d, &["//table[1] | # | clean | :2023", "shares.html"]),
        "@(5200000.0; 0.12; null)"
    );
    assert_eq!(
        ok(&d, &["//table[1] | # | clean | :2023 | sum", "shares.html"]),
        "5200000.12"
    );
    assert_eq!(
        ok(
            &d,
            &["= @(12_%; 3; 0.5) | #(--header=@(x)) | type", "one.json"]
        ),
        "float"
    );
    assert_eq!(
        ok(
            &d,
            &["= @(12_%; 2_%) | #(--header=@(share)) | type", "one.json"]
        ),
        "quantity"
    );
    let _ = refused;
}
