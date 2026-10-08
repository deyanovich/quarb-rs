//! The OpenDocument text-level adapter: `.odt` bytes in, the
//! reader's model out — `text:report.odt` beside `text:report.docx`.
//! The Word reader's sibling: the same block vocabulary from the
//! same kind of declaration, in ODF's spelling.
//!
//! Everything derives from *declared* ODF structure, never
//! heuristics:
//!
//! - A heading is `text:h`, its depth `text:outline-level`; a
//!   paragraph whose style chain declares
//!   `style:default-outline-level` (along `style:parent-style-name`)
//!   is a heading of that depth too. Style *names* are never
//!   matched for headings.
//! - Lists are the declared `text:list` / `text:list-item`
//!   containers, nested as written; ordered or unordered from the
//!   list style's level kind (`text:list-level-style-number` vs
//!   `-bullet`), unordered when nothing resolves.
//! - Tables lower through the shared [`Block::Table`] rule; the
//!   header is the declared `table:table-header-rows`, otherwise
//!   none.
//! - Block quotes are the one style-keyed mapping: LibreOffice's
//!   `Quotations` paragraph style, resolved along the parent
//!   chain — the same closed set the Word reader cites.
//! - Notes are first class: a `text:note` is a callout at its
//!   offset in the paragraph (`<deixis>`, the family from
//!   `text:note-class`), its `text:note-body` the body (`<note>`,
//!   at the document end), `text:note-citation` the label.
//! - Bookmarks and reference marks (`text:bookmark`,
//!   `text:reference-mark`, their `-start` forms) name the block
//!   when opened before its first run, else they are point
//!   anchors; `text:a` is a `ref` to its `xlink:href` (a `#name`
//!   fragment internal, the rest external);
//!   `text:bookmark-ref` / `text:reference-ref` are internal
//!   refs with their cached text as prose;
//!   `text:alphabetical-index-mark` (and the `-start` form) is an
//!   `index-mark`.
//! - Emphasis and code come from character styles' declared
//!   properties (an italic or bold `fo:font-style` /
//!   `fo:font-weight`, a fixed `style:font-pitch`), as spans of
//!   the paragraph.
//! - Tracked changes read as the accepted view: the flow's text
//!   is the text (insertions included), the `text:tracked-changes`
//!   record with the deletions is dropped.
//! - Every other field keeps its cached text. Generated matter
//!   (`text:index-body` of a table of contents, a bibliography,
//!   an index) is not prose and is dropped, as are frames,
//!   drawings, text boxes and annotations.
//! - `meta.xml` declares the identity: `dc:title`,
//!   `dc:creator` / `meta:initial-creator`, `dc:description`,
//!   `dc:subject`, `meta:keyword`, under the HTML head's keys.
//!
//! A flat `.fodt` (one XML holding meta, styles and body) and an
//! OpenOffice.org 1.0 `.sxw` (the same shape under older
//! namespaces) read through the same rules; `.ods` is a
//! spreadsheet and `.odp` a deck, neither read here.

use std::collections::HashMap;
use std::io::Read;

use quarb::source::SourceDecoding;

use quarb_text::{Block, Cell, Container, HeadMeta, NoteFamily, TextModel};
use quick_xml::Reader;
use quick_xml::events::Event;

#[derive(Debug, thiserror::Error)]
pub enum OdtError {
    #[error("member {name}: {error}")]
    MemberText { name: String, error: String },
    #[error("not an OpenDocument (zip) container: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("content.xml is missing — not an OpenDocument package")]
    NoContent,
    #[error("the package's mimetype is {0:?}, not an OpenDocument text")]
    NotText(String),
    #[error("malformed ODF XML in {0}: {1}")]
    Xml(&'static str, quick_xml::Error),
}

/// The ODF text mimetype (`application/vnd.oasis.opendocument.text`)
/// and OpenOffice.org 1.0's.
const TEXT_MIMETYPES: &[&str] = &[
    "application/vnd.oasis.opendocument.text",
    "application/vnd.oasis.opendocument.text-master",
    "application/vnd.sun.xml.writer",
];

/// Whether a zip's `mimetype` member declares an OpenDocument
/// text — the dispatch for a package under another extension.
pub fn declares_text(bytes: &[u8]) -> bool {
    let Ok(mut zip) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
        return false;
    };
    member(&mut zip, "mimetype").is_some_and(|m| TEXT_MIMETYPES.contains(&m.trim()))
}

