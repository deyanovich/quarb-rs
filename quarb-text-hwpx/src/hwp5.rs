//! The binary container of the reading: HWP 5 (`.hwp`), a compound
//! file whose streams hold the same records the OWPML package writes
//! as XML, by Hancom's published description of the format (HWP 5.0
//! file format, revision 1.3). `FileHeader` names the version and
//! whether the streams are deflated; `DocInfo` holds the paragraph
//! shapes (a shape's heading type and level: outline, numbering,
//! bullet) and the numberings; each `BodyText/SectionN` is a run of
//! tagged records — a paragraph's header and its UTF-16LE text with
//! control characters, the controls it anchors (a table and its
//! cells, a footnote or endnote and its body, a hyperlink field, a
//! header or footer) as records below it. The result is the same
//! [`Para`] stream the XML reader lowers, so a text saved in either
//! container answers the same queries.
use std::collections::HashMap;
use std::io::{Cursor, Read, Seek};

use quarb_text::{Cell, NoteFamily};

use crate::{
    Apparatus, HeadingKind, HwpxError, Para, ParaProp, Props, TABLE_MARK, serialize_table,
};

/// Whether the bytes are an HWP 5 document: an OLE2 compound file
/// holding the `FileHeader` stream with the format's signature.
pub fn is_hwp5(bytes: &[u8]) -> bool {
    if !bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        return false;
    }
    let Ok(mut cfb) = cfb::CompoundFile::open(Cursor::new(bytes)) else {
        return false;
    };
    read_stream(&mut cfb, "/FileHeader").is_some_and(|h| h.starts_with(b"HWP Document File"))
}

fn read_stream<F: Read + Seek>(cfb: &mut cfb::CompoundFile<F>, name: &str) -> Option<Vec<u8>> {
    let mut s = cfb.open_stream(name).ok()?;
    let mut out = Vec::new();
    s.read_to_end(&mut out).ok()?;
    Some(out)
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *b.get(at)?,
        *b.get(at + 1)?,
        *b.get(at + 2)?,
        *b.get(at + 3)?,
    ]))
}

/// A tagged record: its tag, nesting level and payload.
struct Record {
    tag: u16,
    level: u16,
    data: Vec<u8>,
}

const TAG_BEGIN: u16 = 0x010;
const PARA_SHAPE: u16 = TAG_BEGIN + 9;
const NUMBERING: u16 = TAG_BEGIN + 7;
const PARA_HEADER: u16 = TAG_BEGIN + 50;
const PARA_TEXT: u16 = TAG_BEGIN + 51;
const CTRL_HEADER: u16 = TAG_BEGIN + 55;
const LIST_HEADER: u16 = TAG_BEGIN + 56;
const TABLE: u16 = TAG_BEGIN + 61;

fn records(stream: &[u8]) -> Vec<Record> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(h) = u32_at(stream, at) {
        at += 4;
        let tag = (h & 0x3FF) as u16;
        let level = ((h >> 10) & 0x3FF) as u16;
        let mut size = ((h >> 20) & 0xFFF) as usize;
        if size == 0xFFF {
            let Some(s) = u32_at(stream, at) else { break };
            size = s as usize;
            at += 4;
        }
        let end = (at + size).min(stream.len());
        out.push(Record {
            tag,
            level,
            data: stream[at..end].to_vec(),
        });
        at = end;
        if size == 0 && h == 0 {
            break;
        }
    }
    out
}

/// A stream as the format stores it: raw deflate when the header
/// says the document is compressed.
fn inflate(bytes: Vec<u8>, compressed: bool) -> Vec<u8> {
    if !compressed {
        return bytes;
    }
    let mut out = Vec::new();
    let mut d = flate2::read::DeflateDecoder::new(Cursor::new(&bytes));
    if d.read_to_end(&mut out).is_err() && out.is_empty() {
        // A stream the header calls compressed that is not: as is.
        return bytes;
    }
    out
}

/// The document's paragraph stream and its identity.
pub(crate) struct Hwp5 {
    pub paras: Vec<Para>,
    pub props: Props,
    pub notes: Vec<(String, NoteFamily, Vec<Para>)>,
    pub identity: Vec<(String, String)>,
}

