//! The LaTeX text-level adapter: `text:paper.tex` — the
//! structural subset a `.tex` source declares, read from its
//! own commands (the ruling-#35 amendment in lang/TODO.md).
//!
//! - **Sections** from the sectioning commands' own fixed
//!   hierarchy: `\part` (1), `\chapter` (2), `\section` (3),
//!   `\subsection` (4), `\subsubsection` (5) — starred forms
//!   included; nesting is relative, so an article without
//!   chapters nests exactly as written.
//! - **Footnotes are the third convention** (the body declared AT
//!   the callout, vs litogramma's document end and docx's
//!   separate part): `\footnote{...}` yields a callout in its
//!   paragraph and the body at the document end (the model's
//!   canonical placement); onyms are the print order ("1", "2",
//!   …) since LaTeX declares none. `\footnotemark[n]` /
//!   `\footnotetext[n]{...}` pair by their explicit number; the
//!   bare forms pair sequentially. The endnotes package's
//!   `\endnote` / `\endnotemark` / `\endnotetext` read
//!   identically into the endnote family, on their own counter;
//!   `\theendnotes` vanishes — the model already holds the notes.
//!   Tufte-style `\sidenote` / `\sidenotemark` /
//!   `\sidenotetext` are footnotes whose renderer puts them in
//!   the margin — placement is presentation, so they join the
//!   footnote family (sharing its counters and namespace) and
//!   carry `::::form = "margin"` on both ends.
//! - **Margin content** — `\marginpar` / `\marginnote`, the
//!   unnumbered anchored forms — is litogramma's aside family:
//!   an `aside` deixis at the flow point, the body at the
//!   document end, an `->aside` edge between them. Content, not
//!   apparatus: no `<note>` on the body.
//! - **Index marks** (ruling #36): `\index{term}` becomes an
//!   `index-mark` in its flow position, the term as written (`!`
//!   subentries kept; `|...` directives stripped by the model).
//!   `\makeindex` / `\printindex` vanish — the back-of-book
//!   listing is a query over the marks, never a stored structure.
//! - **Quotes** from `quote`/`quotation` environments, **lists**
//!   from `itemize`/`enumerate` with `\item`, **verbatim** from
//!   the `verbatim` environment, kept as authored; the `verse`
//!   environment lowers to the verse vocabulary (ruling #37 —
//!   `\\` breaks lines, a blank line breaks strophes).
//! - Prose reads with a drop list, not a guess list: known
//!   metadata commands vanish (`\label`, spacing,
//!   `\documentclass`, `\usepackage`, `\includegraphics`, …),
//!   `\ref`/`\cite` keep their key as written (the reader sees a
//!   mark there), every *other* one-argument command unwraps to
//!   its content — the house corpus wraps prose in custom macros,
//!   and dropping their arguments would eat the text. Comments
//!   (`%` to end of line) vanish; math is kept as authored,
//!   without its `$` fences.
//! - `\input`/`\include` are skipped — a stated limit:
//!   one file, one reading (a multi-file document reads file by
//!   file, or through its built PDF's outline).

use quarb_text::{Block, Container, NoteFamily, TextModel};

/// Parse LaTeX source into the text-level model.
pub fn parse(source: &str) -> TextModel {
    TextModel::build(blocks(source))
}

const SECTIONING: &[(&str, u8)] = &[
    ("part", 1),
    ("chapter", 2),
    ("section", 3),
    ("subsection", 4),
    ("subsubsection", 5),
];

/// Commands whose argument is metadata, not prose: the command and
/// its braced argument both vanish.
const DROP: &[&str] = &[
    "label",
    "documentclass",
    "usepackage",
    "input",
    "include",
    "includegraphics",
    "bibliography",
    "bibliographystyle",
    "vspace",
    "hspace",
    "pagestyle",
    "thispagestyle",
    "newcommand",
    "renewcommand",
    "providecommand",
    "newenvironment",
    "renewenvironment",
    "newtheorem",
    "setlength",
    "definecolor",
    "hypersetup",
    "author",
    "date",
];

