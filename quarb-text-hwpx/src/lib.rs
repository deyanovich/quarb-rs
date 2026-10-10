//! The Hangul Word Processor text-level adapter: `.hwpx` bytes in
//! (KS X 6101, OWPML — the zip of XML Hancom's office writes and
//! nearly every Korean public document is written in), the reader's
//! model out — `text:report.hwpx` beside `text:report.docx` and
//! `text:report.odt`, in the same block vocabulary from the same kind
//! of declaration, so a text saved in any of the three answers the
//! same queries.
//!
//! Everything derives from *declared* OWPML structure, never from
//! style names or heuristics:
//!
//! - A heading is a paragraph whose paragraph property
//!   (`header.xml`'s `hh:paraPr`, by `paraPrIDRef`) declares
//!   `hh:heading type="OUTLINE"`; its `level` (0-based in the
//!   file) is the heading's depth. Style names (`개요 1`,
//!   `Outline 1`) are never matched.
//! - A list is the run of paragraphs whose property declares
//!   `hh:heading type="NUMBER"` (an ordered list, its `start` from
//!   the numbering it names) or `type="BULLET"` (unordered), nested
//!   by the declared `level`; a paragraph of another kind closes
//!   the list.
//! - A table (`hp:tbl`) lowers through the shared [`Block::Table`]
//!   rule: `hp:tr` rows, `hp:tc` cells (a cell's `hp:subList`
//!   paragraphs joined), a first row of cells declared
//!   `header="1"` the header; a cell spanning columns pads its row.
//! - Notes are first class: `hp:footNote` and `hp:endNote` inside a
//!   run are a callout at their offset in the paragraph (`<deixis>`,
//!   the family from the element) and a body at the document end
//!   (`<note>`), the body's paragraphs its own.
//! - A hyperlink field (`hp:fieldBegin type="HYPERLINK"`, its URL
//!   in the `Command` parameter) is a `ref` with the text between
//!   the field's ends; a bookmark field names a point anchor.
//! - Tracked changes read as the accepted view: insertions
//!   (`hp:insertBegin` … `hp:insertEnd`) are text, deletions
//!   (`hp:deleteBegin` … `hp:deleteEnd`) are dropped.
//! - Not carried: headers and footers (`hp:header`, `hp:footer`),
//!   pictures, drawings, text boxes (`hp:pic`, `hp:rect`, …),
//!   equations, the layout records (`hp:linesegarray`, `hp:secPr`,
//!   `hp:ctrl` other than fields), and formatting.
//! - `Contents/content.hpf` (the OPF package file) declares the
//!   identity: `opf:title`, `opf:language`, and the `opf:meta`
//!   names (`creator`, `subject`, `description`, `keyword`,
//!   `CreatedDate`, `ModifiedDate`), under the HTML head's keys.
//!
//! The binary HWP 5 (`.hwp`, a compound file whose streams hold the
//! same records) is the second container of this reading and is not
//! read yet; the plain mount of an `.hwpx` is the zip of its XML.
use std::collections::HashMap;
use std::io::Read;

mod hwp5;
pub use hwp5::{dump as hwp5_dump, is_hwp5, streams as hwp5_streams};

use quarb::Value;
use quarb::source::SourceDecoding;
use quarb_text::{Block, Cell, Container, HeadMeta, NoteFamily, TextModel};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

#[derive(Debug, thiserror::Error)]
pub enum HwpxError {
    #[error("not an HWPX package: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("not an HWPX package: {0}")]
    Malformed(String),
    #[error("{0}: {1}")]
    Xml(&'static str, quick_xml::Error),
}

/// Whether the bytes are an HWPX package: a zip whose `mimetype`
/// member declares `application/hwp+zip`, or that holds the OWPML
/// package file.
pub fn is_hwpx(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"PK") {
        return false;
    }
    let Ok(mut zip) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
        return false;
    };
    member(&mut zip, "mimetype").is_some_and(|m| m.trim() == "application/hwp+zip")
        || zip.by_name("Contents/content.hpf").is_ok()
}

