//! The text level: a shared, source-independent semantics for
//! written documents — sections, paragraphs, quotes, lists, and
//! verbatim blocks — produced by format crates and served by this
//! crate's single adapter.
//!
//! The block model follows the atrep markup language (litogramma's
//! koine core): every block is `(kind, taxis?, lemma?, body,
//! hypograph?)` — the lemma is the head or title, the hypograph the
//! footer or attribution, and a paragraph is the degenerate
//! lemma-less, hypograph-less block. The format adapters (`quarb-text-html`,
//! `quarb-text-markdown`, the built-in plain-text reader) lower
//! their format into the [`Block`] event stream; this crate derives
//! the section tree and implements the adapter once, so
//! `//section[::lemma ...]`, `//paragraph`, and `//blockquote` read
//! identically over any text substrate — including an atrep
//! document mounted by `quarb-atrep`.
//!
//! - Node names are the structural kinds: `section`, `paragraph`,
//!   `blockquote`, `unordered-list`, `ordered-list`,
//!   `unordered-item`, `ordered-item`, `verbatim` — plus the
//!   apparatus: `footnote` and `endnote` (callout and body share
//!   the family name, ruling #35), `aside` (litogramma's fourth
//!   deixis-attached family — anchored content, not apparatus),
//!   `index-mark` (ruling #36), and the verse vocabulary
//!   (ruling #37): `verse` holding `strophe`s holding `stichos`
//!   lines, the stichos `::taxis` the citation coordinate.
//! - `::lemma`, `::hypograph`, and `::taxis` are properties; bare
//!   `::` (and `::text`) is the flattened prose of the subtree,
//!   lemma first, hypograph last.
//! - `::::level` on a section is the source heading level;
//!   `::::lang` on a verbatim block is its declared language.
//! - Sections are derived from the flat heading stream by the
//!   outline rule: a heading closes every open section at its
//!   level or deeper, then opens a section under the nearest
//!   shallower one. Content before the first heading belongs to
//!   the document root. A heading inside an open container
//!   (blockquote, list) is decorative, not sectioning: it lowers
//!   to a paragraph of its text.
//! - Every kind admits `::lemma`, `::taxis`, and `::hypograph` —
//!   the atrep model, where these are universal affordances of a
//!   block rather than privileges of particular kinds.
//! - Tables denormalize into nested lists: an `ordered-list`
//!   carrying the `<table>` trait (`::lemma` = the caption), one
//!   `ordered-item` per row (`::taxis` = row number, `<row>`
//!   trait), one `unordered-item` per cell (`<cell>` trait) whose
//!   `::lemma` is the column name — from the header row in grids,
//!   from the row's `th` label otherwise; headerless cells carry
//!   no lemma. A lemma'd item flattens as `lemma: prose`, so a
//!   row exists, the bare cell text otherwise. Empty cells are
//!   skipped.

use quarb::{AstAdapter, NodeId, Value};

pub mod render;
pub use render::{Render, render_node, render_nodes};

/// A block-level event in the text-level vocabulary — what a format
/// adapter emits. Headings arrive flat; the section tree is
/// derived here, once, for every adapter.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// A flat heading: `level` is the source level (`h2` → 2, a
    /// LaTeX `\section` → its depth), `lemma` its text.
    Heading { level: u8, lemma: String },
    /// A plain paragraph — the implicit, lemma-less block.
    Paragraph { text: String },
    /// A speech: a paragraph whose lemma names the speaker (TEI's
    /// `<said who>` / `<sp><speaker>`, litogramma's dialogue sim).
    /// The reader sees a paragraph; `::lemma` answers the speaker
    /// and the text stays the words spoken.
    Dialogue {
        lemma: String,
        text: String,
        /// The literary reading names the block `dialogue`
        /// (litogramma's sim); the text level reads it as a
        /// paragraph with a lemma.
        lit: bool,
    },
    /// The literary reading (`lit:`): a prose block that is a
    /// speech — litogramma's `dialogue-line` (at-drama `@:-`),
    /// TEI's whole-paragraph `<said>` — with the unseen metadata
    /// its aphanes monosims carry (`prosopon`, the speaker) as
    /// fields and its genoses as traits.
    Speech {
        text: String,
        genoses: Vec<String>,
        fields: Vec<(String, String)>,
    },
    /// The literary reading: an inline span of the preceding flow
    /// block — a quotation, an annotation (a name), an emphasis, a
    /// stage direction — named by its litogramma sim, at byte
    /// offsets into the block's normalized text, with its genoses
    /// as traits and its aphanes monosims as fields. Spans nest by
    /// containment of offsets: a name inside a speech is the
    /// speech's child.
    Span {
        kind: String,
        lo: u32,
        hi: u32,
        genoses: Vec<String>,
        fields: Vec<(String, String)>,
    },
    /// The literary reading: a milestone (`@("page:12")`) at an
    /// offset into the preceding flow block — `::scheme`,
    /// `::value`.
    Milestone {
        scheme: String,
        value: String,
        at: u32,
    },
    /// The literary reading: unseen metadata on the preceding
    /// block itself (a paragraph's `eidos`).
    Annotate { fields: Vec<(String, String)> },
    /// The parsing pack (ruling #80): a token the source itself
    /// parsed — litogramma's at-epimerismos diaphane, which TEI's
    /// `w`/`pc`, the Russian National Corpus, OpenCorpora and
    /// PROIEL import to — at byte offsets into the preceding flow
    /// block, with its parsings (the first is the reading's; the
    /// rest are alternatives) and the onym a head may point at.
    Parsed {
        lo: u32,
        hi: u32,
        onym: Option<String>,
        parsings: Vec<Parsing>,
    },
    /// The parsing pack: a sentence the source declared (the
    /// periodos diaphane, TEI's `s`), at byte offsets into the
    /// preceding flow block, with the source's id.
    Periodos { lo: u32, hi: u32, id: String },
    /// The literary reading: a cast entry the source declares
    /// (at-drama's `@:!! name … !!:@(id)`, a dramatis-persona
    /// line, TEI's cast list) — the name, the id when the source
    /// gives one, the description as the node's text.
    Character {
        lemma: String,
        onym: Option<String>,
        text: String,
        genoses: Vec<String>,
    },
    /// The literary reading: genoses the preceding block wears as
    /// traits — the sim of a paragraph-level simmere that has no
    /// reading of its own (a block stage direction's paragraphs
    /// read `paragraph<stage-direction>`).
    Wear { genoses: Vec<String> },
    /// Inline content belonging directly to the open container (a
    /// list item's own text, a bare-text blockquote). With no open
    /// container it is read as a paragraph.
    Text { text: String },
    /// Open a nesting container. Items take their `unordered-` /
    /// `ordered-` flavor (and taxis) from the enclosing list.
    Open {
        kind: Container,
        lemma: Option<String>,
    },
    /// Close the innermost open container, optionally with its
    /// hypograph (footer or attribution).
    Close { hypograph: Option<String> },
    /// A verbatim block — code or other preformatted lines, kept
    /// as authored.
    Verbatim { lang: Option<String>, text: String },
    /// An in-text note callout (the deixis, ruling #35): `onym`
    /// names the body it cites within its family. Emitted after
    /// the flow block it sits in; it becomes that block's child,
    /// in order, and the block's own text stays clean of markers.
    /// `family: None` means the source declares none (an HTML
    /// noteref): the callout takes its resolved body's family,
    /// and a dangling one defaults to footnote.
    NoteRef {
        onym: String,
        family: Option<NoteFamily>,
        /// The declared spelling placed this pair in the margin
        /// (a Tufte-style sidenote): surfaced as `::::form =
        /// "margin"` on both ends — the family stays footnote,
        /// placement is presentation.
        margin: bool,
        /// The callout's byte offset in the flow block's text,
        /// when the producer knows it (the literary reading's
        /// inline notes): the citation in force at the note.
        at: Option<u32>,
    },
    /// An index mark (ruling #36): an invisible anchor declaring
    /// this place concerns `term`. The back-of-book index is a
    /// query over these, never a stored structure.
    IndexMark { term: String },
    /// A reference mark — the text-level mention (html's in-prose
    /// `<a href>`, markdown's `[text](url)`, LaTeX's `\ref`):
    /// `target` is the identifier as written, `text` the authored
    /// link text (LaTeX `\ref` has none), and `internal` is the
    /// producer's declaration that the target names a label in
    /// *this* document (`#fragment`, a `\ref` key) rather than
    /// another document. A mention, not an attachment: refs do
    /// not carry `<deixis>`.
    Ref {
        target: String,
        text: Option<String>,
        internal: bool,
    },
    /// A point anchor — an invisible in-flow node bearing an
    /// `onym` at this position (LaTeX's mid-flow `\label`, an
    /// inline html `id=`). The IndexMark construction: child of
    /// its flow block, document order.
    Anchor { onym: String },
    /// A citation mark — the bibliography's mention (LaTeX's
    /// `\cite`, koine's cite monosim): `target` is the authored
    /// key, resolved against the `bib` bearers — a namespace of
    /// its own, separate from labels (LaTeX precedent).
    Cite { target: String },
    /// One bibliographic entry: a block bearing its authored
    /// `key`. Unstructured sources (\bibitem) carry the full
    /// reference as `text`; structured ones (bibliogramma,
    /// BibTeX/BibLaTeX through it) carry `fields` — canonical
    /// campus name → value, Latin per the bibliogramma
    /// vocabulary — and the entry's `genus` (liber,
    /// commentarius, …).
    Bib {
        key: String,
        text: String,
        fields: Vec<(String, String)>,
        genus: Option<String>,
    },
    /// A block label: the innermost open section takes `onym` as
    /// the name it bears (a heading's `id`; a `\label` attached
    /// to a sectioning command — the promotion rule). With no
    /// open section it degrades to a point [`Block::Anchor`].
    Label { onym: String },
    /// A verse block (ruling #37 — litogramma's stichoi model):
    /// strophes of lines, denormalized like [`Block::Table`].
    /// Lines arrive flattened by the adapter and are kept as
    /// authored; the stichos `::taxis` numbers lines 1-based,
    /// CONTINUOUSLY across strophes — the citation coordinate.
    Verse {
        lemma: Option<String>,
        strophes: Vec<Vec<String>>,
        hypograph: Option<String>,
    },
    /// A table, denormalized here into nested lists (rows =
    /// ordered items with the `<row>` trait, cells = unordered
    /// items with the `<cell>` trait and the column name as
    /// `::lemma`). Header *detection* is the adapter's job; the
    /// lowering rule lives here. A cell's own `label` (a row's
    /// `th`) wins over the positional `headers` entry.
    Table {
        lemma: Option<String>,
        headers: Option<Vec<String>>,
        rows: Vec<Vec<Cell>>,
    },
}

/// One table cell as an adapter hands it over: the text, plus the
/// label a row-shaped dialect attaches directly (an infobox row's
/// `th`). Grid dialects leave `label` empty and let the lowering
/// zip the header row on by position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub label: Option<String>,
    pub text: String,
}

impl From<&str> for Cell {
    fn from(text: &str) -> Self {
        Cell {
            label: None,
            text: text.to_string(),
        }
    }
}

impl From<String> for Cell {
    fn from(text: String) -> Self {
        Cell { label: None, text }
    }
}

/// The nesting containers an adapter opens and closes explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Container {
    Blockquote,
    UnorderedList,
    /// `start` is the first item's ordinal (Markdown's `3.` lists).
    OrderedList {
        start: i64,
    },
    /// A list item; flavor and taxis come from the enclosing list.
    Item,
    /// A note body (the noted, ruling #35): opens a note node of
    /// its family at the document root — litogramma's canonical
    /// document-end placement — holding its own blocks. `margin`
    /// as on [`Block::NoteRef`].
    Note {
        onym: String,
        family: NoteFamily,
        margin: bool,
    },
}

/// The deixis-attached families litogramma's canon names
/// (ruling #35, as amended): each family's callout and body
/// share its name. `Aside` rides the same construction —
/// koine's `aside` sim, an anchored body whose insertion point
/// is a deixis — but is content, not apparatus: its bodies do
/// not carry `<note>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoteFamily {
    Footnote,
    Endnote,
    Aside,
}

impl NoteFamily {
    fn kind(self) -> Kind {
        match self {
            NoteFamily::Footnote => Kind::Footnote,
            NoteFamily::Endnote => Kind::Endnote,
            NoteFamily::Aside => Kind::Aside,
        }
    }
}

/// One parsing of a source-parsed token (ruling #80), in the
/// pack's own vocabulary: the lexeme's lemma, the part of speech
/// (`UPOS` or `UPOS/XPOS`), the accidents (features, in the
/// source's spelling), a semantic class, the head's onym and the
/// dependency relation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsing {
    pub lexema: Option<String>,
    pub meros: Option<String>,
    pub parepomena: Option<String>,
    pub semasia: Option<String>,
    pub kephale: Option<String>,
    pub schesis: Option<String>,
}

/// A source-parsed token waiting for the tokenizer: its byte
/// range in the block's prose, its onym, its parsings.
#[derive(Debug, Clone)]
struct DeclaredToken {
    lo: u32,
    hi: u32,
    onym: Option<String>,
    parsings: Vec<Parsing>,
}

/// A row of a cast table (ruling #79): the character's id (the
/// `prosopon` a mention answers), its name, the pattern a word
/// token must match to be its mention, and the table's other
/// columns as the character's fields.
#[derive(Debug, Clone)]
pub struct CastRow {
    pub id: String,
    pub name: String,
    pub pattern: Option<regex::Regex>,
    /// How many word tokens the pattern spans (a `mention` or
    /// `stem` of several words matches a run of tokens).
    pub words: usize,
    pub fields: Vec<(String, String)>,
}

impl CastRow {
    /// Read a cast table from a CSV file: an `id` column (required),
    /// a `name`, and the pattern a word token must match to be the
    /// character's mention — `pattern` as a regex, `stem` (or
    /// `prefix`) as a prefix, or `mention` / `form` as the whole
    /// word — with every
    /// other column kept as the character's fields.
    pub fn read_csv(path: &str) -> Result<Vec<CastRow>, String> {
        let mut reader = csv::Reader::from_path(path).map_err(|e| e.to_string())?;
        let headers: Vec<String> = reader
            .headers()
            .map_err(|e| e.to_string())?
            .iter()
            .map(str::to_string)
            .collect();
        if !headers.iter().any(|h| h == "id") {
            return Err(format!(
                "a cast table needs an id column (found: {})",
                headers.join(", ")
            ));
        }
        let mut rows = Vec::new();
        for record in reader.records() {
            let record = record.map_err(|e| e.to_string())?;
            let get = |k: &str| {
                headers
                    .iter()
                    .position(|h| h == k)
                    .and_then(|i| record.get(i))
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
            };
            let Some(id) = get("id") else { continue };
            let (pattern, words) = if let Some(p) = get("pattern") {
                let re = regex::Regex::new(p).map_err(|e| format!("cast pattern for {id}: {e}"))?;
                // A pattern of several words matches a run of that
                // many tokens, as a stem or a mention does.
                (Some(re), p.split_whitespace().count().max(1))
            } else if let Some(stem) = get("stem").or_else(|| get("prefix")) {
                let re = regex::Regex::new(&format!("^{}", regex::escape(stem))).unwrap();
                (Some(re), stem.split_whitespace().count())
            } else if let Some(form) = get("mention").or_else(|| get("form")) {
                let re = regex::Regex::new(&format!("^{}$", regex::escape(form))).unwrap();
                (Some(re), form.split_whitespace().count())
            } else {
                (None, 0)
            };
            let fields: Vec<(String, String)> = headers
                .iter()
                .zip(record.iter())
                .filter(|(h, v)| h.as_str() != "id" && !v.trim().is_empty())
                .map(|(h, v)| (h.clone(), v.trim().to_string()))
                .collect();
            rows.push(CastRow {
                id: id.to_string(),
                name: get("name").unwrap_or(id).to_string(),
                pattern,
                words,
                fields,
            });
        }
        Ok(rows)
    }
}

/// The structural kind of a node — also its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Document,
    Section,
    /// The literary reading's cast entry (ruling #79): a character
    /// declared by the source or by a cast table, `::lemma` its
    /// name, `::onym` its id, `<-character` its mentions.
    Character,
    /// The literary reading's gazetteer entry (ruling #87): a place
    /// declared by a places table, `::lemma` its name, `::onym` its
    /// id, `<-place` its mentions.
    Place,
    Paragraph,
    Blockquote,
    UnorderedList,
    OrderedList,
    UnorderedItem,
    OrderedItem,
    Verbatim,
    /// Callout and body both — they share the name by
    /// construction (litogramma F5): `//footnote` gathers the
    /// whole apparatus, `<deixis>`/`<note>` tell them apart.
    Footnote,
    /// The second note family, same construction.
    Endnote,
    /// The anchored-content family, same construction
    /// (litogramma's aside): body + insertion-point deixis. Content, not
    /// apparatus — no `<note>` on its bodies.
    Aside,
    /// An index mark (ruling #36): `::term`, in flow position.
    IndexMark,
    /// A reference mark: `::target`, `-->` resolves it.
    Ref,
    /// A citation mark: `::target` the authored key, resolved
    /// against the `bib` namespace.
    Cit,
    /// One bibliographic entry: `::onym` the key it bears, its
    /// projection the full reference text.
    Bib,
    /// A point anchor: `::onym`, an invisible in-flow bearer.
    Anchor,
    /// The verse vocabulary (ruling #37): the stichoi container…
    Verse,
    /// …its strophes…
    Strophe,
    /// …and its lines, `::taxis` the citation coordinate.
    Stichos,
    /// The corpus reading (ruling #62): one token of a prose
    /// block — a UAX #29 word segment — with `::` its form,
    /// `::lower` the case-folded form, `<word>` / `<number>` /
    /// `<punct>` its class, `::::n` its position in the document
    /// and `::::sentence` the UAX #29 sentence it falls in. Flat
    /// under the block: a sentence is an annotation on the token,
    /// never a container, so sibling hops run across a false
    /// sentence break ("Dr. Smith") as they do across a true one.
    Token,
    /// A treebank's sentence (ruling #63): in a standalone CoNLL-U
    /// document the file's blank-line blocks are declared
    /// structure that nothing re-segments, so each is a prose
    /// block — `::` its text (`# text`, else the forms joined per
    /// `SpaceAfter`), `::id` its `# sent_id`, every other `# key =
    /// value` comment a property — and under `corpus:` its tokens
    /// are its children. Over prose, a sidecar's sentence stays an
    /// annotation on the token.
    Sentence,
    /// The literary reading's speech block: litogramma's
    /// `dialogue-line`.
    Speech,
    /// The literary reading's drama speech (`@: Speaker`): the
    /// lemma is the printed speech prefix.
    Dialogue,
    /// The literary reading's inline span, named by its sim
    /// (`quotation`, `annotation`, `emphasis`, …).
    Span,
    /// The literary reading's milestone, a point in the prose.
    Milestone,
}

