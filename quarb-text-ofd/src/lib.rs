//! The reader's model of an OFD — `text:invoice.ofd` — built from
//! declared structure only, by the print reading's doctrine (the
//! PDF pair, `quarb-text-pdf`): what the file says, never a guess.
//!
//! OFD (GB/T 33190-2016, Open Fixed-layout Document) is a zip of
//! XML in the `http://www.ofdspec.org/2016` namespace: `OFD.xml`
//! names the documents (`DocBody`, with `DocInfo` and `DocRoot`),
//! a document's `Document.xml` its pages, outline, template pages
//! and attachments, a page's `Content.xml` the layers of text,
//! path and image objects placed on it.
//!
//! - **Sections from the outline** (`Outlines/OutlineElem`), where
//!   there is one: an entry's `Title` is the section's `::lemma`,
//!   its nesting the tree, its extent the text from its
//!   destination (`Goto/Dest` by `PageID` and `Top`, or a named
//!   `Bookmark`) to the next entry's. Text before the first
//!   destination stays at the root. Without an outline the tree is
//!   `page/*` and what lies beneath — every page the document
//!   lists, with or without text.
//! - **Lines with geometry**: a `line` groups the text codes a
//!   page shows at exactly the same declared baseline (the
//!   object's boundary `y` plus the code's `Y`, no tolerance), in
//!   `x` order; a gap between two codes' declared extents reads as
//!   a space. `:::page` and the line's bounding box `:::x`,
//!   `:::y`, `:::width`, `:::height` in the page's units
//!   (millimetres, top-left origin), and `:::baseline`. A
//!   `paragraph` is never minted — the format declares none.
//! - A page's template pages (`Template`, drawn beneath it) are
//!   part of what the page shows: their text joins the page's
//!   lines. A text code's characters are the text (the format
//!   carries them beside the glyphs); fonts, paths and images are
//!   outside the text level.
//! - `DocInfo` is the declared identity: title, author, subject,
//!   abstract, keywords, the dates, the creator, and every custom
//!   datum under its own name, as properties of the document node.
//!
//! Attachments, annotations and signatures are members of the
//! package and read under the plain mount (the archive adapter
//! grafts them); verifying a signature is outside the reading.
use std::collections::HashMap;
use std::io::Read;

use quarb::{AstAdapter, NodeId, Value};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

#[derive(Debug, thiserror::Error)]
pub enum OfdTextError {
    #[error("not an OFD package: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("not an OFD package: {0}")]
    Malformed(String),
}

// ---------------------------------------------------------------
// The extracted model
// ---------------------------------------------------------------

/// One text code as placed: its baseline and left edge on the
/// page, its declared extent, its characters.
#[derive(Debug, Clone)]
struct Code {
    page: u32,
    baseline: f64,
    x: f64,
    right: f64,
    top: f64,
    bottom: f64,
    text: String,
}

#[derive(Debug, Clone)]
struct Line {
    page: u32,
    baseline: f64,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    text: String,
}

#[derive(Debug)]
struct OutlineEntry {
    level: u8,
    title: String,
    /// (page number, top) — `None` for a page-granular fit.
    dest: (u32, Option<f64>),
}

#[derive(Debug)]
enum Node {
    Root {
        children: Vec<usize>,
    },
    Section {
        lemma: String,
        level: u8,
        parent: usize,
        children: Vec<usize>,
    },
    Page {
        /// 1-based, as a reader counts.
        index: u32,
        width: f64,
        height: f64,
        parent: usize,
        children: Vec<usize>,
    },
    Line {
        line: Line,
        parent: usize,
    },
}

#[derive(Debug)]
pub struct OfdText {
    nodes: Vec<Node>,
    /// `DocInfo`, as (key, value): the declared identity.
    identity: Vec<(String, String)>,
    n_pages: usize,
}

