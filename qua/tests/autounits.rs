//! The unit suffix convention on demand (spec: Mounts, the autounits
//! option; ruling 2026-11-08, the 0.57 round): `?autounits` reads
//! every property whose last `_` segment names a unit the run knows
//! as the property without the suffix, holding quantities in that
//! unit; `?autounit=_km,_g` the named suffixes only; `_at` and `_on`
//! type a column as instants and dates in place. Never on by default.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-autounits-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("penguins.csv"),
        "species,island,bill_length_mm,body_mass_g,created_at,published_on,speed_mb,checked_in,user_id,note\n\
         Adelie,Torgersen,39.1,3750,2024-01-05T10:20:30Z,2024-01-05,12,3,7,first\n\
         Adelie,Torgersen,39.5,3800,1700000000,2024-01-06,13,4,8,second\n\
         Gentoo,Biscoe,46.1,,2024-02-01T00:00:00Z,,14,5,9,n/a\n",
    )
    .unwrap();
    std::fs::write(dir.join("lengths.csv"), "name,length,length_km\na,1,2\n").unwrap();
    std::fs::write(
        dir.join("quays.csv"),
        "Port,Draught (m),Share %,Checked in,Quay m,Mass [kg]\nPiraeus,18.5,12,3,1200,5000\nKoper,14,2.2,4,650,2100\n",
    )
    .unwrap();
    std::fs::write(dir.join("times.csv"), "name,t_min,t_h\na,60,1\n").unwrap();
    std::fs::write(
        dir.join("legs.json"),
        r#"{"legs": [{"id": 1, "distance_km": 1235, "time_min": 130, "created_at": "2024-01-05T10:00:00Z"}, {"id": 2, "distance_km": 820, "time_min": null, "created_at": 1700000000}]}"#,
    )
    .unwrap();
    let db = rusqlite::Connection::open(dir.join("flights.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE flights (id INTEGER PRIMARY KEY, carrier TEXT, distance_km INTEGER,
                               time_min INTEGER, created_at INTEGER, fare_eur REAL);
         INSERT INTO flights VALUES (1,'JP',1235,130,1700000000,155.5),
                                    (2,'LH',820,95,1700003600,99.0),
                                    (3,'TK',1900,170,1700007200,210.25);",
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
fn the_flag_converts_every_known_suffix_and_tells() {
    let d = fixture("flag");
    let t = "penguins.csv?autounits";
    assert_eq!(ok(&d, &["/row[1] | ::body_mass", t]), "3750_g");
    assert_eq!(ok(&d, &["/row[1] | ::bill_length", t]), "39.1_mm");
    // the suffixed name answers no more
    assert_eq!(ok(&d, &["/row[1] | ::body_mass_g", t]), "null");
    assert_eq!(
        ok(&d, &["/row[1] | :props", t]),
        "@(::species; ::island; ::bill_length; ::body_mass; ::created_at; ::published_on; \
         ::speed_mb; ::checked_in; ::user_id; ::note)"
    );
    // a null cell stays null; a predicate reads the quantity
    assert_eq!(ok(&d, &["/row[3] | ::body_mass", t]), "null");
    assert_eq!(
        ok(&d, &["/row[::body_mass > 3760_g] | ::species", t]),
        "Adelie"
    );
    // the info line, once per header: the conversions, then what
    // was skipped and why
    let (_, _, err) = qua(&d, &["--info", "/row[1] | ::body_mass", t]);
    assert!(
        err.contains(
            "autounits on penguins.csv: bill_length_mm → bill_length in mm, body_mass_g → \
             body_mass in g, created_at: instants, published_on: dates; skipped: speed_mb \
             ('mb' is no unit), checked_in ('in' is a word here, not a unit), user_id ('id' is \
             no unit)"
        ),
        "{err}"
    );
    // without the option nothing converts
    assert_eq!(ok(&d, &["/row[1] | ::body_mass_g", "penguins.csv"]), "3750");
    assert_eq!(ok(&d, &["/row[1] | ::body_mass", "penguins.csv"]), "null");
}

#[test]
fn temporal_suffixes_type_the_column_in_place() {
    let d = fixture("temporal");
    let t = "penguins.csv?autounits";
    // the name stays; ISO text, an epoch number, a date
    assert_eq!(
        ok(&d, &["/row[1] | ::created_at", t]),
        "2024-01-05T10:20:30Z"
    );
    assert_eq!(
        ok(&d, &["/row[2] | ::created_at", t]),
        "2023-11-14T22:13:20"
    );
    assert_eq!(ok(&d, &["/row[1] | ::published_on", t]), "2024-01-05");
    assert_eq!(ok(&d, &["/row[3] | ::published_on", t]), "null");
    assert_eq!(
        ok(&d, &["/row[::created_at > 2024-01-10] | ::species", t]),
        "Gentoo"
    );
    // the epoch rule is one: seconds, milliseconds from 10^11 up
    assert_eq!(ok(&d, &["= 1700000000 | datetime"]), "2023-11-14T22:13:20");
    assert_eq!(
        ok(&d, &["= 1700000000000 | datetime"]),
        "2023-11-14T22:13:20"
    );
    assert_eq!(
        ok(&d, &["= %(when<instant> = 1700000000)"]),
        "%(when = 2023-11-14T22:13:20)"
    );
    assert_eq!(
        ok(
            &d,
            &["= @(%(when = 1700000000); %(when = 1700003600)) | #(-c=:when<instant>) | :when"]
        ),
        "@(2023-11-14T22:13:20; 2023-11-14T23:13:20)"
    );
    assert_eq!(
        ok(
            &d,
            &["= @(%(when = 1700000000)) | # | map(<instant>; --to=:when) | :when"]
        ),
        "@(2023-11-14T22:13:20)"
    );
    // inference never reads a number as an instant
    assert_eq!(
        ok(&d, &["= @(%(when = 1700000000)) | # | :::header"]),
        "@(:when<*; int>)"
    );
}

#[test]
fn the_table_fast_path_types_the_converted_columns() {
    let d = fixture("table");
    let t = "penguins.csv?autounits";
    assert_eq!(
        ok(&d, &["/row @| # | type", t]),
        r#"@("text"; "text"; "quantity"; "quantity"; "instant"; "instant"; "int"; "int"; "int"; "text")"#
    );
    let header = ok(&d, &["/row @| # | :::header", t]);
    assert!(header.contains(":body_mass<*; quantity>"), "{header}");
    assert!(header.contains(":created_at<*; instant>"), "{header}");
    assert!(!header.contains("body_mass_g"), "{header}");
    assert_eq!(
        ok(&d, &["/row @| # | :bill_length | mean", t]),
        "41.5666666666667_mm"
    );
    assert_eq!(
        ok(&d, &["/row @| # | :body_mass", t]),
        "@(3750_g; 3800_g; null)"
    );
    assert_eq!(
        ok(&d, &["/row @| # | :created_at", t]),
        "@(2024-01-05T10:20:30Z; 2023-11-14T22:13:20; 2024-02-01T00:00:00Z)"
    );
}

#[test]
fn the_named_form_converts_the_named_suffixes_only() {
    let d = fixture("named");
    let t = "penguins.csv?autounit=_mm";
    assert_eq!(ok(&d, &["/row[1] | ::bill_length", t]), "39.1_mm");
    assert_eq!(ok(&d, &["/row[1] | ::body_mass_g", t]), "3750");
    let (_, _, err) = qua(&d, &["--info", "/row[1] | ::bill_length", t]);
    assert!(
        err.contains("autounit=_mm on penguins.csv: bill_length_mm → bill_length in mm"),
        "{err}"
    );
    // a comma list; a repeated option unions
    assert_eq!(
        ok(
            &d,
            &["/row[1] | ::body_mass", "penguins.csv?autounit=_mm,_g"]
        ),
        "3750_g"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "/row[1] | ::body_mass",
                "penguins.csv?autounit=_mm&autounit=_g"
            ]
        ),
        "3750_g"
    );
    // the named form converts what it names, stop list or not: the
    // inch
    assert_eq!(
        ok(&d, &["/row[1] | ::checked", "penguins.csv?autounit=_in"]),
        "3_in"
    );
    // a named suffix no unit knows refuses when the rows are read
    let err = refused(&d, &["/row[1] | ::speed", "penguins.csv?autounit=_mb"]);
    assert!(
        err.contains("autounit=_mb on penguins.csv: 'mb' is no unit this run knows"),
        "{err}"
    );
}