impl Kind {
    fn name(self) -> Option<&'static str> {
        Some(match self {
            Kind::Document => return None,
            Kind::Section => "section",
            Kind::Paragraph => "paragraph",
            Kind::Blockquote => "blockquote",
            Kind::Footnote => "footnote",
            Kind::Endnote => "endnote",
            Kind::Aside => "aside",
            Kind::IndexMark => "index-mark",
            Kind::Ref => "ref",
            Kind::Cit => "cit",
            Kind::Bib => "bib",
            Kind::Anchor => "anchor",
            Kind::Verse => "verse",
            Kind::Strophe => "strophe",
            Kind::Stichos => "stichos",
            Kind::UnorderedList => "unordered-list",
            Kind::OrderedList => "ordered-list",
            Kind::UnorderedItem => "unordered-item",
            Kind::OrderedItem => "ordered-item",
            Kind::Verbatim => "verbatim",
            Kind::Token => "token",
            Kind::Sentence => "sentence",
            Kind::Speech => "dialogue-line",
            Kind::Character => "character",
            Kind::Place => "place",
            Kind::Dialogue => "dialogue",
            // The adapter answers the sim name kept on the node.
            Kind::Span => "span",
            Kind::Milestone => "milestone",
        })
    }
}

struct Node {
    kind: Kind,
    lemma: Option<String>,
    hypograph: Option<String>,
    taxis: Option<i64>,
    /// Source heading level, on sections.
    level: Option<u8>,
    /// Declared language, on verbatim blocks.
    lang: Option<String>,
    /// First ordinal of an ordered list (not exposed; feeds the
    /// items' taxis).
    start: i64,
    /// The node's own (direct) text, before subtree flattening.
    text: String,
    /// The flattened prose of the subtree — the `::` projection.
    prose: String,
    /// The node heads a denormalized table (`<table>` trait).
    table: bool,
    /// The node is a denormalized table row (`<row>` trait).
    row: bool,
    /// The node is a denormalized table cell (`<cell>` trait).
    cell: bool,
    /// The apparatus (ruling #35): the note name as written —
    /// and, on an index mark (ruling #36), the term.
    onym: Option<String>,
    /// A footnote callout (`<deixis>`) rather than a body.
    deixis: bool,
    /// A callout whose body is missing (`<dangling>`).
    dangling: bool,
    /// A callout whose source declared no family: resolution may
    /// re-kind it from the body it reaches.
    family_open: bool,
    /// The declared spelling was a margin form (`::::form`).
    margin: bool,
    /// Callout -> body edge, once resolved.
    note_edge: Option<NodeId>,
    /// Body <- callouts (the reverse index).
    cites: Vec<NodeId>,
    /// A ref's target identifier, as written.
    target: Option<String>,
    /// The producer declared the target in-document.
    internal: bool,
    /// A structured bib entry's fields (campus → value) and its
    /// genus, per the bibliogramma vocabulary.
    fields: Vec<(String, String)>,
    genus: Option<String>,
    /// Ref -> bearer edge, once resolved (in-document).
    ref_edge: Option<NodeId>,
    /// Bearer <- refs (the reverse index).
    ref_cites: Vec<NodeId>,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    /// The corpus reading's annotation, on token nodes.
    token: Option<TokenInfo>,
    /// A sentence the corpus reading derived at mount (ruling
    /// #64) — from the segmentation in force, never declared by
    /// the source — as against a treebank's declared one.
    derived: bool,
    /// The literary reading: the genoses a simmere wore (its
    /// traits), the sim name of a span, its byte range in the
    /// flow block's text, and the tokens it covers.
    genoses: Vec<String>,
    span_kind: Option<String>,
    at: Option<(u32, u32)>,
    span_tokens: Vec<NodeId>,
    /// The character a mention resolves to (`->character`), and on
    /// a character the mentions that resolve to it (`<-character`).
    character: Option<NodeId>,
    mentions: Vec<NodeId>,
    /// The place a mention resolves to (`->place`); a place keeps
    /// its mentions in `mentions` too.
    place: Option<NodeId>,
    /// The parsing pack's tokens and sentences declared on this
    /// flow block, consumed by the tokenizer (ruling #80).
    declared_tokens: Vec<DeclaredToken>,
    declared_sentences: Vec<(u32, u32, String)>,
}

/// A token's class — the trait it wears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenClass {
    /// Carries a letter.
    Word,
    /// Carries a digit and no letter.
    Number,
    /// Everything else that is not whitespace.
    Punct,
}

impl TokenClass {
    fn name(self) -> &'static str {
        match self {
            TokenClass::Word => "word",
            TokenClass::Number => "number",
            TokenClass::Punct => "punct",
        }
    }
}

#[derive(Debug, Clone)]
struct TokenInfo {
    class: TokenClass,
    /// 1-based position among the document's tokens; 0 for an
    /// empty node, which is not in the text (`::::n` null).
    n: u32,
    /// 1-based ordinal of the sentence, document-wide.
    sentence: u32,
    /// The sentence's byte range in the block's prose — `::sentence`
    /// is that substring.
    span: (u32, u32),
    /// The annotation a CoNLL-U source supplied, when one did.
    annot: Option<Box<Annot>>,
    /// The token's own byte offset in the block's prose, and the
    /// literary reading's spans that cover it, outermost first.
    at: u32,
    spans: Vec<NodeId>,
}

/// A token's linguistic annotation from a CoNLL-U source (ruling
/// #63): its in-sentence id, the lemma, the universal and
/// language-specific parts of speech, the raw feature, enhanced
/// dependency and miscellany columns, the dependency relation, the
/// basic head (`->head`; `<-head` the dependents) and the enhanced
/// heads with their relations (`->ehead`, `$-::rel`). Every
/// `Key=Value` of FEATS, MISC and a CoNLL-U Plus column answers as
/// a property under its key, in the annotation's own spelling and
/// case-folded.
#[derive(Debug, Clone, Default)]
struct Annot {
    id: String,
    lemma: Option<String>,
    upos: Option<String>,
    xpos: Option<String>,
    feats: Option<String>,
    deprel: Option<String>,
    deps: Option<String>,
    misc: Option<String>,
    /// FEATS, MISC and extra-column pairs, in source order.
    keys: Vec<(String, String)>,
    /// The multiword range this word belongs to: its id (`1-2`)
    /// and surface form (`don't`).
    mwt: Option<(String, String)>,
    /// An empty node (`8.1`): present in the enhanced graph only.
    empty: bool,
    head: Option<NodeId>,
    dependents: Vec<NodeId>,
    eheads: Vec<(NodeId, String)>,
    edependents: Vec<(NodeId, String)>,
    /// The mentions this token falls in, as 1-based ordinals into
    /// the model's mention list (several where mentions nest).
    mentions: Vec<u32>,
}

/// A mention (ruling #63): a span of tokens an annotation marks as
/// a named entity or a coreference mention — the sentence pattern
/// again, an annotation on the tokens, never a container. Decoded
/// at mount from a `ner` key in MISC (BIO and BIOES tags) and from
/// CorefUD's `Entity=` bracket notation.
#[derive(Debug, Clone)]
struct Mention {
    tokens: Vec<NodeId>,
    /// The entity type (PERSON, GPE, …), when the source names one.
    etype: Option<String>,
    /// The coreference cluster id, when the source declares one.
    cluster: Option<String>,
}

impl Annot {
    /// A `Key=Value` pair's value: the key as the annotation spells
    /// it first, then case-folded.
    fn key(&self, name: &str) -> Option<&str> {
        self.keys
            .iter()
            .find(|(k, _)| k == name)
            .or_else(|| self.keys.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)))
            .map(|(_, v)| v.as_str())
    }
}

/// The bracket entries of a CorefUD `Entity=` value, which may
/// run together (`(e4-object-5-(e3-person-3-`, `e3)(e5-x-1)`) or
/// be `|`-separated: an opening entry runs from its `(` to the
/// next bracket, a closing one ends at its `)`.
fn entity_entries(spec: &str) -> Vec<String> {
    let mut out = Vec::new();
    for piece in spec.split('|') {
        let mut cur = String::new();
        for ch in piece.chars() {
            if ch == '(' && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur.push(ch);
            if ch == ')' {
                out.push(std::mem::take(&mut cur));
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

/// `Key=Value|Key=Value` (FEATS, MISC) as pairs; an entry without
/// `=` keeps its text as the key with an empty value; `_` is none.
fn key_values(column: &str) -> Vec<(String, String)> {
    if column == "_" || column.is_empty() {
        return Vec::new();
    }
    column
        .split('|')
        .filter(|e| !e.is_empty())
        .map(|e| match e.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (e.to_string(), String::new()),
        })
        .collect()
}

/// The UAX #29 sentence spans of a block's prose — with the
/// sentence bonds applied when a `.desm` set is in force (the
/// `syndesmos` crate: abbreviations a sentence may end in without
/// ending, and patterns whose match straddles a break).
fn sentence_spans(prose: &str, bonds: Option<&syndesmos::Syndesmos>) -> Vec<(usize, usize)> {
    let raw: Vec<(usize, &str)> = match bonds {
        Some(b) => b.split_sentence_bound_indices(prose),
        None => {
            use unicode_segmentation::UnicodeSegmentation;
            prose.split_sentence_bound_indices().collect()
        }
    };
    raw.into_iter()
        .filter(|(_, seg)| !seg.trim().is_empty())
        .map(|(at, seg)| (at, at + seg.len()))
        .collect()
}

/// The ten standard CoNLL-U columns, in order.
const CONLLU_COLUMNS: [&str; 10] = [
    "ID", "FORM", "LEMMA", "UPOS", "XPOS", "FEATS", "HEAD", "DEPREL", "DEPS", "MISC",
];

/// One token line of a CoNLL-U sentence, as read.
#[derive(Debug, Clone, Default)]
struct ConlluToken {
    id: String,
    /// A multiword range `a-b`, parsed.
    range: Option<(u32, u32)>,
    /// The numeric id of a word, or an empty node's integer part.
    ord: u32,
    /// An empty node (`8.1`).
    empty: bool,
    form: String,
    lemma: String,
    upos: String,
    xpos: String,
    feats: String,
    head: String,
    deprel: String,
    deps: String,
    misc: String,
    /// CoNLL-U Plus columns beyond the ten, under their declared
    /// names with `:` read as `-`.
    extra: Vec<(String, String)>,
}

/// One sentence block of a CoNLL-U file.
#[derive(Debug, Clone, Default)]
struct ConlluSentence {
    /// `# key = value` comments, in order (`sent_id`, `text`, …).
    comments: Vec<(String, String)>,
    /// `# newdoc [id = X]` opened a document before this sentence.
    newdoc: Option<Option<String>>,
    /// `# newpar [id = X]` opened a paragraph before this sentence.
    newpar: Option<Option<String>>,
    tokens: Vec<ConlluToken>,
}

/// A CoNLL-U file: its sentences and its file-level declarations
/// (`# global.columns`, `# global.Entity`).
#[derive(Debug, Clone, Default)]
struct ConlluDoc {
    sentences: Vec<ConlluSentence>,
    globals: Vec<(String, String)>,
}

/// Whether `text` reads as CoNLL-U: its first line that is neither
/// blank nor a `#` comment is a tab-separated token line whose id
/// is a word (`3`), a range (`3-4`) or an empty node (`3.1`), with
/// the ten columns (or, under a `# global.columns` header, that
/// header's count). The sniff for an extensionless input or a
/// pipe, where no `.conllu` names the reading.
pub fn looks_like_conllu(text: &str) -> bool {
    let mut columns = CONLLU_COLUMNS.len();
    for line in text.lines().take(200) {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if let Some(c) = line.strip_prefix('#') {
            if let Some((k, v)) = c.split_once('=')
                && k.trim() == "global.columns"
            {
                columns = v.split_whitespace().count().max(2);
            }
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != columns {
            return false;
        }
        let id = fields[0];
        let (a, b) = match id.split_once(['-', '.']) {
            Some((a, b)) => (a, Some(b)),
            None => (id, None),
        };
        return a.parse::<u32>().is_ok() && b.is_none_or(|b| b.parse::<u32>().is_ok());
    }
    false
}

/// The treebank files of a directory, for the set readings: every
/// `.conllu` / `.conllup` beneath `dir` (a UD treebank's train /
/// dev / test, a release's languages), named by their path
/// relative to `dir`, in path order. Hidden entries are skipped.
/// An empty set is an error: the directory holds no treebank.
pub fn read_conllu_dir(dir: &std::path::Path) -> Result<Vec<(String, String)>, String> {
    fn walk(
        dir: &std::path::Path,
        base: &std::path::Path,
        out: &mut Vec<(String, String)>,
    ) -> Result<(), String> {
        let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .map_err(|e| format!("reading {}: {e}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                !p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.'))
            })
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(&p, base, out)?;
            } else if p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                e.eq_ignore_ascii_case("conllu") || e.eq_ignore_ascii_case("conllup")
            }) {
                let text = std::fs::read_to_string(&p)
                    .map_err(|e| format!("reading {}: {e}", p.display()))?;
                let text = text
                    .strip_prefix('\u{feff}')
                    .map(str::to_owned)
                    .unwrap_or(text);
                let name = p
                    .strip_prefix(base)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .into_owned();
                out.push((name, text));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out)?;
    if out.is_empty() {
        return Err(format!("{} holds no .conllu file", dir.display()));
    }
    Ok(out)
}

/// The sentences of a CoNLL-U text: token lines split on tabs,
/// sentences separated by blank lines, comments read (`# key =
/// value` kept on the sentence, `# newdoc` / `# newpar` as
/// structure, `# global.*` on the file), empty nodes (`1.1`) kept,
/// multiword ranges (`1-2`) kept as the unit that aligns to the
/// text. A `# global.columns` line (CoNLL-U Plus) reorders the
/// columns and names extra ones.
fn parse_conllu(text: &str) -> Result<ConlluDoc, String> {
    let mut doc = ConlluDoc::default();
    let mut current = ConlluSentence::default();
    let mut columns: Vec<String> = CONLLU_COLUMNS.iter().map(|c| c.to_string()).collect();
    let trimmed = |s: &str| s.trim().to_string();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            if !current.tokens.is_empty() {
                doc.sentences.push(std::mem::take(&mut current));
            }
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            let comment = comment.trim();
            if let Some(rest) = comment.strip_prefix("newdoc") {
                let id = rest
                    .trim()
                    .strip_prefix("id")
                    .and_then(|r| r.trim().strip_prefix('='));
                current.newdoc = Some(id.map(trimmed));
            } else if let Some(rest) = comment.strip_prefix("newpar") {
                let id = rest
                    .trim()
                    .strip_prefix("id")
                    .and_then(|r| r.trim().strip_prefix('='));
                current.newpar = Some(id.map(trimmed));
            } else if let Some((k, v)) = comment.split_once('=') {
                let (k, v) = (k.trim(), v.trim());
                if let Some(g) = k.strip_prefix("global.") {
                    if g == "columns" {
                        columns = v.split_whitespace().map(|c| c.to_string()).collect();
                    }
                    doc.globals.push((g.to_string(), v.to_string()));
                } else {
                    current.comments.push((k.to_string(), v.to_string()));
                }
            }
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 2 {
            return Err(format!(
                "CoNLL-U line {}: expected tab-separated columns",
                i + 1
            ));
        }
        let mut tok = ConlluToken::default();
        for (k, name) in columns.iter().enumerate() {
            let v = f.get(k).copied().unwrap_or("_");
            match name.to_ascii_uppercase().as_str() {
                "ID" => tok.id = v.to_string(),
                "FORM" => tok.form = v.to_string(),
                "LEMMA" => tok.lemma = v.to_string(),
                "UPOS" => tok.upos = v.to_string(),
                "XPOS" => tok.xpos = v.to_string(),
                "FEATS" => tok.feats = v.to_string(),
                "HEAD" => tok.head = v.to_string(),
                "DEPREL" => tok.deprel = v.to_string(),
                "DEPS" => tok.deps = v.to_string(),
                "MISC" => tok.misc = v.to_string(),
                _ => {
                    if v != "_" && !v.is_empty() {
                        tok.extra.push((name.replace(':', "-"), v.to_string()));
                    }
                }
            }
        }
        if tok.id.is_empty() {
            return Err(format!("CoNLL-U line {}: no token id", i + 1));
        }
        if let Some((a, b)) = tok.id.split_once('-') {
            match (a.parse::<u32>(), b.parse::<u32>()) {
                (Ok(a), Ok(b)) if a <= b => {
                    tok.range = Some((a, b));
                    tok.ord = a;
                }
                _ => {
                    return Err(format!(
                        "CoNLL-U line {}: malformed multiword range {:?}",
                        i + 1,
                        tok.id
                    ));
                }
            }
        } else if let Some((a, _)) = tok.id.split_once('.') {
            tok.empty = true;
            tok.ord = a.parse().map_err(|_| {
                format!(
                    "CoNLL-U line {}: malformed empty-node id {:?}",
                    i + 1,
                    tok.id
                )
            })?;
        } else {
            tok.ord = tok
                .id
                .parse()
                .map_err(|_| format!("CoNLL-U line {}: malformed token id {:?}", i + 1, tok.id))?;
        }
        current.tokens.push(tok);
    }
    if !current.tokens.is_empty() {
        doc.sentences.push(current);
    }
    Ok(doc)
}

/// The sentence's surface text from its tokens: forms in order, a
/// multiword range standing for its words, a space after each
/// unless `SpaceAfter=No`; empty nodes are not in the text.
fn conllu_surface(tokens: &[ConlluToken]) -> String {
    let mut out = String::new();
    let mut covered: u32 = 0; // the last word id a range covers
    for t in tokens {
        if t.empty || (t.range.is_none() && t.ord <= covered) {
            continue;
        }
        if let Some((_, b)) = t.range {
            covered = b;
        }
        out.push_str(&t.form);
        let no_space = key_values(&t.misc)
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("SpaceAfter") && v == "No");
        if !no_space {
            out.push(' ');
        }
    }
    out.trim_end().to_string()
}

fn class_of(seg: &str) -> TokenClass {
    if seg.chars().any(char::is_alphabetic) {
        TokenClass::Word
    } else if seg.chars().any(char::is_numeric) {
        TokenClass::Number
    } else {
        TokenClass::Punct
    }
}

/// The class an annotation declares (ruling #65): UPOS `PUNCT` is
/// punctuation and `NUM` a number whatever the form's letters say
/// (`3rd`, a tagger's `PUNCT` on a lettered token); every other
/// tag, or none, leaves the form to decide.
fn class_of_annotated(form: &str, upos: &str) -> TokenClass {
    match upos {
        "PUNCT" => TokenClass::Punct,
        "NUM" => TokenClass::Number,
        _ => class_of(form),
    }
}

/// Whether a token's form is a quotation mark (ruling #65): every
/// character one of Unicode's initial and final quotation
/// punctuation, the ASCII quotes, the guillemets, or the CJK
/// corner brackets. Such a token wears `<quote>` beside `<punct>`.
fn is_quote(form: &str) -> bool {
    !form.is_empty()
        && form.chars().all(|c| {
            matches!(
                c,
                '"' | '\'' | '`'
                    | '\u{AB}' | '\u{BB}'
                    | '\u{2018}'..='\u{201F}'
                    | '\u{2039}' | '\u{203A}'
                    | '\u{300C}'..='\u{300F}'
                    | '\u{301D}'..='\u{301F}'
                    | '\u{FF02}' | '\u{FF07}'
            )
        })
}

impl Node {
    fn new(kind: Kind, parent: Option<NodeId>) -> Self {
        Node {
            kind,
            token: None,
            derived: false,
            lemma: None,
            hypograph: None,
            taxis: None,
            level: None,
            lang: None,
            start: 1,
            text: String::new(),
            prose: String::new(),
            table: false,
            row: false,
            cell: false,
            onym: None,
            deixis: false,
            dangling: false,
            family_open: false,
            margin: false,
            note_edge: None,
            cites: Vec::new(),
            target: None,
            internal: false,
            fields: Vec::new(),
            genus: None,
            ref_edge: None,
            ref_cites: Vec::new(),
            parent,
            children: Vec::new(),
            genoses: Vec::new(),
            span_kind: None,
            at: None,
            span_tokens: Vec::new(),
            character: None,
            place: None,
            mentions: Vec::new(),
            declared_tokens: Vec::new(),
            declared_sentences: Vec::new(),
        }
    }
}