/// Whether the bytes are a zip holding an `OFD.xml` entry — the
/// package under any extension.
pub fn is_ofd_package(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK")
        && zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .is_ok_and(|mut z| z.by_name("OFD.xml").is_ok())
}

/// Read an OFD package from its bytes.
pub fn parse(bytes: &[u8]) -> Result<OfdText, OfdTextError> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    let entry = member(&mut zip, "OFD.xml")?
        .ok_or_else(|| OfdTextError::Malformed("no OFD.xml entry".into()))?;
    let (identity, doc_roots) = read_entry(&entry)?;
    let mut pages: Vec<PageSpec> = Vec::new();
    let mut outline: Vec<OutlineEntry> = Vec::new();
    for root in &doc_roots {
        let root = resolve("", root);
        let Some(doc_xml) = member(&mut zip, &root)? else {
            return Err(OfdTextError::Malformed(format!(
                "DocRoot {root} is not in the package"
            )));
        };
        let base = dir_of(&root);
        let first = pages.len() as u32;
        let d = read_document(&doc_xml, &base, first)?;
        pages.extend(d.pages);
        outline.extend(d.outline);
    }
    // The text codes, page by page: the templates a page draws
    // beneath itself, then its own content.
    let mut codes: Vec<Code> = Vec::new();
    let mut page_sizes: Vec<(f64, f64)> = Vec::new();
    for p in &pages {
        let Some(xml) = member(&mut zip, &p.path)? else {
            return Err(OfdTextError::Malformed(format!(
                "page {} is not in the package",
                p.path
            )));
        };
        let content = read_page(&xml, p.number)?;
        for t in &content.templates {
            if let Some(path) = p.templates.get(t)
                && let Some(txml) = member(&mut zip, path)?
            {
                codes.extend(read_page(&txml, p.number)?.codes);
            }
        }
        page_sizes.push(content.size.unwrap_or(p.default_size));
        codes.extend(content.codes);
    }
    let mut lines = assemble_lines(codes);
    Ok(assemble(&mut lines, outline, &page_sizes, identity))
}

struct PageSpec {
    number: u32,
    path: String,
    default_size: (f64, f64),
    /// Template id → the template page's content path.
    templates: HashMap<String, String>,
}

struct DocumentSpec {
    pages: Vec<PageSpec>,
    outline: Vec<OutlineEntry>,
}

struct PageContent {
    size: Option<(f64, f64)>,
    templates: Vec<String>,
    codes: Vec<Code>,
}

// ---------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------

