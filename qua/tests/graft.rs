//! `graft(NOTATION)` — grafting on demand (spec: Composition,
//! Grafting): a text topic parses as the notation named, or as it
//! declares itself, and the thread then stands on the parsed
//! document's root in navigation mode, so a hop walks on into what a
//! program or a column handed over as text. A text that does not
//! parse as asked refuses; a null topic stays null.

use quarb::{QueryResult, Value};

fn doc() -> quarb_compose::ComposeAdapter<quarb_json::JsonAdapter> {
    quarb_compose::ComposeAdapter::new(
        quarb_json::JsonAdapter::parse(
            r#"{"rows":[{"id":1,"body":"{\"t\": 1, \"items\": [\"a\", \"b\"]}"},
                       {"id":2,"body":"{\"t\": 2, \"items\": [\"c\"]}"},
                       {"id":3,"body":null}],
                "csv":"a,b\n1,2\n3,4",
                "xml":"<r><x>1</x></r>",
                "declared":"<?xml version=\"1.0\"?><r><x>9</x></r>",
                "prose":"para one\n\npara two"}"#,
        )
        .unwrap(),
    )
}

fn run(q: &str) -> quarb::Result<Vec<String>> {
    let d = doc();
    let out = quarb::Query::parse(q, &d)?.run(&d, &quarb::Run::ambient())?;
    Ok(match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    })
}

fn refuse(q: &str) -> String {
    run(q).unwrap_err().to_string()
}

