//! Units and quantities after the code review of 2026-11-10: the
//! value rows B1, B2, B9, B13, T2 and T3, the stdlib rows B6 and
//! B16, and the rulings of the same day on `convert` across
//! dimensions, `mod` and `idiv` of a quantity, the year's prefixes,
//! two NaNs under `unique`, and closeness at distance zero.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-units-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mt.csv"), "c,Emissions (Mt per year)\nA,5\n").unwrap();
    std::fs::write(dir.join("ms.csv"), "id,seen_at\n1,1700000000123\n").unwrap();
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

/// A count of milliseconds since the epoch splits in integers: the
/// division in a double left the nanoseconds as noise.
#[test]
fn a_millisecond_epoch_keeps_its_milliseconds() {
    let d = fixture("epoch");
    assert_eq!(
        ok(&d, &["= 1700000000123 | datetime"]),
        "2023-11-14T22:13:20.123Z"
    );
    assert_eq!(
        ok(&d, &["= 1700000000123 = 2023-11-14T22:13:20.123Z"]),
        "true"
    );
    // seconds, and a float of seconds, as before
    assert_eq!(ok(&d, &["= 1700000000 | datetime"]), "2023-11-14T22:13:20Z");
    assert_eq!(
        ok(&d, &["= 1700000000.5 | datetime"]),
        "2023-11-14T22:13:20.5Z"
    );
    // the `_at` suffix reads the same rule
    assert_eq!(
        ok(&d, &["/row | ::seen_at", "ms.csv?autounits"]),
        "2023-11-14T22:13:20.123Z"
    );
}

/// A symbol among a title's words keeps its case, which is the
/// unit: `Mt` the megatonne, `kB` the kilobyte, `A` the ampere.
#[test]
fn a_symbol_among_words_keeps_its_case() {
    let d = fixture("words");
    assert_eq!(
        ok(&d, &["= '5 Mt per year' | convert(kg/a)"]),
        "5000000000_kg/a"
    );
    assert_eq!(ok(&d, &["= '3 kB per second' | convert(B/s)"]), "3000_B/s");
    assert_eq!(ok(&d, &["= '5 A per m' | unit"]), "A/m");
    // the dictionary still reads its words whatever their case
    assert_eq!(ok(&d, &["= '5 Metric Tonnes per Year' | unit"]), "t/a");
    // a scraped title
    assert_eq!(
        ok(&d, &["/row | ::Emissions", "mt.csv?autounits"]),
        "5_Mt/a"
    );
}

/// A ratio and a bare number order in the base under `sort` as
/// under `<`, and a ratio is no more equal to numeric text than to
/// the number.
#[test]
fn a_ratio_sorts_with_numbers_and_equals_none() {
    let d = fixture("ratio");
    assert_eq!(ok(&d, &["= 5_% < 0.5"]), "true");
    assert_eq!(
        ok(&d, &["= @(0.5; 5_%; 0.01; 50_%) | sort"]),
        "@(0.01; 5_%; 0.5; 50_%)"
    );
    assert_eq!(ok(&d, &["= 50_% = 0.5"]), "false");
    assert_eq!(ok(&d, &["= 50_% = '0.5'"]), "false");
    assert_eq!(ok(&d, &["= 50_% = '50 %'"]), "true");
}

/// What has no meaning across dimensions is refused, never null
/// (spec: Physical Quantities): the fold as the operator, and
/// `convert` (ruling 2026-11-10).
#[test]
fn unlike_dimensions_are_refused() {
    let d = fixture("unlike");
    for q in [
        "= @(5_km; 3_s) | sum",
        "= @(5_km; 3_s) | mean",
        "= @(5_km; 3_s) | median",
    ] {
        let e = refused(&d, &[q]);
        assert!(e.contains("unlike dimensions (m and s)"), "{q}: {e}");
    }
    let e = refused(&d, &["= 5_km | convert(s)"]);
    assert!(
        e.contains("convert(s)") && e.contains("unlike (m and s)"),
        "{e}"
    );
    // one dimension under several units still folds, and converts
    assert_eq!(ok(&d, &["= @(1_km; 500_m) | sum"]), "1.5_km");
    assert_eq!(ok(&d, &["= 5_km | convert(m)"]), "5000_m");
    // a bare number beside a dimensioned quantity is refused, as
    // `5_km + 5` is (ruling 2026-11-10; both answered null)
    let e = refused(&d, &["= @(5_km; 5) | sum"]);
    assert!(
        e.contains("a fold over a quantity and a number without a unit (m and a bare number)"),
        "{e}"
    );
}

