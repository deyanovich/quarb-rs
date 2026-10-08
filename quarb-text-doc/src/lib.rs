//! The legacy Word text-level adapter: `.doc` (Word 97–2003, an
//! OLE2 compound file) bytes in, the reader's model out —
//! `text:thesis.doc` beside `text:report.docx`. The text-only
//! first cut: what the piece table declares as the document's
//! text, with the Word reader's vocabulary where the binary
//! format states it plainly.
//!
//! - The container is OLE2 (the `cfb` crate): the `WordDocument`
//!   stream holds the FIB and the text, the table stream (`1Table`
//!   or `0Table`, as the FIB's `fWhichTblStm` says) the CLX with
//!   the piece table. Each piece names its file position and
//!   whether its characters are 8-bit (`fCompressed`) or UTF-16LE;
//!   8-bit pieces decode in the code page of the FIB's language
//!   (`lid`: Russian → windows-1251, …), the default being
//!   windows-1252.
//! - Paragraphs are the runs between paragraph marks (0x0D); a
//!   cell or row mark (0x07) ends a paragraph too. Page and
//!   section breaks and the picture, object and field characters
//!   are not text; a field's instruction (between 0x13 and 0x14)
//!   is dropped and its result kept, as the Word reader keeps a
//!   field's cached text.
//! - The main document is the first `ccpText` characters of the
//!   text; the footnote text follows it (`ccpFtn`), split into
//!   bodies by `PlcffndTxt`; the k-th footnote mark (0x02) in the
//!   main text is the callout of the k-th body, at its offset.
//!   Headers, annotations, endnotes and text boxes stay out of
//!   the prose.
//! - `\x05SummaryInformation` declares the identity: title,
//!   subject, author, keywords, comments, under the HTML head's
//!   keys.
//! - Not in this cut: outline levels, lists and tables (they need
//!   the paragraph property pages and the stylesheet), tracked
//!   changes, bookmarks, hyperlinks. Documented as the format's
//!   first reading; the structure comes in a later one.

use std::io::{Cursor, Read, Seek};

use quarb_text::{Block, Container, HeadMeta, NoteFamily, TextModel};

