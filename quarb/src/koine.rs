//! The koine renderer: serialize a subtree that speaks the
//! text-level vocabulary (`section`, `paragraph`, `blockquote`,
//! lists, `verbatim`, with `::lemma`/`::taxis`/`::hypograph` as
//! universal affordances) into Markdown, HTML, plain text, or
//! litogramma (atrep's markup). Adapter-generic: it reads names
//! and properties through the trait, so it renders `text:`
//! mounts, atrep documents, and anything else emitting the
//! vocabulary; kinds outside it degrade to a paragraph of their
//! prose. Powers the `| markdown` / `| html` / `| atrep`
//! pipeline stages, the extension's export buttons, and the
//! notebook renderers.

use crate::adapter::AstAdapter;
use crate::{NodeId, Value};

/// The output markup of a render call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Render {
    Markdown,
    Html,
    Plain,
    /// litogramma (atrep's markup), full standard dialect — the
    /// emitted subset is the koine by construction. Section depth
    /// is normalized relative to the rendered root (litogramma
    /// enforces real nesting); prose lines that would open a sim
    /// are a known escaping seam, recorded in the spec.
    Atrep,
    /// CoNLL-U (ruling #63): the tokens of the rendered subtree as
    /// sentence blocks — the corpus reading's round trip to the
    /// treebank tools. Prose without tokens renders nothing.
    Conllu,
}

impl Render {
    /// Parse a format name (`md`/`markdown`, `html`, `txt`/`text`/
    /// `plain`, `atrep`, `conllu`).
    pub fn from_name(name: &str) -> Option<Render> {
        match name {
            "md" | "markdown" => Some(Render::Markdown),
            "html" => Some(Render::Html),
            "txt" | "text" | "plain" => Some(Render::Plain),
            "atrep" | "atd" => Some(Render::Atrep),
            "conllu" => Some(Render::Conllu),
            _ => None,
        }
    }
}

/// The CoNLL-U rendering of the tokens under `roots`, in document
/// order, one block per sentence (consecutive tokens sharing
/// `::::sentence`): `# sent_id` from a sentence node's `::id`,
/// `# text` from the tokens' `::sentence`, then the ten columns —
/// `::id` (else the position), the form, `::lemma`, `::upos`,
/// `::xpos`, `::feats`, the `->head` target's id (`0` for a root
/// that carries a relation, `_` for an unannotated token),
/// `::deprel`, `::deps`, `::misc` — with a multiword range line
/// (`::::mwt`, `::mwt`) before the first of its words.
/// Adapter-generic: any arbor speaking the corpus vocabulary.
fn render_conllu(a: &dyn AstAdapter, roots: &[NodeId]) -> String {
    fn collect(a: &dyn AstAdapter, node: NodeId, out: &mut Vec<NodeId>, depth: usize) {
        if a.name(node).as_deref() == Some("token") {
            out.push(node);
            return;
        }
        if depth > MAX_DEPTH {
            return;
        }
        for c in a.children(node) {
            collect(a, c, out, depth + 1);
        }
    }
    let mut tokens = Vec::new();
    for &r in roots {
        collect(a, r, &mut tokens, 0);
    }
    let col = |t: NodeId, name: &str| str_prop(a, t, name).unwrap_or_else(|| "_".to_string());
    let mut out = String::new();
    let mut i = 0;
    while i < tokens.len() {
        let s = int_meta(a, tokens[i], "sentence");
        let mut j = i;
        while j < tokens.len() && int_meta(a, tokens[j], "sentence") == s {
            j += 1;
        }
        let group = &tokens[i..j];
        i = j;
        if !out.is_empty() {
            out.push('\n');
        }
        if let Some(p) = a.parent(group[0])
            && a.name(p).as_deref() == Some("sentence")
            && let Some(id) = str_prop(a, p, "id")
        {
            out.push_str(&format!("# sent_id = {id}\n"));
        }
        if let Some(text) = str_prop(a, group[0], "sentence") {
            out.push_str(&format!("# text = {text}\n"));
        }
        // A token's id: the annotation's, else its position among
        // the sentence's tokens.
        let id_of = |t: NodeId| -> String {
            str_prop(a, t, "id").unwrap_or_else(|| {
                let pos = group.iter().position(|&g| g == t).unwrap_or(0) + 1;
                pos.to_string()
            })
        };
        let mut last_range: Option<String> = None;
        for &t in group {
            match a.metadata(t, "mwt") {
                Some(Value::Str(rid)) => {
                    if last_range.as_deref() != Some(rid.as_str()) {
                        let form = str_prop(a, t, "mwt").unwrap_or_default();
                        out.push_str(&format!("{rid}\t{form}\t_\t_\t_\t_\t_\t_\t_\t_\n"));
                        last_range = Some(rid);
                    }
                }
                _ => last_range = None,
            }
            let form = prose(a, t);
            let deprel = col(t, "deprel");
            let head = a
                .links(t)
                .into_iter()
                .find(|(label, _)| label == "head")
                .map(|(_, h)| id_of(h))
                .unwrap_or_else(|| {
                    if deprel == "_" {
                        "_".to_string()
                    } else {
                        "0".to_string()
                    }
                });
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                id_of(t),
                form,
                col(t, "lemma"),
                col(t, "upos"),
                col(t, "xpos"),
                col(t, "feats"),
                head,
                deprel,
                col(t, "deps"),
                col(t, "misc"),
            ));
        }
    }
    out
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
        Render::Plain | Render::Conllu => lines.join("\n"),
        Render::Markdown => lines.join("\n\n"),
        Render::Atrep => {
            let body = lines.join("\n\n");
            if body.is_empty() {
                body
            } else {
                format!("@@@!litogramma\n\n{body}")
            }
        }
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
    if kind == Render::Conllu {
        return render_conllu(a, nodes);
    }
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

