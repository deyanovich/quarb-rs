//! The source boundary: `?encoding=`, a byte-order mark, the
//! declaration a format carries, strict UTF-8 — and the output
//! boundary, `@| encoding(LABEL)`.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-encoding-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("folder")).unwrap();
    let w = |p: &str, bytes: &[u8]| std::fs::write(dir.join(p), bytes).unwrap();
    // windows-1251, declared in the prologue.
    let xml = concat!(
        "<?xml version=\"1.0\" encoding=\"windows-1251\"?>\n",
        "<поэма><песнь n=\"1\">Гнев, богиня, воспой</песнь>",
        "<песнь n=\"2\">Сон</песнь></поэма>"
    );
    w("iliada.xml", &cp1251(xml));
    // KOI8-R, declared by a meta charset.
    let html = "<html><head><meta charset=\"koi8-r\"><title>Илиада</title></head>\
                <body><p>Гнев</p></body></html>";
    w("page.html", &koi8r(html));
    // KOI8-R, nothing declared: needs the option.
    w("koi.txt", &koi8r("Гнев, богиня, воспой.\n\nСон.\n"));
    // UTF-8 with a byte-order mark.
    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend_from_slice(r#"{"title": "Илиада"}"#.as_bytes());
    w("bom.json", &bom);
    // UTF-8 with one malformed byte.
    w("bad.json", b"{\"title\": \"\xd0\x98\xd0\xbb\xff\xd0\xb0\"}");
    // A folder of the same, for grafting.
    w("folder/iliada.xml", &cp1251(xml));
    w("folder/page.html", &koi8r(html));
    w("folder/k.txt", &koi8r("Ахилл"));
    // One archive of the folder.
    let tar = std::process::Command::new("tar")
        .args(["czf", "ar.tar.gz", "-C", "folder", "."])
        .current_dir(&dir)
        .status()
        .unwrap();
    assert!(tar.success());
    dir
}

/// A few Cyrillic letters by hand: enough for the fixtures, no
/// dependency on the crate under test.
fn cp1251(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| match c as u32 {
            0x0410..=0x044F => (c as u32 - 0x0410 + 0xC0) as u8,
            0x0401 => 0xA8,
            0x0451 => 0xB8,
            n if n < 0x80 => n as u8,
            _ => panic!("not in the fixture alphabet: {c:?}"),
        })
        .collect()
}

fn koi8r(s: &str) -> Vec<u8> {
    const UPPER: &str = "ЮАБЦДЕФГХИЙКЛМНОПЯРСТУЖВЬЫЗШЭЩЧЪ";
    const LOWER: &str = "юабцдефгхийклмнопярстужвьызшэщчъ";
    s.chars()
        .map(|c| {
            if (c as u32) < 0x80 {
                return c as u8;
            }
            if let Some(i) = UPPER.chars().position(|u| u == c) {
                return 0xE0 + i as u8;
            }
            if let Some(i) = LOWER.chars().position(|l| l == c) {
                return 0xC0 + i as u8;
            }
            panic!("not in the fixture alphabet: {c:?}")
        })
        .collect()
}

fn qua(dir: &Path, args: &[&str]) -> (Vec<u8>, String, bool) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        out.stdout,
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
        out.status.success(),
    )
}

fn text(dir: &Path, args: &[&str]) -> String {
    let (out, err, ok) = qua(dir, args);
    assert!(ok, "{args:?}: {err}");
    String::from_utf8(out).unwrap().trim().to_string()
}

#[test]
fn declarations_and_the_option_decide() {
    let dir = fixture("decide");
    // the XML prologue
    assert_eq!(
        text(&dir, &["//песнь | ::", "iliada.xml"]),
        "Гнев, богиня, воспой\nСон"
    );
    assert_eq!(text(&dir, &["^::::encoding", "iliada.xml"]), "windows-1251");
    assert_eq!(text(&dir, &["^::::replaced", "iliada.xml"]), "0");
    // the HTML meta charset
    assert_eq!(text(&dir, &["//title | ::", "page.html"]), "Илиада");
    assert_eq!(text(&dir, &["^::::encoding", "page.html"]), "KOI8-R");
    // the byte-order mark
    assert_eq!(text(&dir, &["/title::", "bom.json"]), "Илиада");
    assert_eq!(text(&dir, &["^::::encoding", "bom.json"]), "UTF-8");
    // the option, on a plain file and on a reader prefix
    assert_eq!(
        text(
            &dir,
            &["//paragraph | :: @| count", "text:koi.txt?encoding=koi8-r"]
        ),
        "2"
    );
    assert_eq!(
        text(&dir, &["^::::encoding", "koi.txt?encoding=KOI8-R"]),
        "KOI8-R"
    );
    // the option outranks the declaration
    assert_eq!(
        text(&dir, &["^::::encoding", "iliada.xml?encoding=koi8-r"]),
        "KOI8-R"
    );
}

