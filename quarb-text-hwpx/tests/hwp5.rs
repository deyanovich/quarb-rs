//! The binary container: a compound file written in-test after the
//! HWP 5 record layout (uncompressed), read into the same model as
//! the XML package.
use std::io::Write;

use quarb::{AstAdapter, QueryResult, Value};

fn rec(tag: u16, level: u16, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let size = data.len() as u32;
    let header: u32 =
        (tag as u32) | ((level as u32) << 10) | (if size >= 0xFFF { 0xFFF } else { size } << 20);
    out.extend_from_slice(&header.to_le_bytes());
    if size >= 0xFFF {
        out.extend_from_slice(&size.to_le_bytes());
    }
    out.extend_from_slice(data);
    out
}

/// A paragraph shape: attr1 with the heading type and level, then
/// the margins, the line spacing, the tab definition, the
/// numbering id, the border fill and offsets.
fn para_shape(kind: u32, level: u32, numbering: u16) -> Vec<u8> {
    let attr: u32 = (kind << 23) | (level << 25);
    let mut d = Vec::new();
    d.extend_from_slice(&attr.to_le_bytes());
    for _ in 0..6 {
        d.extend_from_slice(&0i32.to_le_bytes());
    }
    d.extend_from_slice(&0u16.to_le_bytes()); // tab def
    d.extend_from_slice(&numbering.to_le_bytes());
    d.extend_from_slice(&0u16.to_le_bytes()); // border fill
    for _ in 0..4 {
        d.extend_from_slice(&0i16.to_le_bytes());
    }
    d.extend_from_slice(&0u32.to_le_bytes());
    d.extend_from_slice(&0u32.to_le_bytes());
    d.extend_from_slice(&0u32.to_le_bytes());
    d
}

fn units(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
}

/// An extended control character: the code, six units of data, the
/// code again.
fn ctrl_char(code: u16, id: &[u8; 4]) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(&code.to_le_bytes());
    // The control id, as the format stores it (reversed).
    let rev: Vec<u8> = id.iter().rev().copied().collect();
    d.extend_from_slice(&rev);
    d.extend_from_slice(&[0u8; 8]);
    d.extend_from_slice(&code.to_le_bytes());
    d
}

fn para_header(shape: u16, nchars: u32) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(&nchars.to_le_bytes());
    d.extend_from_slice(&0u32.to_le_bytes());
    d.extend_from_slice(&shape.to_le_bytes());
    d.push(0);
    d.push(0);
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&0u16.to_le_bytes());
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&0u32.to_le_bytes());
    d
}

/// A plain paragraph at `level` with `shape`: header, text (with a
/// paragraph end), line segment.
fn para(level: u16, shape: u16, text: &str, extra: &[u8]) -> Vec<u8> {
    let mut t = units(text);
    t.extend_from_slice(extra);
    t.extend_from_slice(&13u16.to_le_bytes());
    let n = (t.len() / 2) as u32;
    let mut out = rec(0x42, level, &para_header(shape, n));
    out.extend(rec(0x43, level + 1, &t));
    out.extend(rec(0x45, level + 1, &[0u8; 36]));
    out
}

fn ctrl_header(id: &[u8; 4], extra: &[u8]) -> Vec<u8> {
    let mut d = Vec::new();
    let v = u32::from_be_bytes(*id);
    d.extend_from_slice(&v.to_le_bytes());
    d.extend_from_slice(extra);
    d
}

