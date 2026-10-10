//! The Uniform Office Format text-level adapter: `.uot` bytes in
//! (GB/T 20916-2007, UOF 1.0 — one XML document; UOF 2.0 — the same
//! vocabulary in a zip package), the reader's model out —
//! `text:report.uot` beside `text:report.docx` and `text:report.odt`.
//! The format's mark is its Chinese element names: a paragraph is
//! `字:段落` in the word-processing namespace `字:`, and this reader
//! matches the names as written, whichever prefix a file binds.
//!
//! Everything derives from *declared* structure, never from style
//! names or heuristics:
//!
//! - A heading is a paragraph whose properties (`字:段落属性`)
//!   declare an outline level (`字:大纲级别`, 1-based), or whose
//!   paragraph style (`@字:式样引用` into `uof:式样集/uof:段落式样`)
//!   does; the level is the heading's depth.
//! - A list is the run of paragraphs whose properties name an
//!   automatic numbering (`字:自动编号信息/@字:编号引用`): ordered
//!   unless the numbering's first level declares a bullet
//!   (`字:项目符号`); nested by the declared level (`@字:级别`,
//!   0-based) where a file gives one.
//! - A table (`字:文字表`) lowers through the shared [`Block::Table`]
//!   rule: `字:行` rows, `字:单元格` cells (their paragraphs joined),
//!   the rows declared `字:表头行` the header.
//! - Notes are first class: `字:脚注` and `字:尾注` inside a run are a
//!   callout at their offset in the paragraph (`<deixis>`) and a
//!   body at the document end (`<note>`), the body's paragraphs
//!   its own.
//! - A hyperlink is a region (`字:区域开始 @字:类型="hyperlink"`)
//!   whose identifier the document's link set resolves
//!   (`uof:链接集/uof:超级链接 @uof:链源` → `@uof:链宿`): a `ref` with
//!   the region's text. A bookmark region names a point anchor,
//!   by its own name or the bookmark set's (`uof:书签集`).
//! - Tracked changes read as the accepted view: an insertion
//!   region's text is text, a deletion region's is dropped.
//! - Not carried: anchored objects (`字:锚点` and what hangs on
//!   it), fields other than their cached text, comments
//!   (`字:批注`), headers and footers, and formatting.
//! - `uof:元数据` declares the identity: `uof:标题`, `uof:创建者`,
//!   `uof:主题`, `uof:摘要`, `uof:关键字集/uof:关键字`, `uof:创建日期`,
//!   `uof:编辑时间`, under the HTML head's keys.
//!
//! The spreadsheet (`表:`) and presentation (`演:`) modules are not
//! read; the plain mount of a `.uot` keeps its XML.
use std::collections::HashMap;
use std::io::Read;

use quarb::Value;
use quarb::source::SourceDecoding;
use quarb_text::{Block, Cell, Container, HeadMeta, NoteFamily, TextModel};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