/// Environments whose entire content is out of the prose.
const SKIP_ENVS: &[&str] = &["tikzpicture", "titlepage", "comment", "tabular", "figure", "table"];

/// Lower LaTeX source into the block event stream.
pub fn blocks(source: &str) -> Vec<Block> {
    Lower::new(source).run()
}

struct Lower<'a> {
    src: &'a [u8],
    pos: usize,
    out: Vec<Block>,
    /// The open flow paragraph, accumulating prose.
    para: String,
    /// Callouts and index marks for the open paragraph, in order.
    inline: Vec<Inline>,
    /// Note bodies collected for the document end:
    /// (onym, family, margin-form, text).
    notes: Vec<(String, NoteFamily, bool, String)>,
    /// The auto-number for \footnote and bare \footnotemark, and
    /// the endnote family's own.
    counter: usize,
    e_counter: usize,
    /// Bare \footnotetext / \endnotetext bodies pair with these.
    text_counter: usize,
    e_text_counter: usize,
    /// Anchored asides (\marginpar / \marginnote) number here.
    a_counter: usize,
    /// Open list/quote environments, innermost last: the kind
    /// and whether an \item is open in THAT frame (per-frame,
    /// not shared — a nested list must not eat the outer
    /// item's close).
    envs: Vec<(&'static str, bool)>,
}

/// Inline apparatus captured mid-paragraph, emitted after the
/// paragraph's block in source order.
enum Inline {
    Note(String, NoteFamily, bool),
    Mark(String),
}

impl<'a> Lower<'a> {
    fn new(source: &'a str) -> Self {
        Lower {
            src: source.as_bytes(),
            pos: 0,
            out: Vec::new(),
            para: String::new(),
            inline: Vec::new(),
            notes: Vec::new(),
            counter: 0,
            e_counter: 0,
            text_counter: 0,
            e_text_counter: 0,
            a_counter: 0,
            envs: Vec::new(),
        }
    }

