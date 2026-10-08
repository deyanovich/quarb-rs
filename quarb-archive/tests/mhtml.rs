//! An MHTML web archive, hand-written: a multipart/related
//! envelope with an HTML part in quoted-printable declaring
//! windows-1251, a CSS part in base64, and an image part with no
//! extension in its location — members under their locations,
//! decoded under the mount's rule, the HTML readable as HTML
//! through the compose graft.

use quarb::{AstAdapter, QueryResult};
use quarb_archive::ArchiveAdapter;

const BOUNDARY: &str = "----MultipartBoundary--abc123----";

fn mhtml() -> Vec<u8> {
    // «Мёртвые души» in windows-1251, quoted-printable.
    let title_qp = "=CC=B8=F0=F2=E2=FB=E5 =E4=F3=F8=E8";
    let html = format!(
        "<!doctype html><html><head><meta charset=3D\"windows-1251\"><title>{title_qp}</tit=\r\nle></head><body><h1>{title_qp}</h1><p>=D2=EE=EC =EF=E5=F0=E2=FB=E9.</p></body></html>"
    );
    let css = quarb::base64("body { color: black; }\n".as_bytes());
    let png = quarb::base64(&[0x89, b'P', b'N', b'G', 0, 1, 2, 3]);
    let mut s = String::new();
    s.push_str("From: <Saved by Blink>\r\n");
    s.push_str("Snapshot-Content-Location: https://example.org/dead-souls/\r\n");
    s.push_str("Subject: Dead Souls\r\n");
    s.push_str("MIME-Version: 1.0\r\n");
    s.push_str(&format!(
        "Content-Type: multipart/related;\r\n\ttype=\"text/html\";\r\n\tboundary=\"{BOUNDARY}\"\r\n\r\n"
    ));
    s.push_str(&format!("--{BOUNDARY}\r\n"));
    s.push_str("Content-Type: text/html; charset=\"windows-1251\"\r\n");
    s.push_str("Content-Transfer-Encoding: quoted-printable\r\n");
    s.push_str("Content-Location: https://example.org/dead-souls/\r\n\r\n");
    s.push_str(&html);
    s.push_str(&format!("\r\n--{BOUNDARY}\r\n"));
    s.push_str("Content-Type: text/css\r\n");
    s.push_str("Content-Transfer-Encoding: base64\r\n");
    s.push_str("Content-Location: https://example.org/style.css\r\n\r\n");
    s.push_str(&css);
    s.push_str(&format!("\r\n--{BOUNDARY}\r\n"));
    s.push_str("Content-Type: image/png\r\n");
    s.push_str("Content-Transfer-Encoding: base64\r\n");
    s.push_str("Content-ID: <frame-1@mhtml.blink>\r\n\r\n");
    s.push_str(&png);
    s.push_str(&format!("\r\n--{BOUNDARY}--\r\n"));
    s.into_bytes()
}

fn values<A: AstAdapter>(a: &A, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| a.name(*n).unwrap_or_default()).collect(),
    }
}

#[test]
fn parts_are_members_under_their_locations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dead-souls.mhtml");
    std::fs::write(&path, mhtml()).unwrap();
    let a = ArchiveAdapter::open(&path).unwrap();
    assert_eq!(
        values(&a, "//*<file> | :::name"),
        ["index.html", "style.css", "frame-1@mhtml.blink.png"]
    );
    assert_eq!(
        values(&a, "/example.org/dead-souls/index.html::::encoding"),
        ["windows-1251"]
    );
    assert_eq!(
        values(&a, "/example.org/style.css::"),
        ["body { color: black; }\n"]
    );
    // The quoted-printable soft break is undone and the bytes
    // decoded under the part's charset.
    let html = values(&a, "/example.org/dead-souls/index.html::").remove(0);
    assert!(html.contains("<title>Мёртвые души</title>"), "{html}");
    assert!(html.contains("<p>Том первый.</p>"), "{html}");
}

#[test]
fn the_html_part_reads_as_html_through_the_graft() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dead-souls.mht");
    std::fs::write(&path, mhtml()).unwrap();
    let a = quarb_compose::ComposeAdapter::new(ArchiveAdapter::open(&path).unwrap());
    assert_eq!(
        values(&a, "/example.org/dead-souls/index.html//title::"),
        ["Мёртвые души"]
    );
    assert_eq!(
        values(&a, "/example.org/dead-souls/index.html//p::"),
        ["Том первый."]
    );
}

#[test]
fn a_file_that_is_no_envelope_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.mht");
    std::fs::write(&path, b"just some notes\n").unwrap();
    assert!(ArchiveAdapter::open(&path).is_err());
}