/// The reference pass: collect every borne name (block labels
/// and point anchors — deixis callouts pair within their own
/// families and stay out of this namespace), then land each
/// internal ref on its bearer, `<dangling>` when nothing bears
/// the name. External refs resolve at query time, through the
/// engine's reference machinery.
fn resolve_refs(nodes: &mut [Node]) -> std::collections::HashMap<String, NodeId> {
    let mut onyms: std::collections::HashMap<String, NodeId> = std::collections::HashMap::new();
    // Citation keys are a namespace of their own, separate from
    // labels (LaTeX's \bibcite vs \newlabel precedent): `cit`
    // resolves against `bib` bearers only.
    let mut bibs: std::collections::HashMap<String, NodeId> = std::collections::HashMap::new();
    for (i, n) in nodes.iter().enumerate() {
        if n.deixis || matches!(n.kind, Kind::IndexMark | Kind::Ref | Kind::Cit) {
            continue;
        }
        if n.kind == Kind::Bib {
            if let Some(k) = &n.onym {
                bibs.entry(k.clone()).or_insert(NodeId(i as u64));
            }
            continue;
        }
        let bearer = n.kind == Kind::Anchor
            || (!matches!(n.kind, Kind::Footnote | Kind::Endnote | Kind::Aside)
                && n.onym.is_some());
        if bearer && let Some(o) = &n.onym {
            onyms.entry(o.clone()).or_insert(NodeId(i as u64));
        }
    }
    for i in 0..nodes.len() {
        let (map, key) = match nodes[i].kind {
            Kind::Ref if nodes[i].internal => (
                &onyms,
                nodes[i]
                    .target
                    .as_deref()
                    .map(|t| t.trim_start_matches('#').to_string())
                    .unwrap_or_default(),
            ),
            Kind::Cit => (&bibs, nodes[i].target.clone().unwrap_or_default()),
            _ => continue,
        };
        match map.get(&key) {
            Some(&bearer) => {
                nodes[i].ref_edge = Some(bearer);
                let me = NodeId(i as u64);
                nodes[bearer.0 as usize].ref_cites.push(me);
            }
            None => nodes[i].dangling = true,
        }
    }
    onyms
}

/// Collapse whitespace runs to single spaces and trim — the prose
/// normalization adapters apply to inline content. Verbatim text
/// is the exception: it is kept as authored.
pub fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What a document declares about itself, in a standard
/// vocabulary its format provides — HTML's `<head>` (the `meta`
/// name/property pairs, `title`, the `lang` attribute, the
/// canonical link, `rel="tag"` links) and a schema.org JSON-LD
/// block. Two layers: every declaration lands verbatim under
/// `::::name` (`::::description`, `::::og:type`,
/// `::::article:section`, `::::schema:@type`), so nothing a page
/// says is lost; and the standard vocabularies feed a curated
/// core — the tags (`keywords`, `article:tag`, `rel=tag`, JSON-LD
/// `keywords`) become traits on the document and `::tags`, the
/// one classification (`article:section` / `articleSection`)
/// becomes `::category` and a trait too, `description`, `author`,
/// and the `published` / `modified` instants. Declared identity
/// only: a format adapter reads a cited vocabulary, never a class
/// name or a layout.
#[derive(Debug, Clone, Default)]
pub struct HeadMeta {
    /// Every declaration, name → values, in document order; a
    /// repeated name accumulates.
    pub declared: Vec<(String, Vec<String>)>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    /// The one classification (a section of the site, a category).
    pub category: Option<String>,
    /// The tags, as written, first appearance wins.
    pub tags: Vec<String>,
    pub published: Option<Value>,
    pub modified: Option<Value>,
}

impl HeadMeta {
    /// Record a declaration verbatim (the lossless layer).
    pub fn declare(&mut self, name: &str, value: &str) {
        let value = normalize_ws(value);
        if name.is_empty() || value.is_empty() {
            return;
        }
        match self.declared.iter_mut().find(|(k, _)| k == name) {
            Some((_, vs)) => vs.push(value),
            None => self.declared.push((name.to_string(), vec![value])),
        }
    }

    /// Add a tag unless already present (case-insensitively).
    pub fn tag(&mut self, tag: &str) {
        let tag = normalize_ws(tag);
        if tag.is_empty() || self.tags.iter().any(|t| t.eq_ignore_ascii_case(&tag)) {
            return;
        }
        self.tags.push(tag);
    }

    /// A declaration's value: the string when declared once, the
    /// list when repeated.
    pub fn declared_value(&self, name: &str) -> Option<Value> {
        let (_, vs) = self.declared.iter().find(|(k, _)| k == name)?;
        Some(match vs.as_slice() {
            [one] => Value::Str(one.clone()),
            many => Value::list(many.iter().cloned().map(Value::Str).collect()),
        })
    }

    /// The trait spelling of a tag or category: as declared, case
    /// kept, with runs of anything a name cannot carry (spaces,
    /// slashes, parentheses) collapsed to one dash — `Execution
    /// Model` is `<tag:Execution-Model>`, `now()` is `<tag:now>`.
    pub fn trait_name(s: &str) -> String {
        let mut out = String::new();
        let mut dash = false;
        for c in s.chars() {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '+') {
                out.push(c);
                dash = c == '-';
            } else if !dash && !out.is_empty() {
                out.push('-');
                dash = true;
            }
        }
        while out.ends_with('-') {
            out.pop();
        }
        out
    }
}

/// A Quarb adapter over a text-level document.
pub struct TextModel {
    nodes: Vec<Node>,
    root: NodeId,
    /// Every borne name → its bearer (block labels and point
    /// anchors, one namespace, first bearer in document order
    /// wins) — the landing map for `-->` and for a sibling
    /// document's `#fragment`.
    onyms: std::collections::HashMap<String, NodeId>,
    /// The document's own URL, when the mount knows it.
    document_url: Option<url::Url>,
    /// The base relative reference targets join against when the
    /// document declares one (html's `<base href>`); else the
    /// document's own URL.
    link_base: Option<url::Url>,
    /// Where the document is served from when no absolute URL is
    /// known — the mount's path, an archive member's path — the
    /// base `::href` links against.
    document_path: Option<String>,
    /// The citation scheme `::cite` answers under when the
    /// document carries several (`?scheme=`); else the first
    /// milestone's (ruling #81).
    cite_scheme: Option<String>,
    /// The spelling table the corpus reading folds through
    /// (`?modernize=`, ruling #82): `::lower` and `::modern` read
    /// the modern spelling of a historical edition's tokens; `::`
    /// stays the edition's own.
    orthography: Option<String>,
    /// Alias → canonical campus, for bib field lookup: the
    /// bibliogramma vocabulary's census (BibLaTeX's field names
    /// are its English rows), so `::author` answers as
    /// `::auctor` — in any covered language. Set by the koine
    /// route, which holds the dialektos.
    bib_aliases: std::collections::HashMap<String, String>,
    /// What the document declares about itself (see [`HeadMeta`]);
    /// answered on the document node.
    head: HeadMeta,
    /// The corpus reading's positional index: case-folded form →
    /// the tokens bearing it, in document order. Built on first
    /// use, once the tokens exist; answers `//token[::lower = "x"]`
    /// through `descendants_where` without a walk.
    token_index: std::sync::OnceLock<std::collections::HashMap<String, Vec<NodeId>>>,
    /// The mentions an annotation marked (ruling #63), in document
    /// order of their first token; tokens point in by ordinal.
    mentions: Vec<Mention>,
}

impl TextModel {
    /// Assemble the document tree from a adapter's event stream.
    ///
    /// Iterative throughout (the stream is flat; prose flattening
    /// runs over indices), so pathological nesting cannot overflow
    /// the call stack. Lenient on malformed streams: a stray
    /// `Close` is ignored, unclosed containers close at the end.
    pub fn build(blocks: Vec<Block>) -> Self {
        let mut nodes = vec![Node::new(Kind::Document, None)];
        let root = NodeId(0);
        // Innermost-last stack of open *sections* (outline-derived).
        let mut sections: Vec<NodeId> = Vec::new();
        // Innermost-last stack of open explicit containers.
        let mut containers: Vec<NodeId> = Vec::new();
        // The block a NoteRef callout attaches to: the last flow
        // node created or appended to (ruling #35).
        let mut last_flow: Option<NodeId> = None;
        // The block a Label names: the most recently opened block
        // of any kind — a heading's section, a paragraph, a
        // container, a verbatim — the html-id / attached-\label
        // parity rule.
        let mut last_block: Option<NodeId> = None;

        for block in blocks {
            match block {
                Block::Heading { level, lemma } => {
                    let lemma = normalize_ws(&lemma);
                    if !containers.is_empty() {
                        // Decorative heading inside a container:
                        // not sectioning — lower to a paragraph.
                        if !lemma.is_empty() {
                            let parent = *containers.last().unwrap();
                            let id = push(&mut nodes, Kind::Paragraph, parent);
                            nodes[id.0 as usize].text = lemma;
                        }
                        continue;
                    }
                    while let Some(&open) = sections.last() {
                        if nodes[open.0 as usize].level >= Some(level) {
                            sections.pop();
                        } else {
                            break;
                        }
                    }
                    let parent = sections.last().copied().unwrap_or(root);
                    let id = push(&mut nodes, Kind::Section, parent);
                    let n = &mut nodes[id.0 as usize];
                    n.lemma = Some(lemma);
                    n.level = Some(level);
                    sections.push(id);
                    last_flow = Some(id);
                    last_block = Some(id);
                }
                Block::Paragraph { text } => {
                    let text = normalize_ws(&text);
                    if text.is_empty() {
                        continue;
                    }
                    let parent = cursor(&sections, &containers, root);
                    let id = push(&mut nodes, Kind::Paragraph, parent);
                    nodes[id.0 as usize].text = text;
                    last_flow = Some(id);
                    last_block = Some(id);
                }
                // A speech is a paragraph that knows its speaker.
                Block::Dialogue { lemma, text, lit } => {
                    let text = normalize_ws(&text);
                    if text.is_empty() {
                        continue;
                    }
                    let parent = cursor(&sections, &containers, root);
                    let kind = if lit { Kind::Dialogue } else { Kind::Paragraph };
                    let id = push(&mut nodes, kind, parent);
                    let n = &mut nodes[id.0 as usize];
                    n.text = text;
                    n.lemma = Some(normalize_ws(&lemma)).filter(|l| !l.is_empty());
                    last_flow = Some(id);
                    last_block = Some(id);
                }
                // The literary reading's speech block.
                Block::Speech {
                    text,
                    genoses,
                    fields,
                } => {
                    let text = normalize_ws(&text);
                    if text.is_empty() {
                        continue;
                    }
                    let parent = cursor(&sections, &containers, root);
                    let id = push(&mut nodes, Kind::Speech, parent);
                    let n = &mut nodes[id.0 as usize];
                    n.text = text;
                    n.genoses = genoses;
                    n.fields = fields;
                    last_flow = Some(id);
                    last_block = Some(id);
                }
                // An inline span of the last flow block, nested
                // under the innermost earlier span that contains it.
                Block::Span {
                    kind,
                    lo,
                    hi,
                    genoses,
                    fields,
                } => {
                    let Some(flow) = last_flow else { continue };
                    let text = nodes[flow.0 as usize]
                        .text
                        .get(lo as usize..hi as usize)
                        .unwrap_or("")
                        .to_string();
                    let parent = enclosing_span(&nodes, flow, lo, hi).unwrap_or(flow);
                    let id = push(&mut nodes, Kind::Span, parent);
                    let n = &mut nodes[id.0 as usize];
                    n.span_kind = Some(kind);
                    n.at = Some((lo, hi));
                    n.text = text;
                    n.genoses = genoses;
                    n.fields = fields;
                }
                Block::Character {
                    lemma,
                    onym,
                    text,
                    genoses,
                } => {
                    let parent = cursor(&sections, &containers, root);
                    let id = push(&mut nodes, Kind::Character, parent);
                    let n = &mut nodes[id.0 as usize];
                    n.lemma = Some(normalize_ws(&lemma)).filter(|l| !l.is_empty());
                    n.onym = onym;
                    n.text = normalize_ws(&text);
                    n.genoses = genoses;
                    last_block = Some(id);
                }
                Block::Parsed {
                    lo,
                    hi,
                    onym,
                    parsings,
                } => {
                    let Some(flow) = last_flow else { continue };
                    nodes[flow.0 as usize].declared_tokens.push(DeclaredToken {
                        lo,
                        hi,
                        onym,
                        parsings,
                    });
                }
                Block::Periodos { lo, hi, id } => {
                    let Some(flow) = last_flow else { continue };
                    nodes[flow.0 as usize].declared_sentences.push((lo, hi, id));
                }
                Block::Milestone { scheme, value, at } => {
                    let Some(flow) = last_flow else { continue };
                    // `u32::MAX`: the milestone stood in a block with
                    // no prose of its own — the end of the flow
                    // block before it.
                    let at = if at == u32::MAX {
                        nodes[flow.0 as usize].text.len() as u32
                    } else {
                        at
                    };
                    let parent = enclosing_span(&nodes, flow, at, at).unwrap_or(flow);
                    let id = push(&mut nodes, Kind::Milestone, parent);
                    let n = &mut nodes[id.0 as usize];
                    n.at = Some((at, at));
                    n.fields = vec![("scheme".to_string(), scheme), ("value".to_string(), value)];
                }
                Block::Annotate { fields } => {
                    if let Some(b) = last_block {
                        nodes[b.0 as usize].fields.extend(fields);
                    }
                }
                Block::Wear { genoses } => {
                    if let Some(b) = last_block {
                        let own = &mut nodes[b.0 as usize].genoses;
                        for g in genoses {
                            if !own.contains(&g) {
                                own.push(g);
                            }
                        }
                    }
                }
                Block::Text { text } => {
                    let text = normalize_ws(&text);
                    if text.is_empty() {
                        continue;
                    }
                    match containers.last() {
                        Some(&open) => {
                            let own = &mut nodes[open.0 as usize].text;
                            if !own.is_empty() {
                                own.push(' ');
                            }
                            own.push_str(&text);
                            last_flow = Some(open);
                        }
                        None => {
                            let parent = sections.last().copied().unwrap_or(root);
                            let id = push(&mut nodes, Kind::Paragraph, parent);
                            nodes[id.0 as usize].text = text;
                            last_flow = Some(id);
                        }
                    }
                }
                Block::Open { kind, lemma } => {
                    // A note body opens at the document root —
                    // litogramma's canonical document-end
                    // placement — whatever else is open.
                    let kind = match kind {
                        Container::Note {
                            onym,
                            family,
                            margin,
                        } => {
                            let id = push(&mut nodes, family.kind(), root);
                            let n = &mut nodes[id.0 as usize];
                            n.onym = Some(onym.trim().to_string()).filter(|o| !o.is_empty());
                            n.margin = margin;
                            containers.push(id);
                            last_flow = Some(id);
                            continue;
                        }
                        other => other,
                    };
                    let parent = cursor(&sections, &containers, root);
                    let (nkind, start) = match kind {
                        Container::Blockquote => (Kind::Blockquote, None),
                        Container::UnorderedList => (Kind::UnorderedList, None),
                        Container::OrderedList { start } => (Kind::OrderedList, Some(start)),
                        Container::Item => (
                            match nodes[parent.0 as usize].kind {
                                Kind::OrderedList => Kind::OrderedItem,
                                _ => Kind::UnorderedItem,
                            },
                            None,
                        ),
                        Container::Note { .. } => unreachable!("handled above"),
                    };
                    let id = push(&mut nodes, nkind, parent);
                    last_block = Some(id);
                    nodes[id.0 as usize].lemma =
                        lemma.map(|l| normalize_ws(&l)).filter(|l| !l.is_empty());
                    if let Some(start) = start {
                        nodes[id.0 as usize].start = start;
                    }
                    if nkind == Kind::OrderedItem {
                        // `push` already appended this item, so the
                        // count includes it.
                        let nth = nodes[parent.0 as usize]
                            .children
                            .iter()
                            .filter(|&&c| nodes[c.0 as usize].kind == Kind::OrderedItem)
                            .count() as i64;
                        let start = nodes[parent.0 as usize].start;
                        nodes[id.0 as usize].taxis = Some(start + nth - 1);
                    }
                    containers.push(id);
                }
                Block::Close { hypograph } => {
                    if let Some(open) = containers.pop() {
                        nodes[open.0 as usize].hypograph = hypograph
                            .map(|h| normalize_ws(&h))
                            .filter(|h| !h.is_empty());
                    }
                }
                // The callout: a `footnote` child of the flow
                // block it sits in, `<deixis>`-traited; the edge
                // to its body resolves after the stream.
                Block::NoteRef {
                    onym,
                    family,
                    margin,
                    at,
                } => {
                    let parent = last_flow.unwrap_or(root);
                    // A declared family names the callout now; an
                    // undeclared one is settled at resolution from
                    // the body it reaches (footnote when dangling).
                    let kind = family.map(NoteFamily::kind).unwrap_or(Kind::Footnote);
                    let id = push(&mut nodes, kind, parent);
                    let n = &mut nodes[id.0 as usize];
                    n.deixis = true;
                    n.family_open = family.is_none();
                    n.margin = margin;
                    n.at = at.map(|a| (a, a));
                    n.onym = Some(onym.trim().to_string()).filter(|o| !o.is_empty());
                }
                Block::Ref {
                    target,
                    text,
                    internal,
                } => {
                    let parent = last_flow.unwrap_or(root);
                    let id = push(&mut nodes, Kind::Ref, parent);
                    let n = &mut nodes[id.0 as usize];
                    n.internal = internal;
                    n.target = Some(target.trim().to_string()).filter(|t| !t.is_empty());
                    n.text = text.map(|t| normalize_ws(&t)).unwrap_or_default();
                }
                Block::Cite { target } => {
                    let parent = last_flow.unwrap_or(root);
                    let id = push(&mut nodes, Kind::Cit, parent);
                    let n = &mut nodes[id.0 as usize];
                    n.internal = true;
                    n.target = Some(target.trim().to_string()).filter(|t| !t.is_empty());
                }
                Block::Bib {
                    key,
                    text,
                    fields,
                    genus,
                } => {
                    let parent = cursor(&sections, &containers, root);
                    let id = push(&mut nodes, Kind::Bib, parent);
                    last_block = Some(id);
                    let n = &mut nodes[id.0 as usize];
                    n.onym = Some(key.trim().to_string()).filter(|k| !k.is_empty());
                    n.text = normalize_ws(&text);
                    n.fields = fields
                        .into_iter()
                        .map(|(k, v)| (k.trim().to_string(), normalize_ws(&v)))
                        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
                        .collect();
                    n.genus = genus
                        .map(|g| g.trim().to_string())
                        .filter(|g| !g.is_empty());
                }
                Block::Anchor { onym } => {
                    let parent = last_flow.unwrap_or(root);
                    let id = push(&mut nodes, Kind::Anchor, parent);
                    nodes[id.0 as usize].onym =
                        Some(onym.trim().to_string()).filter(|o| !o.is_empty());
                }
                Block::Label { onym } => {
                    let onym = onym.trim().to_string();
                    if onym.is_empty() {
                        continue;
                    }
                    // The most recently opened block bears the
                    // name — a heading's section, a paragraph, a
                    // blockquote: the html-id / attached-\label
                    // parity rule. A label before any block, or on
                    // a block already named, degrades to a point.
                    let free = last_block
                        .map(|b| nodes[b.0 as usize].onym.is_none())
                        .unwrap_or(false);
                    match (last_block, free) {
                        (Some(b), true) => {
                            nodes[b.0 as usize].onym = Some(onym);
                        }
                        _ => {
                            let parent = last_flow.unwrap_or(root);
                            let id = push(&mut nodes, Kind::Anchor, parent);
                            nodes[id.0 as usize].onym = Some(onym);
                        }
                    }
                }
                Block::IndexMark { term } => {
                    let parent = last_flow.unwrap_or(root);
                    let id = push(&mut nodes, Kind::IndexMark, parent);
                    nodes[id.0 as usize].onym = Some(quarb_term(&term)).filter(|t| !t.is_empty());
                }
                Block::Verbatim { lang, text } => {
                    let parent = cursor(&sections, &containers, root);
                    let id = push(&mut nodes, Kind::Verbatim, parent);
                    last_block = Some(id);
                    let n = &mut nodes[id.0 as usize];
                    n.lang = lang.filter(|l| !l.is_empty());
                    n.text = text;
                }
                Block::Table {
                    lemma,
                    headers,
                    rows,
                } => {
                    let parent = cursor(&sections, &containers, root);
                    lower_table(&mut nodes, parent, lemma, headers, rows);
                }
                Block::Verse {
                    lemma,
                    strophes,
                    hypograph,
                } => {
                    let parent = cursor(&sections, &containers, root);
                    let id = lower_verse(&mut nodes, parent, lemma, strophes, hypograph);
                    last_flow = Some(id);
                    last_block = Some(id);
                }
            }
        }

        resolve_notes(&mut nodes);
        let onyms = resolve_refs(&mut nodes);
        flatten_prose(&mut nodes);
        let mut model = TextModel {
            nodes,
            root,
            onyms,
            document_url: None,
            link_base: None,
            document_path: None,
            cite_scheme: None,
            orthography: None,
            bib_aliases: Default::default(),
            head: HeadMeta::default(),
            token_index: Default::default(),
            mentions: Vec::new(),
        };
        model.link_characters();
        model
    }

