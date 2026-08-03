//! Markdown producer for the Quarb text level. Unlike the
//! DOM-level `quarb-markdown` (which renders to HTML and serves
//! `quarb-html`), this crate maps pulldown-cmark's event stream
//! onto the shared `quarb-text` vocabulary directly — heading
//! levels, list ordinals, and fence languages come from the
//! source, with no HTML round-trip.
//!
//! Producer rules:
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
use quarb_text::{Block, Container, TextModel};

/// Parse Markdown `text` and lower it to a text-level document.
pub fn parse(text: &str) -> TextModel {
    TextModel::build(blocks(text))
}

/// The event stream `parse` builds from — exposed for testing and
/// composition.
pub fn blocks(text: &str) -> Vec<Block> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_TASKLISTS);

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

    for event in Parser::new_ext(text, opts) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { .. } | Tag::Paragraph => {
                    flush(&mut run, &mut out);
                    cap = Some(String::new());
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
                // Emphasis, links, footnote definitions, …: their
                // inner text flows.
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(level) => {
                    out.push(Block::Heading {
                        level: heading_level(level),
                        lemma: cap.take().unwrap_or_default(),
                    });
                }
                TagEnd::Paragraph => {
                    out.push(Block::Paragraph {
                        text: cap.take().unwrap_or_default(),
                    });
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
                _ => {}
            },
            Event::Text(t) | Event::Code(t) => {
                if image == 0 {
                    sink(&mut cap, &mut run, &t);
                }
            }
            Event::SoftBreak | Event::HardBreak => sink(&mut cap, &mut run, " "),
            Event::Rule => flush(&mut run, &mut out),
            // Raw HTML, footnote reference markers, task-list
            // checkboxes, math: not prose.
            Event::Html(_)
            | Event::InlineHtml(_)
            | Event::FootnoteReference(_)
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

fn flush(run: &mut String, out: &mut Vec<Block>) {
    if !run.trim().is_empty() {
        out.push(Block::Text { text: run.clone() });
    }
    run.clear();
}