/// Parse `.hwpx` bytes into the text-level model, with what the
/// package file declares as the head.
pub fn parse(bytes: &[u8]) -> Result<TextModel, HwpxError> {
    let pkg = Package::open(bytes)?;
    let mut model = TextModel::build(pkg.blocks()?);
    model.set_head_meta(pkg.head());
    Ok(model)
}

/// [`parse`] under a mount's source-boundary rule; OWPML's parts are
/// UTF-8 XML, so the rule has nothing to decide here.
pub fn parse_with_decoding(
    bytes: &[u8],
    _decoding: &SourceDecoding,
) -> Result<TextModel, HwpxError> {
    parse(bytes)
}

/// Lower the bytes into the block event stream (the
/// [`TextModel::build`] input) — exposed for tests and tooling.
pub fn blocks(bytes: &[u8]) -> Result<Vec<Block>, HwpxError> {
    Package::open(bytes)?.blocks()
}

/// What the document declares about itself.
pub fn head(bytes: &[u8]) -> Result<HeadMeta, HwpxError> {
    Ok(Package::open(bytes)?.head())
}

/// Parse `.hwp` bytes — the binary HWP 5 container — into the same
/// text-level model: the paragraph stream the compound file's
/// records hold, lowered by the rules above, with the summary
/// information as the head.
pub fn parse_hwp(bytes: &[u8]) -> Result<TextModel, HwpxError> {
    let doc = hwp5::read(bytes)?;
    let mut lower = Lower::new(&doc.props);
    for p in doc.paras {
        lower.emit_para(p);
    }
    lower.finish();
    let mut out = lower.out;
    for (onym, family, paras) in doc.notes {
        out.push(Block::Open {
            kind: Container::Note {
                onym,
                family,
                margin: false,
            },
            lemma: None,
        });
        for p in paras {
            let text = p.text.trim().to_string();
            if !text.is_empty() {
                out.push(Block::Paragraph { text });
            }
        }
        out.push(Block::Close { hypograph: None });
    }
    let mut model = TextModel::build(out);
    model.set_head_meta(head_of(&doc.identity));
    Ok(model)
}

/// [`parse_hwp`] under a mount's rule; the streams are UTF-16LE.
pub fn parse_hwp_with_decoding(
    bytes: &[u8],
    _decoding: &SourceDecoding,
) -> Result<TextModel, HwpxError> {
    parse_hwp(bytes)
}

/// The head from (name, value) declarations under the package
/// file's names.
fn head_of(metadata: &[(String, String)]) -> HeadMeta {
    Package {
        metadata: metadata.to_vec(),
        header: String::new(),
        sections: Vec::new(),
    }
    .head()
}

fn member(zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, name: &str) -> Option<String> {
    let mut f = zip.by_name(name).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

fn local(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b':') {
        Some(i) => &name[i + 1..],
        None => name,
    }
}

fn attr(e: &BytesStart, name: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| local(a.key.as_ref()) == name)
        .map(|a| String::from_utf8_lossy(&a.value).into_owned())
}

fn text_of(t: &quick_xml::events::BytesText) -> String {
    t.xml_content()
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| String::from_utf8_lossy(t).into_owned())
}

// ---------------------------------------------------------------
// The package
// ---------------------------------------------------------------

/// The parts the reading uses: the package file's metadata, the
/// header's paragraph properties, the sections in spine order.
struct Package {
    metadata: Vec<(String, String)>,
    header: String,
    sections: Vec<String>,
}

