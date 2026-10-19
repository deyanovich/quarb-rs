//! The koine adapter for the Quarb text level: `koine:doc.atk` —
//! the reader's model from an atrep document, lowered through the
//! resolved sim-name vocabulary of its dialektos (the koine hub
//! design row in lang/TODO.md).
//!
//! An atrep document — parsed by the atrep crate, settled by
//! `kanonizo::tasso` — lowers into the shared `quarb-text` block
//! vocabulary, so `//section[::lemma ...]`, `//footnote`, and
//! `//index-mark` read identically over a `.atk` file and every
//! other text-level mount. Foreign formats arrive through
//! **atrep's endomorphosis importers** (`atrep::endo`): Markdown,
//! HTML, reStructuredText, Org, and djot import into their mirror
//! dialects (`at-markdown`, `at-html`, …) and lower here through
//! those dialects' resolved sim names — the semantic mapping
//! stays in atrep, this crate stays one vocabulary reader. The
//! per-format `text:` adapters remain the sibling route (the
//! multi-view); the dialect-faithful reading (`quarb-atrep`, sims
//! by their own names) remains the plain mount.
//!
//! The mapping keys on **resolved sim names** — the dialektos
//! vocabulary, cited from the atrep std dialects (koine,
//! at-prosa, at-poesia, at-drama) — never on raw symbols:
//!
//! - **Headings** come in both shapes. The enclosing
//!   para-simmeres (`title`, `part`, `chapter`, `section`, …)
//!   become flat [`Block::Heading`]s whose level is their
//!   heading-nesting depth, so the shared derivation rebuilds
//!   exactly the source nesting. The flat dialects' `heading-1`
//!   … `heading-6` endos carry their level in the name, and the
//!   derivation builds the outline the usual way. Flat sibling
//!   list items (at-markdown has no list containers) wrap into
//!   runs.
//! - **The apparatus** maps family to family: `footnote` →
//!   footnote, `endnote` → endnote, `aside` → aside; a deixis
//!   becomes the callout, the body block the note body.
//!   `chapter-endnote` reads as the endnote family in this
//!   version (the placement doctrine; a fourth model family is
//!   the recorded alternative). `manuscript-note` — critical
//!   apparatus, "not part of the Text itself" — is out of the
//!   reader's model by design.
//! - **Index marks**: the `index` endo yields an `index-mark`
//!   carrying the full entry path as `::term` (`>` sub-entries
//!   kept as written) while its displayed segment — the last —
//!   stays in the prose; `index-hidden` yields the mark alone.
//! - **Lists** (`unordered-list`/`ordered-list` and their items,
//!   `definition-list`/`definition-item` with the item lemma)
//!   and **blockquote**/`epigraph` (hypograph as attribution)
//!   map onto their text-level counterparts; `quotation` and the
//!   emphasis endos flatten into prose.
//! - **References** (`ref`, `heading-ref`, `page-ref`,
//!   `line-ref`, `cite`) keep their key as written in the prose
//!   — the reader sees a mark there (the LaTeX adapter's
//!   convention).
//! - **Verbatim** blocks keep their text as authored
//!   (`:::lang` from the first genos); an `englossis` reads as
//!   verbatim in its embedded dialect's name; verse stichoi
//!   lower as `verse`/`strophe`/`stichos`, the
//!   stichos taxis the citation coordinate.
//! - **Tables** (litogramma's stichoi table: pipe cells, a
//!   dash-run separating the header row) lower through the
//!   shared [`Block::Table`] rule.
//! - Unknown para-simmeres are transparent — their lemma opens
//!   as a paragraph, their children flow — so semantic
//!   containers (`theorema`, drama structure, …) degrade to
//!   readable prose instead of vanishing. `toc`,
//!   `section-break`, media, axiomata, and anaphors are out of
//!   the linear text.

use std::path::Path;

use atrep::dendron::{Block as ABlock, Inline, Strophe, Taxis};
use atrep::dialektos::{self, Dialektos};
use atrep::{Checked, kanonizo};
use quarb_text::{Block, Cell, Container, NoteFamily, TextModel};

/// An error mounting a document at the koine reading.
#[derive(Debug, thiserror::Error)]
pub enum KoineError {
    #[error(transparent)]
    Atrep(#[from] atrep::Error),
    #[error("input is an atrep dialektos definition, not a document")]
    NotADocument,
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error(
        "unknown koine format {0:?} — known: md, html, rst, org, djot, tei, docbook, jats, usx, osis, atd"
    )]
    UnknownFormat(String),
}

/// Mount a document file (`.atd` source or `.atk` kanon) at the
/// text level. `kanonizo::tasso` settles taxis and autonyms first,
/// so note onyms are always present.
pub fn parse_file(path: &Path) -> Result<TextModel, KoineError> {
    match atrep::check_any(path)? {
        Checked::Dialektos(_) => Err(KoineError::NotADocument),
        Checked::Document(mut doc) => {
            let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
            let dial = dialektos::resolve(&base, &doc.dialect_id)?;
            kanonizo::tasso(&mut doc, &dial, &base)?;
            let head = head_meta(&doc, &dial);
            let mut model = finish_model(blocks(&doc, &dial));
            model.set_head_meta(head);
            Ok(model)
        }
    }
}

/// Mount document text; `dir` anchors dialektos resolution (the
/// std dialektoi resolve regardless).
pub fn parse_str(source: &str, dir: &Path) -> Result<TextModel, KoineError> {
    match atrep::check_source(source, &dir.join("<memory>.atd"))? {
        Checked::Dialektos(_) => Err(KoineError::NotADocument),
        Checked::Document(mut doc) => {
            let dial = dialektos::resolve(dir, &doc.dialect_id)?;
            kanonizo::tasso(&mut doc, &dial, dir)?;
            let head = head_meta(&doc, &dial);
            let mut model = finish_model(blocks(&doc, &dial));
            model.set_head_meta(head);
            Ok(model)
        }
    }
}

/// Import Markdown through atrep's endomorphosis (`at-markdown`)
/// and lower it — `koine:notes.md`.
pub fn parse_markdown(text: &str) -> Result<TextModel, KoineError> {
    import(atrep::endo::markdown_to_document(text)?)
}

/// Import HTML through atrep's endomorphosis (`at-html`) and
/// lower it — `koine:page.html`.
pub fn parse_html(text: &str) -> Result<TextModel, KoineError> {
    import(atrep::endo::html_to_document(text)?)
}

/// Import reStructuredText through atrep's endomorphosis
/// (`at-rst`) and lower it — `koine:doc.rst`.
pub fn parse_rst(text: &str) -> Result<TextModel, KoineError> {
    import(atrep::endo::rst_to_document(text)?)
}

/// Import Org through atrep's endomorphosis (`at-org`) and lower
/// it — `koine:notes.org`.
pub fn parse_org(text: &str) -> Result<TextModel, KoineError> {
    import(atrep::endo::org_to_document(text)?)
}

/// Import djot through atrep's endomorphosis (`at-djot`) and
/// lower it — `koine:doc.dj`.
pub fn parse_djot(text: &str) -> Result<TextModel, KoineError> {
    import(atrep::endo::djot_to_document(text)?)
}

/// Import one of the XML vocabularies by its declared identity —
/// `kind` as detected by [`detect_xml_kind`] or forced by
/// `?format=`: `"tei"`, `"docbook"`, `"jats"`, `"usx"`, `"osis"`,
/// `"fb2"`.
pub fn parse_xml_as(text: &str, kind: &str) -> Result<TextModel, KoineError> {
    let doc = match kind {
        "usfm" => atrep::endo::usfm_to_document(text)?,
        "tei" => atrep::endo::tei_to_document(text)?,
        // FictionBook 2: titled sections, epigraphs and citations as
        // quotation blocks, poems as strophes, notes as footnotes;
        // the description's title and author as declared identity.
        "fb2" => atrep::fb2::fb2_to_document(text)?,
        "docbook" => atrep::endo::docbook_to_document(text)?,
        "jats" => atrep::endo::jats_to_document(text)?,
        "usx" => atrep::endo::usx_to_document(text)?,
        "osis" => atrep::endo::osis_to_document(text)?,
        "rnc" => atrep::epimerismos::rnc_to_document(text)?,
        "opencorpora" => atrep::epimerismos::opencorpora_to_document(text)?,
        "proiel" => atrep::epimerismos::proiel_to_document(text)?,
        other => return Err(KoineError::UnknownFormat(other.to_string())),
    };
    import(doc)
}

