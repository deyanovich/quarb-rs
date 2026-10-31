//! An edge-declared row is its own edge (ruling 10-28, spec: Edge
//! rows): two flight rows on one leg are two edges — two threads from
//! the hop, `$-::no` naming each, `->flight @| count` counting rows,
//! `@| edges` a row per row, the headless hop one thread per row
//! either way. An edge statement's scope may carry a predicate.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-edgerows-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("flights.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE airports (code TEXT PRIMARY KEY, city TEXT NOT NULL);
         INSERT INTO airports VALUES ('LHR', 'London'), ('JFK', 'New York'), ('CDG', 'Paris');
         CREATE TABLE flights (id INTEGER PRIMARY KEY, no TEXT NOT NULL, carrier TEXT,
           origin TEXT NOT NULL REFERENCES airports(code),
           dest TEXT NOT NULL REFERENCES airports(code),
           minutes INTEGER NOT NULL);
         INSERT INTO flights VALUES
           (1, 'QB100', 'panam', 'LHR', 'JFK', 460),
           (2, 'QB102', 'swissair', 'LHR', 'JFK', 490),
           (3, 'QB101', 'panam', 'JFK', 'LHR', 420),
           (4, 'QB200', 'swissair', 'LHR', 'CDG', 75),
           (5, 'QB300', NULL, 'CDG', 'LHR', 80);",
    )
    .unwrap();
    std::fs::write(
        dir.join("flights.model"),
        "ref /flights/*::origin --> /airports/*[::code = $];\n\
         ref /flights/*::dest --> /airports/*[::code = $];\n\
         edge /flights/*: ::origin -- ::dest as flight;\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("carriers.model"),
        "ref /flights/*::origin --> /airports/*[::code = $];\n\
         ref /flights/*::dest --> /airports/*[::code = $];\n\
         edge /flights/*[::carrier = \"panam\"]: ::origin -- ::dest as panam;\n\
         edge /flights/*[::carrier = \"swissair\"]: ::origin -- ::dest as swissair;\n",
    )
    .unwrap();
    // the label by property: one line for every carrier
    std::fs::write(
        dir.join("by-carrier.model"),
        "ref /flights/*::origin --> /airports/*[::code = $] as ::carrier;\n\
         ref /flights/*::dest --> /airports/*[::code = $];\n\
         edge /flights/*: ::origin -- ::dest as ::carrier;\n",
    )
    .unwrap();
    // a reference no edge statement consumed
    std::fs::write(
        dir.join("ref-only.model"),
        "ref /flights/*::origin --> /airports/*[::code = $];\n",
    )
    .unwrap();
    // a label property that is no text
    std::fs::write(
        dir.join("bad-label.model"),
        "ref /flights/*::origin --> /airports/*[::code = $];\n\
         ref /flights/*::dest --> /airports/*[::code = $];\n\
         edge /flights/*: ::origin -- ::dest as ::minutes;\n",
    )
    .unwrap();
    dir
}