impl Package {
    fn open(bytes: &[u8]) -> Result<Package, HwpxError> {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        // The package file, where the container points (the usual
        // `Contents/content.hpf`).
        let hpf_path = member(&mut zip, "META-INF/container.xml")
            .and_then(|c| rootfile(&c))
            .unwrap_or_else(|| "Contents/content.hpf".to_string());
        let hpf = member(&mut zip, &hpf_path).ok_or_else(|| {
            HwpxError::Malformed(format!("no {hpf_path} (the OWPML package file)"))
        })?;
        let base = match hpf_path.rfind('/') {
            Some(i) => &hpf_path[..i],
            None => "",
        };
        let (metadata, items) = read_hpf(&hpf)?;
        // A manifest href is written from the package root (Hancom
        // writes `Contents/header.xml`); one written from the package
        // file's own directory is taken as a second reading.
        let mut resolve = |href: &str| -> String {
            let plain = href.trim_start_matches('/').to_string();
            if zip.by_name(&plain).is_ok() || base.is_empty() {
                plain
            } else {
                format!("{base}/{plain}")
            }
        };
        let header_path = items
            .iter()
            .find(|(id, href)| id == "header" || href.ends_with("header.xml"))
            .map(|(_, href)| resolve(href))
            .unwrap_or_else(|| "Contents/header.xml".to_string());
        let mut section_paths: Vec<(u32, String)> = items
            .iter()
            .filter_map(|(_, href)| {
                let name = href.rsplit('/').next().unwrap_or(href);
                let n = name
                    .strip_prefix("section")
                    .and_then(|r| r.strip_suffix(".xml"))
                    .and_then(|d| d.parse::<u32>().ok())?;
                Some((n, href.clone()))
            })
            .collect();
        let section_paths: Vec<(u32, String)> = section_paths
            .drain(..)
            .map(|(n, href)| (n, resolve(&href)))
            .collect();
        let header = member(&mut zip, &header_path).unwrap_or_default();
        // The sections, numbered as named.
        let mut section_paths = section_paths;
        if section_paths.is_empty() {
            section_paths.push((0, "Contents/section0.xml".to_string()));
        }
        section_paths.sort();
        let mut sections = Vec::new();
        for (_, p) in section_paths {
            if let Some(s) = member(&mut zip, &p) {
                sections.push(s);
            }
        }
        Ok(Package {
            metadata,
            header,
            sections,
        })
    }

    fn head(&self) -> HeadMeta {
        let mut h = HeadMeta::default();
        for (name, value) in &self.metadata {
            if value.is_empty() {
                continue;
            }
            h.declare(name, value);
            match name.as_str() {
                "title" => h.title = Some(value.clone()),
                "creator" => h.author = Some(value.clone()),
                "description" | "subject" if h.description.is_none() => {
                    h.description = Some(value.clone())
                }
                "keyword" | "keywords" => {
                    for k in value.split([';', ',']) {
                        let k = k.trim();
                        if !k.is_empty() && !h.tags.iter().any(|t| t == k) {
                            h.tags.push(k.to_string());
                        }
                    }
                }
                "CreatedDate" => h.published = Some(Value::Str(value.clone())),
                "ModifiedDate" => h.modified = Some(Value::Str(value.clone())),
                _ => {}
            }
        }
        h
    }

    fn blocks(&self) -> Result<Vec<Block>, HwpxError> {
        let props = read_header(&self.header)?;
        let mut lower = Lower::new(&props);
        for (i, s) in self.sections.iter().enumerate() {
            lower.section(s, i)?;
        }
        lower.finish();
        let mut out = lower.out;
        for (onym, family, paras) in lower.notes {
            out.push(Block::Open {
                kind: Container::Note {
                    onym,
                    family,
                    margin: false,
                },
                lemma: None,
            });
            out.extend(paras);
            out.push(Block::Close { hypograph: None });
        }
        Ok(out)
    }
}

/// `META-INF/container.xml`: the rootfile with the OWPML package
/// media type, else the first.
fn rootfile(xml: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    let mut first = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e))
                if local(e.name().as_ref()) == b"rootfile" =>
            {
                let path = attr(&e, b"full-path")?;
                let mt = attr(&e, b"media-type").unwrap_or_default();
                if mt == "application/hwpml-package+xml" {
                    return Some(path);
                }
                first.get_or_insert(path);
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    first
}

