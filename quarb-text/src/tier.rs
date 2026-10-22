//! The corpus reading's tier on plain prose: sentences and tokens no
//! node stores. A block's sentences and tokens are computed from its
//! prose when a query first reaches them (the segmenter and the
//! sentence bonds in force), their ids minted by arithmetic from
//! the block and an ordinal, their forms slices of the prose. The
//! engine sees a `sentence` node per span under the block and a
//! `token` node per word under the sentence, as the stored tier
//! gave it; nothing is kept per token beyond a 16-byte record in a
//! table built per block on demand. Annotated sources — a CoNLL-U
//! document, a parsing pack that declares its tokens — keep stored
//! token nodes, since the token is the source's own row.

use super::{Kind, TextModel, TokenClass, class_of, is_quote, sentence_spans};
use quarb::{NodeId, Value};
use std::rc::Rc;

/// A virtual id: bit 55 set (the mount table owns the top byte).
const V: u64 = 1 << 55;
/// A token's id (else a sentence's).
const TK: u64 = 1 << 54;
/// The ordinal's bits: a block holds at most this many tokens.
const ORD: u32 = 20;
const ORD_MASK: u64 = (1 << ORD) - 1;
const BLOCK_MASK: u64 = (1 << 34) - 1;
/// Tokens a block's table holds at most; the rest of a block past
/// it are not tokenized (a paragraph of a million words).
pub(super) const MAX_PER_BLOCK: usize = ORD_MASK as usize;

/// What a virtual id names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Virt {
    Sentence {
        block: usize,
        k: u32,
    },
    Token {
        block: usize,
        k: u32,
    },
    /// An annotated token's row ([`super::ann`]).
    Ann {
        row: u32,
    },
}

pub(super) fn virt(id: NodeId) -> Option<Virt> {
    if id.0 & V == 0 {
        return None;
    }
    if let Some(row) = super::ann::ann_row(id) {
        return Some(Virt::Ann { row });
    }
    let block = ((id.0 >> ORD) & BLOCK_MASK) as usize;
    let k = (id.0 & ORD_MASK) as u32;
    Some(if id.0 & TK != 0 {
        Virt::Token { block, k }
    } else {
        Virt::Sentence { block, k }
    })
}

pub(super) fn sentence_id(block: usize, k: u32) -> NodeId {
    NodeId(V | ((block as u64) << ORD) | k as u64)
}

pub(super) fn token_id(block: usize, k: u32) -> NodeId {
    NodeId(V | TK | ((block as u64) << ORD) | k as u64)
}

/// One token of a block: its byte range in the block's prose, its
/// sentence (an index into the block's spans), its class.
#[derive(Clone, Copy, Debug)]
pub(super) struct Tok {
    pub at: u32,
    pub end: u32,
    pub sent: u32,
    pub class: TokenClass,
}

/// A block's tier: the sentence spans, the tokens in order, and
/// the first token of each sentence (one more entry closes the
/// last).
pub(super) struct Tier {
    pub sents: Vec<(u32, u32)>,
    pub toks: Vec<Tok>,
    pub first: Vec<u32>,
}

impl Tier {
    /// The segmentation the stored tier used: sentence spans from
    /// the bonds or UAX #29, tokens from the word bounds, whitespace
    /// dropped, each token in the sentence its start falls in (the
    /// last sentence past them all).
    pub(super) fn build(prose: &str, bonds: Option<&syndesmos::Syndesmos>) -> Tier {
        Tier::over(prose, sentence_spans(prose, bonds))
    }

    /// A given segmentation (a `.sentences` file's lines aligned to
    /// the block): the spans as given, the tokens UAX #29's.
    pub(super) fn given(prose: &str, spans: &[(u32, u32)]) -> Tier {
        Tier::over(
            prose,
            spans
                .iter()
                .map(|&(a, b)| (a as usize, b as usize))
                .collect(),
        )
    }

