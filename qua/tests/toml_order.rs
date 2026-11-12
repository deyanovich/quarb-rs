//! A TOML table's keys come in the file's order, as a JSON object's
//! and a YAML mapping's do. They came sorted: the `toml` crate was
//! built without its `preserve_order` feature, so its tables were
//! sorted maps (lang's finding on the adapter pages, 2026-11-12).
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    let test = std::thread::current()
        .name()
        .unwrap_or("t")
        .replace(':', "-");
    let dir = std::env::temp_dir().join(format!("quarb-toml-order-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("library.toml"),
        "library = \"Bodleian\"\nopen = true\n\n[shelves]\nzoology = 3\nastronomy = 1\nmusic = 2\n\n\
         [keeper]\nname = \"Thomas\"\nborn = 1545\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("library.yaml"),
        "library: Bodleian\nopen: true\nshelves:\n  zoology: 3\n  astronomy: 1\n  music: 2\n\
         keeper:\n  name: Thomas\n  born: 1545\n",
    )
    .unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_qua"))
        .current_dir(dir)
        .env("QUARB_LOCALE", "C")
        .env("HOME", dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn a_toml_table_keeps_the_file_s_order() {
    let d = fixture();
    // the root's keys and a table's, as written (a table header ends
    // the root's own keys, so the scalars stand first in any TOML)
    assert_eq!(
        qua(&d, &["/* | :name", "library.toml"]),
        "library\nopen\nshelves\nkeeper"
    );
    assert_eq!(
        qua(&d, &["/shelves/* | :name", "library.toml"]),
        "zoology\nastronomy\nmusic"
    );
    assert_eq!(
        qua(&d, &["/keeper | %", "library.toml"]),
        "%(name = \"Thomas\"; born = 1545)"
    );
    // YAML alike, as before
    assert_eq!(
        qua(&d, &["/shelves/* | :name", "library.yaml"]),
        "zoology\nastronomy\nmusic"
    );
    // the text decoded in a query the same
    assert_eq!(
        qua(
            &d,
            &[r#"= "t = { zeta = 1, alpha = 2, mu = 3 }" | decode(toml) | :t | keys"#]
        ),
        r#"@("zeta"; "alpha"; "mu")"#
    );
}
