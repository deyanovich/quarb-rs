//! The `property` statement of a model file (spec: Model Files, the
//! property statement; ruling 2026-11-08, the 0.57 round): `property
//! SCOPE::name<TRAIT> = EXPR;` declares a property on every node of
//! the scope, its value the expression at the node, typed by the
//! trait — a unit as the typed weight clause types, a type word as
//! the declared column casts; the bare form types a property in place.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-property-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("legs.json"),
        r#"{"legs": [{"id": 1, "distance_kilometers": 1235, "distance_mi": 767, "time_hours": 2.5, "fare": "155 EUR", "n": "7"},
          {"id": 2, "distance_kilometers": 820, "distance_mi": 510, "time_hours": 1.5, "fare": "99 EUR", "n": "x"},
          {"id": 3, "distance_kilometers": null, "distance_mi": 1180, "time_hours": 3.0, "fare": null, "n": "9"}]}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("legs.csv"),
        "id,distance_km,time_min,carrier\n1,1235,130,JP\n2,820,95,LH\n3,1900,170,TK\n",
    )
    .unwrap();
    let db = rusqlite::Connection::open(dir.join("flights.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE flights (id INTEGER PRIMARY KEY, carrier TEXT, distance_km INTEGER);
         INSERT INTO flights VALUES (1,'JP',1235),(2,'LH',820),(3,'TK',1900);",
    )
    .unwrap();
    dir
}

fn model(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(name), text).unwrap();
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

const LEGS: &str = "property /legs/*::distance<_km> = ::distance_kilometers;\n\
                    property /legs/*::distance_mi<_mi>;\n\
                    property /legs/*::time_hours<_h>;\n\
                    property /legs/*::speed<_mph> = ::distance_mi div ::time_hours;\n\
                    property /legs/*::ratio = ::distance_mi div ::distance;\n";

#[test]
fn a_declared_property_reads_its_expression_at_the_node() {
    let d = fixture("read");
    model(&d, "legs.model", LEGS);
    let m = ["--model", "legs.model"];
    // a remap to a new name: the source stays, the new name is typed
    assert_eq!(
        ok(&d, &[m[0], m[1], "/legs/*[1] | ::distance", "legs.json"]),
        "1235_km"
    );
    assert_eq!(
        ok(
            &d,
            &[
                m[0],
                m[1],
                "/legs/*[1] | ::distance_kilometers",
                "legs.json"
            ]
        ),
        "1235"
    );
    // typed in place: the bare form
    assert_eq!(
        ok(&d, &[m[0], m[1], "/legs/*[1] | ::time_hours", "legs.json"]),
        "2.5_h"
    );
    // a derived dimension checks against the trait and is written in it
    assert_eq!(
        ok(&d, &[m[0], m[1], "/legs/*[1] | ::speed", "legs.json"]),
        "306.8_mph"
    );
    // untyped: the value as computed
    assert_eq!(
        ok(&d, &[m[0], m[1], "/legs/*[1] | ::ratio", "legs.json"]),
        "0.9994873263157895"
    );
    // a null source yields a null property
    assert_eq!(
        ok(&d, &[m[0], m[1], "/legs/*[3] | ::distance", "legs.json"]),
        "null"
    );
    // the names: the source's, then the declared ones; the record
    // carries them all
    assert_eq!(
        ok(&d, &[m[0], m[1], "/legs/*[1] | :props", "legs.json"]),
        "@(::id; ::distance_kilometers; ::distance_mi; ::time_hours; ::fare; ::n; ::distance; \
         ::speed; ::ratio)"
    );
    assert_eq!(
        ok(&d, &[m[0], m[1], "/legs/*[1] | %", "legs.json"]),
        "%(id = 1; distance_kilometers = 1235; distance_mi = 767_mi; time_hours = 2.5_h; \
         fare = \"155 EUR\"; n = \"7\"; distance = 1235_km; speed = 306.8_mph; \
         ratio = 0.9994873263157895)"
    );
    // a predicate on a declared property: the engine computes it
    assert_eq!(
        ok(
            &d,
            &[m[0], m[1], "/legs/*[::speed > 350_mph] | ::id", "legs.json"]
        ),
        "3"
    );
    // a table over the scope carries the declared properties as columns
    let header = ok(&d, &[m[0], m[1], "/legs/* @| # | :::header", "legs.json"]);
    assert!(header.contains(":speed<*; quantity>"), "{header}");
    assert!(header.contains(":distance_mi<*; quantity>"), "{header}");
    assert!(header.contains(":ratio<*; float>"), "{header}");
    assert_eq!(
        ok(&d, &[m[0], m[1], "/legs/* @| # | :speed", "legs.json"]),
        "@(306.8_mph; 340_mph; 393.333333333333_mph)"
    );
}