/// Parse `.odt` (or `.fodt`, `.sxw`) bytes into the text-level
/// model, with what `meta.xml` declares as the head.
pub fn parse(bytes: &[u8]) -> Result<TextModel, OdtError> {
    parse_with_decoding(bytes, &SourceDecoding::default())
}

/// [`parse`] under a mount's source-boundary rule: every XML part
/// decodes by `?encoding=`, its byte-order mark, its XML
/// declaration, else strict UTF-8.
pub fn parse_with_decoding(bytes: &[u8], decoding: &SourceDecoding) -> Result<TextModel, OdtError> {
    let parts = Parts::open(bytes, decoding)?;
    let mut model = TextModel::build(parts.blocks()?);
    model.set_head_meta(parts.head());
    Ok(model)
}

/// Lower the bytes into the block event stream (the
/// [`TextModel::build`] input) — exposed for tests and tooling.
pub fn blocks(bytes: &[u8]) -> Result<Vec<Block>, OdtError> {
    Parts::open(bytes, &SourceDecoding::default())?.blocks()
}

/// What the document declares about itself.
pub fn head(bytes: &[u8]) -> Result<HeadMeta, OdtError> {
    Ok(Parts::open(bytes, &SourceDecoding::default())?.head())
}

/// The XML parts a document comes in: a package's `styles.xml`,
/// `content.xml` and `meta.xml`, or the one XML of a flat file
/// holding all three.
struct Parts {
    styles: Option<String>,
    content: String,
    meta: Option<String>,
}

impl Parts {
    fn open(bytes: &[u8], decoding: &SourceDecoding) -> Result<Parts, OdtError> {
        if bytes.starts_with(b"PK") {
            let mut zip =
                Members::new(zip::ZipArchive::new(std::io::Cursor::new(bytes))?, decoding);
            if let Some(m) = zip.get("mimetype")?
                && !TEXT_MIMETYPES.contains(&m.trim())
            {
                return Err(OdtError::NotText(m.trim().to_string()));
            }
            let Some(content) = zip.get("content.xml")? else {
                return Err(OdtError::NoContent);
            };
            Ok(Parts {
                styles: zip.get("styles.xml")?,
                meta: zip.get("meta.xml")?,
                content,
            })
        } else {
            // A flat document: every part in the one XML, decoded
            // under the mount's rule (its declaration counts).
            let xml = quarb::source::decode(
                bytes,
                decoding.explicit.as_deref(),
                quarb::source::xml_declared_encoding,
            )
            .map_err(|e| OdtError::MemberText {
                name: "(flat document)".to_string(),
                error: e.to_string(),
            })?
            .text;
            Ok(Parts {
                styles: None,
                meta: Some(xml.clone()),
                content: xml,
            })
        }
    }

    fn blocks(&self) -> Result<Vec<Block>, OdtError> {
        let mut styles = Styles::default();
        if let Some(xml) = &self.styles {
            parse_styles(xml, "styles.xml", &mut styles)?;
        }
        // The automatic styles live beside the body (a package's
        // content.xml; the flat file's one XML).
        parse_styles(&self.content, "content.xml", &mut styles)?;
        let mut lower = Lower::new(&styles);
        lower.body(&self.content)?;
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

    fn head(&self) -> HeadMeta {
        let mut h = HeadMeta::default();
        let Some(xml) = &self.meta else {
            return h;
        };
        let mut reader = Reader::from_str(xml);
        let mut buf = Vec::new();
        let mut open: Option<String> = None;
        let mut text = String::new();
        while let Ok(ev) = reader.read_event_into(&mut buf) {
            match &ev {
                Event::Start(e) => {
                    let name = e.name();
                    let name = String::from_utf8_lossy(name.as_ref()).to_string();
                    if matches!(
                        name.as_str(),
                        "dc:title"
                            | "dc:creator"
                            | "meta:initial-creator"
                            | "dc:description"
                            | "dc:subject"
                            | "meta:keyword"
                            | "dc:language"
                            | "dc:date"
                            | "meta:creation-date"
                    ) {
                        open = Some(name);
                        text.clear();
                    }
                }
                Event::Text(t) if open.is_some() => {
                    if let Ok(s) = t.decode() {
                        text.push_str(&s);
                    }
                }
                Event::GeneralRef(r) if open.is_some() => {
                    if let Some(c) = general_ref(r) {
                        text.push_str(&c);
                    }
                }
                Event::End(_) => {
                    if let Some(name) = open.take() {
                        let value = quarb_text::normalize_ws(&text);
                        if value.is_empty() {
                            continue;
                        }
                        match name.as_str() {
                            "dc:title" => {
                                h.declare("title", &value);
                                h.title = Some(value);
                            }
                            "dc:creator" | "meta:initial-creator" => {
                                h.declare("author", &value);
                                if h.author.is_none() {
                                    h.author = Some(value);
                                }
                            }
                            "dc:description" => {
                                h.declare("description", &value);
                                h.description = Some(value);
                            }
                            "dc:subject" => {
                                h.declare("subject", &value);
                                if h.category.is_none() {
                                    h.category = Some(value);
                                }
                            }
                            "meta:keyword" => {
                                h.declare("keywords", &value);
                                h.tag(&value);
                            }
                            "dc:language" => h.declare("lang", &value),
                            "dc:date" => h.declare("modified", &value),
                            "meta:creation-date" => h.declare("created", &value),
                            _ => {}
                        }
                    }
                }
                Event::Eof => break,
                _ => {}
            }
            buf.clear();
        }
        h
    }
}

fn member(zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, name: &str) -> Option<String> {
    let mut f = zip.by_name(name).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

/// The package's members, read under the mount's rule: a member
/// decodes by `?encoding=`, its byte-order mark, its XML
/// declaration, else strict UTF-8. A member that decodes under
/// none is an error naming it, never a part silently missing.
struct Members<'a> {
    zip: zip::ZipArchive<std::io::Cursor<&'a [u8]>>,
    explicit: Option<String>,
}

impl<'a> Members<'a> {
    fn new(zip: zip::ZipArchive<std::io::Cursor<&'a [u8]>>, decoding: &SourceDecoding) -> Self {
        Members {
            zip,
            explicit: decoding.explicit.clone(),
        }
    }

