//! The reader's model of a PDF — `text:paper.pdf` — built from
//! declared structure only, per the four-rung ladder in the
//! document-adapter design row (lang/TODO.md):
//!
//! - **Sections from the outline** (the navigation bar): the
//!   `/Outlines` tree is the author's declared structure, with
//!   excellent coverage (every hyperref/LaTeX document, books,
//!   generated reports). Bookmark titles become `::lemma`
//!   (PDFDocEncoding / UTF-16BE, spec'd decodings); nesting
//!   follows the outline tree; a section's extent runs from its
//!   destination to the next entry's in outline order (`XYZ`
//!   destinations bound at page+y, page-only fits at the page).
//!   `GoTo` actions and named destinations resolve; a malformed
//!   or absent outline yields no sections — never a guess.
//! - **Lines with geometry**: the real objects. A `line` groups
//!   the text runs a page shows at exactly the same declared
//!   baseline (identical y after the declared matrix math — no
//!   tolerance, no clustering), in x order. `::::page`, `::::x`,
//!   `::::y` carry the coordinates. A `paragraph` is NEVER minted
//!   from a PDF — print formats declare none, and the vocabulary
//!   says so.
//! - Run text decodes through the font's `/ToUnicode` CMap when
//!   present; a simple font without one reads its printable-ASCII
//!   range per the spec's standard encodings (declared, exact); a
//!   composite font without a CMap is undecodable and its runs
//!   are skipped (null, propagating — never mojibake).
//! - Heuristic extraction (column inference, hyphenation repair,
//!   reading order) stays outside the engine, at the shell door.
//!
//! Without an outline the tree is `page/*` → lines. With one,
//! front-matter lines (before the first destination) stay at the
//! root, and each section holds its lines and child sections.

use std::collections::HashMap;

use lopdf::{Dictionary, Document, Object, ObjectId};
use quarb::{AstAdapter, NodeId, Value};