/// Render one subtree. For litogramma the document opens with
/// its dialektos declaration, so every rendered subtree is a
/// valid `.atd` from byte one.
pub fn render_node(a: &dyn AstAdapter, node: NodeId, kind: Render) -> String {
    if kind == Render::Conllu {
        return render_conllu(a, &[node]);
    }
    let mut ctx = Ctx {
        a,
        kind,
        atrep_depth: 0,
    };
    let blocks = ctx.blocks(node, 1, 0);
    let body = blocks.join("\n\n");
    match kind {
        Render::Atrep if !body.is_empty() => {
            format!("@@@!litogramma\n\n{body}")
        }
        _ => body,
    }
}

struct Ctx<'a> {
    a: &'a dyn AstAdapter,
    kind: Render,
    /// Section nesting within this render — litogramma's marker
    /// count (`@#`, `@##`, …), relative to the rendered root.
    atrep_depth: usize,
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
/// newlines. An *item's* lemma is inline (`lemma: text` — a table
/// cell, a definition) and stays in the emitted text; only the
/// block-shaped lemma of a section or list is stripped here.
fn own_text(a: &dyn AstAdapter, node: NodeId) -> String {
    let full = prose(a, node);
    let mut s = full.as_str();
    let inline_lemma = matches!(
        a.name(node).as_deref(),
        Some("unordered-item") | Some("ordered-item")
    );
    if !inline_lemma
        && let Some(lemma) = str_prop(a, node, "lemma")
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
    s.trim_end().to_string()
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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
            return if p.is_empty() {
                vec![]
            } else {
                vec![self.para(&p)]
            };
        }
        let name = self.a.name(node);
        match name.as_deref() {
            None => self.child_blocks(node, level, depth),
            Some("section") => self.section(node, level, depth),
            Some("paragraph") => {
                let p = prose(self.a, node);
                if p.is_empty() {
                    vec![]
                } else {
                    vec![self.para(&p)]
                }
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
            // The corpus reading's tokens are annotation, already
            // in their block's prose: nothing of their own to
            // render.
            Some("token") => vec![],
            // Outside the vocabulary: a paragraph of its prose.
            Some(_) => {
                let p = prose(self.a, node);
                if p.is_empty() {
                    vec![]
                } else {
                    vec![self.para(&p)]
                }
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
            Render::Markdown | Render::Plain | Render::Atrep | Render::Conllu => p.to_string(),
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
        if self.kind == Render::Atrep {
            self.atrep_depth += 1;
            let marks = "#".repeat(self.atrep_depth);
            let mut out = vec![format!("@{marks} {lemma}")];
            out.extend(self.child_blocks(node, level + 1, depth));
            out.push(format!("{marks}@"));
            self.atrep_depth -= 1;
            return out;
        }
        let heading = match self.kind {
            Render::Markdown => format!("{} {}", "#".repeat(level), lemma),
            Render::Html => format!("<h{level}>{}</h{level}>", escape_html(&lemma)),
            Render::Plain | Render::Conllu => lemma.clone(),
            Render::Atrep => unreachable!(),
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
            Render::Atrep => {
                let body = inner.join("\n\n");
                match &hypograph {
                    Some(h) => format!("@\"/\n{body}\n/\"@ {h}"),
                    None => format!("@\"\n{body}\n\"@"),
                }
            }
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
            Render::Plain | Render::Conllu => {
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
        if self.kind == Render::Atrep {
            return self.list_atrep(node, ordered, depth, &items);
        }
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
            Render::Markdown | Render::Plain | Render::Atrep | Render::Conllu => {
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

    /// litogramma lists: `@.. @.-(n) … -.@ ..@` ordered,
    /// `@-- @- … -@ --@` unordered — and when every item carries
    /// a `::lemma` (a definition list, a table row's cells), the
    /// definition form `@::; @:: lemma @; … ;@ ::@ … ;::@`, the
    /// lemma emitted as the term rather than folded into prose.
    fn list_atrep(
        &mut self,
        _node: NodeId,
        ordered: bool,
        depth: usize,
        items: &[NodeId],
    ) -> String {
        let all_lemmad = !items.is_empty()
            && items
                .iter()
                .all(|&i| str_prop(self.a, i, "lemma").is_some());
        if all_lemmad && !ordered {
            let mut out = String::from("@::;");
            for &item in items {
                let lemma = str_prop(self.a, item, "lemma").unwrap_or_default();
                let body = self.item_body_atrep(item, &lemma, depth);
                out.push_str(&format!(
                    "\n@:: {lemma}\n@;\n{}\n;@\n::@",
                    if body.is_empty() { "\u{2014}" } else { &body }
                ));
            }
            out.push_str("\n;::@");
            return out;
        }
        let (open, iopen, iclose, close) = if ordered {
            ("@..", "@.-", "-.@", "..@")
        } else {
            ("@--", "@-", "-@", "--@")
        };
        let mut out = String::from(open);
        for (i, &item) in items.iter().enumerate() {
            let marker = if ordered {
                let taxis = match self.a.property(item, "taxis") {
                    Some(Value::Int(t)) => t,
                    _ => i as i64 + 1,
                };
                format!("{iopen}({taxis})")
            } else {
                iopen.to_string()
            };
            let content = join_item_content(&self.item_blocks(item, depth + 1));
            out.push_str(&format!("\n{marker}\n{content}\n{iclose}"));
        }
        out.push_str(&format!("\n{close}"));
        out
    }

    /// A lemma'd item's content with the lemma lifted off — the
    /// definition form spells the term separately, so the inline
    /// `lemma: ` fold comes back off the prose.
    fn item_body_atrep(&mut self, item: NodeId, lemma: &str, depth: usize) -> String {
        let blocks = self.item_blocks(item, depth + 1);
        let joined = join_item_content(&blocks);
        let prefix = format!("{lemma}: ");
        match joined.strip_prefix(&prefix) {
            Some(rest) => rest.to_string(),
            None => joined,
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
            Render::Atrep => {
                let genos = if lang.is_empty() {
                    String::new()
                } else {
                    format!(".{lang}")
                };
                format!("@@@\"\n{text}\n\"@@@{genos}")
            }
            Render::Markdown => format!("```{lang}\n{text}\n```"),
            Render::Html => {
                let class = if lang.is_empty() {
                    String::new()
                } else {
                    format!(" class=\"language-{lang}\"")
                };
                format!("<pre><code{class}>{}</code></pre>", escape_html(&text))
            }
            Render::Plain | Render::Conllu => text,
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
