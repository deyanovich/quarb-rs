//! FictionBook 2 at the text level: by its extension, by its
//! namespace, inside a `.fb2.zip`, and under a declared encoding.

use std::path::{Path, PathBuf};

const FB2: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink">
<description><title-info>
<author><first-name>Александр</first-name><last-name>Пушкин</last-name></author>
<book-title>Евгений Онегин</book-title>
</title-info></description>
<body>
<section><title><p>Глава первая</p></title>
<epigraph><p>И жить торопится, и чувствовать спешит.</p><text-author>Кн. Вяземский</text-author></epigraph>
<poem><stanza>
<v>«Мой дядя самых честных правил,</v>
<v>Когда не в шутку занемог,</v>
</stanza><stanza>
<v>Так думал молодой повеса,</v>
<v>Наследник всех своих родных.</v>
</stanza></poem>
<p>Так <emphasis>начинается</emphasis> роман.</p>
</section>
</body>
</FictionBook>"##;

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-fb2-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("shelf")).unwrap();
    std::fs::write(dir.join("onegin.fb2"), FB2).unwrap();
    std::fs::write(dir.join("onegin.xml"), FB2).unwrap();
    std::fs::write(dir.join("shelf/onegin.fb2"), FB2).unwrap();
    // windows-1251, declared in the prologue (the fixture alphabet
    // is Cyrillic and ASCII only).
    let cp1251: Vec<u8> = FB2
        .replace("encoding=\"UTF-8\"", "encoding=\"windows-1251\"")
        .chars()
        .map(|c| match c as u32 {
            0x0410..=0x044F => (c as u32 - 0x0410 + 0xC0) as u8,
            0xAB => 0xAB,
            n if n < 0x80 => n as u8,
            _ => panic!("not in the fixture alphabet: {c:?}"),
        })
        .collect();
    std::fs::write(dir.join("onegin1251.fb2"), cp1251).unwrap();
    let zip = std::process::Command::new("zip")
        .args(["-q", "onegin.fb2.zip", "onegin.fb2"])
        .current_dir(&dir)
        .status()
        .unwrap();
    assert!(zip.success());
    dir
}

fn qua(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn every_door_reads_fictionbook() {
    let dir = fixture("doors");
    // by extension, at the text level
    assert_eq!(qua(&dir, &["//stichos @| count", "text:onegin.fb2"]), "4");
    assert_eq!(
        qua(&dir, &["//section::title", "text:onegin.fb2"]),
        "Глава первая"
    );
    assert_eq!(
        qua(&dir, &["//blockquote::hypograph", "text:onegin.fb2"]),
        "Кн. Вяземский"
    );
    // the koine route and the corpus reading
    assert_eq!(qua(&dir, &["//strophe @| count", "koine:onegin.fb2"]), "2");
    assert_eq!(
        qua(&dir, &["//sentence @| count", "corpus:onegin.fb2"]),
        "8"
    );
    // the literary reading keeps the emphasis and the identity
    assert_eq!(qua(&dir, &["//emphasis::", "lit:onegin.fb2"]), "начинается");
    assert_eq!(
        qua(&dir, &["//author::", "lit:onegin.fb2"]),
        "Александр Пушкин"
    );
    // an .xml declaring the FictionBook namespace reads the same
    assert_eq!(qua(&dir, &["//stichos @| count", "text:onegin.xml"]), "4");
    assert_eq!(qua(&dir, &["//stichos @| count", "koine:onegin.xml"]), "4");
    // the plain mount keeps the file's own tree
    assert_eq!(
        qua(&dir, &["//book-title::", "onegin.fb2"]),
        "Евгений Онегин"
    );
    // a folder of books, and the book in its zip
    assert_eq!(
        qua(&dir, &["/onegin.fb2//stichos @| count", "text:shelf"]),
        "4"
    );
    assert_eq!(
        qua(
            &dir,
            &["/onegin.fb2//stichos @| count", "text:onegin.fb2.zip"]
        ),
        "4"
    );
    // a windows-1251 book reads by its prologue's declaration
    assert_eq!(
        qua(&dir, &["//stichos[1]::", "text:onegin1251.fb2"]),
        "«Мой дядя самых честных правил,"
    );
    assert_eq!(
        qua(&dir, &["^::::encoding", "text:onegin1251.fb2"]),
        "windows-1251"
    );
}