/// The codes of one page at one declared baseline, in `x` order,
/// are a line; a gap between a code's declared right edge and the
/// next code's left edge reads as a space.
fn assemble_lines(mut codes: Vec<Code>) -> Vec<Line> {
    codes.sort_by(|a, b| {
        (a.page, ordf(a.baseline), ordf(a.x))
            .partial_cmp(&(b.page, ordf(b.baseline), ordf(b.x)))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut lines: Vec<Line> = Vec::new();
    let mut last_right = f64::NAN;
    for c in codes {
        let joins = lines
            .last()
            .is_some_and(|l| l.page == c.page && l.baseline.to_bits() == c.baseline.to_bits());
        if joins {
            let l = lines.last_mut().expect("joins");
            if c.x - last_right > 0.01
                && !l.text.ends_with(char::is_whitespace)
                && !c.text.starts_with(char::is_whitespace)
            {
                l.text.push(' ');
            }
            l.text.push_str(&c.text);
            let right = (l.x + l.width).max(c.right);
            let bottom = (l.y + l.height).max(c.bottom);
            l.x = l.x.min(c.x);
            l.y = l.y.min(c.top);
            l.width = right - l.x;
            l.height = bottom - l.y;
        } else {
            lines.push(Line {
                page: c.page,
                baseline: c.baseline,
                x: c.x,
                y: c.top,
                width: c.right - c.x,
                height: c.bottom - c.top,
                text: c.text.clone(),
            });
        }
        last_right = c.right;
    }
    lines
}

/// Document order for boundary comparison: a later page, or lower
/// on the same page (OFD's `y` grows downward).
fn at_or_after(line: &Line, dest: &(u32, Option<f64>)) -> bool {
    match dest {
        (p, None) => line.page >= *p,
        (p, Some(top)) => line.page > *p || (line.page == *p && line.baseline >= *top),
    }
}

fn assemble(
    lines: &mut Vec<Line>,
    outline: Vec<OutlineEntry>,
    page_sizes: &[(f64, f64)],
    identity: Vec<(String, String)>,
) -> OfdText {
    let n_pages = page_sizes.len();
    let mut nodes = vec![Node::Root {
        children: Vec::new(),
    }];
    if outline.is_empty() {
        // Every listed page, its lines beneath it.
        let mut line_iter = lines.drain(..).peekable();
        for (i, &(width, height)) in page_sizes.iter().enumerate() {
            let number = i as u32 + 1;
            let pn = nodes.len();
            nodes.push(Node::Page {
                index: number,
                width,
                height,
                parent: 0,
                children: Vec::new(),
            });
            if let Node::Root { children } = &mut nodes[0] {
                children.push(pn);
            }
            while line_iter.peek().is_some_and(|l| l.page == number) {
                let line = line_iter.next().expect("peeked");
                let ln = nodes.len();
                nodes.push(Node::Line { line, parent: pn });
                if let Node::Page { children, .. } = &mut nodes[pn] {
                    children.push(ln);
                }
            }
        }
        return OfdText {
            nodes,
            identity,
            n_pages,
        };
    }
    // Sections in outline order; each takes the lines from its
    // destination up to the next entry's. Front matter stays at
    // the root. Nesting follows the entry levels.
    let mut line_iter = lines.drain(..).peekable();
    let mut take_until =
        |nodes: &mut Vec<Node>, parent: usize, stop: Option<&(u32, Option<f64>)>| {
            while let Some(line) = line_iter.peek() {
                if let Some(stop) = stop
                    && at_or_after(line, stop)
                {
                    break;
                }
                let line = line_iter.next().expect("peeked");
                let ln = nodes.len();
                nodes.push(Node::Line { line, parent });
                match &mut nodes[parent] {
                    Node::Root { children }
                    | Node::Section { children, .. }
                    | Node::Page { children, .. } => children.push(ln),
                    Node::Line { .. } => unreachable!(),
                }
            }
        };
    take_until(&mut nodes, 0, Some(&outline[0].dest));
    let mut stack: Vec<(u8, usize)> = Vec::new();
    for (i, entry) in outline.iter().enumerate() {
        while stack.last().is_some_and(|(l, _)| *l >= entry.level) {
            stack.pop();
        }
        let parent = stack.last().map_or(0, |(_, n)| *n);
        let sn = nodes.len();
        nodes.push(Node::Section {
            lemma: entry.title.clone(),
            level: entry.level,
            parent,
            children: Vec::new(),
        });
        match &mut nodes[parent] {
            Node::Root { children } | Node::Section { children, .. } => children.push(sn),
            _ => unreachable!(),
        }
        stack.push((entry.level, sn));
        take_until(&mut nodes, sn, outline.get(i + 1).map(|e| &e.dest));
    }
    OfdText {
        nodes,
        identity,
        n_pages,
    }
}

fn ordf(f: f64) -> ordered::F {
    ordered::F(f)
}

mod ordered {
    #[derive(PartialEq, PartialOrd)]
    pub struct F(pub f64);
}

// ---------------------------------------------------------------
// The package's XML
// ---------------------------------------------------------------

fn member(
    zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    name: &str,
) -> Result<Option<String>, OfdTextError> {
    let Ok(mut f) = zip.by_name(name) else {
        return Ok(None);
    };
    let mut s = String::new();
    f.read_to_string(&mut s)
        .map_err(|e| OfdTextError::Malformed(format!("{name}: {e}")))?;
    Some(s).map(Ok).transpose()
}

/// A package location against the directory of the file naming
/// it: a leading `/` is the package root, `./` is dropped.
fn resolve(base: &str, loc: &str) -> String {
    let loc = loc.trim();
    if let Some(abs) = loc.strip_prefix('/') {
        return abs.to_string();
    }
    let loc = loc.strip_prefix("./").unwrap_or(loc);
    if base.is_empty() {
        loc.to_string()
    } else {
        format!("{base}/{loc}")
    }
}

fn dir_of(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..i].to_string(),
        None => String::new(),
    }
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

fn unescape(t: &quick_xml::events::BytesText) -> String {
    t.xml_content()
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| String::from_utf8_lossy(t).into_owned())
}

