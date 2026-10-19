//! A Word 97 package built in-test with the `cfb` crate: a FIB
//! naming its table stream and its CLX, a piece table with one
//! 8-bit piece in the FIB language's code page and one UTF-16
//! piece, a footnote mark with its body in the footnote text,
//! and a SummaryInformation property set — asserted through real
//! queries. No Word writer runs here; the offsets are MS-DOC's.

use std::io::Write;

use quarb::{AstAdapter, QueryResult};
use quarb_text_doc::{blocks, is_ole2, parse};

fn values(model: &quarb_text::TextModel, q: &str) -> Vec<String> {
    match quarb::run(q, model).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns
            .iter()
            .map(|n| model.name(*n).unwrap_or_default())
            .collect(),
    }
}

/// Build a `.doc`: `main` as an 8-bit (windows-1251) piece and
/// `tail` as a UTF-16 piece, then `ftn` as the footnote text; the
/// FIB's lid is Russian.
fn doc(main: &str, tail: &str, ftn: &str, summary: Option<(&str, &str)>) -> Vec<u8> {
    let enc = encoding_rs::WINDOWS_1251;
    let main8 = enc.encode(main).0.into_owned();
    let tail16: Vec<u8> = tail.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let ftn16: Vec<u8> = ftn.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let main_cps = main.chars().count() as u32;
    let tail_cps = tail.chars().count() as u32;
    let ftn_cps = ftn.chars().count() as u32;
    // WordDocument: a 1024-byte FIB area, then the pieces.
    let mut word = vec![0u8; 1024];
    word[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
    word[2..4].copy_from_slice(&0x00C1u16.to_le_bytes()); // nFib
    word[6..8].copy_from_slice(&0x0419u16.to_le_bytes()); // lid Russian
    word[0x0A..0x0C].copy_from_slice(&0x0200u16.to_le_bytes()); // fWhichTblStm = 1Table
    let fc_main = word.len();
    word.extend_from_slice(&main8);
    if !word.len().is_multiple_of(2) {
        word.push(0);
    }
    let fc_tail = word.len();
    word.extend_from_slice(&tail16);
    let fc_ftn = word.len();
    word.extend_from_slice(&ftn16);
    word[0x4C..0x50].copy_from_slice(&(main_cps + tail_cps).to_le_bytes()); // ccpText
    word[0x50..0x54].copy_from_slice(&ftn_cps.to_le_bytes()); // ccpFtn
    // 1Table: PlcffndTxt then the CLX.
    let mut table = Vec::new();
    let fc_plcffnd = table.len();
    if ftn_cps > 0 {
        for cp in [0u32, ftn_cps, ftn_cps + 1] {
            table.extend_from_slice(&cp.to_le_bytes());
        }
    }
    let lcb_plcffnd = table.len() - fc_plcffnd;
    let fc_clx = table.len();
    table.push(0x02);
    let n = 3u32;
    let lcb = 4 * (n + 1) + 8 * n;
    table.extend_from_slice(&lcb.to_le_bytes());
    for cp in [
        0,
        main_cps,
        main_cps + tail_cps,
        main_cps + tail_cps + ftn_cps,
    ] {
        table.extend_from_slice(&cp.to_le_bytes());
    }
    let pcd = |fc: usize, compressed: bool| {
        let mut p = vec![0u8; 8];
        let fc = if compressed {
            (fc as u32 * 2) | 0x4000_0000
        } else {
            fc as u32
        };
        p[2..6].copy_from_slice(&fc.to_le_bytes());
        p
    };
    table.extend(pcd(fc_main, true));
    table.extend(pcd(fc_tail, false));
    table.extend(pcd(fc_ftn, false));
    let lcb_clx = table.len() - fc_clx;
    word[0x1A2..0x1A6].copy_from_slice(&(fc_clx as u32).to_le_bytes());
    word[0x1A6..0x1AA].copy_from_slice(&(lcb_clx as u32).to_le_bytes());
    word[0xB2..0xB6].copy_from_slice(&(fc_plcffnd as u32).to_le_bytes());
    word[0xB6..0xBA].copy_from_slice(&(lcb_plcffnd as u32).to_le_bytes());
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut cfb = cfb::CompoundFile::create(&mut cursor).unwrap();
        cfb.create_stream("/WordDocument")
            .unwrap()
            .write_all(&word)
            .unwrap();
        cfb.create_stream("/1Table")
            .unwrap()
            .write_all(&table)
            .unwrap();
        if let Some((title, author)) = summary {
            cfb.create_stream("/\u{5}SummaryInformation")
                .unwrap()
                .write_all(&summary_stream(title, author))
                .unwrap();
        }
        cfb.flush().unwrap();
    }
    cursor.into_inner()
}