#[test]
fn malformed_options_refuse_with_the_spelling() {
    let d = fixture("options");
    let err = refused(&d, &["/row[1]", "penguins.csv?autounits=yes"]);
    assert!(err.contains("autounits takes no value"), "{err}");
    assert!(err.contains("'autounit=_km,_g'"), "{err}");
    let err = refused(&d, &["/row[1]", "penguins.csv?autounit=km"]);
    assert!(
        err.contains("the suffix of 'length_km' is '_km' ('autounit=_km')"),
        "{err}"
    );
    let err = refused(&d, &["/row[1]", "penguins.csv?autounit"]);
    assert!(err.contains("autounit names its suffixes"), "{err}");
    let err = refused(&d, &["/row[1]", "penguins.csv?autounit=_1"]);
    assert!(err.contains("shaped as no unit"), "{err}");
}

#[test]
fn collisions_refuse_naming_the_columns() {
    let d = fixture("collide");
    let err = refused(&d, &["/row[1] | ::length", "lengths.csv?autounits"]);
    assert!(
        err.contains(
            "autounits on lengths.csv: 'length_km' would become 'length', a property the rows \
             already carry"
        ),
        "{err}"
    );
    let err = refused(&d, &["/row[1] | ::t", "times.csv?autounits"]);
    assert!(
        err.contains("autounits on times.csv: 't_min' and 't_h' would both become 't'"),
        "{err}"
    );
}