/// Which XML vocabulary a document declares itself to be — from
/// its root default namespace, its DOCTYPE public identifier, or
/// an unambiguous root element name, in that order. Declarations
/// only: a bare `<article>` with neither namespace nor doctype is
/// genuinely ambiguous (JATS vs DocBook 4) and returns `None` —
/// the `?format=` override is the escape hatch, never a guess.
pub fn detect_xml_kind(text: &str) -> Option<&'static str> {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    let mut reader = Reader::from_str(text);
    let mut buf = Vec::new();
    let mut doctype: Option<String> = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::DocType(d)) => {
                doctype = Some(String::from_utf8_lossy(d.as_ref()).to_string());
            }
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let xmlns = e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == b"xmlns")
                    .and_then(|a| a.unescape_value().ok().map(|v| v.to_string()));
                if let Some(ns) = xmlns.as_deref() {
                    match ns {
                        "http://www.tei-c.org/ns/1.0" => return Some("tei"),
                        "http://docbook.org/ns/docbook" => return Some("docbook"),
                        "http://www.bibletechnologies.net/2003/OSIS/namespace" => {
                            return Some("osis");
                        }
                        "http://www.gribuser.ru/xml/fictionbook/2.0" => return Some("fb2"),
                        _ => {}
                    }
                }
                if let Some(d) = doctype.as_deref() {
                    if d.contains("//NLM//") || d.contains("JATS") {
                        return Some("jats");
                    }
                    if d.contains("//OASIS//DTD DocBook") {
                        return Some("docbook");
                    }
                    if d.contains("//TEI") {
                        return Some("tei");
                    }
                }
                let name = e.name();
                let local = name.local_name();
                return match local.as_ref() {
                    b"TEI" | b"teiCorpus" => Some("tei"),
                    b"FictionBook" => Some("fb2"),
                    b"usx" => Some("usx"),
                    b"osis" => Some("osis"),
                    b"book" => Some("docbook"),
                    // The parsed corpora: OpenCorpora's
                    // `<annotation>`, PROIEL's `<proiel>`, and the
                    // Russian National Corpus's XHTML-shaped export,
                    // told from a web page by its `<ana` analyses.
                    b"annotation" => Some("opencorpora"),
                    b"proiel" => Some("proiel"),
                    b"html" if text.contains("<ana ") => Some("rnc"),
                    // <article> alone: JATS or DocBook 4 — refuse.
                    _ => None,
                };
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

/// Settle an imported mirror-dialect document and lower it: the
/// endomorphosis did the format work in atrep; this crate reads
/// the dialect vocabulary.
fn import(doc: atrep::Document) -> Result<TextModel, KoineError> {
    import_with(doc, false)
}

fn import_with(mut doc: atrep::Document, lit: bool) -> Result<TextModel, KoineError> {
    let dial = dialektos::resolve(Path::new("."), &doc.dialect_id)?;
    kanonizo::tasso(&mut doc, &dial, Path::new("."))?;
    let head = head_meta(&doc, &dial);
    let mut model = finish_model(blocks_with(&doc, &dial, lit));
    model.set_head_meta(head);
    Ok(model)
}

/// The document's declared identity, as the html reading declares
/// its head: the hoisted header an importer writes at the top
/// (`=` title, `=:` author, `="` abstract — a FictionBook's
/// description, a TEI title statement) becomes `:::title`,
/// `:::author`, `:::description` on the document node. The
/// paragraphs themselves stay in the text as they are.
fn head_meta(doc: &atrep::Document, dial: &Dialektos) -> quarb_text::HeadMeta {
    let mut head = quarb_text::HeadMeta::default();
    for block in &doc.blocks {
        let ABlock::Paragraph(inlines) = block else {
            // The abstract is a block of its own (`="`); any other
            // structural block ends the hoisted header.
            if let ABlock::Para {
                symbol, children, ..
            } = block
                && sim_name(dial, symbol) == "abstract"
            {
                let text = children
                    .iter()
                    .filter_map(|c| match c {
                        ABlock::Paragraph(i) => Some(flat(i)),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                head.declare("description", &text);
                if head.description.is_none() && !text.trim().is_empty() {
                    head.description = Some(quarb_text::normalize_ws(&text));
                }
                continue;
            }
            if matches!(block, ABlock::Para { .. } | ABlock::Stichoi { .. }) {
                break;
            }
            continue;
        };
        let [
            Inline::Endo {
                symbol, content, ..
            },
        ] = inlines.as_slice()
        else {
            continue;
        };
        let text = flat(content);
        match sim_name(dial, symbol).as_str() {
            "title" => {
                head.declare("title", &text);
                if head.title.is_none() {
                    head.title = Some(quarb_text::normalize_ws(&text));
                }
            }
            "author" => {
                head.declare("author", &text);
                if head.author.is_none() {
                    head.author = Some(quarb_text::normalize_ws(&text));
                }
            }
            "abstract" => {
                head.declare("description", &text);
                if head.description.is_none() {
                    head.description = Some(quarb_text::normalize_ws(&text));
                }
            }
            // The dictionary vocabulary: the languages line, the
            // index language first — `:::languages`.
            "languages" => {
                for code in text.split(|c: char| c.is_whitespace() || c == ',') {
                    if !code.is_empty() {
                        head.declare("languages", code);
                    }
                }
            }
            _ => {}
        }
    }
    head
}

/// The dictionary vocabulary's inline sims (lexigramma's names)
/// and the node name each lowers to, with which genos-borne
/// property the node carries (`kind` or `lang`).
fn dict_inline(name: &str) -> Option<(&'static str, Option<&'static str>)> {
    Some(match name {
        "variant" => ("variant", Some("kind")),
        "pronunciation" => ("pronunciation", Some("kind")),
        "syllabification" => ("syllabification", None),
        "grammar" => ("grammar", None),
        "grammar-label" => ("grammar-label", None),
        "etymology" => ("etymology", None),
        "usage-label" => ("label", Some("kind")),
        "equivalent" => ("equivalent", Some("lang")),
        "citation" => ("citation", None),
        "inflection" => ("inflection", Some("kind")),
        "languages" => ("languages", None),
        _ => return None,
    })
}

/// A genos as the spec's kind value: usage labels by their
/// lexigramma genoses, variants and pronunciations by theirs.
fn dict_kind(kind: &str, genos: &str) -> Option<&'static str> {
    Some(match (kind, genos) {
        ("label", "reg") => "register",
        ("label", "dom") => "domain",
        ("label", "geo") => "geography",
        ("label", "time") => "period",
        ("label", "lang") => "language",
        ("variant", "spelling") => "spelling",
        ("variant", "dialect") => "dialect",
        ("variant", "inflection" | "inflected") => "inflected",
        ("pronunciation", "ipa") => "ipa",
        ("pronunciation", "respell") => "respell",
        _ => return None,
    })
}

/// The literary reading (`lit:`) of a document file: every inline
/// simmere kept as a node named by its sim, its genoses as
/// traits, the aphanes monosims as its properties.
pub fn parse_lit_file(path: &Path) -> Result<TextModel, KoineError> {
    parse_lit_file_with(path, None)
}

/// [`parse_lit_file`] under a citation scheme: the
/// at-usfm chapter and verse markers become milestones with
/// full-path values under `scheme` (`scheme:jhn.3.16`), and
/// `::cite` answers under it.
pub fn parse_lit_file_with(path: &Path, scheme: Option<&str>) -> Result<TextModel, KoineError> {
    match atrep::check_any(path)? {
        Checked::Dialektos(_) => Err(KoineError::NotADocument),
        Checked::Document(mut doc) => {
            let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
            let dial = dialektos::resolve(&base, &doc.dialect_id)?;
            if let Some(s) = scheme {
                atrep::endo::usfm_apply_scheme(&mut doc, s);
            }
            kanonizo::tasso(&mut doc, &dial, &base)?;
            let head = head_meta(&doc, &dial);
            let mut model = finish_model(blocks_with(&doc, &dial, true));
            model.set_head_meta(head);
            if let Some(s) = scheme {
                model.set_cite_scheme(s);
            }
            Ok(model)
        }
    }
}

/// The literary reading of document text; `dir` anchors dialektos
/// resolution.
pub fn parse_lit_str(source: &str, dir: &Path) -> Result<TextModel, KoineError> {
    match atrep::check_source(source, &dir.join("<memory>.atd"))? {
        Checked::Dialektos(_) => Err(KoineError::NotADocument),
        Checked::Document(mut doc) => {
            let dial = dialektos::resolve(dir, &doc.dialect_id)?;
            kanonizo::tasso(&mut doc, &dial, dir)?;
            let head = head_meta(&doc, &dial);
            let mut model = finish_model(blocks_with(&doc, &dial, true));
            model.set_head_meta(head);
            Ok(model)
        }
    }
}

/// The literary reading of one of the literary XML vocabularies —
/// `"tei"`, `"usx"`, `"osis"`, `"fb2"` — through atrep's importer,
/// which emits the aphanes monosims from the source's attributes.
pub fn parse_lit_xml_as(text: &str, kind: &str) -> Result<TextModel, KoineError> {
    parse_lit_xml_as_with(text, kind, None)
}

/// [`parse_lit_xml_as`] under a citation scheme —
/// also the route for USFM (`"usfm"`), whose chapter and verse
/// markers the scheme turns into milestones.
pub fn parse_lit_xml_as_with(
    text: &str,
    kind: &str,
    scheme: Option<&str>,
) -> Result<TextModel, KoineError> {
    let mut doc = match kind {
        "usfm" => atrep::endo::usfm_to_document(text)?,
        // Under a scheme a TEI verse text gets line milestones
        // (`book.line` at every fifth `<l n>`) beside its own.
        "tei" => atrep::endo::tei_to_document_lines(text, scheme)?,
        "usx" => atrep::endo::usx_to_document(text)?,
        "osis" => atrep::endo::osis_to_document(text)?,
        "fb2" => atrep::fb2::fb2_to_document(text)?,
        "rnc" => atrep::epimerismos::rnc_to_document(text)?,
        "opencorpora" => atrep::epimerismos::opencorpora_to_document(text)?,
        "proiel" => atrep::epimerismos::proiel_to_document(text)?,
        other => return Err(KoineError::UnknownFormat(other.to_string())),
    };
    if let Some(s) = scheme {
        atrep::endo::usfm_apply_scheme(&mut doc, s);
    }
    let mut model = import_with(doc, true)?;
    if let Some(s) = scheme {
        model.set_cite_scheme(s);
    }
    Ok(model)
}

/// Lower a settled document into the block event stream —
/// exposed for tests and tooling.
pub fn blocks(doc: &atrep::Document, dial: &Dialektos) -> Vec<Block> {
    blocks_with(doc, dial, false)
}

/// [`blocks`] in the literary reading when `lit` is set: inline
/// simmeres become [`Block::Span`]s (byte offsets into the
/// normalized text of their block, genoses, aphanes fields), a
/// dialogue line a [`Block::Speech`], a milestone a
/// [`Block::Milestone`], a paragraph's own aphanes monosims a
/// [`Block::Annotate`]. The text level's flattening reading is
/// `lit = false`.
pub fn blocks_with(doc: &atrep::Document, dial: &Dialektos, lit: bool) -> Vec<Block> {
    let mut lower = Lower {
        out: Vec::new(),
        pending: Vec::new(),
        heading_depth: 0,
        lit,
        // The dictionary vocabulary (spec: The Dictionary
        // Vocabulary): lexigramma's sims lower to entries, senses
        // and the inline apparatus whatever the reading.
        dict: doc.dialect_id == "lexigramma",
        seen_entry: false,
        open_spans: Vec::new(),
        para_fields: Vec::new(),
        note_bodies: Vec::new(),
        notes: 0,
    };
    lower.seq(&doc.blocks, dial);
    lower.out
}

/// Import a Word document through atrep's importer (`atrep::docx`,
/// declared structure only — the parity with the native `text:`
/// reading) and lower it — `koine:report.docx`, the second route
/// beside `text:report.docx`.
pub fn parse_docx(bytes: &[u8]) -> Result<TextModel, KoineError> {
    import(atrep::docx::docx_to_document(bytes)?)
}

/// Import a Lingvo DSL dictionary (`.dsl`; the encoding by its
/// byte-order mark or `#SOURCE_CODE_PAGE` header) through atrep's
/// importer into lexigramma and lower it under the dictionary
/// vocabulary — `koine:webster.dsl`. `abbreviations` is the file
/// an `#INCLUDE` names, when the caller has it.
pub fn parse_dsl(bytes: &[u8], abbreviations: Option<&[u8]>) -> Result<TextModel, KoineError> {
    let doc = match abbreviations {
        Some(a) => atrep::dsl::dsl_to_document_with_abbreviations(bytes, a)?,
        None => atrep::dsl::dsl_to_document(bytes)?,
    };
    import(doc)
}

/// [`parse_dsl`] over a file: `.dsl.dz` (gzip) is decompressed,
/// and the file an `#INCLUDE` names is read beside it.
pub fn parse_dsl_file(path: &Path) -> Result<TextModel, KoineError> {
    let bytes = read_dsl_bytes(path)?;
    let abbreviations = match atrep::dsl::dsl_include(&bytes)? {
        Some(name) => {
            let p = path.parent().unwrap_or(Path::new(".")).join(&name);
            Some(read_dsl_bytes(&p)?)
        }
        None => None,
    };
    parse_dsl(&bytes, abbreviations.as_deref())
}

/// The file's bytes, a `.dz` member decompressed (gzip).
fn read_dsl_bytes(path: &Path) -> Result<Vec<u8>, KoineError> {
    let bytes = std::fs::read(path)?;
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("dz"))
        || bytes.starts_with(&[0x1f, 0x8b])
    {
        use std::io::Read;
        let mut out = Vec::new();
        flate2::read::MultiGzDecoder::new(&bytes[..]).read_to_end(&mut out)?;
        return Ok(out);
    }
    Ok(bytes)
}

