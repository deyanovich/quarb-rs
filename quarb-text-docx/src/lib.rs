//! The Word text-level adapter: `.docx` bytes in, the
//! reader's model out — `text:report.docx` beside `text:page.html`.
//!
//! Everything derives from *declared* OOXML structure, never
//! heuristics (the docx design row in lang/TODO.md):
//!
//! - A paragraph is a heading iff it carries an outline level —
//!   directly (`w:pPr/w:outlineLvl`) or through its style chain
//!   (`styles.xml` `w:outlineLvl`, resolved along `w:basedOn`).
//!   Style *names* are never matched for headings; the level is
//!   the author's declaration, and `TextModel::build` derives the
//!   section tree from it exactly as it does for `h1/h2`.
//! - Lists come from declared numbering (`w:numPr`), their
//!   ordered/unordered flavor from `numbering.xml`'s `w:numFmt`
//!   (`bullet` = unordered; unresolvable numbering reads as
//!   unordered). Nesting follows `w:ilvl`.
//! - Block quotes are the one styleId-keyed mapping: a closed,
//!   cited set of the major adapters' own declared ids —
//!   Word's `Quote` / `IntenseQuote`, pandoc's `BlockText`,
//!   LibreOffice's `Quotations` — resolved through `basedOn`.
//!   Each id is fixed by its producing software; the set grows only by
//!   citation, never by guessing.
//! - Tables lower through the shared [`Block::Table`] rule; the
//!   header row is the declared `w:trPr/w:tblHeader`, positional
//!   otherwise none — no first-row guessing.
//! - Tracked changes read as the accepted view: `w:ins` content
//!   is text, `w:del` subtrees are skipped entirely.
//! - Footnotes are first class (ruling #35, the litogramma F5
//!   model): a `w:footnoteReference` becomes a `footnote`
//!   callout under its paragraph (`<deixis>`, `->footnote` to
//!   the body; the paragraph's text stays clean of markers),
//!   and `word/footnotes.xml` supplies the bodies (`<note>`,
//!   at the document end; the separator pseudo-notes skipped).
//!   `w:endnoteReference` / `word/endnotes.xml` read identically
//!   into the endnote family.
//! - Index marks (ruling #36): the `XE "term"` field instruction
//!   — Word's declared index mark — becomes an `index-mark` in
//!   its flow position, whether spelled as a `w:fldSimple` or a
//!   `w:instrText` run sequence. XE is the one instruction read
//!   as a fact; every other field keeps only its cached result
//!   text.
//! - Out of band in this version, recorded in the design row:
//!   headers/footers, textboxes.
//!
//! The same file keeps its other readings untouched: plain
//! `report.docx` is the archive graft with the raw OOXML grafted
//! as XML — three readings of one file, the ruling-#31 pattern.

use std::collections::HashMap;
use std::io::Read;

use quarb_text::{Block, Cell, Container, NoteFamily, TextModel};
use quick_xml::Reader;
use quick_xml::events::Event;