#[test]
fn a_database_and_a_document_read_alike() {
    let d = fixture("backends");
    let db = "flights.db?autounits";
    assert_eq!(ok(&d, &["/flights/*[1] | ::distance", db]), "1235_km");
    assert_eq!(
        ok(&d, &["/flights/*[::carrier = \"LH\"] | ::time", db]),
        "95_min"
    );
    assert_eq!(
        ok(&d, &["/flights/*[1] | ::created_at", db]),
        "2023-11-14T22:13:20"
    );
    // the engine evaluates what names a converted column; the source
    // keeps the rest
    assert_eq!(
        ok(&d, &["/flights/*[::distance > 1000_km] @| count", db]),
        "2"
    );
    assert_eq!(
        ok(&d, &["/flights/*[::distance_km > 1000] @| count", db]),
        "0"
    );
    // the child view agrees with the property view: a step by the new
    // name, the locator, the table
    assert_eq!(ok(&d, &["/flights/1/distance", db]), "/flights/1/distance");
    assert_eq!(ok(&d, &["/flights/1/distance_km", db]), "");
    let header = ok(&d, &["/flights/* @| # | :::header", db]);
    assert!(header.contains(":distance<*; quantity>"), "{header}");
    assert!(header.contains(":created_at<*; instant>"), "{header}");
    assert_eq!(
        ok(&d, &["/flights/* @| # | :distance | mean", db]),
        "1318.33333333333_km"
    );
    let (_, _, err) = qua(&d, &["--info", "/flights/*[1] | ::distance", db]);
    assert!(
        err.contains(
            "autounits on flights.db: distance_km → distance in km, time_min → time in min, \
             created_at: instants (epoch seconds); skipped: fare_eur ('eur' is no unit)"
        ),
        "{err}"
    );

    let js = "legs.json?autounits";
    assert_eq!(ok(&d, &["/legs/*[1] | ::distance", js]), "1235_km");
    assert_eq!(ok(&d, &["/legs/*[2] | ::time", js]), "null");
    assert_eq!(
        ok(&d, &["/legs/*[2] | ::created_at", js]),
        "2023-11-14T22:13:20"
    );
    assert_eq!(
        ok(&d, &["/legs/*/distance", js]),
        "/legs/0/distance\n/legs/1/distance"
    );
    assert_eq!(ok(&d, &["/legs/*/distance_km", js]), "");
    assert_eq!(ok(&d, &["//distance | ::", js]), "1235_km\n820_km");
    assert_eq!(
        ok(&d, &["/legs/* @| # | type", js]),
        r#"@("int"; "quantity"; "duration"; "instant")"#
    );
}