fn fails(dir: &Path, model: &str, query: &str) -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(["--model", model, query, "flights.db"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success(), "{query} succeeded");
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn lines(dir: &Path, model: &str, query: &str) -> Vec<String> {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(["--model", model, query, "flights.db"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{query}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn two_rows_on_one_leg_are_two_edges() {
    let dir = fixture("two");
    let q = |query: &str| lines(&dir, "flights.model", query);
    // the hop: one thread per row; the same node twice
    assert_eq!(q("/airports/LHR->flight @| count"), ["3"]);
    assert_eq!(q("/airports/LHR->flight::code"), ["JFK", "JFK", "CDG"]);
    // each thread's edge is its own row
    assert_eq!(
        q("/airports/LHR->flight | $-::no"),
        ["QB100", "QB102", "QB200"]
    );
    assert_eq!(q("/airports/JFK<-flight | $-::no"), ["QB100", "QB102"]);
    assert_eq!(
        q("/airports/LHR->flight[$-::minutes > 480] | $-::no"),
        ["QB102"]
    );
    // inside a group: one walk per row
    assert_eq!(
        q("/airports/LHR(->flight){1} | $-::no"),
        ["QB100", "QB102", "QB200"]
    );
    assert_eq!(
        q("/airports/LHR(->flight){1} | [::code = \"JFK\"] @| count"),
        ["2"]
    );
    assert_eq!(
        q("/airports/LHR(->flight){1} | [::code = \"JFK\"] @| collapse @| count"),
        ["1"]
    );
    // the headless hop: every row either way, one thread each
    assert_eq!(q("/airports/JFK--flight @| count"), ["3"]);
    assert_eq!(q("/airports/LHR--flight @| count"), ["5"]);
    assert_eq!(
        q("/airports/JFK--flight | $-::no"),
        ["QB101", "QB100", "QB102"]
    );
    // the edges table: a row per row, its cells (the printed table:
    // a header line, then one `%(…)` line per row)
    let table = q("/airports/* @| edges(->flight)");
    let rows: Vec<&String> = table
        .iter()
        .filter(|l| l.trim_start().starts_with("%("))
        .collect();
    assert_eq!(rows.len(), 5, "{table:?}");
    for no in ["QB100", "QB102", "QB200", "QB101", "QB300"] {
        assert!(
            rows.iter().any(|l| l.contains(&format!("\"{no}\""))),
            "{table:?}"
        );
    }
    assert!(
        table[0].contains("no") && table[0].contains("minutes"),
        "{table:?}"
    );
    // the degree over rows: 5 rows over 3 airports
    assert_eq!(q("/airports/* | (->flight @| count) @| sum"), ["5"]);
}

#[test]
fn a_label_by_property_names_each_edge_by_its_row() {
    let dir = fixture("by-carrier");
    let q = |query: &str| lines(&dir, "by-carrier.model", query);
    // the edge: one airline, any, an alliance; `$-` the carrier,
    // `$-::no` the flight
    assert_eq!(q("/airports/LHR->panam | $-::no"), ["QB100"]);
    assert_eq!(q("/airports/LHR->swissair | $-::no"), ["QB102", "QB200"]);
    assert_eq!(
        q("/airports/LHR->* | $-"),
        ["panam", "swissair", "swissair"]
    );
    assert_eq!(q("/airports/LHR->(panam || swissair) @| count"), ["3"]);
    // into JFK by the wildcard: the two edge rows. The dest refs and
    // the carrier-labelled origin ref of the flight leaving it are
    // the references the edge statement consumed, which a wildcard
    // skips and the name still walks (spec: Edge rows)
    let mut into = q("/airports/JFK<-* | $-");
    into.sort();
    assert_eq!(into, ["panam", "swissair"]);
    assert_eq!(q("/airports/JFK<-dest @| count"), ["2"]);
    // the row whose carrier is null declares no edge under either
    // labelled statement: CDG→LHR is no edge and no carrier-labelled
    // ref. The catalog's own foreign-key edges stay, under their
    // column names — the model's statement refined nothing there —
    // reached by name, consumed for the wildcard like the rest.
    assert_eq!(q("/airports/CDG->* @| count"), ["0"]);
    assert_eq!(q("/airports/CDG--* | $-"), ["swissair"]);
    assert_eq!(q("/airports/CDG<-origin::no"), ["QB300"]);
    assert_eq!(q("/flights/5->* @| count"), ["0"]);
    let mut five = q("/flights/5->(origin || dest) | $-");
    five.sort();
    assert_eq!(five, ["dest", "origin"]);
    // the ref: the hop from the row to its origin, under the carrier;
    // its edge is the row, so `$-::no` reads the flight either way
    assert_eq!(q("/flights/1->panam::code"), ["LHR"]);
    assert_eq!(q("/flights/2->swissair::code"), ["LHR"]);
    assert_eq!(q("/flights/2->swissair | $-::no"), ["QB102"]);
    assert_eq!(q("/airports/LHR<-swissair | $-::no"), ["QB102", "QB200"]);
    // a value that is no text refuses at load, naming the statement
    let err = fails(&dir, "bad-label.model", "/airports/LHR->* @| count");
    assert!(
        err.contains("::minutes") && err.contains("no text"),
        "{err}"
    );
}

#[test]
fn an_edge_scope_may_carry_a_predicate() {
    let dir = fixture("carriers");
    let q = |query: &str| lines(&dir, "carriers.model", query);
    assert_eq!(q("/airports/LHR->panam | $-::no"), ["QB100"]);
    assert_eq!(q("/airports/LHR->swissair | $-::no"), ["QB102", "QB200"]);
    assert_eq!(q("/airports/LHR->* @| count"), ["3"]);
    assert_eq!(q("/airports/LHR->(panam || swissair) @| count"), ["3"]);
    assert_eq!(
        q("/airports/LHR->* | $-"),
        ["panam", "swissair", "swissair"]
    );
}

/// A wildcard hop skips the references an edge statement consumed
/// (spec: Edge rows): from an airport `<-*` is the carrier edges in,
/// not the flight rows whose origin or dest it is; `<-origin` named
/// still reaches the rows; a reference no edge consumed stays
/// visible to wildcards.
#[test]
fn wildcards_skip_the_references_an_edge_consumed() {
    let dir = fixture("wild");
    // flights.model: flights 1, 2, 4 leave LHR; 3 and 5 arrive
    assert_eq!(
        lines(&dir, "flights.model", "/airports/LHR->* @| count"),
        ["3"]
    );
    assert_eq!(
        lines(&dir, "flights.model", "/airports/LHR<-* @| count"),
        ["2"]
    );
    assert_eq!(
        lines(&dir, "flights.model", "/airports/LHR--* @| count"),
        ["5"]
    );
    let mut from = lines(&dir, "flights.model", "/airports/LHR<-*::code");
    from.sort();
    assert_eq!(from, ["CDG", "JFK"]);
    // the reference named walks as before
    assert_eq!(
        lines(&dir, "flights.model", "/airports/LHR<-origin @| count"),
        ["3"]
    );
    assert_eq!(
        lines(&dir, "flights.model", "/airports/LHR<-dest @| count"),
        ["2"]
    );
    // from the row, the consumed references are hidden from `->*`
    // and open under their names (the catalog's own labels)
    assert_eq!(
        lines(&dir, "flights.model", "/flights/1->* @| count"),
        ["0"]
    );
    assert_eq!(
        lines(&dir, "flights.model", "/flights/1->origin::code"),
        ["LHR"]
    );
    assert_eq!(
        lines(&dir, "flights.model", "/flights/1->dest::code"),
        ["JFK"]
    );
    // the navop star's crosslink part: no flight row among its ends
    assert_eq!(
        lines(&dir, "flights.model", "/airports/LHR~*[::no] @| count"),
        ["0"]
    );
    // the same under one edge statement per carrier (the rows of no
    // carrier declare no edge, their references are consumed all the
    // same) and under the label by property
    assert_eq!(
        lines(&dir, "carriers.model", "/airports/LHR<-* @| count"),
        ["1"]
    );
    assert_eq!(
        lines(&dir, "carriers.model", "/airports/LHR--* @| count"),
        ["4"]
    );
    assert_eq!(
        lines(&dir, "carriers.model", "/airports/LHR<-origin @| count"),
        ["3"]
    );
    assert_eq!(
        lines(&dir, "by-carrier.model", "/airports/LHR<-* @| count"),
        ["1"]
    );
    assert_eq!(
        lines(&dir, "by-carrier.model", "/airports/LHR->* @| count"),
        ["3"]
    );
    // a reference no edge consumed: wildcards reach the rows
    assert_eq!(
        lines(&dir, "ref-only.model", "/airports/LHR<-* @| count"),
        ["5"]
    );
    assert_eq!(
        lines(&dir, "ref-only.model", "/flights/1->* @| count"),
        ["2"]
    );
    // the induced edges of the airports: the five flights, as before
    assert_eq!(
        lines(&dir, "flights.model", "/airports/* @| edges(--*) | count"),
        ["5"]
    );
}