#[test]
fn a_type_trait_casts_and_refuses_at_read() {
    let d = fixture("cast");
    model(&d, "count.model", "property /legs/*::count<int> = ::n;\n");
    let m = ["--model", "count.model"];
    assert_eq!(
        ok(&d, &[m[0], m[1], "/legs/*[1] | ::count", "legs.json"]),
        "7"
    );
    // the load check passes on the first node; a later bad value
    // refuses at read, as a declared column does
    let err = refused(&d, &[m[0], m[1], "/legs/*[2] | ::count", "legs.json"]);
    assert!(
        err.contains(
            "property /legs/*::count<int>: the expression yields \"x\" at /legs/2, which does \
             not cast to int"
        ),
        "{err}"
    );
    let err = refused(&d, &[m[0], m[1], "/legs/* @| # | :count", "legs.json"]);
    assert!(err.contains("which does not cast to int"), "{err}");
}

#[test]
fn the_load_check_refuses_by_name() {
    let d = fixture("load");
    // another dimension
    model(&d, "dim.model", "property /legs/*::speed<_mph> = ::fare;\n");
    let err = refused(
        &d,
        &["--model", "dim.model", "/legs/*[1] | ::speed", "legs.json"],
    );
    assert!(
        err.contains(
            "property /legs/*::speed<_mph>: the expression yields 155 EUR at /legs/1 — a \
             quantity of another dimension"
        ),
        "{err}"
    );
    // a non-number under a unit trait: minted when numeric text, refused otherwise
    model(&d, "num.model", "property /legs/*::x<_km> = ::n;\n");
    assert_eq!(
        ok(
            &d,
            &["--model", "num.model", "/legs/*[1] | ::x", "legs.json"]
        ),
        "7_km"
    );
    let err = refused(
        &d,
        &["--model", "num.model", "/legs/*[2] | ::x", "legs.json"],
    );
    assert!(
        err.contains(
            "property /legs/*::x<_km>: the expression yields \"x\" at /legs/2, which is no number"
        ),
        "{err}"
    );
    // an unknown unit
    model(
        &d,
        "unit.model",
        "property /legs/*::d<_furlong> = ::distance_mi;\n",
    );
    let err = refused(
        &d,
        &["--model", "unit.model", "/legs/*[1] | ::d", "legs.json"],
    );
    assert!(
        err.contains(
            "property /legs/*::d<_furlong>: 'furlong' is no unit; a unit statement declares one \
             ('unit furlong = …;')"
        ),
        "{err}"
    );
    // a cycle
    model(
        &d,
        "cycle.model",
        "property /legs/*::a = ::b;\nproperty /legs/*::b = ::a;\n",
    );
    let err = refused(
        &d,
        &["--model", "cycle.model", "/legs/*[1] | ::a", "legs.json"],
    );
    assert!(
        err.contains(
            "property /legs/*::b: the declared properties read each other in a cycle at /legs/1 \
             (::b → ::a → ::b)"
        ),
        "{err}"
    );
    // a scope with a descendant axis
    model(&d, "deep.model", "property //legs/*::a = 1;\n");
    let err = refused(
        &d,
        &["--model", "deep.model", "/legs/*[1] | ::a", "legs.json"],
    );
    assert!(
        err.contains(
            "a property's scope is a path of child steps ('/legs/*', '/flights/*[::carrier = \
             \"JP\"]'); a descendant axis walks, and membership is read at the node: '//legs/*'"
        ),
        "{err}"
    );
    // the statement's own shape
    model(&d, "shape.model", "property /legs/*::a<km> = 1;\n");
    let err = refused(
        &d,
        &["--model", "shape.model", "/legs/*[1] | ::a", "legs.json"],
    );
    assert!(err.contains("names no type between '<' and '>'"), "{err}");
    model(
        &d,
        "twice.model",
        "property /legs/*::a = 1;\nproperty /legs/*::a = 2;\n",
    );
    let err = refused(
        &d,
        &["--model", "twice.model", "/legs/*[1] | ::a", "legs.json"],
    );
    assert!(
        err.contains("property ::a is declared twice on /legs/*"),
        "{err}"
    );
    model(&d, "empty.model", "property /legs/*::a = ;\n");
    let err = refused(
        &d,
        &["--model", "empty.model", "/legs/*[1] | ::a", "legs.json"],
    );
    assert!(err.contains("nothing stands right of '='"), "{err}");
}