/// A property set with the code page (1251), the title (VT_LPSTR)
/// and the author (VT_LPWSTR).
fn summary_stream(title: &str, author: &str) -> Vec<u8> {
    let mut ps = vec![0u8; 48];
    ps[0..2].copy_from_slice(&0xFFFEu16.to_le_bytes());
    ps[24..28].copy_from_slice(&1u32.to_le_bytes()); // one section
    ps[44..48].copy_from_slice(&48u32.to_le_bytes()); // section offset
    let title8 = encoding_rs::WINDOWS_1251.encode(title).0.into_owned();
    let author16: Vec<u8> = author
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    // Section: size, count, then (id, offset) pairs, then values.
    let mut sec = Vec::new();
    let count = 3u32;
    let header = 8 + 8 * count as usize;
    let mut vals = Vec::new();
    let mut offs = Vec::new();
    // 1: code page
    offs.push((1u32, header + vals.len()));
    vals.extend_from_slice(&2u32.to_le_bytes());
    vals.extend_from_slice(&1251u16.to_le_bytes());
    vals.extend_from_slice(&0u16.to_le_bytes());
    // 2: title, VT_LPSTR
    offs.push((2, header + vals.len()));
    vals.extend_from_slice(&30u32.to_le_bytes());
    vals.extend_from_slice(&((title8.len() + 1) as u32).to_le_bytes());
    vals.extend_from_slice(&title8);
    vals.push(0);
    while !vals.len().is_multiple_of(4) {
        vals.push(0);
    }
    // 4: author, VT_LPWSTR
    offs.push((4, header + vals.len()));
    vals.extend_from_slice(&31u32.to_le_bytes());
    vals.extend_from_slice(&((author.encode_utf16().count() + 1) as u32).to_le_bytes());
    vals.extend_from_slice(&author16);
    vals.extend_from_slice(&[0, 0]);
    sec.extend_from_slice(&((header + vals.len()) as u32).to_le_bytes());
    sec.extend_from_slice(&count.to_le_bytes());
    for (id, off) in offs {
        sec.extend_from_slice(&id.to_le_bytes());
        sec.extend_from_slice(&(off as u32).to_le_bytes());
    }
    sec.extend(vals);
    ps.extend(sec);
    ps
}

#[test]
fn pieces_paragraphs_and_a_footnote() {
    let bytes = doc(
        "Мёртвые души.\rПоэма\u{2} в трёх томах.\r",
        "Chapter one.\u{13} HYPERLINK x \u{14}The field's result\u{15} and\u{b}a line break.\r",
        "Так назвал её автор.\r",
        Some(("Мёртвые души", "Николай Гоголь")),
    );
    assert!(is_ole2(&bytes));
    let m = parse(&bytes).unwrap();
    assert_eq!(
        values(&m, "//paragraph::"),
        [
            "Мёртвые души.",
            "Поэма в трёх томах.",
            "Chapter one.The field's result and a line break.",
            "Так назвал её автор."
        ]
    );
    assert_eq!(values(&m, "//footnote<deixis> @| count"), ["1"]);
    assert_eq!(values(&m, "//footnote<deixis>:::at"), ["10"]);
    assert_eq!(
        values(&m, "//footnote<deixis>->footnote::"),
        ["Так назвал её автор."]
    );
    assert_eq!(values(&m, "^::title"), ["Мёртвые души"]);
    assert_eq!(values(&m, "^::author"), ["Николай Гоголь"]);
    // Three paragraphs, the callout, the body (open, one paragraph, close).
    assert_eq!(blocks(&bytes).unwrap().len(), 7);
}

#[test]
fn not_word_is_refused() {
    assert!(!is_ole2(b"plain text in a .doc"));
    assert!(parse(b"plain text in a .doc").is_err());
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut cfb = cfb::CompoundFile::create(&mut cursor).unwrap();
        cfb.create_stream("/Workbook")
            .unwrap()
            .write_all(b"x")
            .unwrap();
        cfb.flush().unwrap();
    }
    let e = match parse(&cursor.into_inner()) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a workbook read as Word"),
    };
    assert!(e.contains("WordDocument"), "{e}");
}
