//! Draft 181: the budget quantifier and the edge facts on a
//! foreign-key graph. Stations and stages, the stage rows the edges
//! (`edge … as stage;`), their cells the edges' properties — a
//! quantity text column with a model-declared unit.

use std::path::{Path, PathBuf};

const MODEL: &str = "\
unit stade, stades = 185 m;
unit parasang, parasangs = 30 stade;
ref /stages/*::from_id --> /stations/*[::id = $];
ref /stages/*::to_id --> /stations/*[::id = $];
edge /stages/*: ::from_id -- ::to_id as stage;
";

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-roads-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("roads.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE stations (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
         INSERT INTO stations VALUES (1, 'Ephesus'), (2, 'Sardis'), (3, 'Gates'), (4, 'Euphrates');
         CREATE TABLE stages (id INTEGER PRIMARY KEY, from_id INTEGER REFERENCES stations(id),
           to_id INTEGER REFERENCES stations(id), parasangs TEXT, days TEXT, source TEXT);
         INSERT INTO stages VALUES
           (1, 1, 2, '18 parasang', '3 d', 'Herodotus'),
           (2, 2, 3, '100 parasang', '20 d', 'Herodotus'),
           (3, 2, 4, '90 parasang', '15 d', 'Xenophon'),
           (4, 3, 4, '20 parasang', '4 d', 'Herodotus');",
    )
    .unwrap();
    std::fs::write(dir.join("roads.model"), MODEL).unwrap();
    dir
}

fn qua(dir: &Path, query: &str) -> Vec<String> {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(["--model", "roads.model", query, "roads.db"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{query}: {err}");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn a_declared_edge_is_its_row() {
    let dir = fixture("edge");
    // `as stage` names the edge; its stored direction is the row's,
    // from_id to to_id; `--stage` reads both ways.
    assert_eq!(
        qua(&dir, "/stations/2->stage::name"),
        ["Gates", "Euphrates"]
    );
    assert_eq!(qua(&dir, "/stations/2<-stage::name"), ["Ephesus"]);
    assert_eq!(
        qua(&dir, "/stations/2--stage::name"),
        ["Gates", "Euphrates", "Ephesus"]
    );
    // The row's cells are the edge's properties: `$-` for the
    // command after a group's exit, `@-` (the final-hop edges, a
    // list) after a plain hop.
    assert_eq!(
        qua(
            &dir,
            "/stations/2(->stage){1} | %(::name; len = $-::parasangs; by = $-::source)"
        ),
        [
            r#"%(name = "Gates"; len = "100 parasang"; by = "Herodotus")"#,
            r#"%(name = "Euphrates"; len = "90 parasang"; by = "Xenophon")"#
        ]
    );
    assert_eq!(
        qua(&dir, "/stations/2->stage | %(::name; len = @-::parasangs)"),
        [
            r#"%(name = "Gates"; len = @("100 parasang"))"#,
            r#"%(name = "Euphrates"; len = @("90 parasang"))"#
        ]
    );
    // A reducer reads the declared unit as a single value does.
    assert_eq!(
        qua(&dir, "/stages/* | ::parasangs @| sum"),
        ["228_parasang"]
    );
    assert_eq!(
        qua(&dir, "/stages/* | ::parasangs @| sum | convert(km)"),
        ["1265.4_km"]
    );
}

#[test]
fn an_acyclic_edge_expands_by_node_and_refuses_a_cycle() {
    let dir = fixture("acyclic");
    let model = format!("{}", MODEL.replace("as stage;", "as stage acyclic;"));
    std::fs::write(dir.join("roads.model"), &model).unwrap();
    // the promise changes no answer: the exact forms expand by node
    // and round
    assert_eq!(
        qua(&dir, "/stations/1(->stage){2}::name"),
        ["Gates", "Euphrates"]
    );
    assert_eq!(qua(&dir, "/stations/1(->stage)+!::name"), ["Euphrates"]);
    assert_eq!(
        qua(&dir, "/stations/1(->stage){;200_parasang}!::name"),
        ["Euphrates"]
    );
    // a stage back to Ephesus closes a cycle: an exact walk under
    // the promise refuses naming the declaration
    let db = rusqlite::Connection::open(dir.join("roads.db")).unwrap();
    db.execute_batch("INSERT INTO stages VALUES (5, 4, 1, '200 parasang', '40 d', 'nobody');")
        .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .current_dir(&dir)
        .args([
            "--model",
            "roads.model",
            "/stations/1(->stage){4}::name",
            "roads.db",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(
            "a cycle under the promise 'edge /stages/*: ::from_id -- ::to_id as stage acyclic'"
        ),
        "{err}"
    );
    assert!(
        err.contains("the walk from /stations/1 returns to /stations/1 after 3 hops"),
        "{err}"
    );
}

#[test]
fn a_budget_walks_the_stages() {
    let dir = fixture("budget");
    // Within 120 parasangs of Sardis, one thread per walk (spec:
    // Expansion paths): Sardis itself (the start at zero), the
    // Gates (100), the Euphrates by Xenophon (90), Ephesus (18), the
    // Euphrates by the Gates (120) and the Gates by the Euphrates
    // (110); collapsed, each station once.
    assert_eq!(
        qua(&dir, "/stations/2(--stage){;120_parasang}::name"),
        [
            "Sardis",
            "Gates",
            "Euphrates",
            "Ephesus",
            "Euphrates",
            "Gates"
        ]
    );
    assert_eq!(
        qua(
            &dir,
            "/stations/2(--stage){;120_parasang} @| collapse | ::name"
        ),
        ["Sardis", "Gates", "Euphrates", "Ephesus"]
    );
    // The lightest way to the Euphrates: Xenophon's 90, and its total
    // read by the one command after the walk, in the budget's unit.
    assert_eq!(
        qua(
            &dir,
            "/stations/2(->stage){;200_parasang}[::name = \"Euphrates\"]? | $-:total"
        ),
        ["90_parasang"]
    );
    assert_eq!(
        qua(
            &dir,
            "/stations/2(->stage){;200_parasang}[::name = \"Euphrates\"]? | $-:hops"
        ),
        ["1"]
    );
    // The heaviest within the budget: by the Gates, 120.
    assert_eq!(
        qua(
            &dir,
            "/stations/2(->stage){;200_parasang}[::name = \"Euphrates\"]! | $-:total"
        ),
        ["120_parasang"]
    );
    // A budget in kilometres converts: 90 parasang is 499.5 km.
    assert_eq!(
        qua(
            &dir,
            "/stations/2(->stage){;500_km}[::name = \"Euphrates\"]? | $-:total"
        ),
        ["499.5_km"]
    );
    // A time budget sums the days.
    assert_eq!(
        qua(&dir, "/stations/2(->stage){;16_d}::name"),
        ["Sardis", "Euphrates"]
    );
}