#[derive(Debug, thiserror::Error)]
pub enum DocxError {
    #[error("not a docx (zip) container: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("reading {0}: {1}")]
    Member(&'static str, std::io::Error),
    #[error("word/document.xml is missing — not a Word document")]
    NoDocument,
    #[error("malformed OOXML in {0}: {1}")]
    Xml(&'static str, quick_xml::Error),
}

/// Parse `.docx` bytes into the text-level model.
pub fn parse(bytes: &[u8]) -> Result<TextModel, DocxError> {
    Ok(TextModel::build(blocks(bytes)?))
}

/// Lower `.docx` bytes into the block event stream (the
/// [`TextModel::build`] input) — exposed for tests and tooling.
pub fn blocks(bytes: &[u8]) -> Result<Vec<Block>, DocxError> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    let styles = member(&mut zip, "word/styles.xml")
        .map(|xml| parse_styles(&xml))
        .unwrap_or_default();
    let numbering = member(&mut zip, "word/numbering.xml")
        .map(|xml| parse_numbering(&xml))
        .unwrap_or_default();
    let footnotes = member(&mut zip, "word/footnotes.xml");
    let endnotes = member(&mut zip, "word/endnotes.xml");
    let rels = member(&mut zip, "word/_rels/document.xml.rels")
        .map(|xml| parse_rels(&xml))
        .unwrap_or_default();
    let Some(document) = member(&mut zip, "word/document.xml") else {
        return Err(DocxError::NoDocument);
    };
    let mut out = lower(&document, &styles, &numbering, &rels)?;
    if let Some(xml) = footnotes {
        append_notes(
            &xml,
            "word/footnotes.xml",
            b"w:footnote",
            NoteFamily::Footnote,
            &mut out,
        )?;
    }
    if let Some(xml) = endnotes {
        append_notes(
            &xml,
            "word/endnotes.xml",
            b"w:endnote",
            NoteFamily::Endnote,
            &mut out,
        )?;
    }
    Ok(out)
}

/// The note bodies of one family, appended at the document end
/// (litogramma's canonical placement): one note container per
/// `w:footnote` / `w:endnote`, its paragraphs inside; the
/// separator pseudo-notes (`w:type` separator /
/// continuationSeparator) are presentation, not notes.
fn append_notes(
    xml: &str,
    part: &'static str,
    tag: &[u8],
    family: NoteFamily,
    out: &mut Vec<Block>,
) -> Result<(), DocxError> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut current: Option<String> = None;
    loop {
        let ev = reader
            .read_event_into(&mut buf)
            .map_err(|e| DocxError::Xml(part, e))?;
        match &ev {
            Event::Start(e) if e.name().as_ref() == tag => {
                let separator = attr(e, b"w:type")
                    .is_some_and(|t| t.ends_with("eparator"));
                let id = attr(e, b"w:id").unwrap_or_default();
                if separator || id.is_empty() {
                    current = None;
                } else {
                    current = Some(id.clone());
                    out.push(Block::Open {
                        kind: Container::Note {
                            onym: id,
                            family,
                            margin: false,
                        },
                        lemma: None,
                    });
                }
            }
            Event::End(e) if e.name().as_ref() == tag => {
                if current.take().is_some() {
                    out.push(Block::Close { hypograph: None });
                }
            }
            Event::Start(e) if e.name().as_ref() == b"w:p" && current.is_some() => {
                let para = read_para(&mut reader, &std::collections::HashMap::new())?;
                let text = quarb_text::normalize_ws(&para.text);
                if !text.is_empty() {
                    out.push(Block::Paragraph { text });
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

fn member(zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, name: &str) -> Option<String> {
    let mut f = zip.by_name(name).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

// ---------------------------------------------------------------
// styles.xml — outline levels and the quote chain
// ---------------------------------------------------------------

/// The closed quote vocabulary, by adapter: Word's built-ins,
/// pandoc's, LibreOffice's. Grows only by citation.
const QUOTE_IDS: &[&str] = &["Quote", "IntenseQuote", "BlockText", "Quotations"];

#[derive(Default, Clone)]
struct Style {
    outline: Option<u8>,
    based_on: Option<String>,
    /// Word's built-in quote vocabulary, by styleId.
    quote: bool,
}

type Styles = HashMap<String, Style>;

fn parse_styles(xml: &str) -> Styles {
    let mut out = Styles::new();
    let mut reader = Reader::from_str(xml);
    let mut current: Option<(String, Style)> = None;
    let mut buf = Vec::new();
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match &ev {
            Event::Start(e) if e.name().as_ref() == b"w:style" => {
                let id = attr(e, b"w:styleId").unwrap_or_default();
                let quote = QUOTE_IDS.contains(&id.as_str());
                current = Some((id, Style { quote, ..Style::default() }));
            }
            // A childless style still names itself (Quote often
            // carries nothing but its id).
            Event::Empty(e) if e.name().as_ref() == b"w:style" && current.is_none() => {
                let id = attr(e, b"w:styleId").unwrap_or_default();
                let quote = QUOTE_IDS.contains(&id.as_str());
                out.insert(id, Style { quote, ..Style::default() });
            }
            Event::End(e) if e.name().as_ref() == b"w:style" => {
                if let Some((id, style)) = current.take() {
                    out.insert(id, style);
                }
            }
            Event::Start(e) | Event::Empty(e) if current.is_some() => {
                let (_, style) = current.as_mut().unwrap();
                match e.name().as_ref() {
                    b"w:basedOn" => style.based_on = attr(e, b"w:val"),
                    b"w:outlineLvl" => {
                        style.outline = attr(e, b"w:val").and_then(|v| v.parse().ok());
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// Resolve a property through the basedOn chain (cycle-capped).
fn chain<'a, T>(styles: &'a Styles, id: &str, pick: impl Fn(&'a Style) -> Option<T>) -> Option<T> {
    let mut id = id.to_string();
    for _ in 0..16 {
        let style = styles.get(&id)?;
        if let Some(v) = pick(style) {
            return Some(v);
        }
        id = style.based_on.clone()?;
    }
    None
}

// ---------------------------------------------------------------
// numbering.xml — ordered vs unordered per (numId, ilvl)
// ---------------------------------------------------------------

#[derive(Default)]
struct Numbering {
    /// numId -> abstractNumId
    nums: HashMap<String, String>,
    /// (abstractNumId, ilvl) -> numFmt
    fmts: HashMap<(String, u8), String>,
}

impl Numbering {
    /// `bullet` numFmt is an unordered list; anything else (or an
    /// unresolvable chain) reads as unordered too — the flavor is
    /// presentation, the membership is the declaration.
    fn ordered(&self, num_id: &str, ilvl: u8) -> bool {
        self.nums
            .get(num_id)
            .and_then(|a| self.fmts.get(&(a.clone(), ilvl)))
            .is_some_and(|f| f != "bullet" && f != "none")
    }
}

fn parse_numbering(xml: &str) -> Numbering {
    let mut out = Numbering::default();
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut abstract_id: Option<String> = None;
    let mut num_id: Option<String> = None;
    let mut lvl: Option<u8> = None;
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match &ev {
            Event::Start(e) | Event::Empty(e) => match e.name().as_ref() {
                b"w:abstractNum" => abstract_id = attr(e, b"w:abstractNumId"),
                b"w:lvl" => lvl = attr(e, b"w:ilvl").and_then(|v| v.parse().ok()),
                b"w:numFmt" => {
                    if let (Some(a), Some(l), Some(f)) =
                        (abstract_id.clone(), lvl, attr(e, b"w:val"))
                    {
                        out.fmts.insert((a, l), f);
                    }
                }
                b"w:num" => num_id = attr(e, b"w:numId"),
                b"w:abstractNumId" => {
                    if let (Some(n), Some(a)) = (num_id.clone(), attr(e, b"w:val")) {
                        out.nums.insert(n, a);
                    }
                }
                _ => {}
            },
            Event::End(e) if e.name().as_ref() == b"w:abstractNum" => abstract_id = None,
            Event::End(e) if e.name().as_ref() == b"w:num" => num_id = None,
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

// ---------------------------------------------------------------
// document.xml — the body walk
// ---------------------------------------------------------------

/// `word/_rels/document.xml.rels`: relationship id → target,
/// resolving a `w:hyperlink r:id` to its URL.
fn parse_rels(xml: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match &ev {
            Event::Start(e) | Event::Empty(e)
                if e.name().as_ref() == b"Relationship" =>
            {
                if let (Some(id), Some(target)) = (attr(e, b"Id"), attr(e, b"Target")) {
                    out.insert(id, target);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// A field instruction that references: `REF bm` / `PAGEREF bm`
/// (internal), `HYPERLINK "url"` (external), `HYPERLINK \l "bm"`
/// (internal) — as (target, internal).
fn field_ref(instr: &str) -> Option<(String, bool)> {
    let t = instr.trim();
    let word = t.split_whitespace().next()?;
    match word {
        "REF" | "PAGEREF" => {
            let bm = t[word.len()..].trim().split_whitespace().next()?;
            let bm = bm.trim_matches('"');
            (!bm.is_empty()).then(|| (bm.to_string(), true))
        }
        "HYPERLINK" => {
            let rest = t["HYPERLINK".len()..].trim();
            if let Some(rest) = rest.strip_prefix("\\l") {
                let bm = rest.trim().split_whitespace().next()?.trim_matches('"');
                (!bm.is_empty()).then(|| (bm.to_string(), true))
            } else {
                let url = rest.split('"').nth(1).or_else(|| rest.split_whitespace().next())?;
                (!url.is_empty()).then(|| (url.to_string(), false))
            }
        }
        _ => None,
    }
}

/// One body paragraph, fully read.
#[derive(Default)]
struct Para {
    style: Option<String>,
    direct_outline: Option<u8>,
    num: Option<(String, u8)>,
    text: String,
    /// Inline apparatus in run order — note callouts
    /// (`w:footnoteReference` / `w:endnoteReference` ids), XE
    /// index marks, mentions, and mid-paragraph bookmarks —
    /// emitted after the paragraph's block.
    apparatus: Vec<Apparatus>,
    /// A bookmark opened before any run text: the block's own
    /// label (Word's heading cross-reference bookmarks), the
    /// attachment rule.
    label: Option<String>,
}

/// One piece of inline apparatus met in a paragraph's runs.
enum Apparatus {
    Note(String, NoteFamily),
    Mark(String),
    /// A mention met in the runs: (target, text, internal) — a
    /// w:hyperlink, or a REF / PAGEREF / HYPERLINK field.
    Ref(String, String, bool),
    /// A mid-paragraph bookmark: a point anchor at this position.
    Point(String),
}

fn lower(
    xml: &str,
    styles: &Styles,
    numbering: &Numbering,
    rels: &std::collections::HashMap<String, String>,
) -> Result<Vec<Block>, DocxError> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    // Open-container state across body children: list frames
    // (each carrying its declared numId and an item-open flag)
    // and the quote run. A numId change at the same level is a
    // NEW list — the declaration names the list identity.
    let mut frames: Vec<Frame> = Vec::new();
    let mut quote_open = false;
    loop {
        let ev = reader
            .read_event_into(&mut buf)
            .map_err(|e| DocxError::Xml("word/document.xml", e))?;
        match &ev {
            Event::Start(e) if e.name().as_ref() == b"w:p" => {
                let para = read_para(&mut reader, rels)?;
                emit_para(para, styles, numbering, &mut frames, &mut quote_open, &mut out);
            }
            Event::Start(e) if e.name().as_ref() == b"w:tbl" => {
                close_lists(&mut frames, 0, &mut out);
                close_quote(&mut quote_open, &mut out);
                let (headers, rows) = read_table(&mut reader)?;
                if !rows.is_empty() || headers.is_some() {
                    out.push(Block::Table { lemma: None, headers, rows });
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    close_lists(&mut frames, 0, &mut out);
    close_quote(&mut quote_open, &mut out);
    Ok(out)
}

fn emit_para(
    para: Para,
    styles: &Styles,
    numbering: &Numbering,
    frames: &mut Vec<Frame>,
    quote_open: &mut bool,
    out: &mut Vec<Block>,
) {
    let text = quarb_text::normalize_ws(&para.text);
    // The declared outline level: direct formatting wins over the
    // style chain. 9 spells "body text" in OOXML — not a heading.
    let outline = para.direct_outline.or_else(|| {
        para.style
            .as_deref()
            .and_then(|id| chain(styles, id, |s| s.outline))
    });
    if let Some(level) = outline.filter(|l| *l < 9) {
        close_lists(frames, 0, out);
        close_quote(quote_open, out);
        if !text.is_empty() {
            out.push(Block::Heading { level: level + 1, lemma: text });
        }
        emit_callouts(&para, out);
        return;
    }
    if let Some((num_id, ilvl)) = &para.num {
        close_quote(quote_open, out);
        let depth = (*ilvl as usize) + 1;
        close_lists(frames, depth.min(frames.len()), out);
        // A different declared list at this level closes the open
        // one: numId is the list's identity.
        if frames.len() == depth
            && frames.last().is_some_and(|f| &f.num_id != num_id)
        {
            close_lists(frames, depth - 1, out);
        }
        while frames.len() < depth {
            let kind = if numbering.ordered(num_id, frames.len() as u8) {
                Container::OrderedList { start: 1 }
            } else {
                Container::UnorderedList
            };
            out.push(Block::Open { kind, lemma: None });
            frames.push(Frame { num_id: num_id.clone(), item_open: false });
        }
        if let Some(frame) = frames.last_mut() {
            if frame.item_open {
                out.push(Block::Close { hypograph: None });
            }
            out.push(Block::Open { kind: Container::Item, lemma: None });
            frame.item_open = true;
        }
        if !text.is_empty() {
            out.push(Block::Text { text });
        }
        emit_callouts(&para, out);
        return;
    }
    let quote = para
        .style
        .as_deref()
        .and_then(|id| chain(styles, id, |s| s.quote.then_some(())))
        .is_some();
    if quote {
        close_lists(frames, 0, out);
        if !*quote_open {
            out.push(Block::Open { kind: Container::Blockquote, lemma: None });
            *quote_open = true;
        }
        if !text.is_empty() {
            out.push(Block::Text { text });
        }
        emit_callouts(&para, out);
        return;
    }
    close_lists(frames, 0, out);
    close_quote(quote_open, out);
    if !text.is_empty() {
        out.push(Block::Paragraph { text });
    }
    emit_callouts(&para, out);
}

fn emit_callouts(para: &Para, out: &mut Vec<Block>) {
    // A bookmark opened before the runs names the block just
    // emitted — Word's heading cross-reference bookmarks, the
    // attachment rule.
    if let Some(onym) = &para.label {
        out.push(Block::Label { onym: onym.clone() });
    }
    for a in &para.apparatus {
        match a {
            Apparatus::Note(onym, family) => out.push(Block::NoteRef {
                onym: onym.clone(),
                family: Some(*family),
                margin: false,
            }),
            Apparatus::Mark(term) => out.push(Block::IndexMark { term: term.clone() }),
            Apparatus::Ref(target, text, internal) => out.push(Block::Ref {
                target: target.clone(),
                text: Some(text.clone()).filter(|t| !t.is_empty()),
                internal: *internal,
            }),
            Apparatus::Point(onym) => out.push(Block::Anchor { onym: onym.clone() }),
        }
    }
}

struct Frame {
    num_id: String,
    item_open: bool,
}

fn close_lists(frames: &mut Vec<Frame>, to: usize, out: &mut Vec<Block>) {
    while frames.len() > to {
        if frames.pop().is_some_and(|f| f.item_open) {
            out.push(Block::Close { hypograph: None });
        }
        out.push(Block::Close { hypograph: None });
    }
}

fn close_quote(quote_open: &mut bool, out: &mut Vec<Block>) {
    if *quote_open {
        out.push(Block::Close { hypograph: None });
        *quote_open = false;
    }
}

/// Read one `w:p` to its end tag: properties and the accepted-view
/// run text (`w:ins` included, `w:del` skipped). Field
/// instructions (`w:instrText`) are captured out of the flow and
/// scanned for `XE` index marks; the cached result text of any
/// field stays, as before.
fn read_para(
    reader: &mut Reader<&[u8]>,
    rels: &std::collections::HashMap<String, String>,
) -> Result<Para, DocxError> {
    let mut para = Para::default();
    let mut buf = Vec::new();
    let mut depth = 1usize;
    let mut skip: usize = 0; // inside w:del subtrees
    let mut in_text = false;
    let mut in_instr = false;
    // An open w:hyperlink: (target, internal, where its text
    // began in the paragraph).
    let mut hl: Option<(String, bool, usize)> = None;
    // A referencing field past its `separate` (its visible result
    // text follows until `end`), same shape.
    let mut pending_field: Option<(String, bool, usize)> = None;
    // An open w:fldSimple that references, same shape.
    let mut fs: Option<(String, bool, usize)> = None;
    // The accumulated field instruction, XE-parsed at the field's
    // end or separate fldChar (instructions span several runs).
    let mut instr = String::new();
    loop {
        let ev = reader
            .read_event_into(&mut buf)
            .map_err(|e| DocxError::Xml("word/document.xml", e))?;
        match &ev {
            Event::Start(e) => {
                depth += 1;
                let name = e.name();
                let name = name.as_ref();
                if skip > 0 || matches!(name, b"w:del" | b"w:moveFrom") {
                    skip += 1;
                    buf.clear();
                    continue;
                }
                match name {
                    b"w:t" => in_text = true,
                    b"w:instrText" => in_instr = true,
                    b"w:hyperlink" => {
                        // Internal (w:anchor names a bookmark) or
                        // external (r:id resolves in the rels).
                        hl = attr(e, b"w:anchor")
                            .filter(|a| !a.is_empty())
                            .map(|a| (a, true, para.text.len()))
                            .or_else(|| {
                                attr(e, b"r:id")
                                    .and_then(|id| rels.get(&id).cloned())
                                    .map(|url| (url, false, para.text.len()))
                            });
                    }
                    b"w:bookmarkStart" => {
                        if let Some(nm) = attr(e, b"w:name").filter(|n| n != "_GoBack") {
                            if para.text.trim().is_empty() && para.label.is_none() {
                                para.label = Some(nm);
                            } else {
                                para.apparatus.push(Apparatus::Point(nm));
                            }
                        }
                    }
                    b"w:fldSimple" => {
                        let instr = attr(e, b"w:instr").unwrap_or_default();
                        if let Some(term) = xe_term(&instr) {
                            para.apparatus.push(Apparatus::Mark(term));
                        }
                        if let Some((t, internal)) = field_ref(&instr) {
                            fs = Some((t, internal, para.text.len()));
                        }
                    }
                    b"w:pStyle" => para.style = attr(e, b"w:val"),
                    b"w:outlineLvl" => {
                        para.direct_outline = attr(e, b"w:val").and_then(|v| v.parse().ok());
                    }
                    _ => {}
                }
            }
            Event::Empty(e) if skip == 0 => {
                let name = e.name();
                match name.as_ref() {
                    b"w:pStyle" => para.style = attr(e, b"w:val"),
                    b"w:outlineLvl" => {
                        para.direct_outline = attr(e, b"w:val").and_then(|v| v.parse().ok());
                    }
                    b"w:ilvl" => {
                        let lvl = attr(e, b"w:val").and_then(|v| v.parse().ok()).unwrap_or(0);
                        para.num.get_or_insert((String::new(), 0)).1 = lvl;
                    }
                    b"w:numId" => {
                        if let Some(id) = attr(e, b"w:val") {
                            para.num.get_or_insert((String::new(), 0)).0 = id;
                        }
                    }
                    b"w:tab" | b"w:br" | b"w:cr" => para.text.push(' '),
                    b"w:footnoteReference" => {
                        if let Some(id) = attr(e, b"w:id") {
                            para.apparatus.push(Apparatus::Note(id, NoteFamily::Footnote));
                        }
                    }
                    b"w:endnoteReference" => {
                        if let Some(id) = attr(e, b"w:id") {
                            para.apparatus.push(Apparatus::Note(id, NoteFamily::Endnote));
                        }
                    }
                    b"w:bookmarkStart" => {
                        if let Some(nm) = attr(e, b"w:name").filter(|n| n != "_GoBack") {
                            if para.text.trim().is_empty() && para.label.is_none() {
                                para.label = Some(nm);
                            } else {
                                para.apparatus.push(Apparatus::Point(nm));
                            }
                        }
                    }
                    b"w:fldSimple" => {
                        let si = attr(e, b"w:instr").unwrap_or_default();
                        if let Some(term) = xe_term(&si) {
                            para.apparatus.push(Apparatus::Mark(term));
                        }
                        if let Some((t, internal)) = field_ref(&si) {
                            // No result runs: a mention without
                            // visible text.
                            para.apparatus.push(Apparatus::Ref(t, String::new(), internal));
                        }
                    }
                    b"w:fldChar" => {
                        // The instruction is complete at separate
                        // (the result follows) or at end.
                        let ty = attr(e, b"w:fldCharType").unwrap_or_default();
                        if ty == "separate" || ty == "end" {
                            if let Some(term) = xe_term(&instr) {
                                para.apparatus.push(Apparatus::Mark(term));
                            }
                            if let Some((t, internal)) = field_ref(&instr) {
                                if ty == "separate" {
                                    // The visible result follows.
                                    pending_field = Some((t, internal, para.text.len()));
                                } else {
                                    para.apparatus.push(Apparatus::Ref(t, String::new(), internal));
                                }
                            }
                            instr.clear();
                        }
                        if ty == "end"
                            && let Some((t, internal, at)) = pending_field.take()
                        {
                            let text = para.text.get(at..).unwrap_or_default().trim().to_string();
                            para.apparatus.push(Apparatus::Ref(t, text, internal));
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(t) if skip == 0 => {
                if in_instr {
                    if let Ok(s) = t.decode() {
                        instr.push_str(&s);
                    }
                } else if in_text
                    && let Ok(s) = t.decode()
                {
                    para.text.push_str(&s);
                }
            }
            Event::End(e) => {
                depth -= 1;
                if skip > 0 {
                    skip -= 1;
                } else {
                    match e.name().as_ref() {
                        b"w:t" => in_text = false,
                        b"w:instrText" => in_instr = false,
                        b"w:hyperlink" => {
                            if let Some((t, internal, at)) = hl.take() {
                                let text =
                                    para.text.get(at..).unwrap_or_default().trim().to_string();
                                para.apparatus.push(Apparatus::Ref(t, text, internal));
                            }
                        }
                        b"w:fldSimple" => {
                            if let Some((t, internal, at)) = fs.take() {
                                let text =
                                    para.text.get(at..).unwrap_or_default().trim().to_string();
                                para.apparatus.push(Apparatus::Ref(t, text, internal));
                            }
                        }
                        _ => {}
                    }
                }
                if depth == 0 {
                    // A numId of "0" (or one never set) is the
                    // OOXML spelling of "not numbered".
                    if para
                        .num
                        .as_ref()
                        .is_some_and(|(id, _)| id.is_empty() || id == "0")
                    {
                        para.num = None;
                    }
                    return Ok(para);
                }
            }
            Event::Eof => return Ok(para),
            _ => {}
        }
        buf.clear();
    }
}

/// Read one `w:tbl` to its end tag. The declared `w:tblHeader` on
/// leading rows yields the header vector; nested tables flatten
/// into their cell's text.
#[allow(clippy::type_complexity)]
fn read_table(
    reader: &mut Reader<&[u8]>,
) -> Result<(Option<Vec<String>>, Vec<Vec<Cell>>), DocxError> {
    let mut buf = Vec::new();
    let mut depth = 1usize;
    let mut skip = 0usize;
    let mut rows: Vec<(bool, Vec<Cell>)> = Vec::new();
    let mut row: Option<(bool, Vec<Cell>)> = None;
    let mut cell: Option<String> = None;
    let mut in_text = false;
    loop {
        let ev = reader
            .read_event_into(&mut buf)
            .map_err(|e| DocxError::Xml("word/document.xml", e))?;
        match &ev {
            Event::Start(e) => {
                depth += 1;
                let name = e.name();
                let name = name.as_ref();
                if skip > 0 || matches!(name, b"w:del" | b"w:moveFrom" | b"w:instrText") {
                    skip += 1;
                    buf.clear();
                    continue;
                }
                match name {
                    b"w:tr" => row = Some((false, Vec::new())),
                    b"w:tc" => cell = Some(String::new()),
                    b"w:t" => in_text = true,
                    _ => {}
                }
            }
            Event::Empty(e) if skip == 0 => match e.name().as_ref() {
                b"w:tblHeader" => {
                    if let Some((header, _)) = row.as_mut() {
                        *header = true;
                    }
                }
                b"w:tab" | b"w:br" | b"w:cr" => {
                    if let Some(c) = cell.as_mut() {
                        c.push(' ');
                    }
                }
                _ => {}
            },
            Event::Text(t) if in_text && skip == 0 => {
                if let (Some(c), Ok(s)) = (cell.as_mut(), t.decode()) {
                    c.push_str(&s);
                }
            }
            Event::End(e) => {
                depth -= 1;
                let name = e.name();
                let name = name.as_ref();
                if skip > 0 {
                    skip -= 1;
                } else {
                    match name {
                        b"w:t" => in_text = false,
                        b"w:tc" => {
                            if let (Some((_, cells)), Some(text)) = (row.as_mut(), cell.take()) {
                                cells.push(Cell {
                                    text: quarb_text::normalize_ws(&text),
                                    label: None,
                                });
                            }
                        }
                        b"w:tr" => {
                            if let Some(r) = row.take() {
                                rows.push(r);
                            }
                        }
                        // A paragraph boundary inside a cell is a
                        // word boundary in its flattened text.
                        b"w:p" => {
                            if let Some(c) = cell.as_mut() {
                                c.push(' ');
                            }
                        }
                        _ => {}
                    }
                }
                if depth == 0 {
                    let headers = if rows.first().is_some_and(|(h, _)| *h) {
                        Some(rows.remove(0).1.into_iter().map(|c| c.text).collect())
                    } else {
                        None
                    };
                    return Ok((headers, rows.into_iter().map(|(_, r)| r).collect()));
                }
            }
            Event::Eof => return Ok((None, Vec::new())),
            _ => {}
        }
        buf.clear();
    }
}

fn attr(e: &quick_xml::events::BytesStart, name: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name)
        .and_then(|a| a.unescape_value().ok().map(|v| v.to_string()))
}

/// The one field instruction read as a fact (ruling #36): an
/// ` XE "term" ` instruction declares an index mark. The term is
/// the first quoted string after the XE token; the switches
/// (`\b`, `\i`, …) are presentation and drop. Any other
/// instruction yields nothing.
fn xe_term(instr: &str) -> Option<String> {
    let rest = instr.trim_start().strip_prefix("XE")?;
    // A complete token, not the prefix of another instruction.
    if !rest.starts_with([' ', '\t', '"']) {
        return None;
    }
    let open = rest.find('"')?;
    let rest = &rest[open + 1..];
    let close = rest.find('"')?;
    Some(rest[..close].to_string())
}
