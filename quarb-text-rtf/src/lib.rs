//! The Rich Text Format text-level adapter: `.rtf` bytes in, the
//! reader's model out — `text:decision.rtf` beside
//! `text:report.docx`. The vocabulary is the Word reader's; the
//! declarations are RTF's own control words.
//!
//! Everything derives from *declared* structure, never heuristics:
//!
//! - A paragraph is a heading iff it carries an outline level —
//!   directly (`\outlinelevelN`) or through its style
//!   (`\sN` resolved in the stylesheet along `\sbasedonN`). Style
//!   *names* are never matched for headings.
//! - Lists come from the declared list reference (`\lsN` with
//!   `\ilvlN`, resolved through the list override table to the
//!   list table's levels, `\levelnfc23` the bullet) or the older
//!   paragraph numbering group (`\pnlvlblt` a bullet, the rest
//!   numbered); nesting follows the level.
//! - Block quotes are the one style-name-keyed mapping: the same
//!   closed, cited set as the Word reader's (`Quote`, `Intense
//!   Quote`, `Block Text`, `Quotations`), looked up in the
//!   stylesheet by name and resolved along `\sbasedon`.
//! - Tables lower through the shared [`Block::Table`] rule:
//!   `\trowd` … `\cell` … `\row`, the header the declared
//!   `\trhdr` rows, positional otherwise none.
//! - Tracked changes read as the accepted view: `\revised` text
//!   is text, `\deleted` text is dropped.
//! - Notes are first class: a `{\footnote …}` group is a callout
//!   at its byte offset in the paragraph (`\ftnalt` makes it an
//!   endnote), its paragraphs the body at the document end.
//! - `{\field{\*\fldinst HYPERLINK …}{\fldrslt …}}` is a `ref`
//!   (`\l` names a bookmark, internal) with the result as its
//!   text; `{\*\bkmkstart name}` names the block when opened
//!   before its first run, else it is a point anchor; `{\xe …}`
//!   is an index mark; every other field keeps its result text.
//! - Emphasis is the declared `\i` / `\b` of a run (a character
//!   style's through the stylesheet), code a run in a font the
//!   font table declares `\fmodern` (fixed pitch).
//! - The text's bytes: `\'xx` is a byte of the declared code page
//!   (`\ansicpgN`, the `\mac` / `\pc` / `\pca` charsets), `\uN`
//!   a Unicode scalar whose substitute (`\ucN` of them) is
//!   skipped. [`declared_encoding`] reports the code page as the
//!   source's declaration, so `:::encoding` answers it.
//! - `{\info …}` declares the identity: `\title`, `\author`,
//!   `\subject`, `\keywords`, `\doccomm`, `\creatim`, `\revtim`.
//! - Not carried: headers and footers, pictures and objects,
//!   shapes, fields' instructions other than the two read.

use std::collections::HashMap;

use quarb_text::{Block, Cell, Container, HeadMeta, NoteFamily, TextModel};

#[derive(Debug, thiserror::Error)]
pub enum RtfError {
    #[error("not an RTF document (no `{{\\rtf` header)")]
    NotRtf,
}

/// Parse `.rtf` bytes into the text-level model, with what the
/// info group declares as the head.
pub fn parse(bytes: &[u8]) -> Result<TextModel, RtfError> {
    let (blocks, head) = lower(bytes)?;
    let mut model = TextModel::build(blocks);
    model.set_head_meta(head);
    Ok(model)
}

/// Lower the bytes into the block event stream — exposed for
/// tests and tooling.
pub fn blocks(bytes: &[u8]) -> Result<Vec<Block>, RtfError> {
    Ok(lower(bytes)?.0)
}

/// The code page the header declares, as the Encoding Standard's
/// label (`\ansicpg1251` → `windows-1251`; `\mac` → `macintosh`;
/// `\pc` and `\pca` the DOS pages, which the Standard does not
/// name and so read as `windows-1252`); `None` when nothing is
/// declared. The source boundary's declaration reporter for
/// `.rtf`.
pub fn declared_encoding(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(512)];
    let head = String::from_utf8_lossy(head);
    if let Some(i) = head.find("\\ansicpg") {
        let digits: String = head[i + 8..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(n) = digits.parse::<u32>() {
            return Some(code_page_label(n));
        }
    }
    if head.contains("\\mac") {
        return Some("macintosh".into());
    }
    if head.contains("\\ansi") || head.contains("\\pc") {
        return Some("windows-1252".into());
    }
    None
}

fn code_page_label(n: u32) -> String {
    match n {
        10000 => "macintosh".into(),
        932 => "shift_jis".into(),
        936 => "gbk".into(),
        949 => "euc-kr".into(),
        950 => "big5".into(),
        1200 | 1201 => "utf-16".into(),
        65001 => "utf-8".into(),
        437 | 850 | 852 | 866 => "windows-1252".into(),
        n if (1250..=1258).contains(&n) => format!("windows-{n}"),
        28591..=28606 => format!("iso-8859-{}", n - 28590),
        20866 => "koi8-r".into(),
        21866 => "koi8-u".into(),
        _ => "windows-1252".into(),
    }
}

// ---------------------------------------------------------------
// tokens
// ---------------------------------------------------------------

