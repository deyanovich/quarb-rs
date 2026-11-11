//! The offset as a declared property of an instant (the user's
//! rulings of 2026-11-10; spec: Temporal Kinds, the offset trait,
//! Temporal Commands). An instant is written at `Z`, at an offset
//! from UTC, or with none. The trait `<+02:00>` (short `<+2>`, UTC
//! `<Z>`) declares the offset of an instant written with none, the
//! wall time kept and the point moved, and displays one that has an
//! offset at the offset named, the point kept. The components read
//! in the written offset. An instant made from an epoch is `Z`.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-offset-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("events.csv"),
        "at,n,tz\n2024-02-15T10:00:00,1,+02:00\n2024-02-15T23:30:00,2,Z\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("events.json"),
        r#"{"events":[{"id":1,"at":"2024-02-15T10:00:00"},{"id":2,"at":"2024-02-15T23:30:00Z"}]}"#,
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

/// The components read in the offset the instant is written at, no
/// longer in UTC: `hour` and `strftime("%H")` agree, 23:30 at -05:00
/// is in that day.
#[test]
fn the_components_read_in_the_written_offset() {
    let d = fixture("components");
    let q = |query: &str| ok(&d, &[query]);
    let late = "2024-02-15T23:30:00-05:00";
    assert_eq!(q("= 2024-02-15T10:00:00+02:00 | hour"), "10");
    assert_eq!(q(r#"= 2024-02-15T10:00:00+02:00 | strftime("%H")"#), "10");
    assert_eq!(q(&format!("= {late} | date")), "2024-02-15");
    assert_eq!(q(&format!("= {late} | day")), "15");
    assert_eq!(q(&format!("= {late} | hour")), "23");
    assert_eq!(q(&format!("= {late} | weekday")), "4");
    assert_eq!(q(&format!("= {late} | isodate")), "2024-02-15");
    assert_eq!(q(&format!("= {late} | isoweek")), "2024-W07");
    // the text of an instant reads as the instant does
    assert_eq!(q(&format!(r#"= "{late}" | date"#)), "2024-02-15");
    // `truncate` floors in the written offset and keeps it
    assert_eq!(
        q(&format!("= {late} | truncate(1_d)")),
        "2024-02-15T00:00:00-05:00"
    );
    assert_eq!(
        q("= 2024-02-15T10:20:00+05:30 | truncate(1_h)"),
        "2024-02-15T10:00:00+05:30"
    );
    // the period of an instant is the wall clock's
    assert_eq!(q("= 2024-02-29T23:30:00-05:00 | period(month)"), "2024-02");
    assert_eq!(q("= 2024-02-29T23:30:00-05:00 | period(day)"), "2024-02-29");
    // `epoch` and `isoformat` stay on the timeline
    assert_eq!(q(&format!("= {late} | isoformat")), "2024-02-16T04:30:00Z");
    assert_eq!(q(&format!("= {late} | epoch")), "1708057800");
    // with no offset the wall clock is UTC's, as before
    assert_eq!(q("= 2024-02-15T10:00:00 | hour"), "10");
    assert_eq!(q("= 2024-02-15T10:00:00 | truncate(1_d)"), "2024-02-15");
    // comparison stays on the timeline
    assert_eq!(q(&format!("= {late} = 2024-02-16T04:30:00Z")), "true");
}

/// An instant made from an epoch is a point of the timeline, `Z`:
/// "no offset" is what an offset can be declared on, and an epoch is
/// none of those. It prints its `Z`, at midnight too.
#[test]
fn an_epoch_is_z() {
    let d = fixture("epoch");
    let q = |query: &str| ok(&d, &[query]);
    assert_eq!(q("= 1707991200 | datetime"), "2024-02-15T10:00:00Z");
    assert_eq!(q("= 1707955200 | datetime"), "2024-02-15T00:00:00Z");
    assert_eq!(q("= 1707991200 | datetime | offset"), "Z");
    assert_eq!(
        q("= #(@(at<instant>); %(1707991200))"),
        "#(@(at);\n  %(2024-02-15T10:00:00Z))"
    );
    // displayed at an offset, the point kept
    assert_eq!(
        q("= 1707991200 | datetime | offset(+2)"),
        "2024-02-15T12:00:00+02:00"
    );
}

/// The trait declares on an instant written with no offset and
/// displays one that has an offset or `Z`; a date is its midnight
/// there. Where it stands: a header, a record's key, a projection,
/// an accessor, `map`, the constructor's cast.
#[test]
fn the_offset_trait_declares_and_displays() {
    let d = fixture("trait");
    let q = |query: &str| ok(&d, &[query]);
    // declared: the wall time stays
    assert_eq!(
        q("= #(@(at<+02:00>); %(2024-02-15T10:00:00))"),
        "#(@(at);\n  %(2024-02-15T10:00:00+02:00))"
    );
    // displayed: the point stays
    assert_eq!(
        q("= #(@(at<+2>); %(2024-02-15T10:00:00Z))"),
        "#(@(at);\n  %(2024-02-15T12:00:00+02:00))"
    );
    // a half hour written out; a date its midnight there; text read
    assert_eq!(
        q(
            r#"= #(@(at<+05:30>); %(2024-02-15T10:00:00-05:00); %(2024-02-15); %("2024-02-15T01:00:00"))"#
        ),
        "#(@(at);\n  %(2024-02-15T20:30:00+05:30);\n  %(2024-02-15T00:00:00+05:30);\n  \
         %(2024-02-15T01:00:00+05:30))"
    );
    // UTC: `<Z>`, and `<UTC>` and `<+0>` read the same
    for utc in ["Z", "UTC", "+0", "+00:00"] {
        assert_eq!(
            q(&format!("= #(@(at<{utc}>); %(2024-02-15T10:00:00+02:00))")),
            "#(@(at);\n  %(2024-02-15T08:00:00Z))",
            "{utc}"
        );
        assert_eq!(
            q(&format!(
                "= #(@(at<{utc}>); %(2024-02-15T10:00:00)) | header"
            )),
            "@(:at<Z>)",
            "{utc}"
        );
    }
    // the header and the type say the offset, as a unit column its unit
    assert_eq!(
        q("= #(@(at<-5>; t); %(2024-02-15T10:00:00; 2024-02-15T10:00:00)) | header"),
        "@(:at<-05:00>; :t<*; instant>)"
    );
    assert_eq!(
        q("= #(@(at<-5>; n); %(2024-02-15T10:00:00; 1)) | type"),
        r#"@("-05:00"; "int")"#
    );
    // a record's key, a projection, an accessor
    assert_eq!(
        q("= %(at = 2024-02-15T10:00:00) | %(when<-5> = :at)"),
        "%(when = 2024-02-15T10:00:00-05:00)"
    );
    assert_eq!(
        q("= %(at = 2024-02-15T10:00:00) | :at<+2>"),
        "2024-02-15T10:00:00+02:00"
    );
    assert_eq!(
        q("= 2024-02-15T10:00:00Z | .t | $.t<-5>"),
        "2024-02-15T05:00:00-05:00"
    );
    // the components follow it; the epoch shows the point moved
    assert_eq!(q("= %(at = 2024-02-15T10:00:00) | :at<+2> | hour"), "10");
    assert_eq!(
        q("= %(at = 2024-02-15T10:00:00) | :at<+2> | epoch"),
        "1707984000"
    );
    assert_eq!(
        q("= %(at = 2024-02-15T10:00:00) | :at<Z> | epoch"),
        "1707991200"
    );
    // on a source's property, in a predicate too
    assert_eq!(
        ok(&d, &["/* | ::at<-5>", "events.csv"]),
        "2024-02-15T10:00:00-05:00\n2024-02-15T23:30:00-05:00"
    );
    assert_eq!(
        ok(
            &d,
            &["/*[::at<-5> > 2024-02-16T03:00:00Z] | ::n", "events.csv"]
        ),
        "2"
    );
    // `map`, the constructor's cast, the header option
    let t = "#(@(at); %(2024-02-15T10:00:00))";
    let declared = "#(@(at);\n  %(2024-02-15T10:00:00+02:00))";
    assert_eq!(q(&format!("= {t} | map(<+2>; --to=:at)")), declared);
    assert_eq!(q(&format!("= {t} | #(-c=:at<+2>)")), declared);
    assert_eq!(q(&format!("= {t} | #(--header=@(:at<+2>))")), declared);
    // the option reads back as written, `<Z>` canonical
    assert_eq!(
        q("= q(#(--header=@(:at<-05:00>; :b<UTC>))) | :unq"),
        "#(--header=@(:at<-05:00>; :b<Z>))"
    );
    // glued to a projection, a value that is no instant is null; in a
    // header it refuses naming the column
    assert_eq!(q(r#"= %(at = "x") | :at<+2>"#), "null");
    assert_eq!(q("= %(at = 5) | :at<+2>"), "null");
    let e = refused(&d, &["= #(@(at<+2>); %(5))"]);
    assert!(
        e.contains("column 'at' is declared +02:00, and row 1 holds 5"),
        "{e}"
    );
    // a comparison against a negative number is no trait
    assert_eq!(q("= %(a = -5) | (:a < -3)"), "true");
}

/// The refusals: a word of an offset's shape that names none, and
/// the trait on a value, which takes the command.
#[test]
fn an_offset_that_names_none_is_refused() {
    let d = fixture("refusals");
    let e = refused(&d, &["= #(@(at<+19>); %(2024-02-15))"]);
    assert!(
        e.contains("'+19' names no offset: an offset from UTC is at most eighteen hours"),
        "{e}"
    );
    let e = refused(&d, &["= #(@(at<+05:75>); %(2024-02-15))"]);
    assert!(e.contains("the minutes run to 59"), "{e}");
    let e = refused(&d, &["= #(@(at<+2:5>); %(2024-02-15))"]);
    assert!(
        e.contains("the hours are one or two digits and the minutes two"),
        "{e}"
    );
    for q in ["= 2024-02-15T10:00:00<+2>", "= (2024-02-15T10:00:00)<+2>"] {
        let e = refused(&d, &[q]);
        assert!(
            e.contains("puts an offset trait on a value") && e.contains("| offset(+02:00)'"),
            "{q}: {e}"
        );
    }
    // the model's property declaration takes the trait and refuses
    // the same word
    std::fs::write(
        d.join("local.model"),
        "property /events/*::local<+02:00> = ::at;\n",
    )
    .unwrap();
    assert_eq!(
        ok(
            &d,
            &[
                "--model",
                "local.model",
                "/events/* | %(::id; ::local; h = (::local | hour))",
                "events.json"
            ]
        ),
        "%(id = 1; local = 2024-02-15T10:00:00+02:00; h = 10)\n\
         %(id = 2; local = 2024-02-16T01:30:00+02:00; h = 1)"
    );
    std::fs::write(
        d.join("bad.model"),
        "property /events/*::local<+19> = ::at;\n",
    )
    .unwrap();
    let e = refused(
        &d,
        &["--model", "bad.model", "/events/* | ::local", "events.json"],
    );
    assert!(e.contains("'+19' names no offset"), "{e}");
}

/// `offset{instant}(O?)`, the command of the trait. Bare: the offset
/// the instant is written at, as text, null where it has none. With
/// an operand: declared or displayed, as the trait does; the operand
/// written or computed.
#[test]
fn the_offset_command() {
    let d = fixture("command");
    let q = |query: &str| ok(&d, &[query]);
    assert_eq!(q("= 2024-02-15T10:00:00+02:00 | offset"), "+02:00");
    assert_eq!(q("= 2024-02-15T10:00:00Z | offset"), "Z");
    assert_eq!(q("= 2024-02-15T10:00:00 | offset"), "null");
    assert_eq!(q(r#"= "x" | offset"#), "null");
    for o in ["+2", "+02:00", " +2 ", r#""+02:00""#] {
        assert_eq!(
            q(&format!("= 2024-02-15T10:00:00 | offset({o})")),
            "2024-02-15T10:00:00+02:00",
            "{o}"
        );
    }
    assert_eq!(
        q("= 2024-02-15T10:00:00Z | offset(-5)"),
        "2024-02-15T05:00:00-05:00"
    );
    for utc in ["Z", "UTC", "+0"] {
        assert_eq!(
            q(&format!("= 2024-02-15T10:00:00+02:00 | offset({utc})")),
            "2024-02-15T08:00:00Z",
            "{utc}"
        );
    }
    // a zero offset is `Z`, one value
    assert_eq!(q("= 2024-02-15T10:00:00 | offset(+0) | offset"), "Z");
    assert_eq!(
        q("= 2024-02-15 | offset(+05:30)"),
        "2024-02-15T00:00:00+05:30"
    );
    // the offset as data, per row
    assert_eq!(
        ok(
            &d,
            &["/* | (::at | offset(::tz)) | isoformat", "events.csv"]
        ),
        "2024-02-15T08:00:00Z\n2024-02-15T23:30:00Z"
    );
    // a topic that is no instant is null
    assert_eq!(q("= 5 | offset(+2)"), "null");
    // the refusals
    let e = refused(&d, &["= 2024-02-15T10:00:00 | offset(+19)"]);
    assert!(e.contains("offset: '+19' names no offset"), "{e}");
    let e = refused(&d, &["= 2024-02-15T10:00:00 | offset(east)"]);
    assert!(e.contains("offset: 'east' names no offset from UTC"), "{e}");
    let e = refused(&d, &["= 2024-02-15T10:00:00 | offset(+2; +3)"]);
    assert!(e.contains("'offset' takes 1 operand at most"), "{e}");
}

/// A column of instants is converted to another offset as a unit
/// column is to another unit (the user, 2026-11-10): by `map` with
/// the trait, by the header, by the trait glued to the column's
/// read, by the command over the column, and row by row. The points
/// stay; the header says the offset the column is now written at.
#[test]
fn an_instant_column_converts_as_a_unit_column_does() {
    let d = fixture("columns");
    let q = |query: &str| ok(&d, &[query]);
    let t = "#(@(at<+02:00>; n); %(2024-02-15T10:00:00; 1); %(2024-02-15T23:30:00; 2))";
    let at_minus_5 = "#(@(at; n);\n  %(2024-02-15T03:00:00-05:00; 1);\n  \
                      %(2024-02-15T16:30:00-05:00; 2))";
    // the unit twin of each form, for the record
    let u = "#(@(d<_km>; n); %(5; 1); %(12; 2))";
    assert_eq!(
        q(&format!("= {u} | map(<_m>; --to=:d)")),
        "#(@(d; n);\n  %(5000_m; 1);\n  %(12000_m; 2))"
    );
    assert_eq!(q(&format!("= {u} | :d<_m>")), "@(5000_m; 12000_m)");

    assert_eq!(q(&format!("= {t} | map(<-5>; --to=:at)")), at_minus_5);
    assert_eq!(
        q(&format!("= {t} | map(<-5>; --to=:at) | header")),
        "@(:at<-05:00>; :n<*; int>)"
    );
    assert_eq!(
        q(&format!("= {t} | #(--header=@(:at<-5>; :n))")),
        at_minus_5
    );
    assert_eq!(
        q(&format!("= {t} | map(<Z>; --to=:at) | header")),
        "@(:at<Z>; :n<*; int>)"
    );
    // the column read at another offset: the trait glued, the command
    let col = "@(2024-02-15T03:00:00-05:00; 2024-02-15T16:30:00-05:00)";
    assert_eq!(q(&format!("= {t} | :at<-5>")), col);
    assert_eq!(q(&format!("= {t}:at<-5>")), col);
    assert_eq!(q(&format!("= {t} | :at | offset(-5)")), col);
    assert_eq!(
        q(&format!("= {t} | :at | offset(Z)")),
        "@(2024-02-15T08:00:00Z; 2024-02-15T21:30:00Z)"
    );
    // a list of instants, as a list of quantities under `convert`
    assert_eq!(
        q("= @(2024-02-15T10:00:00Z; 2024-02-15T23:30:00+02:00) | offset(-5)"),
        "@(2024-02-15T05:00:00-05:00; 2024-02-15T16:30:00-05:00)"
    );
    // row by row: the trait right of `=` is the value, the key
    // undeclared; on the key it declares the field
    assert_eq!(
        q(&format!("= {t} | @ $| %(:n; x = :at<-5>)")),
        "@%(@(n; x); %(1; 2024-02-15T03:00:00-05:00); %(2; 2024-02-15T16:30:00-05:00))"
    );
    assert_eq!(
        q("= %(at = 2024-02-15T10:00:00Z) | %(u = :at<+2>; v<+2> = :at) | header"),
        "@(:u<*; instant>; :v<+02:00>)"
    );
    assert_eq!(
        ok(
            &d,
            &["/* | %(::n; local<+2> = ::at; u = ::at<Z>)", "events.csv"]
        ),
        "%(n = \"1\"; local = 2024-02-15T10:00:00+02:00; u = 2024-02-15T10:00:00Z)\n\
         %(n = \"2\"; local = 2024-02-15T23:30:00+02:00; u = 2024-02-15T23:30:00Z)"
    );
    // a column declared at one offset and cast to another by `-c`
    // refuses, as a unit column does
    let e = refused(&d, &[&format!("= {t} | #(-c=:at<-5>)")]);
    assert!(
        e.contains("column 'at' is declared +02:00 by the header and cast to -05:00 by -c"),
        "{e}"
    );
}
