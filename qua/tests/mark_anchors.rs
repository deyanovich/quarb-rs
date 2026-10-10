//! A mark anchor (`$$.name`) is a node wherever it stands: bare
//! as a navigation command, and with an arrow glued to it.

use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-mark-anchors-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("log.json"),
        r#"{"rows": [{"id": 1}, {"id": 2}, {"id": 3}, {"id": 4, "e-x": 7}]}"#,
    )
    .unwrap();
    dir
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && err.trim().is_empty(),
        "{args:?}: {err}"
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .trim_end()
        .to_string()
}

#[test]
fn a_mark_anchor_stands_bare_and_takes_a_glued_arrow() {
    let dir = fixture();
    // bare, the anchor is the marked node: the thread returns to it
    assert_eq!(
        ok(&dir, &["/rows/3 .e | /.. | $$.e", "log.json"]),
        "/rows/3"
    );
    assert_eq!(ok(&dir, &["/rows/3 .e | $$.e::id", "log.json"]), "4");
    assert_eq!(ok(&dir, &["/rows/3 .e | $$.e/..", "log.json"]), "/rows");
    // an arrow glued to the anchor is an arrow, not part of the name
    assert_eq!(ok(&dir, &["/rows/3 .e | $$.e->next", "log.json"]), "");
    assert_eq!(ok(&dir, &["/rows/3 .e | $$.e-->::id", "log.json"]), "");
    // a hyphen inside the name still belongs to it
    assert_eq!(ok(&dir, &["/rows/3 .e-x | $$.e-x::id", "log.json"]), "4");
}
