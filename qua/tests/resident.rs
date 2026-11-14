//! Resident sessions: a session answers the queries it was started
//! for — its sources from its directory, in its output form — and
//! another client with another form or directory gets a session of
//! its own.

#![cfg(unix)]

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-resident-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for sub in ["a", "b"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
        std::fs::write(dir.join(sub).join("t.txt"), format!("in {sub}\n")).unwrap();
    }
    std::fs::write(dir.join("a/f.json"), r#"{"n": 7}"#).unwrap();
    dir
}

fn qua(cwd: &Path, args: &[&str]) -> String {
    let runtime = std::env::temp_dir().join(format!("quarb-resident-run-{}", std::process::id()));
    std::fs::create_dir_all(&runtime).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .env("XDG_RUNTIME_DIR", &runtime)
        .current_dir(cwd)
        .args(["--resident", "--resident-ttl", "2"])
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn a_session_is_keyed_by_its_output_form_and_directory() {
    let dir = fixture("key");
    // a --json session, then a plain client for the same source
    assert_eq!(qua(&dir.join("a"), &["--json", "/n::", "f.json"]), "[7]");
    assert_eq!(qua(&dir.join("a"), &["/n::", "f.json"]), "7");
    assert_eq!(qua(&dir.join("a"), &["--json", "/n::", "f.json"]), "[7]");
    // the same relative source from two directories
    assert_eq!(qua(&dir.join("a"), &["/paragraph::", "text:t.txt"]), "in a");
    assert_eq!(qua(&dir.join("b"), &["/paragraph::", "text:t.txt"]), "in b");
}

fn qua_refused(cwd: &Path, args: &[&str]) -> String {
    let runtime = std::env::temp_dir().join(format!("quarb-resident-run-{}", std::process::id()));
    std::fs::create_dir_all(&runtime).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .env("XDG_RUNTIME_DIR", &runtime)
        .current_dir(cwd)
        .args(["--resident", "--resident-ttl", "2"])
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "{args:?} answered: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// A session is keyed by the root it was opened at and by the
/// environments it mounts (the review of 2026-11-10): a client that
/// named neither was answered by a session that had them, so it read
/// another tree, and ran a mounted program it had not mounted.
#[test]
fn a_session_is_keyed_by_its_root_and_its_environments() {
    let dir = fixture("reach");
    std::fs::write(dir.join("a/t.json"), r#"{"a": {"b": 1}, "c": 3}"#).unwrap();
    let a = dir.join("a");
    assert_eq!(qua(&a, &["/*:name", "t.json"]), "a\nc");
    assert_eq!(qua(&a, &["--root", "/a", "/*:name", "t.json"]), "b");
    assert_eq!(qua(&a, &["/*:name", "t.json"]), "a\nc");
    // the session that mounted the program answers its own client
    assert_eq!(qua(&a, &["/c:: | bin:cat", "t.json", "bin=exec:/bin"]), "3");
    // and no other: the client that mounted nothing is refused, as a
    // one-off is
    let e = qua_refused(&a, &["/c:: | bin:cat", "t.json"]);
    assert!(e.contains("'bin'"), "{e}");
}

/// A model's mounts under a resident session: the session mounts
/// what the model states, under the names as written (a mount name
/// is an identifier, and admits a dot). quai's daemon mode drives
/// this door; it handed the model's mounts over as inputs as well,
/// each name was mounted twice, and the session refused to start.
#[test]
fn a_session_mounts_what_its_model_states() {
    let dir = fixture("model");
    std::fs::write(
        dir.join("a/shelf.model"),
        "mount f.json: f.json;\nmount notes-1.0: text:t.txt;\n",
    )
    .unwrap();
    let a = dir.join("a");
    assert_eq!(qua(&a, &["--model", "shelf.model", "/f.json/n::"]), "7");
    assert_eq!(
        qua(&a, &["--model", "shelf.model", "/notes-1.0/paragraph::"]),
        "in a"
    );
    assert_eq!(
        qua(&a, &["--model", "shelf.model", "/* | :name"]),
        "notes-1.0\nf.json"
    );
}