#[derive(Debug, thiserror::Error)]
pub enum UofError {
    #[error("not a UOF document: {0}")]
    Malformed(String),
    #[error("{0}: {1}")]
    Xml(&'static str, quick_xml::Error),
    #[error("not a UOF package: {0}")]
    Zip(#[from] zip::result::ZipError),
}

/// Whether the bytes are a UOF document: an XML whose root is
/// `uof:UOF` (any prefix), or a zip package holding one.
pub fn is_uof(bytes: &[u8]) -> bool {
    document_xml(bytes).is_ok_and(|x| root_is_uof(&x))
}

fn root_is_uof(xml: &str) -> bool {
    let head = &xml[..xml.len().min(4096)];
    head.contains(":UOF") || head.contains("<UOF")
}

/// The document's XML: the file itself, or the member of a UOF 2.0
/// package whose root is `UOF`.
fn document_xml(bytes: &[u8]) -> Result<String, UofError> {
    if bytes.starts_with(b"PK") {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        let names: Vec<String> = zip.file_names().map(str::to_string).collect();
        let mut candidates: Vec<&String> = names
            .iter()
            .filter(|n| n.ends_with(".xml") || n.ends_with(".uot"))
            .collect();
        candidates.sort_by_key(|n| {
            let base = n.rsplit('/').next().unwrap_or(n);
            match base {
                "uof.xml" | "content.xml" | "document.xml" => 0,
                _ => 1,
            }
        });
        for name in candidates {
            let mut f = zip.by_name(name)?;
            let mut s = String::new();
            if f.read_to_string(&mut s).is_ok() && root_is_uof(&s) {
                return Ok(s);
            }
        }
        return Err(UofError::Malformed(
            "the package holds no UOF document".into(),
        ));
    }
    let s = String::from_utf8_lossy(bytes).into_owned();
    if !root_is_uof(&s) {
        return Err(UofError::Malformed("the root is not uof:UOF".into()));
    }
    Ok(s)
}

/// Parse `.uot` bytes into the text-level model, with what the
/// metadata declares as the head.
pub fn parse(bytes: &[u8]) -> Result<TextModel, UofError> {
    let doc = Doc::read(&document_xml(bytes)?)?;
    let mut model = TextModel::build(doc.blocks);
    model.set_head_meta(doc.head);
    Ok(model)
}

/// [`parse`] under a mount's rule; a UOF document is UTF-8 XML by
/// its own declaration.
pub fn parse_with_decoding(
    bytes: &[u8],
    _decoding: &SourceDecoding,
) -> Result<TextModel, UofError> {
    parse(bytes)
}

/// The block event stream (the [`TextModel::build`] input).
pub fn blocks(bytes: &[u8]) -> Result<Vec<Block>, UofError> {
    Ok(Doc::read(&document_xml(bytes)?)?.blocks)
}

/// What the document declares about itself.
pub fn head(bytes: &[u8]) -> Result<HeadMeta, UofError> {
    Ok(Doc::read(&document_xml(bytes)?)?.head)
}

fn local(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b':') {
        Some(i) => &name[i + 1..],
        None => name,
    }
}

fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| local(a.key.as_ref()) == name.as_bytes())
        .map(|a| String::from_utf8_lossy(&a.value).into_owned())
}

fn text_of(t: &quick_xml::events::BytesText) -> String {
    t.xml_content()
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| String::from_utf8_lossy(t).into_owned())
}

// ---------------------------------------------------------------
// The document
// ---------------------------------------------------------------

/// One inline attachment of a paragraph, in order.
enum Apparatus {
    Note(String, NoteFamily, u32),
    Ref(String, String, bool),
    Anchor(String),
}

struct Para {
    outline: Option<u8>,
    list: Option<(String, u8)>,
    text: String,
    apparatus: Vec<Apparatus>,
}

/// What a document declares once, read before the body: styles'
/// outline levels, numberings' kinds, the link and bookmark sets.
#[derive(Default)]
struct Sets {
    style_outline: HashMap<String, u8>,
    /// Numbering id → unordered (a bullet at its first level).
    numbering_bullet: HashMap<String, bool>,
    /// Hyperlink region id → target.
    links: HashMap<String, String>,
    /// Bookmark region id → name.
    bookmarks: HashMap<String, String>,
}

struct Doc {
    blocks: Vec<Block>,
    head: HeadMeta,
}

impl Doc {
    fn read(xml: &str) -> Result<Doc, UofError> {
        let sets = read_sets(xml)?;
        let head = read_head(xml)?;
        let mut lower = Lower::new(&sets);
        lower.body(xml)?;
        lower.finish();
        let mut blocks = lower.out;
        for (onym, family, paras) in lower.notes {
            blocks.push(Block::Open {
                kind: Container::Note {
                    onym,
                    family,
                    margin: false,
                },
                lemma: None,
            });
            blocks.extend(paras);
            blocks.push(Block::Close { hypograph: None });
        }
        Ok(Doc { blocks, head })
    }
}

