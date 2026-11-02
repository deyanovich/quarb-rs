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

/// A session driven with its own environment: `env` pairs set, the
/// NO_COLOR of the harness lifted.
fn quai_env(dir: &Path, args: &[&str], lines: &str, env: &[(&str, &str)]) -> (String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_quai"));
    cmd.args(args)
        .arg("books.json")
        .current_dir(dir)
        .env_remove("NO_COLOR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
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
    )
}

#[test]
fn the_level_flags_and_the_colour_gate_reach_the_session() {
    let dir = scratch();
    // The default level shows no info; `--info` and
    // `--diagnostics=info` show the same lines, plain on a pipe.
    let (quiet, _, _) = quai(&dir, &[], "/books/* | ::title\n");
    assert!(!quiet.contains("info "), "{quiet}");
    let (long, _, _) = quai(&dir, &["--diagnostics=info"], "/books/* | ::title\n");
    let (short, _, _) = quai(&dir, &["--info"], "/books/* | ::title\n");
    assert_eq!(long, short);
    assert!(!long.contains('\x1b'), "{long}");
    // `--color=always` paints the severity word alone, on stdout
    // where quai's diagnostics go, and the `error` of a refusal on
    // stderr.
    let (out, err) = quai_env(
        &dir,
        &["--color=always", "--info"],
        "/books/* | ::title\n/books/* | nope\n",
        &[],
    );
    if out.contains("info ") {
        assert!(out.contains("\x1b[32minfo\x1b[0m "), "{out}");
    }
    assert!(err.contains("\x1b[1;31merror\x1b[0m: "), "{err}");
    // A pipe is no terminal: the automatic rule leaves both plain.
    let (out, err) = quai_env(&dir, &["--info"], "/books/* | nope\n", &[]);
    assert!(
        !out.contains('\x1b') && !err.contains('\x1b'),
        "{out} {err}"
    );
    assert!(err.contains("error: "), "{err}");
}