pub(crate) fn read(bytes: &[u8]) -> Result<Hwp5, HwpxError> {
    let mut cfb = cfb::CompoundFile::open(Cursor::new(bytes))
        .map_err(|e| HwpxError::Malformed(format!("not a compound file: {e}")))?;
    let header = read_stream(&mut cfb, "/FileHeader")
        .ok_or_else(|| HwpxError::Malformed("no FileHeader stream".into()))?;
    if !header.starts_with(b"HWP Document File") {
        return Err(HwpxError::Malformed(
            "FileHeader carries no HWP signature".into(),
        ));
    }
    let flags = u32_at(&header, 36).unwrap_or(0);
    let compressed = flags & 1 != 0;
    if flags & 2 != 0 {
        return Err(HwpxError::Malformed(
            "the document is password-protected (its streams are encrypted)".into(),
        ));
    }
    let docinfo = read_stream(&mut cfb, "/DocInfo")
        .map(|b| inflate(b, compressed))
        .unwrap_or_default();
    let props = doc_props(&records(&docinfo));
    let mut sections: Vec<(u32, Vec<u8>)> = Vec::new();
    let names: Vec<String> = cfb
        .walk()
        .filter(|e| e.is_stream())
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect();
    for name in &names {
        if let Some(rest) = name.strip_prefix("/BodyText/Section")
            && let Ok(n) = rest.parse::<u32>()
            && let Some(b) = read_stream(&mut cfb, name)
        {
            sections.push((n, inflate(b, compressed)));
        }
    }
    sections.sort_by_key(|(n, _)| *n);
    let mut doc = Hwp5 {
        paras: Vec::new(),
        props,
        notes: Vec::new(),
        identity: summary(&mut cfb),
    };
    let mut note_count = 0usize;
    for (_, s) in &sections {
        let recs = records(s);
        let mut i = 0usize;
        while i < recs.len() {
            if recs[i].tag == PARA_HEADER {
                let (para, next) = read_para(&recs, i, &doc.props, &mut doc.notes, &mut note_count);
                doc.paras.extend(para);
                i = next;
            } else {
                i += 1;
            }
        }
    }
    Ok(doc)
}

/// `DocInfo`: the paragraph shapes (by index, as `paraPrIDRef`
/// names them) and the numberings' starts.
fn doc_props(recs: &[Record]) -> Props {
    let mut props = Props::default();
    let mut shape = 0u32;
    let mut numbering = 0u32;
    for r in recs {
        match r.tag {
            PARA_SHAPE => {
                let attr = u32_at(&r.data, 0).unwrap_or(0);
                let kind = match (attr >> 23) & 0x3 {
                    1 => HeadingKind::Outline,
                    2 => HeadingKind::Number,
                    3 => HeadingKind::Bullet,
                    _ => HeadingKind::None,
                };
                let level = ((attr >> 25) & 0x7) as u8;
                // The numbering / bullet the shape names.
                let id_ref = u16_at(&r.data, 34).unwrap_or(0) as u32;
                props.para.insert(
                    shape.to_string(),
                    ParaProp {
                        kind,
                        level,
                        id_ref,
                    },
                );
                shape += 1;
            }
            NUMBERING => {
                numbering += 1;
                props.numbering_start.insert(numbering, 1);
            }
            _ => {}
        }
    }
    props
}

/// The control a paragraph anchors, by its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ctrl {
    Table,
    Footnote,
    Endnote,
    HeaderFooter,
    Hyperlink,
    Bookmark,
    Other,
}

fn ctrl_kind(data: &[u8]) -> Ctrl {
    let Some(id) = u32_at(data, 0) else {
        return Ctrl::Other;
    };
    // The id is four characters, stored so that reading them from
    // the high byte down spells the name.
    let b = id.to_be_bytes();
    match &b {
        b"tbl " => Ctrl::Table,
        b"fn  " => Ctrl::Footnote,
        b"en  " => Ctrl::Endnote,
        b"head" | b"foot" => Ctrl::HeaderFooter,
        b"%hlk" => Ctrl::Hyperlink,
        b"bokm" => Ctrl::Bookmark,
        _ => Ctrl::Other,
    }
}

