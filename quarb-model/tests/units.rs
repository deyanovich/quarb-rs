//! `unit NAME[, ALIAS...] = FACTOR EXPRESSION;` and `units PATH;`
//! — declared units joining the unit table the quantity coercion
//! consults, for every source the model mounts: every spelling a
//! declaration lists reads, a name the table holds already is
//! refused at load, and a units file holds declarations only.

use quarb::AstAdapter as _;
use quarb_json::JsonAdapter;
use quarb_model::{ModelAdapter, parse_model_file};
use std::path::PathBuf;

const DATA: &str = r#"{"грузы": [
      {"что": "мука",  "вес": "2 пуда",  "путь": "3 версты"},
      {"что": "соль",  "вес": "20 фунтов", "путь": "700 саженей"},
      {"что": "сено",  "вес": "1 пуд",   "путь": "1 верста"}]}"#;

/// Pre-metric Russian measures: Cyrillic names, the inflected forms
/// a numeral governs listed as aliases.
const MERY: &str = "\
unit аршин, аршина = 0.7112 m;
unit сажень, сажени, саженей = 3 аршин;
unit верста, версты, вёрст = 500 сажень;
unit пуд, пуда, пудов = 16.3804964 kg;
unit фунт, фунта, фунтов = 1/40 пуд;
";

fn fixture(tag: &str, model: &str, units: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-units-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mery.model"), units).unwrap();
    std::fs::write(dir.join("m.model"), model).unwrap();
    dir
}

fn shop(dir: &PathBuf) -> Result<ModelAdapter<JsonAdapter>, String> {
    shop_(dir)
}

fn refused(dir: &PathBuf) -> String {
    match shop_(dir) {
        Ok(_) => panic!("expected a refusal"),
        Err(e) => e,
    }
}

fn shop_(dir: &PathBuf) -> Result<ModelAdapter<JsonAdapter>, String> {
    let base = JsonAdapter::parse(DATA).unwrap();
    let a = ModelAdapter::new(base, parse_model_file(&dir.join("m.model"))?);
    a.check()?;
    Ok(a)
}

fn values(a: &ModelAdapter<JsonAdapter>, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns
            .into_iter()
            .map(|n| a.name(n).unwrap_or_default())
            .collect(),
    }
}

/// The magnitude of a one-quantity answer.
fn mag(a: &ModelAdapter<JsonAdapter>, q: &str) -> f64 {
    let v = values(a, q).remove(0);
    v.split_once('_').unwrap().0.parse().unwrap()
}

#[test]
fn every_listed_spelling_reads() {
    let dir = fixture("reads", "units mery.model;\n", MERY);
    let a = shop(&dir).unwrap();
    // conversion through the imported factor
    assert_eq!(values(&a, "/грузы/0::вес | convert(kg)"), ["32.7609928_kg"]);
    // (three versts of three times five hundred arshins: the float
    // product carries noise, so compare the magnitude)
    assert!((mag(&a, "/грузы/0::путь | convert(km)") - 3.2004).abs() < 1e-9);
    // twenty pounds are half a pood
    assert!((mag(&a, "/грузы/1::вес | convert(kg)") - 8.1902482).abs() < 1e-9);
    // the inflected forms are the same unit
    // (text against text compares as text; the lifted side reads
    // through the table)
    assert_eq!(
        values(&a, "/грузы/*[(::вес | quantity) > '1 пуд']::что"),
        ["мука"]
    );
    assert_eq!(
        values(&a, "/грузы/*[(::вес | quantity) >= '1 пуд']::что"),
        ["мука", "сено"]
    );
    assert!((mag(&a, "/грузы/0 | ::вес + 1пуд") - 3.0).abs() < 1e-9);
    assert!((mag(&a, "/грузы/* | ::вес | quantity @| sum") - 3.5).abs() < 1e-9);
    // the declared unit is a conversion's operand; a name the
    // library does not list is refused there, the nearest declared
    // spelling named (spec: The conversion)
    assert_eq!(values(&a, "/грузы/0::вес | convert(пуда)"), ["2_пуда"]);
    assert!((mag(&a, "/грузы/1::вес | convert(пуд)") - 0.5).abs() < 1e-9);
    let e = quarb::run("/грузы/0::вес | convert(пудъ)", &a)
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("convert(пудъ): no unit 'пудъ' (the nearest: пуд"),
        "{e}"
    );
    let e = quarb::run("/грузы/0::вес | convert(золотник)", &a)
        .unwrap_err()
        .to_string();
    assert!(e.contains("no unit 'золотник'"), "{e}");
}