#[test]
fn reducers_over_quantities_skip_nulls_and_text() {
    let d = fixture("reducers");
    // the numeric rule, on quantities (ruling 2026-11-08): a null,
    // a bool or text with no reading is missing
    assert_eq!(ok(&d, &["= @(1_kg; null; \"NA\"; 2_kg) | mean"]), "1.5_kg");
    assert_eq!(ok(&d, &["= @(1_kg; null; \"NA\"; 2_kg) | sum"]), "3_kg");
    assert_eq!(ok(&d, &["= @(1_kg; \"NA\"; 3_kg; null) | median"]), "2_kg");
    assert_eq!(ok(&d, &["= @(1_kg; \"NA\"; 3_kg) | min"]), "1_kg");
    assert_eq!(ok(&d, &["= @(1_kg; \"NA\"; 3_kg) | max"]), "3_kg");
    assert_eq!(ok(&d, &["= @(2_h; \"NA\"; null; 30_min) | mean"]), "1.25_h");
    // mixed dimensions, and a bare number off the time dimension,
    // still refuse
    assert_eq!(ok(&d, &["= @(1_kg; 1_s) | sum"]), "null");
    assert_eq!(ok(&d, &["= @(1_kg; 5; 3_kg) | sum"]), "null");
    // the penguins case: a converted column with "NA" cells
    std::fs::write(
        d.join("na.csv"),
        "species,body_mass_g,flipper_mm,price_min,price_max,created_at\n\
         Adelie,3750,181,10,20,2024-01-05\n\
         Adelie,NA,186,12,22,2024-01-06\n\
         Gentoo,3800,NA,n/a,25,2024-01-07\n\
         Gentoo,,190,14,28,2024-01-08\n",
    )
    .unwrap();
    assert_eq!(
        ok(&d, &["/row::body_mass @| mean", "na.csv?autounits"]),
        "3775_g"
    );
}

#[test]
fn the_null_option_reads_cells_as_missing_at_the_csv() {
    let d = fixture("nulls");
    std::fs::write(
        d.join("na.csv"),
        "species,body_mass_g,flipper_mm,price_min,price_max,created_at\n\
         Adelie,3750,181,10,20,2024-01-05\n\
         Adelie,NA,186,12,22,2024-01-06\n\
         Gentoo,3800,NA,n/a,25,2024-01-07\n\
         Gentoo,,190,14,28,2024-01-08\n",
    )
    .unwrap();
    // without the option "NA" is text, and a converted column with
    // one falls to text in a table
    assert_eq!(ok(&d, &["/row[2] | ::body_mass_g", "na.csv"]), "NA");
    let header = ok(&d, &["/row @| # | :::header", "na.csv?autounits"]);
    assert!(header.contains(":body_mass<*; text>"), "{header}");
    // `?null=NA,n/a`: the cells are missing in the rows and in the build
    assert_eq!(
        ok(&d, &["/row[2] | ::body_mass_g", "na.csv?null=NA"]),
        "null"
    );
    assert_eq!(
        ok(&d, &["/row[3] | ::price_min", "na.csv?null=NA,n/a"]),
        "null"
    );
    assert_eq!(
        ok(
            &d,
            &["/row @| # | :::header", "na.csv?autounits&null=NA,n/a"]
        ),
        "@(:species<*; text>; :body_mass<*; quantity>; :flipper<*; quantity>; \
         :price_min<*; int>; :price_max<*; int>; :created_at<*; instant>)"
    );
    assert_eq!(
        ok(&d, &["/row @| # | :body_mass_g | mean", "na.csv?null=NA"]),
        "3775.0"
    );
    assert_eq!(
        ok(&d, &["/row::flipper @| sum", "na.csv?autounits&null=NA"]),
        "557_mm"
    );
    // the option repeats and unions; an empty list refuses
    assert_eq!(
        ok(&d, &["/row[3] | ::price_min", "na.csv?null=NA&null=n/a"]),
        "null"
    );
    let err = refused(&d, &["/row[1]", "na.csv?null="]);
    assert!(
        err.contains("null= names the cell texts read as null, a comma list ('null=NA,n/a')"),
        "{err}"
    );
}

