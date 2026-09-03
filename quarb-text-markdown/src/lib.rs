//! Markdown adapter for the Quarb text level. Unlike the
//! DOM-level `quarb-markdown` (which renders to HTML and serves
//! `quarb-html`), this crate maps pulldown-cmark's event stream
//! onto the shared `quarb-text` vocabulary directly — heading
//! levels, list ordinals, and fence languages come from the
//! source, with no HTML round-trip.
//!
//! Adapter rules:
//!
//! - Headings arrive flat; `quarb-text` derives the enclosing
//!   section tree.
//! - Tight and loose lists differ as authored: a tight item's
//!   inline content becomes the item's own text, a loose item's
//!   paragraphs become child paragraphs.
//! - Fenced code becomes a verbatim block, language from the
//!   fence info string; pipe tables become [`Block::Table`]s
//!   (the syntactic header row as headers), denormalized to
//!   nested lists by `quarb-text`.
//! - Inline markup flattens to its text; image alt text and raw
//!   inline HTML are dropped; footnote definitions flow as
//!   ordinary content, their reference markers are dropped.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use quarb_text::{Block, Container, NoteFamily, TextModel};

/// Parse Markdown `text` and lower it to a text-level document.
pub fn parse(text: &str) -> TextModel {
    TextModel::build(blocks(text))
}

/// Pandoc-style bracketed citations: every `@key` inside a
/// `[...]` group whose `@` sits at a citation boundary (the
/// group start, or after whitespace, `-`, `;`, or `(`) yields a
/// cit mark — `[@knuth84]`, `[see @a, p. 3; -@b]`. The bare
/// narrative `@key` is deliberately not read: outside pandoc's
/// own parser it is indistinguishable from emails and
/// @mentions, so the bracket is the unambiguous spelling. The
/// bracket text stays in the prose as authored.
fn scan_citations(text: &str, cites: &mut Vec<String>) {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '[' {
            i += 1;
            continue;
        }
        let Some(close) = chars[i + 1..].iter().position(|&c| c == ']') else {
            break;
        };
        let group = &chars[i + 1..i + 1 + close];
        let mut j = 0;
        while j < group.len() {
            if group[j] == '@'
                && (j == 0
                    || group[j - 1].is_whitespace()
                    || matches!(group[j - 1], '-' | ';' | '('))
                && let Some(key) = citation_key(&group[j + 1..])
            {
                cites.push(key);
            }
            j += 1;
        }
        i += 1 + close + 1;
    }
}

/// A pandoc citation key: a letter, digit, or `_` first, then
/// alphanumerics with internal punctuation from the pandoc set —
/// each punctuation character must be followed by an
/// alphanumeric, so a trailing comma or period stays prose.
fn citation_key(rest: &[char]) -> Option<String> {
    let first = *rest.first()?;
    if !(first.is_alphanumeric() || first == '_') {
        return None;
    }
    let mut key = String::new();
    key.push(first);
    let mut i = 1;
    while i < rest.len() {
        let c = rest[i];
        if c.is_alphanumeric() || c == '_' {
            key.push(c);
            i += 1;
        } else if matches!(c, ':' | '.' | '#' | '$' | '%' | '&' | '-' | '+' | '?' | '<' | '>' | '~' | '/')
            && rest
                .get(i + 1)
                .is_some_and(|n| n.is_alphanumeric() || *n == '_')
        {
            key.push(c);
            i += 1;
        } else {
            break;
        }
    }
    Some(key)
}

/// Emit the block's citation marks after it, in source order.
fn drain_cites(cites: &mut Vec<String>, out: &mut Vec<Block>) {
    for target in cites.drain(..) {
        out.push(Block::Cite { target });
    }
}

/// Emit the block's mentions after it, in source order.
fn drain_refs(refs: &mut Vec<(String, String, bool)>, out: &mut Vec<Block>) {
    for (target, text, internal) in refs.drain(..) {
        out.push(Block::Ref {
            target,
            text: Some(text).filter(|t| !t.is_empty()),
            internal,
        });
    }
}