/// The package file: the metadata (name, text) and the manifest's
/// items (id, href).
fn read_hpf(xml: &str) -> Result<(Vec<(String, String)>, Vec<(String, String)>), HwpxError> {
    let mut reader = Reader::from_str(xml);
    let mut metadata = Vec::new();
    let mut items = Vec::new();
    let mut open: Option<String> = None;
    loop {
        let ev = reader
            .read_event()
            .map_err(|e| HwpxError::Xml("content.hpf", e))?;
        match ev {
            Event::Start(e) | Event::Empty(e) => {
                let qname = e.name();
                match local(qname.as_ref()) {
                    b"item" => {
                        if let (Some(id), Some(href)) = (attr(&e, b"id"), attr(&e, b"href")) {
                            items.push((id, href));
                        }
                    }
                    b"meta" => open = attr(&e, b"name"),
                    b"title" => open = Some("title".into()),
                    b"language" => open = Some("language".into()),
                    _ => open = None,
                }
            }
            Event::Text(t) => {
                if let Some(name) = &open {
                    let text = text_of(&t);
                    let text = text.trim();
                    if !text.is_empty() {
                        metadata.push((name.clone(), text.to_string()));
                    }
                }
            }
            Event::End(_) => open = None,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok((metadata, items))
}

// ---------------------------------------------------------------
// The header: paragraph properties
// ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeadingKind {
    None,
    Outline,
    Number,
    Bullet,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ParaProp {
    pub(crate) kind: HeadingKind,
    /// The declared level, 0-based.
    pub(crate) level: u8,
    /// The numbering or bullet the paragraph names.
    pub(crate) id_ref: u32,
}

#[derive(Default)]
pub(crate) struct Props {
    pub(crate) para: HashMap<String, ParaProp>,
    /// Numbering id → its start.
    pub(crate) numbering_start: HashMap<u32, i64>,
}

fn read_header(xml: &str) -> Result<Props, HwpxError> {
    let mut props = Props::default();
    if xml.trim().is_empty() {
        return Ok(props);
    }
    let mut reader = Reader::from_str(xml);
    let mut current: Option<String> = None;
    loop {
        let ev = reader
            .read_event()
            .map_err(|e| HwpxError::Xml("header.xml", e))?;
        match ev {
            Event::Start(e) | Event::Empty(e) => {
                let qname = e.name();
                match local(qname.as_ref()) {
                    b"paraPr" => {
                        if let Some(id) = attr(&e, b"id") {
                            props.para.insert(
                                id.clone(),
                                ParaProp {
                                    kind: HeadingKind::None,
                                    level: 0,
                                    id_ref: 0,
                                },
                            );
                            current = Some(id);
                        }
                    }
                    b"heading" => {
                        if let Some(id) = &current
                            && let Some(p) = props.para.get_mut(id)
                        {
                            p.kind = match attr(&e, b"type").as_deref() {
                                Some("OUTLINE") => HeadingKind::Outline,
                                Some("NUMBER") => HeadingKind::Number,
                                Some("BULLET") => HeadingKind::Bullet,
                                _ => HeadingKind::None,
                            };
                            p.level = attr(&e, b"level").and_then(|l| l.parse().ok()).unwrap_or(0);
                            p.id_ref = attr(&e, b"idRef").and_then(|l| l.parse().ok()).unwrap_or(0);
                        }
                    }
                    b"numbering" => {
                        if let Some(id) = attr(&e, b"id").and_then(|i| i.parse::<u32>().ok()) {
                            let start =
                                attr(&e, b"start").and_then(|s| s.parse().ok()).unwrap_or(1);
                            props.numbering_start.insert(id, start.max(1));
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) => {
                if local(e.name().as_ref()) == b"paraPr" {
                    current = None;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(props)
}

// ---------------------------------------------------------------
// The sections
// ---------------------------------------------------------------

/// An inline attachment met while reading a paragraph, in order.
pub(crate) enum Apparatus {
    Note(String, NoteFamily, u32),
    Ref(String, String, bool),
    Anchor(String),
}

pub(crate) struct Para {
    pub(crate) prop: Option<ParaProp>,
    pub(crate) text: String,
    pub(crate) apparatus: Vec<Apparatus>,
}

struct Lower<'a> {
    props: &'a Props,
    out: Vec<Block>,
    /// The note bodies: (onym, family, paragraphs), emitted at the
    /// document end.
    notes: Vec<(String, NoteFamily, Vec<Block>)>,
    note_count: usize,
    /// The open lists, innermost last: (kind, an item open).
    lists: Vec<(HeadingKind, u32, bool)>,
}

impl<'a> Lower<'a> {
    fn new(props: &'a Props) -> Self {
        Lower {
            props,
            out: Vec::new(),
            notes: Vec::new(),
            note_count: 0,
            lists: Vec::new(),
        }
    }

    /// Lower one `sectionN.xml`.
    fn section(&mut self, xml: &str, _index: usize) -> Result<(), HwpxError> {
        let mut reader = Reader::from_str(xml);
        let paras = self.read_paras(&mut reader, None)?;
        for p in paras {
            self.emit_para(p);
        }
        Ok(())
    }

    /// Read paragraphs until `until` closes (the whole document when
    /// None): a body's `hp:p` run, with tables and notes handled
    /// where they stand.
    fn read_paras(
        &mut self,
        reader: &mut Reader<&[u8]>,
        until: Option<&[u8]>,
    ) -> Result<Vec<Para>, HwpxError> {
        let mut paras: Vec<Para> = Vec::new();
        loop {
            let ev = reader
                .read_event()
                .map_err(|e| HwpxError::Xml("section", e))?;
            match ev {
                Event::Start(e) => {
                    let qname = e.name();
                    let name = local(qname.as_ref());
                    match name {
                        b"p" => {
                            let prop = attr(&e, b"paraPrIDRef")
                                .and_then(|id| self.props.para.get(&id).copied());
                            let (para, tables) = self.read_para(reader, prop)?;
                            paras.push(para);
                            // A table stands after the paragraph that
                            // anchors it, as its own block.
                            paras.extend(tables);
                        }
                        // Non-prose subtrees at this level.
                        b"header" | b"footer" | b"secPr" | b"linesegarray" | b"ctrl" | b"pic"
                        | b"rect" | b"ellipse" | b"line" | b"arc" | b"polygon" | b"curve"
                        | b"connectLine" | b"container" | b"equation" | b"ole" | b"video"
                        | b"textart" | b"compose" | b"dutmal" | b"btn" | b"radioBtn"
                        | b"checkBtn" | b"comboBox" | b"listBox" | b"edit" | b"scrollBar" => {
                            skip_to_end(reader, name)?;
                        }
                        _ => {}
                    }
                }
                Event::End(e) => {
                    if let Some(u) = until
                        && local(e.name().as_ref()) == u
                    {
                        break;
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        Ok(paras)
    }

    /// Read one `hp:p` to its end: its runs' text (deletions
    /// dropped), the fields and notes met on the way; a table inside
    /// is returned beside it as a paragraph-shaped block of its own.
    fn read_para(
        &mut self,
        reader: &mut Reader<&[u8]>,
        prop: Option<ParaProp>,
    ) -> Result<(Para, Vec<Para>), HwpxError> {
        let mut para = Para {
            prop,
            text: String::new(),
            apparatus: Vec::new(),
        };
        let mut tables: Vec<Para> = Vec::new();
        let mut deleting = 0usize;
        // An open hyperlink field: (target, the text start offset).
        let mut field: Option<(String, usize)> = None;
        loop {
            let ev = reader
                .read_event()
                .map_err(|e| HwpxError::Xml("section", e))?;
            match ev {
                Event::Start(e) => {
                    let qname = e.name();
                    let name = local(qname.as_ref());
                    match name {
                        b"t" => {}
                        b"tab" => para.text.push('\t'),
                        b"lineBreak" => para.text.push(' '),
                        b"deleteBegin" => deleting += 1,
                        b"deleteEnd" => deleting = deleting.saturating_sub(1),
                        b"footNote" | b"endNote" => {
                            let family = if name == b"footNote" {
                                NoteFamily::Footnote
                            } else {
                                NoteFamily::Endnote
                            };
                            self.note_count += 1;
                            let onym = format!("n{}", self.note_count);
                            let body = self.read_paras(reader, Some(name))?;
                            let at = para.text.trim_end().len() as u32;
                            para.apparatus
                                .push(Apparatus::Note(onym.clone(), family, at));
                            let blocks: Vec<Block> = body
                                .into_iter()
                                .filter_map(|p| {
                                    let text = p.text.trim().to_string();
                                    (!text.is_empty()).then_some(Block::Paragraph { text })
                                })
                                .collect();
                            self.notes.push((onym, family, blocks));
                        }
                        b"tbl" => {
                            let (headers, rows) = self.read_table(reader)?;
                            if !rows.is_empty() || headers.is_some() {
                                tables.push(Para {
                                    prop: None,
                                    text: String::new(),
                                    apparatus: vec![],
                                });
                                // Carried as a block through the
                                // apparatus channel below.
                                let t = tables.last_mut().expect("pushed");
                                t.apparatus.push(Apparatus::Ref(
                                    TABLE_MARK.to_string(),
                                    serialize_table(&headers, &rows),
                                    false,
                                ));
                            }
                        }
                        b"fieldBegin" => {
                            let kind = attr(&e, b"type").unwrap_or_default();
                            let params = read_field_params(reader)?;
                            match kind.as_str() {
                                "HYPERLINK" => {
                                    let target = params
                                        .get("Command")
                                        .map(|c| {
                                            c.split(';').next().unwrap_or(c).trim().to_string()
                                        })
                                        .unwrap_or_default();
                                    if !target.is_empty() {
                                        field = Some((target, para.text.len()));
                                    }
                                }
                                "BOOKMARK" => {
                                    if let Some(name) = attr(&e, b"name")
                                        .filter(|n| !n.is_empty())
                                        .or_else(|| params.get("Name").cloned())
                                    {
                                        para.apparatus.push(Apparatus::Anchor(name));
                                    }
                                }
                                _ => {}
                            }
                        }
                        b"fieldEnd" => {
                            if let Some((target, start)) = field.take() {
                                let text =
                                    para.text[start.min(para.text.len())..].trim().to_string();
                                let internal = target.starts_with('#');
                                para.apparatus.push(Apparatus::Ref(target, text, internal));
                            }
                        }
                        // Objects anchored in the paragraph: not prose.
                        b"pic" | b"rect" | b"ellipse" | b"line" | b"arc" | b"polygon"
                        | b"curve" | b"connectLine" | b"container" | b"equation" | b"ole"
                        | b"video" | b"textart" | b"compose" | b"dutmal" | b"btn" | b"radioBtn"
                        | b"checkBtn" | b"comboBox" | b"listBox" | b"edit" | b"scrollBar"
                        | b"linesegarray" | b"secPr" | b"header" | b"footer" | b"hiddenComment"
                        | b"memo" => {
                            skip_to_end(reader, name)?;
                        }
                        _ => {}
                    }
                }
                Event::Empty(e) => {
                    let qname = e.name();
                    match local(qname.as_ref()) {
                        b"tab" => para.text.push('\t'),
                        b"lineBreak" => para.text.push(' '),
                        b"deleteBegin" => deleting += 1,
                        b"deleteEnd" => deleting = deleting.saturating_sub(1),
                        b"fieldEnd" => {
                            if let Some((target, start)) = field.take() {
                                let text =
                                    para.text[start.min(para.text.len())..].trim().to_string();
                                let internal = target.starts_with('#');
                                para.apparatus.push(Apparatus::Ref(target, text, internal));
                            }
                        }
                        _ => {}
                    }
                }
                Event::Text(t) => {
                    if deleting == 0 {
                        para.text.push_str(&text_of(&t));
                    }
                }
                Event::End(e) => {
                    if local(e.name().as_ref()) == b"p" {
                        break;
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        Ok((para, tables))
    }

    /// `hp:tbl`: rows of cells, each cell its paragraphs joined; a
    /// header row where the cells declare `header="1"`; a column
    /// span pads its row.
    fn read_table(
        &mut self,
        reader: &mut Reader<&[u8]>,
    ) -> Result<(Option<Vec<String>>, Vec<Vec<Cell>>), HwpxError> {
        let mut rows: Vec<(bool, Vec<Cell>)> = Vec::new();
        loop {
            let ev = reader
                .read_event()
                .map_err(|e| HwpxError::Xml("section", e))?;
            match ev {
                Event::Start(e) => {
                    let qname = e.name();
                    match local(qname.as_ref()) {
                        b"tr" => rows.push((true, Vec::new())),
                        b"tc" => {
                            let header = attr(&e, b"header").is_some_and(|h| h == "1");
                            let (text, span) = self.read_cell(reader)?;
                            if let Some((all_header, cells)) = rows.last_mut() {
                                *all_header &= header;
                                cells.push(Cell::from(text.as_str()));
                                for _ in 1..span {
                                    cells.push(Cell::from(""));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Event::End(e) => {
                    if local(e.name().as_ref()) == b"tbl" {
                        break;
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        let headers = match rows.first() {
            Some((true, cells)) if !cells.is_empty() && rows.len() > 1 => {
                let h: Vec<String> = cells.iter().map(|c| c.text.clone()).collect();
                rows.remove(0);
                Some(h)
            }
            _ => None,
        };
        Ok((headers, rows.into_iter().map(|(_, c)| c).collect()))
    }

    /// `hp:tc`: the paragraphs of its `hp:subList` joined by a
    /// space, and its column span.
    fn read_cell(&mut self, reader: &mut Reader<&[u8]>) -> Result<(String, usize), HwpxError> {
        let mut texts: Vec<String> = Vec::new();
        let mut span = 1usize;
        loop {
            let ev = reader
                .read_event()
                .map_err(|e| HwpxError::Xml("section", e))?;
            match ev {
                Event::Start(e) => {
                    let qname = e.name();
                    match local(qname.as_ref()) {
                        b"p" => {
                            let (p, _) = self.read_para(reader, None)?;
                            let t = p.text.trim().to_string();
                            if !t.is_empty() {
                                texts.push(t);
                            }
                        }
                        b"cellSpan" => {
                            span = attr(&e, b"colSpan")
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(1);
                        }
                        _ => {}
                    }
                }
                Event::Empty(e) => {
                    if local(e.name().as_ref()) == b"cellSpan" {
                        span = attr(&e, b"colSpan")
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(1);
                    }
                }
                Event::End(e) => {
                    if local(e.name().as_ref()) == b"tc" {
                        break;
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        Ok((texts.join(" "), span.max(1)))
    }

    fn emit_para(&mut self, para: Para) {
        // A table carried through: emit it as a block.
        if let Some(Apparatus::Ref(mark, payload, _)) = para.apparatus.first()
            && mark == TABLE_MARK
        {
            self.close_lists();
            let (headers, rows) = deserialize_table(payload);
            self.out.push(Block::Table {
                lemma: None,
                headers,
                rows,
            });
            return;
        }
        let text = para.text.trim().to_string();
        match para.prop {
            Some(ParaProp {
                kind: HeadingKind::Outline,
                level,
                ..
            }) => {
                self.close_lists();
                if !text.is_empty() {
                    self.out.push(Block::Heading {
                        level: level.saturating_add(1),
                        lemma: text,
                    });
                }
                self.emit_apparatus(&para);
            }
            Some(ParaProp {
                kind: kind @ (HeadingKind::Number | HeadingKind::Bullet),
                level,
                id_ref,
            }) => {
                self.enter_list(kind, id_ref, level as usize + 1);
                if !text.is_empty() {
                    self.out.push(Block::Text { text });
                }
                self.emit_apparatus(&para);
            }
            _ => {
                self.close_lists();
                if !text.is_empty() {
                    self.out.push(Block::Paragraph { text });
                }
                self.emit_apparatus(&para);
            }
        }
    }

    /// Stand inside an item at `depth` of a list of `kind` naming
    /// `id_ref`: close what is deeper or different, open what is
    /// missing (a deeper list opens inside the current item), then
    /// open the item.
    fn enter_list(&mut self, kind: HeadingKind, id_ref: u32, depth: usize) {
        while self.lists.len() > depth {
            self.close_list();
        }
        if self.lists.len() == depth
            && self
                .lists
                .last()
                .is_some_and(|(k, id, _)| *k != kind || *id != id_ref)
        {
            self.close_list();
        }
        while self.lists.len() < depth {
            if let Some((_, _, open)) = self.lists.last_mut()
                && !*open
            {
                self.out.push(Block::Open {
                    kind: Container::Item,
                    lemma: None,
                });
                *open = true;
            }
            let container = match kind {
                HeadingKind::Number => Container::OrderedList {
                    start: self
                        .props
                        .numbering_start
                        .get(&id_ref)
                        .copied()
                        .unwrap_or(1),
                },
                _ => Container::UnorderedList,
            };
            self.out.push(Block::Open {
                kind: container,
                lemma: None,
            });
            self.lists.push((kind, id_ref, false));
        }
        if let Some((_, _, open)) = self.lists.last_mut() {
            if *open {
                self.out.push(Block::Close { hypograph: None });
            }
            self.out.push(Block::Open {
                kind: Container::Item,
                lemma: None,
            });
            *open = true;
        }
    }

    fn close_list(&mut self) {
        if let Some((_, _, open)) = self.lists.pop() {
            if open {
                self.out.push(Block::Close { hypograph: None });
            }
            self.out.push(Block::Close { hypograph: None });
        }
    }

    fn close_lists(&mut self) {
        while !self.lists.is_empty() {
            self.close_list();
        }
    }

    fn finish(&mut self) {
        self.close_lists();
    }

    fn emit_apparatus(&mut self, para: &Para) {
        for a in &para.apparatus {
            match a {
                Apparatus::Note(onym, family, at) => self.out.push(Block::NoteRef {
                    onym: onym.clone(),
                    family: Some(*family),
                    margin: false,
                    at: Some(*at),
                }),
                Apparatus::Ref(target, text, internal) => self.out.push(Block::Ref {
                    target: target.clone(),
                    text: Some(text.clone()).filter(|t| !t.is_empty()),
                    internal: *internal,
                }),
                Apparatus::Anchor(onym) => self.out.push(Block::Anchor { onym: onym.clone() }),
            }
        }
    }
}

/// A table carried from the paragraph reader to the emitter inside
/// the apparatus channel: the mark, and the grid as text.
pub(crate) const TABLE_MARK: &str = "\u{1}table";

pub(crate) fn serialize_table(headers: &Option<Vec<String>>, rows: &[Vec<Cell>]) -> String {
    let mut s = String::new();
    match headers {
        Some(h) => {
            s.push('H');
            s.push_str(&h.join("\u{2}"));
        }
        None => s.push('N'),
    }
    for r in rows {
        s.push('\u{3}');
        s.push_str(
            &r.iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join("\u{2}"),
        );
    }
    s
}

fn deserialize_table(s: &str) -> (Option<Vec<String>>, Vec<Vec<Cell>>) {
    let mut parts = s.split('\u{3}');
    let head = parts.next().unwrap_or("N");
    let headers = head
        .strip_prefix('H')
        .map(|h| h.split('\u{2}').map(str::to_string).collect());
    let rows = parts
        .map(|r| r.split('\u{2}').map(Cell::from).collect())
        .collect();
    (headers, rows)
}

/// `hp:fieldBegin`'s parameters: name → text.
fn read_field_params(reader: &mut Reader<&[u8]>) -> Result<HashMap<String, String>, HwpxError> {
    let mut params = HashMap::new();
    let mut open: Option<String> = None;
    loop {
        let ev = reader
            .read_event()
            .map_err(|e| HwpxError::Xml("section", e))?;
        match ev {
            Event::Start(e) => {
                let qname = e.name();
                match local(qname.as_ref()) {
                    b"stringParam" | b"integerParam" => open = attr(&e, b"name"),
                    _ => {}
                }
            }
            Event::Text(t) => {
                if let Some(name) = &open {
                    params.insert(name.clone(), text_of(&t));
                }
            }
            Event::End(e) => {
                let qname = e.name();
                match local(qname.as_ref()) {
                    b"stringParam" | b"integerParam" => open = None,
                    b"fieldBegin" => break,
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(params)
}

/// Skip to the end of the element `name` just opened (nesting of the
/// same name counted).
fn skip_to_end(reader: &mut Reader<&[u8]>, name: &[u8]) -> Result<(), HwpxError> {
    let mut depth = 1usize;
    loop {
        let ev = reader
            .read_event()
            .map_err(|e| HwpxError::Xml("section", e))?;
        match ev {
            Event::Start(e) if local(e.name().as_ref()) == name => depth += 1,
            Event::End(e) if local(e.name().as_ref()) == name => {
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}