/// `x y w h` as four numbers.
fn box4(s: &str) -> Option<[f64; 4]> {
    let mut it = s.split_whitespace().map(|n| n.parse::<f64>().ok());
    Some([it.next()??, it.next()??, it.next()??, it.next()??])
}

/// `OFD.xml`: the identity from the first `DocInfo`, every custom
/// datum under its name, and the `DocRoot` of each document.
fn read_entry(xml: &str) -> Result<(Vec<(String, String)>, Vec<String>), OfdTextError> {
    let mut reader = Reader::from_str(xml);
    let mut identity: Vec<(String, String)> = Vec::new();
    let mut roots = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut custom: Option<String> = None;
    let mut keywords: Vec<String> = Vec::new();
    let mut docs = 0usize;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(local(e.name().as_ref())).into_owned();
                if name == "DocBody" {
                    docs += 1;
                }
                if name == "CustomData" {
                    custom = attr(&e, b"Name");
                }
                stack.push(name);
            }
            Ok(Event::Empty(_)) => {}
            Ok(Event::Text(t)) => {
                let text = unescape(&t);
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                let Some(name) = stack.last() else { continue };
                if name == "DocRoot" {
                    roots.push(text.to_string());
                } else if docs == 1 && stack.iter().any(|s| s == "DocInfo") {
                    let key = match name.as_str() {
                        "Title" => "title",
                        "Author" => "author",
                        "Subject" => "subject",
                        "Abstract" => "abstract",
                        "CreationDate" => "created",
                        "ModDate" => "modified",
                        "Creator" => "creator",
                        "CreatorVersion" => "creator-version",
                        "DocID" => "doc-id",
                        "DocUsage" => "usage",
                        "Cover" => continue,
                        "Keyword" => {
                            keywords.push(text.to_string());
                            continue;
                        }
                        "CustomData" => {
                            if let Some(k) = &custom {
                                identity.push((k.clone(), text.to_string()));
                            }
                            continue;
                        }
                        _ => continue,
                    };
                    identity.push((key.to_string(), text.to_string()));
                }
            }
            Ok(Event::End(_)) => {
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(OfdTextError::Malformed(format!("OFD.xml: {e}"))),
            _ => {}
        }
    }
    if !keywords.is_empty() {
        identity.push(("keywords".to_string(), keywords.join("; ")));
    }
    if roots.is_empty() {
        return Err(OfdTextError::Malformed("OFD.xml names no DocRoot".into()));
    }
    Ok((identity, roots))
}

