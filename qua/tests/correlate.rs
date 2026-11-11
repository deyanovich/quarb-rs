//! `correlate(t; --on=:col?; --inner? --first-wins? --last-wins?
//! --many?)` (ruling 2026-11-09): the foreign-key join. The topic's
//! key — the column `--on` names, its own index when omitted — is
//! matched against the argument table's index; the topic's rows in
//! the topic's order with its index kept, the argument's columns
//! appended, its index not repeated; an unmatched row keeps nulls,
//! `--inner` drops it; a clashing title lands as `title_2`; a label
//! repeated in the argument's index refuses unless `--first-wins`,
//! `--last-wins` or `--many` says how.

use std::path::{Path, PathBuf};

const QUAYS: &str = r#"(#(@(Port; "Quay, m"; Country); %("Piraeus"; 1200; "Greece"); %("Koper"; 650; "Slovenia"); %("Rijeka"; 400; "Croatia")) | #(-i=:Port))"#;
const DOUBLED: &str =
    r#"(#(@(Port; Quay); %("Piraeus"; 1200); %("Koper"; 650); %("Koper"; 700)) | #(-i=:Port))"#;

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-correlate-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("t1.csv"),
        "Port,2023\nPiraeus,5200000\nKoper,1017788\nRijeka,330500\nBar,1234\n",
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
fn the_topic_keeps_its_rows_and_the_argument_appends_its_columns() {
    let d = fixture("join");
    let q = format!("/row @| # | correlate({QUAYS}; --on=:Port)");
    assert_eq!(
        ok(&d, &[&q, "t1.csv"]),
        "#(@(Port; \"2023\"; \"Quay, m\"; Country);\n  %(\"Piraeus\"; 5200000; 1200; \"Greece\");\n  \
         %(\"Koper\"; 1017788; 650; \"Slovenia\");\n  %(\"Rijeka\"; 330500; 400; \"Croatia\");\n  \
         %(\"Bar\"; 1234; null; null))"
    );
    let (_, _, err) = qua(&d, &["--info", &q, "t1.csv"]);
    assert!(
        err.contains("correlate on the table: 4 rows, 3 matched, 1 unmatched kept with nulls"),
        "{err}"
    );
    // the appended columns keep their types; the key stays a column
    assert_eq!(
        ok(&d, &[&format!("{q} | :::header"), "t1.csv"]),
        "@(:Port<*; text>; :2023<*; int>; :'Quay, m'<*; int>; :Country<*; text>)"
    );
    assert_eq!(
        ok(&d, &[&format!("{q} | corr(:2023; :\"Quay, m\")"), "t1.csv"]),
        "0.9839115275131508"
    );
    // --inner drops the unmatched
    let inner = format!("/row @| # | correlate({QUAYS}; --on=:Port --inner)");
    assert_eq!(ok(&d, &[&format!("{inner} | dim"), "t1.csv"]), "@(3; 4)");
    let (_, _, err) = qua(&d, &["--info", &inner, "t1.csv"]);
    assert!(err.contains("1 unmatched dropped"), "{err}");
    // --on omitted: the topic's index is the key, and stays the index
    let by_index = format!("/row @| # | #(-i=:Port) | correlate({QUAYS})");
    assert_eq!(
        ok(&d, &[&format!("{by_index} | :::index"), "t1.csv"]),
        r#"@("Piraeus"; "Koper"; "Rijeka"; "Bar")"#
    );
    // (a column of an indexed table reads as a record over the index)
    assert_eq!(
        ok(&d, &[&format!("{by_index} | :\"Quay, m\""), "t1.csv"]),
        "%(Piraeus = 1200; Koper = 650; Rijeka = 400; Bar = null)"
    );
    // --on may name the topic's own index; a quoted title too
    assert_eq!(
        ok(
            &d,
            &[
                &format!("/row @| # | #(-i=:Port) | correlate({QUAYS}; --on=:Port) | dim"),
                "t1.csv"
            ]
        ),
        "@(4; 4)"
    );
    assert_eq!(
        ok(
            &d,
            &[
                &format!("/row @| # | correlate({QUAYS}; --on=:\"Port\") | dim"),
                "t1.csv"
            ]
        ),
        "@(4; 4)"
    );
    // a key matching nothing: every appended cell null
    assert_eq!(
        ok(
            &d,
            &[
                &format!("/row @| # | correlate({QUAYS}; --on=:2023) | :Country"),
                "t1.csv"
            ]
        ),
        "@(null; null; null; null)"
    );
}