    /// Register the bib-field alias census (alias → canonical
    /// campus), lowercased keys — the friction remover: a field
    /// authored or queried under any covered name answers.
    pub fn set_bib_aliases(&mut self, map: std::collections::HashMap<String, String>) {
        self.bib_aliases = map;
    }

    /// Declare the document's own URL — the base a relative ref
    /// target joins against when `-->` reaches across documents.
    pub fn set_document_url(&mut self, url: &str) {
        self.document_url = url::Url::parse(url).ok();
    }

    /// Declare the base relative reference targets join against
    /// — html's `<base href>` — when it differs from the
    /// document's own URL.
    pub fn set_link_base(&mut self, url: &str) {
        self.link_base = url::Url::parse(url).ok();
    }

    /// Declare where the document is served from when it declares
    /// no absolute URL of its own: the base `::href` links against
    /// (a mount path, an archive member's path).
    pub fn set_document_path(&mut self, path: &str) {
        self.document_path = Some(path.to_string());
    }

    /// The scheme `::cite` reads (ruling #81): the mount's
    /// `?scheme=`.
    pub fn set_cite_scheme(&mut self, scheme: &str) {
        self.cite_scheme = Some(scheme.to_string());
    }

    /// The spelling table the tokens fold through (ruling #82):
    /// the mount's `?modernize=`. An unknown table is an error.
    pub fn set_orthography(&mut self, table: &str) -> Result<(), String> {
        quarb::translit::modernize("", table)?;
        self.orthography = Some(table.to_string());
        Ok(())
    }

    /// A token's modern spelling: the form through the mount's
    /// spelling table, else the form itself.
    fn modern(&self, form: &str) -> String {
        match &self.orthography {
            Some(t) => quarb::translit::modernize(form, t).unwrap_or_else(|_| form.to_string()),
            None => form.to_string(),
        }
    }

    /// A token's `::lower`: the modern spelling, case-folded.
    fn lower(&self, form: &str) -> String {
        self.modern(form).to_lowercase()
    }

    /// The document's milestones under the citation scheme, in
    /// document order: (flow block, offset, milestone).
    fn cite_table(&self) -> Vec<(NodeId, u32, NodeId)> {
        let mut scheme = self.cite_scheme.clone();
        let mut out = Vec::new();
        for i in 0..self.nodes.len() {
            let n = &self.nodes[i];
            if n.kind != Kind::Milestone {
                continue;
            }
            let this = n
                .fields
                .iter()
                .find(|(k, _)| k == "scheme")
                .map(|(_, v)| v.clone());
            let Some(this) = this else { continue };
            match &scheme {
                None => scheme = Some(this),
                Some(s) if *s != this => continue,
                _ => {}
            }
            let at = n.at.map(|(a, _)| a).unwrap_or(0);
            let mut block = NodeId(i as u64);
            while self.nodes[block.0 as usize].kind == Kind::Span
                || self.nodes[block.0 as usize].kind == Kind::Milestone
            {
                match self.nodes[block.0 as usize].parent {
                    Some(p) => block = p,
                    None => break,
                }
            }
            out.push((block, at, NodeId(i as u64)));
        }
        out.sort_by_key(|(b, at, _)| (b.0, *at));
        out
    }

    /// The citation in force at a node (ruling #81): the value of
    /// the last milestone under the citation scheme at or before
    /// the node's point in the document — a token's or a span's
    /// offset in its block, a sentence's first token, a flow
    /// block's start; a container (a section, the document) cites
    /// as the first milestone inside it. Null where no milestone
    /// governs.
    fn cite(&self, node: NodeId) -> Option<String> {
        let table = self.cite_table();
        if table.is_empty() {
            return self.structural_cite(node);
        }
        let n = &self.nodes[node.0 as usize];
        let value = |m: NodeId| {
            self.nodes[m.0 as usize]
                .fields
                .iter()
                .find(|(k, _)| k == "value")
                .map(|(_, v)| v.clone())
        };
        // Containers: the first milestone beneath.
        let container = matches!(n.kind, Kind::Document | Kind::Section)
            || (n.kind != Kind::Span
                && n.kind != Kind::Milestone
                && n.token.is_none()
                && n.kind != Kind::Sentence
                && !self.prose_blocks().contains(&node)
                && !n.children.is_empty()
                && n.prose.is_empty());
        if container {
            let inside = |mut b: NodeId| {
                loop {
                    if b == node {
                        return true;
                    }
                    match self.nodes[b.0 as usize].parent {
                        Some(p) => b = p,
                        None => return false,
                    }
                }
            };
            return table
                .iter()
                .find(|(b, _, _)| inside(*b))
                .and_then(|(_, _, m)| value(*m));
        }
        // A note body, and everything in it, cites as its callout.
        let body = if matches!(n.kind, Kind::Footnote | Kind::Endnote | Kind::Aside) && !n.deixis {
            Some(node)
        } else {
            self.note_body_of(node)
        };
        if let Some(body) = body {
            let callout = (0..self.nodes.len())
                .find(|&i| self.nodes[i].deixis && self.nodes[i].note_edge == Some(body))
                .map(|i| NodeId(i as u64));
            return callout.and_then(|c| self.cite(c));
        }
        // Everything else: its point in the document.
        let (block, at) = match n.kind {
            _ if n.deixis && n.at.is_some() => {
                (n.parent.unwrap_or(node), n.at.map(|(a, _)| a).unwrap_or(0))
            }
            Kind::Milestone | Kind::Span => {
                let mut b = node;
                while matches!(self.nodes[b.0 as usize].kind, Kind::Span | Kind::Milestone) {
                    match self.nodes[b.0 as usize].parent {
                        Some(p) => b = p,
                        None => break,
                    }
                }
                (b, n.at.map(|(a, _)| a).unwrap_or(0))
            }
            _ if n.token.is_some() => {
                let mut b = node;
                while self.nodes[b.0 as usize].token.is_some()
                    || self.nodes[b.0 as usize].kind == Kind::Sentence
                {
                    match self.nodes[b.0 as usize].parent {
                        Some(p) => b = p,
                        None => break,
                    }
                }
                (b, n.token.as_ref().map(|t| t.at).unwrap_or(0))
            }
            Kind::Sentence if n.derived => {
                let mut toks = Vec::new();
                self.collect_tokens(node, &mut toks);
                let at = toks
                    .first()
                    .and_then(|t| self.nodes[t.0 as usize].token.as_ref().map(|t| t.at))
                    .unwrap_or(0);
                (n.parent.unwrap_or(node), at)
            }
            _ => (node, 0),
        };
        table
            .iter()
            .rev()
            .find(|(b, a, _)| (b.0, *a) <= (block.0, at))
            .and_then(|(_, _, m)| value(*m))
            .or_else(|| self.structural_cite(node))
    }

    /// The citation a text without milestones still has: the
    /// enclosing sections' names from the outermost in, joined by
    /// " / " — a chapter, an act and its scene. Null at the root.
    fn structural_cite(&self, node: NodeId) -> Option<String> {
        let mut names: Vec<String> = Vec::new();
        let mut cur = Some(node);
        while let Some(n) = cur {
            let node = &self.nodes[n.0 as usize];
            if node.kind == Kind::Section
                && let Some(l) = &node.lemma
                && !l.is_empty()
            {
                names.push(l.clone());
            }
            cur = node.parent;
        }
        if names.is_empty() {
            return None;
        }
        names.reverse();
        Some(names.join(" / "))
    }

    /// The base `::href` links against: the document's URL, else
    /// its declared path.
    fn href_base(&self) -> Option<String> {
        if let Some(u) = &self.document_url {
            return Some(u.to_string());
        }
        self.document_path.clone()
    }

    /// The nearest bearer of a name, the node itself first, then
    /// its ancestors — the anchor a link into this node lands on.
    /// `exact` says the node bears it itself.
    fn nearest_onym(&self, node: NodeId) -> Option<(String, bool)> {
        let mut cur = Some(node);
        let mut exact = true;
        while let Some(id) = cur {
            let n = &self.nodes[id.0 as usize];
            if n.kind != Kind::Document
                && !n.deixis
                && let Some(o) = &n.onym
            {
                return Some((o.clone(), exact));
            }
            exact = false;
            cur = n.parent;
        }
        None
    }

    /// A link into `node`, as far as the served form can narrow
    /// it. The base is the document's URL (its canonical link,
    /// else the mount's path). An HTML page — a base ending in
    /// `.html` / `.htm` / `/` or without an extension — narrows
    /// with the nearest anchor and, for a block below the anchored
    /// one, a text fragment (`#:~:text=start,end`) built from the
    /// node's own prose. A PDF narrows to nothing yet; any other
    /// form links the document alone, since no browser takes a
    /// fragment into it. The narrowing stays available as data
    /// beside the link (`::anchor`, the prose) for a client that
    /// renders the document itself.
    fn href(&self, node: NodeId) -> Option<String> {
        let base = self.href_base()?;
        let n = &self.nodes[node.0 as usize];
        if n.kind == Kind::Document {
            return Some(base);
        }
        let served = base.split(['?', '#']).next().unwrap_or(&base);
        let last = served.rsplit('/').next().unwrap_or(served);
        let html = served.ends_with('/')
            || !last.contains('.')
            || last.ends_with(".html")
            || last.ends_with(".htm");
        if !html {
            return Some(base);
        }
        let mut out = base;
        let (anchor, exact) = match self.nearest_onym(node) {
            Some((a, exact)) => (Some(a), exact),
            None => (None, false),
        };
        if let Some(a) = &anchor {
            out.push('#');
            out.push_str(a);
        }
        if !exact {
            let words: Vec<&str> = n.prose.split_whitespace().collect();
            if !words.is_empty() {
                use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
                let enc =
                    |ws: &[&str]| utf8_percent_encode(&ws.join(" "), NON_ALPHANUMERIC).to_string();
                if anchor.is_none() {
                    out.push('#');
                }
                out.push_str(":~:text=");
                if words.len() <= 12 {
                    out.push_str(&enc(&words));
                } else {
                    out.push_str(&enc(&words[..6]));
                    out.push(',');
                    out.push_str(&enc(&words[words.len() - 6..]));
                }
            }
        }
        Some(out)
    }

    /// The document's title for a link: what its head declares,
    /// else its first top-level section's lemma.
    fn document_title(&self) -> Option<String> {
        if let Some(t) = &self.head.title {
            return Some(t.clone());
        }
        self.nodes[self.root.0 as usize]
            .children
            .iter()
            .map(|c| &self.nodes[c.0 as usize])
            .find(|n| n.kind == Kind::Section)
            .and_then(|n| n.lemma.clone())
    }

    /// Attach what the document declares about itself; the format
    /// adapter reads it from the source's own metadata vocabulary.
    pub fn set_head_meta(&mut self, head: HeadMeta) {
        self.head = head;
    }

    /// What the document declares about itself.
    pub fn head_meta(&self) -> &HeadMeta {
        &self.head
    }

    /// The document's text as one string: every section's lemma
    /// and every block's prose, one per line, in document order —
    /// the lowering a substrate stores beside the document so a
    /// substring predicate inside the graft can be prefiltered on
    /// it. The invariant a prefilter relies on: every grafted
    /// node's prose (and every lemma) is a substring of this.
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        let mut stack = vec![self.root];
        while let Some(id) = stack.pop() {
            let n = &self.nodes[id.0 as usize];
            if let Some(l) = &n.lemma
                && !l.is_empty()
            {
                out.push_str(l);
                out.push('\n');
            }
            // Every node's own prose, leaf or not: a paragraph that
            // holds a ref or an anchor still has prose of its own,
            // and the invariant covers it too.
            if !n.prose.is_empty() && n.kind != Kind::Document {
                out.push_str(&n.prose);
                out.push('\n');
            }
            for c in n.children.iter().rev() {
                stack.push(*c);
            }
        }
        out
    }

    /// How many nodes the document holds (the root included) — the
    /// bound a host needs to pack this document's node ids into a
    /// wider id space.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Read plain text: blank-line-separated paragraphs, each
    /// collapsed to one line — the atramento paragraph rule. No
    /// headings, no markup.
    pub fn parse_plain(text: &str) -> Self {
        let mut blocks = Vec::new();
        let mut para: Vec<&str> = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                if !para.is_empty() {
                    blocks.push(Block::Paragraph {
                        text: para.join(" "),
                    });
                    para.clear();
                }
            } else {
                para.push(line);
            }
        }
        if !para.is_empty() {
            blocks.push(Block::Paragraph {
                text: para.join(" "),
            });
        }
        Self::build(blocks)
    }

    /// A locator path to `node`, like `/section[2]/paragraph[3]`,
    /// for rendering. A `[n]` index is added only to disambiguate
    /// same-name siblings.
    pub fn locator(&self, node: NodeId) -> String {
        let mut segments = Vec::new();
        let mut cur = Some(node);
        while let Some(id) = cur {
            let n = &self.nodes[id.0 as usize];
            if let Some(name) = n.kind.name() {
                segments.push(self.segment(id, name));
            }
            cur = n.parent;
        }
        segments.reverse();
        format!("/{}", segments.join("/"))
    }

    fn segment(&self, node: NodeId, name: &str) -> String {
        let Some(parent) = self.nodes[node.0 as usize].parent else {
            return name.to_string();
        };
        let siblings = &self.nodes[parent.0 as usize].children;
        let same_name: Vec<NodeId> = siblings
            .iter()
            .copied()
            .filter(|&s| self.nodes[s.0 as usize].kind == self.nodes[node.0 as usize].kind)
            .collect();
        if same_name.len() > 1 {
            let n = same_name.iter().position(|&s| s == node).unwrap() + 1;
            format!("{name}[{n}]")
        } else {
            name.to_string()
        }
    }
}

/// Where the next block lands: the innermost open container, else
/// the innermost open section, else the root.
/// Ruling #35: wire each callout to its body by (family, onym) —
/// first body wins on a duplicate — build the reverse index, and
/// mark the dangling: a callout with no body keeps its node,
/// carries `<dangling>` and `::::resolved = false`, and emits no
/// edge.
fn resolve_notes(nodes: &mut [Node]) {
    let mut bodies: std::collections::HashMap<(Kind, String), NodeId> = Default::default();
    for (i, n) in nodes.iter().enumerate() {
        if matches!(n.kind, Kind::Footnote | Kind::Endnote | Kind::Aside)
            && !n.deixis
            && let Some(o) = &n.onym
        {
            bodies
                .entry((n.kind, o.clone()))
                .or_insert(NodeId(i as u64));
        }
    }
    for i in 0..nodes.len() {
        if !matches!(nodes[i].kind, Kind::Footnote | Kind::Endnote | Kind::Aside)
            || !nodes[i].deixis
        {
            continue;
        }
        let Some(onym) = nodes[i].onym.clone() else {
            nodes[i].dangling = true;
            continue;
        };
        let hit = if nodes[i].family_open {
            // No declared family (an HTML noteref): the body's
            // family is the callout's; footnote when dangling.
            [Kind::Footnote, Kind::Endnote, Kind::Aside]
                .into_iter()
                .find_map(|k| bodies.get(&(k, onym.clone())).map(|b| (k, *b)))
        } else {
            bodies
                .get(&(nodes[i].kind, onym))
                .map(|b| (nodes[i].kind, *b))
        };
        match hit {
            Some((family, body)) => {
                nodes[i].kind = family;
                nodes[i].note_edge = Some(body);
                let callout = NodeId(i as u64);
                nodes[body.0 as usize].cites.push(callout);
            }
            None => nodes[i].dangling = true,
        }
    }
}

/// An index term as written, with the `|...` formatting and range
/// directives stripped (v1 of ruling #36; ranges and
/// see-references are recorded follow-ups).
fn quarb_term(term: &str) -> String {
    normalize_ws(term.split('|').next().unwrap_or(term))
}

fn cursor(sections: &[NodeId], containers: &[NodeId], root: NodeId) -> NodeId {
    containers
        .last()
        .or(sections.last())
        .copied()
        .unwrap_or(root)
}