    fn over(prose: &str, mut spans: Vec<(usize, usize)>) -> Tier {
        use unicode_segmentation::UnicodeSegmentation;
        if spans.is_empty() {
            spans.push((0, prose.len()));
        }
        let sents: Vec<(u32, u32)> = spans.iter().map(|&(a, b)| (a as u32, b as u32)).collect();
        let mut toks = Vec::new();
        let mut s = 0usize;
        for (at, seg) in prose.split_word_bound_indices() {
            if seg.chars().all(char::is_whitespace) {
                continue;
            }
            while s + 1 < spans.len() && at >= spans[s].1 {
                s += 1;
            }
            if toks.len() >= MAX_PER_BLOCK {
                break;
            }
            toks.push(Tok {
                at: at as u32,
                end: (at + seg.len()) as u32,
                sent: s as u32,
                class: class_of(seg),
            });
        }
        let mut first = vec![0u32; sents.len() + 1];
        let mut k = 0usize;
        for (i, t) in toks.iter().enumerate() {
            while k <= t.sent as usize {
                first[k] = i as u32;
                k += 1;
            }
        }
        while k <= sents.len() {
            first[k] = toks.len() as u32;
            k += 1;
        }
        Tier { sents, toks, first }
    }

    /// The token ordinals of sentence `k`.
    pub(super) fn tokens_of(&self, k: u32) -> std::ops::Range<u32> {
        let k = k as usize;
        if k + 1 >= self.first.len() {
            return 0..0;
        }
        self.first[k]..self.first[k + 1]
    }
}

impl TextModel {
    /// Whether `block` is a prose block whose tier is virtual: the
    /// corpus reading is on and nothing in the document declared
    /// its tokens.
    pub(super) fn virtual_block(&self, block: usize) -> bool {
        self.tiered
            && block < self.nodes.len()
            && (self.is_prose_block(block) || self.heading_text(block).is_some())
            // `?notes=omit`: a note's body is left untokenized, as
            // the stored tier left it.
            && !(self.notes_omitted && self.in_body(NodeId(block as u64)).is_some())
    }

    /// A title or heading is a sentence too (the ruling of
    /// 2026-10-09: so the computed tier and a given one agree, and
    /// match the corpora that number headings): the document's
    /// title and every section's heading get one sentence under
    /// the node, before its content. The text the tier is built
    /// from, for such a node.
    pub(super) fn heading_text(&self, block: usize) -> Option<&str> {
        let n = self.nodes.get(block)?;
        match n.kind {
            Kind::Section => n.lemma.as_deref().filter(|l| !l.is_empty()),
            Kind::Document => self.head.title.as_deref().filter(|t| !t.is_empty()),
            _ => None,
        }
    }

    /// The text a block's tier is built from: a heading's own text
    /// on a section or the titled document, else the block's prose.
    pub(super) fn tier_text(&self, block: usize) -> &str {
        self.heading_text(block)
            .unwrap_or_else(|| &self.nodes[block].prose)
    }

    /// The tier's blocks in reading order: the prose blocks as
    /// [`TextModel::prose_blocks`] orders them, each heading before
    /// the first block that follows it in the document.
    pub(super) fn tier_blocks(&self) -> Vec<usize> {
        let mut headings: Vec<usize> = (0..self.nodes.len())
            .filter(|&i| self.heading_text(i).is_some())
            .collect();
        headings.reverse();
        let mut out = Vec::new();
        for b in self.prose_blocks() {
            let block = b.0 as usize;
            while let Some(&h) = headings.last()
                && h < block
            {
                out.push(h);
                headings.pop();
            }
            out.push(block);
        }
        while let Some(h) = headings.pop() {
            out.push(h);
        }
        out
    }