/// Whether a path names a Lingvo DSL dictionary: `.dsl`, or
/// `.dsl.dz`.
pub fn is_dsl_path(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_ascii_lowercase())
        .unwrap_or_default();
    name.ends_with(".dsl") || name.ends_with(".dsl.dz")
}

/// The encoding a DSL file declares, for the source boundary's
/// record: a UTF-16 byte-order mark, else its `#SOURCE_CODE_PAGE`
/// header (Lingvo's names map onto the Encoding Standard's
/// labels), else nothing (UTF-8).
pub fn dsl_declared_encoding(bytes: &[u8]) -> Option<String> {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return Some("utf-16le".to_string());
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return Some("utf-16be".to_string());
    }
    let head = std::str::from_utf8(&bytes[..bytes.len().min(2048)]).ok()?;
    let line = head.lines().find(|l| l.starts_with("#SOURCE_CODE_PAGE"))?;
    let value = line.split('"').nth(1)?.trim();
    Some(match value {
        "Cyrillic" => "windows-1251".to_string(),
        "Latin" | "Western" => "windows-1252".to_string(),
        "EasternEuropean" => "windows-1250".to_string(),
        "Greek" => "windows-1253".to_string(),
        "Turkish" => "windows-1254".to_string(),
        "Hebrew" => "windows-1255".to_string(),
        "Arabic" => "windows-1256".to_string(),
        "Baltic" => "windows-1257".to_string(),
        "Vietnamese" => "windows-1258".to_string(),
        "Thai" => "windows-874".to_string(),
        other => other.to_string(),
    })
}

/// The flat heading endos (at-html and its heirs): the level
/// lives in the name.
fn flat_heading_level(name: &str) -> Option<u8> {
    let n = name.strip_prefix("heading-")?;
    n.parse::<u8>().ok().filter(|n| (1..=6).contains(n))
}

/// The enclosing heading sims. Level is nesting depth, so the
/// ladder order here is irrelevant; membership is what matters.
const HEADINGS: &[&str] = &[
    "title",
    "part",
    "chapter",
    "section",
    "subsection",
    "subsubsection",
    "subsubsubsection",
    "act",
    "scene",
];

/// Sims whose content is not part of the linear text.
const SKIP: &[&str] = &[
    "toc",
    "section-break",
    "ordinal",
    "manuscript-note",
    "index-marker",
];

/// Inline apparatus collected while flattening a paragraph,
/// emitted after its block in source order.
/// One bibliogramma entry block, lowered: the sim's lemma is the
/// citation key, its genos the genus, its `field` children the
/// campi — every term canonicalized through the bibliogramma
/// dialektos, so a bibliography authored in any covered language
/// (or imported from BibTeX/BibLaTeX, whose names are aliases)
/// lands on the same Latin canon.
fn bib_entry(block: &ABlock, bg: &Dialektos) -> Option<Block> {
    let ABlock::Para {
        lemma,
        children,
        ann,
        ..
    } = block
    else {
        return None;
    };
    let key = flat(lemma);
    if key.trim().is_empty() {
        return None;
    }
    let canon = |vocab: &str, term: &str| -> String {
        bg.vocabularies
            .get(vocab)
            .map(|v| v.canonicalize(term).to_string())
            .unwrap_or_else(|| term.to_string())
    };
    let genus = ann.genoses.first().map(|g| canon("genera", g));
    let mut fields = Vec::new();
    for f in children {
        if let ABlock::Para {
            lemma: fname,
            children: fval,
            ..
        } = f
        {
            let campus = canon("campi", &flat(fname));
            let value = fval.iter().map(block_text).collect::<Vec<_>>().join(" ");
            fields.push((campus, value));
        }
    }
    Some(Block::Bib {
        key,
        text: String::new(),
        fields,
        genus,
    })
}

/// Build the model and, when the document carried a
/// bibliography (an embedded bibliogramma englossis), attach
/// the field alias census so any covered name answers.
fn finish_model(blocks: Vec<Block>) -> TextModel {
    let has_bib = blocks.iter().any(|b| matches!(b, Block::Bib { .. }));
    let mut model = TextModel::build(blocks);
    if has_bib {
        attach_bib_aliases(&mut model);
    }
    model
}