#[test]
fn nothing_said_means_strict_utf8() {
    let dir = fixture("strict");
    let (_, err, ok) = qua(&dir, &["/ | ::", "text:koi.txt"]);
    assert!(!ok);
    assert!(err.contains("not valid UTF-8 at byte 0"), "{err}");
    assert!(err.contains("?encoding=LABEL"), "{err}");
    let (_, err, ok) = qua(&dir, &["/title::", "bad.json"]);
    assert!(!ok);
    assert!(err.contains("not valid UTF-8 at byte 15"), "{err}");
    // under a decided encoding the malformed byte is replaced and counted
    assert_eq!(
        text(&dir, &["/title::", "bad.json?encoding=utf-8"]),
        "Ил\u{FFFD}а"
    );
    assert_eq!(
        text(&dir, &["^::::replaced", "bad.json?encoding=utf-8"]),
        "1"
    );
    // an unknown label is refused at open
    let (_, err, ok) = qua(&dir, &["/title::", "bad.json?encoding=cp-nonesuch"]);
    assert!(!ok);
    assert!(err.contains("unknown encoding label"), "{err}");
}

#[test]
fn mounts_folders_and_archives_carry_the_rule() {
    let dir = fixture("layers");
    // each mount its own record
    assert_eq!(
        text(&dir, &["/a::::encoding", "a=iliada.xml", "b=page.html"]),
        "windows-1251"
    );
    assert_eq!(
        text(&dir, &["/b::::encoding", "a=iliada.xml", "b=page.html"]),
        "KOI8-R"
    );
    // a grafted folder: the leaf answers for the document it grafted
    assert_eq!(
        text(&dir, &["/iliada.xml::::encoding", "--graft", "folder"]),
        "windows-1251"
    );
    assert_eq!(
        text(&dir, &["/page.html//title | ::", "--graft", "folder"]),
        "Илиада"
    );
    // a plain leaf reads under the folder's option
    assert_eq!(
        text(&dir, &["/k.txt | ::", "--graft", "folder?encoding=koi8-r"]),
        "Ахилл"
    );
    // archive members decode under the archive's rule
    assert_eq!(
        text(&dir, &["//iliada.xml::::encoding", "ar.tar.gz"]),
        "windows-1251"
    );
    assert_eq!(
        text(&dir, &["//iliada.xml//песнь | :: @| count", "ar.tar.gz"]),
        "2"
    );
    assert_eq!(
        text(&dir, &["//k.txt | ::", "ar.tar.gz?encoding=koi8-r"]),
        "Ахилл"
    );
    assert_eq!(
        text(&dir, &["//k.txt::::encoding", "ar.tar.gz?encoding=koi8-r"]),
        "KOI8-R"
    );
}

#[test]
fn the_output_boundary() {
    let dir = fixture("output");
    let (out, err, ok) = qua(
        &dir,
        &["//песнь | :: @| encoding(windows-1251)", "iliada.xml"],
    );
    assert!(ok, "{err}");
    assert_eq!(out, cp1251("Гнев, богиня, воспой\nСон\n"));
    // a serialization stage before it, bytes as the encoding has them
    let (out, _, ok) = qua(
        &dir,
        &[
            "//песнь | %(n = ::n; t = ::) @| csv @| encoding(koi8-r)",
            "iliada.xml",
        ],
    );
    assert!(ok);
    assert_eq!(out, koi8r("n,t\n1,\"Гнев, богиня, воспой\"\n2,Сон\n"));
    // a character the encoding cannot represent refuses the whole output
    let (out, err, ok) = qua(
        &dir,
        &["//песнь | :: @| encoding(iso-8859-1)", "iliada.xml"],
    );
    assert!(!ok);
    assert!(out.is_empty());
    assert!(err.contains("cannot represent"), "{err}");
    // after @| json a character the encoding lacks becomes the
    // format's escape, and the file reads back
    let (out, err, ok) = qua(
        &dir,
        &[
            "//песнь | %(n = ::n; t = \"é\") @| jsonl @| encoding(koi8-r)",
            "iliada.xml",
        ],
    );
    assert!(ok, "{err}");
    assert!(
        out.windows(6).any(|w| w == b"\\u00e9"),
        "no escape in the bytes"
    );
    assert!(
        out.starts_with(b"{\"n\": \"1\", \"t\": \"\\u00e9\"}"),
        "{}",
        String::from_utf8_lossy(&out)
    );
    // after plain text the same character refuses
    let (_, err, ok) = qua(
        &dir,
        &[
            "//песнь | %(t = \"é\") @| csv @| encoding(koi8-r)",
            "iliada.xml",
        ],
    );
    assert!(!ok);
    assert!(err.contains("cannot represent"), "{err}");
    // anywhere but last is refused
    let (_, err, ok) = qua(
        &dir,
        &["//песнь | :: @| encoding(koi8-r) @| count", "iliada.xml"],
    );
    assert!(!ok);
    assert!(err.contains("stands last"), "{err}");
    // an unknown label is refused before anything runs
    let (_, err, ok) = qua(&dir, &["//песнь | :: @| encoding(nonesuch)", "iliada.xml"]);
    assert!(!ok);
    assert!(err.contains("unknown label"), "{err}");
}