impl TextModel {
    /// The corpus reading (ruling #62): give every prose block its
    /// tokens as children. Segmentation is UAX #29 word boundaries
    /// (the standard `sentences` already follows): `don’t`, `Tom’s`
    /// and `3,000` are one token each, an em dash is its own, and
    /// whitespace is none. Positions and sentence ordinals count
    /// through the document in reading order. The block's own
    /// `::` is unchanged; the tokens sit after any children it
    /// already had (an inline ref, a callout).
    /// The literary reading's cast layer (ruling #79): declare the
    /// characters a cast table names (those the source did not),
    /// annotate every word token a row's pattern matches with the
    /// row's id as its `prosopon`, then resolve every mention to
    /// its character.
    pub fn apply_cast(&mut self, rows: &[CastRow]) {
        for row in rows {
            let known = self
                .nodes
                .iter()
                .any(|n| n.kind == Kind::Character && n.onym.as_deref() == Some(row.id.as_str()));
            if known {
                continue;
            }
            let root = NodeId(0);
            let id = push(&mut self.nodes, Kind::Character, root);
            let n = &mut self.nodes[id.0 as usize];
            n.lemma = Some(row.name.clone()).filter(|l| !l.is_empty());
            n.onym = Some(row.id.clone());
            n.fields = row.fields.clone();
        }
        // A pattern of several words ("Aunt Polly") matches a run
        // of word tokens under one parent, joined by a space; the
        // run's first token carries the mention.
        let patterns: Vec<(&CastRow, &regex::Regex, usize)> = rows
            .iter()
            .filter_map(|r| r.pattern.as_ref().map(|p| (r, p, r.words.max(1))))
            .collect();
        let widest = patterns.iter().map(|(_, _, w)| *w).max().unwrap_or(0);
        if widest > 0 {
            let words: Vec<usize> = (0..self.nodes.len())
                .filter(|&i| {
                    self.nodes[i]
                        .token
                        .as_ref()
                        .is_some_and(|t| t.class == TokenClass::Word)
                })
                .collect();
            for (k, &i) in words.iter().enumerate() {
                if self.nodes[i].fields.iter().any(|(k, _)| k == "prosopon") {
                    continue;
                }
                let mut run = vec![self.nodes[i].text.as_str()];
                for &j in words.iter().skip(k + 1).take(widest - 1) {
                    if self.nodes[j].parent != self.nodes[i].parent {
                        break;
                    }
                    run.push(self.nodes[j].text.as_str());
                }
                // A pattern whose alternatives differ in length
                // ("Tom|Tom Sawyer") is tried from its widest run
                // down to one token.
                let hit = patterns.iter().find(|(_, p, w)| {
                    (1..=(*w).min(run.len()))
                        .rev()
                        .any(|n| p.is_match(&run[..n].join(" ")))
                });
                if let Some((row, _, _)) = hit {
                    self.nodes[i]
                        .fields
                        .push(("prosopon".to_string(), row.id.clone()));
                }
            }
        }
        // A coreference chain (a `cluster` column, the source's
        // chain id): every mention of the chain is the character's,
        // its first token carrying the mention.
        let chains: Vec<(&CastRow, &str)> = rows
            .iter()
            .filter_map(|r| {
                r.fields
                    .iter()
                    .find(|(k, _)| k == "cluster" || k == "chain")
                    .map(|(_, v)| (r, v.as_str()))
            })
            .collect();
        if !chains.is_empty() {
            let firsts: Vec<(NodeId, String)> = self
                .mentions
                .iter()
                .filter_map(|m| {
                    let c = m.cluster.as_deref()?;
                    let (row, _) = chains.iter().find(|(_, id)| *id == c)?;
                    Some((*m.tokens.first()?, row.id.clone()))
                })
                .collect();
            for (t, id) in firsts {
                let f = &mut self.nodes[t.0 as usize].fields;
                if !f.iter().any(|(k, _)| k == "prosopon") {
                    f.push(("prosopon".to_string(), id));
                }
            }
        }
        // A play's speech names its speaker by the label the source
        // printed (its lemma): a label a pattern matches is that
        // character's speech.
        if !patterns.is_empty() {
            for i in 0..self.nodes.len() {
                let n = &self.nodes[i];
                if n.kind != Kind::Dialogue || n.fields.iter().any(|(k, _)| k == "prosopon") {
                    continue;
                }
                let Some(label) = n.lemma.clone() else {
                    continue;
                };
                if let Some((row, _, _)) = patterns.iter().find(|(_, p, _)| p.is_match(&label)) {
                    self.nodes[i]
                        .fields
                        .push(("prosopon".to_string(), row.id.clone()));
                }
            }
        }
        self.link_characters();
    }

    /// Resolve `->character`: every node whose own `prosopon`
    /// names a declared character links to it, and the character
    /// lists it among its mentions.
    fn link_characters(&mut self) {
        let mut by_onym: std::collections::HashMap<String, NodeId> =
            std::collections::HashMap::new();
        for (i, n) in self.nodes.iter().enumerate() {
            if n.kind == Kind::Character
                && let Some(o) = &n.onym
            {
                by_onym.entry(o.clone()).or_insert(NodeId(i as u64));
            }
        }
        for n in &mut self.nodes {
            n.mentions.clear();
        }
        for i in 0..self.nodes.len() {
            if self.nodes[i].kind == Kind::Character {
                continue;
            }
            let target = self.nodes[i]
                .fields
                .iter()
                .find(|(k, _)| k == "prosopon")
                .and_then(|(_, v)| by_onym.get(v).copied());
            self.nodes[i].character = target;
            if let Some(c) = target {
                self.nodes[c.0 as usize].mentions.push(NodeId(i as u64));
            }
        }
    }

    /// The literary reading's gazetteer (ruling #87): a places
    /// table declares the places a source names, as a cast table
    /// declares its people. Each row is a `place` node under the
    /// root (`::lemma` its name, `::onym` its id, every other
    /// column a field — `::lat`, `::lon`, `::kind`); a word token a
    /// row's pattern matches answers the row's id as `::chora` and
    /// reaches the place as `->place`, and the place reaches its
    /// mentions back as `<-place`.
    pub fn apply_places(&mut self, rows: &[CastRow]) {
        for row in rows {
            let known = self
                .nodes
                .iter()
                .any(|n| n.kind == Kind::Place && n.onym.as_deref() == Some(row.id.as_str()));
            if known {
                continue;
            }
            let root = NodeId(0);
            let id = push(&mut self.nodes, Kind::Place, root);
            let n = &mut self.nodes[id.0 as usize];
            n.lemma = Some(row.name.clone()).filter(|l| !l.is_empty());
            n.onym = Some(row.id.clone());
            n.fields = row.fields.clone();
        }
        let patterns: Vec<(&CastRow, &regex::Regex, usize)> = rows
            .iter()
            .filter_map(|r| r.pattern.as_ref().map(|p| (r, p, r.words.max(1))))
            .collect();
        let widest = patterns.iter().map(|(_, _, w)| *w).max().unwrap_or(0);
        if widest > 0 {
            let words: Vec<usize> = (0..self.nodes.len())
                .filter(|&i| {
                    self.nodes[i]
                        .token
                        .as_ref()
                        .is_some_and(|t| t.class == TokenClass::Word)
                })
                .collect();
            for (k, &i) in words.iter().enumerate() {
                if self.nodes[i].fields.iter().any(|(k, _)| k == "chora") {
                    continue;
                }
                let mut run = vec![self.nodes[i].text.as_str()];
                for &j in words.iter().skip(k + 1).take(widest - 1) {
                    if self.nodes[j].parent != self.nodes[i].parent {
                        break;
                    }
                    run.push(self.nodes[j].text.as_str());
                }
                // A pattern whose alternatives differ in length
                // ("Tom|Tom Sawyer") is tried from its widest run
                // down to one token.
                let hit = patterns.iter().find(|(_, p, w)| {
                    (1..=(*w).min(run.len()))
                        .rev()
                        .any(|n| p.is_match(&run[..n].join(" ")))
                });
                if let Some((row, _, _)) = hit {
                    self.nodes[i]
                        .fields
                        .push(("chora".to_string(), row.id.clone()));
                }
            }
        }
        self.link_places();
    }

    /// Resolve `->place`: every node whose own `chora` names a
    /// declared place links to it, and the place lists it among
    /// its mentions.
    fn link_places(&mut self) {
        let mut by_onym: std::collections::HashMap<String, NodeId> =
            std::collections::HashMap::new();
        for (i, n) in self.nodes.iter().enumerate() {
            if n.kind == Kind::Place
                && let Some(o) = &n.onym
            {
                by_onym.entry(o.clone()).or_insert(NodeId(i as u64));
            }
        }
        for n in &mut self.nodes {
            if n.kind == Kind::Place {
                n.mentions.clear();
            }
        }
        for i in 0..self.nodes.len() {
            if self.nodes[i].kind == Kind::Place {
                continue;
            }
            let target = self.nodes[i]
                .fields
                .iter()
                .find(|(k, _)| k == "chora")
                .and_then(|(_, v)| by_onym.get(v).copied());
            self.nodes[i].place = target;
            if let Some(c) = target {
                self.nodes[c.0 as usize].mentions.push(NodeId(i as u64));
            }
        }
    }

    pub fn tokenize(&mut self) {
        self.tokenize_with(None)
    }

    /// The prose blocks the corpus reading tokenizes, in reading
    /// order.
    fn prose_blocks(&self) -> Vec<NodeId> {
        (0..self.nodes.len())
            .filter(|&id| {
                let node = &self.nodes[id];
                let prose_block = match node.kind {
                    // A treebank paragraph delegates to its declared
                    // sentences (ruling #63); the sentences the
                    // corpus reading derives beneath a block (ruling
                    // #64) are annotation, not blocks.
                    Kind::Paragraph => !node.children.iter().any(|c| {
                        let c = &self.nodes[c.0 as usize];
                        c.kind == Kind::Sentence && !c.derived
                    }),
                    Kind::Sentence => !node.derived,
                    Kind::UnorderedItem | Kind::OrderedItem | Kind::Stichos => true,
                    Kind::Speech | Kind::Dialogue => true,
                    Kind::Footnote | Kind::Endnote | Kind::Aside => !node.deixis,
                    _ => false,
                };
                prose_block && !node.prose.is_empty() && !self.in_apparatus(NodeId(id as u64))
            })
            .map(|id| NodeId(id as u64))
            .collect()
    }

    /// The note body a node stands in, if any: the nearest
    /// ancestor that is a footnote, endnote or aside body (not the
    /// callout). The corpus reading tokenizes the text, not the
    /// apparatus; a node in a body cites as the body's callout.
    fn note_body_of(&self, node: NodeId) -> Option<NodeId> {
        let mut cur = self.nodes[node.0 as usize].parent;
        while let Some(p) = cur {
            let n = &self.nodes[p.0 as usize];
            if matches!(n.kind, Kind::Footnote | Kind::Endnote | Kind::Aside) && !n.deixis {
                return Some(p);
            }
            cur = n.parent;
        }
        None
    }

    fn in_apparatus(&self, node: NodeId) -> bool {
        let n = &self.nodes[node.0 as usize];
        (matches!(n.kind, Kind::Footnote | Kind::Endnote | Kind::Aside) && !n.deixis)
            || self.note_body_of(node).is_some()
    }