/// Attach the bib-field alias census to a model another reading
/// built with `bib` entries under the Latin canon (Word's sources
/// list through quarb-text-docx), so `::author`, `::title`,
/// `::year` answer there too.
pub fn attach_bib_aliases(model: &mut TextModel) {
    if let Ok(bg) = dialektos::resolve(Path::new("."), "bibliogramma") {
        model.set_bib_aliases(bib_alias_census(&bg));
    }
}

/// The bib-field alias census: every campus alias (lowercased)
/// → its canonical Latin campus, from the bibliogramma
/// vocabulary — BibLaTeX's field names are its English rows, so
/// `::author` / `::journaltitle` answer beside `::auctor` /
/// `::ephemeris`, in any covered language.
fn bib_alias_census(bg: &Dialektos) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for vocab in ["campi", "genera"] {
        if let Some(v) = bg.vocabularies.get(vocab) {
            for (canon, aliases) in &v.terms {
                for a in aliases {
                    map.insert(a.to_lowercase(), canon.clone());
                }
            }
        }
    }
    map
}

/// Read a standalone BibTeX / BibLaTeX file as a text-level
/// document of `bib` entries — atrep's own importer parses it
/// into bibliogramma, and the same lowering serves both the
/// embedded englossis and the mounted file.
pub fn parse_bibtex(source: &str) -> Result<TextModel, KoineError> {
    let doc = atrep::endo::bibtex_to_document(source).map_err(KoineError::Atrep)?;
    let bg = dialektos::resolve(Path::new("."), "bibliogramma")?;
    let mut blocks = Vec::new();
    for b in &doc.blocks {
        if let Some(entry) = bib_entry(b, &bg) {
            blocks.push(entry);
        }
    }
    let mut model = TextModel::build(blocks);
    model.set_bib_aliases(bib_alias_census(&bg));
    Ok(model)
}

enum Pending {
    /// A note callout: the body's onym, its family, and the
    /// callout's offset in the block when the note was inline.
    Note(String, NoteFamily, Option<u32>),
    Mark(String),
    /// A mention met in flow: (target, internal).
    Ref(String, bool),
    /// A standalone onym anchor: a point bearer at this position.
    Point(String),
    /// A citation mark: the key, and the locator when the cite
    /// monosim opened an endo-diaphane whose text is the pinpoint.
    Cite(String, Option<String>),
    /// The literary reading: an inline simmere as a span of the
    /// block being flattened (pre-order: pushed at its start, the
    /// end filled in after its content).
    Span {
        kind: String,
        lo: u32,
        hi: u32,
        genoses: Vec<String>,
        fields: Vec<(String, String)>,
    },
    /// The literary reading: a milestone at a byte offset.
    Milestone {
        scheme: String,
        value: String,
        at: u32,
    },
    /// The dictionary vocabulary: an inline node with text of its
    /// own that is not in the prose (an inflection lookup key).
    Hidden {
        kind: String,
        text: String,
        genoses: Vec<String>,
        fields: Vec<(String, String)>,
    },
    /// The parsing pack: a token the source parsed,
    /// at byte offsets, with its parsings and its onym.
    Parsed {
        lo: u32,
        hi: u32,
        onym: Option<String>,
        parsings: Vec<quarb_text::Parsing>,
    },
    /// The parsing pack: a declared sentence at byte offsets.
    Periodos {
        lo: u32,
        hi: u32,
        id: String,
    },
}

/// at-epimerismos, the parsing pack: its monosims by symbol.
const PERIODOS: &str = "!.";
const LEXEMA: &str = "!=";
const MEROS: &str = "!/";
const PAREPOMENA: &str = "!%";
const SEMASIA: &str = "!&";
const KEPHALE: &str = "!>";
const SCHESIS: &str = "!-";

fn is_pack(symbol: &str) -> bool {
    matches!(
        symbol,
        LEXEMA | MEROS | PAREPOMENA | SEMASIA | KEPHALE | SCHESIS
    )
}

/// The parsings a token diaphane opens with: its leading pack
/// monosims, a lexema opening each parsing.
fn parsings_of(content: &[Inline]) -> Vec<quarb_text::Parsing> {
    let mut out: Vec<quarb_text::Parsing> = Vec::new();
    for inline in content {
        let Inline::Monosim { symbol, param, .. } = inline else {
            break;
        };
        if !is_pack(symbol) {
            break;
        }
        if out.is_empty() || (symbol == LEXEMA && out.last().is_some_and(|p| p.lexema.is_some())) {
            out.push(quarb_text::Parsing::default());
        }
        let p = out.last_mut().expect("opened");
        let v = Some(param.clone());
        match symbol.as_str() {
            LEXEMA => p.lexema = v,
            MEROS => p.meros = v,
            PAREPOMENA => p.parepomena = v,
            SEMASIA => p.semasia = v,
            KEPHALE => p.kephale = v,
            SCHESIS => p.schesis = v,
            _ => {}
        }
    }
    out
}

struct Lower {
    out: Vec<Block>,
    pending: Vec<Pending>,
    heading_depth: u8,
    /// The literary reading is on: keep the inline simmeres.
    lit: bool,
    /// The document is a dictionary (lexigramma): entries, senses
    /// and the inline apparatus lower under the dictionary
    /// vocabulary, whatever the reading.
    dict: bool,
    /// An entry has been met: a definition list before the first
    /// entry is the abbreviations list.
    seen_entry: bool,
    /// Indices into `pending` of the spans open around the text
    /// being flattened (innermost last): an aphanes monosim
    /// annotates the innermost open span.
    open_spans: Vec<usize>,
    /// Aphanes monosims met outside any span: the block's own.
    para_fields: Vec<(String, String)>,
    /// The bodies of the inline notes met while flattening the
    /// current block (onym, family, text), emitted after it.
    note_bodies: Vec<(String, NoteFamily, String)>,
    /// A running number for the inline notes' onyms.
    notes: u32,
}

/// Whether a monosim is an at-aphanes sim: the unseen pack's
/// symbols all begin with `?`. Its sim name (`prosopon`, `chora`,
/// `skopos`, …) is the property the value answers under.
fn is_aphanes(symbol: &str) -> bool {
    symbol.starts_with('?')
}

