//! A refused query is a message, not a crash (the outside review of
//! 2026-11-10): under `RUST_BACKTRACE=1` every error captured a
//! stack, and the refusal printed it after its causes. The variable
//! keeps its meaning for a panic; the stack of an error is asked for
//! by its own name, `RUST_LIB_BACKTRACE=1`.

use std::path::PathBuf;

fn fixture() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-backtrace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("x.json"), "{}").unwrap();
    dir
}

/// The refusal's text under the given variables, the two of the
/// runtime cleared first.
fn refusal(dir: &PathBuf, args: &[&str], env: &[(&str, &str)]) -> String {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_qua"));
    cmd.env("QUARB_LOCALE", "C")
        .env_remove("RUST_BACKTRACE")
        .env_remove("RUST_LIB_BACKTRACE")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{args:?} is refused");
    String::from_utf8(out.stderr).unwrap()
}

#[test]
fn a_refusal_prints_no_stack_under_rust_backtrace() {
    let dir = fixture();
    // a parse error, and a refusal with a cause under it
    for args in [&["/a[", "x.json"][..], &["/a", "missing.json"][..]] {
        let plain = refusal(&dir, args, &[]);
        assert!(plain.starts_with("Error: "), "{plain}");
        assert!(!plain.contains("Stack backtrace"), "{plain}");
        for value in ["1", "full"] {
            let traced = refusal(&dir, args, &[("RUST_BACKTRACE", value)]);
            assert_eq!(traced, plain, "RUST_BACKTRACE={value} changes nothing");
        }
        // asked for by name, the stack follows the refusal
        let asked = refusal(&dir, args, &[("RUST_LIB_BACKTRACE", "1")]);
        assert!(asked.starts_with(plain.trim_end()), "{asked}");
        assert!(asked.contains("Stack backtrace"), "{asked}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