/// The paragraph at `recs[i]` (its header), its text and the
/// controls recorded below it; returns the paragraphs it yields (the
/// paragraph, then any table it anchors) and the index past them.
fn read_para(
    recs: &[Record],
    i: usize,
    props: &Props,
    notes: &mut Vec<(String, NoteFamily, Vec<Para>)>,
    note_count: &mut usize,
) -> (Vec<Para>, usize) {
    let head = &recs[i];
    let level = head.level;
    let shape = u16_at(&head.data, 8).unwrap_or(0);
    let prop = props.para.get(&shape.to_string()).copied();
    let mut para = Para {
        prop,
        text: String::new(),
        apparatus: Vec::new(),
    };
    let mut tables: Vec<Para> = Vec::new();
    // Field spans in the text, in order: (start, end) offsets.
    let mut fields: Vec<(usize, Option<usize>)> = Vec::new();
    let mut j = i + 1;
    // The paragraph's records sit one level below its header: the
    // text, the character shapes and line segments, and the
    // controls it anchors (each with its content below that).
    let mut field_no = 0usize;
    while j < recs.len() && recs[j].level > level {
        let r = &recs[j];
        if r.level == level + 1 && r.tag == PARA_TEXT {
            text_of(&r.data, &mut para.text, &mut fields);
            j += 1;
            continue;
        }
        if r.tag == CTRL_HEADER && r.level == level + 1 {
            let kind = ctrl_kind(&r.data);
            let end = block_end(recs, j);
            match kind {
                Ctrl::Table => {
                    let (headers, rows) = read_table(&recs[j + 1..end], props, notes, note_count);
                    if !rows.is_empty() {
                        tables.push(Para {
                            prop: None,
                            text: String::new(),
                            apparatus: vec![Apparatus::Ref(
                                TABLE_MARK.to_string(),
                                serialize_table(&headers, &rows),
                                false,
                            )],
                        });
                    }
                }
                Ctrl::Footnote | Ctrl::Endnote => {
                    let family = if kind == Ctrl::Footnote {
                        NoteFamily::Footnote
                    } else {
                        NoteFamily::Endnote
                    };
                    *note_count += 1;
                    let onym = format!("n{}", *note_count);
                    let body = paras_in(&recs[j + 1..end], props, notes, note_count);
                    let at = para.text.trim_end().len() as u32;
                    para.apparatus
                        .push(Apparatus::Note(onym.clone(), family, at));
                    notes.push((onym, family, body));
                }
                Ctrl::Hyperlink => {
                    // The field's command: a counted UTF-16 string
                    // after the attribute and the extra byte.
                    let target = hwp_string(&r.data, 9).unwrap_or_default();
                    let target = target.split(';').next().unwrap_or("").trim().to_string();
                    let span = fields.get(field_no).copied();
                    field_no += 1;
                    if !target.is_empty() {
                        let text = span
                            .and_then(|(s, e)| para.text.get(s..e.unwrap_or(para.text.len())))
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        let internal = target.starts_with('#');
                        para.apparatus.push(Apparatus::Ref(target, text, internal));
                    }
                }
                Ctrl::Bookmark => {
                    // The name rides a parameter set after the header;
                    // the first string in it is the bookmark's.
                    if let Some(name) = recs[j + 1..end]
                        .iter()
                        .find_map(|c| first_string(&c.data))
                        .filter(|n| !n.is_empty())
                    {
                        para.apparatus.push(Apparatus::Anchor(name));
                    }
                }
                Ctrl::HeaderFooter | Ctrl::Other => {
                    if kind == Ctrl::Other && is_field(&r.data) {
                        field_no += 1;
                    }
                }
            }
            j = end;
        } else {
            j += 1;
        }
    }
    let mut out = vec![para];
    out.extend(tables);
    (out, j)
}

/// The index past the block starting at `recs[i]`: every record
/// below its level.
fn block_end(recs: &[Record], i: usize) -> usize {
    let level = recs[i].level;
    let mut j = i + 1;
    while j < recs.len() && recs[j].level > level {
        j += 1;
    }
    j
}

/// A field control: its id opens with `%`.
fn is_field(data: &[u8]) -> bool {
    u32_at(data, 0).is_some_and(|id| id.to_be_bytes()[0] == b'%')
}

/// The paragraphs of a run of records (a note's body, a cell).
fn paras_in(
    recs: &[Record],
    props: &Props,
    notes: &mut Vec<(String, NoteFamily, Vec<Para>)>,
    note_count: &mut usize,
) -> Vec<Para> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < recs.len() {
        if recs[i].tag == PARA_HEADER {
            let (p, next) = read_para(recs, i, props, notes, note_count);
            out.extend(p);
            i = next;
        } else {
            i += 1;
        }
    }
    out
}