enum Tok {
    Open,
    Close,
    /// A control word with its optional numeric parameter.
    Word(String, Option<i32>),
    /// A control symbol (`\~`, `\-`, `\*`, …).
    Symbol(char),
    /// A `\'xx` byte of the document's code page.
    Byte(u8),
    /// Plain text.
    Text(String),
}

fn tokenize(bytes: &[u8]) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut text = String::new();
    let flush = |text: &mut String, out: &mut Vec<Tok>| {
        if !text.is_empty() {
            out.push(Tok::Text(std::mem::take(text)));
        }
    };
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'{' => {
                flush(&mut text, &mut out);
                out.push(Tok::Open);
                i += 1;
            }
            b'}' => {
                flush(&mut text, &mut out);
                out.push(Tok::Close);
                i += 1;
            }
            b'\\' => {
                flush(&mut text, &mut out);
                i += 1;
                let Some(&n) = bytes.get(i) else { break };
                if n == b'\'' {
                    let hex = bytes.get(i + 1..i + 3).unwrap_or(&[]);
                    let v = std::str::from_utf8(hex)
                        .ok()
                        .and_then(|h| u8::from_str_radix(h, 16).ok());
                    i += 3;
                    if let Some(v) = v {
                        out.push(Tok::Byte(v));
                    }
                } else if n.is_ascii_alphabetic() {
                    let start = i;
                    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                    let word = String::from_utf8_lossy(&bytes[start..i]).into_owned();
                    let pstart = i;
                    if i < bytes.len() && (bytes[i] == b'-' || bytes[i].is_ascii_digit()) {
                        i += 1;
                        while i < bytes.len() && bytes[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                    let param = std::str::from_utf8(&bytes[pstart..i])
                        .ok()
                        .and_then(|p| p.parse::<i32>().ok());
                    // One space after a control word is its
                    // delimiter, not text.
                    if bytes.get(i) == Some(&b' ') {
                        i += 1;
                    }
                    out.push(Tok::Word(word, param));
                } else {
                    out.push(Tok::Symbol(n as char));
                    i += 1;
                }
            }
            b'\r' | b'\n' => i += 1,
            _ => {
                // The bytes outside escapes are the code page's
                // too; ASCII here, anything else decoded later.
                if c < 0x80 {
                    text.push(c as char);
                } else {
                    flush(&mut text, &mut out);
                    out.push(Tok::Byte(c));
                }
                i += 1;
            }
        }
    }
    flush(&mut text, &mut out);
    out
}

// ---------------------------------------------------------------
// lowering
// ---------------------------------------------------------------

/// The closed quote vocabulary, by stylesheet name.
const QUOTE_NAMES: &[&str] = &["Quote", "Intense Quote", "Block Text", "Quotations"];

#[derive(Default, Clone)]
struct Style {
    outline: Option<u8>,
    based_on: Option<i32>,
    quote: bool,
    italic: bool,
    bold: bool,
}

/// What a run carries: the formatting in force, scoped to its
/// group.
#[derive(Clone, Default)]
struct Scope {
    italic: bool,
    bold: bool,
    /// The run's font is a fixed-pitch one.
    code: bool,
    deleted: bool,
    /// The number of substitute characters after a `\u`.
    uc: usize,
    /// The group is a destination whose text is not prose.
    skip: bool,
    /// Inside `\fldinst`: the instruction, not prose.
    instr: bool,
    /// Inside `\fldrslt`.
    result: bool,
    /// Inside a note group.
    note: bool,
}

/// The paragraph being read.
#[derive(Default)]
struct Para {
    text: String,
    outline: Option<u8>,
    style: Option<i32>,
    /// `\lsN` and `\ilvlN`.
    list: Option<(i32, u8)>,
    /// The old paragraph-numbering group's kind: bullet or not.
    pn: Option<bool>,
    /// A header row (`\trhdr`) is being read.
    in_table: bool,
    label: Option<String>,
    apparatus: Vec<Apparatus>,
}