/// `uof:元数据`.
fn read_head(xml: &str) -> Result<HeadMeta, UofError> {
    let mut reader = Reader::from_str(xml);
    let mut h = HeadMeta::default();
    let mut in_meta = false;
    let mut open: Option<String> = None;
    loop {
        match reader
            .read_event()
            .map_err(|e| UofError::Xml("metadata", e))?
        {
            Event::Start(e) => {
                let qname = e.name();
                let name = String::from_utf8_lossy(local(qname.as_ref())).into_owned();
                if name == "元数据" {
                    in_meta = true;
                } else if in_meta {
                    open = Some(name);
                } else if name == "文字处理" || name == "主体" {
                    break;
                }
            }
            Event::Text(t) if in_meta => {
                if let Some(name) = &open {
                    let value = text_of(&t);
                    let value = value.trim();
                    if value.is_empty() {
                        continue;
                    }
                    let key = match name.as_str() {
                        "标题" => "title",
                        "创建者" => "creator",
                        "主题" => "subject",
                        "摘要" => "description",
                        "关键字" => "keyword",
                        "创建日期" => "created",
                        "编辑时间" => "modified",
                        "最后作者" => "last-author",
                        "语言" => "language",
                        other => other,
                    };
                    h.declare(key, value);
                    match key {
                        "title" => h.title = Some(value.to_string()),
                        "creator" => h.author = Some(value.to_string()),
                        "description" => h.description = Some(value.to_string()),
                        "subject" if h.category.is_none() => h.category = Some(value.to_string()),
                        "keyword" => {
                            for k in value.split([';', ',']) {
                                let k = k.trim();
                                if !k.is_empty() && !h.tags.iter().any(|t| t == k) {
                                    h.tags.push(k.to_string());
                                }
                            }
                        }
                        "created" => h.published = Some(Value::Str(value.to_string())),
                        "modified" => h.modified = Some(Value::Str(value.to_string())),
                        _ => {}
                    }
                }
            }
            Event::End(e) => {
                let qname = e.name();
                let name = local(qname.as_ref());
                if name == "元数据".as_bytes() {
                    break;
                }
                open = None;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(h)
}

/// The style set's outline levels, the numberings' kinds, the link
/// set and the bookmark set.
fn read_sets(xml: &str) -> Result<Sets, UofError> {
    let mut reader = Reader::from_str(xml);
    let mut sets = Sets::default();
    let mut stack: Vec<String> = Vec::new();
    let mut style: Option<String> = None;
    let mut numbering: Option<String> = None;
    let mut first_level_seen = false;
    let mut bookmark: Option<String> = None;
    loop {
        let ev = reader.read_event().map_err(|e| UofError::Xml("sets", e))?;
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(ev, Event::Empty(_));
                let qname = e.name();
                let name = String::from_utf8_lossy(local(qname.as_ref())).into_owned();
                match name.as_str() {
                    "段落式样" => style = attr(e, "标识符"),
                    "自动编号" => {
                        numbering = attr(e, "标识符");
                        first_level_seen = false;
                    }
                    "级别" if numbering.is_some() && !first_level_seen => {
                        first_level_seen = true;
                        // The first level's kind, read as its children
                        // arrive (below).
                        if let Some(id) = &numbering {
                            sets.numbering_bullet.entry(id.clone()).or_insert(false);
                        }
                        stack.push(name);
                        if empty {
                            stack.pop();
                        }
                        continue;
                    }
                    "项目符号" => {
                        if let Some(id) = &numbering
                            && stack.iter().any(|s| s == "级别")
                            && !sets.numbering_bullet.get(id).copied().unwrap_or(false)
                            && stack.iter().filter(|s| *s == "级别").count() == 1
                            && first_level_only(&stack)
                        {
                            sets.numbering_bullet.insert(id.clone(), true);
                        }
                    }
                    "超级链接" => {
                        if let (Some(src), Some(dst)) = (attr(e, "链源"), attr(e, "链宿")) {
                            sets.links.insert(src, dst);
                        }
                    }
                    "书签" => bookmark = attr(e, "名称"),
                    "文本位置" => {
                        if let (Some(name), Some(id)) = (&bookmark, attr(e, "区域引用")) {
                            sets.bookmarks.insert(id, name.clone());
                        }
                    }
                    "主体" => break,
                    _ => {}
                }
                if !empty {
                    stack.push(name);
                }
            }
            Event::Text(t) => {
                if stack.last().is_some_and(|s| s == "大纲级别")
                    && stack.iter().any(|s| s == "段落式样")
                    && let Some(id) = &style
                    && let Ok(level) = text_of(&t).trim().parse::<u8>()
                {
                    sets.style_outline.insert(id.clone(), level);
                }
            }
            Event::End(e) => {
                let qname = e.name();
                let name = local(qname.as_ref());
                if name == "段落式样".as_bytes() {
                    style = None;
                }
                if name == "自动编号".as_bytes() {
                    numbering = None;
                }
                if name == "书签".as_bytes() {
                    bookmark = None;
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(sets)
}

/// Inside the first `级别` of a numbering: the stack holds one.
fn first_level_only(stack: &[String]) -> bool {
    stack.iter().filter(|s| *s == "级别").count() == 1
}

struct Lower<'a> {
    sets: &'a Sets,
    out: Vec<Block>,
    notes: Vec<(String, NoteFamily, Vec<Block>)>,
    note_count: usize,
    /// The open lists, innermost last: (numbering id, an item open).
    lists: Vec<(String, bool)>,
}

impl<'a> Lower<'a> {
    fn new(sets: &'a Sets) -> Self {
        Lower {
            sets,
            out: Vec::new(),
            notes: Vec::new(),
            note_count: 0,
            lists: Vec::new(),
        }
    }

    /// Lower the body: every `字:段落` and `字:文字表` under `字:主体`.
    fn body(&mut self, xml: &str) -> Result<(), UofError> {
        let mut reader = Reader::from_str(xml);
        let mut in_body = false;
        loop {
            let ev = reader.read_event().map_err(|e| UofError::Xml("body", e))?;
            match ev {
                Event::Start(e) => {
                    let qname = e.name();
                    let name = local(qname.as_ref());
                    if !in_body {
                        if name == "主体".as_bytes() {
                            in_body = true;
                        }
                        continue;
                    }
                    match name {
                        b if b == "段落".as_bytes() => {
                            let para = self.read_para(&mut reader, &e)?;
                            self.emit_para(para);
                        }
                        b if b == "文字表".as_bytes() => {
                            let (headers, rows) = self.read_table(&mut reader)?;
                            if !rows.is_empty() || headers.is_some() {
                                self.close_lists();
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
                Event::End(e) => {
                    if in_body && local(e.name().as_ref()) == "主体".as_bytes() {
                        break;
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        Ok(())
    }

    /// One `字:段落` to its end: its runs' text, the regions and
    /// notes met on the way.
    fn read_para(
        &mut self,
        reader: &mut Reader<&[u8]>,
        start: &BytesStart,
    ) -> Result<Para, UofError> {
        let mut para = Para {
            outline: None,
            list: None,
            text: String::new(),
            apparatus: Vec::new(),
        };
        let _ = start;
        let mut stack: Vec<String> = vec!["段落".to_string()];
        let mut deleting = 0usize;
        // Open regions: (kind, id, name, text start).
        let mut regions: Vec<(String, String, Option<String>, usize)> = Vec::new();
        let mut style_ref: Option<String> = None;
        loop {
            let ev = reader
                .read_event()
                .map_err(|e| UofError::Xml("paragraph", e))?;
            match ev {
                Event::Start(ref e) | Event::Empty(ref e) => {
                    let empty = matches!(ev, Event::Empty(_));
                    let qname = e.name();
                    let name = String::from_utf8_lossy(local(qname.as_ref())).into_owned();
                    match name.as_str() {
                        "段落属性" => style_ref = attr(e, "式样引用"),
                        "自动编号信息" => {
                            if let Some(id) = attr(e, "编号引用") {
                                let level =
                                    attr(e, "级别").and_then(|l| l.parse().ok()).unwrap_or(0);
                                para.list = Some((id, level));
                            }
                        }
                        "空格符" => para.text.push(' '),
                        "换行符" => para.text.push(' '),
                        "制表符" => para.text.push('\t'),
                        "区域开始" => {
                            let kind = attr(e, "类型").unwrap_or_default();
                            let id = attr(e, "标识符").unwrap_or_default();
                            let name = attr(e, "名称");
                            regions.push((kind, id, name, para.text.len()));
                        }
                        "区域结束" => {
                            let closed = attr(e, "标识符")
                                .and_then(|id| regions.iter().rposition(|r| r.1 == id))
                                .or_else(|| (!regions.is_empty()).then_some(regions.len() - 1));
                            if let Some(i) = closed {
                                let (kind, id, name, start) = regions.remove(i);
                                let text =
                                    para.text[start.min(para.text.len())..].trim().to_string();
                                match kind.as_str() {
                                    "hyperlink" => {
                                        if let Some(target) = self.sets.links.get(&id).cloned() {
                                            let internal = target.starts_with('#');
                                            para.apparatus
                                                .push(Apparatus::Ref(target, text, internal));
                                        }
                                    }
                                    "bookmark" => {
                                        let name =
                                            name.or_else(|| self.sets.bookmarks.get(&id).cloned());
                                        if let Some(n) = name.filter(|n| !n.is_empty()) {
                                            para.apparatus.push(Apparatus::Anchor(n));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        "修订开始" => {
                            if attr(e, "类型").as_deref() == Some("delete") {
                                deleting += 1;
                            }
                        }
                        "修订结束" => {
                            // The end of the innermost deletion, when one is open.
                            if deleting > 0 {
                                deleting -= 1;
                            }
                        }
                        "脚注" | "尾注" => {
                            let family = if name == "脚注" {
                                NoteFamily::Footnote
                            } else {
                                NoteFamily::Endnote
                            };
                            self.note_count += 1;
                            let onym = format!("n{}", self.note_count);
                            let body = self.read_note(reader, &name)?;
                            let at = para.text.trim_end().len() as u32;
                            para.apparatus
                                .push(Apparatus::Note(onym.clone(), family, at));
                            self.notes.push((onym, family, body));
                            continue;
                        }
                        // Anchored objects and comments are not prose.
                        "锚点" | "批注" | "域代码" => {
                            if !empty {
                                skip_to_end(reader, &name)?;
                            }
                            continue;
                        }
                        _ => {}
                    }
                    if !empty {
                        stack.push(name);
                    }
                }
                Event::Text(t) => {
                    let top = stack.last().map(String::as_str).unwrap_or("");
                    if deleting == 0 {
                        match top {
                            "文本串" => para.text.push_str(&text_of(&t)),
                            "大纲级别" if stack.iter().any(|s| s == "段落属性") => {
                                if let Ok(l) = text_of(&t).trim().parse::<u8>() {
                                    para.outline = Some(l);
                                }
                            }
                            _ => {}
                        }
                    } else if top == "大纲级别"
                        && let Ok(l) = text_of(&t).trim().parse::<u8>()
                    {
                        para.outline = Some(l);
                    }
                }
                Event::End(e) => {
                    let qname = e.name();
                    let name = local(qname.as_ref());
                    if name == "段落".as_bytes() && stack.len() == 1 {
                        break;
                    }
                    stack.pop();
                }
                Event::Eof => break,
                _ => {}
            }
        }
        if para.outline.is_none()
            && let Some(s) = &style_ref
            && let Some(level) = self.sets.style_outline.get(s)
        {
            para.outline = Some(*level);
        }
        Ok(para)
    }

    /// A note's body: its paragraphs as blocks.
    fn read_note(
        &mut self,
        reader: &mut Reader<&[u8]>,
        name: &str,
    ) -> Result<Vec<Block>, UofError> {
        let mut out = Vec::new();
        loop {
            let ev = reader.read_event().map_err(|e| UofError::Xml("note", e))?;
            match ev {
                Event::Start(e) => {
                    if local(e.name().as_ref()) == "段落".as_bytes() {
                        let p = self.read_para(reader, &e)?;
                        let text = p.text.trim().to_string();
                        if !text.is_empty() {
                            out.push(Block::Paragraph { text });
                        }
                    }
                }
                Event::End(e) => {
                    if local(e.name().as_ref()) == name.as_bytes() {
                        break;
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        Ok(out)
    }

    /// `字:文字表`: rows of cells, the header rows declared.
    fn read_table(
        &mut self,
        reader: &mut Reader<&[u8]>,
    ) -> Result<(Option<Vec<String>>, Vec<Vec<Cell>>), UofError> {
        let mut rows: Vec<(bool, Vec<Cell>)> = Vec::new();
        let mut depth = 0usize; // nested tables are read as their cells' text
        loop {
            let ev = reader.read_event().map_err(|e| UofError::Xml("table", e))?;
            match ev {
                Event::Start(ref e) | Event::Empty(ref e) => {
                    let empty = matches!(ev, Event::Empty(_));
                    let qname = e.name();
                    let name = local(qname.as_ref());
                    if name == "文字表".as_bytes() && !empty {
                        depth += 1;
                        continue;
                    }
                    if depth > 0 {
                        continue;
                    }
                    if name == "行".as_bytes() {
                        rows.push((false, Vec::new()));
                    } else if name == "表头行".as_bytes() {
                        if let Some((header, _)) = rows.last_mut() {
                            *header = attr(e, "值").as_deref() == Some("true");
                        }
                    } else if name == "单元格".as_bytes() {
                        let text = self.read_cell(reader)?;
                        if let Some((_, cells)) = rows.last_mut() {
                            cells.push(Cell::from(text.as_str()));
                        }
                    }
                }
                Event::End(e) => {
                    if local(e.name().as_ref()) == "文字表".as_bytes() {
                        if depth > 0 {
                            depth -= 1;
                            continue;
                        }
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

    /// `字:单元格`: its paragraphs joined by a space.
    fn read_cell(&mut self, reader: &mut Reader<&[u8]>) -> Result<String, UofError> {
        let mut texts = Vec::new();
        loop {
            let ev = reader.read_event().map_err(|e| UofError::Xml("cell", e))?;
            match ev {
                Event::Start(e) => {
                    if local(e.name().as_ref()) == "段落".as_bytes() {
                        let p = self.read_para(reader, &e)?;
                        let t = p.text.trim().to_string();
                        if !t.is_empty() {
                            texts.push(t);
                        }
                    }
                }
                Event::End(e) => {
                    if local(e.name().as_ref()) == "单元格".as_bytes() {
                        break;
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        Ok(texts.join(" "))
    }

    fn emit_para(&mut self, para: Para) {
        let text = para.text.trim().to_string();
        if let Some(level) = para.outline.filter(|l| *l >= 1) {
            self.close_lists();
            if !text.is_empty() {
                self.out.push(Block::Heading { level, lemma: text });
            }
            self.emit_apparatus(&para);
            return;
        }
        if let Some((id, level)) = &para.list {
            self.enter_list(id, *level as usize + 1);
            if !text.is_empty() {
                self.out.push(Block::Text { text });
            }
            self.emit_apparatus(&para);
            return;
        }
        self.close_lists();
        if !text.is_empty() {
            self.out.push(Block::Paragraph { text });
        }
        self.emit_apparatus(&para);
    }

    fn enter_list(&mut self, id: &str, depth: usize) {
        while self.lists.len() > depth {
            self.close_list();
        }
        if self.lists.len() == depth && self.lists.last().is_some_and(|(i, _)| i != id) {
            self.close_list();
        }
        while self.lists.len() < depth {
            if let Some((_, open)) = self.lists.last_mut()
                && !*open
            {
                self.out.push(Block::Open {
                    kind: Container::Item,
                    lemma: None,
                });
                *open = true;
            }
            let kind = if self.sets.numbering_bullet.get(id).copied().unwrap_or(false) {
                Container::UnorderedList
            } else {
                Container::OrderedList { start: 1 }
            };
            self.out.push(Block::Open { kind, lemma: None });
            self.lists.push((id.to_string(), false));
        }
        if let Some((_, open)) = self.lists.last_mut() {
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
        if let Some((_, open)) = self.lists.pop() {
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

fn skip_to_end(reader: &mut Reader<&[u8]>, name: &str) -> Result<(), UofError> {
    let mut depth = 1usize;
    loop {
        match reader.read_event().map_err(|e| UofError::Xml("skip", e))? {
            Event::Start(e) if local(e.name().as_ref()) == name.as_bytes() => depth += 1,
            Event::End(e) if local(e.name().as_ref()) == name.as_bytes() => {
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
