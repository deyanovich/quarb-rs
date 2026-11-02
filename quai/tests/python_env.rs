//! A `python:` environment in a session (spec: Environment Mounts):
//! quai mounts `py=python:./helpers.py` as qua does and the module's
//! functions are commands under the alias for every line of the
//! session. Built only with `--features python`.
#![cfg(feature = "python")]

use std::io::Write;
use std::process::{Command, Stdio};

const HELPERS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../qua/tests/fixtures/helpers.py"
);

fn session(args: &[&str], lines: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("quai-python-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("w.json"),
        r#"{"w":["pear","fig"],"p":[{"x":2},{"x":1}]}"#,
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_quai"))
        .args(args)
        .arg("w.json")
        .current_dir(&dir)
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(lines.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_python_mount_serves_every_line_of_the_session() {
    let mount = format!("py=python:{HELPERS}");
    let (ok, out, err) = session(
        &[&mount],
        "/w/* | :: | py:shout\n/p/* | ::x @| py:spread\n/w/0 | :: | py:sub(\"^p\"; \"P\"; $_)\n",
    );
    assert!(ok, "{err}");
    assert!(out.contains("PEAR") && out.contains("FIG"), "{out}");
    assert!(out.contains("4") && out.contains("2"), "{out}");
    assert!(out.contains("Pear"), "{out}");
    // An unmounted alias refuses in the session as in qua.
    let (_, _, err) = session(&[], "/w/* | :: | py:shout\n");
    assert!(err.contains("environment 'py' is not mounted"), "{err}");
    // An interpreter alone is no target.
    let (ok, _, err) = session(&["py=python:python3"], "/w/*\n");
    assert!(
        !ok && err.contains("an interpreter alone is no target"),
        "{err}"
    );
}