    /// The document's prose as one text — every prose block's `::`,
    /// blank-line separated, in reading order — the text an
    /// external annotator reads and a CoNLL-U source aligns to.
    pub fn corpus_text(&self) -> String {
        self.prose_blocks()
            .iter()
            .map(|&b| self.nodes[b.0 as usize].prose.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Whether the corpus reading's tokens are already in place.
    pub fn is_tokenized(&self) -> bool {
        self.nodes.iter().any(|n| n.kind == Kind::Token)
    }

    /// [`tokenize`](Self::tokenize) with sentence bonds: a `.desm`
    /// set (the `syndesmos` crate) whose abbreviations and patterns
    /// undo UAX #29 breaks.
    pub fn tokenize_with(&mut self, bonds: Option<&syndesmos::Syndesmos>) {
        use unicode_segmentation::UnicodeSegmentation;
        self.token_index = Default::default();
        let mut n: u32 = 0;
        let mut sentence: u32 = 0;
        for block in self.prose_blocks() {
            let id = block.0 as usize;
            let prose = self.nodes[id].prose.clone();
            // The sentence tier (ruling #64): one derived sentence
            // node per span, under the block; a block the
            // segmenter leaves whole is one sentence.
            // The parsing pack (ruling #80): the source's own
            // sentences and tokens stand in for the segmenter's
            // where the source declared them.
            let declared_sentences = std::mem::take(&mut self.nodes[id].declared_sentences);
            let mut declared_tokens = std::mem::take(&mut self.nodes[id].declared_tokens);
            declared_tokens.sort_by_key(|t| t.lo);
            let mut spans = if declared_sentences.is_empty() {
                sentence_spans(&prose, bonds)
            } else {
                declared_sentences
                    .iter()
                    .map(|(lo, hi, _)| (*lo as usize, *hi as usize))
                    .filter(|(lo, hi)| lo < hi && *hi <= prose.len())
                    .collect()
            };
            if spans.is_empty() {
                spans.push((0, prose.len()));
            }
            let sentence_ids: Vec<Option<String>> = if declared_sentences.is_empty() {
                Vec::new()
            } else {
                declared_sentences
                    .iter()
                    .map(|(_, _, sid)| Some(sid.clone()))
                    .collect()
            };
            let sentences: Vec<(usize, usize, u32, NodeId)> = spans
                .into_iter()
                .enumerate()
                .map(|(k, (lo, hi))| {
                    sentence += 1;
                    let node = self.push_derived_sentence(block, &prose[lo..hi], sentence);
                    if let Some(Some(sid)) = sentence_ids.get(k) {
                        self.nodes[node.0 as usize]
                            .fields
                            .push(("sent_id".to_string(), sid.clone()));
                    }
                    (lo, hi, sentence, node)
                })
                .collect();
            // Declared tokens by onym, for the heads.
            let mut by_onym: Vec<(String, NodeId)> = Vec::new();
            let mut heads: Vec<(NodeId, String)> = Vec::new();
            let mut next_declared = 0usize;
            let mut skip_until = 0usize;
            for (at, seg) in prose.split_word_bound_indices() {
                if at < skip_until {
                    continue;
                }
                // A declared token starting here takes the place
                // of the segmenter's, however many word bounds
                // it spans.
                while next_declared < declared_tokens.len()
                    && (declared_tokens[next_declared].lo as usize) < at
                {
                    next_declared += 1;
                }
                if let Some(d) = declared_tokens.get(next_declared)
                    && d.lo as usize == at
                    && (d.hi as usize) > at
                    && (d.hi as usize) <= prose.len()
                {
                    let d = d.clone();
                    next_declared += 1;
                    skip_until = d.hi as usize;
                    let form = &prose[d.lo as usize..d.hi as usize];
                    n += 1;
                    let (in_sentence, span, parent) = sentences
                        .iter()
                        .find(|(lo, hi, _, _)| *lo <= at && at < *hi)
                        .or_else(|| sentences.last())
                        .map(|(lo, hi, s, node)| (*s, (*lo as u32, *hi as u32), *node))
                        .expect("a block has at least one sentence");
                    let tok = self.push_parsed(parent, form, &d, n, in_sentence);
                    if let Some(t) = self.nodes[tok.0 as usize].token.as_mut() {
                        t.span = span;
                        t.at = at as u32;
                    }
                    if let Some(o) = &d.onym {
                        by_onym.push((o.clone(), tok));
                    }
                    if let Some(h) = d.parsings.first().and_then(|p| p.kephale.clone()) {
                        heads.push((tok, h));
                    }
                    continue;
                }
                if seg.chars().all(char::is_whitespace) {
                    continue;
                }
                let class = class_of(seg);
                n += 1;
                let (in_sentence, span, parent) = sentences
                    .iter()
                    .find(|(lo, hi, _, _)| *lo <= at && at < *hi)
                    .or_else(|| sentences.last())
                    .map(|(lo, hi, s, node)| (*s, (*lo as u32, *hi as u32), *node))
                    .expect("a block has at least one sentence");
                let tok = push(&mut self.nodes, Kind::Token, parent);
                let t = &mut self.nodes[tok.0 as usize];
                t.text = seg.to_string();
                t.prose = seg.to_string();
                t.token = Some(TokenInfo {
                    class,
                    n,
                    sentence: in_sentence,
                    span,
                    annot: None,
                    at: at as u32,
                    spans: Vec::new(),
                });
            }
            // The declared heads (`->head`, `<-head`), by onym.
            for (tok, h) in heads {
                let Some((_, head)) = by_onym.iter().find(|(o, _)| *o == h) else {
                    continue;
                };
                let head = *head;
                if let Some(a) = self.nodes[tok.0 as usize]
                    .token
                    .as_mut()
                    .and_then(|t| t.annot.as_mut())
                {
                    a.head = Some(head);
                }
                if let Some(a) = self.nodes[head.0 as usize]
                    .token
                    .as_mut()
                    .and_then(|t| t.annot.as_mut())
                {
                    a.dependents.push(tok);
                }
            }
        }
        self.attribute_spans();
    }

    /// A token the source parsed (ruling #80): the annotation the
    /// CoNLL-U reading would carry, from the first parsing —
    /// `::lemma` the lexema, `::upos`/`::xpos` the meros (split on
    /// `/`), `::feats` the parepomena with every `Key=Value` a
    /// property, `::deprel` the schesis, `::sem` the semasia; the
    /// alternative parsings answer as `::alt`, lemma and meros
    /// joined, one per parsing.
    fn push_parsed(
        &mut self,
        sentence_node: NodeId,
        form: &str,
        d: &DeclaredToken,
        n: u32,
        sentence: u32,
    ) -> NodeId {
        let node = push(&mut self.nodes, Kind::Token, sentence_node);
        let t = &mut self.nodes[node.0 as usize];
        t.text = form.to_string();
        t.prose = form.to_string();
        let first = d.parsings.first().cloned().unwrap_or_default();
        let (upos, xpos) = match first.meros.as_deref() {
            Some(m) => match m.split_once('/') {
                Some((u, x)) => (Some(u.to_string()), Some(x.to_string())),
                None => (Some(m.to_string()), None),
            },
            None => (None, None),
        };
        let feats = first.parepomena.clone();
        // `Key=Value` features (TEI's msd, UD) answer under their
        // keys; a tagset's bare grammemes (RNC's `m,anim=sg,nom`)
        // stay the raw `::feats`.
        let mut keys = match feats.as_deref() {
            Some(f) if f.split([',', '|']).all(|p| p.contains('=')) => {
                key_values(&f.replace(',', "|"))
            }
            _ => Vec::new(),
        };
        if let Some(sem) = &first.semasia {
            keys.push(("sem".to_string(), sem.clone()));
        }
        for alt in d.parsings.iter().skip(1) {
            let mut v = alt.lexema.clone().unwrap_or_default();
            if let Some(m) = &alt.meros {
                v.push(' ');
                v.push_str(m);
            }
            if let Some(f) = &alt.parepomena {
                v.push(' ');
                v.push_str(f);
            }
            keys.push(("alt".to_string(), v.trim().to_string()));
        }
        t.token = Some(TokenInfo {
            class: class_of_annotated(form, upos.as_deref().unwrap_or("")),
            n,
            sentence,
            span: (0, 0),
            at: 0,
            spans: Vec::new(),
            annot: Some(Box::new(Annot {
                id: d.onym.clone().unwrap_or_else(|| n.to_string()),
                lemma: first.lexema.clone(),
                upos,
                xpos,
                feats,
                deprel: first.schesis.clone(),
                deps: None,
                misc: None,
                keys,
                mwt: None,
                empty: false,
                head: None,
                dependents: Vec::new(),
                eheads: Vec::new(),
                edependents: Vec::new(),
                mentions: Vec::new(),
            })),
        });
        node
    }

    /// The literary reading: give every span the tokens its byte
    /// range covers (`->token`), and every token its spans,
    /// outermost first — after the tokens exist.
    fn attribute_spans(&mut self) {
        let spans: Vec<NodeId> = (0..self.nodes.len())
            .filter(|&i| self.nodes[i].kind == Kind::Span && self.nodes[i].at.is_some())
            .map(|i| NodeId(i as u64))
            .collect();
        if spans.is_empty() {
            return;
        }
        for span in spans {
            let (lo, hi) = self.nodes[span.0 as usize].at.expect("filtered");
            // The flow block the span belongs to: up past the
            // enclosing spans.
            let mut block = span;
            while self.nodes[block.0 as usize].kind == Kind::Span
                && let Some(p) = self.nodes[block.0 as usize].parent
            {
                block = p;
            }
            let mut tokens = Vec::new();
            self.collect_tokens(block, &mut tokens);
            let covered: Vec<NodeId> = tokens
                .into_iter()
                .filter(|t| {
                    let at = self.nodes[t.0 as usize].token.as_ref().map(|t| t.at);
                    at.is_some_and(|at| lo <= at && at < hi)
                })
                .collect();
            for &t in &covered {
                if let Some(info) = self.nodes[t.0 as usize].token.as_mut()
                    && !info.spans.contains(&span)
                {
                    info.spans.push(span);
                }
            }
            self.nodes[span.0 as usize].span_tokens = covered;
        }
        // Spans are pushed in pre-order, so node order is outermost
        // first.
        for n in self.nodes.iter_mut() {
            if let Some(t) = n.token.as_mut() {
                t.spans.sort();
            }
        }
    }

    fn collect_tokens(&self, node: NodeId, out: &mut Vec<NodeId>) {
        for &c in &self.nodes[node.0 as usize].children {
            let n = &self.nodes[c.0 as usize];
            if n.token.is_some() {
                out.push(c);
            } else if n.kind == Kind::Sentence {
                self.collect_tokens(c, out);
            }
        }
    }

    /// One sentence of the corpus reading's sentence tier (ruling
    /// #64), under `block`: a `sentence` node the segmentation in
    /// force derived, its `::` the span's text, its ordinal the
    /// document-wide sentence count. Annotation, never a block:
    /// the block's own prose is unchanged and the renderers never
    /// see it.
    fn push_derived_sentence(&mut self, block: NodeId, text: &str, ordinal: u32) -> NodeId {
        let node = push(&mut self.nodes, Kind::Sentence, block);
        let s = &mut self.nodes[node.0 as usize];
        s.text = text.trim().to_string();
        s.prose = s.text.clone();
        s.taxis = Some(ordinal as i64);
        s.derived = true;
        node
    }

    /// The corpus reading from a CoNLL-U annotation (ruling #62's
    /// second layer; ruling #63 carries every column): the
    /// source's sentences and tokens replace the built-in
    /// segmentation wholesale, aligned to the document's prose by
    /// character offset — each token's form is found at the
    /// cursor, whitespace skipped — and every token carries the
    /// source's lemma, parts of speech, features, dependency
    /// relation, head edge, enhanced heads and miscellany. A form
    /// that does not align is an error naming the offset: the
    /// annotation must be of this text. A multiword range (`1-2
    /// don't`) aligns as the unit and its syntactic words share its
    /// span; an empty node (`8.1`) is not in the text and consumes
    /// none of it.
    pub fn annotate_conllu(&mut self, conllu: &str) -> Result<(), String> {
        if self.is_tokenized() {
            return Err("the document is already tokenized".into());
        }
        self.token_index = Default::default();
        let blocks = self.prose_blocks();
        // The document text and each block's range in it.
        let mut doc = String::new();
        let mut ranges: Vec<(usize, usize, NodeId)> = Vec::new();
        for (i, &b) in blocks.iter().enumerate() {
            if i > 0 {
                doc.push_str("\n\n");
            }
            let lo = doc.len();
            doc.push_str(&self.nodes[b.0 as usize].prose);
            ranges.push((lo, doc.len(), b));
        }
        if ranges.is_empty() {
            return Err("the document has no prose to annotate".into());
        }
        let parsed = parse_conllu(conllu)?;
        let mut cursor = 0usize;
        let mut block_at = 0usize;
        let mut n: u32 = 0;
        for (si, sent) in parsed.sentences.iter().enumerate() {
            let ordinal = si as u32 + 1;
            let mut made: Vec<(NodeId, &ConlluToken)> = Vec::new();
            // The (block, lo, hi) extents the sentence covers — and
            // the sentence node under each block (ruling #64): a
            // sentence the annotator runs across a block boundary
            // is one node per block, sharing the ordinal.
            let mut extents: Vec<(NodeId, usize, usize)> = Vec::new();
            let mut nodes_by_block: Vec<(NodeId, NodeId)> = Vec::new();
            // A multiword range in force: its span, last word id,
            // id and surface form.
            let mut range: Option<(usize, usize, u32, String, String)> = None;
            for tok in &sent.tokens {
                // A whitespace-only form (a tool's SPACE token) is
                // no token of the text.
                if !tok.empty && tok.form.trim().is_empty() {
                    continue;
                }
                let (lo, hi, mwt) = if tok.empty {
                    (cursor, cursor, None)
                } else if let Some((lo, hi, until, id, form)) = &range
                    && tok.range.is_none()
                    && tok.ord <= *until
                {
                    (*lo, *hi, Some((id.clone(), form.clone())))
                } else {
                    range = None;
                    while doc[cursor..].starts_with(char::is_whitespace) {
                        cursor += doc[cursor..].chars().next().map_or(0, char::len_utf8);
                    }
                    if !doc[cursor..].starts_with(tok.form.as_str()) {
                        let seen: String = doc[cursor..].chars().take(24).collect();
                        return Err(format!(
                            "CoNLL-U sentence {ordinal}, token {}: {:?} does not align with the text at offset {cursor} ({seen:?})",
                            tok.id, tok.form
                        ));
                    }
                    let lo = cursor;
                    cursor += tok.form.len();
                    if let Some((_, b)) = tok.range {
                        range = Some((lo, cursor, b, tok.id.clone(), tok.form.clone()));
                        continue;
                    }
                    (lo, cursor, None)
                };
                if !tok.empty {
                    while block_at + 1 < ranges.len() && lo >= ranges[block_at].1 {
                        block_at += 1;
                    }
                }
                let (blo, _, block) = ranges[block_at];
                if !tok.empty {
                    n += 1;
                }
                let sentence_node = match nodes_by_block.iter().find(|(b, _)| *b == block) {
                    Some((_, s)) => *s,
                    None => {
                        let s = self.push_derived_sentence(block, "", ordinal);
                        nodes_by_block.push((block, s));
                        s
                    }
                };
                let node = self.push_annotated(
                    sentence_node,
                    tok,
                    if tok.empty { 0 } else { n },
                    ordinal,
                    mwt,
                );
                if !tok.empty {
                    match extents.iter_mut().find(|(b, _, _)| *b == block) {
                        Some(e) => {
                            e.1 = e.1.min(lo - blo);
                            e.2 = e.2.max(hi - blo);
                        }
                        None => extents.push((block, lo - blo, hi - blo)),
                    }
                    if let Some(t) = self.nodes[node.0 as usize].token.as_mut() {
                        t.at = (lo - blo) as u32;
                    }
                }
                made.push((node, tok));
            }
            // The sentence's span per block — on its node's text
            // and on each token — then the edges.
            for (block, s) in &nodes_by_block {
                if let Some((_, lo, hi)) = extents.iter().find(|(b, _, _)| b == block) {
                    let text = self.nodes[block.0 as usize].prose[*lo..*hi]
                        .trim()
                        .to_string();
                    let sn = &mut self.nodes[s.0 as usize];
                    sn.text = text.clone();
                    sn.prose = text;
                }
            }
            for (node, _) in &made {
                let sentence_node = self.nodes[node.0 as usize]
                    .parent
                    .expect("token has a sentence");
                let block = self.nodes[sentence_node.0 as usize]
                    .parent
                    .expect("sentence has a block");
                if let Some((_, lo, hi)) = extents.iter().find(|(b, _, _)| *b == block)
                    && let Some(t) = self.nodes[node.0 as usize].token.as_mut()
                {
                    t.span = (*lo as u32, *hi as u32);
                }
            }
            self.link_sentence(&made);
        }
        let header = parsed
            .globals
            .iter()
            .find(|(k, _)| k == "Entity")
            .map(|(_, v)| v.as_str());
        self.decode_mentions(header);
        Ok(())
    }

    /// A treebank read as a document (ruling #63): the file is the
    /// root; `# newdoc` opens a section, `# newpar` a paragraph,
    /// each sentence block a `sentence` node whose `::` is its
    /// `# text` (else the forms joined per `SpaceAfter`) and whose
    /// comments answer as properties. No tokens: the reading a
    /// bare `.conllu` or `text:` takes.
    pub fn parse_conllu_text(text: &str) -> Result<Self, String> {
        Self::from_conllu_set(&[(None, text)], false)
    }

    /// A treebank read as a corpus: [`parse_conllu_text`]'s
    /// document with the file's own tokens as each sentence's
    /// children, in id order, every column carried — the reading
    /// `corpus:` takes on a `.conllu`.
    ///
    /// [`parse_conllu_text`]: Self::parse_conllu_text
    pub fn parse_conllu_corpus(text: &str) -> Result<Self, String> {
        Self::from_conllu_set(&[(None, text)], true)
    }

    /// A set of treebank files read as one document — a UD
    /// treebank's train / dev / test files, a release directory:
    /// each file a level-1 `section` under the root whose
    /// `::lemma` is the name given, the file's own `# newdoc`
    /// sections nested beneath it. No tokens, as
    /// [`parse_conllu_text`](Self::parse_conllu_text).
    pub fn parse_conllu_text_set(files: &[(&str, &str)]) -> Result<Self, String> {
        let named: Vec<(Option<&str>, &str)> = files.iter().map(|(n, t)| (Some(*n), *t)).collect();
        Self::from_conllu_set(&named, false)
    }

    /// [`parse_conllu_text_set`](Self::parse_conllu_text_set) with
    /// every file's tokens under its sentences: token positions,
    /// sentence ordinals and mention ordinals run across the whole
    /// set, as they do across one file.
    pub fn parse_conllu_corpus_set(files: &[(&str, &str)]) -> Result<Self, String> {
        let named: Vec<(Option<&str>, &str)> = files.iter().map(|(n, t)| (Some(*n), *t)).collect();
        Self::from_conllu_set(&named, true)
    }

    fn from_conllu_set(files: &[(Option<&str>, &str)], tokens: bool) -> Result<Self, String> {
        let mut parsed: Vec<(Option<&str>, ConlluDoc)> = Vec::with_capacity(files.len());
        for (name, text) in files {
            let doc = parse_conllu(text).map_err(|e| match name {
                Some(n) => format!("{n}: {e}"),
                None => e,
            })?;
            parsed.push((*name, doc));
        }
        let mut nodes = vec![Node::new(Kind::Document, None)];
        let root = NodeId(0);
        // Every sentence node, with the file and sentence it came from.
        let mut sentences: Vec<(NodeId, usize, usize)> = Vec::new();
        for (fi, (name, doc)) in parsed.iter().enumerate() {
            let (base, doc_level) = match name {
                Some(n) => {
                    let sec = push(&mut nodes, Kind::Section, root);
                    nodes[sec.0 as usize].lemma = Some(n.to_string());
                    nodes[sec.0 as usize].level = Some(1);
                    (sec, 2)
                }
                None => (root, 1),
            };
            let mut section: Option<NodeId> = None;
            let mut para: Option<NodeId> = None;
            for (si, s) in doc.sentences.iter().enumerate() {
                if let Some(id) = &s.newdoc {
                    let sec = push(&mut nodes, Kind::Section, base);
                    nodes[sec.0 as usize].lemma = id.clone();
                    nodes[sec.0 as usize].level = Some(doc_level);
                    section = Some(sec);
                    para = None;
                }
                if s.newpar.is_some() {
                    let p = push(&mut nodes, Kind::Paragraph, section.unwrap_or(base));
                    para = Some(p);
                }
                let parent = para.or(section).unwrap_or(base);
                let sent = push(&mut nodes, Kind::Sentence, parent);
                nodes[sent.0 as usize].taxis = Some(sentences.len() as i64 + 1);
                let text = s
                    .comments
                    .iter()
                    .find(|(k, _)| k == "text")
                    .map(|(_, v)| v.clone())
                    .unwrap_or_else(|| conllu_surface(&s.tokens));
                nodes[sent.0 as usize].text = text;
                nodes[sent.0 as usize].fields = s.comments.clone();
                sentences.push((sent, fi, si));
            }
        }
        flatten_prose(&mut nodes);
        let mut model = TextModel {
            nodes,
            root,
            onyms: Default::default(),
            document_url: None,
            link_base: None,
            document_path: None,
            cite_scheme: None,
            orthography: None,
            bib_aliases: Default::default(),
            head: HeadMeta::default(),
            token_index: Default::default(),
            mentions: Vec::new(),
        };
        if tokens {
            let mut n: u32 = 0;
            for (i, (sent, fi, si)) in sentences.into_iter().enumerate() {
                let s = &parsed[fi].1.sentences[si];
                let ordinal = i as u32 + 1;
                let len = model.nodes[sent.0 as usize].prose.len() as u32;
                let mut made: Vec<(NodeId, &ConlluToken)> = Vec::new();
                // A multiword range in force: last word id, id, form.
                let mut range: Option<(u32, String, String)> = None;
                for tok in &s.tokens {
                    if let Some((_, b)) = tok.range {
                        range = Some((b, tok.id.clone(), tok.form.clone()));
                        continue;
                    }
                    let mwt = match &range {
                        Some((until, id, form)) if !tok.empty && tok.ord <= *until => {
                            Some((id.clone(), form.clone()))
                        }
                        _ => {
                            range = None;
                            None
                        }
                    };
                    if !tok.empty {
                        n += 1;
                    }
                    let node = model.push_annotated(
                        sent,
                        tok,
                        if tok.empty { 0 } else { n },
                        ordinal,
                        mwt,
                    );
                    if let Some(t) = model.nodes[node.0 as usize].token.as_mut() {
                        t.span = (0, len);
                    }
                    made.push((node, tok));
                }
                model.link_sentence(&made);
            }
            // One `# global.Entity` header serves the set: a
            // release declares it identically in every file.
            let header = parsed
                .iter()
                .flat_map(|(_, d)| d.globals.iter())
                .find(|(k, _)| k == "Entity")
                .map(|(_, v)| v.as_str());
            model.decode_mentions(header);
        }
        Ok(model)
    }

    /// One annotated token under `block`, from a CoNLL-U line.
    fn push_annotated(
        &mut self,
        block: NodeId,
        tok: &ConlluToken,
        n: u32,
        sentence: u32,
        mwt: Option<(String, String)>,
    ) -> NodeId {
        let node = push(&mut self.nodes, Kind::Token, block);
        let t = &mut self.nodes[node.0 as usize];
        t.text = tok.form.clone();
        t.prose = tok.form.clone();
        let opt = |v: &str| (v != "_" && !v.is_empty()).then(|| v.to_string());
        let mut keys = key_values(&tok.feats);
        keys.extend(key_values(&tok.misc));
        keys.extend(tok.extra.iter().cloned());
        t.token = Some(TokenInfo {
            class: class_of_annotated(&tok.form, &tok.upos),
            n,
            sentence,
            span: (0, 0),
            at: 0,
            spans: Vec::new(),
            annot: Some(Box::new(Annot {
                id: tok.id.clone(),
                lemma: opt(&tok.lemma),
                upos: opt(&tok.upos),
                xpos: opt(&tok.xpos),
                feats: opt(&tok.feats),
                deprel: opt(&tok.deprel),
                deps: opt(&tok.deps),
                misc: opt(&tok.misc),
                keys,
                mwt,
                empty: tok.empty,
                head: None,
                dependents: Vec::new(),
                eheads: Vec::new(),
                edependents: Vec::new(),
                mentions: Vec::new(),
            })),
        });
        node
    }

    /// The edges of one sentence: the basic head (`->head`,
    /// `<-head`) and the enhanced graph (`->ehead`, `<-ehead`,
    /// the relation as edge data), resolved by CoNLL-U id.
    fn link_sentence(&mut self, made: &[(NodeId, &ConlluToken)]) {
        let find = |id: &str| made.iter().find(|(_, t)| t.id == id).map(|(n, _)| *n);
        for (node, tok) in made {
            if !tok.empty
                && tok.head != "0"
                && tok.head != "_"
                && !tok.head.is_empty()
                && let Some(h) = find(&tok.head)
            {
                if let Some(a) = self.nodes[node.0 as usize]
                    .token
                    .as_mut()
                    .and_then(|t| t.annot.as_mut())
                {
                    a.head = Some(h);
                }
                if let Some(a) = self.nodes[h.0 as usize]
                    .token
                    .as_mut()
                    .and_then(|t| t.annot.as_mut())
                {
                    a.dependents.push(*node);
                }
            }
            if tok.deps == "_" || tok.deps.is_empty() {
                continue;
            }
            for entry in tok.deps.split('|') {
                let Some((h, rel)) = entry.split_once(':') else {
                    continue;
                };
                if h == "0" {
                    continue;
                }
                let Some(target) = find(h) else {
                    continue;
                };
                if let Some(a) = self.nodes[node.0 as usize]
                    .token
                    .as_mut()
                    .and_then(|t| t.annot.as_mut())
                {
                    a.eheads.push((target, rel.to_string()));
                }
                if let Some(a) = self.nodes[target.0 as usize]
                    .token
                    .as_mut()
                    .and_then(|t| t.annot.as_mut())
                {
                    a.edependents.push((*node, rel.to_string()));
                }
            }
        }
    }
    /// The mentions of the annotation (ruling #63), decoded once
    /// the tokens carry their keys: a `ner` key's BIO / BIOES tags
    /// (Stanza, spacy-conll — `B-PERSON` opens, `I-`/`E-` continue
    /// within the sentence, `S-` stands alone, `O` closes) and
    /// CorefUD's `Entity=` brackets (`(e1-person-1-` opens, `e1)`
    /// closes, `(e1-person-1)` is one token; attributes in the
    /// `# global.Entity` header's order, `eid` first; nesting and
    /// crossing allowed, so a token may fall in several). Each
    /// token records the mentions it falls in.
    fn decode_mentions(&mut self, entity_header: Option<&str>) {
        self.mentions.clear();
        let tokens: Vec<NodeId> = (0..self.nodes.len())
            .filter(|&i| {
                self.nodes[i]
                    .token
                    .as_ref()
                    .is_some_and(|t| t.annot.is_some())
            })
            .map(|i| NodeId(i as u64))
            .collect();
        let key = |nodes: &[Node], t: NodeId, name: &str| -> Option<String> {
            nodes[t.0 as usize]
                .token
                .as_ref()?
                .annot
                .as_ref()?
                .key(name)
                .map(str::to_string)
        };
        let sentence_of = |nodes: &[Node], t: NodeId| -> u32 {
            nodes[t.0 as usize].token.as_ref().map_or(0, |t| t.sentence)
        };
        // Named entities: BIO / BIOES.
        let mut open: Option<(usize, Option<String>, u32)> = None;
        for &t in &tokens {
            let Some(tag) = key(&self.nodes, t, "ner") else {
                open = None;
                continue;
            };
            let (prefix, etype) = match tag.split_once('-') {
                Some((p, e)) => (p.to_string(), Some(e.to_string())),
                None => (tag.clone(), None),
            };
            let sentence = sentence_of(&self.nodes, t);
            let continues = matches!(prefix.as_str(), "I" | "E")
                && open
                    .as_ref()
                    .is_some_and(|(_, ty, s)| *ty == etype && *s == sentence);
            match prefix.as_str() {
                "B" | "S" | "I" | "E" => {
                    if continues {
                        let (idx, _, _) = open.as_ref().unwrap();
                        self.mentions[*idx].tokens.push(t);
                    } else {
                        self.mentions.push(Mention {
                            tokens: vec![t],
                            etype: etype.clone(),
                            cluster: None,
                        });
                    }
                    let idx = if continues {
                        open.as_ref().unwrap().0
                    } else {
                        self.mentions.len() - 1
                    };
                    open = if matches!(prefix.as_str(), "B" | "I") {
                        Some((idx, etype, sentence))
                    } else {
                        None
                    };
                }
                _ => open = None,
            }
        }
        // Coreference: CorefUD brackets.
        let order: Vec<String> = entity_header
            .unwrap_or("eid-etype-head-other")
            .split('-')
            .map(str::to_string)
            .collect();
        let mut stack: Vec<(String, usize)> = Vec::new();
        for &t in &tokens {
            let Some(spec) = key(&self.nodes, t, "Entity") else {
                for (_, idx) in &stack {
                    if self.mentions[*idx].tokens.last() != Some(&t) {
                        self.mentions[*idx].tokens.push(t);
                    }
                }
                continue;
            };
            let mut closings: Vec<String> = Vec::new();
            for entry in entity_entries(&spec) {
                let entry = entry.as_str();
                let opening = entry.starts_with('(');
                let closing = entry.ends_with(')');
                let body = entry.trim_start_matches('(').trim_end_matches(')');
                if opening {
                    let mut attrs = body.splitn(order.len(), '-');
                    let eid = attrs.next().unwrap_or("").to_string();
                    let mut etype = None;
                    for (name, val) in order.iter().skip(1).zip(attrs) {
                        if name == "etype" && !val.is_empty() {
                            etype = Some(val.to_string());
                        }
                    }
                    // A discontinuous mention's parts (`e1[1/2]`)
                    // share the cluster.
                    let cluster = eid.split('[').next().unwrap_or(&eid).to_string();
                    let idx = self.mentions.len();
                    self.mentions.push(Mention {
                        tokens: vec![t],
                        etype,
                        cluster: Some(cluster),
                    });
                    if !closing {
                        stack.push((eid, idx));
                    }
                } else if closing {
                    closings.push(body.to_string());
                }
            }
            for (_, idx) in &stack {
                if self.mentions[*idx].tokens.last() != Some(&t) {
                    self.mentions[*idx].tokens.push(t);
                }
            }
            for eid in closings {
                if let Some(pos) = stack.iter().rposition(|(e, _)| *e == eid) {
                    stack.remove(pos);
                }
            }
        }
        // Each token's membership, by ordinal.
        for (i, m) in self.mentions.iter().enumerate() {
            for &t in &m.tokens {
                if let Some(a) = self.nodes[t.0 as usize]
                    .token
                    .as_mut()
                    .and_then(|t| t.annot.as_mut())
                {
                    a.mentions.push(i as u32 + 1);
                }
            }
        }
    }

    /// A mention's text: its tokens' forms, a space between them
    /// unless the annotation says `SpaceAfter=No`.
    fn mention_text(&self, m: &Mention) -> String {
        let mut out = String::new();
        for (i, &t) in m.tokens.iter().enumerate() {
            let n = &self.nodes[t.0 as usize];
            if i > 0 {
                let prev = &self.nodes[m.tokens[i - 1].0 as usize];
                let glued = prev
                    .token
                    .as_ref()
                    .and_then(|t| t.annot.as_ref())
                    .and_then(|a| a.key("SpaceAfter"))
                    == Some("No");
                if !glued {
                    out.push(' ');
                }
            }
            out.push_str(&n.prose);
        }
        out
    }

    /// The nearest literary-reading field over a token: its spans
    /// innermost first, then the block the token belongs to.
    fn token_field(&self, node: NodeId, name: &str) -> Option<Value> {
        let t = self.nodes[node.0 as usize].token.as_ref()?;
        // A cast table's own annotation on the token comes first.
        if let Some(v) = field_values(&self.nodes[node.0 as usize].fields, name) {
            return Some(v);
        }
        for &s in t.spans.iter().rev() {
            if let Some(v) = field_values(&self.nodes[s.0 as usize].fields, name) {
                return Some(v);
            }
        }
        let mut at = self.nodes[node.0 as usize].parent;
        while let Some(p) = at {
            let n = &self.nodes[p.0 as usize];
            if !(n.kind == Kind::Sentence && n.derived) {
                return field_values(&n.fields, name);
            }
            at = n.parent;
        }
        None
    }

    /// One value or a list, for a token in one or several mentions.
    fn mention_values(&self, node: NodeId, f: impl Fn(&Mention) -> Option<Value>) -> Option<Value> {
        let a = self.nodes[node.0 as usize].token.as_ref()?.annot.as_ref()?;
        let vs: Vec<Value> = a
            .mentions
            .iter()
            .filter_map(|&i| f(&self.mentions[i as usize - 1]))
            .collect();
        match vs.len() {
            0 => None,
            1 => vs.into_iter().next(),
            _ => Some(Value::list(vs)),
        }
    }
}

/// The innermost span under `flow` whose byte range contains
/// `lo..hi` — the parent a nested span (a name inside a speech)
/// lands under. Spans arrive in pre-order, so the deepest match
/// is the right one.
fn enclosing_span(nodes: &[Node], flow: NodeId, lo: u32, hi: u32) -> Option<NodeId> {
    let mut best: Option<NodeId> = None;
    let mut at = flow;
    loop {
        let next = nodes[at.0 as usize].children.iter().rev().find_map(|&c| {
            let n = &nodes[c.0 as usize];
            match (n.kind, n.at) {
                (Kind::Span, Some((slo, shi)))
                    if slo <= lo && hi <= shi && (slo, shi) != (lo, hi) =>
                {
                    Some(c)
                }
                _ => None,
            }
        });
        match next {
            Some(c) => {
                best = Some(c);
                at = c;
            }
            None => return best,
        }
    }
}

/// Every value under `name` among a node's fields: one value, a
/// list where the key repeats, nothing where it is absent.
fn field_values(fields: &[(String, String)], name: &str) -> Option<Value> {
    let vs: Vec<Value> = fields
        .iter()
        .filter(|(k, _)| k == name)
        .map(|(_, v)| Value::Str(v.clone()))
        .collect();
    match vs.len() {
        0 => None,
        1 => vs.into_iter().next(),
        _ => Some(Value::list(vs)),
    }
}

fn push(nodes: &mut Vec<Node>, kind: Kind, parent: NodeId) -> NodeId {
    let id = NodeId(nodes.len() as u64);
    nodes.push(Node::new(kind, Some(parent)));
    nodes[parent.0 as usize].children.push(id);
    id
}

/// Denormalize a table into nested lists (see the module doc).
/// The column name lands as the cell's `::lemma` — a cell's own
/// label (a row's `th`) wins over the positional header entry —
/// and never as folded text: addressing is property projection,
/// the flattening rule alone spells `lemma: value`.
fn lower_table(
    nodes: &mut Vec<Node>,
    parent: NodeId,
    lemma: Option<String>,
    headers: Option<Vec<String>>,
    rows: Vec<Vec<Cell>>,
) {
    let list = push(nodes, Kind::OrderedList, parent);
    {
        let n = &mut nodes[list.0 as usize];
        n.table = true;
        n.lemma = lemma.map(|l| normalize_ws(&l)).filter(|l| !l.is_empty());
    }
    for (i, row) in rows.into_iter().enumerate() {
        let item = push(nodes, Kind::OrderedItem, list);
        nodes[item.0 as usize].taxis = Some(i as i64 + 1);
        nodes[item.0 as usize].row = true;
        let cells = push(nodes, Kind::UnorderedList, item);
        for (j, cell) in row.into_iter().enumerate() {
            let value = normalize_ws(&cell.text);
            if value.is_empty() {
                continue;
            }
            let label = cell
                .label
                .as_deref()
                .or_else(|| headers.as_ref().and_then(|h| h.get(j)).map(|h| h.as_str()))
                .map(normalize_ws)
                .filter(|h| !h.is_empty());
            let cell_item = push(nodes, Kind::UnorderedItem, cells);
            let n = &mut nodes[cell_item.0 as usize];
            n.cell = true;
            n.lemma = label;
            n.text = value;
        }
    }
}

/// Lower a verse block (ruling #37): `verse` → `strophe`s →
/// `stichos` lines. The stichos taxis numbers lines 1-based,
/// continuously across strophes — Iliad 1.34 is a taxis; the
/// strophe taxis is its ordinal. Empty lines are dropped (the
/// strophe boundary carries the separation).
fn lower_verse(
    nodes: &mut Vec<Node>,
    parent: NodeId,
    lemma: Option<String>,
    strophes: Vec<Vec<String>>,
    hypograph: Option<String>,
) -> NodeId {
    let verse = push(nodes, Kind::Verse, parent);
    {
        let n = &mut nodes[verse.0 as usize];
        n.lemma = lemma.map(|l| normalize_ws(&l)).filter(|l| !l.is_empty());
        n.hypograph = hypograph
            .map(|h| normalize_ws(&h))
            .filter(|h| !h.is_empty());
    }
    let mut line_no = 0i64;
    for (i, strophe) in strophes.into_iter().enumerate() {
        let sid = push(nodes, Kind::Strophe, verse);
        nodes[sid.0 as usize].taxis = Some(i as i64 + 1);
        for line in strophe {
            let line = line.trim_end().to_string();
            if line.trim().is_empty() {
                continue;
            }
            line_no += 1;
            let lid = push(nodes, Kind::Stichos, sid);
            let n = &mut nodes[lid.0 as usize];
            n.taxis = Some(line_no);
            n.text = line;
        }
    }
    verse
}

/// Compute every node's flattened prose: lemma first, then the
/// node's own text, then its children's prose in order, then the
/// hypograph, block-joined with newlines. On a list *item*, the
/// lemma joins the rest with `: ` instead — an item's lemma names
/// its content inline (a table cell reads `Outcome: Emus won`, a
/// definition reads `term: description`), where a section's lemma
/// opens its block. Children always carry larger indices than
/// their parents (nodes are interned in document order), so one
/// reverse index scan suffices — no recursion.
fn flatten_prose(nodes: &mut [Node]) {
    for i in (0..nodes.len()).rev() {
        let inline_lemma = matches!(nodes[i].kind, Kind::UnorderedItem | Kind::OrderedItem);
        let mut lemma_part: Option<String> = None;
        let mut parts: Vec<String> = Vec::new();
        if let Some(lemma) = &nodes[i].lemma
            && !lemma.is_empty()
        {
            if inline_lemma {
                lemma_part = Some(lemma.clone());
            } else if matches!(nodes[i].kind, Kind::Paragraph | Kind::Dialogue) {
                // A speech's lemma is its speaker — who said it,
                // not what was said: `::lemma`, never prose, so
                // the words alone are read, tokenized, counted.
            } else {
                parts.push(lemma.clone());
            }
        }
        if !nodes[i].text.is_empty() {
            parts.push(nodes[i].text.clone());
        }
        let mut child_parts: Vec<String> = Vec::new();
        for &child in nodes[i].children.clone().iter() {
            // A ref's recorded link text is already part of the
            // flow it was met in; an anchor is invisible. Neither
            // adds prose (the deixis/index-mark rule).
            if matches!(
                nodes[child.0 as usize].kind,
                Kind::Ref | Kind::Cit | Kind::Anchor | Kind::Span | Kind::Milestone
            ) {
                continue;
            }
            let prose = &nodes[child.0 as usize].prose;
            if !prose.is_empty() {
                child_parts.push(prose.clone());
            }
        }
        // Ruling #37: a verse block separates its strophes by a
        // blank line (lemma and hypograph still join normally) —
        // byte-compatible with the old verbatim lowering.
        if nodes[i].kind == Kind::Verse {
            let joined = child_parts.join("\n\n");
            if !joined.is_empty() {
                parts.push(joined);
            }
        } else if nodes[i].kind == Kind::Paragraph
            && !child_parts.is_empty()
            && nodes[i]
                .children
                .iter()
                .all(|c| nodes[c.0 as usize].kind == Kind::Sentence)
        {
            // Ruling #63: a treebank paragraph is its sentences
            // run together, as the prose would read.
            parts.push(child_parts.join(" "));
        } else {
            parts.extend(child_parts);
        }
        if let Some(hypograph) = &nodes[i].hypograph
            && !hypograph.is_empty()
        {
            parts.push(hypograph.clone());
        }
        let mut prose = parts.join("\n");
        if let Some(lemma) = lemma_part {
            prose = if prose.is_empty() {
                lemma
            } else {
                format!("{lemma}: {prose}")
            };
        }
        nodes[i].prose = prose;
    }
}

impl TextModel {
    /// The body prose: everything between the lemma and the
    /// hypograph — the simmere anatomy's third member, derived
    /// from the flattened prose by construction (the lemma joins
    /// a block on its own line, an item with `: `; the hypograph
    /// closes on its own line).
    fn grammata(&self, node: NodeId) -> String {
        let n = &self.nodes[node.0 as usize];
        let mut s = n.prose.as_str();
        if let Some(lemma) = &n.lemma
            && !lemma.is_empty()
            && let Some(rest) = s.strip_prefix(lemma.as_str())
        {
            s = rest
                .strip_prefix(": ")
                .or_else(|| rest.strip_prefix('\n'))
                .unwrap_or(rest);
        }
        if let Some(h) = &n.hypograph
            && !h.is_empty()
            && let Some(rest) = s.strip_suffix(h.as_str())
        {
            s = rest.strip_suffix('\n').unwrap_or(rest);
        }
        s.trim_end().to_string()
    }
}

impl AstAdapter for TextModel {
    fn root(&self) -> NodeId {
        self.root
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.nodes[node.0 as usize].children.clone()
    }

    fn name(&self, node: NodeId) -> Option<String> {
        let n = &self.nodes[node.0 as usize];
        if n.kind == Kind::Span {
            return n.span_kind.clone();
        }
        n.kind.name().map(str::to_string)
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes[node.0 as usize].parent
    }

    /// The positional index (ruling #62, phase 2): `//token[::lower
    /// = "word"]` below any node answers from the form → tokens
    /// map, scoped to the node's subtree. Only a plain word is
    /// answered — a literal with a numeric, temporal, durational or
    /// unital reading is left to the walk, whose equality reads it.
    fn descendants_where(
        &self,
        node: NodeId,
        name: &str,
        property: &str,
        value: &Value,
    ) -> Option<Vec<(NodeId, usize)>> {
        if name != "token" || property != "lower" {
            return None;
        }
        let Value::Str(word) = value else {
            return None;
        };
        if word.is_empty() || !word.chars().all(|c| c.is_alphabetic() || "’'-".contains(c)) {
            return None;
        }
        let index = self.token_index.get_or_init(|| {
            let mut map: std::collections::HashMap<String, Vec<NodeId>> =
                std::collections::HashMap::new();
            for (i, n) in self.nodes.iter().enumerate() {
                if n.token.is_some() {
                    map.entry(self.lower(&n.prose))
                        .or_default()
                        .push(NodeId(i as u64));
                }
            }
            map
        });
        let Some(hits) = index.get(word.as_str()) else {
            return Some(Vec::new());
        };
        let mut out = Vec::new();
        for &t in hits {
            // The depth below `node`, if `t` is in its subtree.
            let mut depth = 0usize;
            let mut cur = t;
            while let Some(p) = self.nodes[cur.0 as usize].parent {
                depth += 1;
                if p == node {
                    out.push((t, depth));
                    break;
                }
                cur = p;
            }
        }
        Some(out)
    }

    /// The `<block>` family on every block node, plus `<table>` on
    /// a list that denormalizes a table. Kinds are node names, not
    /// traits.
    fn traits(&self, node: NodeId) -> Vec<String> {
        let n = &self.nodes[node.0 as usize];
        let mut out = Vec::new();
        // The document wears its declared classification: one
        // trait per tag and one for the category — the LDAP rule,
        // where a multi-valued classifying attribute is the node's
        // traits — each in its own namespace (`<tag:pandas>`,
        // `<category:Guides>`), so a tag can never collide with a
        // structural trait like `<table>` or `<note>`, and spelled
        // as declared, case kept.
        if n.kind == Kind::Document {
            for (ns, t) in self
                .head
                .tags
                .iter()
                .map(|t| ("tag", t))
                .chain(self.head.category.iter().map(|c| ("category", c)))
            {
                let name = HeadMeta::trait_name(t);
                if name.is_empty() {
                    continue;
                }
                let name = format!("{ns}:{name}");
                if !out.contains(&name) {
                    out.push(name);
                }
            }
        }
        // A callout is inline apparatus, not a block (ruling #35);
        // so is an index mark (ruling #36). Strophes and stichos
        // lines are sub-block structure (ruling #37, litogramma's
        // own family rule) — the verse block carries the trait.
        if let Some(t) = &n.token {
            // An empty node (`8.1`) is in the enhanced graph, not
            // in the text: `<empty>` in place of a class.
            if t.annot.as_ref().is_some_and(|a| a.empty) {
                out.push("empty".to_string());
            } else {
                out.push(t.class.name().to_string());
                // A quotation mark wears `<quote>` beside `<punct>`
                // (ruling #65), so the punctuation counts stand.
                if t.class == TokenClass::Punct && is_quote(&n.prose) {
                    out.push("quote".to_string());
                }
            }
        }
        if n.kind != Kind::Document
            && !n.deixis
            && !n.derived
            && !matches!(
                n.kind,
                Kind::IndexMark
                    | Kind::Ref
                    | Kind::Cit
                    | Kind::Anchor
                    | Kind::Strophe
                    | Kind::Stichos
                    | Kind::Token
                    | Kind::Span
                    | Kind::Milestone
                    | Kind::Character
                    | Kind::Place
            )
        {
            out.push("block".to_string());
        }
        // The literary reading: a simmere's genoses are its
        // traits (`//quotation<said>`, `//annotation<persname>`).
        for g in &n.genoses {
            if !out.contains(g) {
                out.push(g.clone());
            }
        }
        // The corpus reading's sentence tier (ruling #64): a
        // sentence the segmentation derived, as against a
        // treebank's declared one.
        if n.derived {
            out.push("derived".to_string());
        }
        // The reference vocabulary (atrep's semantic traits): a
        // dangling mention keeps its node and the linter finds it;
        // `<target>` marks what bears a referable name — anchors
        // and labeled blocks, never the apparatus families (their
        // onyms pair within the family, not in this namespace).
        if matches!(n.kind, Kind::Ref | Kind::Cit) && n.dangling {
            out.push("dangling".to_string());
        }
        // A structured entry's genus (liber, commentarius, …)
        // surfaces as its trait, the genos rule.
        if n.kind == Kind::Bib
            && let Some(g) = &n.genus
        {
            out.push(g.clone());
        }
        if n.kind == Kind::Anchor
            || (n.onym.is_some()
                && !n.deixis
                && !matches!(
                    n.kind,
                    Kind::Footnote
                        | Kind::Endnote
                        | Kind::Aside
                        | Kind::IndexMark
                        | Kind::Ref
                        | Kind::Cit
                        | Kind::Bib
                        | Kind::Span
                        | Kind::Milestone
                ))
        {
            out.push("target".to_string());
        }
        if matches!(n.kind, Kind::Footnote | Kind::Endnote | Kind::Aside) {
            // The body IS the note — `<note>` marks it, whichever
            // note family; the callout carries `<deixis>` alone.
            // An aside body is content, not apparatus: no
            // `<note>`. No both-ends trait: `//footnote` already
            // gathers a family whole, `//*<deixis>` the callouts
            // across families, `//*<note>` the note bodies.
            if n.deixis {
                out.push("deixis".to_string());
            } else if n.kind != Kind::Aside {
                out.push("note".to_string());
            }
            if n.dangling {
                out.push("dangling".to_string());
            }
        }
        if n.table {
            out.push("table".to_string());
        }
        if n.row {
            out.push("row".to_string());
        }
        if n.cell {
            out.push("cell".to_string());
        }
        out
    }

    /// `::lemma` (title), `::hypograph` (footer or attribution),
    /// `::taxis` (ordinal), `::text` (the flattened prose, same as
    /// the bare projection).
    /// The Greek anatomy — `::lemma`, `::grammata`, `::hypograph`,
    /// `::taxis` — plus the friendly aliases (`::title`, `::body`,
    /// `::attribution`, `::ord`), answered here because this
    /// adapter's property surface IS the vocabulary; on data
    /// adapters those spellings stay ordinary field names. The
    /// Greek is canon in docs and reflection preserves whichever
    /// spelling was written.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        // The citation in force at any node (ruling #81).
        if name == "cite" {
            return self.cite(node).map(Value::Str);
        }
        // The document node answers its declared identity: the
        // curated core of what the head says.
        if n.kind == Kind::Document {
            let h = &self.head;
            match name {
                "title" | "lemma" => return h.title.clone().map(Value::Str),
                "description" => return h.description.clone().map(Value::Str),
                "author" => return h.author.clone().map(Value::Str),
                "category" => return h.category.clone().map(Value::Str),
                "tags" => {
                    return (!h.tags.is_empty())
                        .then(|| Value::list(h.tags.iter().cloned().map(Value::Str).collect()));
                }
                "published" => return h.published.clone(),
                "modified" => return h.modified.clone(),
                _ => {}
            }
        }
        // A structured entry answers its campi first — under the
        // canonical Latin name or any census alias (BibLaTeX's
        // field names included), case-insensitively — ahead of
        // the general vocabulary, so `::title` on a bib means
        // titulus, not the lemma alias.
        if n.kind == Kind::Bib && !n.fields.is_empty() {
            let canon = self
                .bib_aliases
                .get(&name.to_lowercase())
                .map(String::as_str)
                .unwrap_or(name);
            if let Some((_, v)) = n.fields.iter().find(|(k, _)| k == canon) {
                return Some(Value::Str(v.clone()));
            }
        }
        match name {
            // A link into the node, as far as the served form can
            // narrow it; the anchor it lands on; and the record a
            // client renders as a clickable result.
            "href" => return self.href(node).map(Value::Str),
            "anchor" => return self.nearest_onym(node).map(|(a, _)| Value::Str(a)),
            "link" => {
                let href = self.href(node)?;
                let text = match self.default_value(node) {
                    Some(Value::Str(t)) => t,
                    _ => String::new(),
                };
                return Some(Value::Record(vec![
                    (
                        "title".to_string(),
                        self.document_title().map(Value::Str).unwrap_or(Value::Null),
                    ),
                    ("href".to_string(), Value::Str(href)),
                    ("text".to_string(), Value::Str(text)),
                ]));
            }
            _ => {}
        }
        match name {
            // The corpus reading's token properties; the linguistic
            // ones answer only under a CoNLL-U annotation.
            "lemma" | "upos" | "xpos" | "feats" | "deprel" | "deps" | "misc" | "id" | "mwt"
                if n.token.as_ref().is_some_and(|t| t.annot.is_some()) =>
            {
                let a = n.token.as_ref()?.annot.as_ref()?;
                match name {
                    "lemma" => a.lemma.clone(),
                    "upos" => a.upos.clone(),
                    "xpos" => a.xpos.clone(),
                    "feats" => a.feats.clone(),
                    "deprel" => a.deprel.clone(),
                    "deps" => a.deps.clone(),
                    "misc" => a.misc.clone(),
                    "id" => Some(a.id.clone()),
                    _ => a.mwt.as_ref().map(|(_, form)| form.clone()),
                }
                .map(Value::Str)
            }
            // A treebank sentence: `::id` its sent_id, every other
            // comment under its key (ruling #63).
            "id" if n.kind == Kind::Sentence => n
                .fields
                .iter()
                .find(|(k, _)| k == "sent_id")
                .map(|(_, v)| Value::Str(v.clone())),
            "lemma" | "title" => n.lemma.clone().map(Value::Str),
            "onym" => n.onym.clone().map(Value::Str),
            // On an index mark the onym IS the term (ruling #36).
            "term" if n.kind == Kind::IndexMark => n.onym.clone().map(Value::Str),
            // A ref's word: the identifier as written. Never the
            // resolved content — that is what `-->` is for.
            "target" if matches!(n.kind, Kind::Ref | Kind::Cit) => n.target.clone().map(Value::Str),
            // A structured entry answers its campi (auctor,
            // titulus, annus, …) — the bibliogramma vocabulary,
            // canonicalized upstream.
            _ if n.kind == Kind::Bib => {
                let canon = self
                    .bib_aliases
                    .get(&name.to_lowercase())
                    .map(String::as_str)
                    .unwrap_or(name);
                n.fields
                    .iter()
                    .find(|(k, _)| k == canon)
                    .map(|(_, v)| Value::Str(v.clone()))
            }
            "hypograph" | "attribution" => n.hypograph.clone().map(Value::Str),
            "taxis" | "ord" => n.taxis.map(Value::Int),
            "grammata" | "body" => {
                let g = self.grammata(node);
                if g.is_empty() {
                    None
                } else {
                    Some(Value::Str(g))
                }
            }
            "text" => Some(Value::Str(n.prose.clone())),
            "lower" if n.token.is_some() => Some(Value::Str(self.lower(&n.prose))),
            "modern" if n.token.is_some() => Some(Value::Str(self.modern(&n.prose))),
            "class" if n.token.is_some() => n
                .token
                .as_ref()
                .map(|t| Value::Str(t.class.name().to_string())),
            // The text of the token's sentence (its ordinal is
            // `::::sentence`): the block's prose over the span.
            "sentence" if n.token.is_some() => {
                let parent = &self.nodes[n.parent?.0 as usize];
                if parent.kind == Kind::Sentence {
                    return Some(Value::Str(parent.prose.clone()));
                }
                let t = n.token.as_ref()?;
                let (lo, hi) = (t.span.0 as usize, t.span.1 as usize);
                parent
                    .prose
                    .get(lo..hi)
                    .map(|s| Value::Str(s.trim().to_string()))
            }
            // A mention's text and entity type (ruling #63); a list
            // where mentions nest.
            "mention" if n.token.is_some() => {
                self.mention_values(node, |m| Some(Value::Str(self.mention_text(m))))
            }
            "entity" if n.token.is_some() => {
                self.mention_values(node, |m| m.etype.clone().map(Value::Str))
            }
            // Every `Key=Value` of FEATS, MISC and a CoNLL-U Plus
            // column, as the annotation spells it or case-folded.
            // A cast table's own annotation on the token (its
            // `prosopon`) answers when the annotation has no such
            // key.
            _ if n.token.as_ref().is_some_and(|t| t.annot.is_some()) => n
                .token
                .as_ref()?
                .annot
                .as_ref()?
                .key(name)
                .map(|v| Value::Str(v.to_string()))
                .or_else(|| self.token_field(node, name)),
            // A treebank sentence's `# key = value` comments, the
            // same two spellings.
            _ if n.kind == Kind::Sentence => n
                .fields
                .iter()
                .find(|(k, _)| k == name)
                .or_else(|| n.fields.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)))
                .map(|(_, v)| Value::Str(v.clone())),
            // The literary reading: a span's, a speech's, a
            // paragraph's unseen metadata under the sim name of the
            // monosim that carried it (`::prosopon`), a list where
            // it repeats (`eidos`); a milestone's scheme and value.
            _ if matches!(
                n.kind,
                Kind::Span
                    | Kind::Speech
                    | Kind::Dialogue
                    | Kind::Paragraph
                    | Kind::Milestone
                    | Kind::Character
                    | Kind::Place
            ) && !n.fields.is_empty() =>
            {
                field_values(&n.fields, name)
            }
            // A token answers the metadata of the innermost span
            // covering it, else of its block: `//token[::prosopon
            // = "tom"]` is every word Tom speaks.
            _ if n.token.is_some() => self.token_field(node, name),
            _ => None,
        }
    }

    /// The default projection is the flattened prose of the
    /// subtree — lemma first, hypograph last.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        if n.kind == Kind::IndexMark {
            // Invisible in the surrounding prose; its own
            // projection is the term it declares.
            return Some(Value::Str(n.onym.clone().unwrap_or_default()));
        }
        if n.deixis {
            // The atrep rule: a resolved reference projects as its
            // target's rendered form, degrading to the raw onym
            // when dangling.
            return Some(Value::Str(match n.note_edge {
                Some(body) => self.nodes[body.0 as usize].prose.clone(),
                None => n.onym.clone().unwrap_or_default(),
            }));
        }
        if n.kind == Kind::Ref {
            // The authored link text; LaTeX \ref has none, so the
            // atrep projection rule applies — the target's
            // rendered form (its lemma, else its prose), degrading
            // to the raw target when dangling.
            if !n.text.is_empty() {
                return Some(Value::Str(n.text.clone()));
            }
            return Some(Value::Str(match n.ref_edge {
                Some(t) => {
                    let b = &self.nodes[t.0 as usize];
                    b.lemma.clone().unwrap_or_else(|| b.prose.clone())
                }
                None => n.target.clone().unwrap_or_default(),
            }));
        }
        if n.kind == Kind::Bib && n.prose.is_empty() && !n.fields.is_empty() {
            // A structured entry's plain form: its field values in
            // source order — the full data, no citation styling.
            return Some(Value::Str(
                n.fields
                    .iter()
                    .map(|(_, v)| v.as_str())
                    .collect::<Vec<_>>()
                    .join(". "),
            ));
        }
        if n.kind == Kind::Cit {
            // The mark's own identity: the raw key. The entry is
            // one arrow away (`//cit--> ::`); rendered citation
            // styles are presentation, not structure.
            return Some(Value::Str(n.target.clone().unwrap_or_default()));
        }
        if n.kind == Kind::Anchor {
            // Invisible in the prose; its own projection is the
            // name it bears.
            return Some(Value::Str(n.onym.clone().unwrap_or_default()));
        }
        Some(Value::Str(n.prose.clone()))
    }

    /// The edge label is the family name both ends share —
    /// `->footnote` / `->endnote` — the node's own kind, since
    /// resolution re-kinds an open-family callout to its body.
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let n = &self.nodes[node.0 as usize];
        // The literary reading: a mention reaches its character.
        let character: Vec<(String, NodeId)> = n
            .character
            .map(|c| ("character".to_string(), c))
            .into_iter()
            .chain(n.place.map(|p| ("place".to_string(), p)))
            .collect();
        // A token's dependency head (`->head`) and its enhanced
        // heads (`->ehead`, the relation at `$-::rel`), under a
        // CoNLL-U annotation.
        if let Some(a) = n.token.as_ref().and_then(|t| t.annot.as_ref()) {
            let mut out = Vec::new();
            if let Some(h) = a.head {
                out.push(("head".to_string(), h));
            }
            for (t, _) in &a.eheads {
                out.push(("ehead".to_string(), *t));
            }
            out.extend(character);
            return out;
        }
        if !character.is_empty() && n.kind != Kind::Span {
            return character;
        }
        if let Some(t) = n.ref_edge {
            // The atrep rule: a resolved mention emits its typed
            // crosslink — `->ref`, or `->cit` for a citation.
            let label = if n.kind == Kind::Cit { "cit" } else { "ref" };
            return vec![(label.to_string(), t)];
        }
        // The literary reading: a span reaches the tokens it
        // covers (`//quotation<said>->token<word>`).
        if n.kind == Kind::Span && !(n.span_tokens.is_empty() && character.is_empty()) {
            return n
                .span_tokens
                .iter()
                .map(|&t| ("token".to_string(), t))
                .chain(character)
                .collect();
        }
        match n.note_edge {
            Some(body) => vec![(n.kind.name().unwrap_or("footnote").to_string(), body)],
            None => Vec::new(),
        }
    }

    /// `$-::rel` on a dependency edge: the relation — `->head`'s
    /// is the dependent's `::deprel`, `->ehead`'s the enhanced
    /// relation to that head.
    fn link_property(
        &self,
        source: NodeId,
        label: &str,
        target: NodeId,
        name: &str,
    ) -> Option<Value> {
        if name != "rel" {
            return None;
        }
        let a = self.nodes[source.0 as usize]
            .token
            .as_ref()?
            .annot
            .as_ref()?;
        match label {
            "head" if a.head == Some(target) => a.deprel.clone().map(Value::Str),
            "ehead" => a
                .eheads
                .iter()
                .find(|(t, _)| *t == target)
                .map(|(_, r)| Value::Str(r.clone())),
            _ => None,
        }
    }

    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let n = &self.nodes[node.0 as usize];
        if let Some(a) = n.token.as_ref().and_then(|t| t.annot.as_ref()) {
            return a
                .dependents
                .iter()
                .map(|&d| ("head".to_string(), d))
                .chain(a.edependents.iter().map(|(d, _)| ("ehead".to_string(), *d)))
                .collect();
        }
        let mut out: Vec<(String, NodeId)> = n
            .ref_cites
            .iter()
            .map(|&c| {
                let label = if self.nodes[c.0 as usize].kind == Kind::Cit {
                    "cit"
                } else {
                    "ref"
                };
                (label.to_string(), c)
            })
            .collect();
        let label = n.kind.name().unwrap_or("footnote");
        out.extend(n.cites.iter().map(|&c| (label.to_string(), c)));
        let mention_label = if n.kind == Kind::Place {
            "place"
        } else {
            "character"
        };
        out.extend(n.mentions.iter().map(|&m| (mention_label.to_string(), m)));
        out
    }

    /// `//ref-->`: land on the bearer — the block if a block
    /// bears the name, the point anchor if a point does. The
    /// text-level hints choose the reading: `-->block` normalizes
    /// a point landing to its enclosing block, `-->point` selects
    /// point targets only; bare is the honest exact landing.
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let n = &self.nodes[node.0 as usize];
        if !matches!(n.kind, Kind::Ref | Kind::Cit) || property != "target" {
            return None;
        }
        let landed = n.ref_edge?;
        let is_point = self.nodes[landed.0 as usize].kind == Kind::Anchor;
        match hint {
            None | Some("*") => Some(landed),
            Some("point") => is_point.then_some(landed),
            Some("block") => {
                if !is_point {
                    return Some(landed);
                }
                // The anchor's enclosing block.
                let mut at = self.nodes[landed.0 as usize].parent;
                while let Some(b) = at {
                    let bn = &self.nodes[b.0 as usize];
                    if bn.kind != Kind::Document
                        && !matches!(bn.kind, Kind::Ref | Kind::Anchor | Kind::IndexMark)
                    {
                        return Some(b);
                    }
                    at = bn.parent;
                }
                None
            }
            Some(_) => None,
        }
    }

    /// The bare arrow's property: a ref resolves its target.
    fn ref_property(&self, node: NodeId) -> Option<String> {
        matches!(self.nodes[node.0 as usize].kind, Kind::Ref | Kind::Cit)
            .then(|| "target".to_string())
    }

    /// A ref the producer declared external: the absolute URL,
    /// relative targets joined against the document's own URL —
    /// the same acquisition rung the html DOM reading speaks.
    fn external_ref(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<String> {
        if !matches!(hint, None | Some("*")) {
            return None;
        }
        let n = &self.nodes[node.0 as usize];
        if n.kind != Kind::Ref || property != "target" || n.internal {
            return None;
        }
        let t = n.target.as_deref()?.trim();
        if t.is_empty() || t.starts_with('#') {
            return None;
        }
        let u = match url::Url::parse(t) {
            Ok(u) => u,
            Err(url::ParseError::RelativeUrlWithoutBase) => self
                .link_base
                .as_ref()
                .or(self.document_url.as_ref())?
                .join(t)
                .ok()?,
            Err(_) => return None,
        };
        if u.scheme() != "http" && u.scheme() != "https" {
            return None;
        }
        Some(u.into())
    }

    /// A sibling document's `#fragment` lands here: the bearer of
    /// that name — a labeled block, or a point anchor.
    fn resolve_fragment(&self, _node: NodeId, fragment: &str) -> Option<NodeId> {
        self.onyms.get(fragment).copied()
    }

    /// Ruling #29: the text level's surface is the vocabulary
    /// itself — no document can introduce a property name — so
    /// its two annotations answer at `::` as well.
    fn aliased_metadata(&self, _node: NodeId) -> &'static [&'static str] {
        &["level", "lang", "form", "n"]
    }

    /// `::::level` on sections (the source heading level) and
    /// `::::lang` on verbatim blocks (the declared language).
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        // The lossless layer: on the document node, every
        // declaration under the name it was declared with.
        if n.kind == Kind::Document
            && let Some(v) = self.head.declared_value(key)
        {
            return Some(v);
        }
        match key {
            "level" => n.level.map(|l| Value::Int(l as i64)),
            // A verbatim block's declared language; on the
            // literary reading, the genos after `foreign`
            // (`@/…/@.foreign.la`).
            "lang" => n
                .lang
                .clone()
                .or_else(|| {
                    let i = n.genoses.iter().position(|g| g == "foreign")?;
                    n.genoses.get(i + 1).cloned()
                })
                .map(Value::Str),
            "at" if n.at.is_some() => n.at.map(|(lo, _)| Value::Int(lo as i64)),
            // The corpus reading: a token's position and sentence.
            // A sentence node's document-wide ordinal, under both
            // spellings, so `/paragraph[7]/sentence[3]::::n` and a
            // token's `::::sentence` agree (ruling #64).
            "n" | "sentence" if n.kind == Kind::Sentence => n.taxis.map(Value::Int),
            "n" => n
                .token
                .as_ref()
                .filter(|t| t.n > 0)
                .map(|t| Value::Int(t.n as i64)),
            "sentence" => n.token.as_ref().map(|t| Value::Int(t.sentence as i64)),
            // A mention's ordinal and coreference cluster (ruling
            // #63); a list where mentions nest.
            "mention" if n.token.is_some() => {
                let ords: Vec<Value> = n
                    .token
                    .as_ref()
                    .and_then(|t| t.annot.as_ref())
                    .map(|a| a.mentions.iter().map(|&i| Value::Int(i as i64)).collect())
                    .unwrap_or_default();
                match ords.len() {
                    0 => None,
                    1 => ords.into_iter().next(),
                    _ => Some(Value::list(ords)),
                }
            }
            "entity" if n.token.is_some() => {
                self.mention_values(node, |m| m.cluster.clone().map(Value::Str))
            }
            // The multiword range a word belongs to (`1-2`); its
            // surface form is `::mwt`.
            "mwt" => n
                .token
                .as_ref()
                .and_then(|t| t.annot.as_ref())
                .and_then(|a| a.mwt.as_ref())
                .map(|(id, _)| Value::Str(id.clone())),
            // `::::resolved` on a callout: the broken-apparatus
            // linter's fact (ruling #35).
            "resolved" if n.deixis => Some(Value::Bool(!n.dangling)),
            // …and on an internal ref: the broken-cross-reference
            // linter's fact. External refs resolve at query time,
            // so the fact is not theirs to answer.
            "resolved" if n.kind == Kind::Cit || (n.kind == Kind::Ref && n.internal) => {
                Some(Value::Bool(n.ref_edge.is_some()))
            }
            // The declared spelling was a margin form (a Tufte
            // sidenote): family footnote, placement preserved.
            "form" if n.margin => Some(Value::Str("margin".to_string())),
            // The entry's genus, also queryable as an annotation.
            "genus" if n.kind == Kind::Bib => n.genus.clone().map(Value::Str),
            _ => None,
        }
    }
}