enum Apparatus {
    Note(String, NoteFamily, u32),
    Mark(String),
    Ref(String, String, bool),
    Point(String),
    Span(&'static str, u32, u32),
}

impl Para {
    fn push(&mut self, s: &str) {
        for c in s.chars() {
            if c.is_whitespace() {
                if !self.text.is_empty() && !self.text.ends_with(' ') {
                    self.text.push(' ');
                }
            } else {
                self.text.push(c);
            }
        }
    }
    fn at(&self) -> u32 {
        self.text.trim_end().len() as u32
    }
}

struct Frame {
    list: i32,
    item_open: bool,
}

struct Lower {
    toks: Vec<Tok>,
    pos: usize,
    encoding: &'static encoding_rs::Encoding,
    styles: HashMap<i32, Style>,
    style_names: HashMap<i32, String>,
    /// `\lsN` → `\listidN`.
    overrides: HashMap<i32, i32>,
    /// `\listidN` → per level, ordered.
    lists: HashMap<i32, Vec<bool>>,
    /// Font number → fixed pitch.
    fixed_fonts: HashMap<i32, bool>,
    out: Vec<Block>,
    notes: Vec<(String, NoteFamily, Vec<Block>)>,
    note_counter: usize,
    frames: Vec<Frame>,
    quote_open: bool,
    head: HeadMeta,
    /// The table being read: its rows (header flag, cells) and
    /// the open row and cell.
    rows: Vec<(bool, Vec<Cell>)>,
    row: Vec<Cell>,
    row_header: bool,
    cell: String,
    /// A `\fldinst` group's instruction, handed up to its field
    /// group when the inner group closes.
    pending_instr: Option<String>,
}

fn lower(bytes: &[u8]) -> Result<(Vec<Block>, HeadMeta), RtfError> {
    if !bytes.starts_with(b"{\\rtf") {
        return Err(RtfError::NotRtf);
    }
    let encoding = declared_encoding(bytes)
        .and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
        .unwrap_or(encoding_rs::WINDOWS_1252);
    let mut l = Lower {
        toks: tokenize(bytes),
        pos: 0,
        encoding,
        styles: HashMap::new(),
        style_names: HashMap::new(),
        overrides: HashMap::new(),
        lists: HashMap::new(),
        fixed_fonts: HashMap::new(),
        out: Vec::new(),
        notes: Vec::new(),
        note_counter: 0,
        frames: Vec::new(),
        quote_open: false,
        head: HeadMeta::default(),
        rows: Vec::new(),
        row: Vec::new(),
        row_header: false,
        cell: String::new(),
        pending_instr: None,
    };
    // The outermost group holds the document.
    if matches!(l.toks.first(), Some(Tok::Open)) {
        l.pos = 1;
    }
    let mut para = Para::default();
    let scope = Scope {
        uc: 1,
        ..Default::default()
    };
    l.group(scope, &mut para, true);
    l.end_para(&mut para);
    l.close_lists(0);
    l.close_quote();
    l.flush_table();
    let mut out = std::mem::take(&mut l.out);
    for (onym, family, body) in std::mem::take(&mut l.notes) {
        out.push(Block::Open {
            kind: Container::Note {
                onym,
                family,
                margin: false,
            },
            lemma: None,
        });
        out.extend(body);
        out.push(Block::Close { hypograph: None });
    }
    Ok((out, l.head))
}

impl Lower {
    /// Read the tokens of one group (the opener consumed) to its
    /// closer, under `scope`, feeding `para`. `top` is the
    /// document group itself.
    fn group(&mut self, mut scope: Scope, para: &mut Para, top: bool) {
        // A group may open as a destination: its first control
        // word says which; `\*` marks an ignorable one.
        let mut first = true;
        let mut ignorable = false;
        // Pending Unicode substitutes to skip.
        let mut skip_subst = 0usize;
        // A field group's parts.
        let mut field: Option<(String, u32)> = None;
        let mut field_instr = String::new();
        // An xe group's term start.
        let mut xe_start: Option<u32> = None;
        // A note group's body, read as its own paragraphs.
        let mut note_body: Option<(NoteFamily, Vec<Block>, Para)> = None;
        while self.pos < self.toks.len() {
            let tok = std::mem::replace(&mut self.toks[self.pos], Tok::Close);
            self.pos += 1;
            match tok {
                Tok::Open => {
                    first = false;
                    if let Some((_, _, p)) = note_body.as_mut() {
                        // The note's own groups.
                        let saved = std::mem::take(&mut self.frames);
                        let out = std::mem::take(&mut self.out);
                        let mut inner = scope.clone();
                        inner.note = true;
                        self.group(inner, p, false);
                        let produced = std::mem::replace(&mut self.out, out);
                        self.frames = saved;
                        if let Some((_, blocks, _)) = note_body.as_mut() {
                            blocks.extend(produced);
                        }
                        continue;
                    }
                    let inner = scope.clone();
                    self.group(inner, para, false);
                }
                Tok::Close => {
                    if let Some((family, mut blocks, mut p)) = note_body.take() {
                        let text = p.text.trim().to_string();
                        if !text.is_empty() {
                            blocks.push(Block::Paragraph { text });
                        }
                        let mut sub_out = std::mem::take(&mut self.out);
                        std::mem::swap(&mut self.out, &mut sub_out);
                        let apparatus = std::mem::take(&mut p.apparatus);
                        let p2 = Para {
                            text: p.text.clone(),
                            apparatus,
                            ..Default::default()
                        };
                        let out = std::mem::take(&mut self.out);
                        self.emit_apparatus(&p2);
                        let extra = std::mem::replace(&mut self.out, out);
                        blocks.extend(extra);
                        self.note_counter += 1;
                        let onym = format!("n{}", self.note_counter);
                        // The callout stands where the group opened.
                        let at = para.at();
                        para.apparatus
                            .push(Apparatus::Note(onym.clone(), family, at));
                        self.notes.push((onym, family, blocks));
                    }
                    if let Some((instr, lo)) = field.take() {
                        self.finish_field(&instr, lo, para);
                    }
                    if let Some(lo) = xe_start.take() {
                        let term = para
                            .text
                            .get(lo as usize..)
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        para.text.truncate(lo as usize);
                        if !term.is_empty() {
                            para.apparatus.push(Apparatus::Mark(term));
                        }
                    }
                    if scope.instr && !field_instr.trim().is_empty() {
                        // Hand the instruction up to the field
                        // group (it may sit a group deeper than the
                        // `\fldinst` word).
                        self.pending_instr = Some(std::mem::take(&mut field_instr));
                    }
                    return;
                }
                Tok::Symbol(c) => {
                    // `\*` marks the destination the next word
                    // names as ignorable; the word is still the
                    // group's first.
                    if c == '*' {
                        ignorable = true;
                        continue;
                    }
                    first = false;
                    match c {
                        '~' => self.text(&scope, para, &mut note_body, " "),
                        '-' => {}
                        '_' => self.text(&scope, para, &mut note_body, "-"),
                        '\\' | '{' | '}' => self.text(&scope, para, &mut note_body, &c.to_string()),
                        _ => {}
                    }
                }
                Tok::Byte(b) => {
                    if skip_subst > 0 {
                        skip_subst -= 1;
                        continue;
                    }
                    first = false;
                    // A run of code-page bytes decodes as one (a
                    // double-byte page spells one character as two
                    // escapes).
                    let mut run = vec![b];
                    while let Some(Tok::Byte(nb)) = self.toks.get(self.pos) {
                        run.push(*nb);
                        self.pos += 1;
                    }
                    let s = self.encoding.decode(&run).0.into_owned();
                    if scope.instr {
                        field_instr.push_str(&s);
                    } else {
                        self.text(&scope, para, &mut note_body, &s);
                    }
                }
                Tok::Text(s) => {
                    first = false;
                    let s = if skip_subst > 0 {
                        let n = skip_subst.min(s.len());
                        skip_subst -= n;
                        s[n..].to_string()
                    } else {
                        s
                    };
                    if scope.instr {
                        field_instr.push_str(&s);
                    } else {
                        self.text(&scope, para, &mut note_body, &s);
                    }
                }
                Tok::Word(w, n) => {
                    let was_first = first;
                    first = false;
                    if scope.instr && !was_first {
                        // Inside the instruction a control word is
                        // a switch (`\l`, `\o`), part of the text.
                        field_instr.push_str(&format!(" \\{w} "));
                        continue;
                    }
                    match w.as_str() {
                        // Destinations.
                        "fonttbl" if was_first => {
                            self.font_table();
                            return;
                        }
                        "stylesheet" if was_first => {
                            self.stylesheet();
                            return;
                        }
                        "listtable" if was_first => {
                            self.list_table();
                            return;
                        }
                        "listoverridetable" if was_first => {
                            self.override_table();
                            return;
                        }
                        "info" if was_first => {
                            self.info();
                            return;
                        }
                        "colortbl" | "header" | "footer" | "headerl" | "headerr" | "headerf"
                        | "footerl" | "footerr" | "footerf" | "pict" | "object" | "listtext"
                        | "pntext" | "shp" | "shppict" | "nonshppict" | "themedata"
                        | "colorschememapping" | "latentstyles" | "datastore" | "rsidtbl"
                        | "generator" | "revtbl" | "xmlnstbl" | "mmathPr" | "ftnsep"
                        | "ftnsepc" | "aftnsep" | "aftnsepc" | "txe" | "tc" | "do" | "fldtype"
                        | "datafield" | "docvar" | "template" | "userprops" | "protusertbl"
                        | "wgrffmtfilter" | "pgdsctbl" | "pnseclvl"
                            if was_first =>
                        {
                            self.skip_group();
                            return;
                        }
                        "footnote" if was_first => {
                            let family = NoteFamily::Footnote;
                            note_body = Some((family, Vec::new(), Para::default()));
                        }
                        "ftnalt" => {
                            if let Some((f, _, _)) = note_body.as_mut() {
                                *f = NoteFamily::Endnote;
                            }
                        }
                        "field" if was_first => {
                            field = Some((String::new(), para.text.len() as u32));
                        }
                        "fldinst" if was_first => {
                            scope.instr = true;
                        }
                        "fldrslt" if was_first => {
                            scope.result = true;
                        }
                        "xe" if was_first => {
                            xe_start = Some(para.text.len() as u32);
                        }
                        "bkmkstart" if was_first => {
                            let name = self.group_text();
                            let name = name.trim().to_string();
                            if !name.is_empty() {
                                if para.text.trim().is_empty() && para.label.is_none() {
                                    para.label = Some(name);
                                } else {
                                    para.apparatus.push(Apparatus::Point(name));
                                }
                            }
                            return;
                        }
                        "bkmkend" if was_first => {
                            self.skip_group();
                            return;
                        }
                        "pn" if was_first => {
                            // The old numbering group: its kind on
                            // the paragraph, its text not prose.
                            let kind = self.pn_group();
                            para.pn = Some(kind);
                            return;
                        }
                        // Character properties.
                        "i" => scope.italic = n.unwrap_or(1) != 0,
                        "b" => scope.bold = n.unwrap_or(1) != 0,
                        "plain" => {
                            scope.italic = false;
                            scope.bold = false;
                            scope.code = false;
                            scope.deleted = false;
                        }
                        "f" => {
                            scope.code = n
                                .and_then(|f| self.fixed_fonts.get(&f).copied())
                                .unwrap_or(false);
                        }
                        "cs" => {
                            if let Some(st) = n.and_then(|id| self.styles.get(&id)) {
                                scope.italic = scope.italic || st.italic;
                                scope.bold = scope.bold || st.bold;
                            }
                        }
                        "deleted" => scope.deleted = n.unwrap_or(1) != 0,
                        "uc" => scope.uc = n.unwrap_or(1).max(0) as usize,
                        "u" => {
                            if let Some(v) = n {
                                let v = if v < 0 { v + 65536 } else { v };
                                if let Some(c) = char::from_u32(v as u32) {
                                    if scope.instr {
                                        field_instr.push(c);
                                    } else {
                                        self.text(&scope, para, &mut note_body, &c.to_string());
                                    }
                                }
                                skip_subst = scope.uc;
                            }
                        }
                        // Paragraph properties.
                        "pard" => {
                            let p = note_para(&mut note_body, para);
                            p.outline = None;
                            p.style = None;
                            p.list = None;
                            p.pn = None;
                            p.in_table = false;
                        }
                        "par" => {
                            if let Some((_, blocks, p)) = note_body.as_mut() {
                                let text = p.text.trim().to_string();
                                if !text.is_empty() {
                                    blocks.push(Block::Paragraph { text });
                                }
                                let apparatus = std::mem::take(&mut p.apparatus);
                                let p2 = Para {
                                    text: std::mem::take(&mut p.text),
                                    apparatus,
                                    ..Default::default()
                                };
                                let out = std::mem::take(&mut self.out);
                                self.emit_apparatus(&p2);
                                let extra = std::mem::replace(&mut self.out, out);
                                blocks.extend(extra);
                            } else {
                                self.end_para(para);
                            }
                        }
                        "outlinelevel" => {
                            note_para(&mut note_body, para).outline =
                                n.and_then(|v| u8::try_from(v).ok());
                        }
                        "s" => note_para(&mut note_body, para).style = n,
                        "ls" => {
                            let p = note_para(&mut note_body, para);
                            let lvl = p.list.map(|(_, l)| l).unwrap_or(0);
                            p.list = Some((n.unwrap_or(0), lvl));
                        }
                        "ilvl" => {
                            let p = note_para(&mut note_body, para);
                            let id = p.list.map(|(i, _)| i).unwrap_or(0);
                            p.list = Some((id, n.unwrap_or(0).clamp(0, 8) as u8));
                        }
                        "intbl" => para.in_table = true,
                        "trowd" => {
                            self.row.clear();
                            self.row_header = false;
                        }
                        "trhdr" => self.row_header = true,
                        "cell" => {
                            // The cell's paragraphs are its text.
                            let mut text = std::mem::take(&mut self.cell);
                            text.push(' ');
                            text.push_str(para.text.trim());
                            para.text.clear();
                            para.in_table = false;
                            self.row.push(Cell {
                                text: quarb_text::normalize_ws(&text),
                                label: None,
                            });
                        }
                        "row" => {
                            let row = std::mem::take(&mut self.row);
                            self.rows.push((self.row_header, row));
                            self.row_header = false;
                        }
                        // Special characters.
                        "tab" | "line" | "emspace" | "enspace" | "qmspace" => {
                            self.text(&scope, para, &mut note_body, " ")
                        }
                        "bullet" => self.text(&scope, para, &mut note_body, "•"),
                        "lquote" => self.text(&scope, para, &mut note_body, "‘"),
                        "rquote" => self.text(&scope, para, &mut note_body, "’"),
                        "ldblquote" => self.text(&scope, para, &mut note_body, "“"),
                        "rdblquote" => self.text(&scope, para, &mut note_body, "”"),
                        "endash" => self.text(&scope, para, &mut note_body, "–"),
                        "emdash" => self.text(&scope, para, &mut note_body, "—"),
                        "zwj" | "zwnj" | "chftn" | "chdate" | "chpgn" | "sect" | "page"
                        | "column" | "softpage" | "softline" => {}
                        _ => {
                            // An unknown ignorable destination is
                            // skipped whole; any other control word
                            // is formatting.
                            if was_first && ignorable {
                                self.skip_group();
                                return;
                            }
                        }
                    }
                }
            }
        }
        let _ = top;
    }

