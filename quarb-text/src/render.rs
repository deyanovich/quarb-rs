//! Render text-level subtrees back into markup — the unparse side
//! of the vocabulary. Because the whole text surface is exposed
//! through the adapter trait (names, `::lemma` / `::hypograph` /
//! `::taxis`, `::::level` / `::::lang`, prose projection), the
//! renderer is generic over `&dyn AstAdapter`: it works through
//! mounts and wrappers, and over any adapter that speaks the
//! vocabulary — an atrep document mounted by `quarb-atrep` renders
//! its shared kinds the same way.
//!
//! Sections emit as *flat* headings (Markdown `##`, HTML `<h2>`),
//! so a rendered document re-parsed by the matching producer
//! derives the same section tree — the round trip is a property,
//! not an accident. A node whose name is outside the vocabulary
//! renders as a paragraph of its prose projection (its `::`), so
//! DOM-level results degrade to readable text instead of failing.

use quarb::{AstAdapter, NodeId, Value};

/// The output markup of a render call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Render {
    Markdown,
    Html,
    Plain,
}

impl Render {
    /// Parse a format name (`md`/`markdown`, `html`, `txt`/`text`/
    /// `plain`).
    pub fn from_name(name: &str) -> Option<Render> {
        match name {
            "md" | "markdown" => Some(Render::Markdown),
            "html" => Some(Render::Html),
            "txt" | "text" | "plain" => Some(Render::Plain),
            _ => None,
        }
    }
}

/// Nesting deeper than this renders as flattened prose rather than
/// recursing further — a guard against pathological container
/// depth, mirroring the producers' iterative walkers.
const MAX_DEPTH: usize = 128;

