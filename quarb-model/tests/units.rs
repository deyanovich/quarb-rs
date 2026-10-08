//! `units LIBRARY;` — a kaiv `.faiv` unit library imported into the
//! unit table the unital reading consults, for every source the
//! model mounts: every spelling it lists reads, a name the table
//! holds already is refused at load.

use quarb::AstAdapter as _;
use quarb_json::JsonAdapter;
use quarb_model::{ModelAdapter, parse_model_file};
use std::path::PathBuf;

const DATA: &str = r#"{"грузы": [
      {"что": "мука",  "вес": "2 пуда",  "путь": "3 версты"},
      {"что": "соль",  "вес": "20 фунтов", "путь": "700 саженей"},
      {"что": "сено",  "вес": "1 пуд",   "путь": "1 верста"}]}"#;

/// Pre-metric Russian measures, as kaiv writes them (the
/// conformance fixture's shape): Cyrillic names, the inflected
/// forms a numeral governs listed as aliases.
const FAIV: &str = "\
.!faiv mery/units

m 0.7112
&аршин=
&аршина=аршин

аршин 3
&сажень=
&сажени=сажень
&саженей=сажень

сажень 500
&верста=
&версты=верста
&вёрст=верста

kg 16.3804964
&пуд=
&пуда=пуд
&пудов=пуд

пуд 0.025
&фунт=
&фунта=фунт
&фунтов=фунт
";

fn fixture(tag: &str, model: &str, faiv: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-units-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mery.faiv"), faiv).unwrap();
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
    v.split_once(' ').unwrap().0.parse().unwrap()
}

#[test]
fn every_listed_spelling_reads() {
    let dir = fixture("reads", "units mery.faiv;\n", FAIV);
    let a = shop(&dir).unwrap();
    // conversion through the imported factor
    assert_eq!(values(&a, "/грузы/0::вес | convert(kg)"), ["32.7609928 kg"]);
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
    assert!((mag(&a, "/грузы/0 | ::вес + 1пуд") - 3.0 * 16.3804964).abs() < 1e-9);
    assert!((mag(&a, "/грузы/* | ::вес | quantity @| sum") - 3.5 * 16.3804964).abs() < 1e-9);
    // a unit the library does not list has no reading
    assert_eq!(values(&a, "/грузы/0::вес | convert(золотник)"), [""]);
}

#[test]
fn without_the_import_the_text_is_text() {
    let dir = fixture("plain", "", FAIV);
    let a = shop(&dir).unwrap();
    assert_eq!(values(&a, "/грузы/0::вес | convert(kg)"), [""]);
}

#[test]
fn a_name_the_table_holds_is_refused_at_load() {
    // a built-in
    let dir = fixture(
        "builtin",
        "units mery.faiv;\n",
        ".!faiv x/units\n\nm 1000\n&km=\n",
    );
    let err = refused(&dir);
    assert!(err.contains("'km' is a built-in unit already"), "{err}");
    // an earlier library
    let dir = fixture("twice", "units mery.faiv;\nunits again.faiv;\n", FAIV);
    std::fs::write(dir.join("again.faiv"), ".!faiv y/units\n\nkg 16\n&пуд=\n").unwrap();
    let err = refused(&dir);
    assert!(err.contains("'пуд' is already defined"), "{err}");
    // the same file twice is a parse refusal
    let dir = fixture("dup", "units mery.faiv;\nunits mery.faiv;\n", FAIV);
    let err = refused(&dir);
    assert!(err.contains("imported twice"), "{err}");
    // a missing file
    let dir = fixture("missing", "units nowhere.faiv;\n", FAIV);
    let err = refused(&dir);
    assert!(err.contains("units nowhere.faiv"), "{err}");
}