#[derive(Debug, thiserror::Error)]
pub enum PdfTextError {
    #[error("not a PDF: {0}")]
    Parse(#[from] lopdf::Error),
}

// ---------------------------------------------------------------
// The extracted model
// ---------------------------------------------------------------

#[derive(Debug, Clone)]
struct Line {
    page: u32,
    y: f64,
    x: f64,
    text: String,
}

#[derive(Debug)]
struct OutlineEntry {
    level: u8,
    title: String,
    /// (page index, y) — y is `None` for page-granular fits.
    dest: (u32, Option<f64>),
}

enum Node {
    Root { children: Vec<usize> },
    Section { lemma: String, level: u8, parent: usize, children: Vec<usize> },
    Page { index: u32, parent: usize, children: Vec<usize> },
    Line { line: Line, parent: usize },
}

pub struct PdfText {
    nodes: Vec<Node>,
}

pub fn parse(bytes: &[u8]) -> Result<PdfText, PdfTextError> {
    let doc = Document::load_mem(bytes)?;
    let pages = doc.get_pages();
    let page_index: HashMap<ObjectId, u32> = pages.iter().map(|(n, id)| (*id, *n)).collect();
    let mut lines = extract_lines(&doc, &pages);
    let outline = extract_outline(&doc, &page_index);
    Ok(assemble(&mut lines, outline))
}

// ---------------------------------------------------------------
// Assembly: outline boundaries over the line stream
// ---------------------------------------------------------------

/// Document order for boundary comparison: later page, or lower on
/// the same page (PDF y grows upward).
fn at_or_after(line: &Line, dest: &(u32, Option<f64>)) -> bool {
    match dest {
        (p, None) => line.page >= *p,
        (p, Some(y)) => line.page > *p || (line.page == *p && line.y <= *y),
    }
}

fn assemble(lines: &mut Vec<Line>, outline: Vec<OutlineEntry>) -> PdfText {
    // Lines arrive per page in extraction order; sort each page's
    // by (y desc, x asc) — the declared coordinates, no tolerance.
    lines.sort_by(|a, b| {
        (a.page, ordf(b.y), ordf(a.x))
            .partial_cmp(&(b.page, ordf(a.y), ordf(b.x)))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut nodes = vec![Node::Root { children: Vec::new() }];
    if outline.is_empty() {
        let mut page_node: Option<(u32, usize)> = None;
        for line in lines.drain(..) {
            let pn = match page_node {
                Some((p, n)) if p == line.page => n,
                _ => {
                    let n = nodes.len();
                    nodes.push(Node::Page { index: line.page, parent: 0, children: Vec::new() });
                    if let Node::Root { children } = &mut nodes[0] {
                        children.push(n);
                    }
                    page_node = Some((line.page, n));
                    n
                }
            };
            let ln = nodes.len();
            nodes.push(Node::Line { line, parent: pn });
            if let Node::Page { children, .. } = &mut nodes[pn] {
                children.push(ln);
            }
        }
        return PdfText { nodes };
    }
    // Sections in outline order; each takes the lines from its
    // destination up to the next entry's. Front matter stays at
    // the root. Nesting follows the entry levels.
    let mut line_iter = lines.drain(..).peekable();
    let mut take_until = |nodes: &mut Vec<Node>,
                          parent: usize,
                          stop: Option<&(u32, Option<f64>)>| {
        while let Some(line) = line_iter.peek() {
            if let Some(stop) = stop
                && at_or_after(line, stop)
            {
                break;
            }
            let line = line_iter.next().unwrap();
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
    // Front matter: everything before the first destination.
    take_until(&mut nodes, 0, Some(&outline[0].dest));
    let mut stack: Vec<(u8, usize)> = Vec::new(); // (level, node)
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
        // This section's own lines run to the next entry's
        // destination (a child's destination also stops them —
        // the child then owns what follows).
        take_until(&mut nodes, sn, outline.get(i + 1).map(|e| &e.dest));
    }
    // Anything after the last destination belongs to the last
    // section (already taken: its stop was None).
    PdfText { nodes }
}

fn ordf(f: f64) -> ordered::F {
    ordered::F(f)
}

/// A totally ordered f64 wrapper for the sort key (NaN sorts last;
/// coordinates in real documents are finite).
mod ordered {
    #[derive(PartialEq, PartialOrd)]
    pub struct F(pub f64);
}

// ---------------------------------------------------------------
// Outline extraction
// ---------------------------------------------------------------

fn extract_outline(doc: &Document, page_index: &HashMap<ObjectId, u32>) -> Vec<OutlineEntry> {
    let mut out = Vec::new();
    let Ok(catalog) = doc.catalog() else { return out };
    let Some(outlines) = resolve_dict(doc, catalog.get(b"Outlines").ok()) else {
        return out;
    };
    let first = outlines.get(b"First").ok().cloned();
    walk_outline(doc, page_index, first, 1, &mut out, 0);
    // Entries whose destination did not resolve cannot bound
    // anything: drop them (never a guess).
    out
}

fn walk_outline(
    doc: &Document,
    page_index: &HashMap<ObjectId, u32>,
    mut item: Option<Object>,
    level: u8,
    out: &mut Vec<OutlineEntry>,
    depth: usize,
) {
    if depth > 64 {
        return;
    }
    let mut guard = 0;
    while let Some(obj) = item {
        guard += 1;
        if guard > 4096 {
            return;
        }
        let Some(dict) = resolve_dict(doc, Some(&obj)).cloned() else { return };
        let title = match resolve(doc, dict.get(b"Title").ok()) {
            Some(Object::String(bytes, _)) => text_string(bytes),
            _ => String::new(),
        };
        if let Some(dest) = entry_dest(doc, &dict, page_index)
            && !title.is_empty()
        {
            out.push(OutlineEntry { level, title, dest });
        }
        if let Ok(first) = dict.get(b"First") {
            walk_outline(doc, page_index, Some(first.clone()), level + 1, out, depth + 1);
        }
        item = dict.get(b"Next").ok().cloned();
    }
}

/// An entry's destination: /Dest directly, or a GoTo action's /D;
/// names resolve through the catalog's /Dests dictionary or the
/// /Names /Dests name tree.
fn entry_dest(
    doc: &Document,
    entry: &Dictionary,
    page_index: &HashMap<ObjectId, u32>,
) -> Option<(u32, Option<f64>)> {
    let dest = entry.get(b"Dest").ok().cloned().or_else(|| {
        let action = resolve_dict(doc, entry.get(b"A").ok())?;
        match action.get(b"S") {
            Ok(Object::Name(s)) if s == b"GoTo" => action.get(b"D").ok().cloned(),
            _ => None,
        }
    })?;
    dest_target(doc, &dest, page_index, 0)
}

fn dest_target(
    doc: &Document,
    dest: &Object,
    page_index: &HashMap<ObjectId, u32>,
    depth: usize,
) -> Option<(u32, Option<f64>)> {
    if depth > 8 {
        return None;
    }
    match resolve(doc, Some(dest))? {
        Object::Array(parts) => {
            let page = match parts.first()? {
                Object::Reference(id) => *page_index.get(id)?,
                _ => return None,
            };
            // [page /XYZ x y z] carries the y; the /Fit family is
            // page-granular.
            let y = match (parts.get(1), parts.get(3)) {
                (Some(Object::Name(k)), Some(Object::Integer(y))) if k == b"XYZ" => {
                    Some(*y as f64)
                }
                (Some(Object::Name(k)), Some(Object::Real(y))) if k == b"XYZ" => {
                    Some(*y as f64)
                }
                _ => None,
            };
            Some((page, y))
        }
        Object::String(name, _) => {
            let name = name.clone();
            named_dest(doc, &name).and_then(|d| dest_target(doc, &d, page_index, depth + 1))
        }
        Object::Name(name) => {
            let name = name.clone();
            named_dest(doc, &name).and_then(|d| dest_target(doc, &d, page_index, depth + 1))
        }
        _ => None,
    }
}

/// A named destination, from the PDF 1.1 catalog /Dests dictionary
/// or the /Names /Dests name tree. A /D-wrapped dictionary value
/// unwraps to its array.
fn named_dest(doc: &Document, name: &[u8]) -> Option<Object> {
    let catalog = doc.catalog().ok()?;
    let found = resolve_dict(doc, catalog.get(b"Dests").ok())
        .and_then(|d| d.get(name).ok().cloned())
        .or_else(|| {
            let names = resolve_dict(doc, catalog.get(b"Names").ok())?;
            let dests = resolve_dict(doc, names.get(b"Dests").ok())?;
            name_tree_lookup(doc, dests, name, 0)
        })?;
    match resolve(doc, Some(&found))? {
        Object::Dictionary(d) => d.get(b"D").ok().cloned(),
        other => Some(other.clone()),
    }
}

fn name_tree_lookup(
    doc: &Document,
    node: &Dictionary,
    name: &[u8],
    depth: usize,
) -> Option<Object> {
    if depth > 32 {
        return None;
    }
    if let Some(Object::Array(pairs)) = resolve(doc, node.get(b"Names").ok()) {
        let pairs = pairs.clone();
        for pair in pairs.chunks_exact(2) {
            if let Object::String(k, _) = resolve(doc, Some(&pair[0]))?
                && k == name
            {
                return Some(pair[1].clone());
            }
        }
    }
    if let Some(Object::Array(kids)) = resolve(doc, node.get(b"Kids").ok()) {
        let kids = kids.clone();
        for kid in &kids {
            if let Some(d) = resolve_dict(doc, Some(kid))
                && let Some(hit) = name_tree_lookup(doc, &d.clone(), name, depth + 1)
            {
                return Some(hit);
            }
        }
    }
    None
}

// ---------------------------------------------------------------
// Line extraction: the declared matrix math, no tolerance
// ---------------------------------------------------------------

#[derive(Clone, Copy)]
struct Matrix([f64; 6]);

impl Matrix {
    const IDENTITY: Matrix = Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    fn mul(self, m: Matrix) -> Matrix {
        let a = self.0;
        let b = m.0;
        Matrix([
            a[0] * b[0] + a[1] * b[2],
            a[0] * b[1] + a[1] * b[3],
            a[2] * b[0] + a[3] * b[2],
            a[2] * b[1] + a[3] * b[3],
            a[4] * b[0] + a[5] * b[2] + b[4],
            a[4] * b[1] + a[5] * b[3] + b[5],
        ])
    }
    fn origin(self) -> (f64, f64) {
        (self.0[4], self.0[5])
    }
}

fn num(o: &Object) -> Option<f64> {
    match o {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(f) => Some(*f as f64),
        _ => None,
    }
}

/// A font's decoder: a /ToUnicode CMap, or the printable-ASCII
/// range of a simple font's standard encoding, or nothing (a
/// composite font without a CMap — undecodable, runs skipped).
enum Decoder {
    CMap(HashMap<Vec<u8>, String>, usize),
    Ascii,
    None,
}

impl Decoder {
    fn decode(&self, bytes: &[u8]) -> Option<String> {
        match self {
            Decoder::Ascii => Some(
                bytes
                    .iter()
                    .filter(|b| (0x20..0x7f).contains(*b))
                    .map(|&b| b as char)
                    .collect(),
            ),
            Decoder::CMap(map, width) => {
                let mut out = String::new();
                for code in bytes.chunks(*width) {
                    if let Some(s) = map.get(code) {
                        out.push_str(s);
                    }
                }
                Some(out)
            }
            Decoder::None => None,
        }
    }
}

fn font_decoders(doc: &Document, page: ObjectId) -> HashMap<Vec<u8>, Decoder> {
    let mut out = HashMap::new();
    // Resources may sit inline on the page or behind an indirect
    // reference (get_page_resources returns those as ids).
    let Ok((inline, ids)) = doc.get_page_resources(page) else { return out };
    let resolved;
    let res = match inline {
        Some(r) => r,
        None => {
            let Some(r) = ids
                .first()
                .and_then(|id| doc.get_object(*id).ok())
                .and_then(|o| o.as_dict().ok())
            else {
                return out;
            };
            resolved = r;
            resolved
        }
    };
    let Some(fonts) = resolve_dict(doc, res.get(b"Font").ok()) else { return out };
    for (name, fobj) in fonts.iter() {
        let Some(fd) = resolve_dict(doc, Some(fobj)) else { continue };
        let decoder = if let Some(Object::Stream(s)) = resolve(doc, fd.get(b"ToUnicode").ok()) {
            // A filterless stream's decompressed_content is empty
            // by lopdf's contract; the raw bytes ARE the content.
            let data = if s.dict.get(b"Filter").is_ok() {
                s.decompressed_content().unwrap_or_default()
            } else {
                s.content.clone()
            };
            parse_cmap(&data)
        } else {
            let composite = matches!(fd.get(b"Subtype"), Ok(Object::Name(n)) if n == b"Type0");
            if composite { Decoder::None } else { Decoder::Ascii }
        };
        out.insert(name.to_vec(), decoder);
    }
    out
}

/// The bfchar/bfrange subset of a ToUnicode CMap — the spec'd,
/// declared mapping (no font-file digging).
fn parse_cmap(data: &[u8]) -> Decoder {
    let text = String::from_utf8_lossy(data);
    let mut map: HashMap<Vec<u8>, String> = HashMap::new();
    let mut width = 1usize;
    let hexes = |s: &str| -> Vec<Vec<u8>> {
        s.split('<')
            .skip(1)
            .filter_map(|part| {
                let hex = part.split('>').next()?;
                (!hex.is_empty()).then(|| {
                    (0..hex.len())
                        .step_by(2)
                        .filter_map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
                        .collect()
                })
            })
            .collect()
    };
    let utf16 = |b: &[u8]| -> String {
        let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    };
    for section in text.split("beginbfchar").skip(1) {
        let Some(body) = section.split("endbfchar").next() else { continue };
        for line in body.lines() {
            let h = hexes(line);
            if h.len() == 2 {
                width = width.max(h[0].len());
                map.insert(h[0].clone(), utf16(&h[1]));
            }
        }
    }
    for section in text.split("beginbfrange").skip(1) {
        let Some(body) = section.split("endbfrange").next() else { continue };
        for line in body.lines() {
            let h = hexes(line);
            if h.len() == 3 && h[0].len() <= 2 && h[0].len() == h[1].len() {
                width = width.max(h[0].len());
                let lo = be(&h[0]);
                let hi = be(&h[1]);
                let start = be(&h[2]);
                for (k, code) in (lo..=hi).enumerate() {
                    let key = if h[0].len() == 1 {
                        vec![code as u8]
                    } else {
                        (code as u16).to_be_bytes().to_vec()
                    };
                    let target = start + k as u64;
                    let bytes = if h[2].len() <= 2 {
                        (target as u16).to_be_bytes().to_vec()
                    } else {
                        let mut b = h[2].clone();
                        let last = b.len() - 1;
                        b[last] = (be(&h[2]) + k as u64) as u8;
                        b
                    };
                    map.insert(key, utf16(&bytes));
                }
            }
        }
    }
    if map.is_empty() { Decoder::None } else { Decoder::CMap(map, width) }
}

fn be(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |acc, &x| (acc << 8) | x as u64)
}

fn extract_lines(doc: &Document, pages: &std::collections::BTreeMap<u32, ObjectId>) -> Vec<Line> {
    let mut lines: HashMap<(u32, u64), Line> = HashMap::new();
    for (&pnum, &pid) in pages {
        let Ok(content) = doc.get_page_content(pid) else { continue };
        let Ok(ops) = lopdf::content::Content::decode(&content) else { continue };
        let decoders = font_decoders(doc, pid);
        let mut ctm = Matrix::IDENTITY;
        let mut stack: Vec<Matrix> = Vec::new();
        let mut tm = Matrix::IDENTITY;
        let mut tlm = Matrix::IDENTITY;
        let mut leading = 0.0f64;
        let mut font: Option<&Decoder> = None;
        for op in &ops.operations {
            let os = &op.operands;
            match op.operator.as_str() {
                "q" => stack.push(ctm),
                "Q" => ctm = stack.pop().unwrap_or(Matrix::IDENTITY),
                "cm" => {
                    if os.len() == 6 {
                        let m = Matrix([
                            num(&os[0]).unwrap_or(1.0),
                            num(&os[1]).unwrap_or(0.0),
                            num(&os[2]).unwrap_or(0.0),
                            num(&os[3]).unwrap_or(1.0),
                            num(&os[4]).unwrap_or(0.0),
                            num(&os[5]).unwrap_or(0.0),
                        ]);
                        ctm = m.mul(ctm);
                    }
                }
                "BT" => {
                    tm = Matrix::IDENTITY;
                    tlm = tm;
                }
                "Tf" => {
                    if let Some(Object::Name(n)) = os.first() {
                        font = decoders.get(n.as_slice());
                    }
                }
                "TL" => leading = os.first().and_then(num).unwrap_or(0.0),
                "Td" | "TD" => {
                    let tx = os.first().and_then(num).unwrap_or(0.0);
                    let ty = os.get(1).and_then(num).unwrap_or(0.0);
                    if op.operator == "TD" {
                        leading = -ty;
                    }
                    tlm = Matrix([1.0, 0.0, 0.0, 1.0, tx, ty]).mul(tlm);
                    tm = tlm;
                }
                "Tm" => {
                    if os.len() == 6 {
                        tlm = Matrix([
                            num(&os[0]).unwrap_or(1.0),
                            num(&os[1]).unwrap_or(0.0),
                            num(&os[2]).unwrap_or(0.0),
                            num(&os[3]).unwrap_or(1.0),
                            num(&os[4]).unwrap_or(0.0),
                            num(&os[5]).unwrap_or(0.0),
                        ]);
                        tm = tlm;
                    }
                }
                "T*" => {
                    tlm = Matrix([1.0, 0.0, 0.0, 1.0, 0.0, -leading]).mul(tlm);
                    tm = tlm;
                }
                "Tj" | "'" | "\"" => {
                    if op.operator != "Tj" {
                        tlm = Matrix([1.0, 0.0, 0.0, 1.0, 0.0, -leading]).mul(tlm);
                        tm = tlm;
                    }
                    let text_arg = os.last();
                    if let Some(Object::String(bytes, _)) = text_arg
                        && let Some(text) = font.and_then(|f| f.decode(bytes))
                    {
                        show(&mut lines, pnum, tm.mul(ctm), &text);
                    }
                }
                "TJ" => {
                    if let Some(Object::Array(parts)) = os.first() {
                        let mut text = String::new();
                        for part in parts {
                            match part {
                                Object::String(bytes, _) => {
                                    if let Some(s) = font.and_then(|f| f.decode(bytes)) {
                                        text.push_str(&s);
                                    }
                                }
                                // The one presentational rule in
                                // this reading, stated as a fixed
                                // constant: a kerning adjustment of
                                // -180/1000 em or wider reads as a
                                // word gap (kerns run -10..-100,
                                // word glue -200..-400). The same
                                // number on every machine.
                                adj => {
                                    if num(adj).is_some_and(|a| a <= -180.0)
                                        && !text.ends_with(' ')
                                    {
                                        text.push(' ');
                                    }
                                }
                            }
                        }
                        let text = text.trim().to_string();
                        if !text.is_empty() {
                            show(&mut lines, pnum, tm.mul(ctm), &text);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    lines.into_values().collect()
}

/// Append shown text to its line: the run's baseline (exact
/// bit-pattern of the declared y) names the line; the first run
/// fixes the line's x.
fn show(lines: &mut HashMap<(u32, u64), Line>, page: u32, m: Matrix, text: &str) {
    let (x, y) = m.origin();
    let entry = lines.entry((page, y.to_bits())).or_insert(Line {
        page,
        y,
        x,
        text: String::new(),
    });
    if !entry.text.is_empty() {
        entry.text.push(' ');
    }
    entry.text.push_str(text);
}

/// A PDF text string (outline titles): UTF-16BE behind its BOM,
/// PDFDocEncoding otherwise (ASCII-identity).
fn text_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

fn resolve<'a>(doc: &'a Document, o: Option<&'a Object>) -> Option<&'a Object> {
    match o? {
        Object::Reference(id) => doc.get_object(*id).ok(),
        direct => Some(direct),
    }
}

fn resolve_dict<'a>(doc: &'a Document, o: Option<&'a Object>) -> Option<&'a Dictionary> {
    match resolve(doc, o)? {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    }
}

// ---------------------------------------------------------------
// The adapter
// ---------------------------------------------------------------

impl PdfText {
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
            Node::Line { line, .. } => format!("p{}:y{}", line.page, line.y),
        }
    }
}

impl AstAdapter for PdfText {
    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.kids(node.0 as usize).iter().map(|&i| NodeId(i as u64)).collect()
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
            Node::Section { parent, .. } | Node::Page { parent, .. } | Node::Line { parent, .. } => {
                Some(NodeId(*parent as u64))
            }
        }
    }
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        match (&self.nodes[node.0 as usize], name) {
            (Node::Section { lemma, .. }, "lemma") => Some(Value::Str(lemma.clone())),
            (_, "text") => self.default_value(node),
            _ => None,
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
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match (&self.nodes[node.0 as usize], key) {
            (Node::Line { line, .. }, "page") => Some(Value::Int(line.page as i64)),
            (Node::Line { line, .. }, "x") => Some(Value::Float(line.x)),
            (Node::Line { line, .. }, "y") => Some(Value::Float(line.y)),
            (Node::Section { level, .. }, "level") => Some(Value::Int(*level as i64)),
            (Node::Page { index, .. }, "page") => Some(Value::Int(*index as i64)),
            _ => None,
        }
    }
}