#[test]
fn a_typed_column_reads_the_declared_units() {
    let dir = fixture("columns", "units mery.model;\n", MERY);
    let a = shop(&dir).unwrap();
    // the table builder's quantity coercion is the run's: cells in
    // the declared units type as quantities, not text
    assert_eq!(
        values(&a, "/грузы/* | %(::что; ::вес) @| # | type"),
        ["text, quantity"]
    );
    // (the row is a record, its field `:вес`; the table reduction
    // leaves no position for a `::` property to read)
    assert_eq!(
        values(&a, "/грузы/* | %(::вес) @| # | [1] | :вес | convert(kg)"),
        ["32.7609928_kg"]
    );
    // without the import the same column is text
    let dir = fixture("columns-plain", "", MERY);
    let a = shop(&dir).unwrap();
    assert_eq!(
        values(&a, "/грузы/* | %(::что; ::вес) @| # | type"),
        ["text, text"]
    );
}

#[test]
fn without_the_import_the_text_is_text() {
    let dir = fixture("plain", "", MERY);
    let a = shop(&dir).unwrap();
    assert_eq!(values(&a, "/грузы/0::вес | convert(kg)"), [""]);
}

#[test]
fn a_name_the_table_holds_is_refused_at_load() {
    // a built-in
    let dir = fixture("builtin", "units mery.model;\n", "unit km = 1000 m;\n");
    let err = refused(&dir);
    assert!(err.contains("unit km: a built-in unit already"), "{err}");
    // an earlier declaration
    let dir = fixture("twice", "units mery.model;\nunits again.model;\n", MERY);
    std::fs::write(dir.join("again.model"), "unit пуд = 16 kg;\n").unwrap();
    let err = refused(&dir);
    assert!(err.contains("unit пуд: declared already"), "{err}");
    // the same file twice is a parse refusal
    let dir = fixture("dup", "units mery.model;\nunits mery.model;\n", MERY);
    let err = refused(&dir);
    assert!(err.contains("imported twice"), "{err}");
    // a missing file
    let dir = fixture("missing", "units nowhere.model;\n", MERY);
    let err = refused(&dir);
    assert!(err.contains("units nowhere.model"), "{err}");
    // a units file holds declarations only
    let dir = fixture(
        "mixed",
        "units mery.model;\n",
        "unit x = 1 m;\nmount a: b;\n",
    );
    let err = refused(&dir);
    assert!(err.contains("unit declarations only"), "{err}");
    // a factor that is not positive, an expression that is not unit text
    let dir = fixture("factor", "unit x = -1 m;\n", "");
    let err = refused(&dir);
    assert!(err.contains("not a positive decimal or ratio"), "{err}");
    let dir = fixture("expr", "unit x = 1 m^;\n", "");
    let err = refused(&dir);
    assert!(err.contains("not unit text"), "{err}");
}

#[test]
fn declarations_stand_in_the_model_and_name_opaque_units() {
    let dir = fixture(
        "inline",
        "unit пуд, пуда, пудов = 16.3804964 kg;\nunit €, евро = 1 EUR;\n",
        "",
    );
    let a = shop(&dir).unwrap();
    assert_eq!(values(&a, "/грузы/0::вес | convert(kg)"), ["32.7609928_kg"]);
    // a currency symbol and its word are the currency
    assert_eq!(values(&a, "= '12 €' | dimension"), ["EUR"]);
    assert_eq!(values(&a, "= (('12 €' | quantity) = 12_EUR)"), ["true"]);
    assert_eq!(values(&a, "= ('3 евро' + 2_EUR) | convert(EUR)"), ["5_EUR"]);
    // the alias canonicalizes before an exchange
    assert_eq!(
        values(&a, "= '12 €' | exchange(%(EUR = 1.25_USD))"),
        ["15_USD"]
    );
}

#[test]
fn a_declaration_without_a_factor_names_a_base_unit_and_its_prefix_aliases() {
    // a retired currency the table lacks, declared as a base of its own
    let dir = fixture("base", "unit HRK, kuna;\nunit SVC, ₡-;\n", "");
    let a = shop(&dir).unwrap();
    // a declared unit reads glued, spaced or underscored
    assert_eq!(values(&a, "= '12 HRK' | quantity"), ["12_HRK"]);
    assert_eq!(values(&a, "= '12HRK' | quantity"), ["12_HRK"]);
    assert_eq!(values(&a, "= '12_HRK' | quantity"), ["12_HRK"]);
    assert_eq!(values(&a, "= '12 kuna' | quantity"), ["12_kuna"]);
    // a prefix alias stands before the magnitude, glued or spaced,
    // and canonicalizes to the primary name
    assert_eq!(values(&a, "= '₡12' | quantity"), ["12_SVC"]);
    assert_eq!(values(&a, "= '₡ 12.50' | quantity"), ["12.5_SVC"]);
    assert_eq!(values(&a, "= ('₡12' | quantity) + 3_SVC"), ["15_SVC"]);
    assert_eq!(values(&a, "= '₡12' | dimension"), ["SVC"]);
    // an undeclared name is still text spaced
    assert_eq!(values(&a, "= '12 XAU' | quantity"), [""]);
}