    /// Text in force under `scope`: dropped when deleted or inside
    /// a non-prose destination; into the note's paragraph when a
    /// note is open; a span of emphasis or code marked by the run.
    fn text(
        &mut self,
        scope: &Scope,
        para: &mut Para,
        note_body: &mut Option<(NoteFamily, Vec<Block>, Para)>,
        s: &str,
    ) {
        if scope.deleted || scope.skip {
            return;
        }
        let p = match note_body.as_mut() {
            Some((_, _, p)) => p,
            None => para,
        };
        let lo = p.text.len() as u32;
        p.push(s);
        let hi = p.at();
        if lo < hi {
            if scope.code {
                p.apparatus.push(Apparatus::Span("code", lo, hi));
            } else if scope.italic || scope.bold {
                p.apparatus.push(Apparatus::Span("emphasis", lo, hi));
            }
        }
    }

    fn finish_field(&mut self, instr: &str, lo: u32, para: &mut Para) {
        let instr = self
            .pending_instr
            .take()
            .unwrap_or_else(|| instr.to_string());
        let instr = instr.trim();
        let text = para
            .text
            .get(lo as usize..)
            .unwrap_or("")
            .trim()
            .to_string();
        let mut words = instr.split_whitespace();
        if words
            .next()
            .is_some_and(|w| w.eq_ignore_ascii_case("HYPERLINK"))
        {
            let rest: Vec<&str> = words.collect();
            let internal = rest.contains(&"\\l");
            let target = rest
                .iter()
                .filter(|w| !w.starts_with('\\'))
                .map(|w| w.trim_matches('"'))
                .find(|w| !w.is_empty())
                .unwrap_or("")
                .to_string();
            if !target.is_empty() {
                para.apparatus.push(Apparatus::Ref(target, text, internal));
            }
        }
    }