    /// `None` when the package has no such member.
    fn get(&mut self, name: &str) -> Result<Option<String>, OdtError> {
        let Ok(mut f) = self.zip.by_name(name) else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)
            .map_err(|e| OdtError::MemberText {
                name: name.to_string(),
                error: e.to_string(),
            })?;
        let d = quarb::source::decode(
            &bytes,
            self.explicit.as_deref(),
            quarb::source::xml_declared_encoding,
        )
        .map_err(|e| OdtError::MemberText {
            name: name.to_string(),
            error: e.to_string(),
        })?;
        Ok(Some(d.text))
    }
}

// ---------------------------------------------------------------
// styles — outline levels, the quote chain, list kinds, character
// properties
// ---------------------------------------------------------------

/// The closed quote vocabulary: LibreOffice's (and pandoc's)
/// `Quotations`. Grows only by citation.
const QUOTE_STYLES: &[&str] = &["Quotations"];

#[derive(Default, Clone)]
struct Style {
    parent: Option<String>,
    /// `style:default-outline-level`, a paragraph style's declared
    /// heading depth.
    outline: Option<u8>,
    quote: bool,
    /// A character style's declared italic or bold.
    emphasis: bool,
    /// A character style's declared fixed pitch.
    code: bool,
}

#[derive(Default)]
struct Styles {
    paragraph: HashMap<String, Style>,
    text: HashMap<String, Style>,
    /// List style name → level (1-based) → ordered.
    list: HashMap<String, HashMap<u8, bool>>,
}

impl Styles {
    /// A property along a style's parent chain, first declaration
    /// wins (a cycle stops at the depth bound).
    fn chain<'a, T>(
        map: &'a HashMap<String, Style>,
        name: &str,
        pick: impl Fn(&'a Style) -> Option<T>,
    ) -> Option<T> {
        let mut cur = Some(name.to_string());
        let mut hops = 0;
        while let Some(n) = cur {
            let s = map.get(&n)?;
            if let Some(v) = pick(s) {
                return Some(v);
            }
            cur = s.parent.clone();
            hops += 1;
            if hops > 32 {
                return None;
            }
        }
        None
    }

    fn outline(&self, para_style: &str) -> Option<u8> {
        Self::chain(&self.paragraph, para_style, |s| s.outline)
    }

    /// The style's name is its declaration (a flat document may
    /// use `Quotations` without restating it); the chain covers a
    /// style derived from it.
    fn quote(&self, para_style: &str) -> bool {
        QUOTE_STYLES.contains(&para_style)
            || Self::chain(&self.paragraph, para_style, |s| s.quote.then_some(())).is_some()
    }

    fn emphasis(&self, text_style: &str) -> bool {
        Self::chain(&self.text, text_style, |s| s.emphasis.then_some(())).is_some()
    }

