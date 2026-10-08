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