    /// The block's tier, built on first reach: from the given
    /// segmentation when one is mounted, else computed.
    pub(super) fn tier(&self, block: usize) -> Rc<Tier> {
        if let Some(t) = self.tiers.borrow().get(&block) {
            return t.clone();
        }
        let text = self.tier_text(block);
        let t = Rc::new(match self.given.get(&block) {
            Some(spans) => Tier::given(text, spans),
            // A heading is one sentence whatever its punctuation
            // ("CHAPTER I. THE BEGINNING"), as on the stored tiers.
            None if self.heading_text(block).is_some() => {
                let lo = text.len() - text.trim_start().len();
                let hi = text.trim_end().len();
                Tier::given(text, &[(lo as u32, hi as u32)])
            }
            None => Tier::build(text, self.bonds.as_ref()),
        });
        self.tiers.borrow_mut().insert(block, t.clone());
        t
    }

    /// A `.sentences` file's lines, aligned to the text: one line
    /// per sentence in reading order (blank lines ignored, headings
    /// as lines), each matched whitespace-insensitively at the
    /// current position; the first line that does not match is
    /// refused by its number naming the text found, and the first
    /// block the file leaves uncovered by its path. Line k is then
    /// the sentence with `:::n` k.
    pub fn tokenize_given(&mut self, sentences: &str) -> Result<(), String> {
        self.mutated();
        if self.is_tokenized() && !self.tiered {
            return Err("the document is already tokenized".into());
        }
        let lines: Vec<(usize, &str)> = sentences
            .lines()
            .enumerate()
            .map(|(i, l)| (i + 1, l.trim()))
            .filter(|(_, l)| !l.is_empty())
            .collect();
        let mut given: std::collections::HashMap<usize, Vec<(u32, u32)>> = Default::default();
        let mut pos = 0usize;
        self.tiered = true;
        self.bonds = None;
        for block in self.tier_blocks() {
            let text = self.tier_text(block).to_string();
            let mut spans = Vec::new();
            let mut cursor = 0usize;
            loop {
                // Past the whitespace at the cursor.
                while let Some(c) = text[cursor..].chars().next()
                    && c.is_whitespace()
                {
                    cursor += c.len_utf8();
                }
                if cursor >= text.len() {
                    break;
                }
                let Some(&(lineno, line)) = lines.get(pos) else {
                    return Err(format!(
                        "the sentences file ends before {} is covered (at {:?})",
                        self.locator(NodeId(block as u64)),
                        text[cursor..].chars().take(24).collect::<String>()
                    ));
                };
                match match_line(&text, cursor, line) {
                    Some(end) => {
                        spans.push((cursor as u32, end as u32));
                        cursor = end;
                        pos += 1;
                    }
                    None => {
                        return Err(format!(
                            "sentences line {lineno} does not match the text at {} ({:?}): the text reads {:?}",
                            self.locator(NodeId(block as u64)),
                            line.chars().take(24).collect::<String>(),
                            text[cursor..].chars().take(24).collect::<String>()
                        ));
                    }
                }
            }
            given.insert(block, spans);
        }
        if let Some(&(lineno, line)) = lines.get(pos) {
            return Err(format!(
                "sentences line {lineno} ({:?}) has no text left to match",
                line.chars().take(24).collect::<String>()
            ));
        }
        self.given = given;
        self.tiers.borrow_mut().clear();
        self.token_index = Default::default();
        self.attribute_spans();
        Ok(())
    }

    /// The virtual sentences under a block, in order.
    pub(super) fn tier_sentences(&self, block: usize) -> Vec<NodeId> {
        let t = self.tier(block);
        (0..t.sents.len() as u32)
            .map(|k| sentence_id(block, k))
            .collect()
    }