/// A number without a unit beside a dimensioned quantity is refused,
/// under the operators and in a fold alike, as two unlike dimensions
/// are (ruling 2026-11-10: "5_km + 5 should be refused just like
/// incompatible dimensions"). They answered null. A side with no
/// reading keeps the null that propagates, and a quantity of
/// dimension one still computes with a bare number.
#[test]
fn a_bare_number_beside_a_quantity_is_refused() {
    let d = fixture("bare");
    for q in ["= 5_km + 5", "= 5 + 5_km", r#"= 5_km + "5""#] {
        let e = refused(&d, &[q]);
        assert!(
            e.contains("the sum of a quantity and a number without a unit (m and a bare number)"),
            "{q}: {e}"
        );
    }
    assert!(refused(&d, &["= 5_km - 5"]).contains("the difference of a quantity and a number"));
    assert!(refused(&d, &["= @(5_km; 5) | mean"]).contains("a fold over a quantity and a number"));
    for q in ["= 5_km + null", r#"= 5_km + "abc""#] {
        assert_eq!(ok(&d, &[q]), "null", "{q}");
    }
    assert_eq!(ok(&d, &["= @(5_km; null; 3_km) | sum"]), "8_km");
    assert_eq!(ok(&d, &["= 12_% + 5"]), "5.12");
    assert_eq!(ok(&d, &["= 5_km * 2"]), "10_km");
    // Time is a dimension like the others (the user, the same day:
    // "fold refuse too"): a fold of time read a bare number as
    // seconds, `@(5_s; 5) | sum` was 10_s, where the operator
    // answered null. Both refuse.
    assert!(refused(&d, &["= 5_s + 5"]).contains("(s and a bare number)"));
    for reducer in ["sum", "mean", "median", "std", "min", "max"] {
        let e = refused(&d, &[&format!("= @(5_s; 5) | {reducer}")]);
        assert!(
            e.contains("a fold over a quantity and a number without a unit (s and a bare number)"),
            "{reducer}: {e}"
        );
    }
    // span text and a null beside spans are as before
    assert_eq!(ok(&d, &[r#"= @(5_s; "PT5S"; null) | sum"#]), "10_s");
    assert_eq!(ok(&d, &["= @(2_h; 30_min) | sum"]), "2.5_h");
    // `min` and `max` refuse in the folds' words, where they answered
    // null: a number without a unit, and two dimensions
    assert!(
        refused(&d, &["= @(5_km; 5) | min"])
            .contains("a fold over a quantity and a number without a unit (m and a bare number)")
    );
    assert!(
        refused(&d, &["= @(5_km; 3_s) | max"])
            .contains("a fold over quantities of unlike dimensions (m and s)")
    );
    assert_eq!(ok(&d, &["= @(5_km; null; 3_km) | max"]), "5_km");
}

/// `mod` and `idiv` of a quantity by a bare number answer in the
/// unit the quantity is written in (ruling 2026-11-10); a ratio is
/// the number it is. They were a silent null.
#[test]
fn a_quantity_takes_mod_and_idiv_in_its_written_unit() {
    let d = fixture("mod");
    assert_eq!(ok(&d, &["= 10_km mod 3"]), "1_km");
    assert_eq!(ok(&d, &["= 10_km idiv 3"]), "3_km");
    assert_eq!(ok(&d, &["= 10.5_km mod 3"]), "1.5_km");
    assert_eq!(ok(&d, &["= 10_km mod 3 | convert(m)"]), "1000_m");
    assert_eq!(ok(&d, &["= 12_% mod 5"]), "0.12");
    assert_eq!(ok(&d, &["= 12_% idiv 5"]), "0");
    assert_eq!(ok(&d, &["= 10_km mod 0"]), "null");
    let e = refused(&d, &["= 10 mod 3_km"]);
    assert!(e.contains("a number by a quantity is not defined"), "{e}");
    // A quantity by a quantity of its dimension (ruling 2026-11-10;
    // it was refused): how many times the one goes into the other,
    // a number as `div` answers, and what is left in the unit the
    // first is written in, rounded as the numbers' own are.
    assert_eq!(ok(&d, &["= 10_km mod 3_km"]), "1_km");
    assert_eq!(ok(&d, &["= 10_km idiv 3_km"]), "3");
    assert_eq!(ok(&d, &["= 10_km mod 300_m"]), "0.1_km");
    assert_eq!(ok(&d, &["= 7.5_h mod 2_h"]), "1.5_h");
    assert_eq!(ok(&d, &["= -7_km mod 3_km"]), "-1_km");
    assert_eq!(ok(&d, &["= -7_km idiv 3_km"]), "-2");
    assert_eq!(ok(&d, &["= 10_km idiv 0_km"]), "null");
    for q in ["= 10_km mod 3_s", "= 10_km idiv 3_s"] {
        let e = refused(&d, &[q]);
        assert!(
            e.contains("of quantities of unlike dimensions (m and s)"),
            "{q}: {e}"
        );
    }
}

/// The year takes the thousand, the million and the billion; no
/// other unit of the engine's own takes a prefix, and a prefix on
/// one is refused where it minted an unknown unit in silence
/// (ruling 2026-11-10).
#[test]
fn the_year_takes_three_prefixes() {
    let d = fixture("year");
    assert_eq!(ok(&d, &["= 5_ka | convert(a)"]), "5000_a");
    assert_eq!(ok(&d, &["= 2_Myr | convert(kyr)"]), "2000_kyr");
    assert_eq!(ok(&d, &["= 1_Ga | convert(Ma)"]), "1000_Ma");
    assert_eq!(ok(&d, &["= 5_ka + 1_a | convert(a)"]), "5001_a");
    let e = refused(&d, &["= 5_ma"]);
    assert!(e.contains("the year takes the prefixes k, M and G"), "{e}");
    for (q, stem) in [("= 5_kton", "ton"), ("= 5_k%", "%")] {
        let e = refused(&d, &[q]);
        assert!(e.contains(&format!("'{stem}' takes no prefix")), "{q}: {e}");
    }
}

/// Two NaNs are one value to `unique` and one group, whatever their
/// sign bit; `=` between them stays false (ruling 2026-11-10).
#[test]
fn two_nans_dedup_and_do_not_equal() {
    let d = fixture("nan");
    assert_eq!(ok(&d, &["= @(0 div 0; nan) | unique | count"]), "1");
    assert_eq!(ok(&d, &["= @(0 div 0; nan; 1) | unique | count"]), "2");
    assert_eq!(ok(&d, &["= nan = nan"]), "false");
}