#[test]
fn a_clashing_title_lands_as_title_2_and_is_told() {
    let d = fixture("clash");
    let arg = r#"(#(@(Port; "2023"); %("Piraeus"; 9); %("Bar"; 8)) | #(-i=:Port))"#;
    let q = format!("/row @| # | correlate({arg}; --on=:Port)");
    assert_eq!(
        ok(&d, &[&format!("{q} | :::header"), "t1.csv"]),
        "@(:Port<*; text>; :2023<*; int>; :'2023_2'<*; int>)"
    );
    let (_, _, err) = qua(&d, &["--info", &q, "t1.csv"]);
    assert!(err.contains("; renamed: 2023 → 2023_2"), "{err}");
}

#[test]
fn a_repeated_label_in_the_argument_refuses_unless_told_how() {
    let d = fixture("repeat");
    let err = refused(
        &d,
        &[
            &format!("/row @| # | correlate({DOUBLED}; --on=:Port)"),
            "t1.csv",
        ],
    );
    assert!(
        err.contains(
            "the operand's index is not unique: 'Koper' twice (rows 2 and 3); --first-wins or \
             --last-wins take one row of a repeated label, --many every row"
        ),
        "{err}"
    );
    let first = format!("/row @| # | correlate({DOUBLED}; --on=:Port --first-wins)");
    assert_eq!(
        ok(&d, &[&format!("{first} | :Quay"), "t1.csv"]),
        "@(1200; 650; null; null)"
    );
    let (_, _, err) = qua(&d, &["--info", &first, "t1.csv"]);
    assert!(
        err.contains("repeated in the operand's index: 'Koper' ×2, the first row taken"),
        "{err}"
    );
    assert_eq!(
        ok(
            &d,
            &[
                &format!("/row @| # | correlate({DOUBLED}; --on=:Port --last-wins) | :Quay"),
                "t1.csv"
            ]
        ),
        "@(1200; 700; null; null)"
    );
    // --many: one row per match, the topic's row and index repeated
    let many = format!("/row @| # | #(-i=:Port) | correlate({DOUBLED}; --many)");
    assert_eq!(
        ok(&d, &[&format!("{many} | :::index"), "t1.csv"]),
        r#"@("Piraeus"; "Koper"; "Koper"; "Rijeka"; "Bar")"#
    );
    let (_, _, err) = qua(&d, &["--info", &many, "t1.csv"]);
    assert!(err.contains("; --many: 5 rows from 4"), "{err}");
    assert_eq!(
        ok(
            &d,
            &[
                &format!("/row @| # | correlate({DOUBLED}; --on=:Port --many --inner) | :Quay"),
                "t1.csv"
            ]
        ),
        "@(1200; 650; 700)"
    );
    let err = refused(
        &d,
        &[
            &format!("/row @| # | correlate({QUAYS}; --on=:Port --first-wins --many)"),
            "t1.csv",
        ],
    );
    assert!(
        err.contains(
            "--first-wins, --last-wins and --many each choose a way with a repeated label; name one"
        ),
        "{err}"
    );
}

#[test]
fn the_refusals_name_what_is_missing() {
    let d = fixture("refuse");
    let err = refused(
        &d,
        &[
            &format!("/row @| # | correlate({QUAYS}; --on=:Nope)"),
            "t1.csv",
        ],
    );
    assert!(
        err.contains("the topic has no column 'Nope'; its columns: Port, 2023"),
        "{err}"
    );
    let err = refused(&d, &[&format!("/row @| # | correlate({QUAYS})"), "t1.csv"]);
    assert!(
        err.contains(
            "the topic has no index to match on: name the key with --on=:col, or index it with #(-i=:col)"
        ),
        "{err}"
    );
    let err = refused(
        &d,
        &[
            "/row @| # | correlate(#(@(Port; x); %(\"Piraeus\"; 1)); --on=:Port)",
            "t1.csv",
        ],
    );
    assert!(
        err.contains("the operand's index is the key: set it with #(-i=:col)"),
        "{err}"
    );
    let err = refused(&d, &["/row @| # | correlate(12; --on=:Port)", "t1.csv"]);
    assert!(
        err.contains("the operand is a number, not a table: correlate($,t2; --on=:col)"),
        "{err}"
    );
    let err = refused(
        &d,
        &[
            &format!("/row @| # | correlate({QUAYS}; --on=@(:Port; :\"2023\"))"),
            "t1.csv",
        ],
    );
    assert!(err.contains("one key column for now: --on=:col"), "{err}");
}