/// The event stream `parse` builds from — exposed for testing and
/// composition.
pub fn blocks(text: &str) -> Vec<Block> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_HEADING_ATTRIBUTES);

    let mut out = Vec::new();
    // Inline capture for the block being read (heading, paragraph,
    // code block, table cell) — these never nest in pulldown's
    // stream.
    let mut cap: Option<String> = None;
    // Uncaptured inline content (a tight list item's own text).
    let mut run = String::new();
    // Depth inside `Image` tags: alt text is not prose.
    let mut image = 0usize;
    let mut table: Option<TableState> = None;
    // Fence info of the open code block, taken at its end.
    let mut fence: Option<String> = None;
    // Footnote callouts met in the open block ([^name] — the
    // markdown footnote extension declares the footnote family),
    // emitted after the block in source order.
    let mut callouts: Vec<String> = Vec::new();
    // In-prose links met in the open block: (target, text,
    // internal) — mentions, emitted after the block like the
    // callouts. `link_open` remembers where the link's text began
    // in the running capture.
    let mut refs: Vec<(String, String, bool)> = Vec::new();
    // Pandoc-style bracketed citations met in the open block,
    // emitted after it like the callouts.
    let mut cites: Vec<String> = Vec::new();
    let mut link_open: Option<(String, usize)> = None;
    // The heading's `{#id}` attribute — the section's label.
    let mut heading_id: Option<String> = None;

    for event in Parser::new_ext(text, opts) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { id, .. } => {
                    flush(&mut run, &mut out);
                    heading_id = id.map(|i| i.to_string());
                    cap = Some(String::new());
                }
                Tag::Paragraph => {
                    flush(&mut run, &mut out);
                    cap = Some(String::new());
                }
                Tag::Link { dest_url, .. } => {
                    let at = cap.as_ref().map_or(run.len(), String::len);
                    link_open = Some((dest_url.to_string(), at));
                }
                Tag::CodeBlock(kind) => {
                    flush(&mut run, &mut out);
                    fence = match kind {
                        CodeBlockKind::Fenced(info) => info
                            .split_whitespace()
                            .next()
                            .filter(|s| !s.is_empty())
                            .map(str::to_string),
                        CodeBlockKind::Indented => None,
                    };
                    cap = Some(String::new());
                }
                Tag::BlockQuote(_) => {
                    flush(&mut run, &mut out);
                    out.push(Block::Open {
                        kind: Container::Blockquote,
                        lemma: None,
                    });
                }
                Tag::List(start) => {
                    flush(&mut run, &mut out);
                    let kind = match start {
                        Some(n) => Container::OrderedList { start: n as i64 },
                        None => Container::UnorderedList,
                    };
                    out.push(Block::Open { kind, lemma: None });
                }
                Tag::Item => {
                    flush(&mut run, &mut out);
                    out.push(Block::Open {
                        kind: Container::Item,
                        lemma: None,
                    });
                }
                Tag::Table(_) => {
                    flush(&mut run, &mut out);
                    table = Some(TableState::default());
                }
                Tag::TableHead => {
                    if let Some(t) = table.as_mut() {
                        t.in_head = true;
                        t.row.clear();
                    }
                }
                Tag::TableRow => {
                    if let Some(t) = table.as_mut() {
                        t.row.clear();
                    }
                }
                Tag::TableCell => cap = Some(String::new()),
                Tag::Image { .. } => image += 1,
                // The footnote body: a note container of the
                // footnote family; its paragraphs flow inside.
                Tag::FootnoteDefinition(name) => {
                    flush(&mut run, &mut out);
                    out.push(Block::Open {
                        kind: Container::Note {
                            onym: name.to_string(),
                            family: NoteFamily::Footnote,
                            margin: false,
                        },
                        lemma: None,
                    });
                }
                // Emphasis, links, …: their inner text flows.
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(level) => {
                    let lemma = cap.take().unwrap_or_default();
                    scan_citations(&lemma, &mut cites);
                    out.push(Block::Heading {
                        level: heading_level(level),
                        lemma,
                    });
                    if let Some(id) = heading_id.take() {
                        out.push(Block::Label { onym: id });
                    }
                    drain_callouts(&mut callouts, &mut out);
                    drain_refs(&mut refs, &mut out);
                    drain_cites(&mut cites, &mut out);
                }
                TagEnd::Paragraph => {
                    let text = cap.take().unwrap_or_default();
                    scan_citations(&text, &mut cites);
                    out.push(Block::Paragraph { text });
                    drain_callouts(&mut callouts, &mut out);
                    drain_refs(&mut refs, &mut out);
                    drain_cites(&mut cites, &mut out);
                }
                TagEnd::Link => {
                    if let Some((dest, at)) = link_open.take() {
                        let buf = cap.as_deref().unwrap_or(&run);
                        let text = buf.get(at..).unwrap_or_default().trim().to_string();
                        let internal = dest.starts_with('#');
                        refs.push((dest, text, internal));
                    }
                }
                TagEnd::CodeBlock => {
                    // Language recorded at Start; recover it from
                    // the pending fence info.
                    let text = cap.take().unwrap_or_default();
                    let lang = fence.take();
                    out.push(Block::Verbatim {
                        lang,
                        text: text.trim_end_matches('\n').to_string(),
                    });
                }
                TagEnd::BlockQuote(_) | TagEnd::List(_) | TagEnd::Item => {
                    flush(&mut run, &mut out);
                    out.push(Block::Close { hypograph: None });
                }
                TagEnd::TableCell => {
                    if let Some(t) = table.as_mut() {
                        t.row.push(cap.take().unwrap_or_default());
                    }
                }
                TagEnd::TableHead => {
                    if let Some(t) = table.as_mut() {
                        t.headers = Some(std::mem::take(&mut t.row));
                        t.in_head = false;
                    }
                }
                TagEnd::TableRow => {
                    if let Some(t) = table.as_mut() {
                        let row = std::mem::take(&mut t.row);
                        t.rows.push(
                            row.into_iter()
                                .map(|text| quarb_text::Cell { label: None, text })
                                .collect(),
                        );
                    }
                }
                TagEnd::Table => {
                    if let Some(t) = table.take() {
                        out.push(Block::Table {
                            lemma: None,
                            headers: t.headers,
                            rows: t.rows,
                        });
                    }
                }
                TagEnd::Image => image = image.saturating_sub(1),
                TagEnd::FootnoteDefinition => {
                    flush(&mut run, &mut out);
                    out.push(Block::Close { hypograph: None });
                }
                _ => {}
            },
            Event::Text(t) | Event::Code(t) => {
                if image == 0 {
                    sink(&mut cap, &mut run, &t);
                }
            }
            Event::SoftBreak | Event::HardBreak => sink(&mut cap, &mut run, " "),
            Event::Rule => flush(&mut run, &mut out),
            // The callout: a footnote-family reference, no marker
            // text in the prose (ruling #35).
            Event::FootnoteReference(name) => callouts.push(name.to_string()),
            // Raw HTML, task-list checkboxes, math: not prose.
            Event::Html(_)
            | Event::InlineHtml(_)
            | Event::TaskListMarker(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_) => {}
        }
    }
    flush(&mut run, &mut out);
    out
}

#[derive(Default)]
struct TableState {
    in_head: bool,
    headers: Option<Vec<String>>,
    rows: Vec<Vec<quarb_text::Cell>>,
    row: Vec<String>,
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Inline content goes to the open capture if any, else to the
/// bare run (a tight item's own text).
fn sink(cap: &mut Option<String>, run: &mut String, text: &str) {
    match cap {
        Some(c) => c.push_str(text),
        None => run.push_str(text),
    }
}

fn drain_callouts(callouts: &mut Vec<String>, out: &mut Vec<Block>) {
    for onym in callouts.drain(..) {
        out.push(Block::NoteRef {
            onym,
            family: Some(NoteFamily::Footnote),
            margin: false,
        });
    }
}

fn flush(run: &mut String, out: &mut Vec<Block>) {
    if !run.trim().is_empty() {
        out.push(Block::Text { text: run.clone() });
    }
    run.clear();
}