    fn run(mut self) -> Vec<Block> {
        while self.pos < self.src.len() {
            match self.src[self.pos] {
                b'%' => self.skip_comment(),
                b'\\' => self.command(),
                b'\n' => {
                    // A blank line ends the paragraph.
                    let mut look = self.pos + 1;
                    while look < self.src.len() && (self.src[look] == b' ' || self.src[look] == b'\t')
                    {
                        look += 1;
                    }
                    if look < self.src.len() && self.src[look] == b'\n' {
                        self.flush_para();
                        self.pos = look + 1;
                    } else {
                        self.para.push(' ');
                        self.pos += 1;
                    }
                }
                b'$' => {
                    // Math, kept as authored without its fences.
                    self.pos += 1;
                    let display = self.eat(b'$');
                    let start = self.pos;
                    while self.pos < self.src.len() && self.src[self.pos] != b'$' {
                        // An escaped character (\$, \\, …) cannot
                        // close the math — skip it whole.
                        if self.src[self.pos] == b'\\' && self.pos + 1 < self.src.len() {
                            self.pos += 1;
                        }
                        self.pos += 1;
                    }
                    self.para
                        .push_str(&String::from_utf8_lossy(&self.src[start..self.pos]));
                    self.pos += 1;
                    if display {
                        self.eat(b'$');
                    }
                }
                b'{' | b'}' | b'~' => {
                    if self.src[self.pos] == b'~' {
                        self.para.push(' ');
                    }
                    self.pos += 1;
                }
                _ => {
                    // A maximal plain run, pushed as UTF-8 — byte
                    // -wise char pushes mangled multibyte text.
                    let start = self.pos;
                    while self.pos < self.src.len()
                        && !matches!(
                            self.src[self.pos],
                            b'%' | b'\\' | b'\n' | b'$' | b'{' | b'}' | b'~'
                        )
                    {
                        self.pos += 1;
                    }
                    self.para
                        .push_str(&String::from_utf8_lossy(&self.src[start..self.pos]));
                }
            }
        }
        self.flush_para();
        self.close_lists_to(0);
        // The bodies, at the document end — the model's canonical
        // placement, whatever LaTeX's own at-callout convention.
        for (onym, family, margin, text) in std::mem::take(&mut self.notes) {
            self.out.push(Block::Open {
                kind: Container::Note {
                    onym,
                    family,
                    margin,
                },
                lemma: None,
            });
            self.out.push(Block::Paragraph { text });
            self.out.push(Block::Close { hypograph: None });
        }
        self.out
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.pos < self.src.len() && self.src[self.pos] == b {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn skip_comment(&mut self) {
        while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
            self.pos += 1;
        }
        self.pos += 1; // the newline is consumed with the comment
    }

    /// A control sequence at `pos` (on the backslash).
    fn command(&mut self) {
        self.pos += 1;
        // An escaped symbol: \%, \&, \$, \_, \#, \{, \}, \\ …
        if self.pos < self.src.len() && !self.src[self.pos].is_ascii_alphabetic() {
            let c = self.src[self.pos];
            self.pos += 1;
            match c {
                b'\\' => self.para.push(' '),
                _ => self.para.push(c as char),
            }
            return;
        }
        let start = self.pos;
        while self.pos < self.src.len() && self.src[self.pos].is_ascii_alphabetic() {
            self.pos += 1;
        }
        let name = String::from_utf8_lossy(&self.src[start..self.pos]).to_string();
        let starred = self.eat(b'*');
        let _ = starred;
        if name == "verb" {
            // \verb<delim>...<delim>: inline verbatim with an
            // arbitrary delimiter — copied as authored, so a $,
            // %, or \ inside cannot derail the scan.
            if self.pos < self.src.len() {
                let delim = self.src[self.pos];
                self.pos += 1;
                let start = self.pos;
                while self.pos < self.src.len() && self.src[self.pos] != delim {
                    self.pos += 1;
                }
                self.para
                    .push_str(&String::from_utf8_lossy(&self.src[start..self.pos]));
                self.pos += 1;
            }
            return;
        }
        if let Some((_, level)) = SECTIONING.iter().find(|(n, _)| *n == name) {
            let _ = self.optional();
            let lemma = self.braced().unwrap_or_default();
            self.flush_para();
            self.close_lists_to(0);
            self.out.push(Block::Heading {
                level: *level,
                lemma: strip_inline(&lemma),
            });
            return;
        }
        match name.as_str() {
            "footnote" | "endnote" | "sidenote" => {
                let family = family_of(&name);
                let margin = name == "sidenote";
                let _ = self.optional();
                let body = self.braced().unwrap_or_default();
                let c = self.auto_counter(family);
                *c += 1;
                let onym = c.to_string();
                self.inline.push(Inline::Note(onym.clone(), family, margin));
                self.notes.push((onym, family, margin, strip_inline(&body)));
            }
            "footnotemark" | "endnotemark" | "sidenotemark" => {
                let family = family_of(&name);
                let onym = match self.optional() {
                    Some(n) => n,
                    None => {
                        let c = self.auto_counter(family);
                        *c += 1;
                        c.to_string()
                    }
                };
                self.inline
                    .push(Inline::Note(onym, family, name == "sidenotemark"));
            }
            "footnotetext" | "endnotetext" | "sidenotetext" => {
                let family = family_of(&name);
                let onym = match self.optional() {
                    Some(n) => n,
                    None => {
                        let c = self.body_counter(family);
                        *c += 1;
                        c.to_string()
                    }
                };
                let body = self.braced().unwrap_or_default();
                self.notes
                    .push((onym, family, name == "sidenotetext", strip_inline(&body)));
            }
            "marginpar" | "marginnote" => {
                // Unnumbered anchored content — the aside family.
                // \marginpar takes its optional before the body,
                // \marginnote after; consume both sides.
                let _ = self.optional();
                let body = self.braced().unwrap_or_default();
                let _ = self.optional();
                self.a_counter += 1;
                let onym = self.a_counter.to_string();
                self.inline
                    .push(Inline::Note(onym.clone(), NoteFamily::Aside, false));
                self.notes
                    .push((onym, NoteFamily::Aside, false, strip_inline(&body)));
            }
            "index" => {
                // An index mark (ruling #36): the term as written;
                // the model strips `|...` directives.
                if let Some(term) = self.braced() {
                    self.inline.push(Inline::Mark(term));
                }
            }
            "begin" => {
                let env = self.braced().unwrap_or_default();
                self.environment(&env);
            }
            "end" => {
                let env = self.braced().unwrap_or_default();
                self.end_environment(&env);
            }
            "item" => {
                let _ = self.optional();
                self.flush_para();
                if let Some(top) = self.envs.last_mut() {
                    if top.1 {
                        self.out.push(Block::Close { hypograph: None });
                    }
                    top.1 = true;
                    self.out.push(Block::Open {
                        kind: Container::Item,
                        lemma: None,
                    });
                }
            }
            "ref" | "cref" | "Cref" | "eqref" | "pageref" | "cite" | "citep" | "citet" => {
                // The reader sees a mark; the key is what is
                // declared. Kept as written.
                if let Some(key) = self.braced() {
                    self.para.push_str(&key);
                }
            }
            "par" => self.flush_para(),
            "maketitle" | "tableofcontents" | "makeindex" | "printindex" | "theendnotes"
            | "newpage" | "clearpage" | "noindent" | "centering" | "raggedright" | "small"
            | "large" | "Large" | "footnotesize" | "scriptsize" | "ldots" | "dots" => {
                if name == "ldots" || name == "dots" {
                    self.para.push_str("...");
                }
            }
            "title" => {
                // The document title is the level-0 fact LaTeX
                // declares; the text level has no document lemma,
                // so it reads as front-matter prose.
                if let Some(t) = self.braced() {
                    self.flush_para();
                    self.out.push(Block::Paragraph { text: strip_inline(&t) });
                }
            }
            _ if DROP.contains(&name.as_str()) => {
                // A definition's argument train interleaves
                // [defaults] with {bodies}: \newcommand{\x}[1]{...}
                // puts the [1] BETWEEN the braced groups, so
                // optionals are consumed before every group —
                // otherwise the macro body survives as prose and
                // its \index marks fire (the spec.tex leak).
                let extra = match name.as_str() {
                    "newcommand" | "renewcommand" | "providecommand" | "newtheorem"
                    | "setlength" => 1,
                    "newenvironment" | "renewenvironment" => 2,
                    _ => 0,
                };
                while self.optional().is_some() {}
                let _ = self.braced();
                for _ in 0..extra {
                    while self.optional().is_some() {}
                    let _ = self.braced();
                }
                // \newtheorem{x}{T}[within] — the trailing scope.
                if name == "newtheorem" {
                    while self.optional().is_some() {}
                }
            }
            _ => {
                // Any other command unwraps: its braced content is
                // prose (the house corpus wraps text in custom
                // macros); a command with no argument vanishes.
                let _ = self.optional();
                if let Some(content) = self.braced() {
                    self.para.push_str(&strip_inline(&content));
                }
            }
        }
    }

    fn environment(&mut self, env: &str) {
        match env {
            "quote" | "quotation" => {
                self.flush_para();
                self.out.push(Block::Open {
                    kind: Container::Blockquote,
                    lemma: None,
                });
                self.envs.push(("quote", false));
            }
            "itemize" | "enumerate" => {
                self.flush_para();
                self.out.push(Block::Open {
                    kind: if env == "enumerate" {
                        Container::OrderedList { start: 1 }
                    } else {
                        Container::UnorderedList
                    },
                    lemma: None,
                });
                self.envs.push(("list", false));
            }
            "verse" => {
                // The verse vocabulary (ruling #37): `\\\\` breaks
                // lines, a blank line breaks strophes; inline
                // markup unwraps per line.
                self.flush_para();
                let end = b"\\end{verse}";
                let rest = &self.src[self.pos..];
                let stop = rest
                    .windows(end.len())
                    .position(|w| w == end)
                    .unwrap_or(rest.len());
                let body = String::from_utf8_lossy(&rest[..stop]).to_string();
                self.pos += stop + end.len().min(rest.len() - stop);
                // Each line lowers through a sub-scan whose
                // apparatus is harvested: a \footnote or
                // \endnotemark inside a verse line anchors at
                // the verse block (line-level anchoring is the
                // recorded follow-up). Auto-numbered marks inside
                // verse need explicit [n] — the sub-scan's
                // counters are isolated.
                let mut strophes: Vec<Vec<String>> = Vec::new();
                for chunk in body.split("\n\n") {
                    let mut lines = Vec::new();
                    for line in chunk.split("\\\\") {
                        let mut sub = Lower::new(line);
                        while sub.pos < sub.src.len() {
                            match sub.src[sub.pos] {
                                b'%' => sub.skip_comment(),
                                b'\\' => sub.command(),
                                b'{' | b'}' => sub.pos += 1,
                                b'~' => {
                                    sub.para.push(' ');
                                    sub.pos += 1;
                                }
                                _ => {
                                    let start = sub.pos;
                                    while sub.pos < sub.src.len()
                                        && !matches!(
                                            sub.src[sub.pos],
                                            b'%' | b'\\' | b'{' | b'}' | b'~'
                                        )
                                    {
                                        sub.pos += 1;
                                    }
                                    sub.para.push_str(&String::from_utf8_lossy(
                                        &sub.src[start..sub.pos],
                                    ));
                                }
                            }
                        }
                        self.inline.append(&mut sub.inline);
                        self.notes.append(&mut sub.notes);
                        let text = quarb_text::normalize_ws(&sub.para);
                        if !text.is_empty() {
                            lines.push(text);
                        }
                    }
                    if !lines.is_empty() {
                        strophes.push(lines);
                    }
                }
                self.out.push(Block::Verse {
                    lemma: None,
                    strophes,
                    hypograph: None,
                });
                for ev in std::mem::take(&mut self.inline) {
                    match ev {
                        Inline::Note(onym, family, margin) => self.out.push(Block::NoteRef {
                            onym,
                            family: Some(family),
                            margin,
                        }),
                        Inline::Mark(term) => self.out.push(Block::IndexMark { term }),
                    }
                }
            }
            "verbatim" => {
                self.flush_para();
                let end = b"\\end{verbatim}";
                let rest = &self.src[self.pos..];
                let stop = rest
                    .windows(end.len())
                    .position(|w| w == end)
                    .unwrap_or(rest.len());
                let text = String::from_utf8_lossy(&rest[..stop])
                    .trim_matches('\n')
                    .to_string();
                self.out.push(Block::Verbatim { lang: None, text });
                self.pos += stop + end.len().min(rest.len() - stop);
            }
            e if SKIP_ENVS.contains(&e) => {
                // Skip to the matching \end{e} (non-nested scan —
                // these environments do not nest themselves).
                let end = format!("\\end{{{e}}}");
                let end = end.as_bytes();
                let rest = &self.src[self.pos..];
                let stop = rest
                    .windows(end.len())
                    .position(|w| w == end)
                    .map(|p| p + end.len())
                    .unwrap_or(rest.len());
                self.pos += stop;
            }
            _ => {
                // Unknown environments are transparent: their flow
                // content is kept, the wrapper leaves no node.
            }
        }
    }

    fn end_environment(&mut self, env: &str) {
        match env {
            "quote" | "quotation" => {
                self.flush_para();
                if self.envs.last().is_some_and(|f| f.0 == "quote") {
                    self.close_frame();
                }
            }
            "itemize" | "enumerate" => {
                self.flush_para();
                if self.envs.last().is_some_and(|f| f.0 == "list") {
                    self.close_frame();
                }
            }
            _ => {}
        }
    }

    fn close_lists_to(&mut self, to: usize) {
        while self.envs.len() > to {
            self.close_frame();
        }
    }

    /// Close the innermost frame: its open item first, then the
    /// container itself.
    fn close_frame(&mut self) {
        if let Some((_, item_open)) = self.envs.pop() {
            if item_open {
                self.out.push(Block::Close { hypograph: None });
            }
            self.out.push(Block::Close { hypograph: None });
        }
    }

    fn auto_counter(&mut self, family: NoteFamily) -> &mut usize {
        match family {
            NoteFamily::Footnote => &mut self.counter,
            NoteFamily::Endnote => &mut self.e_counter,
            NoteFamily::Aside => &mut self.a_counter,
        }
    }

    fn body_counter(&mut self, family: NoteFamily) -> &mut usize {
        match family {
            NoteFamily::Footnote => &mut self.text_counter,
            NoteFamily::Endnote => &mut self.e_text_counter,
            NoteFamily::Aside => &mut self.a_counter,
        }
    }

    /// Flush the open paragraph as flow (Text inside containers,
    /// Paragraph otherwise), then its inline apparatus — callouts
    /// and index marks — in source order.
    fn flush_para(&mut self) {
        let text = quarb_text::normalize_ws(&self.para);
        self.para.clear();
        if !text.is_empty() {
            if self.envs.is_empty() {
                self.out.push(Block::Paragraph { text });
            } else {
                self.out.push(Block::Text { text });
            }
        }
        for ev in std::mem::take(&mut self.inline) {
            match ev {
                Inline::Note(onym, family, margin) => self.out.push(Block::NoteRef {
                    onym,
                    family: Some(family),
                    margin,
                }),
                Inline::Mark(term) => self.out.push(Block::IndexMark { term }),
            }
        }
    }

    /// A `[...]` optional argument, if present.
    fn optional(&mut self) -> Option<String> {
        self.skip_ws();
        if self.pos >= self.src.len() || self.src[self.pos] != b'[' {
            return None;
        }
        self.pos += 1;
        let start = self.pos;
        while self.pos < self.src.len() && self.src[self.pos] != b']' {
            self.pos += 1;
        }
        let s = String::from_utf8_lossy(&self.src[start..self.pos]).to_string();
        self.pos += 1;
        Some(s)
    }

    /// A balanced `{...}` argument, if present, raw.
    fn braced(&mut self) -> Option<String> {
        self.skip_ws();
        if self.pos >= self.src.len() || self.src[self.pos] != b'{' {
            return None;
        }
        self.pos += 1;
        let start = self.pos;
        let mut depth = 1usize;
        while self.pos < self.src.len() {
            match self.src[self.pos] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                b'\\' if self.pos + 1 < self.src.len() => self.pos += 1,
                _ => {}
            }
            self.pos += 1;
        }
        let s = String::from_utf8_lossy(&self.src[start..self.pos]).to_string();
        self.pos += 1;
        Some(s)
    }