    fn end_para(&mut self, para: &mut Para) {
        let taken = std::mem::take(para);
        if taken.in_table {
            // A cell's paragraph: kept for `\cell`.
            let mut text = std::mem::take(&mut self.cell);
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(taken.text.trim());
            self.cell = text;
            return;
        }
        self.flush_table();
        let text = taken.text.trim().to_string();
        let outline = taken
            .outline
            .or_else(|| taken.style.and_then(|s| self.chain(s, |st| st.outline)));
        if let Some(level) = outline.filter(|l| *l < 9) {
            self.close_lists(0);
            self.close_quote();
            if !text.is_empty() {
                self.out.push(Block::Heading {
                    level: level + 1,
                    lemma: text,
                });
            }
            self.emit_apparatus(&taken);
            return;
        }
        let list = taken
            .list
            .filter(|(id, _)| *id != 0)
            .map(|(id, lvl)| (id, lvl, self.ordered(id, lvl)))
            .or_else(|| taken.pn.map(|bullet| (-1, 0, !bullet)));
        if let Some((id, lvl, ordered)) = list {
            self.close_quote();
            let depth = lvl as usize + 1;
            let keep = depth.min(self.frames.len());
            self.close_lists(keep);
            if self.frames.len() == depth && self.frames.last().is_some_and(|f| f.list != id) {
                self.close_lists(depth - 1);
            }
            while self.frames.len() < depth {
                let kind = if ordered {
                    Container::OrderedList { start: 1 }
                } else {
                    Container::UnorderedList
                };
                self.out.push(Block::Open { kind, lemma: None });
                self.frames.push(Frame {
                    list: id,
                    item_open: false,
                });
            }
            if let Some(frame) = self.frames.last_mut() {
                if frame.item_open {
                    self.out.push(Block::Close { hypograph: None });
                }
                self.out.push(Block::Open {
                    kind: Container::Item,
                    lemma: None,
                });
                frame.item_open = true;
            }
            if !text.is_empty() {
                self.out.push(Block::Text { text });
            }
            self.emit_apparatus(&taken);
            return;
        }
        let quote = taken
            .style
            .and_then(|s| self.chain(s, |st| st.quote.then_some(())))
            .is_some();
        if quote {
            self.close_lists(0);
            if !self.quote_open {
                self.out.push(Block::Open {
                    kind: Container::Blockquote,
                    lemma: None,
                });
                self.quote_open = true;
            }
            if !text.is_empty() {
                self.out.push(Block::Text { text });
            }
            self.emit_apparatus(&taken);
            return;
        }
        self.close_lists(0);
        self.close_quote();
        if !text.is_empty() {
            self.out.push(Block::Paragraph { text });
        }
        self.emit_apparatus(&taken);
    }