/// `Document.xml`: the page list, the template pages, the page
/// area, the outline (destinations by page and top, bookmarks
/// resolved).
fn read_document(xml: &str, base: &str, first: u32) -> Result<DocumentSpec, OfdTextError> {
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<String> = Vec::new();
    let mut default_size = (0.0, 0.0);
    let mut templates: HashMap<String, String> = HashMap::new();
    let mut page_locs: Vec<(String, String)> = Vec::new(); // (id, path)
    let mut bookmarks: HashMap<String, (String, Option<f64>)> = HashMap::new();
    let mut pending_bookmark: Option<String> = None;
    // The outline: (level, title, dest page id, top, bookmark name).
    struct Raw {
        level: u8,
        title: String,
        page_id: Option<String>,
        top: Option<f64>,
        bookmark: Option<String>,
    }
    let mut raw: Vec<Raw> = Vec::new();
    let mut depth = 0u8;
    loop {
        let ev = reader.read_event();
        match ev {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let empty = matches!(ev, Ok(Event::Empty(_)));
                let name = String::from_utf8_lossy(local(e.name().as_ref())).into_owned();
                match name.as_str() {
                    "TemplatePage" => {
                        if let (Some(id), Some(loc)) = (attr(e, b"ID"), attr(e, b"BaseLoc")) {
                            templates.insert(id, resolve(base, &loc));
                        }
                    }
                    "Page" if stack.last().is_some_and(|s| s == "Pages") => {
                        if let (Some(id), Some(loc)) = (attr(e, b"ID"), attr(e, b"BaseLoc")) {
                            page_locs.push((id, resolve(base, &loc)));
                        }
                    }
                    "OutlineElem" => {
                        depth += 1;
                        raw.push(Raw {
                            level: depth,
                            title: attr(e, b"Title").unwrap_or_default(),
                            page_id: None,
                            top: None,
                            bookmark: None,
                        });
                    }
                    "Bookmark" if stack.last().is_some_and(|s| s == "Bookmarks") => {
                        pending_bookmark = attr(e, b"Name");
                    }
                    "Bookmark" if stack.iter().any(|s| s == "OutlineElem") => {
                        if let Some(r) = raw.last_mut() {
                            r.bookmark = attr(e, b"Name");
                        }
                    }
                    "Dest" => {
                        let page_id = attr(e, b"PageID");
                        let top = attr(e, b"Top").and_then(|t| t.parse().ok());
                        if stack.iter().any(|s| s == "OutlineElem") {
                            if let Some(r) = raw.last_mut()
                                && r.page_id.is_none()
                            {
                                r.page_id = page_id;
                                r.top = top;
                            }
                        } else if let (Some(name), Some(p)) = (&pending_bookmark, page_id) {
                            bookmarks.insert(name.clone(), (p, top));
                        }
                    }
                    _ => {}
                }
                if !empty {
                    stack.push(name);
                }
            }
            Ok(Event::Text(t)) => {
                if stack.last().is_some_and(|s| s == "PhysicalBox")
                    && stack.iter().any(|s| s == "PageArea")
                    && !stack.iter().any(|s| s == "Page")
                    && let Some([_, _, w, h]) = box4(&unescape(&t))
                {
                    default_size = (w, h);
                }
            }
            Ok(Event::End(e)) => {
                let qname = e.name();
                let name = local(qname.as_ref());
                if name == b"OutlineElem" {
                    depth = depth.saturating_sub(1);
                }
                if name == b"Bookmark" {
                    pending_bookmark = None;
                }
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(OfdTextError::Malformed(format!("Document.xml: {e}"))),
            _ => {}
        }
    }
    let number_of: HashMap<&str, u32> = page_locs
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (id.as_str(), first + i as u32 + 1))
        .collect();
    let pages = page_locs
        .iter()
        .enumerate()
        .map(|(i, (_, path))| PageSpec {
            number: first + i as u32 + 1,
            path: path.clone(),
            default_size,
            templates: templates.clone(),
        })
        .collect();
    // An entry without a reachable destination is a heading with
    // no extent of its own: it takes the next entry's start.
    let mut outline = Vec::new();
    let mut pending: Vec<(u8, String)> = Vec::new();
    for r in raw {
        let dest = match (&r.page_id, &r.bookmark) {
            (Some(p), _) => number_of.get(p.as_str()).map(|n| (*n, r.top)),
            (None, Some(b)) => bookmarks
                .get(b)
                .and_then(|(p, top)| number_of.get(p.as_str()).map(|n| (*n, *top))),
            (None, None) => None,
        };
        match dest {
            Some(d) => {
                for (level, title) in pending.drain(..) {
                    outline.push(OutlineEntry {
                        level,
                        title,
                        dest: d,
                    });
                }
                outline.push(OutlineEntry {
                    level: r.level,
                    title: r.title,
                    dest: d,
                });
            }
            None => pending.push((r.level, r.title)),
        }
    }
    Ok(DocumentSpec { pages, outline })
}