    /// Every virtual token of the document in reading order, with
    /// its form and its sentence: the cast and places matching
    /// walk it as they walked the stored tokens.
    pub(super) fn tier_words(&self) -> Vec<(NodeId, String, NodeId)> {
        let mut out = Vec::new();
        for block in self.tier_blocks() {
            let t = self.tier(block);
            for (k, tok) in t.toks.iter().enumerate() {
                if tok.class != TokenClass::Word {
                    continue;
                }
                out.push((
                    token_id(block, k as u32),
                    self.form(block, tok).to_string(),
                    sentence_id(block, tok.sent),
                ));
            }
        }
        out
    }

    /// The document-wide ordinals: for each prose block in reading
    /// order, the count of tokens and of sentences before it. One
    /// pass over the prose, building every tier.
    fn ords(&self) -> &std::collections::HashMap<usize, (u32, u32)> {
        self.ords.get_or_init(|| {
            let mut map = std::collections::HashMap::new();
            let (mut n, mut s) = (0u32, 0u32);
            for block in self.tier_blocks() {
                map.insert(block, (n, s));
                let t = self.tier(block);
                n += t.toks.len() as u32;
                s += t.sents.len() as u32;
            }
            map
        })
    }

    pub(super) fn form(&self, block: usize, t: &Tok) -> &str {
        &self.tier_text(block)[t.at as usize..t.end as usize]
    }

    fn sentence_text(&self, block: usize, k: u32) -> Option<String> {
        let t = self.tier(block);
        let (lo, hi) = *t.sents.get(k as usize)?;
        self.tier_text(block)
            .get(lo as usize..hi as usize)
            .map(|s| s.trim().to_string())
    }

    /// A byte offset in the block's text as a character offset.
    pub(super) fn chars_to(&self, block: usize, byte: u32) -> i64 {
        self.tier_text(block)
            .get(..byte as usize)
            .map_or(0, |s| s.chars().count() as i64)
    }

    fn tok(&self, block: usize, k: u32) -> Option<Tok> {
        self.tier(block).toks.get(k as usize).copied()
    }

    /// A node's parent, stored or virtual.
    pub(super) fn any_parent(&self, id: NodeId) -> Option<NodeId> {
        match virt(id) {
            Some(v) => self.v_parent(v),
            None => self.nodes[id.0 as usize].parent,
        }
    }

    /// The locator of a virtual node: the block's, then the
    /// sentence and the token by their ordinals among their
    /// same-named siblings (indexed only where there are several).
    pub(super) fn v_locator(&self, v: Virt) -> String {
        let (block, k) = match v {
            Virt::Sentence { block, k } | Virt::Token { block, k } => (block, k),
            Virt::Ann { row } => return self.a_locator(row),
        };
        let t = self.tier(block);
        let base = self.locator(NodeId(block as u64));
        let seg = |name: &str, i: usize, n: usize| {
            if n > 1 {
                format!("{name}[{}]", i + 1)
            } else {
                name.to_string()
            }
        };
        match v {
            Virt::Ann { row } => self.a_locator(row),
            Virt::Sentence { .. } => {
                format!("{base}/{}", seg("sentence", k as usize, t.sents.len()))
            }
            Virt::Token { .. } => {
                let Some(tok) = t.toks.get(k as usize) else {
                    return base;
                };
                let range = t.tokens_of(tok.sent);
                format!(
                    "{base}/{}/{}",
                    seg("sentence", tok.sent as usize, t.sents.len()),
                    seg("token", (k - range.start) as usize, range.len())
                )
            }
        }
    }

    pub(super) fn v_children(&self, v: Virt) -> Vec<NodeId> {
        match v {
            Virt::Ann { .. } => Vec::new(),
            Virt::Sentence { block, k } => {
                let t = self.tier(block);
                t.tokens_of(k).map(|i| token_id(block, i)).collect()
            }
            Virt::Token { .. } => Vec::new(),
        }
    }

    pub(super) fn v_parent(&self, v: Virt) -> Option<NodeId> {
        match v {
            Virt::Ann { row } => self.a_parent(row),
            Virt::Sentence { block, .. } => Some(NodeId(block as u64)),
            Virt::Token { block, k } => self.tok(block, k).map(|t| sentence_id(block, t.sent)),
        }
    }