/// A table's cells: after the `TABLE` record, each cell is a
/// `LIST_HEADER` (its column, row and spans) with its paragraphs
/// below; a first row of cells all flagged as header cells is the
/// header (the attribute's header bit).
fn read_table(
    recs: &[Record],
    props: &Props,
    notes: &mut Vec<(String, NoteFamily, Vec<Para>)>,
    note_count: &mut usize,
) -> (Option<Vec<String>>, Vec<Vec<Cell>>) {
    let mut grid: Vec<Vec<(String, usize, bool)>> = Vec::new(); // rows of (text, colspan, header)
    let mut i = 0usize;
    let table_level = recs.first().map(|r| r.level).unwrap_or(0);
    let mut rows_n = 0usize;
    while i < recs.len() {
        let r = &recs[i];
        if r.tag == TABLE {
            rows_n = u16_at(&r.data, 4).unwrap_or(0) as usize;
            i += 1;
            continue;
        }
        if r.tag == LIST_HEADER && r.level == table_level {
            let attr = u32_at(&r.data, 2).unwrap_or(0);
            let col = u16_at(&r.data, 6).unwrap_or(0) as usize;
            let row = u16_at(&r.data, 8).unwrap_or(0) as usize;
            let colspan = u16_at(&r.data, 10).unwrap_or(1).max(1) as usize;
            let _ = col;
            let end = {
                let mut j = i + 1;
                while j < recs.len()
                    && !(recs[j].tag == LIST_HEADER && recs[j].level == table_level)
                {
                    j += 1;
                }
                j
            };
            let paras = paras_in(&recs[i + 1..end], props, notes, note_count);
            let text = paras
                .iter()
                .map(|p| p.text.trim())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            // Bit 7 of the list header's attribute: a header cell
            // (the OWPML `header="1"`).
            let header = attr & (1 << 7) != 0;
            while grid.len() <= row {
                grid.push(Vec::new());
            }
            grid[row].push((text, colspan, header));
            i = end;
            continue;
        }
        i += 1;
    }
    let _ = rows_n;
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let mut header_row: Option<Vec<String>> = None;
    for (ri, row) in grid.iter().enumerate() {
        let mut cells: Vec<Cell> = Vec::new();
        for (text, span, _) in row {
            cells.push(Cell::from(text.as_str()));
            for _ in 1..*span {
                cells.push(Cell::from(""));
            }
        }
        if ri == 0 && grid.len() > 1 && !row.is_empty() && row.iter().all(|(_, _, h)| *h) {
            header_row = Some(cells.into_iter().map(|c| c.text).collect());
        } else {
            rows.push(cells);
        }
    }
    (header_row, rows)
}

/// `PARA_TEXT`: UTF-16LE with control characters — the extended and
/// inline controls take eight units, the others one; a line break
/// is a space, a paragraph end nothing, a tab a tab; field starts
/// and ends mark the spans a hyperlink's text sits in.
fn text_of(data: &[u8], out: &mut String, fields: &mut Vec<(usize, Option<usize>)>) {
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let mut i = 0usize;
    while i < units.len() {
        let u = units[i];
        match u {
            0 => i += 1,
            3 => {
                fields.push((out.len(), None));
                i += 8;
            }
            4 => {
                if let Some(f) = fields.iter_mut().rev().find(|f| f.1.is_none()) {
                    f.1 = Some(out.len());
                }
                i += 8;
            }
            9 => {
                out.push('\t');
                i += 8;
            }
            10 => {
                out.push(' ');
                i += 1;
            }
            13 => i += 1,
            1 | 2 | 5..=8 | 11 | 12 | 14..=23 => i += 8,
            24..=31 => {
                match u {
                    30 => out.push('\u{a0}'),
                    31 => out.push(' '),
                    24 => out.push('-'),
                    _ => {}
                }
                i += 1;
            }
            _ => {
                let mut end = i;
                while end < units.len() && (units[end] >= 32 || units[end] == 9) && units[end] != 9
                {
                    end += 1;
                }
                if end == i {
                    end = i + 1;
                }
                out.push_str(&String::from_utf16_lossy(&units[i..end]));
                i = end;
            }
        }
    }
}

/// An HWP string at `at`: a unit count, then UTF-16LE.
fn hwp_string(data: &[u8], at: usize) -> Option<String> {
    let n = u16_at(data, at)? as usize;
    let bytes = data.get(at + 2..at + 2 + n * 2)?;
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Some(String::from_utf16_lossy(&units))
}

/// The first string in a parameter set (`CTRL_DATA`): after the
/// set's id and count, an item is its id, its type (1 is a string)
/// and the value; a bookmark's name is the set's one item. The
/// header's exact width varies by version, so the item is found by
/// its shape: a string type followed by a count that fits.
fn first_string(data: &[u8]) -> Option<String> {
    for at in (4..=12).step_by(2) {
        if u16_at(data, at + 2) == Some(1)
            && let Some(n) = u16_at(data, at + 4)
            && n > 0
            && at + 6 + n as usize * 2 <= data.len()
        {
            return hwp_string(data, at + 4);
        }
    }
    None
}

