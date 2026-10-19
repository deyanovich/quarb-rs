//! Trait segments (spec: Traits): a trait is a colon-joined
//! sequence of segments; `.` stands for one whole segment and `*`
//! for a run of one or more, and neither is ever part of a segment.
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-traits-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
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

const PAGE: &str = r##"<!doctype html>
<html lang="en">
  <head>
    <title>Herodotus — Book One</title>
    <meta name="keywords" content="Lydia, Croesus, Persia">
    <meta property="article:section" content="Histories">
  </head>
  <body><main><h1>Book One</h1><p>Croesus was a Lydian by birth.</p></main></body>
</html>"##;

/// The curated HTML reading lands its tags as `tag:NAME` and the
/// category as `category:NAME` on the document; the segment
/// wildcards reach the families, the literal names stay literal.
#[test]
fn segment_wildcards_over_a_documents_traits() {
    let dir = fixture("html");
    std::fs::write(dir.join("one.html"), PAGE).unwrap();
    let count = |q: &str| qua(&dir, &[&format!("{q} @| count"), "page=text:one.html"]);
    // the document carries three tags and one category
    assert_eq!(
        qua(&dir, &["/page:traits", "page=text:one.html"]),
        r#"@("tag:Lydia"; "tag:Croesus"; "tag:Persia"; "category:Histories")"#
    );
    assert_eq!(count("//*<tag:Lydia>"), "1");
    assert_eq!(count("//*<tag:.>"), "1");
    assert_eq!(count("//*<tag:*>"), "1");
    assert_eq!(count("//*<*:Histories>"), "1");
    assert_eq!(count("//*<.:Histories>"), "1");
    assert_eq!(count("//*<category:. && tag:.>"), "1");
    // the bare namespace is not a trait, and a star is not a glob
    assert_eq!(count("//*<tag>"), "0");
    assert_eq!(count("//*<tag*>"), "0");
    assert_eq!(count("//*<Lyd*>"), "0");
    // `.` is one unnamespaced segment: the blocks, not the document
    assert_eq!(count("//*<.>"), "2");
    assert_eq!(count("//*<. && !tag:*>"), "2");
    // the rounded spelling reads the same
    assert_eq!(count("//*(:tag:.)"), "1");
    assert_eq!(count("//*(:!tag:*)"), "2");
}
