//! HTML producer for the Quarb text level: reduces a page to what
//! it *says* — headings, paragraphs, quotes, lists, verbatim
//! blocks, tables — and drops the markup soup. The DOM-faithful
//! view is `quarb-html`; this crate lowers the same substrate into
//! the shared `quarb-text` vocabulary, so `//section[::lemma ...]`
//! and `//paragraph` read the same over a page as over any other
//! text substrate.
//!
//! Producer rules (the HTML-specific knowledge lives here):
//!
//! - `h1`–`h6` become flat [`Block::Heading`]s; `quarb-text`
//!   derives the enclosing section tree.
//! - Soup is dropped: `script`, `style`, `nav`, `header`,
//!   `footer`, `aside`, `form`, media elements, the whole
//!   `head` — and, whatever the element, ARIA chrome (landmark
//!   roles like `navigation`/`banner`, or `aria-hidden="true"`).
//! - Structural wrappers (`div`, `section`, `article`, `main`, …)
//!   are transparent: their flow content is walked, the wrapper
//!   itself leaves no node.
//! - A `blockquote`'s trailing `cite`/`footer` child — or the
//!   `figcaption` of a `figure`-wrapped quote — becomes the
//!   quote's hypograph (attribution).
//! - `pre` becomes a verbatim block, language from a
//!   `language-*` class; `table` becomes a [`Block::Table`]
//!   (caption from `<caption>`, headers from `thead`/`th` cells),
//!   denormalized to nested lists by `quarb-text`.
//! - Inline markup flattens to its text.

use ego_tree::NodeRef;
use quarb_text::{Block, Cell, Container, TextModel};
use scraper::{ElementRef, Html, Node as DomNode};

/// Parse `html` and lower it to a text-level document.
pub fn parse(html: &str) -> TextModel {
    TextModel::build(blocks(html))
}

/// The event stream `parse` builds from — exposed for testing and
/// composition.
pub fn blocks(html: &str) -> Vec<Block> {
    let document = Html::parse_document(html);
    let mut out = Vec::new();
    let mut run = String::new();

    // Explicit work stack (children pushed reversed, popped in
    // document order) so pathologically deep markup cannot
    // overflow the call stack.
    let mut stack: Vec<Work> = vec![Work::El(document.root_element())];
    while let Some(work) = stack.pop() {
        match work {
            Work::Text(text) => run.push_str(&text),
            Work::Flush => flush(&mut run, &mut out),
            Work::Open { kind, lemma } => {
                flush(&mut run, &mut out);
                out.push(Block::Open { kind, lemma });
            }
            Work::Close { hypograph } => {
                flush(&mut run, &mut out);
                out.push(Block::Close { hypograph });
            }
            Work::El(el) => element(el, &mut run, &mut out, &mut stack),
        }
    }
    flush(&mut run, &mut out);
    out
}

enum Work<'a> {
    El(ElementRef<'a>),
    Text(String),
    Flush,
    Open { kind: Container, lemma: Option<String> },
    Close { hypograph: Option<String> },
}

/// Elements whose entire subtree is soup at the text level.
const SKIP: &[&str] = &[
    "head", "script", "style", "noscript", "template", "nav", "header", "footer", "aside", "form",
    "button", "select", "input", "textarea", "label", "iframe", "svg", "math", "img", "picture",
    "video", "audio", "canvas", "object", "map", "colgroup", "col",
];

/// ARIA landmark roles that mark chrome, whatever the element —
/// the standards-level equivalent of the SKIP tags.
const CHROME_ROLES: &[&str] = &[
    "navigation",
    "banner",
    "contentinfo",
    "search",
    "complementary",
    "menu",
    "menubar",
    "toolbar",
    "presentation",
    "none",
];

/// Whether an element is chrome by its ARIA surface: a chrome
/// landmark role, or hidden from assistive readers outright.
fn aria_chrome(el: ElementRef) -> bool {
    if el.value().attr("aria-hidden") == Some("true") {
        return true;
    }
    el.value()
        .attr("role")
        .is_some_and(|r| CHROME_ROLES.contains(&r))
}