/// `\x05HwpSummaryInformation`: the standard property ids, strings
/// in UTF-16LE (the set's code page is Unicode) or the code page.
fn summary<F: Read + Seek>(cfb: &mut cfb::CompoundFile<F>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(ps) = read_stream(cfb, "/\u{5}HwpSummaryInformation") else {
        return out;
    };
    let Some(section) = u32_at(&ps, 44).map(|o| o as usize) else {
        return out;
    };
    let count = u32_at(&ps, section + 4).unwrap_or(0) as usize;
    for k in 0..count.min(64) {
        let (Some(id), Some(off)) = (
            u32_at(&ps, section + 8 + k * 8),
            u32_at(&ps, section + 12 + k * 8),
        ) else {
            break;
        };
        let at = section + off as usize;
        let key = match id {
            2 => "title",
            3 => "subject",
            4 => "creator",
            5 => "keyword",
            6 => "description",
            _ => continue,
        };
        let ty = u32_at(&ps, at).unwrap_or(0);
        let len = u32_at(&ps, at + 4).unwrap_or(0) as usize;
        let value = match ty {
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
            30 => {
                let bytes = ps.get(at + 8..(at + 8 + len).min(ps.len())).unwrap_or(&[]);
                let bytes = bytes.split(|b| *b == 0).next().unwrap_or(&[]);
                String::from_utf8_lossy(bytes).into_owned()
            }
            _ => continue,
        };
        let value = value.trim().to_string();
        if !value.is_empty() {
            out.push((key.to_string(), value));
        }
    }
    out
}

/// The streams a compound file holds — for tooling.
pub fn streams(bytes: &[u8]) -> Vec<String> {
    let Ok(cfb) = cfb::CompoundFile::open(Cursor::new(bytes)) else {
        return Vec::new();
    };
    cfb.walk()
        .filter(|e| e.is_stream())
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect()
}

#[allow(dead_code)]
fn _unused(_: HashMap<u8, u8>) {}

/// The file header's version and flags, and the first records of
/// `DocInfo` and the first section (tag, level, size) — for
/// checking the reader against a real file.
pub fn dump(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let Ok(mut cfb) = cfb::CompoundFile::open(Cursor::new(bytes)) else {
        return "not a compound file".into();
    };
    let Some(header) = read_stream(&mut cfb, "/FileHeader") else {
        return "no FileHeader".into();
    };
    let version = u32_at(&header, 32).unwrap_or(0);
    let flags = u32_at(&header, 36).unwrap_or(0);
    let _ = writeln!(out, "version {:08x} flags {:08x}", version, flags);
    let compressed = flags & 1 != 0;
    for name in ["/DocInfo", "/BodyText/Section0"] {
        let Some(b) = read_stream(&mut cfb, name) else {
            continue;
        };
        let b = inflate(b, compressed);
        let recs = records(&b);
        let _ = writeln!(out, "{name}: {} bytes, {} records", b.len(), recs.len());
        for r in recs.iter().take(80) {
            let _ = write!(
                out,
                "  tag {:#05x} lvl {} size {}",
                r.tag,
                r.level,
                r.data.len()
            );
            if r.tag == PARA_SHAPE {
                let attr = u32_at(&r.data, 0).unwrap_or(0);
                let _ = write!(
                    out,
                    "  attr {:08x} heading {} level {}",
                    attr,
                    (attr >> 23) & 3,
                    (attr >> 25) & 7
                );
            }
            if r.tag == CTRL_HEADER {
                let _ = write!(
                    out,
                    "  ctrl {:?}",
                    String::from_utf8_lossy(&u32_at(&r.data, 0).unwrap_or(0).to_be_bytes())
                );
            }
            if r.tag == PARA_TEXT {
                let mut t = String::new();
                let mut f = Vec::new();
                text_of(&r.data, &mut t, &mut f);
                let _ = write!(out, "  text {:?}", t.chars().take(40).collect::<String>());
            }
            if r.tag == PARA_HEADER {
                let _ = write!(out, "  shape {}", u16_at(&r.data, 8).unwrap_or(0));
            }
            if r.tag == TAG_BEGIN + 71 {
                let hex: Vec<String> = r.data.iter().take(48).map(|b| format!("{b:02x}")).collect();
                let _ = write!(out, "  data {}", hex.join(" "));
            }
            let _ = writeln!(out);
        }
    }
    out
}