    fn emit_apparatus(&mut self, para: &Para) {
        if let Some(onym) = &para.label {
            self.out.push(Block::Label { onym: onym.clone() });
        }
        let len = para.text.trim_end().len() as u32;
        // Adjacent runs of one kind are one span.
        let mut spans: Vec<(&'static str, u32, u32)> = Vec::new();
        for a in &para.apparatus {
            match a {
                Apparatus::Note(onym, family, at) => self.out.push(Block::NoteRef {
                    onym: onym.clone(),
                    family: Some(*family),
                    margin: false,
                    at: Some((*at).min(len)),
                }),
                Apparatus::Mark(term) => self.out.push(Block::IndexMark { term: term.clone() }),
                Apparatus::Ref(target, text, internal) => self.out.push(Block::Ref {
                    target: target.clone(),
                    text: Some(text.clone()).filter(|t| !t.is_empty()),
                    internal: *internal,
                }),
                Apparatus::Point(onym) => self.out.push(Block::Anchor { onym: onym.clone() }),
                Apparatus::Span(kind, lo, hi) => {
                    let (lo, hi) = ((*lo).min(len), (*hi).min(len));
                    if lo >= hi {
                        continue;
                    }
                    match spans.last_mut() {
                        Some((k, _, h)) if *k == *kind && *h >= lo.saturating_sub(1) => {
                            *h = hi.max(*h);
                        }
                        _ => spans.push((kind, lo, hi)),
                    }
                }
            }
        }
        for (kind, lo, hi) in spans {
            // A span that the run's trailing space widened stops
            // at its text; one that covers the whole block is the
            // block's formatting (a bold title), not an emphasis.
            let hi = para.text[..hi as usize].trim_end().len() as u32;
            if lo < hi && !(lo == 0 && hi >= len) {
                self.out.push(Block::Span {
                    kind: kind.to_string(),
                    lo,
                    hi,
                    genoses: Vec::new(),
                    fields: Vec::new(),
                });
            }
        }
    }

    fn close_lists(&mut self, to: usize) {
        while self.frames.len() > to {
            if self.frames.pop().is_some_and(|f| f.item_open) {
                self.out.push(Block::Close { hypograph: None });
            }
            self.out.push(Block::Close { hypograph: None });
        }
    }

    fn close_quote(&mut self) {
        if self.quote_open {
            self.out.push(Block::Close { hypograph: None });
            self.quote_open = false;
        }
    }

    fn flush_table(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.close_lists(0);
        self.close_quote();
        let mut rows = std::mem::take(&mut self.rows);
        let headers = if rows.first().is_some_and(|(h, _)| *h) {
            Some(rows.remove(0).1.into_iter().map(|c| c.text).collect())
        } else {
            None
        };
        let rows: Vec<Vec<Cell>> = rows
            .into_iter()
            .filter(|(h, _)| !h)
            .map(|(_, r)| r)
            .collect();
        if !rows.is_empty() || headers.is_some() {
            self.out.push(Block::Table {
                lemma: None,
                headers,
                rows,
            });
        }
    }

    fn chain<T>(&self, id: i32, pick: impl Fn(&Style) -> Option<T>) -> Option<T> {
        let mut cur = Some(id);
        let mut hops = 0;
        while let Some(i) = cur {
            let st = self.styles.get(&i)?;
            if let Some(v) = pick(st) {
                return Some(v);
            }
            cur = st.based_on.filter(|b| *b != i);
            hops += 1;
            if hops > 32 {
                return None;
            }
        }
        None
    }

    fn ordered(&self, ls: i32, level: u8) -> bool {
        self.overrides
            .get(&ls)
            .and_then(|id| self.lists.get(id))
            .and_then(|levels| levels.get(level as usize).copied())
            .unwrap_or(false)
    }

    // ---- destinations --------------------------------------------

    /// Skip to the end of the current group.
    fn skip_group(&mut self) {
        let mut depth = 1usize;
        while self.pos < self.toks.len() {
            match self.toks[self.pos] {
                Tok::Open => depth += 1,
                Tok::Close => {
                    depth -= 1;
                    if depth == 0 {
                        self.pos += 1;
                        return;
                    }
                }
                _ => {}
            }
            self.pos += 1;
        }
    }

    /// The plain text of the current group (nested groups
    /// included), consumed to its end.
    fn group_text(&mut self) -> String {
        let mut depth = 1usize;
        let mut s = String::new();
        let mut skip = 0usize;
        while self.pos < self.toks.len() {
            let tok = &self.toks[self.pos];
            self.pos += 1;
            match tok {
                Tok::Open => depth += 1,
                Tok::Close => {
                    depth -= 1;
                    if depth == 0 {
                        return s;
                    }
                }
                Tok::Text(t) => {
                    if skip > 0 {
                        let n = skip.min(t.len());
                        skip -= n;
                        s.push_str(&t[n..]);
                    } else {
                        s.push_str(t);
                    }
                }
                Tok::Byte(b) => {
                    if skip > 0 {
                        skip -= 1;
                    } else {
                        let b = *b;
                        s.push_str(&self.encoding.decode(&[b]).0);
                    }
                }
                Tok::Word(w, n) => match w.as_str() {
                    "u" => {
                        if let Some(c) = n
                            .map(|v| if v < 0 { v + 65536 } else { v })
                            .and_then(|v| char::from_u32(v as u32))
                        {
                            s.push(c);
                        }
                        skip = 1;
                    }
                    "tab" | "line" => s.push(' '),
                    _ => {}
                },
                Tok::Symbol('~') => s.push(' '),
                _ => {}
            }
        }
        s
    }

    fn font_table(&mut self) {
        let mut depth = 1usize;
        let mut font: Option<i32> = None;
        while self.pos < self.toks.len() {
            let tok = &self.toks[self.pos];
            self.pos += 1;
            match tok {
                Tok::Open => depth += 1,
                Tok::Close => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                Tok::Word(w, n) => match w.as_str() {
                    "f" => {
                        font = *n;
                        if let Some(f) = font {
                            self.fixed_fonts.entry(f).or_insert(false);
                        }
                    }
                    "fmodern" => {
                        if let Some(f) = font {
                            self.fixed_fonts.insert(f, true);
                        }
                    }
                    "fprq" if *n == Some(1) => {
                        if let Some(f) = font {
                            self.fixed_fonts.insert(f, true);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }

    fn stylesheet(&mut self) {
        let mut depth = 1usize;
        let mut cur: Option<(i32, Style, String, bool)> = None;
        while self.pos < self.toks.len() {
            let tok = std::mem::replace(&mut self.toks[self.pos], Tok::Close);
            self.pos += 1;
            match tok {
                Tok::Open => {
                    depth += 1;
                    if depth == 2 {
                        // The paragraph default style has no \s:
                        // it is \s0.
                        cur = Some((0, Style::default(), String::new(), false));
                    }
                }
                Tok::Close => {
                    depth -= 1;
                    if depth == 1
                        && let Some((id, mut st, name, is_char)) = cur.take()
                    {
                        let name = name.trim().trim_end_matches(';').trim().to_string();
                        st.quote = QUOTE_NAMES.contains(&name.as_str());
                        self.style_names.insert(id, name);
                        let _ = is_char;
                        self.styles.insert(id, st);
                    }
                    if depth == 0 {
                        return;
                    }
                }
                Tok::Word(w, n) => {
                    if let Some((id, st, _, is_char)) = cur.as_mut() {
                        match w.as_str() {
                            "s" => *id = n.unwrap_or(0),
                            "cs" => {
                                *id = n.unwrap_or(0);
                                *is_char = true;
                            }
                            "ds" | "ts" => *id = n.unwrap_or(0),
                            "sbasedon" => st.based_on = n,
                            "outlinelevel" => st.outline = n.and_then(|v| u8::try_from(v).ok()),
                            "i" => st.italic = n.unwrap_or(1) != 0,
                            "b" => st.bold = n.unwrap_or(1) != 0,
                            _ => {}
                        }
                    }
                }
                Tok::Text(t) => {
                    if let Some((_, _, name, _)) = cur.as_mut() {
                        name.push_str(&t);
                    }
                }
                Tok::Byte(b) => {
                    if let Some((_, _, name, _)) = cur.as_mut() {
                        name.push_str(&self.encoding.decode(&[b]).0);
                    }
                }
                _ => {}
            }
        }
    }

    fn list_table(&mut self) {
        let mut depth = 1usize;
        // The list being read: its levels so far and its id.
        let mut levels: Vec<bool> = Vec::new();
        let mut id: Option<i32> = None;
        let mut in_level = 0usize;
        while self.pos < self.toks.len() {
            let tok = &self.toks[self.pos];
            self.pos += 1;
            match tok {
                Tok::Open => {
                    depth += 1;
                }
                Tok::Close => {
                    if in_level == depth {
                        in_level = 0;
                    }
                    depth -= 1;
                    if depth == 1 {
                        if let Some(i) = id.take() {
                            self.lists.insert(i, std::mem::take(&mut levels));
                        }
                        levels.clear();
                    }
                    if depth == 0 {
                        return;
                    }
                }
                Tok::Word(w, n) => match w.as_str() {
                    "listlevel" => {
                        levels.push(true);
                        in_level = depth;
                    }
                    "levelnfc" => {
                        if let Some(last) = levels.last_mut() {
                            *last = *n != Some(23);
                        }
                    }
                    "listid" => id = *n,
                    _ => {}
                },
                _ => {}
            }
        }
    }

    fn override_table(&mut self) {
        let mut depth = 1usize;
        let mut list: Option<i32> = None;
        while self.pos < self.toks.len() {
            let tok = &self.toks[self.pos];
            self.pos += 1;
            match tok {
                Tok::Open => depth += 1,
                Tok::Close => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                Tok::Word(w, n) => match w.as_str() {
                    "listid" => list = *n,
                    "ls" => {
                        if let (Some(ls), Some(l)) = (n, list) {
                            self.overrides.insert(*ls, l);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }

    /// The old paragraph-numbering group: a bullet or a number.
    fn pn_group(&mut self) -> bool {
        let mut depth = 1usize;
        let mut bullet = false;
        while self.pos < self.toks.len() {
            let tok = &self.toks[self.pos];
            self.pos += 1;
            match tok {
                Tok::Open => depth += 1,
                Tok::Close => {
                    depth -= 1;
                    if depth == 0 {
                        return bullet;
                    }
                }
                Tok::Word(w, _) if w == "pnlvlblt" => bullet = true,
                _ => {}
            }
        }
        bullet
    }

    fn info(&mut self) {
        let mut depth = 1usize;
        while self.pos < self.toks.len() {
            let tok = std::mem::replace(&mut self.toks[self.pos], Tok::Close);
            self.pos += 1;
            match tok {
                Tok::Open => {
                    depth += 1;
                    // The next token names the field; its text is
                    // the group's.
                    if let Some(Tok::Word(w, _)) = self.toks.get(self.pos) {
                        let key = match w.as_str() {
                            "title" => "title",
                            "author" => "author",
                            "subject" => "subject",
                            "keywords" => "keywords",
                            "doccomm" => "description",
                            "creatim" => "created",
                            "revtim" => "modified",
                            _ => "",
                        };
                        if key.is_empty() {
                            self.skip_group();
                            depth -= 1;
                            continue;
                        }
                        let w = w.clone();
                        self.pos += 1;
                        let value = if w == "creatim" || w == "revtim" {
                            self.time_group()
                        } else {
                            self.group_text()
                        };
                        depth -= 1;
                        let value = quarb_text::normalize_ws(&value);
                        if value.is_empty() {
                            continue;
                        }
                        self.head.declare(key, &value);
                        match key {
                            "title" => self.head.title = Some(value),
                            "author" => self.head.author = Some(value),
                            "description" => self.head.description = Some(value),
                            "subject" => self.head.category = Some(value),
                            "keywords" => {
                                for t in value.split([',', ';']) {
                                    self.head.tag(t);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Tok::Close => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    /// A `\yrN\moN\dyN\hrN\minN` group as an ISO date-time.
    fn time_group(&mut self) -> String {
        let mut depth = 1usize;
        let (mut y, mut mo, mut d, mut h, mut mi) = (0, 1, 1, 0, 0);
        while self.pos < self.toks.len() {
            let tok = &self.toks[self.pos];
            self.pos += 1;
            match tok {
                Tok::Open => depth += 1,
                Tok::Close => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                Tok::Word(w, Some(n)) => match w.as_str() {
                    "yr" => y = *n,
                    "mo" => mo = *n,
                    "dy" => d = *n,
                    "hr" => h = *n,
                    "min" => mi = *n,
                    _ => {}
                },
                _ => {}
            }
        }
        if y == 0 {
            return String::new();
        }
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:00")
    }
}

/// The paragraph a property applies to: the note's while a note
/// is open, else the flow's.
fn note_para<'a>(
    note_body: &'a mut Option<(NoteFamily, Vec<Block>, Para)>,
    para: &'a mut Para,
) -> &'a mut Para {
    match note_body.as_mut() {
        Some((_, _, p)) => p,
        None => para,
    }
}