/// Push text with whitespace normalized as `normalize_ws` would:
/// runs collapse to one space, none at the start. Offsets taken
/// on the result survive the builder's own normalization.
fn push_norm(out: &mut String, s: &str) {
    for c in s.chars() {
        if c.is_whitespace() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
}

impl Lower {
    fn block(&mut self, block: &ABlock, dial: &Dialektos) {
        match block {
            ABlock::Paragraph(inlines) => {
                // A flat heading — a heading-N endo alone in its
                // paragraph (the at-html family): the level lives
                // in the name; the shared derivation builds the
                // outline as it does for h1/h2.
                if let [
                    Inline::Endo {
                        symbol,
                        content,
                        ann,
                        ..
                    },
                ] = inlines.as_slice()
                    && let Some(level) = flat_heading_level(&sim_name(dial, symbol))
                {
                    let lemma = self.flatten(content, dial);
                    self.out.push(Block::Heading { level, lemma });
                    if let Some(onym) = &ann.onym {
                        self.out.push(Block::Label { onym: onym.clone() });
                    }
                    self.drain_pending();
                    return;
                }
                // The literary reading: a dramatis-persona line
                // (at-drama `@:! name !:@`, a TEI castItem) is a cast
                // entry — the name its lemma, an onym its id.
                if self.lit
                    && let [
                        Inline::Endo {
                            symbol,
                            content,
                            ann,
                            ..
                        },
                    ] = inlines.as_slice()
                    && sim_name(dial, symbol) == "dramatis-persona"
                {
                    let lemma = self.flatten(content, dial);
                    self.out.push(Block::Character {
                        lemma,
                        onym: ann.onym.clone(),
                        text: String::new(),
                        genoses: ann.genoses.clone(),
                    });
                    self.drain_pending();
                    return;
                }
                // The literary reading: a paragraph that is one
                // dialogue line (at-drama `@:-…-:@`, TEI's
                // whole-paragraph `<said>`) is a speech block; the
                // aphanes monosims first inside it are its fields.
                if self.lit
                    && let [
                        Inline::Endo {
                            symbol,
                            content,
                            ann,
                            ..
                        },
                    ] = inlines.as_slice()
                    && sim_name(dial, symbol) == "dialogue-line"
                {
                    let text = self.flatten(content, dial);
                    let mut fields = std::mem::take(&mut self.para_fields);
                    fields.retain(|(k, _)| !k.is_empty());
                    self.out.push(Block::Speech {
                        text,
                        genoses: ann.genoses.clone(),
                        fields,
                    });
                    self.drain_pending();
                    return;
                }
                let text = self.flatten(inlines, dial);
                // A paragraph that is only a milestone (a chapter
                // marker between paragraphs) has no prose of its
                // own: the milestone stands at the END of the flow
                // block before it, not at offset 0 of it.
                if text.is_empty() {
                    for pending in &mut self.pending {
                        if let Pending::Milestone { at, .. } = pending {
                            *at = u32::MAX;
                        }
                    }
                }
                self.out.push(Block::Paragraph { text });
                if self.lit && !self.para_fields.is_empty() {
                    let fields = std::mem::take(&mut self.para_fields);
                    self.out.push(Block::Annotate { fields });
                }
                self.drain_pending();
            }
            ABlock::Para {
                symbol,
                taxis,
                lemma,
                children,
                hypograph,
                ann,
                ..
            } => {
                let name = sim_name(dial, symbol);
                let name = name.as_str();
                if SKIP.contains(&name) {
                    return;
                }
                let taxis_n = match taxis {
                    Some(Taxis::Explicit(n)) => Some(*n as i64),
                    _ => None,
                };
                match name {
                    // The dictionary vocabulary: an entry is a
                    // record named by its headword (the lemma; the
                    // taxis its homograph number), a sense a block
                    // under it numbered at its depth, nested as the
                    // dictionary subdivides; the definition is the
                    // sense's own paragraphs.
                    "entry" if self.dict => {
                        self.seen_entry = true;
                        let lemma = self.flatten(lemma, dial);
                        self.out.push(Block::Open {
                            kind: Container::Entry {
                                taxis: taxis_n,
                                onym: ann.onym.clone(),
                            },
                            lemma: (!lemma.is_empty()).then_some(lemma),
                        });
                        self.drain_pending();
                        self.seq(children, dial);
                        self.out.push(Block::Close { hypograph: None });
                    }
                    "sense" if self.dict => {
                        let lemma = self.flatten(lemma, dial);
                        self.out.push(Block::Open {
                            kind: Container::Sense { taxis: taxis_n },
                            lemma: (!lemma.is_empty()).then_some(lemma),
                        });
                        self.drain_pending();
                        self.seq(children, dial);
                        self.out.push(Block::Close { hypograph: None });
                    }
                    // The table model (litogramma's table / row /
                    // header-row / cell sims, the spans as monosims
                    // first inside a cell): rows are records, cells
                    // fields named by their column — the shared
                    // table rule's shape.
                    "table" => {
                        self.table_rows(lemma, children, hypograph, dial);
                    }
                    // The abbreviations list (lexigramma's
                    // `abbreviations` sim, `@[[ … ]]@`): the
                    // definition list it wraps is the list of that
                    // kind, which the labels resolve against.
                    "abbreviations" if self.dict => {
                        for child in children {
                            match child {
                                ABlock::Para {
                                    symbol, children, ..
                                } if sim_name(dial, symbol) == "definition-list" => {
                                    self.out.push(Block::Open {
                                        kind: Container::UnorderedList,
                                        lemma: None,
                                    });
                                    self.out.push(Block::Annotate {
                                        fields: vec![(
                                            "kind".to_string(),
                                            "abbreviations".to_string(),
                                        )],
                                    });
                                    for item in children {
                                        self.block(item, dial);
                                    }
                                    self.out.push(Block::Close { hypograph: None });
                                }
                                other => self.block(other, dial),
                            }
                        }
                    }
                    _ if HEADINGS.contains(&name) => {
                        let lemma = self.flatten(lemma, dial);
                        self.heading_depth += 1;
                        self.out.push(Block::Heading {
                            level: self.heading_depth,
                            lemma,
                        });
                        // The section's own onym (`#@(name)`)
                        // names it — the block-style bearer.
                        if let Some(onym) = &ann.onym {
                            self.out.push(Block::Label { onym: onym.clone() });
                        }
                        self.drain_pending();
                        self.seq(children, dial);
                        self.heading_depth -= 1;
                    }
                    "footnote" | "endnote" | "chapter-endnote" | "aside" => {
                        let family = match name {
                            "footnote" => NoteFamily::Footnote,
                            "aside" => NoteFamily::Aside,
                            // chapter-endnote reads as the endnote
                            // family in this version (placement).
                            _ => NoteFamily::Endnote,
                        };
                        self.out.push(Block::Open {
                            kind: Container::Note {
                                onym: ann.onym.clone().unwrap_or_default(),
                                family,
                                margin: false,
                            },
                            lemma: None,
                        });
                        self.seq(children, dial);
                        self.out.push(Block::Close { hypograph: None });
                    }
                    "unordered-list" | "ordered-list" | "definition-list" => {
                        self.out.push(Block::Open {
                            kind: if name == "ordered-list" {
                                Container::OrderedList { start: 1 }
                            } else {
                                Container::UnorderedList
                            },
                            lemma: None,
                        });
                        // The dictionary vocabulary: a definition
                        // list standing before the first entry of a
                        // dictionary, outside the `abbreviations`
                        // sim (a hand-written list left undeclared,
                        // or the importer's shape before the sim),
                        // is the abbreviations list as well.
                        if name == "definition-list" && self.dict && !self.seen_entry {
                            self.out.push(Block::Annotate {
                                fields: vec![("kind".to_string(), "abbreviations".to_string())],
                            });
                        }
                        for child in children {
                            self.block(child, dial);
                        }
                        self.out.push(Block::Close { hypograph: None });
                    }
                    "unordered-item" | "ordered-item" | "definition-item" => {
                        let lemma = self.flatten(lemma, dial);
                        self.out.push(Block::Open {
                            kind: Container::Item,
                            lemma: (!lemma.is_empty()).then_some(lemma),
                        });
                        self.seq(children, dial);
                        self.out.push(Block::Close { hypograph: None });
                    }
                    "blockquote" | "epigraph" => {
                        self.out.push(Block::Open {
                            kind: Container::Blockquote,
                            lemma: None,
                        });
                        self.seq(children, dial);
                        let hypograph = self.flatten(hypograph, dial);
                        self.out.push(Block::Close {
                            hypograph: (!hypograph.is_empty()).then_some(hypograph),
                        });
                        self.drain_pending();
                    }
                    // A `definition` (@;) flows inside its item.
                    "definition" => {
                        self.seq(children, dial);
                    }
                    // A `dialogue` (@:) — TEI's `<said who>` and
                    // `<sp><speaker>` arrive as this sim — is one
                    // paragraph whose lemma names the speaker, never
                    // a label paragraph before the speech: the
                    // first paragraph the speech lowers to takes the
                    // speaker as its lemma, the rest of the body
                    // flows after it.
                    "dialogue" | ":" => {
                        let speaker = self.flatten(lemma, dial);
                        let at = self.out.len();
                        self.seq(children, dial);
                        if !speaker.is_empty()
                            && let Some(Block::Paragraph { text }) = self.out.get(at)
                        {
                            let text = text.clone();
                            self.out[at] = Block::Dialogue {
                                lemma: speaker,
                                text,
                                lit: self.lit,
                            };
                        }
                        let hypograph = self.flatten(hypograph, dial);
                        if !hypograph.is_empty() {
                            self.out.push(Block::Paragraph { text: hypograph });
                            self.drain_pending();
                        }
                    }
                    // The literary reading: a cast entry with a
                    // description (at-drama `@:!! name … !!:@(id)`)
                    // is a character node, the description its text.
                    "character" if self.lit => {
                        let lemma = self.flatten(lemma, dial);
                        let text = children
                            .iter()
                            .filter_map(|c| match c {
                                ABlock::Paragraph(inlines) => Some(self.flatten(inlines, dial)),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        self.out.push(Block::Character {
                            lemma,
                            onym: ann.onym.clone(),
                            text,
                            genoses: ann.genoses.clone(),
                        });
                        self.drain_pending();
                    }
                    _ => {
                        // Unknown para-simmere: transparent — the
                        // lemma opens as a paragraph, children
                        // flow, the hypograph closes as one.
                        let at = self.out.len();
                        let lemma = self.flatten(lemma, dial);
                        if !lemma.is_empty() {
                            self.out.push(Block::Paragraph { text: lemma });
                            self.drain_pending();
                        }
                        self.seq(children, dial);
                        let hypograph = self.flatten(hypograph, dial);
                        if !hypograph.is_empty() {
                            self.out.push(Block::Paragraph { text: hypograph });
                            self.drain_pending();
                        }
                        // The literary reading: the paragraphs of a
                        // named simmere with no reading of its own
                        // wear its sim as a trait — the block form's
                        // `-block` dropped, since the paragraph kind
                        // already says block (at-drama's
                        // `stage-direction-block`, TEI's block
                        // `<stage>`: `paragraph<stage-direction>`) —
                        // and its genoses with it.
                        if self.lit && dial.sims.contains_key(symbol.as_str()) {
                            let mut genoses = vec![name.trim_end_matches("-block").to_string()];
                            genoses.extend(ann.genoses.iter().cloned());
                            let tail: Vec<Block> = self.out.drain(at..).collect();
                            for block in tail {
                                let para = matches!(block, Block::Paragraph { .. });
                                self.out.push(block);
                                if para {
                                    self.out.push(Block::Wear {
                                        genoses: genoses.clone(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
            ABlock::Stichoi {
                symbol,
                lemma,
                strophes,
                hypograph,
                ..
            } => {
                let name = symbol
                    .as_deref()
                    .map(|s| sim_name(dial, s))
                    .unwrap_or_else(|| "stichoi".to_string());
                if name == "table" {
                    self.table(lemma, strophes, hypograph, dial);
                } else {
                    // The verse vocabulary: stichoi
                    // lower as verse/strophe/stichos, the line
                    // taxis the citation coordinate.
                    let lemma = self.flatten(lemma, dial);
                    let hypograph = self.flatten(hypograph, dial);
                    // Deixes inside stichos lines collect and
                    // anchor at the verse block (line-level
                    // anchoring is the recorded follow-up).
                    let strophes = strophes
                        .iter()
                        .map(|s| s.0.iter().map(|line| self.flatten(line, dial)).collect())
                        .collect();
                    self.out.push(Block::Verse {
                        lemma: (!lemma.is_empty()).then_some(lemma),
                        strophes,
                        hypograph: (!hypograph.is_empty()).then_some(hypograph),
                    });
                    self.drain_pending();
                }
            }
            ABlock::ParaDiaphane { children, .. } => {
                self.seq(children, dial);
            }
            ABlock::VerbatimBlock { content, ann } => {
                self.out.push(Block::Verbatim {
                    lang: ann.genoses.first().cloned(),
                    // As authored, without the block's own frame
                    // newlines (the other adapters' behavior).
                    text: content.trim_matches('\n').to_string(),
                });
            }
            ABlock::MonadEnglossis {
                dialect, children, ..
            } => {
                // An embedded bibliogramma is the bibliography:
                // its entries become bib blocks, campi and genera
                // canonicalized to the Latin vocabulary. Any other
                // foreign-dialect subtree reads as verbatim in
                // that dialect's name.
                if dialect == "bibliogramma"
                    && let Ok(bg) = dialektos::resolve(Path::new("."), "bibliogramma")
                {
                    for child in children {
                        if let Some(b) = bib_entry(child, &bg) {
                            self.out.push(b);
                        }
                    }
                    return;
                }
                let text = children
                    .iter()
                    .map(block_text)
                    .collect::<Vec<_>>()
                    .join("\n\n");
                self.out.push(Block::Verbatim {
                    lang: Some(dialect.clone()),
                    text,
                });
            }
            // Media, transclusions, and axiomata are not the
            // linear text.
            ABlock::Enmedia { .. }
            | ABlock::EnmediaHashed { .. }
            | ABlock::AnaphorEnglossis { .. }
            | ABlock::AnaphorEnlexis { .. }
            | ABlock::ParaAxioma { .. }
            | ABlock::AxiomaRefBlock { .. } => {}
        }
    }

    /// Lower the table model: `row` / `header-row` children, each
    /// holding `cell` blocks (the optional lemma names the column
    /// explicitly, winning over the header's); an `hspan` /
    /// `vspan` monosim first inside a cell covers the cells the
    /// columns to the right / the rows below omit, which become
    /// empty cells so every row stays a record of the same width.
    fn table_rows(
        &mut self,
        lemma: &[Inline],
        children: &[ABlock],
        hypograph: &[Inline],
        dial: &Dialektos,
    ) {
        let mut headers: Option<Vec<String>> = None;
        let mut rows: Vec<Vec<Cell>> = Vec::new();
        // Columns a cell above spans into: (column, rows left).
        let mut reserved: Vec<(usize, usize)> = Vec::new();
        let take_reserved = |reserved: &mut Vec<(usize, usize)>, col: usize| -> bool {
            match reserved.iter().position(|(c, _)| *c == col) {
                Some(i) => {
                    reserved[i].1 -= 1;
                    if reserved[i].1 == 0 {
                        reserved.remove(i);
                    }
                    true
                }
                None => false,
            }
        };
        for child in children {
            let ABlock::Para {
                symbol, children, ..
            } = child
            else {
                continue;
            };
            let kind = sim_name(dial, symbol);
            if kind != "row" && kind != "header-row" {
                continue;
            }
            let mut row: Vec<Cell> = Vec::new();
            let mut col = 0usize;
            let mut cells = children.iter().filter_map(|c| match c {
                ABlock::Para {
                    symbol,
                    lemma,
                    children,
                    ..
                } if sim_name(dial, symbol) == "cell" => Some((lemma, children)),
                _ => None,
            });
            loop {
                if take_reserved(&mut reserved, col) {
                    row.push(Cell::from(""));
                    col += 1;
                    continue;
                }
                let Some((lemma, blocks)) = cells.next() else {
                    break;
                };
                let (text, hspan, vspan) = self.cell_text(blocks, dial);
                let label = self.flatten(lemma, dial);
                row.push(Cell {
                    label: (!label.is_empty()).then_some(label),
                    text,
                });
                if vspan > 1 {
                    reserved.push((col, vspan - 1));
                }
                col += 1;
                for _ in 1..hspan {
                    row.push(Cell::from(""));
                    col += 1;
                }
            }
            while take_reserved(&mut reserved, col) {
                row.push(Cell::from(""));
                col += 1;
            }
            if kind == "header-row" {
                if headers.is_none() {
                    headers = Some(row.into_iter().map(|c| c.text).collect());
                }
            } else {
                rows.push(row);
            }
        }
        let lemma = self.flatten(lemma, dial);
        let hypograph = self.flatten(hypograph, dial);
        let caption = if !lemma.is_empty() {
            Some(lemma)
        } else if !hypograph.is_empty() {
            Some(hypograph)
        } else {
            None
        };
        self.out.push(Block::Table {
            lemma: caption,
            headers,
            rows,
        });
        self.drain_pending();
    }

    /// A cell's text (its blocks' prose, joined) and its spans
    /// (1 when none): the `hspan` / `vspan` monosims stand first
    /// inside the cell's first paragraph.
    fn cell_text(&mut self, blocks: &[ABlock], dial: &Dialektos) -> (String, usize, usize) {
        let mut hspan = 1usize;
        let mut vspan = 1usize;
        let mut parts: Vec<String> = Vec::new();
        for (i, block) in blocks.iter().enumerate() {
            match block {
                ABlock::Paragraph(inlines) => {
                    let mut rest: &[Inline] = inlines;
                    if i == 0 {
                        while let Some(Inline::Monosim { symbol, param, .. }) = rest.first() {
                            match sim_name(dial, symbol).as_str() {
                                "hspan" => hspan = param.trim().parse().unwrap_or(1).max(1),
                                "vspan" => vspan = param.trim().parse().unwrap_or(1).max(1),
                                _ => break,
                            }
                            rest = &rest[1..];
                        }
                    }
                    let text = self.flatten(rest, dial);
                    if !text.is_empty() {
                        parts.push(text);
                    }
                }
                other => {
                    let text = block_text(other);
                    if !text.trim().is_empty() {
                        parts.push(quarb_text::normalize_ws(&text));
                    }
                }
            }
        }
        (parts.join(" "), hspan, vspan)
    }

    /// Lower litogramma's stichoi table: one row per line, cells
    /// split on pipes, a dash-run line separating the header row.
    fn table(
        &mut self,
        lemma: &[Inline],
        strophes: &[Strophe],
        hypograph: &[Inline],
        dial: &Dialektos,
    ) {
        let lines: Vec<String> = strophes
            .iter()
            .flat_map(|s| s.0.iter().map(|line| flat(line)))
            .collect();
        let is_rule = |l: &str| {
            !l.trim().is_empty() && l.trim().chars().all(|c| c == '-' || c.is_whitespace())
        };
        let split = lines.iter().position(|l| is_rule(l));
        let (headers, body): (Option<Vec<String>>, &[String]) = match split {
            Some(i) => (
                lines.get(i.wrapping_sub(1)).map(|h| cells_of(h)),
                &lines[i + 1..],
            ),
            None => (None, &lines[..]),
        };
        let rows: Vec<Vec<Cell>> = body
            .iter()
            .filter(|l| !is_rule(l) && !l.trim().is_empty())
            .map(|l| cells_of(l).into_iter().map(Cell::from).collect())
            .collect();
        let lemma = self.flatten(lemma, dial);
        let hypograph = self.flatten(hypograph, dial);
        let caption = if !lemma.is_empty() {
            Some(lemma)
        } else if !hypograph.is_empty() {
            Some(hypograph)
        } else {
            None
        };
        self.out.push(Block::Table {
            lemma: caption,
            headers,
            rows,
        });
    }

    /// Flatten inline content to prose, collecting the apparatus:
    /// a deixis contributes its callout (and no marker text), an
    /// index endo its mark (the displayed segment stays in the
    /// prose; `index-hidden` leaves none), reference monosims keep
    /// their key as written.
    fn flatten(&mut self, inlines: &[Inline], dial: &Dialektos) -> String {
        let mut out = String::new();
        self.flatten_into(inlines, dial, &mut out);
        // `push_norm` left at most one trailing space; the result
        // is what `normalize_ws` would give, so the literary
        // reading's offsets hold.
        let trimmed = out.trim_end().len();
        out.truncate(trimmed);
        out
    }

    fn flatten_into(&mut self, inlines: &[Inline], dial: &Dialektos, out: &mut String) {
        for inline in inlines {
            match inline {
                Inline::Text(t) => push_norm(out, t),
                Inline::Endo {
                    symbol,
                    content,
                    ann,
                    ..
                } => match sim_name(dial, symbol).as_str() {
                    "index" | "index-hidden" => {
                        let term = flat(content);
                        if sim_name(dial, symbol) == "index" {
                            // The last `>` segment is displayed.
                            out.push_str(term.rsplit('>').next().unwrap_or(&term).trim());
                        }
                        self.pending.push(Pending::Mark(term));
                    }
                    // The link endo: the grammata IS the target
                    // (at-org's own definition), and the endo
                    // importers emit the bare `><` symbol across
                    // dialects — so both spellings answer. The
                    // target stays in the prose (the importers'
                    // "desc (url)" normalization already put the
                    // authored text beside it) and the mention
                    // becomes a ref node; a `#fragment` target is
                    // internal.
                    // An inline-bodied note (at-usfm's `\f`, a
                    // `note` simmere): the body is a note of its
                    // family, never the block's prose; the callout
                    // stands at its offset.
                    "note" => {
                        let family = if ann.genoses.iter().any(|g| g == "fe") {
                            NoteFamily::Endnote
                        } else {
                            NoteFamily::Footnote
                        };
                        self.notes += 1;
                        let onym = format!("note-{}", self.notes);
                        let text = flat(content);
                        self.pending.push(Pending::Note(
                            onym.clone(),
                            family,
                            Some(out.len() as u32),
                        ));
                        self.note_bodies.push((onym, family, text));
                    }
                    name if name == "link" || symbol == "><" => {
                        let target = flat(content);
                        self.flatten_into(content, dial, out);
                        let t = target.trim().to_string();
                        if !t.is_empty() {
                            let internal = t.starts_with('#');
                            self.pending.push(Pending::Ref(t, internal));
                        }
                    }
                    // The dictionary vocabulary: the inline
                    // apparatus — a variant, a pronunciation, the
                    // grammatical line and labels, the etymology, a
                    // usage label, an equivalent, a citation — is
                    // a span named by its sim with its kind (or
                    // language) from the genos, whatever the
                    // reading; an inflection is a hidden node
                    // (never in the prose); the languages line is
                    // head metadata.
                    name if self.dict && dict_inline(name).is_some() => {
                        let (kind, field) = dict_inline(name).expect("checked");
                        if kind == "inflection" {
                            let mut fields = Vec::new();
                            if ann.genoses.iter().any(|g| g == "sort") {
                                fields.push(("kind".to_string(), "sort".to_string()));
                            }
                            self.pending.push(Pending::Hidden {
                                kind: kind.to_string(),
                                text: flat(content),
                                genoses: Vec::new(),
                                fields,
                            });
                            continue;
                        }
                        if kind == "languages" {
                            continue;
                        }
                        let mut fields = Vec::new();
                        let mut genoses = Vec::new();
                        for g in &ann.genoses {
                            match (field, g.as_str()) {
                                (Some("lang"), lang) => {
                                    fields.push(("lang".to_string(), lang.to_string()))
                                }
                                (Some("kind"), g) => match dict_kind(kind, g) {
                                    Some(k) => fields.push(("kind".to_string(), k.to_string())),
                                    None => genoses.push(g.to_string()),
                                },
                                _ => genoses.push(g.to_string()),
                            }
                        }
                        let idx = self.pending.len();
                        self.pending.push(Pending::Span {
                            kind: kind.to_string(),
                            lo: out.len() as u32,
                            hi: 0,
                            genoses,
                            fields,
                        });
                        self.open_spans.push(idx);
                        self.flatten_into(content, dial, out);
                        self.open_spans.pop();
                        let hi = out.trim_end().len() as u32;
                        if let Pending::Span { lo, hi: h, .. } = &mut self.pending[idx] {
                            while (*lo as usize) < hi as usize
                                && out.as_bytes()[*lo as usize] == b' '
                            {
                                *lo += 1;
                            }
                            *h = hi.max(*lo);
                        }
                    }
                    // The literary reading keeps every other
                    // inline simmere as a span named by its sim,
                    // in pre-order: its entry is pushed before its
                    // content is flattened and closed after.
                    name if self.lit => {
                        let idx = self.pending.len();
                        self.pending.push(Pending::Span {
                            kind: name.to_string(),
                            lo: out.len() as u32,
                            hi: 0,
                            genoses: ann.genoses.clone(),
                            fields: Vec::new(),
                        });
                        self.open_spans.push(idx);
                        self.flatten_into(content, dial, out);
                        self.open_spans.pop();
                        let hi = out.trim_end().len() as u32;
                        if let Pending::Span { lo, hi: h, .. } = &mut self.pending[idx] {
                            // A span never starts on the space
                            // that separates it from the word before.
                            while (*lo as usize) < hi as usize
                                && out.as_bytes()[*lo as usize] == b' '
                            {
                                *lo += 1;
                            }
                            *h = hi.max(*lo);
                        }
                    }
                    _ => self.flatten_into(content, dial, out),
                },
                // The parsing pack: a diaphane opened
                // by the periodos is a declared sentence, one
                // opened by a token monosim a parsed token — both
                // at their offsets in the prose, the monosims
                // never in it.
                Inline::EndoDiaphane { content, ann } => match content.first() {
                    // The dictionary vocabulary: text shown only in
                    // a full reading (DSL's secondary text) is a
                    // span of kind `secondary`.
                    _ if self.dict && ann.genoses.iter().any(|g| g == "secondary") => {
                        let idx = self.pending.len();
                        self.pending.push(Pending::Span {
                            kind: "span".to_string(),
                            lo: out.len() as u32,
                            hi: 0,
                            genoses: Vec::new(),
                            fields: vec![("kind".to_string(), "secondary".to_string())],
                        });
                        self.open_spans.push(idx);
                        self.flatten_into(content, dial, out);
                        self.open_spans.pop();
                        let hi = out.trim_end().len() as u32;
                        if let Pending::Span { lo, hi: h, .. } = &mut self.pending[idx] {
                            while (*lo as usize) < hi as usize
                                && out.as_bytes()[*lo as usize] == b' '
                            {
                                *lo += 1;
                            }
                            *h = hi.max(*lo);
                        }
                    }
                    Some(Inline::Monosim { symbol, param, .. }) if symbol == PERIODOS => {
                        let lo = out.len() as u32;
                        self.flatten_into(content, dial, out);
                        let hi = out.trim_end().len() as u32;
                        self.pending.push(Pending::Periodos {
                            lo,
                            hi: hi.max(lo),
                            id: param.clone(),
                        });
                    }
                    // A cite monosim opening the diaphane annotates
                    // its span: one mark, the key as target, the
                    // span's text as the locator (`@@.@>[(key)ch.
                    // 15.@@`); the text stays in the prose, the key
                    // does not.
                    Some(Inline::Monosim { symbol, param, .. })
                        if sim_name(dial, symbol) == "cite" =>
                    {
                        let rest: Vec<Inline> = content.iter().skip(1).cloned().collect();
                        let mut text = String::new();
                        self.flatten_into(&rest, dial, &mut text);
                        let locator = Some(text.trim().to_string()).filter(|t| !t.is_empty());
                        out.push_str(&text);
                        self.pending.push(Pending::Cite(param.clone(), locator));
                    }
                    // An editorial pair (TEI's `<choice>`, at-aphanes'
                    // paradosis): the reading in the prose, the
                    // original as the `choice` span's field under the
                    // literary reading (atrep's parser has already
                    // decoded the parameter's escaped spaces).
                    Some(Inline::Monosim { symbol, param, .. })
                        if is_aphanes(symbol) && self.lit =>
                    {
                        let key = sim_name(dial, symbol);
                        let idx = self.pending.len();
                        self.pending.push(Pending::Span {
                            kind: "choice".to_string(),
                            lo: out.len() as u32,
                            hi: 0,
                            genoses: ann.genoses.clone(),
                            fields: vec![(key, param.clone())],
                        });
                        self.open_spans.push(idx);
                        let rest: Vec<Inline> = content.iter().skip(1).cloned().collect();
                        self.flatten_into(&rest, dial, out);
                        self.open_spans.pop();
                        let hi = out.trim_end().len() as u32;
                        if let Pending::Span { lo, hi: h, .. } = &mut self.pending[idx] {
                            while (*lo as usize) < hi as usize
                                && out.as_bytes()[*lo as usize] == b' '
                            {
                                *lo += 1;
                            }
                            *h = hi.max(*lo);
                        }
                    }
                    Some(Inline::Monosim { symbol, .. }) if is_pack(symbol) => {
                        let lo = out.len() as u32;
                        let parsings = parsings_of(content);
                        self.flatten_into(content, dial, out);
                        let hi = out.trim_end().len() as u32;
                        self.pending.push(Pending::Parsed {
                            lo,
                            hi: hi.max(lo),
                            onym: ann.onym.clone(),
                            parsings,
                        });
                    }
                    _ => self.flatten_into(content, dial, out),
                },
                // A pack monosim is analysis, never prose.
                Inline::Monosim { symbol, .. } if symbol == PERIODOS || is_pack(symbol) => {}
                Inline::VerbatimInline { content, .. } => push_norm(out, content),
                // An aphanes monosim (`@?:(polly)`) is unseen
                // metadata: never prose. On the literary reading
                // it is a field of the innermost open span, else
                // of the block itself.
                Inline::Monosim { symbol, param, .. } if is_aphanes(symbol) => {
                    if self.lit {
                        let key = sim_name(dial, symbol);
                        match self.open_spans.last() {
                            Some(&idx) => {
                                if let Pending::Span { fields, .. } = &mut self.pending[idx] {
                                    fields.push((key, param.clone()));
                                }
                            }
                            None => self.para_fields.push((key, param.clone())),
                        }
                    }
                }
                Inline::Monosim { symbol, param, .. } => {
                    // The ref family: the key stays in the prose
                    // (as before) and the mention becomes a ref
                    // node, internal by construction. `cite` keys
                    // are a namespace of their own (the
                    // bibliogramma) and stay prose until that
                    // story lands. Every other monosim is a marker
                    // (a chapter or verse number, a milestone the
                    // scheme option turns into a node): metadata,
                    // never prose.
                    match sim_name(dial, symbol).as_str() {
                        "ref" | "heading-ref" | "page-ref" | "line-ref" => {
                            push_norm(out, param);
                            self.pending.push(Pending::Ref(param.clone(), true));
                        }
                        // The citation namespace is its own: cite
                        // marks resolve against bib entries (an
                        // embedded bibliogramma's entries are the
                        // recorded follow-up).
                        "cite" => {
                            push_norm(out, param);
                            self.pending.push(Pending::Cite(param.clone(), None));
                        }
                        _ => {}
                    }
                }
                Inline::Deixis { symbol, onym, .. } => {
                    let family = match sim_name(dial, symbol).as_str() {
                        "footnote" => Some(NoteFamily::Footnote),
                        "endnote" | "chapter-endnote" => Some(NoteFamily::Endnote),
                        "aside" => Some(NoteFamily::Aside),
                        _ => None,
                    };
                    if let Some(family) = family {
                        self.pending.push(Pending::Note(onym.clone(), family, None));
                    }
                }
                Inline::OnymAnchor(o) => {
                    // A standalone anchor bears its onym at this
                    // position — the point style.
                    self.pending.push(Pending::Point(o.clone()));
                }
                // The literary reading keeps a milestone as a
                // point in the prose (`@("page:12")`).
                Inline::Milestone { scheme, value, .. } if self.lit => {
                    self.pending.push(Pending::Milestone {
                        scheme: scheme.clone(),
                        value: value.clone(),
                        at: out.len() as u32,
                    });
                }
                Inline::Milestone { .. } | Inline::EndoAxioma { .. } | Inline::AxiomaRef { .. } => {
                }
            }
        }
    }

    /// Walk a block sequence, wrapping FLAT sibling item runs
    /// (at-markdown has no list containers) into a list. The
    /// list-container arm bypasses this — its items are already
    /// housed.
    fn seq(&mut self, blocks: &[ABlock], dial: &Dialektos) {
        let mut open: Option<&'static str> = None;
        for block in blocks {
            let flavor = flat_item_flavor(block, dial);
            if let Some(o) = open
                && flavor != Some(o)
            {
                self.out.push(Block::Close { hypograph: None });
                open = None;
            }
            if let Some(f) = flavor
                && open.is_none()
            {
                self.out.push(Block::Open {
                    kind: if f == "ordered" {
                        Container::OrderedList { start: 1 }
                    } else {
                        Container::UnorderedList
                    },
                    lemma: None,
                });
                open = Some(f);
            }
            self.block(block, dial);
        }
        if open.is_some() {
            self.out.push(Block::Close { hypograph: None });
        }
    }

    fn drain_pending(&mut self) {
        self.drain_pending_only();
        for (onym, family, text) in std::mem::take(&mut self.note_bodies) {
            self.out.push(Block::Open {
                kind: Container::Note {
                    onym,
                    family,
                    margin: false,
                },
                lemma: None,
            });
            self.out.push(Block::Paragraph { text });
            self.out.push(Block::Close { hypograph: None });
        }
    }

    fn drain_pending_only(&mut self) {
        for p in std::mem::take(&mut self.pending) {
            match p {
                Pending::Note(onym, family, at) => self.out.push(Block::NoteRef {
                    onym,
                    family: Some(family),
                    margin: false,
                    at,
                }),
                Pending::Mark(term) => self.out.push(Block::IndexMark { term }),
                Pending::Ref(target, internal) => self.out.push(Block::Ref {
                    target,
                    text: None,
                    internal,
                }),
                Pending::Point(onym) => self.out.push(Block::Anchor { onym }),
                Pending::Cite(target, locator) => self.out.push(Block::Cite { target, locator }),
                Pending::Span {
                    kind,
                    lo,
                    hi,
                    genoses,
                    fields,
                } => self.out.push(Block::Span {
                    kind,
                    lo,
                    hi,
                    genoses,
                    fields,
                }),
                Pending::Milestone { scheme, value, at } => {
                    self.out.push(Block::Milestone { scheme, value, at })
                }
                Pending::Hidden {
                    kind,
                    text,
                    genoses,
                    fields,
                } => self.out.push(Block::Hidden {
                    kind,
                    text,
                    genoses,
                    fields,
                }),
                Pending::Parsed {
                    lo,
                    hi,
                    onym,
                    parsings,
                } => self.out.push(Block::Parsed {
                    lo,
                    hi,
                    onym,
                    parsings,
                }),
                Pending::Periodos { lo, hi, id } => self.out.push(Block::Periodos { lo, hi, id }),
            }
        }
    }
}

/// A bare item para-simmere — a flat run member needing a
/// synthesized list around it.
fn flat_item_flavor(block: &ABlock, dial: &Dialektos) -> Option<&'static str> {
    if let ABlock::Para { symbol, .. } = block {
        match sim_name(dial, symbol).as_str() {
            "unordered-item" => Some("unordered"),
            "ordered-item" => Some("ordered"),
            _ => None,
        }
    } else {
        None
    }
}

fn sim_name(dial: &Dialektos, symbol: &str) -> String {
    dial.sims
        .get(symbol)
        .map(|d| d.name.clone())
        .unwrap_or_else(|| symbol.to_string())
}

/// Plain inline flattening, apparatus-blind (lemmas of tables,
/// verse lines).
fn flat(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(t) => out.push_str(t),
            Inline::Endo { content, .. }
            | Inline::EndoDiaphane { content, .. }
            | Inline::EndoAxioma { content, .. } => out.push_str(&flat(content)),
            Inline::VerbatimInline { content, .. } => out.push_str(content),
            Inline::Monosim { param, .. } => out.push_str(param),
            _ => {}
        }
    }
    out
}

fn cells_of(line: &str) -> Vec<String> {
    line.split('|').map(|c| c.trim().to_string()).collect()
}

fn block_text(block: &ABlock) -> String {
    match block {
        ABlock::Paragraph(inlines) => flat(inlines),
        ABlock::Para {
            lemma,
            children,
            hypograph,
            ..
        } => {
            let mut parts = vec![flat(lemma)];
            parts.extend(children.iter().map(block_text));
            parts.push(flat(hypograph));
            parts.retain(|p| !p.is_empty());
            parts.join("\n")
        }
        ABlock::Stichoi { strophes, .. } => strophes
            .iter()
            .map(|s| s.0.iter().map(|l| flat(l)).collect::<Vec<_>>().join("\n"))
            .collect::<Vec<_>>()
            .join("\n\n"),
        ABlock::VerbatimBlock { content, .. } => content.clone(),
        _ => String::new(),
    }
}
