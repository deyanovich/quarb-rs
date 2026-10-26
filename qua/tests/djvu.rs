//! A DjVu scan at the text level: `text:scan.djvu` reads the text
//! layer under the outline; the plain mount points there.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-djvu-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("onegin.djvu"),
        include_bytes!("fixtures/onegin.djvu"),
    )
    .unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
        out.status.success(),
    )
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let (out, err, ok) = qua(dir, args);
    assert!(ok, "{args:?}: {err}");
    out
}

#[test]
fn the_text_layer_under_the_outline() {
    let dir = fixture("text");
    assert_eq!(
        ok(&dir, &["/section::lemma", "text:onegin.djvu"]),
        "Глава первая\nГлава вторая"
    );
    assert_eq!(
        ok(&dir, &["/section[2]/section::lemma", "text:onegin.djvu"]),
        "Примечание"
    );
    assert_eq!(
        ok(
            &dir,
            &[
                "//line[2] | %(t = ::; :::page; :::x; :::y)",
                "text:onegin.djvu"
            ]
        ),
        "%(t = \"Мой дядя самых честных правил,\"; page = 1; x = 100; y = 180)"
    );
    assert_eq!(ok(&dir, &["//page @| count", "text:onegin.djvu"]), "3");
    assert_eq!(
        ok(&dir, &["//page[!:::text-layer]:::page", "text:onegin.djvu"]),
        "3"
    );
    assert_eq!(
        ok(&dir, &["^ | %(::title; ::author)", "text:onegin.djvu"]),
        "%(title = \"Евгений Онегин\"; author = \"Пушкин\")"
    );
    assert_eq!(
        ok(&dir, &["//paragraph[2]::", "text:onegin.djvu"]),
        "Мой дядя самых честных правил,\nКогда не в шутку занемог,"
    );
    // the plain mount names the reading
    let (_, err, ok) = qua(&dir, &["/*", "onegin.djvu"]);
    assert!(!ok);
    assert!(err.contains("text:onegin.djvu"), "{err}");
    // as one mount among others
    assert_eq!(
        ok_(&dir, &["/scan//line @| count", "scan=text:onegin.djvu"]),
        "5"
    );
}

fn ok_(dir: &Path, args: &[&str]) -> String {
    ok(dir, args)
}
