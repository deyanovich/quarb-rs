//! Finding 17: `text:` over an archive reads the same on a named
//! mount as on the single flow — every marked-up member grafted as
//! the reader's model.

use std::io::Write;
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    // one directory per test: the file's tests run in one process
    // and each removes and recreates its fixture
    let test = std::thread::current()
        .name()
        .unwrap_or("t")
        .replace(':', "-");
    let dir =
        std::env::temp_dir().join(format!("quarb-text-archive-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut cursor);
        let opts = zip::write::SimpleFileOptions::default();
        z.start_file("a.html", opts).unwrap();
        z.write_all(b"<html><body><h1>Alpha</h1><p>One.</p><p>Two.</p></body></html>")
            .unwrap();
        z.start_file("b.md", opts).unwrap();
        z.write_all(b"# Beta\n\nThree.\n").unwrap();
        z.finish().unwrap();
    }
    std::fs::write(dir.join("site.zip"), cursor.into_inner()).unwrap();
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
fn an_archive_at_the_text_level_mounts_as_it_reads_alone() {
    let dir = fixture();
    assert_eq!(ok(&dir, &["//paragraph @| count", "text:site.zip"]), "3");
    assert_eq!(
        ok(&dir, &["//paragraph @| count", "site=text:site.zip"]),
        "3"
    );
    assert_eq!(
        ok(&dir, &["/site//section::lemma", "site=text:site.zip"]),
        "Alpha\nBeta"
    );
}

#[test]
fn an_entrys_size_mirrors_as_a_data_property() {
    // The mirroring rule: an adapter field answers at `::` where no
    // conflict is possible, `::size` beside `:::size`, as the file
    // system's does — on an entry that grafts too, since data wins
    // and the document has no `size`.
    let dir = fixture();
    let both = ok(&dir, &["/b.md | %(a = ::size; b = :::size)", "site.zip"]);
    assert!(both.contains("_B"), "{both}");
    let (a, b) = both
        .trim_start_matches("%(a = ")
        .trim_end_matches(')')
        .split_once("; b = ")
        .unwrap();
    assert_eq!(a, b, "{both}");
    assert_ne!(a, "null", "{both}");
}