    pub(super) fn v_name(v: Virt) -> Option<String> {
        Some(
            match v {
                Virt::Sentence { .. } => "sentence",
                Virt::Token { .. } | Virt::Ann { .. } => "token",
            }
            .to_string(),
        )
    }

    pub(super) fn v_traits(&self, v: Virt) -> Vec<String> {
        match v {
            Virt::Ann { row } => self.a_traits(row),
            // the kind first (spec: Traits): a sentence, a token,
            // then the class
            Virt::Sentence { .. } => vec!["sentence".to_string(), "derived".to_string()],
            Virt::Token { block, k } => {
                let Some(t) = self.tok(block, k) else {
                    return vec!["token".to_string()];
                };
                let mut out = vec!["token".to_string(), t.class.name().to_string()];
                if t.class == TokenClass::Punct && is_quote(self.form(block, &t)) {
                    out.push("quote".to_string());
                }
                out
            }
        }
    }

    pub(super) fn v_default_value(&self, v: Virt) -> Option<Value> {
        match v {
            Virt::Ann { row } => self.a_default_value(row),
            Virt::Sentence { block, k } => self.sentence_text(block, k).map(Value::Str),
            Virt::Token { block, k } => {
                let t = self.tok(block, k)?;
                Some(Value::Str(self.form(block, &t).to_string()))
            }
        }
    }

    /// The citation at a virtual node: the block's, at the token's
    /// byte offset (a sentence's first token's).
    fn v_cite(&self, v: Virt) -> Option<String> {
        let (block, at) = match v {
            Virt::Token { block, k } => (block, self.tok(block, k)?.at),
            Virt::Sentence { block, k } => {
                let t = self.tier(block);
                let at = t
                    .tokens_of(k)
                    .next()
                    .and_then(|i| t.toks.get(i as usize))
                    .map(|t| t.at)
                    .unwrap_or(0);
                (block, at)
            }
            Virt::Ann { row } => return self.a_cite_row(row),
        };
        self.cite_in_block(block, at)
    }

    /// The citation at byte `at` of `block`: the structural one
    /// when no milestone is declared, a note body's as its
    /// callout, else the milestone in force.
    pub(super) fn cite_in_block(&self, block: usize, at: u32) -> Option<String> {
        let b = NodeId(block as u64);
        let caches = self.caches();
        if caches.cite_table.is_empty() {
            return self.structural_cite(b);
        }
        // A note body, and everything in it, cites as its callout.
        let n = &self.nodes[block];
        let body = if matches!(n.kind, Kind::Footnote | Kind::Endnote | Kind::Aside) && !n.deixis {
            Some(b)
        } else {
            self.note_body_of(b)
        };
        if let Some(body) = body {
            let callout = caches.callout_of_body.get(&body).copied();
            return callout.and_then(|c| self.cite(c));
        }
        self.cite_at(b, at, b)
    }

    /// The sentence's document-wide ordinal.
    fn v_sentence_ordinal(&self, block: usize, k: u32) -> Option<i64> {
        let (_, s) = *self.ords().get(&block)?;
        Some(s as i64 + k as i64 + 1)
    }

    pub(super) fn v_property(&self, v: Virt, name: &str) -> Option<Value> {
        if let Virt::Ann { row } = v {
            return self.a_property(row, name);
        }
        if name == "cite" {
            return self.v_cite(v).map(Value::Str);
        }
        match v {
            Virt::Ann { row } => self.a_property(row, name),
            Virt::Sentence { block, k } => match name {
                "text" => self.sentence_text(block, k).map(Value::Str),
                "taxis" | "ord" => self.v_sentence_ordinal(block, k).map(Value::Int),
                _ => None,
            },
            Virt::Token { block, k } => {
                let t = self.tok(block, k)?;
                match name {
                    "text" => Some(Value::Str(self.form(block, &t).to_string())),
                    "lower" => Some(Value::Str(self.lower(self.form(block, &t)))),
                    "modern" => Some(Value::Str(self.modern(self.form(block, &t)))),
                    "class" => Some(Value::Str(t.class.name().to_string())),
                    "sentence" => self.sentence_text(block, t.sent).map(Value::Str),
                    "lemma" | "title" | "onym" | "hypograph" | "attribution" | "taxis" | "ord"
                    | "grammata" | "body" | "mention" | "entity" => None,
                    _ => self.v_token_field(v, block, &t, name),
                }
            }
        }
    }

