//! The `.sentences` file (lang design/sentences-file.md): a given
//! segmentation mounted with `?sentences=`, headings as sentences
//! everywhere, `:::at` / `:::end` as the coordinates that survive
//! a resegmentation.

use std::path::{Path, PathBuf};

fn scratch() -> PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("qua-sentences-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("play.md"),
        "# The Play\n\nA word here. Another\none!\n\n## Act Two\n\nLast words.\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("play.sentences"),
        "The Play\nA word here. Another one!\n\nAct Two\nLast words.\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("wrong.sentences"),
        "The Play\nA word there.\nAct Two\nLast words.\n",
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

#[test]
fn headings_are_sentences_in_the_computed_tier() {
    let dir = scratch();
    let (out, err, ok) = qua(&dir, &["//sentence | ::", "corpus:play.md"]);
    assert!(ok, "{err}");
    assert_eq!(
        out,
        "The Play\nA word here.\nAnother one!\nAct Two\nLast words."
    );
    let (out, _, _) = qua(&dir, &["//sentence[3]:::at", "corpus:play.md"]);
    assert_eq!(out, "13");
    // A heading is one sentence whatever its punctuation, as on the
    // stored tiers.
    std::fs::write(
        dir.join("chapters.md"),
        "# The Book\n\n## Chapter I. The Beginning\n\nA word here. Another one!\n",
    )
    .unwrap();
    let (out, err, ok) = qua(&dir, &["//sentence | ::", "corpus:chapters.md"]);
    assert!(ok, "{err}");
    assert_eq!(
        out,
        "The Book\nChapter I. The Beginning\nA word here.\nAnother one!"
    );
    let (out, _, _) = qua(
        &dir,
        &["//sentence[2]/token @| count", "corpus:chapters.md"],
    );
    assert_eq!(out, "5");
}

#[test]
fn a_sentences_file_is_the_given_tier() {
    let dir = scratch();
    let (out, err, ok) = qua(
        &dir,
        &["//sentence | ::", "corpus:play.md?sentences=play.sentences"],
    );
    assert!(ok, "{err}");
    assert_eq!(
        out,
        "The Play\nA word here. Another one!\nAct Two\nLast words."
    );
    // Line k is the sentence with :::n = k.
    let (out, _, _) = qua(
        &dir,
        &[
            "//sentence[:::n = 2]::",
            "corpus:play.md?sentences=play.sentences",
        ],
    );
    assert_eq!(out, "A word here. Another one!");
    let (_, err, ok) = qua(
        &dir,
        &[
            "//sentence @| count",
            "corpus:play.md?sentences=wrong.sentences",
        ],
    );
    assert!(!ok);
    assert!(err.contains("line 2 does not match"), "{err}");
    let (_, err, ok) = qua(
        &dir,
        &[
            "//sentence @| count",
            "corpus:play.md?sentences=play.sentences&desm=x.desm",
        ],
    );
    assert!(!ok);
    assert!(err.contains("desm= does not apply"), "{err}");
}
