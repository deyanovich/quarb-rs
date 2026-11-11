//! An option is read as the option it is (the code review of
//! 2026-11-10: the syntax rows B2, B3 and B17, the pipeline row F1,
//! and the ruling of the same day on a quoted aggregate push).
//!
//! - a graph command's long flag written first is the flag the
//!   inventory declares, not an undirected hop;
//! - at the top of an option's value a dash pair opens the next
//!   option, a flag as much as a valued one;
//! - an argument that names a column stays a name inside a
//!   parenthesised pipe as in the pipe itself;
//! - the aggregate push takes a quoted name.

use std::path::{Path, PathBuf};

const MODEL: &str = "\
ref /stages/*::from_id --> /stations/*[::id = $];
ref /stages/*::to_id --> /stations/*[::id = $];
edge /stages/*: ::from_id -- ::to_id as stage;
";

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-options-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("roads.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE stations (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
         INSERT INTO stations VALUES (1, 'Ephesus'), (2, 'Sardis'), (3, 'Gates'), (4, 'Euphrates');
         CREATE TABLE stages (id INTEGER PRIMARY KEY, from_id INTEGER REFERENCES stations(id),
           to_id INTEGER REFERENCES stations(id), parasangs TEXT);
         INSERT INTO stages VALUES (1, 1, 2, '18'), (2, 2, 3, '100'), (3, 2, 4, '90'), (4, 3, 4, '20');",
    )
    .unwrap();
    std::fs::write(dir.join("roads.model"), MODEL).unwrap();
    std::fs::write(
        dir.join("rows.json"),
        r#"{"rows":[{"tag":"a"},{},{"tag":"b"}]}"#,
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

fn roads(dir: &Path, query: &str) -> String {
    ok(dir, &["--model", "roads.model", query, "roads.db"])
}

/// A graph command's long flag is the flag its inventory row
/// declares. A list in the parser named seven words; any other long
/// flag written first was read as an undirected hop of that name, so
/// `--harmonic` and `--coefficient` answered zeros.
#[test]
fn a_graph_commands_long_flag_is_its_option() {
    let d = fixture("graph");
    for (long, short) in [
        ("graph:closeness(--harmonic)", "graph:closeness(-H)"),
        ("graph:triangles(--coefficient)", "graph:triangles(-c)"),
    ] {
        let by_long = roads(&d, &format!("/stations/* @| {long}"));
        assert_eq!(
            by_long,
            roads(&d, &format!("/stations/* @| {short}")),
            "{long}"
        );
        assert!(by_long.lines().any(|l| l != "0" && l != "0.0"), "{by_long}");
    }
    // a valued one parses where it was "expected ')'"
    let out = roads(&d, "/stations/* @| graph:communities(--resolution=2)");
    assert!(out.lines().all(|l| l.parse::<i64>().is_ok()), "{out}");
    // a hop written first is still the hop: `stage` is no option
    assert_eq!(
        roads(&d, "/stations/* @| graph:closeness(--stage; --harmonic)"),
        roads(&d, "/stations/* @| graph:closeness(--stage; -H)")
    );
    assert_eq!(
        roads(
            &d,
            "/stations/* @| graph:distance(--stage; --from=/stations/1)"
        ),
        "0\n1\n2\n2"
    );
}

/// At the top of an option's value a dash pair opens the next
/// option of the segment. A flag after a path-valued option was read
/// as an undirected hop on the path, and a path from the root, a
/// mark or an anchor before an option was refused.
#[test]
fn a_dash_pair_ends_the_path_of_an_options_value() {
    let d = fixture("segment");
    let rows = |q: &str| ok(&d, &[q, "rows.json"]);
    assert_eq!(rows("/rows/* @| count(--values --of=/tag)"), "2");
    assert_eq!(rows("/rows/* @| count(--of=/tag --values)"), "2");
    // a path from the root before a flag, refused before ("an
    // operand stands before the segment")
    assert_eq!(
        roads(&d, "/stations/* @| count(--of=^/stations/1 --values)"),
        "4"
    );
    assert_eq!(
        roads(
            &d,
            "/stations/* @| graph:distance(--stage; --from=^/stations/1)"
        ),
        "0\n1\n2\n2"
    );
    // inside a group of the value the dash pair is the hop it is
    // anywhere: the stations two stages from Ephesus
    assert_eq!(
        roads(
            &d,
            "/stations/* @| count(--of=(^/stations/1--stage--stage) --values)"
        ),
        "4"
    );
}

/// An argument that names a column stays a name in a parenthesised
/// pipe as in the pipe. The two positions kept a list each, and
/// `melt`, `clean` and `correlate` were missing from the tail's.
#[test]
fn a_column_name_stays_a_name_in_a_pipe_tail() {
    let d = fixture("tail");
    let t = "#(@(a; b; c); %(1; 2; 3); %(4; 5; 6))";
    let piped = ok(&d, &[&format!("= {t} | melt(:a)")]);
    assert_eq!(ok(&d, &[&format!("= ({t} | melt(:a))")]), piped);
    // inside a record under another topic: `:a` is the table's
    // column, not a field of the topic 1
    assert_eq!(
        ok(&d, &[&format!("= 1 | %(x = ({t} | melt(:a))) | :x")]),
        piped
    );
    let piped = ok(&d, &[&format!("= {t} | clean(:b)")]);
    assert_eq!(ok(&d, &[&format!("= ({t} | clean(:b))")]), piped);
    let u = "(#(@(a; city); %(1; 'Troy'); %(4; 'Ilium')) | #(-i=:a))";
    let piped = ok(&d, &[&format!("= {t} | correlate({u}; --on=:a)")]);
    assert_eq!(
        ok(&d, &[&format!("= ({t} | correlate({u}; --on=:a))")]),
        piped
    );
}

/// The aggregate push takes a quoted name as the plain push does
/// (ruling 2026-11-10): the sigil glued only to an unquoted name.
#[test]
fn the_aggregate_push_takes_a_quoted_name() {
    let d = fixture("push");
    assert_eq!(
        ok(
            &d,
            &[
                "/rows/* | .\"how many\"@(count) | $.\"how many\"",
                "rows.json"
            ]
        ),
        "3\n3\n3"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "/rows/* | .g(::tag ?? 'none') | .\"per tag\"@{.g}(count) | $.\"per tag\"",
                "rows.json"
            ]
        ),
        "1\n1\n1"
    );
    // a recall glued to `@` is still a recall
    assert_eq!(
        ok(&d, &["/rows/* | .n(1) @| sum(--of=$.n)", "rows.json"]),
        "3"
    );
}