    fn code(&self, text_style: &str) -> bool {
        Self::chain(&self.text, text_style, |s| s.code.then_some(())).is_some()
    }

    fn ordered(&self, list_style: &str, level: u8) -> bool {
        self.list
            .get(list_style)
            .and_then(|levels| levels.get(&level))
            .copied()
            .unwrap_or(false)
    }
}

fn parse_styles(xml: &str, part: &'static str, styles: &mut Styles) -> Result<(), OdtError> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    // The open style:style (its family and name) and the open
    // text:list-style.
    let mut open: Option<(String, String)> = None;
    let mut list: Option<String> = None;
    loop {
        let ev = reader
            .read_event_into(&mut buf)
            .map_err(|e| OdtError::Xml(part, e))?;
        match &ev {
            Event::Start(e) | Event::Empty(e) => {
                let empty = matches!(ev, Event::Empty(_));
                let name = e.name();
                match name.as_ref() {
                    b"style:style" => {
                        let family = attr(e, b"style:family").unwrap_or_default();
                        let name = attr(e, b"style:name").unwrap_or_default();
                        let mut st = Style {
                            parent: attr(e, b"style:parent-style-name"),
                            outline: attr(e, b"style:default-outline-level")
                                .and_then(|v| v.parse().ok()),
                            ..Default::default()
                        };
                        if family == "paragraph" {
                            let display = attr(e, b"style:display-name");
                            st.quote = QUOTE_STYLES.contains(&name.as_str())
                                || display
                                    .as_deref()
                                    .is_some_and(|d| QUOTE_STYLES.contains(&d));
                            styles.paragraph.insert(name.clone(), st);
                        } else if family == "text" {
                            styles.text.insert(name.clone(), st);
                        }
                        if !empty {
                            open = Some((family, name));
                        }
                    }
                    b"style:text-properties" => {
                        if let Some((family, name)) = &open
                            && family == "text"
                            && let Some(st) = styles.text.get_mut(name)
                        {
                            let italic = attr(e, b"fo:font-style")
                                .is_some_and(|v| v == "italic" || v == "oblique");
                            let bold = attr(e, b"fo:font-weight").is_some_and(|v| {
                                v == "bold" || v.parse::<u32>().is_ok_and(|w| w >= 600)
                            });
                            st.emphasis = st.emphasis || italic || bold;
                            st.code = st.code
                                || attr(e, b"style:font-pitch").is_some_and(|v| v == "fixed");
                        }
                    }
                    b"text:list-style" => {
                        let name = attr(e, b"style:name").unwrap_or_default();
                        styles.list.entry(name.clone()).or_default();
                        if !empty {
                            list = Some(name);
                        }
                    }
                    b"text:list-level-style-number" | b"text:list-level-style-bullet" => {
                        if let Some(l) = &list
                            && let Some(level) = attr(e, b"text:level").and_then(|v| v.parse().ok())
                        {
                            let ordered = name.as_ref() == b"text:list-level-style-number";
                            styles
                                .list
                                .entry(l.clone())
                                .or_default()
                                .insert(level, ordered);
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"style:style" => open = None,
                b"text:list-style" => list = None,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

// ---------------------------------------------------------------
// the body
// ---------------------------------------------------------------

/// What a paragraph's inline content yields besides its text.
enum Apparatus {
    /// A note callout: id, family, byte offset in the text.
    Note(String, NoteFamily, u32),
    Mark(String),
    /// A reference: target, visible text, internal.
    Ref(String, String, bool),
    Point(String),
    /// An emphasis or code span: kind, lo, hi.
    Span(&'static str, u32, u32),
}

#[derive(Default)]
struct Para {
    text: String,
    style: Option<String>,
    /// `text:h`'s declared depth.
    outline: Option<u8>,
    label: Option<String>,
    apparatus: Vec<Apparatus>,
}

impl Para {
    /// Append text, collapsing whitespace as the reader's model
    /// does, so byte offsets taken while reading hold in the
    /// finished block.
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

struct Lower<'a> {
    styles: &'a Styles,
    out: Vec<Block>,
    /// The note bodies, appended after the body: id, family, the
    /// body's blocks.
    notes: Vec<(String, NoteFamily, Vec<Block>)>,
    quote_open: bool,
}

impl<'a> Lower<'a> {
    fn new(styles: &'a Styles) -> Self {
        Lower {
            styles,
            out: Vec::new(),
            notes: Vec::new(),
            quote_open: false,
        }
    }

    /// Lower the `office:text` body of `xml`.
    fn body(&mut self, xml: &str) -> Result<(), OdtError> {
        let mut reader = Reader::from_str(xml);
        let mut buf = Vec::new();
        let mut in_body = false;
        // Nesting of subtrees that are not prose.
        let mut skip = 0usize;
        // The open lists' styles (an unstyled nested list
        // continues its parent's), and whether an item is open at
        // each depth.
        let mut lists: Vec<(String, bool)> = Vec::new();
        loop {
            let ev = reader
                .read_event_into(&mut buf)
                .map_err(|e| OdtError::Xml("content.xml", e))?;
            match &ev {
                Event::Start(e) => {
                    let name = e.name();
                    let name = name.as_ref();
                    if !in_body {
                        if name == b"office:text" {
                            in_body = true;
                        }
                        buf.clear();
                        continue;
                    }
                    if skip > 0 || is_dropped(name) {
                        skip += 1;
                        buf.clear();
                        continue;
                    }
                    match name {
                        b"text:p" | b"text:h" => {
                            let para = self.read_para(&mut reader, e, "content.xml")?;
                            self.emit_para(para, !lists.is_empty());
                        }
                        b"text:list" => {
                            self.close_quote();
                            let style = attr(e, b"text:style-name")
                                .or_else(|| lists.last().map(|(s, _)| s.clone()))
                                .unwrap_or_default();
                            let level = lists.len() as u8 + 1;
                            let kind = if self.styles.ordered(&style, level) {
                                Container::OrderedList { start: 1 }
                            } else {
                                Container::UnorderedList
                            };
                            self.out.push(Block::Open { kind, lemma: None });
                            lists.push((style, false));
                        }
                        b"text:list-item" | b"text:list-header" => {
                            if let Some((_, open)) = lists.last_mut() {
                                self.out.push(Block::Open {
                                    kind: Container::Item,
                                    lemma: None,
                                });
                                *open = true;
                            }
                        }
                        b"table:table" => {
                            self.close_quote();
                            let (headers, rows) = read_table(&mut reader, "content.xml")?;
                            if !rows.is_empty() || headers.is_some() {
                                self.out.push(Block::Table {
                                    lemma: None,
                                    headers,
                                    rows,
                                });
                            }
                        }
                        _ => {}
                    }
                }
                Event::Empty(e) if in_body && skip == 0 => {
                    // An empty paragraph or heading: nothing to say.
                    let _ = e;
                }
                Event::End(e) => {
                    let name = e.name();
                    let name = name.as_ref();
                    if skip > 0 {
                        skip -= 1;
                        buf.clear();
                        continue;
                    }
                    if !in_body {
                        buf.clear();
                        continue;
                    }
                    match name {
                        b"office:text" => break,
                        b"text:list-item" | b"text:list-header" => {
                            if let Some((_, open)) = lists.last_mut()
                                && *open
                            {
                                self.out.push(Block::Close { hypograph: None });
                                *open = false;
                            }
                        }
                        b"text:list" => {
                            if let Some((_, open)) = lists.pop() {
                                if open {
                                    self.out.push(Block::Close { hypograph: None });
                                }
                                self.out.push(Block::Close { hypograph: None });
                            }
                        }
                        _ => {}
                    }
                }
                Event::Eof => break,
                _ => {}
            }
            buf.clear();
        }
        self.close_quote();
        Ok(())
    }

    fn close_quote(&mut self) {
        if self.quote_open {
            self.out.push(Block::Close { hypograph: None });
            self.quote_open = false;
        }
    }

    fn emit_para(&mut self, para: Para, in_list: bool) {
        let text = para.text.trim().to_string();
        let outline = para
            .outline
            .or_else(|| para.style.as_deref().and_then(|s| self.styles.outline(s)));
        if let Some(level) = outline.filter(|l| *l >= 1) {
            self.close_quote();
            if !text.is_empty() {
                self.out.push(Block::Heading { level, lemma: text });
            }
            self.emit_apparatus(&para);
            return;
        }
        if in_list {
            if !text.is_empty() {
                self.out.push(Block::Text { text });
            }
            self.emit_apparatus(&para);
            return;
        }
        let quote = para.style.as_deref().is_some_and(|s| self.styles.quote(s));
        if quote {
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
            self.emit_apparatus(&para);
            return;
        }
        self.close_quote();
        if !text.is_empty() {
            self.out.push(Block::Paragraph { text });
        }
        self.emit_apparatus(&para);
    }

    fn emit_apparatus(&mut self, para: &Para) {
        if let Some(onym) = &para.label {
            self.out.push(Block::Label { onym: onym.clone() });
        }
        let len = para.text.trim_end().len() as u32;
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
                    if lo < hi {
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
        }
    }

    /// Read one `text:p` / `text:h` to its end tag: the collapsed
    /// text, the style, the declared outline level, and the
    /// apparatus met on the way. A `text:note` inside yields the
    /// callout here and its body to [`Self::notes`].
    fn read_para(
        &mut self,
        reader: &mut Reader<&[u8]>,
        start: &quick_xml::events::BytesStart,
        part: &'static str,
    ) -> Result<Para, OdtError> {
        let mut para = Para {
            style: attr(start, b"text:style-name"),
            outline: (start.name().as_ref() == b"text:h")
                .then(|| attr(start, b"text:outline-level").and_then(|v| v.parse().ok()))
                .flatten(),
            ..Default::default()
        };
        let mut buf = Vec::new();
        let mut depth = 1usize;
        let mut skip = 0usize;
        // Open spans: style name and the offset they began at.
        let mut spans: Vec<(Option<String>, u32)> = Vec::new();
        // An open text:a: target, internal, where its text began.
        let mut link: Option<(String, bool, u32)> = None;
        // An open bookmark-ref / reference-ref, same shape.
        let mut field_ref: Option<(String, u32)> = None;
        // An open index-mark-start: where its term began.
        let mut mark: Option<u32> = None;
        // An open note: id, family, the offset of its callout, and
        // its body's paragraphs.
        let mut note: Option<(String, NoteFamily, u32, Vec<Block>)> = None;
        let mut in_note_body = false;
        loop {
            let ev = reader
                .read_event_into(&mut buf)
                .map_err(|e| OdtError::Xml(part, e))?;
            match &ev {
                Event::Start(e) => {
                    depth += 1;
                    let name = e.name();
                    let name = name.as_ref();
                    if skip > 0 || is_dropped(name) {
                        skip += 1;
                        buf.clear();
                        continue;
                    }
                    match name {
                        // A span begins after any pending space, so
                        // its text starts at its first character.
                        b"text:span" => {
                            spans.push((attr(e, b"text:style-name"), para.text.len() as u32))
                        }
                        b"text:a" => {
                            if let Some(href) = attr(e, b"xlink:href") {
                                let (target, internal) = split_href(&href);
                                link = Some((target, internal, para.text.len() as u32));
                            }
                        }
                        b"text:bookmark-ref" | b"text:reference-ref" => {
                            if let Some(name) = attr(e, b"text:ref-name") {
                                field_ref = Some((name, para.text.len() as u32));
                            }
                        }
                        b"text:bookmark-start" | b"text:reference-mark-start" => {
                            self.bookmark(&mut para, attr(e, b"text:name"));
                        }
                        b"text:alphabetical-index-mark-start" => {
                            mark = Some(para.text.len() as u32)
                        }
                        b"text:note" => {
                            let id = attr(e, b"text:id").unwrap_or_default();
                            let family = match attr(e, b"text:note-class").as_deref() {
                                Some("endnote") => NoteFamily::Endnote,
                                _ => NoteFamily::Footnote,
                            };
                            note = Some((id, family, para.at(), Vec::new()));
                        }
                        b"text:note-citation" => {
                            // The label is presentation; its text
                            // stays out of the prose.
                            skip += 1;
                        }
                        b"text:note-body" => in_note_body = true,
                        b"text:p" | b"text:h" if in_note_body => {
                            // The body's paragraphs, lowered in their
                            // own right (their apparatus too).
                            let inner = self.read_para(reader, e, part)?;
                            depth -= 1;
                            let text = inner.text.trim().to_string();
                            if let Some((_, _, _, blocks)) = note.as_mut() {
                                if !text.is_empty() {
                                    blocks.push(Block::Paragraph { text });
                                }
                                let mut sub = Lower::new(self.styles);
                                sub.emit_apparatus(&inner);
                                blocks.extend(sub.out);
                                self.notes.extend(sub.notes);
                            }
                        }
                        _ => {}
                    }
                }
                Event::Empty(e) if skip == 0 => {
                    let name = e.name();
                    match name.as_ref() {
                        b"text:s" => {
                            let n: usize =
                                attr(e, b"text:c").and_then(|v| v.parse().ok()).unwrap_or(1);
                            para.push(&" ".repeat(n));
                        }
                        b"text:tab" | b"text:line-break" | b"text:soft-page-break" => {
                            para.push(" ")
                        }
                        b"text:bookmark"
                        | b"text:bookmark-start"
                        | b"text:reference-mark"
                        | b"text:reference-mark-start" => {
                            self.bookmark(&mut para, attr(e, b"text:name"));
                        }
                        b"text:alphabetical-index-mark" => {
                            if let Some(term) = attr(e, b"text:string-value") {
                                para.apparatus.push(Apparatus::Mark(term));
                            }
                        }
                        b"text:alphabetical-index-mark-start" => {
                            mark = Some(para.text.len() as u32)
                        }
                        b"text:alphabetical-index-mark-end" => {
                            if let Some(lo) = mark.take() {
                                let term = para.text.get(lo as usize..).unwrap_or("").trim();
                                if !term.is_empty() {
                                    para.apparatus.push(Apparatus::Mark(term.to_string()));
                                }
                            }
                        }
                        b"text:bookmark-ref" | b"text:reference-ref" => {
                            if let Some(name) = attr(e, b"text:ref-name") {
                                para.apparatus
                                    .push(Apparatus::Ref(name, String::new(), true));
                            }
                        }
                        _ => {}
                    }
                }
                Event::Text(t) if skip == 0 && !in_note_body => {
                    if let Ok(s) = t.decode() {
                        para.push(&s);
                    }
                }
                Event::GeneralRef(r) if skip == 0 && !in_note_body => {
                    if let Some(c) = general_ref(r) {
                        para.push(&c);
                    }
                }
                Event::End(e) => {
                    depth -= 1;
                    if skip > 0 {
                        skip -= 1;
                    } else {
                        match e.name().as_ref() {
                            b"text:span" => {
                                if let Some((style, lo)) = spans.pop()
                                    && let Some(style) = style
                                {
                                    let hi = para.at();
                                    if self.styles.code(&style) {
                                        para.apparatus.push(Apparatus::Span("code", lo, hi));
                                    } else if self.styles.emphasis(&style) {
                                        para.apparatus.push(Apparatus::Span("emphasis", lo, hi));
                                    }
                                }
                            }
                            b"text:a" => {
                                if let Some((target, internal, lo)) = link.take() {
                                    let text = para.text.get(lo as usize..).unwrap_or("").trim();
                                    para.apparatus.push(Apparatus::Ref(
                                        target,
                                        text.to_string(),
                                        internal,
                                    ));
                                }
                            }
                            b"text:bookmark-ref" | b"text:reference-ref" => {
                                if let Some((name, lo)) = field_ref.take() {
                                    let text = para.text.get(lo as usize..).unwrap_or("").trim();
                                    para.apparatus.push(Apparatus::Ref(
                                        name,
                                        text.to_string(),
                                        true,
                                    ));
                                }
                            }
                            b"text:alphabetical-index-mark-start" => {
                                if let Some(lo) = mark.take() {
                                    let term = para.text.get(lo as usize..).unwrap_or("").trim();
                                    if !term.is_empty() {
                                        para.apparatus.push(Apparatus::Mark(term.to_string()));
                                    }
                                }
                            }
                            b"text:note-body" => in_note_body = false,
                            b"text:note" => {
                                if let Some((id, family, at, blocks)) = note.take() {
                                    para.apparatus.push(Apparatus::Note(id.clone(), family, at));
                                    self.notes.push((id, family, blocks));
                                }
                            }
                            _ => {}
                        }
                    }
                    if depth == 0 {
                        return Ok(para);
                    }
                }
                Event::Eof => return Ok(para),
                _ => {}
            }
            buf.clear();
        }
    }

    /// A bookmark or reference mark: the block's name when opened
    /// before its first run, a point anchor after.
    fn bookmark(&self, para: &mut Para, name: Option<String>) {
        let Some(name) = name.filter(|n| !n.is_empty()) else {
            return;
        };
        if para.text.trim().is_empty() && para.label.is_none() {
            para.label = Some(name);
        } else {
            para.apparatus.push(Apparatus::Point(name));
        }
    }
}

/// Subtrees that are not prose: generated matter, frames and
/// drawings, annotations, the change record, forms.
fn is_dropped(name: &[u8]) -> bool {
    matches!(
        name,
        b"text:index-body"
            | b"text:index-title"
            | b"text:tracked-changes"
            | b"office:annotation"
            | b"office:annotation-end"
            | b"draw:frame"
            | b"draw:custom-shape"
            | b"draw:g"
            | b"draw:line"
            | b"draw:rect"
            | b"draw:ellipse"
            | b"draw:path"
            | b"draw:polygon"
            | b"draw:polyline"
            | b"draw:connector"
            | b"draw:control"
            | b"office:forms"
            | b"text:page-sequence"
            | b"text:variable-decls"
            | b"text:user-field-decls"
            | b"text:sequence-decls"
            | b"text:dde-connection-decls"
            | b"text:alphabetical-index-auto-mark-file"
    )
}

/// An `xlink:href`: a fragment names an in-document bookmark
/// (LibreOffice spells a heading target `#Name|outline`); the
/// rest is external.
fn split_href(href: &str) -> (String, bool) {
    match href.strip_prefix('#') {
        Some(frag) => (
            frag.split_once('|').map_or(frag, |(f, _)| f).to_string(),
            true,
        ),
        None => (href.to_string(), false),
    }
}

/// Read one `table:table` to its end tag. The declared header rows
/// yield the header vector (the first of them); nested tables
/// flatten into their cell's text.
#[allow(clippy::type_complexity)]
fn read_table(
    reader: &mut Reader<&[u8]>,
    part: &'static str,
) -> Result<(Option<Vec<String>>, Vec<Vec<Cell>>), OdtError> {
    let mut buf = Vec::new();
    let mut depth = 1usize;
    let mut skip = 0usize;
    let mut in_header = false;
    let mut rows: Vec<(bool, Vec<Cell>)> = Vec::new();
    let mut row: Option<(bool, Vec<Cell>)> = None;
    let mut cell: Option<String> = None;
    loop {
        let ev = reader
            .read_event_into(&mut buf)
            .map_err(|e| OdtError::Xml(part, e))?;
        match &ev {
            Event::Start(e) => {
                depth += 1;
                let name = e.name();
                let name = name.as_ref();
                if skip > 0 || is_dropped(name) || name == b"text:note-citation" {
                    skip += 1;
                    buf.clear();
                    continue;
                }
                match name {
                    b"table:table-header-rows" => in_header = true,
                    b"table:table-row" => row = Some((in_header, Vec::new())),
                    b"table:table-cell" => cell = Some(String::new()),
                    b"table:covered-table-cell" => cell = Some(String::new()),
                    _ => {}
                }
            }
            Event::Empty(e) if skip == 0 => match e.name().as_ref() {
                b"table:table-cell" | b"table:covered-table-cell" => {
                    if let Some((_, cells)) = row.as_mut() {
                        cells.push(Cell {
                            text: String::new(),
                            label: None,
                        });
                    }
                }
                b"text:s" | b"text:tab" | b"text:line-break" => {
                    if let Some(c) = cell.as_mut() {
                        c.push(' ');
                    }
                }
                _ => {}
            },
            Event::Text(t) if skip == 0 => {
                if let (Some(c), Ok(s)) = (cell.as_mut(), t.decode()) {
                    c.push_str(&s);
                }
            }
            Event::GeneralRef(r) if skip == 0 => {
                if let (Some(c), Some(s)) = (cell.as_mut(), general_ref(r)) {
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
                        b"table:table-header-rows" => in_header = false,
                        b"table:table-cell" | b"table:covered-table-cell" => {
                            if let (Some((_, cells)), Some(text)) = (row.as_mut(), cell.take()) {
                                cells.push(Cell {
                                    text: quarb_text::normalize_ws(&text),
                                    label: None,
                                });
                            }
                        }
                        b"table:table-row" => {
                            if let Some(r) = row.take() {
                                rows.push(r);
                            }
                        }
                        // A paragraph boundary inside a cell is a
                        // word boundary in its flattened text.
                        b"text:p" | b"text:h" => {
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
                    // Further declared header rows are presentation.
                    return Ok((
                        headers,
                        rows.into_iter()
                            .filter(|(h, _)| !h)
                            .map(|(_, r)| r)
                            .collect(),
                    ));
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

/// A general entity reference as its text: the predefined five and
/// numeric character references.
fn general_ref(r: &quick_xml::events::BytesRef) -> Option<String> {
    let name = String::from_utf8_lossy(r.as_ref());
    match name.as_ref() {
        "amp" => Some("&".into()),
        "lt" => Some("<".into()),
        "gt" => Some(">".into()),
        "quot" => Some("\"".into()),
        "apos" => Some("'".into()),
        n => {
            let code = n
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| n.strip_prefix('#').and_then(|d| d.parse().ok()))?;
            char::from_u32(code).map(|c| c.to_string())
        }
    }
}