/// Structural wrappers walked transparently: their flow content is
/// kept, the wrapper leaves no node. A wrapper boundary breaks an
/// inline run.
const TRANSPARENT: &[&str] = &[
    "html", "body", "div", "section", "article", "main", "hgroup", "details", "dialog", "address",
    "fieldset", "center", "tbody", "thead", "tfoot", "tr", "td", "th",
];

/// Block elements read as plain paragraphs.
const P_LIKE: &[&str] = &["p", "figcaption", "dt", "dd", "summary", "legend", "caption"];

fn element<'a>(
    el: ElementRef<'a>,
    run: &mut String,
    out: &mut Vec<Block>,
    stack: &mut Vec<Work<'a>>,
) {
    let tag = el.value().name();
    match tag {
        _ if SKIP.contains(&tag) || aria_chrome(el) => {}
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            flush(run, out);
            out.push(Block::Heading {
                level: tag[1..].parse().unwrap(),
                lemma: text_of(el),
            });
        }
        _ if P_LIKE.contains(&tag) => {
            flush(run, out);
            out.push(Block::Paragraph { text: text_of(el) });
        }
        "blockquote" => {
            flush(run, out);
            out.push(Block::Open {
                kind: Container::Blockquote,
                lemma: None,
            });
            let (children, hypograph) = quote_content(el);
            stack.push(Work::Close { hypograph });
            push_children(children, stack);
        }
        "figure" => {
            flush(run, out);
            let quote = child_by_tag(el, "blockquote");
            let caption = child_by_tag(el, "figcaption");
            match (quote, caption) {
                (Some(quote), Some(caption)) => {
                    // A figure-wrapped quotation: the figcaption is
                    // the attribution.
                    out.push(Block::Open {
                        kind: Container::Blockquote,
                        lemma: None,
                    });
                    let (children, inner) = quote_content(quote);
                    stack.push(Work::Close {
                        hypograph: inner.or(Some(text_of(caption))),
                    });
                    push_children(children, stack);
                }
                _ => {
                    stack.push(Work::Flush);
                    push_children(el.children().collect(), stack);
                }
            }
        }
        "ul" => open_list(el, Container::UnorderedList, stack, run, out),
        "ol" => {
            let start = el
                .value()
                .attr("start")
                .and_then(|s| s.parse().ok())
                .unwrap_or(1);
            open_list(el, Container::OrderedList { start }, stack, run, out);
        }
        "dl" => {
            flush(run, out);
            out.push(Block::Open {
                kind: Container::UnorderedList,
                lemma: None,
            });
            stack.push(Work::Close { hypograph: None });
            for (terms, dds) in dl_groups(el).into_iter().rev() {
                stack.push(Work::Close { hypograph: None });
                for dd in dds.into_iter().rev() {
                    push_children(dd, stack);
                    stack.push(Work::Flush);
                }
                stack.push(Work::Open {
                    kind: Container::Item,
                    lemma: Some(terms),
                });
            }
        }
        "li" => {
            flush(run, out);
            out.push(Block::Open {
                kind: Container::Item,
                lemma: None,
            });
            stack.push(Work::Close { hypograph: None });
            push_children(el.children().collect(), stack);
        }
        "pre" => {
            flush(run, out);
            out.push(Block::Verbatim {
                lang: verbatim_lang(el),
                text: text_of_raw(el),
            });
        }
        "table" => {
            flush(run, out);
            out.push(table_block(el));
        }
        "hr" => flush(run, out),
        "br" => run.push(' '),
        _ if TRANSPARENT.contains(&tag) => {
            stack.push(Work::Flush);
            push_children(el.children().collect(), stack);
        }
        // Everything else is inline: flatten to text.
        _ => run.push_str(&text_of_raw(el)),
    }
}

/// Push DOM children (elements and text nodes) reversed, so they
/// pop in document order.
fn push_children<'a>(children: Vec<NodeRef<'a, DomNode>>, stack: &mut Vec<Work<'a>>) {
    for child in children.into_iter().rev() {
        if let Some(el) = ElementRef::wrap(child) {
            stack.push(Work::El(el));
        } else if let DomNode::Text(text) = child.value() {
            stack.push(Work::Text(text.to_string()));
        }
    }
}