fn document() -> Vec<u8> {
    // DocInfo: four shapes — 0 plain, 1 outline level 0, 2 outline
    // level 1, 3 numbered level 0 — and one numbering.
    let mut docinfo = Vec::new();
    docinfo.extend(rec(0x10, 0, &[0u8; 26]));
    docinfo.extend(rec(0x11, 0, &[0u8; 72]));
    docinfo.extend(rec(0x17, 1, &[0u8; 20]));
    docinfo.extend(rec(0x19, 1, &para_shape(0, 0, 0)));
    docinfo.extend(rec(0x19, 1, &para_shape(1, 0, 0)));
    docinfo.extend(rec(0x19, 1, &para_shape(1, 1, 0)));
    docinfo.extend(rec(0x19, 1, &para_shape(2, 0, 1)));
    // Section 0.
    let mut sec = Vec::new();
    sec.extend(para(0, 1, "Volume I", &[]));
    sec.extend(para(0, 2, "Chapter I", &[]));
    sec.extend(para(
        0,
        0,
        "Emma Woodhouse, handsome, clever, and rich.",
        &[],
    ));
    // A paragraph anchoring a footnote: the extended control in the
    // text, the control's records below.
    let mut with_note = para(0, 0, "Highbury", &ctrl_char(17, b"fn  "));
    with_note.extend(rec(0x47, 1, &ctrl_header(b"fn  ", &[0u8; 16])));
    with_note.extend(rec(0x48, 2, &[1u8, 0, 0, 0, 0, 0]));
    with_note.extend(para(2, 0, "A large and populous village.", &[]));
    sec.extend(with_note);
    sec.extend(para(0, 3, "First", &[]));
    sec.extend(para(0, 3, "Second", &[]));
    // A paragraph anchoring a table of two rows.
    let mut with_table = para(0, 0, "", &ctrl_char(11, b"tbl "));
    with_table.extend(rec(0x47, 1, &ctrl_header(b"tbl ", &[0u8; 40])));
    let mut table = Vec::new();
    table.extend_from_slice(&0u32.to_le_bytes());
    table.extend_from_slice(&2u16.to_le_bytes());
    table.extend_from_slice(&2u16.to_le_bytes());
    table.extend_from_slice(&[0u8; 20]);
    with_table.extend(rec(0x4D, 2, &table));
    let cell = |col: u16, row: u16, header: bool| {
        let mut d = Vec::new();
        d.extend_from_slice(&1u16.to_le_bytes()); // paragraph count
        d.extend_from_slice(&(if header { 1u32 << 7 } else { 0 }).to_le_bytes());
        d.extend_from_slice(&col.to_le_bytes());
        d.extend_from_slice(&row.to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&[0u8; 20]);
        d
    };
    for (col, row, text, header) in [
        (0, 0, "title", true),
        (1, 0, "price", true),
        (0, 1, "Emma", false),
        (1, 1, "25", false),
    ] {
        with_table.extend(rec(0x48, 2, &cell(col, row, header)));
        with_table.extend(para(3, 0, text, &[]));
    }
    sec.extend(with_table);
    // A hyperlink field around "Read it".
    let mut link_text = units("See ");
    link_text.extend(ctrl_char(3, b"%hlk"));
    link_text.extend(units("Read it"));
    link_text.extend(ctrl_char(4, b"%hlk"));
    link_text.extend(units(" online."));
    let mut with_link = para(0, 0, "", &link_text);
    let mut field = Vec::new();
    field.extend_from_slice(&0u32.to_le_bytes());
    field.push(0);
    let target = "https://www.gutenberg.org/ebooks/158;1;0;0";
    field.extend_from_slice(&(target.encode_utf16().count() as u16).to_le_bytes());
    field.extend(units(target));
    with_link.extend(rec(0x47, 1, &ctrl_header(b"%hlk", &field)));
    sec.extend(with_link);
    // The compound file.
    let mut header = vec![0u8; 256];
    header[..17].copy_from_slice(b"HWP Document File");
    header[32..36].copy_from_slice(&0x05000500u32.to_le_bytes());
    header[36..40].copy_from_slice(&0u32.to_le_bytes());
    let cursor = std::io::Cursor::new(Vec::new());
    let mut cfb = cfb::CompoundFile::create(cursor).unwrap();
    cfb.create_stream("/FileHeader")
        .unwrap()
        .write_all(&header)
        .unwrap();
    cfb.create_stream("/DocInfo")
        .unwrap()
        .write_all(&docinfo)
        .unwrap();
    cfb.create_storage("/BodyText").unwrap();
    cfb.create_stream("/BodyText/Section0")
        .unwrap()
        .write_all(&sec)
        .unwrap();
    cfb.into_inner().into_inner()
}

fn values(q: &str, a: &impl AstAdapter) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| a.name(*n).unwrap_or_default()).collect(),
    }
}

#[test]
fn the_binary_container_reads_into_the_same_model() {
    let bytes = document();
    assert!(quarb_text_hwpx::is_hwp5(&bytes));
    let m = quarb_text_hwpx::parse_hwp(&bytes).unwrap();
    assert_eq!(values("/section::lemma", &m), ["Volume I"]);
    assert_eq!(values("/section/section::lemma", &m), ["Chapter I"]);
    assert_eq!(
        values("/section/section/paragraph::", &m),
        [
            "Emma Woodhouse, handsome, clever, and rich.",
            "Highbury",
            "See Read it online."
        ]
    );
    assert_eq!(
        values("//footnote<note>::", &m),
        ["A large and populous village."]
    );
    assert_eq!(
        values("/section/section/paragraph[2]/*<deixis> @| count", &m),
        ["1"]
    );
    // The first ordered list is the numbering; the table's rows are
    // an ordered list of their own (the koine table lowering).
    assert_eq!(
        values("//ordered-list[1]/ordered-item::", &m),
        ["First", "Second"]
    );
    assert_eq!(
        values("//*<tabula>//*<cell> | ::lemma", &m),
        ["title", "price"]
    );
    assert_eq!(values("//*<tabula>//*<cell> | ::body", &m), ["Emma", "25"]);
    assert_eq!(
        values("//ref | %(::target; t = ::)", &m),
        ["%(target = \"https://www.gutenberg.org/ebooks/158\"; t = \"Read it\")"]
    );
}