#[test]
fn a_table_source_keeps_its_fast_path_and_a_predicate_scope_holds() {
    let d = fixture("csv");
    model(
        &d,
        "legs.model",
        "property /row::distance_km<_km>;\n\
         property /row::minutes<_min> = ::time_min;\n\
         property /row::speed<_km/h> = ::distance_km div ::minutes;\n\
         property /row::long<bool> = ::distance_km > 1000_km;\n",
    );
    let m = ["--model", "legs.model"];
    assert_eq!(
        ok(&d, &[m[0], m[1], "/row @| # | :::header", "legs.csv"]),
        "@(:id<*; int>; :distance_km<*; quantity>; :time_min<*; int>; :carrier<*; text>; \
         :minutes<*; duration>; :speed<*; quantity>; :long<*; bool>)"
    );
    assert_eq!(
        ok(&d, &[m[0], m[1], "/row @| # | :speed", "legs.csv"]),
        "@(570_km/h; 517.894736842105_km/h; 670.588235294118_km/h)"
    );
    assert_eq!(
        ok(&d, &[m[0], m[1], "/row[::long] | ::id", "legs.csv"]),
        "1\n3"
    );
    // untyped text: the record path
    model(
        &d,
        "tag.model",
        "property /row::tag = ::carrier ++ \"-\" ++ ::id;\n",
    );
    assert_eq!(
        ok(
            &d,
            &["--model", "tag.model", "/row @| # | :tag", "legs.csv"]
        ),
        r#"@("JP-1"; "LH-2"; "TK-3")"#
    );
    // a scope with a predicate holds the matching rows alone
    model(
        &d,
        "lh.model",
        "property /row[::carrier = \"LH\"]::cheap<bool> = true;\n",
    );
    assert_eq!(
        ok(&d, &["--model", "lh.model", "/row | ::cheap", "legs.csv"]),
        "null\ntrue\nnull"
    );
    assert_eq!(
        ok(
            &d,
            &["--model", "lh.model", "/row[::cheap] | ::id", "legs.csv"]
        ),
        "2"
    );
    // a database: the engine computes what names a declared property
    model(
        &d,
        "far.model",
        "property /flights/*::far<bool> = ::distance_km > 1000;\n",
    );
    assert_eq!(
        ok(
            &d,
            &[
                "--model",
                "far.model",
                "/flights/*[::far] @| count",
                "flights.db"
            ]
        ),
        "2"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "--model",
                "far.model",
                "/flights/*[::carrier = \"LH\"] | ::far",
                "flights.db"
            ]
        ),
        "false"
    );
}