#[derive(Debug, thiserror::Error)]
pub enum DocError {
    #[error("not an OLE2 compound file: {0}")]
    Container(std::io::Error),
    #[error("no WordDocument stream — not a Word document")]
    NoWordDocument,
    #[error("the FIB is not Word's (ident {0:#x})")]
    NotWord(u16),
    #[error("the table stream {0} is missing")]
    NoTable(&'static str),
    #[error("the piece table is malformed: {0}")]
    Clx(&'static str),
}

/// Whether the bytes are an OLE2 compound file — the dispatch
/// between a Word binary and a plain-text file named `.doc`.
pub fn is_ole2(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
}

/// Parse `.doc` bytes into the text-level model, with what the
/// summary information declares as the head.
pub fn parse(bytes: &[u8]) -> Result<TextModel, DocError> {
    let doc = Document::open(bytes)?;
    let mut model = TextModel::build(doc.blocks());
    model.set_head_meta(doc.head);
    Ok(model)
}

/// Lower the bytes into the block event stream — exposed for
/// tests and tooling.
pub fn blocks(bytes: &[u8]) -> Result<Vec<Block>, DocError> {
    Ok(Document::open(bytes)?.blocks())
}

/// The text as the piece table gives it, the main document's
/// length in characters, the footnote bodies' boundaries, and the
/// head.
struct Document {
    text: Vec<char>,
    ccp_text: usize,
    ccp_ftn: usize,
    /// Footnote body boundaries as CPs within the footnote text.
    ftn_bounds: Vec<u32>,
    head: HeadMeta,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn read_stream<F: Read + Seek>(cfb: &mut cfb::CompoundFile<F>, name: &str) -> Option<Vec<u8>> {
    let mut s = cfb.open_stream(name).ok()?;
    let mut v = Vec::new();
    s.read_to_end(&mut v).ok()?;
    Some(v)
}

impl Document {
    fn open(bytes: &[u8]) -> Result<Document, DocError> {
        let mut cfb = cfb::CompoundFile::open(Cursor::new(bytes)).map_err(DocError::Container)?;
        let Some(word) = read_stream(&mut cfb, "/WordDocument") else {
            return Err(DocError::NoWordDocument);
        };
        let ident = u16_at(&word, 0).unwrap_or(0);
        if ident != 0xA5EC {
            return Err(DocError::NotWord(ident));
        }
        let flags = u16_at(&word, 0x0A).unwrap_or(0);
        let lid = u16_at(&word, 0x06).unwrap_or(0x0409);
        let encoding = code_page_for_lid(lid);
        let which = if flags & 0x0200 != 0 {
            "/1Table"
        } else {
            "/0Table"
        };
        let table = read_stream(&mut cfb, which)
            .or_else(|| {
                read_stream(
                    &mut cfb,
                    if which == "/1Table" {
                        "/0Table"
                    } else {
                        "/1Table"
                    },
                )
            })
            .ok_or(DocError::NoTable(if flags & 0x0200 != 0 {
                "1Table"
            } else {
                "0Table"
            }))?;
        // FibRgLw97: ccpText at 0x4C, ccpFtn at 0x50.
        let ccp_text = u32_at(&word, 0x4C).unwrap_or(0) as usize;
        let ccp_ftn = u32_at(&word, 0x50).unwrap_or(0) as usize;
        // FibRgFcLcb97: fcClx/lcbClx at 0x1A2/0x1A6, fcPlcffndTxt/
        // lcbPlcffndTxt at 0xB2/0xB6.
        let fc_clx = u32_at(&word, 0x1A2).unwrap_or(0) as usize;
        let lcb_clx = u32_at(&word, 0x1A6).unwrap_or(0) as usize;
        let clx = table
            .get(fc_clx..fc_clx + lcb_clx)
            .ok_or(DocError::Clx("CLX out of range"))?;
        let text = piece_text(clx, &word, encoding)?;
        let fc_ftn = u32_at(&word, 0xB2).unwrap_or(0) as usize;
        let lcb_ftn = u32_at(&word, 0xB6).unwrap_or(0) as usize;
        let ftn_bounds = table
            .get(fc_ftn..fc_ftn + lcb_ftn)
            .map(|p| {
                p.chunks_exact(4)
                    .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect()
            })
            .unwrap_or_default();
        let head = summary(&mut cfb, encoding);
        Ok(Document {
            text,
            ccp_text,
            ccp_ftn,
            ftn_bounds,
            head,
        })
    }

    fn blocks(&self) -> Vec<Block> {
        let main_end = self.ccp_text.min(self.text.len());
        let main = &self.text[..main_end];
        let mut out = Vec::new();
        let mut callouts = 0usize;
        for para in paragraphs(main) {
            let (text, marks) = clean(para);
            if text.is_empty() && marks.is_empty() {
                continue;
            }
            if !text.is_empty() {
                out.push(Block::Paragraph { text: text.clone() });
            }
            for at in marks {
                callouts += 1;
                out.push(Block::NoteRef {
                    onym: callouts.to_string(),
                    family: Some(NoteFamily::Footnote),
                    margin: false,
                    at: Some(at.min(text.len() as u32)),
                });
            }
        }
        // The footnote text: one body per PlcffndTxt span (the
        // last boundary closes the region; a trailing empty span
        // is the format's sentinel).
        let ftn_start = main_end;
        let ftn_end = (ftn_start + self.ccp_ftn).min(self.text.len());
        let ftn = &self.text[ftn_start..ftn_end];
        let bounds: Vec<usize> = if self.ftn_bounds.len() >= 2 {
            self.ftn_bounds
                .iter()
                .map(|&b| (b as usize).min(ftn.len()))
                .collect()
        } else if !ftn.is_empty() {
            vec![0, ftn.len()]
        } else {
            Vec::new()
        };
        let mut n = 0usize;
        for w in bounds.windows(2) {
            let (lo, hi) = (w[0], w[1]);
            if lo >= hi {
                continue;
            }
            n += 1;
            out.push(Block::Open {
                kind: Container::Note {
                    onym: n.to_string(),
                    family: NoteFamily::Footnote,
                    margin: false,
                },
                lemma: None,
            });
            for para in paragraphs(&ftn[lo..hi]) {
                let (text, _) = clean(para);
                if !text.is_empty() {
                    out.push(Block::Paragraph { text });
                }
            }
            out.push(Block::Close { hypograph: None });
        }
        out
    }
}

/// The runs between paragraph (0x0D) and cell (0x07) marks.
fn paragraphs(text: &[char]) -> impl Iterator<Item = &[char]> {
    text.split(|c| *c == '\r' || *c == '\u{7}')
}

/// A paragraph's prose: whitespace collapsed, the control
/// characters applied (a line break a space, a field's
/// instruction dropped, a footnote mark recorded at its offset).
fn clean(para: &[char]) -> (String, Vec<u32>) {
    let mut out = String::new();
    let mut marks = Vec::new();
    let mut in_instr = false;
    for &c in para {
        match c {
            '\u{13}' => in_instr = true,
            '\u{14}' => in_instr = false,
            '\u{15}' => in_instr = false,
            _ if in_instr => {}
            '\u{2}' => marks.push(out.trim_end().len() as u32),
            '\u{1}' | '\u{8}' | '\u{c}' | '\u{1f}' | '\u{3}' | '\u{4}' | '\u{5}' | '\u{6}' => {}
            '\u{b}' | '\t' => push_space(&mut out),
            '\u{1e}' => out.push('-'),
            c if c.is_whitespace() => push_space(&mut out),
            c => out.push(c),
        }
    }
    (out.trim_end().to_string(), marks)
}

fn push_space(out: &mut String) {
    if !out.is_empty() && !out.ends_with(' ') {
        out.push(' ');
    }
}

/// The CLX: `Prc` entries (0x01, a grpprl) then the `Pcdt` (0x02,
/// its length, the `PlcPcd`): n+1 CPs and n `Pcd`s, each naming
/// its file position with bit 30 for 8-bit (compressed) text.
fn piece_text(
    clx: &[u8],
    word: &[u8],
    encoding: &'static encoding_rs::Encoding,
) -> Result<Vec<char>, DocError> {
    let mut i = 0;
    while clx.get(i) == Some(&0x01) {
        let cb = u16_at(clx, i + 1).ok_or(DocError::Clx("Prc truncated"))? as usize;
        i += 3 + cb;
    }
    if clx.get(i) != Some(&0x02) {
        return Err(DocError::Clx("no Pcdt"));
    }
    let lcb = u32_at(clx, i + 1).ok_or(DocError::Clx("Pcdt truncated"))? as usize;
    let plc = clx
        .get(i + 5..i + 5 + lcb)
        .ok_or(DocError::Clx("PlcPcd out of range"))?;
    if lcb < 4 || !(lcb - 4).is_multiple_of(12) {
        return Err(DocError::Clx("PlcPcd size"));
    }
    let n = (lcb - 4) / 12;
    let cps: Vec<u32> = (0..=n).map(|k| u32_at(plc, k * 4).unwrap_or(0)).collect();
    let mut text = Vec::new();
    for k in 0..n {
        let pcd = (n + 1) * 4 + k * 8;
        let fc = u32_at(plc, pcd + 2).ok_or(DocError::Clx("Pcd truncated"))?;
        let compressed = fc & 0x4000_0000 != 0;
        let fc = (fc & 0x3FFF_FFFF) as usize;
        let chars = (cps[k + 1].saturating_sub(cps[k])) as usize;
        if compressed {
            let start = fc / 2;
            let bytes = word
                .get(start..(start + chars).min(word.len()))
                .unwrap_or(&[]);
            // The 8-bit pieces are the code page's, with Word's
            // own substitutions in the C1 range.
            let mut s = String::with_capacity(bytes.len());
            for &b in bytes {
                match b {
                    0x82 => s.push('‚'),
                    0x83 => s.push('ƒ'),
                    0x84 => s.push('„'),
                    0x85 => s.push('…'),
                    0x86 => s.push('†'),
                    0x87 => s.push('‡'),
                    0x88 => s.push('ˆ'),
                    0x89 => s.push('‰'),
                    0x8A => s.push('Š'),
                    0x8B => s.push('‹'),
                    0x8C => s.push('Œ'),
                    0x91 => s.push('‘'),
                    0x92 => s.push('’'),
                    0x93 => s.push('“'),
                    0x94 => s.push('”'),
                    0x95 => s.push('•'),
                    0x96 => s.push('–'),
                    0x97 => s.push('—'),
                    0x98 => s.push('˜'),
                    0x99 => s.push('™'),
                    0x9A => s.push('š'),
                    0x9B => s.push('›'),
                    0x9C => s.push('œ'),
                    0x9F => s.push('Ÿ'),
                    _ => s.push_str(&encoding.decode(&[b]).0),
                }
            }
            text.extend(s.chars());
        } else {
            let bytes = word
                .get(fc..(fc + chars * 2).min(word.len()))
                .unwrap_or(&[]);
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            text.extend(char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')));
        }
    }
    Ok(text)
}

/// The Windows code page of a language id's ANSI pieces.
fn code_page_for_lid(lid: u16) -> &'static encoding_rs::Encoding {
    let primary = lid & 0x3FF;
    let label: &str = match primary {
        0x19 | 0x02 | 0x22 | 0x23 | 0x1A | 0x2F | 0x43 | 0x40 | 0x3F | 0x50 | 0x28 | 0x44
        | 0x46 => "windows-1251", // Russian, Bulgarian, Ukrainian, Belarusian, Serbian…, Macedonian, Kazakh…
        0x05 | 0x0E | 0x15 | 0x1B | 0x24 | 0x18 | 0x1C | 0x1D if primary != 0x1D => "windows-1250", // Czech, Hungarian, Polish, Slovak, Slovenian, Romanian, Albanian
        0x08 => "windows-1253",
        0x1F => "windows-1254",
        0x0D => "windows-1255",
        0x01 | 0x20 | 0x29 => "windows-1256",
        0x25..=0x27 => "windows-1257",
        0x2A => "windows-1258",
        0x1E => "windows-874",
        0x11 => "shift_jis",
        0x04 if lid == 0x0804 || lid == 0x1004 => "gbk",
        0x04 => "big5",
        0x12 => "euc-kr",
        _ => "windows-1252",
    };
    encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::WINDOWS_1252)
}

/// The `\x05SummaryInformation` property set: the standard
/// property ids under the head's keys.
fn summary<F: Read + Seek>(
    cfb: &mut cfb::CompoundFile<F>,
    encoding: &'static encoding_rs::Encoding,
) -> HeadMeta {
    let mut h = HeadMeta::default();
    let Some(ps) = read_stream(cfb, "/\u{5}SummaryInformation") else {
        return h;
    };
    // The header: 28 bytes, then the first section's offset.
    let Some(section) = u32_at(&ps, 44).map(|o| o as usize) else {
        return h;
    };
    let count = u32_at(&ps, section + 4).unwrap_or(0) as usize;
    // The code page property (1) names the strings' encoding.
    let mut enc = encoding;
    let props: Vec<(u32, usize)> = (0..count.min(64))
        .filter_map(|k| {
            let id = u32_at(&ps, section + 8 + k * 8)?;
            let off = u32_at(&ps, section + 12 + k * 8)? as usize;
            Some((id, section + off))
        })
        .collect();
    for &(id, at) in &props {
        if id == 1 && u32_at(&ps, at) == Some(2) {
            let cp = u16_at(&ps, at + 4).unwrap_or(1252);
            enc = encoding_rs::Encoding::for_label(
                match cp {
                    65001 => "utf-8".to_string(),
                    1200 => "utf-16le".to_string(),
                    n => format!("windows-{n}"),
                }
                .as_bytes(),
            )
            .unwrap_or(encoding);
        }
    }
    for &(id, at) in &props {
        let key = match id {
            2 => "title",
            3 => "subject",
            4 => "author",
            5 => "keywords",
            6 => "description",
            _ => continue,
        };
        // VT_LPSTR (30): a length-prefixed, NUL-terminated string in
        // the set's code page; VT_LPWSTR (31) in UTF-16LE.
        let ty = u32_at(&ps, at).unwrap_or(0);
        let len = u32_at(&ps, at + 4).unwrap_or(0) as usize;
        let value = match ty {
            30 => {
                let bytes = ps.get(at + 8..(at + 8 + len).min(ps.len())).unwrap_or(&[]);
                let bytes = bytes.split(|b| *b == 0).next().unwrap_or(&[]);
                enc.decode(bytes).0.into_owned()
            }
            31 => {
                let bytes = ps
                    .get(at + 8..(at + 8 + len * 2).min(ps.len()))
                    .unwrap_or(&[]);
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .take_while(|u| *u != 0)
                    .collect();
                String::from_utf16_lossy(&units)
            }
            _ => continue,
        };
        let value = quarb_text::normalize_ws(&value);
        if value.is_empty() {
            continue;
        }
        h.declare(key, &value);
        match key {
            "title" => h.title = Some(value),
            "author" => h.author = Some(value),
            "description" => h.description = Some(value),
            "subject" => h.category = Some(value),
            "keywords" => {
                for t in value.split([',', ';']) {
                    h.tag(t);
                }
            }
            _ => {}
        }
    }
    h
}