/// A `dl`'s term groups: each run of `dt`s (terms joined with
/// `, `) paired with its following `dd`s' content children, one
/// batch per `dd` — HTML's serialization of "items with lemmas",
/// regrouped.
type DdBatches<'a> = Vec<Vec<NodeRef<'a, DomNode>>>;
fn dl_groups<'a>(el: ElementRef<'a>) -> Vec<(String, DdBatches<'a>)> {
    let mut groups: Vec<(String, DdBatches<'a>)> = Vec::new();
    let mut terms: Vec<String> = Vec::new();
    for child in el.children() {
        let Some(cel) = ElementRef::wrap(child) else {
            continue;
        };
        match cel.value().name() {
            "dt" => {
                if !groups.is_empty()
                    && terms.is_empty()
                    && groups.last().is_some_and(|(_, dds)| dds.is_empty())
                {
                    // consecutive groups without dd stay separate
                }
                terms.push(text_of(cel));
            }
            "dd" => {
                if !terms.is_empty() {
                    groups.push((terms.join(", "), Vec::new()));
                    terms.clear();
                }
                if let Some((_, dds)) = groups.last_mut() {
                    dds.push(cel.children().collect());
                }
            }
            // div wrappers around dt/dd groups are legal HTML
            "div" => {
                for inner in cel.children() {
                    if let Some(iel) = ElementRef::wrap(inner) {
                        match iel.value().name() {
                            "dt" => terms.push(text_of(iel)),
                            "dd" => {
                                if !terms.is_empty() {
                                    groups.push((terms.join(", "), Vec::new()));
                                    terms.clear();
                                }
                                if let Some((_, dds)) = groups.last_mut() {
                                    dds.push(iel.children().collect());
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if !terms.is_empty() {
        groups.push((terms.join(", "), Vec::new()));
    }
    groups
}

/// The first direct child element with `tag`, if any.
fn child_by_tag<'a>(el: ElementRef<'a>, tag: &str) -> Option<ElementRef<'a>> {
    el.children()
        .filter_map(ElementRef::wrap)
        .find(|c| c.value().name() == tag)
}

/// A blockquote's content children and its attribution: the last
/// direct `cite`/`footer` child, removed from the content.
fn quote_content<'a>(el: ElementRef<'a>) -> (Vec<NodeRef<'a, DomNode>>, Option<String>) {
    let mut hypograph = None;
    let mut attribution_id = None;
    for child in el.children() {
        if let Some(c) = ElementRef::wrap(child)
            && matches!(c.value().name(), "cite" | "footer") {
                hypograph = Some(text_of(c));
                attribution_id = Some(child.id());
            }
    }
    let children = el
        .children()
        .filter(|c| Some(c.id()) != attribution_id)
        .collect();
    (children, hypograph)
}

fn open_list<'a>(
    el: ElementRef<'a>,
    kind: Container,
    stack: &mut Vec<Work<'a>>,
    run: &mut String,
    out: &mut Vec<Block>,
) {
    flush(run, out);
    out.push(Block::Open { kind, lemma: None });
    stack.push(Work::Close { hypograph: None });
    push_children(el.children().collect(), stack);
}

/// The language of a `pre` block, from a `language-*` class on the
/// `pre` itself or a direct `code` child.
fn verbatim_lang(el: ElementRef) -> Option<String> {
    let mut candidates = vec![el];
    candidates.extend(el.children().filter_map(ElementRef::wrap));
    for c in candidates {
        if let Some(class) = c.value().attr("class") {
            for word in class.split_whitespace() {
                if let Some(lang) = word.strip_prefix("language-")
                    && !lang.is_empty() {
                        return Some(lang.to_string());
                    }
            }
        }
    }
    None
}

/// Lower a `table` element: caption, a header row (from `thead` or
/// a leading all-`th` row of two or more cells), and the data
/// rows. Two shapes beyond the plain grid are recognized:
///
/// - a **leading single-`th` row** is the table's title (a
///   Wikipedia-infobox convention): it becomes the lemma when no
///   `<caption>` claimed it;
/// - a **row-label row** (`th` first, then `td`s) carries its
///   label onto the first value — `Date: 2 November 1932` — the
///   row-wise mirror of the column-header prefix; a lone mid-table
///   `th` stays a bare line (a subheading within the table).
///
/// Cell text is flattened; the nested-list denormalization is
/// `quarb-text`'s.
fn table_block(el: ElementRef) -> Block {
    let mut lemma = None;
    let mut headers: Option<Vec<String>> = None;
    let mut rows: Vec<Vec<Cell>> = Vec::new();

    let mut table_rows: Vec<(ElementRef, bool)> = Vec::new();
    for child in el.children().filter_map(ElementRef::wrap) {
        match child.value().name() {
            "caption" => lemma = Some(text_of(child)),
            "tr" => table_rows.push((child, false)),
            "thead" | "tbody" | "tfoot" => {
                let in_head = child.value().name() == "thead";
                for tr in child.children().filter_map(ElementRef::wrap) {
                    if tr.value().name() == "tr" {
                        table_rows.push((tr, in_head));
                    }
                }
            }
            _ => {}
        }
    }

    let mut first = true;
    for (tr, in_head) in table_rows {
        // (is_th, text) per cell, in order.
        let cells: Vec<(bool, String)> = tr
            .children()
            .filter_map(ElementRef::wrap)
            .filter_map(|cell| match cell.value().name() {
                "th" => Some((true, text_of(cell))),
                "td" => Some((false, text_of(cell))),
                _ => None,
            })
            .collect();
        if cells.is_empty() {
            continue;
        }
        let all_th = cells.iter().all(|(th, _)| *th);
        // A leading lone th is the table's title.
        if first && all_th && cells.len() == 1 {
            if lemma.is_none() {
                lemma = Some(cells[0].1.clone());
            } else {
                rows.push(vec![Cell {
                    label: None,
                    text: cells[0].1.clone(),
                }]);
            }
            first = false;
            continue;
        }
        first = false;
        // A th row of two or more cells before any data row is the
        // header row.
        if all_th && cells.len() > 1 && headers.is_none() && rows.is_empty() {
            headers = Some(cells.into_iter().map(|(_, t)| t).collect());
            continue;
        }
        if in_head && headers.is_none() && rows.is_empty() {
            headers = Some(cells.into_iter().map(|(_, t)| t).collect());
            continue;
        }
        // A row-label row: th first, td values follow — the label
        // becomes the first value's lemma (a lone label with no
        // value stands as bare text).
        if cells.len() > 1 && cells[0].0 && cells[1..].iter().all(|(th, _)| !th) {
            let label = &cells[0].1;
            let mut out = Vec::new();
            for (i, (_, t)) in cells[1..].iter().enumerate() {
                if i == 0 && !label.is_empty() && !t.is_empty() {
                    out.push(Cell {
                        label: Some(label.clone()),
                        text: t.clone(),
                    });
                } else if i == 0 && !label.is_empty() {
                    out.push(Cell {
                        label: None,
                        text: label.clone(),
                    });
                } else {
                    out.push(Cell {
                        label: None,
                        text: t.clone(),
                    });
                }
            }
            rows.push(out);
            continue;
        }
        rows.push(
            cells
                .into_iter()
                .map(|(_, t)| Cell {
                    label: None,
                    text: t,
                })
                .collect(),
        );
    }

    Block::Table {
        lemma,
        headers,
        rows,
    }
}

/// Subtree text, whitespace-normalized.
fn text_of(el: ElementRef) -> String {
    quarb_text::normalize_ws(&text_of_raw(el))
}

/// Subtree text as authored (verbatim blocks, inline runs — runs
/// are normalized at flush). Unlike `ElementRef::text`, text under
/// soup descendants is excluded — an inline `<style>` inside a
/// wrapper, an `aria-hidden` tooltip — so flattened prose carries
/// only what the reader sees.
fn text_of_raw(el: ElementRef) -> String {
    let mut out = String::new();
    let mut stack: Vec<NodeRef<DomNode>> = el.children().rev().collect();
    while let Some(node) = stack.pop() {
        if let Some(child) = ElementRef::wrap(node) {
            if SKIP.contains(&child.value().name()) || aria_chrome(child) {
                continue;
            }
            for c in child.children().rev() {
                stack.push(c);
            }
        } else if let DomNode::Text(text) = node.value() {
            out.push_str(text);
        }
    }
    out
}

fn flush(run: &mut String, out: &mut Vec<Block>) {
    if !run.trim().is_empty() {
        out.push(Block::Text { text: run.clone() });
    }
    run.clear();
}
