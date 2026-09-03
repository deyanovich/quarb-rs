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
//!   (`::::lang` from the first genos); an `englossis` reads as
//!   verbatim in its embedded dialect's name; verse stichoi
//!   lower as `verse`/`strophe`/`stichos` (ruling #37), the
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

use atrep::dendron::{Block as ABlock, Inline, Strophe};
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
            Ok(finish_model(blocks(&doc, &dial)))
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
            Ok(finish_model(blocks(&doc, &dial)))
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
/// `?format=`: `"tei"`, `"docbook"`, `"jats"`, `"usx"`, `"osis"`.
pub fn parse_xml_as(text: &str, kind: &str) -> Result<TextModel, KoineError> {
    let doc = match kind {
        "tei" => atrep::endo::tei_to_document(text)?,
        "docbook" => atrep::endo::docbook_to_document(text)?,
        "jats" => atrep::endo::jats_to_document(text)?,
        "usx" => atrep::endo::usx_to_document(text)?,
        "osis" => atrep::endo::osis_to_document(text)?,
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
                    b"usx" => Some("usx"),
                    b"osis" => Some("osis"),
                    b"book" => Some("docbook"),
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
fn import(mut doc: atrep::Document) -> Result<TextModel, KoineError> {
    let dial = dialektos::resolve(Path::new("."), &doc.dialect_id)?;
    kanonizo::tasso(&mut doc, &dial, Path::new("."))?;
    Ok(finish_model(blocks(&doc, &dial)))
}

/// Lower a settled document into the block event stream —
/// exposed for tests and tooling.
pub fn blocks(doc: &atrep::Document, dial: &Dialektos) -> Vec<Block> {
    let mut lower = Lower {
        out: Vec::new(),
        pending: Vec::new(),
        heading_depth: 0,
    };
    lower.seq(&doc.blocks, dial);
    lower.out
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
const SKIP: &[&str] = &["toc", "section-break", "ordinal", "manuscript-note", "index-marker"];

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
    if has_bib
        && let Ok(bg) = dialektos::resolve(Path::new("."), "bibliogramma")
    {
        model.set_bib_aliases(bib_alias_census(&bg));
    }
    model
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
    Note(String, NoteFamily),
    Mark(String),
    /// A mention met in flow: (target, internal).
    Ref(String, bool),
    /// A standalone onym anchor: a point bearer at this position.
    Point(String),
    /// A citation mark: the authored key, the bib namespace.
    Cite(String),
}

struct Lower {
    out: Vec<Block>,
    pending: Vec<Pending>,
    heading_depth: u8,
}

impl Lower {
    fn block(&mut self, block: &ABlock, dial: &Dialektos) {
        match block {
            ABlock::Paragraph(inlines) => {
                // A flat heading — a heading-N endo alone in its
                // paragraph (the at-html family): the level lives
                // in the name; the shared derivation builds the
                // outline as it does for h1/h2.
                if let [Inline::Endo {
                    symbol,
                    content,
                    ann,
                    ..
                }] = inlines.as_slice()
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
                let text = self.flatten(inlines, dial);
                self.out.push(Block::Paragraph { text });
                self.drain_pending();
            }
            ABlock::Para {
                symbol,
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
                match name {
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
                    _ => {
                        // Unknown para-simmere: transparent — the
                        // lemma opens as a paragraph, children
                        // flow, the hypograph closes as one.
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
                    // The verse vocabulary (ruling #37): stichoi
                    // lower as verse/strophe/stichos, the line
                    // taxis the citation coordinate.
                    let lemma = self.flatten(lemma, dial);
                    let hypograph = self.flatten(hypograph, dial);
                    // Deixes inside stichos lines collect and
                    // anchor at the verse block (line-level
                    // anchoring is the recorded follow-up).
                    let strophes = strophes
                        .iter()
                        .map(|s| {
                            s.0.iter()
                                .map(|line| self.flatten(line, dial))
                                .collect()
                        })
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
                let text = children.iter().map(block_text).collect::<Vec<_>>().join("\n\n");
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
        quarb_text::normalize_ws(&out)
    }

    fn flatten_into(&mut self, inlines: &[Inline], dial: &Dialektos, out: &mut String) {
        for inline in inlines {
            match inline {
                Inline::Text(t) => out.push_str(t),
                Inline::Endo {
                    symbol, content, ..
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
                    name if name == "link" || symbol == "><" => {
                        let target = flat(content);
                        self.flatten_into(content, dial, out);
                        let t = target.trim().to_string();
                        if !t.is_empty() {
                            let internal = t.starts_with('#');
                            self.pending.push(Pending::Ref(t, internal));
                        }
                    }
                    _ => self.flatten_into(content, dial, out),
                },
                Inline::EndoDiaphane { content, .. } => self.flatten_into(content, dial, out),
                Inline::VerbatimInline { content, .. } => out.push_str(content),
                Inline::Monosim { symbol, param, .. } => {
                    // The ref family: the key stays in the prose
                    // (as before) and the mention becomes a ref
                    // node, internal by construction. `cite` keys
                    // are a namespace of their own (the
                    // bibliogramma) and stay prose until that
                    // story lands.
                    out.push_str(param);
                    match sim_name(dial, symbol).as_str() {
                        "ref" | "heading-ref" | "page-ref" | "line-ref" => {
                            self.pending.push(Pending::Ref(param.clone(), true));
                        }
                        // The citation namespace is its own: cite
                        // marks resolve against bib entries (an
                        // embedded bibliogramma's entries are the
                        // recorded follow-up).
                        "cite" => {
                            self.pending.push(Pending::Cite(param.clone()));
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
                        self.pending.push(Pending::Note(onym.clone(), family));
                    }
                }
                Inline::OnymAnchor(o) => {
                    // A standalone anchor bears its onym at this
                    // position — the point style.
                    self.pending.push(Pending::Point(o.clone()));
                }
                Inline::Milestone { .. }
                | Inline::EndoAxioma { .. }
                | Inline::AxiomaRef { .. } => {}
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
        for p in std::mem::take(&mut self.pending) {
            match p {
                Pending::Note(onym, family) => self.out.push(Block::NoteRef {
                    onym,
                    family: Some(family),
                    margin: false,
                }),
                Pending::Mark(term) => self.out.push(Block::IndexMark { term }),
                Pending::Ref(target, internal) => self.out.push(Block::Ref {
                    target,
                    text: None,
                    internal,
                }),
                Pending::Point(onym) => self.out.push(Block::Anchor { onym }),
                Pending::Cite(target) => self.out.push(Block::Cite { target }),
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
            .map(|s| {
                s.0.iter().map(|l| flat(l)).collect::<Vec<_>>().join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        ABlock::VerbatimBlock { content, .. } => content.clone(),
        _ => String::new(),
    }
}