#[test]
fn a_min_beside_a_max_is_the_minimum() {
    let d = fixture("minmax");
    std::fs::write(
        d.join("prices.csv"),
        "item,price_min,price_max,wait_min\na,10,20,5\n",
    )
    .unwrap();
    let (_, out, err) = qua(&d, &["--info", "/row[1] | ::wait", "prices.csv?autounits"]);
    assert_eq!(out, "5_min");
    assert!(
        err.contains(
            "autounits on prices.csv: wait_min → wait in min; skipped: price_min ('min' beside \
             price_max is the minimum), price_max ('max' is no unit)"
        ),
        "{err}"
    );
    assert_eq!(
        ok(&d, &["/row[1] | ::price_min", "prices.csv?autounits"]),
        "10"
    );
    // the named form converts what it names
    assert_eq!(
        ok(&d, &["/row[1] | ::price", "prices.csv?autounit=_min"]),
        "10_min"
    );
}

#[test]
fn titles_read_as_units_on_a_mount_and_slug() {
    // one meaning everywhere (ruling 2026-11-09): `?autounits` reads a
    // unit at the end of a column title as the suffix rule reads a
    // property's `_km`; `%` is the percent unit; `?slugs` slugs
    // the names, the unit off first when both are on
    let d = fixture("titles");
    let t = "quays.csv?autounits";
    assert_eq!(
        ok(&d, &["/row[1] | :props", t]),
        "@(::Port; ::Draught; ::Share; ::'Checked in'; ::Quay; ::Mass)"
    );
    assert_eq!(ok(&d, &["/row[1] | ::Draught", t]), "18.5_m");
    assert_eq!(ok(&d, &["/row[1] | ::Share", t]), "12_%");
    assert_eq!(ok(&d, &["/row[2] | ::Share", t]), "2.2_%");
    assert_eq!(ok(&d, &["/row @| # | :Share", t]), "@(12_%; 2.2_%)");
    let (_, _, err) = qua(&d, &["--info", "/row[1] | ::Draught", t]);
    assert!(
        err.contains(
            "autounits on quays.csv: Draught (m) → Draught in m, Share % → Share in %, \
             Quay m → Quay in m, Mass [kg] → Mass in kg"
        ),
        "{err}"
    );
    // the named form by unit takes the titles naming it
    assert_eq!(
        ok(&d, &["/row[1] | :props", "quays.csv?autounit=_m"]),
        "@(::Port; ::Draught; ::'Share %'; ::'Checked in'; ::Quay; ::'Mass [kg]')"
    );
    // `?slugs`, alone and with the units
    assert_eq!(
        ok(&d, &["/row[1] | :props", "quays.csv?slugs"]),
        "@(::port; ::draught_m; ::share; ::checked_in; ::quay_m; ::mass_kg)"
    );
    let (_, _, err) = qua(&d, &["--info", "/row[1] | ::port", "quays.csv?slugs"]);
    assert!(
        err.contains("slugs on quays.csv: Port → port, Draught (m) → draught_m, Share % → share"),
        "{err}"
    );
    let both = "quays.csv?autounits&slugs";
    assert_eq!(
        ok(&d, &["/row[1] | :props", both]),
        "@(::port; ::draught; ::share; ::checked_in; ::quay; ::mass)"
    );
    assert_eq!(
        ok(&d, &["/row @| # | :::header", both]),
        "@(:port<*; text>; :draught<*; quantity>; :share<*; quantity>; :checked_in<*; int>; \
         :quay<*; quantity>; :mass<*; quantity>)"
    );
    assert_eq!(ok(&d, &["/row @| # | :share | sum", both]), "14.2_%");
    assert_eq!(ok(&d, &["/row @| # | :draught | sum", both]), "32.5_m");
    assert_eq!(
        ok(&d, &["/row[1] | :props", "quays.csv?slugs=-"]),
        "@(::port; ::draught-m; ::share; ::checked-in; ::quay-m; ::mass-kg)"
    );
}
