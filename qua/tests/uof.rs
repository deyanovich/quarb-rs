//! A UOF text at the doors: `text:` reads it in the Word reading's
//! vocabulary, `corpus:` its prose, `lit:` refuses naming it, and
//! the plain mount keeps its XML.
use std::path::{Path, PathBuf};

const DOC: &str = r#"<?xml version="1.0" encoding="UTF-8"?><uof:UOF xmlns:uof="http://schemas.uof.org/cn/2003/uof" xmlns:字="http://schemas.uof.org/cn/2003/uof-wordproc" uof:language="en" uof:version="1.0" uof:mimetype="vnd.uof.text"><uof:元数据><uof:标题>Emma</uof:标题><uof:创建者>Jane Austen</uof:创建者></uof:元数据><uof:文字处理><字:主体><字:段落><字:段落属性><字:大纲级别>1</字:大纲级别></字:段落属性><字:句><字:文本串>Chapter I</字:文本串></字:句></字:段落><字:段落><字:句><字:文本串>Emma Woodhouse, handsome, clever, and rich. She had lived nearly twenty-one years.</字:文本串></字:句></字:段落></字:主体></uof:文字处理></uof:UOF>"#;

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-uof-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("emma.uot"), DOC).unwrap();
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
fn the_doors() {
    let dir = fixture("doors");
    assert_eq!(ok(&dir, &["/section::lemma", "text:emma.uot"]), "Chapter I");
    assert_eq!(
        ok(&dir, &["^ | %(::title; ::author)", "text:emma.uot"]),
        "%(title = \"Emma\"; author = \"Jane Austen\")"
    );
    // The title, the heading and two sentences.
    assert_eq!(ok(&dir, &["//sentence @| count", "corpus:emma.uot"]), "4");
    let (_, err, ok_) = qua(&dir, &["//paragraph @| count", "lit:emma.uot"]);
    assert!(
        !ok_ && err.contains("a Uniform Office Format text"),
        "{err}"
    );
    // The plain mount keeps the XML: the Chinese names as written.
    assert_eq!(
        ok(
            &dir,
            &["//*[:name = \"字:文本串\"]:: @| first", "emma.uot"]
        ),
        "Chapter I"
    );
}
