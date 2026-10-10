//! `:plan` prints a line's static plan; `--budget` holds every line
//! to a bound before it runs.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn scratch() -> PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("quai-plan-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("books.json"),
        r#"{"books": [{"title": "Emma", "price": 25}, {"title": "Iliad", "price": 18}]}"#,
    )
    .unwrap();
    dir
}

fn quai(dir: &Path, args: &[&str], lines: &str) -> (String, String, bool) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_quai"))
        .args(args)
        .arg("books.json")
        .current_dir(dir)
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

#[test]
fn plan_prints_the_kaiv_payload() {
    let dir = scratch();
    let (out, err, ok) = quai(&dir, &[], ":plan /books/*/title::\n");
    assert!(ok, "{err}");
    assert!(out.contains("contract=quarb/plan"), "{out}");
    assert!(out.contains("query=/books/*/title::"), "{out}");
    assert!(out.contains("nodes-min=2"), "{out}");
}

#[test]
fn a_budget_refuses_a_line_before_it_runs() {
    let dir = scratch();
    let (out, err, _) = quai(&dir, &["--budget", "nodes=1"], "/books/*/title::\n");
    assert!(err.contains("the budget allows 1"), "{err}");
    assert!(!out.contains("Emma"), "{out}");
    let (out, err, ok) = quai(
        &dir,
        &["--budget", "nodes=100,strict"],
        "/books/*/title::\n",
    );
    assert!(ok, "{err}");
    assert!(out.contains("Emma") && out.contains("Iliad"), "{out}");
    let (_, err, ok) = quai(&dir, &["--budget", "rows"], "/books/*/title::\n");
    assert!(!ok && err.contains("--budget takes nodes=N"), "{err}");
}
