//! `--lenient` (the ruling of 2026-11-17): with a translator, a
//! construct that has no exact Quarb counterpart is translated to
//! the nearest one and each divergence is a warning; without the
//! flag it is refused, and the refusal names the flag.

use std::path::PathBuf;

fn fixture() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-lenient-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("events.json"),
        r#"{"events": [
          {"id": 46817, "kind": "read",  "payload": {"duration_ms": 12}},
          {"id": 46818, "kind": "write", "payload": {"duration_ms": 340}},
          {"id": 46819, "kind": "read",  "payload": {"duration_ms": 75}},
          {"id": 46820, "kind": "write", "payload": {}}
        ]}"#,
    )
    .unwrap();
    std::fs::write(dir.join("r.xml"), "<r><x>1</x><x>2</x></r>").unwrap();
    dir
}

/// Whether the run succeeded, its stdout and its stderr.
fn qua(args: &[&str]) -> (bool, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(fixture())
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

const FILTER: &str = ".events | sort_by(.payload.duration_ms) | .[0].id";

#[test]
fn a_by_key_function_is_refused_strictly_and_names_the_flag() {
    let (ok, out, err) = qua(&["--jq", FILTER, "events.json"]);
    assert!(!ok, "{out}");
    // one line: the word of the reason, the Quarb way in the text,
    // the flag in the tail
    assert!(
        err.starts_with("error SORT_NOT_TRANSLATED (jq): the sort_by function"),
        "{err}"
    );
    assert!(
        err.contains("/events/* @| sort_by(/payload/duration_ms::)"),
        "{err}"
    );
    assert!(
        err.trim_end()
            .ends_with("first in jq). Or pass --lenient: qua man SORT_NOT_TRANSLATED"),
        "{err}"
    );
    assert_eq!(err.trim_end().lines().count(), 1, "{err}");
}

#[test]
fn lenient_answers_and_warns_of_each_divergence() {
    let (ok, out, err) = qua(&["--jq", "--lenient", FILTER, "events.json"]);
    assert!(ok, "{err}");
    assert_eq!(out, "46817");
    assert!(err.contains("LENIENT_TRANSLATION"), "{err}");
    assert!(
        err.contains("sort_by: a null key sorts last, where jq sorts it first"),
        "{err}"
    );
    // the ties and the null key of min_by
    let (ok, out, err) = qua(&[
        "--jq",
        "--lenient",
        ".events | min_by(.payload.duration_ms) | .id",
        "events.json",
    ]);
    assert!(ok, "{err}");
    assert_eq!(out, "46817");
    assert!(err.contains("a null key does not compete"), "{err}");
}

#[test]
fn lenient_is_silent_where_no_leniency_was_needed() {
    let strict = qua(&["--jq", ".events[1].id", "events.json"]);
    let lenient = qua(&["--jq", "--lenient", ".events[1].id", "events.json"]);
    assert!(strict.0 && lenient.0, "{} {}", strict.2, lenient.2);
    assert_eq!(lenient.1, "46818");
    assert_eq!(strict.1, lenient.1);
    assert!(!lenient.2.contains("LENIENT_TRANSLATION"), "{}", lenient.2);
    // the XPath bridge alike: an absolute path needs no leniency
    let (ok, out, err) = qua(&["--xpath", "--lenient", "count(/r/x)", "r.xml"]);
    assert!(ok, "{err}");
    assert_eq!(out, "2");
    assert!(!err.contains("LENIENT_TRANSLATION"), "{err}");
}

#[test]
fn lenient_xpath_reads_a_relative_path_from_the_root() {
    let (ok, _, err) = qua(&["--xpath", "count(r/x)", "r.xml"]);
    assert!(!ok);
    assert_eq!(
        err.trim_end(),
        "error PATH_NOT_TRANSLATED (xpath): relative paths (Quarb queries are root-anchored; \
         start with / or //). Or pass --lenient: qua man PATH_NOT_TRANSLATED"
    );
    let (ok, out, err) = qua(&["--xpath", "--lenient", "count(r/x)", "r.xml"]);
    assert!(ok, "{err}");
    assert_eq!(out, "2");
    assert!(
        err.contains("warning LENIENT_TRANSLATION (xpath): a relative path is read from"),
        "{err}"
    );
}

#[test]
fn lenient_alone_is_refused() {
    let (ok, _, err) = qua(&["--lenient", "/events/1::id", "events.json"]);
    assert!(!ok);
    assert!(
        err.contains("--lenient goes with a translator (--jq, --xpath or --sql)"),
        "{err}"
    );
}