/// Render value results: one line each for plain text,
/// blank-line-separated paragraphs for Markdown, escaped `<p>`
/// lines for HTML.
pub fn render_values(values: &[Value], kind: Render) -> String {
    let lines: Vec<String> = values.iter().map(|v| v.to_string()).collect();
    let mut out = match kind {
        Render::Plain => lines.join("\n"),
        Render::Markdown => lines.join("\n\n"),
        Render::Html => lines
            .iter()
            .map(|l| format!("<p>{}</p>", escape_html(l)))
            .collect::<Vec<_>>()
            .join("\n"),
    };
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Render each node's subtree in order, blank-line separated.
pub fn render_nodes(a: &dyn AstAdapter, nodes: &[NodeId], kind: Render) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for &n in nodes {
        let s = render_node(a, n, kind);
        if !s.is_empty() {
            blocks.push(s);
        }
    }
    let mut out = blocks.join("\n\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Render one subtree.
pub fn render_node(a: &dyn AstAdapter, node: NodeId, kind: Render) -> String {
    let mut ctx = Ctx { a, kind };
    let blocks = ctx.blocks(node, 1, 0);
    blocks.join("\n\n")
}

struct Ctx<'a> {
    a: &'a dyn AstAdapter,
    kind: Render,
}

fn str_prop(a: &dyn AstAdapter, node: NodeId, name: &str) -> Option<String> {
    match a.property(node, name) {
        Some(Value::Str(s)) if !s.is_empty() => Some(s),
        _ => None,
    }
}

fn int_meta(a: &dyn AstAdapter, node: NodeId, key: &str) -> Option<i64> {
    match a.metadata(node, key)? {
        Value::Int(i) => Some(i),
        _ => None,
    }
}

fn prose(a: &dyn AstAdapter, node: NodeId) -> String {
    match a.default_value(node) {
        Some(Value::Str(s)) => s,
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

/// The node's *own* inline text: its prose projection minus the
/// lemma prefix and the children/hypograph suffix. Exact by
/// construction — the projection is those parts joined with
/// newlines.
fn own_text(a: &dyn AstAdapter, node: NodeId) -> String {
    let full = prose(a, node);
    let mut s = full.as_str();
    if let Some(lemma) = str_prop(a, node, "lemma")
        && let Some(rest) = s.strip_prefix(lemma.as_str())
    {
        s = rest.strip_prefix('\n').unwrap_or(rest);
    }
    let mut tail: Vec<String> = a
        .children(node)
        .into_iter()
        .map(|c| prose(a, c))
        .filter(|p| !p.is_empty())
        .collect();
    if let Some(h) = str_prop(a, node, "hypograph") {
        tail.push(h);
    }
    let tail = tail.join("\n");
    if !tail.is_empty()
        && let Some(rest) = s.strip_suffix(tail.as_str())
    {
        s = rest.strip_suffix('\n').unwrap_or(rest);
    }
    s.to_string()
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Prefix every line of `body` with `first` (first line) and `rest`
/// (continuation lines).
fn prefix_lines(body: &str, first: &str, rest: &str) -> String {
    let mut out = String::new();
    for (i, line) in body.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let pre = if i == 0 { first } else { rest };
        let composed = format!("{pre}{line}");
        out.push_str(composed.trim_end());
    }
    if body.is_empty() {
        out.push_str(first.trim_end());
    }
    out
}

impl Ctx<'_> {
    /// The rendered block sequence of `node`'s subtree. `level` is
    /// the heading level the next derived section takes when it
    /// carries no `::::level`; `depth` guards recursion.
    fn blocks(&mut self, node: NodeId, level: usize, depth: usize) -> Vec<String> {
        if depth > MAX_DEPTH {
            let p = prose(self.a, node);
            return if p.is_empty() { vec![] } else { vec![self.para(&p)] };
        }
        let name = self.a.name(node);
        match name.as_deref() {
            None => self.child_blocks(node, level, depth),
            Some("section") => self.section(node, level, depth),
            Some("paragraph") => {
                let p = prose(self.a, node);
                if p.is_empty() { vec![] } else { vec![self.para(&p)] }
            }
            Some("blockquote") => vec![self.blockquote(node, depth)],
            // A list's lemma (a denormalized table's caption) has no
            // list-level home in the output markups: it renders as a
            // caption paragraph before the list, keeping the prose
            // identical.
            Some("unordered-list") => {
                let list = self.list(node, false, depth);
                self.captioned(node, list)
            }
            Some("ordered-list") => {
                let list = self.list(node, true, depth);
                self.captioned(node, list)
            }
            Some("unordered-item") | Some("ordered-item") => {
                // An item reached directly (outside its list):
                // render its content sequence.
                self.item_blocks(node, depth)
            }
            Some("verbatim") => vec![self.verbatim(node)],
            // Outside the vocabulary: a paragraph of its prose.
            Some(_) => {
                let p = prose(self.a, node);
                if p.is_empty() { vec![] } else { vec![self.para(&p)] }
            }
        }
    }

    fn captioned(&self, node: NodeId, list: String) -> Vec<String> {
        match str_prop(self.a, node, "lemma") {
            Some(lemma) => vec![self.para(&lemma), list],
            None => vec![list],
        }
    }

    /// A paragraph block in the output markup.
    fn para(&self, p: &str) -> String {
        match self.kind {
            Render::Html => format!("<p>{}</p>", escape_html(p)),
            Render::Markdown | Render::Plain => p.to_string(),
        }
    }

    fn child_blocks(&mut self, node: NodeId, level: usize, depth: usize) -> Vec<String> {
        let mut out = Vec::new();
        for c in self.a.children(node) {
            out.extend(self.blocks(c, level, depth + 1));
        }
        out
    }

    fn section(&mut self, node: NodeId, level: usize, depth: usize) -> Vec<String> {
        let lemma = str_prop(self.a, node, "lemma").unwrap_or_default();
        let level = int_meta(self.a, node, "level")
            .map(|l| l.clamp(1, 6) as usize)
            .unwrap_or(level.min(6));
        let heading = match self.kind {
            Render::Markdown => format!("{} {}", "#".repeat(level), lemma),
            Render::Html => format!("<h{level}>{}</h{level}>", escape_html(&lemma)),
            Render::Plain => lemma.clone(),
        };
        let mut out = vec![heading];
        out.extend(self.child_blocks(node, level + 1, depth));
        out
    }

    fn blockquote(&mut self, node: NodeId, depth: usize) -> String {
        let mut inner = self.child_blocks(node, 1, depth + 1);
        let own = own_text(self.a, node);
        if !own.is_empty() {
            inner.insert(0, own);
        }
        let hypograph = str_prop(self.a, node, "hypograph");
        match self.kind {
            Render::Markdown => {
                let mut body = inner.join("\n\n");
                if let Some(h) = &hypograph {
                    if !body.is_empty() {
                        body.push_str("\n\n");
                    }
                    body.push_str(&attribution(h));
                }
                prefix_lines(&body, "> ", "> ")
            }
            Render::Html => {
                let mut out = String::from("<blockquote>\n");
                for b in &inner {
                    out.push_str(b);
                    out.push('\n');
                }
                if let Some(h) = &hypograph {
                    out.push_str(&format!("<footer>{}</footer>\n", escape_html(h)));
                }
                out.push_str("</blockquote>");
                out
            }
            Render::Plain => {
                let mut body = inner.join("\n\n");
                if let Some(h) = &hypograph {
                    if !body.is_empty() {
                        body.push('\n');
                    }
                    body.push_str(&attribution(h));
                }
                body
            }
        }
    }

    fn list(&mut self, node: NodeId, ordered: bool, depth: usize) -> String {
        let items: Vec<NodeId> = self.a.children(node);
        match self.kind {
            Render::Html => {
                let start = items
                    .first()
                    .and_then(|&i| match self.a.property(i, "taxis") {
                        Some(Value::Int(t)) => Some(t),
                        _ => None,
                    })
                    .unwrap_or(1);
                let tag = if ordered { "ol" } else { "ul" };
                let mut out = if ordered && start != 1 {
                    format!("<ol start=\"{start}\">\n")
                } else {
                    format!("<{tag}>\n")
                };
                for &item in &items {
                    let mut inner = self.child_blocks(item, 1, depth + 2);
                    let own = own_text(self.a, item);
                    if !own.is_empty() {
                        inner.insert(0, escape_html(&own));
                    }
                    out.push_str("<li>");
                    out.push_str(&inner.join("\n"));
                    out.push_str("</li>\n");
                }
                out.push_str(&format!("</{tag}>"));
                out
            }
            Render::Markdown | Render::Plain => {
                let mut lines = Vec::new();
                for (i, &item) in items.iter().enumerate() {
                    let marker = if ordered {
                        let taxis = match self.a.property(item, "taxis") {
                            Some(Value::Int(t)) => t,
                            _ => i as i64 + 1,
                        };
                        format!("{taxis}. ")
                    } else {
                        "- ".to_string()
                    };
                    let indent = " ".repeat(marker.len());
                    let content = join_item_content(&self.item_blocks(item, depth + 1));
                    lines.push(prefix_lines(&content, &marker, &indent));
                }
                lines.join("\n")
            }
        }
    }

    /// An item's content sequence: its own text first, then its
    /// child blocks.
    fn item_blocks(&mut self, item: NodeId, depth: usize) -> Vec<String> {
        let mut out = Vec::new();
        let own = own_text(self.a, item);
        if !own.is_empty() {
            out.push(if self.kind == Render::Html {
                escape_html(&own)
            } else {
                own
            });
        }
        out.extend(self.child_blocks(item, 1, depth + 1));
        out
    }

    fn verbatim(&mut self, node: NodeId) -> String {
        let text = prose(self.a, node);
        let lang = match self.a.metadata(node, "lang") {
            Some(Value::Str(l)) => l,
            _ => String::new(),
        };
        match self.kind {
            Render::Markdown => format!("```{lang}\n{text}\n```"),
            Render::Html => {
                let class = if lang.is_empty() {
                    String::new()
                } else {
                    format!(" class=\"language-{lang}\"")
                };
                format!("<pre><code{class}>{}</code></pre>", escape_html(&text))
            }
            Render::Plain => text,
        }
    }
}

/// Join an item's content blocks: a nested list follows its item
/// text directly (a blank line would loosen the list on re-parse
/// and break the round trip); anything else gets the normal blank
/// line.
fn join_item_content(blocks: &[String]) -> String {
    let mut out = String::new();
    for (i, b) in blocks.iter().enumerate() {
        if i > 0 {
            out.push_str(if starts_list_marker(b) { "\n" } else { "\n\n" });
        }
        out.push_str(b);
    }
    out
}

fn starts_list_marker(b: &str) -> bool {
    if b.starts_with("- ") {
        return true;
    }
    let digits = b.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0 && b[digits..].starts_with(". ")
}

/// An attribution line: an em-dash prefix unless the text already
/// leads with a dash.
fn attribution(h: &str) -> String {
    if h.starts_with('—') || h.starts_with('–') || h.starts_with('-') {
        h.to_string()
    } else {
        format!("— {h}")
    }
}