#[test]
fn a_text_grafts_and_the_thread_walks_on() {
    assert_eq!(
        run(r#"= "{\"t\": 1}" | graft(json) | /t::"#).unwrap(),
        vec!["1"]
    );
    assert_eq!(
        run(r#"= "[1, 2]" | graft(json) | /*::"#).unwrap(),
        vec!["1", "2"]
    );
    assert_eq!(
        run("/csv:: | graft(csv) | /row::a").unwrap(),
        vec!["1", "3"]
    );
    assert_eq!(run("/xml:: | graft(xml) | /r/x::").unwrap(), vec!["1"]);
    // The notation's synonyms read as the notation.
    assert_eq!(run(r#"= "t: 1" | graft(yml) | /t::"#).unwrap(), vec!["1"]);
}

#[test]
fn per_thread_over_a_column_of_texts_with_the_register_kept() {
    assert_eq!(
        run("/rows/* | .id(::id) | ::body | graft(json) | /items/* | %(id = $.id; item = ::)")
            .unwrap(),
        vec![
            "%(id = 1; item = \"a\")",
            "%(id = 1; item = \"b\")",
            "%(id = 2; item = \"c\")"
        ]
    );
    // A null topic stays null, as for every command: the thread
    // stands where it stood, and a hop from there finds no `t`.
    assert_eq!(
        run("/rows/* | .id(::id) | ::body | graft(json) | %(id = $.id; t = /t::)").unwrap(),
        vec![
            "%(id = 1; t = 1)",
            "%(id = 2; t = 2)",
            "%(id = 3; t = null)"
        ]
    );
}

#[test]
fn a_text_that_declares_itself_may_omit_the_operand() {
    assert_eq!(run("/declared:: | graft | /r/x::").unwrap(), vec!["9"]);
    let e = refuse("/xml:: | graft | /r/x::");
    assert!(e.contains("declares no notation"), "{e}");
}

#[test]
fn a_text_that_does_not_parse_as_asked_refuses() {
    let e = refuse(r#"= "not json" | graft(json)"#);
    assert!(e.contains("does not parse as json"), "{e}");
    let e = refuse(r#"= "x" | graft(nope)"#);
    assert!(e.contains("names no notation"), "{e}");
    // The library harness installs no host reader: a reading other
    // than text refuses naming the host.
    let e = refuse(r#"= "x" | graft(corpus)"#);
    assert!(e.contains("not available in this host"), "{e}");
    let e = refuse("/rows/0 | ::id | graft(json)");
    assert!(e.contains("graft reads a text"), "{e}");
}

#[test]
fn a_reading_parses_the_text_as_a_source() {
    assert_eq!(
        run("/prose:: | graft(text) | //paragraph @| count").unwrap(),
        vec!["2"]
    );
}

#[test]
fn the_root_is_free_and_reflection_sees_the_command() {
    // No parent: the graft is a root of its own, not a child of the
    // source's tree.
    assert_eq!(
        run(r#"= "{\"t\": 1}" | graft(json) | \ @| count"#).unwrap(),
        vec!["0"]
    );
    // The parsed document is its own tree: a path inside it is
    // spelled from its root.
    assert_eq!(
        run(r#"= "{\"t\": 1}" | graft(json) | /t | %(p = $$_)"#).unwrap(),
        vec!["%(p = /t)"]
    );
}

/// The readings over a string run in the host (quarb-open installs
/// the string reader), so they are checked through the binary.
fn cli(q: &str) -> (bool, String, String) {
    // Written once: the tests run in parallel, and a file rewritten
    // under a reader is read half-made.
    static FIXTURE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    let json = FIXTURE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("quarb-graft-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let json = dir.join("g.json");
        std::fs::write(
            &json,
        r##"{"md":"# Title\n\nFirst para here. Two sentences!\n\nSecond para.",
            "html":"<!DOCTYPE html><html><body><p>One.</p><p>Two.</p></body></html>",
            "rs":"fn main() {}\nfn other() {}\n",
            "tei":"<TEI xmlns=\"http://www.tei-c.org/ns/1.0\"><text><body><p>Alpha beta.</p></body></text></TEI>"}"##,
        )
        .unwrap();
        json
    });
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .arg(q)
        .arg(json)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .replace('\n', " "),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The binary's answer to `q`, the query refused being a failure
/// that quotes its message.
fn answer(q: &str) -> String {
    let (ok, out, err) = cli(q);
    assert!(ok, "{q}: {err}");
    assert!(!out.is_empty(), "{q}: nothing on stdout; stderr: {err}");
    out
}

#[test]
fn a_reading_prefix_reads_the_string_as_a_mount_would() {
    // The notation's default reading is the DOM; the text reading
    // is the text model; the corpus reading adds the tokens.
    assert_eq!(answer("/md:: | graft(markdown) | //p @| count"), "2");
    assert_eq!(
        cli("/md:: | graft(text:markdown) | //paragraph @| count").1,
        "2"
    );
    assert_eq!(
        answer("/md:: | graft(corpus:markdown) | //sentence @| count"),
        "4"
    );
    let (ok, out, err) = cli("/md:: | graft(corpus:markdown) | //token @| count");
    assert!(ok && out.parse::<i64>().unwrap() > 5, "{out} {err}");
    // A reading alone takes what the string declares, plain text
    // otherwise: the text model of an HTML page.
    assert_eq!(answer("/html:: | graft(text) | //paragraph @| count"), "2");
    // Plain text: its paragraphs by blank lines, the heading line one.
    assert_eq!(answer("/md:: | graft(text) | //paragraph @| count"), "3");
    // The code reading, by the language's name or extension.
    assert_eq!(
        answer("/rs:: | graft(code:rust) | //<function> @| count"),
        "2"
    );
    assert_eq!(
        answer("/rs:: | graft(code:rs) | //<function> @| count"),
        "2"
    );
    // The literary and koine readings over a TEI string.
    assert_eq!(
        answer("/tei:: | graft(lit:tei) | //paragraph @| count"),
        "1"
    );
    assert_eq!(
        cli("/tei:: | graft(koine:xml) | //paragraph @| count").1,
        "1"
    );
}

#[test]
fn readings_refuse_what_they_do_not_read() {
    let (ok, _, err) = cli("/md:: | graft(web) | //p");
    assert!(
        !ok && err.contains("reads a crawl, not a lone string"),
        "{err}"
    );
    let (ok, _, err) = cli("/md:: | graft(code) | //function");
    assert!(!ok && err.contains("name the language"), "{err}");
    let (ok, _, err) = cli("/md:: | graft(lit:markdown) | //paragraph");
    assert!(!ok && err.contains("lit reading reads TEI"), "{err}");
}

#[test]
fn a_graft_inside_a_subcontext_opens_the_way_for_a_hop() {
    // The static mode along an inline pipe: after graft the thread
    // stands on the parsed root, and the next stage may hop.
    assert_eq!(
        run("/rows/* | %(id = ::id; t = (::body | graft(json) | /t::))").unwrap(),
        vec![
            "%(id = 1; t = 1)",
            "%(id = 2; t = 2)",
            "%(id = 3; t = null)"
        ]
    );
    assert_eq!(
        run("/rows/*[(::body | graft(json) | /t::) = 2]/id::").unwrap(),
        vec!["2"]
    );
    // ... and a hop with nothing filed before it still refuses.
    let e = refuse("/rows/* | %(t = (::body | /t::))");
    assert!(e.contains("cannot navigate in scalar mode"), "{e}");
}

#[test]
fn a_literal_grafts_with_no_source_mounted() {
    // The bare root carries the composing layer: a literal or a
    // program's output grafts with no file behind it.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .arg(r#"= "{\"a\": [1, 2]}" | graft(json) | /a/*::"#)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .replace('\n', " "),
        "1 2"
    );
}

/// Under a mount — a model file's `mount` or two sources on the
/// command line — a text grafts through the mount's composing
/// layer, and the thread walks on inside it.
#[test]
fn a_text_grafts_under_a_mount() {
    let dir = std::env::temp_dir().join(format!("quarb-graft-mount-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("events.json"),
        r#"{"events":[{"id":1,"payload":"{\"user\": {\"name\": \"ada\"}}"},
                     {"id":2,"payload":"{\"user\": {\"name\": \"bo\"}}"}]}"#,
    )
    .unwrap();
    std::fs::write(dir.join("audit.model"), "mount audit: events.json;\n").unwrap();
    let qua = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
            .env("QUARB_LOCALE", "C")
            .current_dir(&dir)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).trim_end().to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let q = "/audit/events/*::payload | graft(json) | /user/name::";
    let (out, err) = qua(&["--model", "audit.model", q]);
    assert_eq!(out, "ada\nbo", "{err}");
    let (out, err) = qua(&[q, "audit=events.json", "other=events.json"]);
    assert_eq!(out, "ada\nbo", "{err}");
    std::fs::remove_dir_all(&dir).ok();
}
