//! An HWPX package at the doors: `text:` reads it in the Word
//! reading's vocabulary, `corpus:` reads its prose, `lit:` refuses
//! naming it, and the plain mount opens the package as a zip.
use std::io::Write;
use std::path::{Path, PathBuf};

fn package() -> Vec<u8> {
    let ns = r#"xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph" xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section" xmlns:hh="http://www.hancom.co.kr/hwpml/2011/head""#;
    let p = |pr: u32, text: &str| {
        format!(
            r#"<hp:p id="0" paraPrIDRef="{pr}" styleIDRef="0"><hp:run charPrIDRef="0"><hp:t>{text}</hp:t></hp:run></hp:p>"#
        )
    };
    let header = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><hh:head {ns} version="1.4" secCnt="1"><hh:refList><hh:paraProperties itemCnt="2"><hh:paraPr id="0"><hh:heading type="NONE" idRef="0" level="0"/></hh:paraPr><hh:paraPr id="1"><hh:heading type="OUTLINE" idRef="0" level="0"/></hh:paraPr></hh:paraProperties></hh:refList></hh:head>"#
    );
    let section = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><hs:sec {ns}>{}{}{}</hs:sec>"#,
        p(1, "Chapter I"),
        p(
            0,
            "Emma Woodhouse, handsome, clever, and rich. She had lived nearly twenty-one years."
        ),
        p(0, "Her mother had died too long ago.")
    );
    let hpf = r#"<?xml version="1.0" encoding="UTF-8"?><opf:package xmlns:opf="http://www.idpf.org/2007/opf/"><opf:metadata><opf:title>Emma</opf:title><opf:meta name="creator" content="text">Jane Austen</opf:meta></opf:metadata><opf:manifest><opf:item id="header" href="Contents/header.xml" media-type="application/xml"/><opf:item id="section0" href="Contents/section0.xml" media-type="application/xml"/></opf:manifest></opf:package>"#;
    let container = r#"<?xml version="1.0" encoding="UTF-8"?><ocf:container xmlns:ocf="urn:oasis:names:tc:opendocument:xmlns:container"><ocf:rootfiles><ocf:rootfile full-path="Contents/content.hpf" media-type="application/hwpml-package+xml"/></ocf:rootfiles></ocf:container>"#;
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, body) in [
        ("mimetype", "application/hwp+zip"),
        ("META-INF/container.xml", container),
        ("Contents/content.hpf", hpf),
        ("Contents/header.xml", header.as_str()),
        ("Contents/section0.xml", section.as_str()),
    ] {
        w.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        w.write_all(body.as_bytes()).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-hwpx-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("emma.hwpx"), package()).unwrap();
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
fn the_text_level_and_the_corpus_reading() {
    let dir = fixture("text");
    assert_eq!(
        ok(&dir, &["/section::lemma", "text:emma.hwpx"]),
        "Chapter I"
    );
    assert_eq!(ok(&dir, &["//paragraph @| count", "text:emma.hwpx"]), "2");
    assert_eq!(
        ok(&dir, &["^ | %(::title; ::author)", "text:emma.hwpx"]),
        "%(title = \"Emma\"; author = \"Jane Austen\")"
    );
    // The title and the heading are sentences too: 1 + 1 + 2 + 1.
    assert_eq!(ok(&dir, &["//sentence @| count", "corpus:emma.hwpx"]), "5");
}

#[test]
fn lit_refuses_and_the_plain_mount_is_the_package() {
    let dir = fixture("doors");
    let (_, err, ok) = qua(&dir, &["//paragraph @| count", "lit:emma.hwpx"]);
    assert!(
        !ok && err.contains("a Hangul Word Processor document"),
        "{err}"
    );
    assert_eq!(
        ok_(&dir, &["/* | :name", "emma.hwpx"]),
        "mimetype\nMETA-INF\nContents"
    );
}

fn ok_(dir: &Path, args: &[&str]) -> String {
    ok(dir, args)
}