/// A page's (or a template page's) `Content.xml`: its own area, the
/// templates it draws, and every text code with its placement.
fn read_page(xml: &str, number: u32) -> Result<PageContent, OfdTextError> {
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<String> = Vec::new();
    let mut size = None;
    let mut templates = Vec::new();
    let mut codes = Vec::new();
    // The open text object's boundary, and the open code's X/Y.
    let mut boundary: Option<[f64; 4]> = None;
    let mut code_at: Option<(f64, f64)> = None;
    loop {
        let ev = reader.read_event();
        match ev {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let empty = matches!(ev, Ok(Event::Empty(_)));
                let name = String::from_utf8_lossy(local(e.name().as_ref())).into_owned();
                match name.as_str() {
                    "Template" => {
                        if let Some(id) = attr(e, b"TemplateID") {
                            templates.push(id);
                        }
                    }
                    "TextObject" => {
                        boundary = attr(e, b"Boundary").and_then(|b| box4(&b));
                    }
                    "TextCode" => {
                        let x = attr(e, b"X").and_then(|v| v.parse().ok()).unwrap_or(0.0);
                        let y = attr(e, b"Y").and_then(|v| v.parse().ok()).unwrap_or(0.0);
                        code_at = Some((x, y));
                    }
                    _ => {}
                }
                if !empty {
                    stack.push(name);
                }
            }
            Ok(Event::Text(t)) => {
                let Some(top) = stack.last() else { continue };
                match top.as_str() {
                    "TextCode" => {
                        if let (Some([bx, by, bw, bh]), Some((cx, cy))) = (boundary, code_at) {
                            let text = unescape(&t);
                            if !text.trim().is_empty() {
                                codes.push(Code {
                                    page: number,
                                    baseline: by + cy,
                                    x: bx + cx,
                                    right: bx + bw,
                                    top: by,
                                    bottom: by + bh,
                                    text,
                                });
                            }
                        }
                    }
                    "PhysicalBox" if stack.iter().any(|s| s == "Area") => {
                        if let Some([_, _, w, h]) = box4(&unescape(&t)) {
                            size = Some((w, h));
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let qname = e.name();
                match local(qname.as_ref()) {
                    b"TextObject" => boundary = None,
                    b"TextCode" => code_at = None,
                    _ => {}
                }
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(OfdTextError::Malformed(format!("page {number}: {e}"))),
            _ => {}
        }
    }
    Ok(PageContent {
        size,
        templates,
        codes,
    })
}

// ---------------------------------------------------------------
// The adapter
// ---------------------------------------------------------------

impl OfdText {
    fn kids(&self, i: usize) -> &[usize] {
        match &self.nodes[i] {
            Node::Root { children }
            | Node::Section { children, .. }
            | Node::Page { children, .. } => children,
            Node::Line { .. } => &[],
        }
    }

    fn flatten(&self, i: usize, out: &mut Vec<String>) {
        if let Node::Line { line, .. } = &self.nodes[i] {
            out.push(line.text.clone());
        }
        for &c in self.kids(i) {
            self.flatten(c, out);
        }
    }

    pub fn locator(&self, node: NodeId) -> String {
        match &self.nodes[node.0 as usize] {
            Node::Root { .. } => "/".into(),
            Node::Section { lemma, .. } => format!("section '{lemma}'"),
            Node::Page { index, .. } => format!("page {index}"),
            Node::Line { line, .. } => format!("p{}:y{}", line.page, line.baseline),
        }
    }
}

impl AstAdapter for OfdText {
    /// This source defines no references to resolve.
    fn resolves_references(&self) -> bool {
        false
    }

    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.kids(node.0 as usize)
            .iter()
            .map(|&i| NodeId(i as u64))
            .collect()
    }
    fn name(&self, node: NodeId) -> Option<String> {
        Some(match &self.nodes[node.0 as usize] {
            Node::Root { .. } => return None,
            Node::Section { .. } => "section".into(),
            Node::Page { .. } => "page".into(),
            Node::Line { .. } => "line".into(),
        })
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        match &self.nodes[node.0 as usize] {
            Node::Root { .. } => None,
            Node::Section { parent, .. }
            | Node::Page { parent, .. }
            | Node::Line { parent, .. } => Some(NodeId(*parent as u64)),
        }
    }
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        match (&self.nodes[node.0 as usize], name) {
            (Node::Section { lemma, .. }, "lemma") => Some(Value::Str(lemma.clone())),
            // The declared identity: DocInfo on the document node.
            (Node::Root { .. }, key) => self
                .identity
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| Value::Str(v.clone()))
                .or_else(|| (key == "text").then(|| self.default_value(node)).flatten()),
            (_, "text") => self.default_value(node),
            _ => None,
        }
    }
    fn properties(&self, node: NodeId) -> Vec<(String, Value)> {
        match &self.nodes[node.0 as usize] {
            Node::Root { .. } => self
                .identity
                .iter()
                .map(|(k, v)| (k.clone(), Value::Str(v.clone())))
                .collect(),
            Node::Section { lemma, .. } => vec![("lemma".into(), Value::Str(lemma.clone()))],
            _ => Vec::new(),
        }
    }
    fn default_value(&self, node: NodeId) -> Option<Value> {
        match &self.nodes[node.0 as usize] {
            Node::Line { line, .. } => Some(Value::Str(line.text.clone())),
            _ => {
                let mut out = Vec::new();
                self.flatten(node.0 as usize, &mut out);
                (!out.is_empty()).then(|| Value::Str(out.join("\n")))
            }
        }
    }
    /// The `:::` keys this adapter answers (`:meta` lists the ones
    /// a node answers).
    fn metadata_catalogue(&self) -> &'static [&'static str] {
        &["page"]
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        /// A length in millimetres as the quantity, written in mm.
        fn mm(v: f64) -> Value {
            Value::Quantity {
                value: v / 1000.0,
                base: "m".to_string(),
                written: Some((v, "mm".to_string())),
            }
        }
        match (&self.nodes[node.0 as usize], key) {
            (Node::Line { line, .. }, "page") => Some(Value::Int(line.page as i64)),
            // Lengths in the page's millimetres, minted as such (spec:
            // adapters mint a measure as a quantity in the source's
            // unit): the extents and the coordinates alike.
            (Node::Line { line, .. }, "x") => Some(mm(line.x)),
            (Node::Line { line, .. }, "y") => Some(mm(line.y)),
            (Node::Line { line, .. }, "width") => Some(mm(line.width)),
            (Node::Line { line, .. }, "height") => Some(mm(line.height)),
            (Node::Line { line, .. }, "baseline") => Some(mm(line.baseline)),
            (Node::Section { level, .. }, "level") => Some(Value::Int(*level as i64)),
            (Node::Page { index, .. }, "page" | "number") => Some(Value::Int(*index as i64)),
            (Node::Page { width, .. }, "width") => Some(mm(*width)),
            (Node::Page { height, .. }, "height") => Some(mm(*height)),
            (Node::Root { .. }, "n-pages") => Some(Value::Int(self.n_pages as i64)),
            _ => None,
        }
    }
}