    /// A virtual token's field: the cast or places table's own
    /// annotation first, then the spans covering the token
    /// (innermost first), then the block and its ancestors.
    fn v_token_field(&self, v: Virt, block: usize, t: &Tok, name: &str) -> Option<Value> {
        let id = match v {
            Virt::Token { block, k } => token_id(block, k),
            Virt::Sentence { .. } | Virt::Ann { .. } => return None,
        };
        if let Some(f) = self.virt_fields.get(&id)
            && let Some(val) = super::field_values(f, name)
        {
            return Some(val);
        }
        for s in self.covering_spans(block, t.at).into_iter().rev() {
            if let Some(val) = super::field_values(&self.nodes[s.0 as usize].fields, name) {
                return Some(val);
            }
        }
        let mut at = Some(NodeId(block as u64));
        while let Some(p) = at {
            let n = &self.nodes[p.0 as usize];
            if let Some(val) = super::field_values(&n.fields, name) {
                return Some(val);
            }
            at = n.parent;
        }
        None
    }

    /// The spans under `block` whose byte range covers `at`,
    /// outermost first (the tree's pre-order).
    pub(super) fn covering_spans(&self, block: usize, at: u32) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.nodes[block].children.iter().rev().copied().collect();
        while let Some(c) = stack.pop() {
            let n = &self.nodes[c.0 as usize];
            if n.kind == Kind::Span {
                if let Some((lo, hi)) = n.at
                    && lo <= at
                    && at < hi
                {
                    out.push(c);
                }
                stack.extend(n.children.iter().rev().copied());
            }
        }
        out
    }

    pub(super) fn v_metadata(&self, v: Virt, key: &str) -> Option<Value> {
        match v {
            Virt::Ann { row } => self.a_metadata(row, key),
            // `:::at` / `:::end`: the character offset and end in
            // the block's text, the coordinate that survives a
            // resegmentation (two mounts of one text join by span).
            Virt::Sentence { block, k } => match key {
                "n" | "sentence" => self.v_sentence_ordinal(block, k).map(Value::Int),
                // The span trimmed of its whitespace at both ends,
                // so `end - at` is the sentence's character count
                // and the text between them is its `::`.
                "at" | "end" => {
                    let t = self.tier(block);
                    let (lo, hi) = *t.sents.get(k as usize)?;
                    let text = self.tier_text(block);
                    let span = text.get(lo as usize..hi as usize)?;
                    let lead = (span.len() - span.trim_start().len()) as u32;
                    let trail = (span.len() - span.trim_end().len()) as u32;
                    Some(Value::Int(self.chars_to(
                        block,
                        if key == "at" { lo + lead } else { hi - trail },
                    )))
                }
                _ => None,
            },
            Virt::Token { block, k } => {
                let t = self.tok(block, k)?;
                match key {
                    "n" => {
                        let (n, _) = *self.ords().get(&block)?;
                        Some(Value::Int(n as i64 + k as i64 + 1))
                    }
                    "sentence" => {
                        let (_, s) = *self.ords().get(&block)?;
                        Some(Value::Int(s as i64 + t.sent as i64 + 1))
                    }
                    "at" => Some(Value::Int(self.chars_to(block, t.at))),
                    "end" => Some(Value::Int(self.chars_to(block, t.end))),
                    _ => None,
                }
            }
        }
    }

    pub(super) fn v_links(&self, v: Virt) -> Vec<(String, NodeId)> {
        let id = match v {
            Virt::Token { block, k } => token_id(block, k),
            Virt::Ann { row } => return self.a_links(row),
            Virt::Sentence { .. } => return Vec::new(),
        };
        let mut out = Vec::new();
        if let Some(c) = self.virt_character.get(&id) {
            out.push(("character".to_string(), *c));
        }
        if let Some(p) = self.virt_place.get(&id) {
            out.push(("place".to_string(), *p));
        }
        out
    }

    /// The lit reading's spans over a virtual tier: each span takes
    /// the tokens its byte range covers, from the block's table.
    pub(super) fn attribute_spans_virtual(&mut self) {
        let spans: Vec<usize> = (0..self.nodes.len())
            .filter(|&i| self.nodes[i].kind == Kind::Span && self.nodes[i].at.is_some())
            .collect();
        for span in spans {
            let (lo, hi) = self.nodes[span].at.expect("filtered");
            let mut block = span;
            while self.nodes[block].kind == Kind::Span
                && let Some(p) = self.nodes[block].parent
            {
                block = p.0 as usize;
            }
            if !self.virtual_block(block) {
                continue;
            }
            let t = self.tier(block);
            let covered: Vec<NodeId> = t
                .toks
                .iter()
                .enumerate()
                .filter(|(_, tok)| lo <= tok.at && tok.at < hi)
                .map(|(k, _)| token_id(block, k as u32))
                .collect();
            self.nodes[span].span_tokens = covered;
        }
    }

    /// The field a cast or places row sets on a token, stored or
    /// virtual.
    pub(super) fn push_token_field(&mut self, id: NodeId, key: &str, value: String) {
        match virt(id) {
            Some(_) => {
                let f = self.virt_fields.entry(id).or_default();
                if !f.iter().any(|(k, _)| k == key) {
                    f.push((key.to_string(), value));
                }
            }
            None => {
                let f = &mut self.nodes[id.0 as usize].fields;
                if !f.iter().any(|(k, _)| k == key) {
                    f.push((key.to_string(), value));
                }
            }
        }
    }

    pub(super) fn has_token_field(&self, id: NodeId, key: &str) -> bool {
        match virt(id) {
            Some(_) => self
                .virt_fields
                .get(&id)
                .is_some_and(|f| f.iter().any(|(k, _)| k == key)),
            None => self.nodes[id.0 as usize]
                .fields
                .iter()
                .any(|(k, _)| k == key),
        }
    }

    /// The positional index over a virtual tier: every token's
    /// lowercased form → its ids, built over every block's table.
    pub(super) fn tier_index(&self) -> std::collections::HashMap<String, Vec<NodeId>> {
        let mut map: std::collections::HashMap<String, Vec<NodeId>> = Default::default();
        for block in self.tier_blocks() {
            let t = self.tier(block);
            for (k, tok) in t.toks.iter().enumerate() {
                map.entry(self.lower(self.form(block, tok)))
                    .or_default()
                    .push(token_id(block, k as u32));
            }
        }
        map
    }
}

/// Where `line` ends in `text` when it matches at `cursor`
/// whitespace-insensitively (the characters of both compared with
/// every whitespace skipped); `None` on the first difference.
fn match_line(text: &str, cursor: usize, line: &str) -> Option<usize> {
    let mut want = line.chars().filter(|c| !c.is_whitespace());
    let mut end = cursor;
    let mut rest = text[cursor..].char_indices();
    let mut next = want.next();
    while let Some(w) = next {
        let (i, c) = rest.next()?;
        if c.is_whitespace() {
            continue;
        }
        if c != w {
            return None;
        }
        end = cursor + i + c.len_utf8();
        next = want.next();
    }
    Some(end)
}
