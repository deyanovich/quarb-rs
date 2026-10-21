//! The context family (spec: Context Accessors): `@_` the topics,
//! `@@_` the positions, `#.*` / `##.*` the context as a table; `@*`
//! retired.
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    let test = std::thread::current().name().unwrap_or("t").replace(':', "-");
    let dir = std::env::temp_dir().join(format!("quarb-context-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ctx.json"),
        r#"{"a":[{"k":1,"x":10},{"k":2,"x":20},{"k":3,"x":30}]}"#,
    )
    .unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_qua"))
        .current_dir(dir)
        .env("QUARB_LOCALE", "C")
        .env("HOME", dir)
        .args(args)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
        out.status.success(),
    )
}

fn ok(dir: &Path, query: &str) -> String {
    let (out, err, good) = qua(dir, &[query, "ctx.json"]);
    assert!(good, "{query}: {err}");
    out
}

fn err(dir: &Path, query: &str) -> String {
    let (out, err, good) = qua(dir, &[query, "ctx.json"]);
    assert!(!good, "{query} succeeded: {out}");
    err
}

/// `@_` is the peers' topics, the plural of `$_`; `@@_` the peers'
/// positions, a glued projection mapping over them; rounded
/// `(*_)` and `((*_))`.
#[test]
fn the_topics_and_the_positions() {
    let d = fixture();
    assert_eq!(ok(&d, "/a/* | ::x | @_ @| first"), "@(10; 20; 30)");
    assert_eq!(ok(&d, "/a/* | ::x | (*_) @| first"), "@(10; 20; 30)");
    assert_eq!(
        ok(&d, "/a/* | ::x | %(pct = $_ * 100 div sum(@_)) @| first"),
        "%(pct = 16.666666666666668)"
    );
    assert_eq!(ok(&d, "/a/* | @@_ @| first"), "@(/a/0; /a/1; /a/2)");
    assert_eq!(ok(&d, "/a/* | @@_::k @| first"), "@(1; 2; 3)");
    assert_eq!(ok(&d, "/a/* | ((*_))::x @| first"), "@(10; 20; 30)");
    // Inside a map, the element context.
    assert_eq!(
        ok(&d, "= @(1; 3) $| ($_ * 100 div sum(@_))"),
        "@(25; 75)"
    );
}

/// `#.*` is the context as a table of the value regulae, one row
/// per thread, the columns named as `%.*` names them; `##.*` the
/// node regulae. A view: typed by the table rule when read.
#[test]
fn the_context_as_a_table() {
    let d = fixture();
    let q = "/a/* | .k(::k) | .v(::x * 2) | ";
    assert_eq!(
        ok(&d, &format!("{q}#.* @| first")),
        "#(@(k; v);\n  %(1; 20);\n  %(2; 40);\n  %(3; 60))"
    );
    assert_eq!(ok(&d, &format!("{q}#.*:v @| first")), "@(20; 40; 60)");
    assert_eq!(ok(&d, &format!("{q}#.*:2 @| first")), "@(20; 40; 60)");
    assert_eq!(
        ok(&d, "/a/* | .k(::k) | .(::x) | #.* @| first"),
        "#(@(k; .2);\n  %(1; 10);\n  %(2; 20);\n  %(3; 30))"
    );
    assert_eq!(
        ok(&d, "/a/* | .A | .k(::k) | ##.* @| first"),
        "#(@(A);\n  %(\"/a/0\");\n  %(\"/a/1\");\n  %(\"/a/2\"))"
    );
    // A regula that is no cell refuses by name.
    assert!(
        err(&d, "/a/* | .l(@(1; 2)) | #.*").contains("'#.*' holds a list under 'l'")
    );
}

/// `@*` and `(*)` are retired, with the pointer at the three.
#[test]
fn the_old_spelling_points_at_the_family() {
    let d = fixture();
    assert!(err(&d, "/a/* | (@*)").contains("'@*' is retired"));
    assert!(err(&d, "/a/* | @*").contains("'@*' is retired"));
    assert!(err(&d, "/a/* | ((*) @| count)").contains("'@*' is retired"));
}
