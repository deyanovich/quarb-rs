//! An OFD package at the doors: `text:` reads the pages' lines under
//! the outline; `corpus:`, `koine:` and `lit:` refuse naming it; the
//! plain mount opens the package as a zip of XML.
use std::io::Write;
use std::path::{Path, PathBuf};

const NS: &str = r#"xmlns:ofd="http://www.ofdspec.org/2016""#;

fn package() -> Vec<u8> {
    let entry = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><ofd:OFD {NS} Version="1.0" DocType="OFD"><ofd:DocBody><ofd:DocInfo><ofd:Title>Emma</ofd:Title><ofd:Author>Jane Austen</ofd:Author></ofd:DocInfo><ofd:DocRoot>Doc_0/Document.xml</ofd:DocRoot></ofd:DocBody></ofd:OFD>"#
    );
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><ofd:Document {NS}><ofd:CommonData><ofd:PageArea><ofd:PhysicalBox>0 0 210 297</ofd:PhysicalBox></ofd:PageArea><ofd:MaxUnitID>9</ofd:MaxUnitID></ofd:CommonData><ofd:Pages><ofd:Page ID="1" BaseLoc="Pages/Page_0/Content.xml"/></ofd:Pages><ofd:Outlines><ofd:OutlineElem Title="Volume I"><ofd:Actions><ofd:Action Event="CLICK"><ofd:Goto><ofd:Dest Type="XYZ" PageID="1" Top="20"/></ofd:Goto></ofd:Action></ofd:Actions></ofd:OutlineElem></ofd:Outlines><ofd:Attachments>Attachs/Attachments.xml</ofd:Attachments></ofd:Document>"#
    );
    let page = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><ofd:Page {NS}><ofd:Content><ofd:Layer ID="2"><ofd:TextObject ID="3" Boundary="30 10 60 5" Font="1" Size="3.5"><ofd:TextCode X="0" Y="4">Emma</ofd:TextCode></ofd:TextObject><ofd:TextObject ID="4" Boundary="30 20 60 5" Font="1" Size="3.5"><ofd:TextCode X="0" Y="4">CHAPTER I</ofd:TextCode></ofd:TextObject><ofd:TextObject ID="5" Boundary="30 30 90 5" Font="1" Size="3.5"><ofd:TextCode X="0" Y="4">Emma Woodhouse, handsome, clever, and rich,</ofd:TextCode></ofd:TextObject></ofd:Layer></ofd:Content></ofd:Page>"#
    );
    let attachments = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><ofd:Attachments {NS}><ofd:Attachment ID="8" Name="notes" Format="xml"><ofd:FileLoc>notes.xml</ofd:FileLoc></ofd:Attachment></ofd:Attachments>"#
    );
    let notes = r#"<?xml version="1.0" encoding="UTF-8"?><notes><note>Highbury</note></notes>"#;
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, body) in [
        ("OFD.xml", entry.as_str()),
        ("Doc_0/Document.xml", document.as_str()),
        ("Doc_0/Pages/Page_0/Content.xml", page.as_str()),
        ("Doc_0/Attachs/Attachments.xml", attachments.as_str()),
        ("Doc_0/Attachs/notes.xml", notes),
    ] {
        w.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        w.write_all(body.as_bytes()).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-ofd-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("emma.ofd"), package()).unwrap();
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
fn the_text_level_reads_lines_under_the_outline() {
    let dir = fixture("text");
    assert_eq!(ok(&dir, &["/section::lemma", "text:emma.ofd"]), "Volume I");
    assert_eq!(ok(&dir, &["/line::", "text:emma.ofd"]), "Emma");
    assert_eq!(
        ok(&dir, &["/section/line::", "text:emma.ofd"]),
        "CHAPTER I\nEmma Woodhouse, handsome, clever, and rich,"
    );
    assert_eq!(
        ok(
            &dir,
            &["//line[2] | %(:::page; :::x; :::y)", "text:emma.ofd"]
        ),
        "%(page = 1; x = 30_mm; y = 20_mm)"
    );
    assert_eq!(
        ok(&dir, &["^ | %(::title; ::author)", "text:emma.ofd"]),
        "%(title = \"Emma\"; author = \"Jane Austen\")"
    );
}

#[test]
fn the_other_doors_refuse_naming_the_reading() {
    let dir = fixture("doors");
    let (_, err, ok) = qua(&dir, &["//paragraph @| count", "corpus:emma.ofd"]);
    assert!(
        !ok && err.contains("text:emma.ofd reads its lines"),
        "{err}"
    );
    let (_, err, ok) = qua(&dir, &["//paragraph @| count", "lit:emma.ofd"]);
    assert!(!ok && err.contains("text:emma.ofd reads"), "{err}");
    let (_, err, ok) = qua(&dir, &["//paragraph @| count", "koine:emma.ofd"]);
    assert!(!ok && err.contains("text:emma.ofd"), "{err}");
}

#[test]
fn the_plain_mount_is_the_package() {
    let dir = fixture("mount");
    // The zip's members, the XML grafted: the attachment reads as
    // XML under the document, the signatures would too.
    assert_eq!(ok(&dir, &["/* | :name", "emma.ofd"]), "OFD.xml\nDoc_0");
    assert_eq!(
        ok(&dir, &["/Doc_0/Attachs/notes.xml//note::", "emma.ofd"]),
        "Highbury"
    );
    assert_eq!(
        ok(
            &dir,
            &["/OFD.xml//*[:name = \"ofd:DocRoot\"]::", "emma.ofd"]
        ),
        "Doc_0/Document.xml"
    );
}