    fn skip_ws(&mut self) {
        while self.pos < self.src.len() && (self.src[self.pos] == b' ' || self.src[self.pos] == b'\t')
        {
            self.pos += 1;
        }
    }
}

/// `side*` commands are footnotes in the margin — family
/// footnote, `::::form` carries the spelling.
fn family_of(command: &str) -> NoteFamily {
    if command.starts_with("end") {
        NoteFamily::Endnote
    } else {
        NoteFamily::Footnote
    }
}

/// Strip inline markup from an argument's raw text: a recursive
/// mini-lowering that unwraps commands and drops the metadata ones
/// (a heading lemma or footnote body may itself carry \emph and
/// \label).
fn strip_inline(raw: &str) -> String {
    let mut lower = Lower::new(raw);
    while lower.pos < lower.src.len() {
        match lower.src[lower.pos] {
            b'%' => lower.skip_comment(),
            b'\\' => lower.command(),
            b'{' | b'}' => lower.pos += 1,
            b'~' => {
                lower.para.push(' ');
                lower.pos += 1;
            }
            _ => {
                let start = lower.pos;
                while lower.pos < lower.src.len()
                    && !matches!(
                        lower.src[lower.pos],
                        b'%' | b'\\' | b'{' | b'}' | b'~'
                    )
                {
                    lower.pos += 1;
                }
                lower
                    .para
                    .push_str(&String::from_utf8_lossy(&lower.src[start..lower.pos]));
            }
        }
    }
    quarb_text::normalize_ws(&lower.para)
}
