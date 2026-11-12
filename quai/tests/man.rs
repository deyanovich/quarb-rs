//! The manual in quai (the user, 2026-11-12): from the shell,
//! `quai man NAME`, as `qua man` is; and as a line of a session,
//! `man sort`, the form every host of a session takes (the browser
//! extension and the playground have no shell and no colon commands).
use std::io::Write;
use std::process::{Command, Stdio};

fn quai(args: &[&str], lines: &str) -> (String, String, bool) {
    let dir = std::env::temp_dir().join(format!("quai-man-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_quai"))
        .args(args)
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
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// `quai man …` from the shell was taken for a source to mount
/// ("reading man: No such file or directory").
#[test]
fn man_from_the_shell() {
    let (out, err, ok) = quai(&["man", "sort"], "");
    assert!(ok && out.starts_with("sort — "), "{out}{err}");
    assert!(out.contains("  synopsis  sort{stream}("), "{out}");
    let (out, err, ok) = quai(&["man", "<=>"], "");
    assert!(ok && out.starts_with("<=> — correlation\n"), "{out}{err}");
    let (out, _, ok) = quai(&["man"], "");
    assert!(ok && out.lines().count() > 150, "{out}");
    let (out, _, ok) = quai(&["man", "--ru", "upper"], "");
    assert!(ok && out.contains("верхнем"), "{out}");
    let (_, err, ok) = quai(&["man", "qqqq"], "");
    assert!(!ok && err.contains("no command 'qqqq'"), "{err}");
    let (help, _, ok) = quai(&["--help"], "");
    assert!(
        ok && help.contains("`quai man NAME` prints a command's page"),
        "{help}"
    );
}

/// A line that opens with `man` is the manual, with the colon or
/// without; it is no query, so it takes no label.
#[test]
fn man_is_a_line_of_the_session() {
    let (out, err, ok) = quai(
        &[],
        "man upper\nman <=>\n:man @|\nman 'syntax'\n= 1 + 1\n:history\n:quit\n",
    );
    assert!(ok, "{out}{err}");
    assert!(out.contains("upper — the text in upper case\n"), "{out}");
    assert!(out.contains("<=> — correlation\n"), "{out}");
    assert!(out.contains("@| — aggregation pipe\n"), "{out}");
    assert!(out.contains("descendant path"), "{out}");
    assert!(
        !out.contains("expected a navigation operator"),
        "{out}{err}"
    );
    // the one query of the session is line 1
    assert!(out.contains("def &1: = 1 + 1 ;"), "{out}");
    // a word the manual lacks is told of, and the session goes on
    let (out, err, ok) = quai(&[], "man qqqq\n= 2 + 2\n:quit\n");
    assert!(
        ok && out.contains("no command 'qqqq'") && out.contains("4"),
        "{out}{err}"
    );
}
