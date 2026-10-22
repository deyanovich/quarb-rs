//! The corpus reading's annotated tokens as rows: a CoNLL-U
//! document's words, a parsing pack's declared tokens, and the
//! plain tokens of a document that declares some — the token is
//! the source's own row, so it is stored, but as a compact row of
//! the source's columns (every string interned once per document)
//! rather than the general node. The engine sees a `token` node
//! under its sentence, as before; the ids are minted from the row
//! index (bit 55 virtual, bit 53 annotated).

use super::{
    ConlluToken, DeclaredToken, Kind, Mention, TextModel, TokenClass, class_of, class_of_annotated,
    is_quote, key_values,
};
use quarb::{NodeId, Value};

const V: u64 = 1 << 55;
const ANN: u64 = 1 << 53;

pub(super) fn ann_id(row: u32) -> NodeId {
    NodeId(V | ANN | row as u64)
}

pub(super) fn ann_row(id: NodeId) -> Option<u32> {
    (id.0 & V != 0 && id.0 & ANN != 0).then_some((id.0 & 0xffff_ffff) as u32)
}

/// One annotated token: the source's columns, interned.
pub(super) struct AnnRow {
    /// The prose block the token lies in (the cite's block).
    pub block: u32,
    /// The sentence node the token hangs under.
    pub sentence_node: u32,
    /// The token's byte offset in the block's prose (0 when the
    /// source's forms are not sliced from a text).
    pub at: u32,
    pub form: u32,
    /// Document-wide ordinals; `n` is 0 for an empty node.
    pub n: u32,
    pub sentence: u32,
    pub class: TokenClass,
    pub empty: bool,
    pub id: u32,
    pub lemma: Option<u32>,
    pub upos: Option<u32>,
    pub xpos: Option<u32>,
    pub feats: Option<u32>,
    pub deprel: Option<u32>,
    pub deps: Option<u32>,
    pub misc: Option<u32>,
    /// FEATS, MISC and extra-column pairs, in source order.
    pub keys: Vec<(u32, u32)>,
    /// The multiword range this word belongs to: its id and form.
    pub mwt: Option<(u32, u32)>,
    pub head: Option<u32>,
    /// The enhanced heads, each with its relation.
    pub eheads: Vec<(u32, u32)>,
    /// The mentions this token falls in, 1-based ordinals.
    pub mentions: Vec<u32>,
}

impl TextModel {
    fn block_of_parent(&self, parent: NodeId) -> u32 {
        let n = &self.nodes[parent.0 as usize];
        if n.kind == Kind::Sentence
            && n.derived
            && let Some(p) = n.parent
        {
            return p.0 as u32;
        }
        parent.0 as u32
    }

    fn opt_name(&mut self, v: &str) -> Option<u32> {
        (v != "_" && !v.is_empty()).then(|| self.ann_names.intern(v))
    }

    fn push_row(&mut self, parent: NodeId, row: AnnRow) -> NodeId {
        let i = self.ann.len() as u32;
        self.ann.push(row);
        self.ann_of.entry(parent.0 as usize).or_default().push(i);
        ann_id(i)
    }

    /// One annotated token under `parent` (a sentence node), from
    /// a CoNLL-U line.
    pub(super) fn push_annotated(
        &mut self,
        parent: NodeId,
        tok: &ConlluToken,
        n: u32,
        sentence: u32,
        mwt: Option<(String, String)>,
    ) -> NodeId {
        let mut keys = key_values(&tok.feats);
        keys.extend(key_values(&tok.misc));
        keys.extend(tok.extra.iter().cloned());
        let keys: Vec<(u32, u32)> = keys
            .iter()
            .map(|(k, v)| (self.ann_names.intern(k), self.ann_names.intern(v)))
            .collect();
        let mwt = mwt.map(|(id, form)| (self.ann_names.intern(&id), self.ann_names.intern(&form)));
        let row = AnnRow {
            block: self.block_of_parent(parent),
            sentence_node: parent.0 as u32,
            at: 0,
            form: self.ann_names.intern(&tok.form),
            n,
            sentence,
            class: class_of_annotated(&tok.form, &tok.upos),
            empty: tok.empty,
            id: self.ann_names.intern(&tok.id),
            lemma: self.opt_name(&tok.lemma),
            upos: self.opt_name(&tok.upos),
            xpos: self.opt_name(&tok.xpos),
            feats: self.opt_name(&tok.feats),
            deprel: self.opt_name(&tok.deprel),
            deps: self.opt_name(&tok.deps),
            misc: self.opt_name(&tok.misc),
            keys,
            mwt,
            head: None,
            eheads: Vec::new(),
            mentions: Vec::new(),
        };
        self.push_row(parent, row)
    }

    /// A token the source parsed (the parsing pack): the
    /// annotation the CoNLL-U reading would carry, from the first
    /// parsing — `::lemma` the lexema, `::upos`/`::xpos` the meros
    /// (split on `/`), `::feats` the parepomena with every
    /// `Key=Value` a property, `::deprel` the schesis, `::sem` the
    /// semasia; the alternative parsings answer as `::alt`, lemma
    /// and meros joined, one per parsing.
    pub(super) fn push_parsed(
        &mut self,
        parent: NodeId,
        form: &str,
        d: &DeclaredToken,
        n: u32,
        sentence: u32,
        at: u32,
    ) -> NodeId {
        let first = d.parsings.first().cloned().unwrap_or_default();
        let (upos, xpos) = match first.meros.as_deref() {
            Some(m) => match m.split_once('/') {
                Some((u, x)) => (Some(u.to_string()), Some(x.to_string())),
                None => (Some(m.to_string()), None),
            },
            None => (None, None),
        };
        let feats = first.parepomena.clone();
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
        let keys: Vec<(u32, u32)> = keys
            .iter()
            .map(|(k, v)| (self.ann_names.intern(k), self.ann_names.intern(v)))
            .collect();
        let id = d.onym.clone().unwrap_or_else(|| n.to_string());
        let row = AnnRow {
            block: self.block_of_parent(parent),
            sentence_node: parent.0 as u32,
            at,
            form: self.ann_names.intern(form),
            n,
            sentence,
            class: class_of_annotated(form, upos.as_deref().unwrap_or("")),
            empty: false,
            id: self.ann_names.intern(&id),
            lemma: first.lexema.as_deref().and_then(|v| self.opt_name(v)),
            upos: upos.as_deref().and_then(|v| self.opt_name(v)),
            xpos: xpos.as_deref().and_then(|v| self.opt_name(v)),
            feats: feats.as_deref().and_then(|v| self.opt_name(v)),
            deprel: first.schesis.as_deref().and_then(|v| self.opt_name(v)),
            deps: None,
            misc: None,
            keys,
            mwt: None,
            head: None,
            eheads: Vec::new(),
            mentions: Vec::new(),
        };
        self.push_row(parent, row)
    }

    /// A plain token (the segmenter's) of a document that declares
    /// some of its tokens: a row with the form alone.
    pub(super) fn push_plain_row(
        &mut self,
        parent: NodeId,
        form: &str,
        n: u32,
        sentence: u32,
        at: u32,
    ) -> NodeId {
        let row = AnnRow {
            block: self.block_of_parent(parent),
            sentence_node: parent.0 as u32,
            at,
            form: self.ann_names.intern(form),
            n,
            sentence,
            class: class_of(form),
            empty: false,
            id: self.ann_names.intern(&n.to_string()),
            lemma: None,
            upos: None,
            xpos: None,
            feats: None,
            deprel: None,
            deps: None,
            misc: None,
            keys: Vec::new(),
            mwt: None,
            head: None,
            eheads: Vec::new(),
            mentions: Vec::new(),
        };
        self.push_row(parent, row)
    }

    /// The row's form.
    pub(super) fn a_form(&self, row: u32) -> &str {
        self.ann_names.get(self.ann[row as usize].form)
    }

    fn a_key(&self, row: u32, name: &str) -> Option<&str> {
        let r = &self.ann[row as usize];
        let exact = r.keys.iter().find(|(k, _)| self.ann_names.get(*k) == name);
        let hit = exact.or_else(|| {
            r.keys
                .iter()
                .find(|(k, _)| self.ann_names.get(*k).eq_ignore_ascii_case(name))
        });
        hit.map(|(_, v)| self.ann_names.get(*v))
    }

    /// The word rows, for the cast and places matching.
    pub(super) fn ann_words(&self) -> Vec<(NodeId, String, NodeId)> {
        self.ann
            .iter()
            .enumerate()
            .filter(|(_, r)| r.class == TokenClass::Word && !r.empty)
            .map(|(i, r)| {
                (
                    ann_id(i as u32),
                    self.ann_names.get(r.form).to_string(),
                    NodeId(r.sentence_node as u64),
                )
            })
            .collect()
    }

    /// The rows under a prose block, in order.
    pub(super) fn ann_under(&self, block: usize) -> Vec<u32> {
        self.ann
            .iter()
            .enumerate()
            .filter(|(_, r)| r.block as usize == block)
            .map(|(i, _)| i as u32)
            .collect()
    }

    /// The edges of one sentence: the basic head (`->head`,
    /// `<-head`) and the enhanced graph (`->ehead`, `<-ehead`,
    /// the relation as edge data), resolved by CoNLL-U id.
    pub(super) fn link_sentence(&mut self, made: &[(NodeId, &ConlluToken)]) {
        self.mutated();
        let find = |id: &str| {
            made.iter()
                .find(|(_, t)| t.id == id)
                .and_then(|(n, _)| ann_row(*n))
        };
        for (node, tok) in made {
            let Some(row) = ann_row(*node) else {
                continue;
            };
            if !tok.empty
                && tok.head != "0"
                && tok.head != "_"
                && !tok.head.is_empty()
                && let Some(h) = find(&tok.head)
            {
                self.ann[row as usize].head = Some(h);
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
                let rel = self.ann_names.intern(rel);
                self.ann[row as usize].eheads.push((target, rel));
            }
        }
    }

    /// The declared heads of a parsing pack (`->head`), by onym.
    pub(super) fn link_declared_heads(&mut self, heads: &[(NodeId, NodeId)]) {
        self.mutated();
        for (tok, head) in heads {
            if let (Some(t), Some(h)) = (ann_row(*tok), ann_row(*head)) {
                self.ann[t as usize].head = Some(h);
            }
        }
    }

    /// The reverse of the basic heads, built once after linking.
    fn ann_dependents(&self) -> &std::collections::HashMap<u32, Vec<u32>> {
        self.ann_deps.get_or_init(|| {
            let mut map: std::collections::HashMap<u32, Vec<u32>> = Default::default();
            for (i, r) in self.ann.iter().enumerate() {
                if let Some(h) = r.head {
                    map.entry(h).or_default().push(i as u32);
                }
            }
            map
        })
    }

    fn ann_edependents(&self) -> &std::collections::HashMap<u32, Vec<(u32, u32)>> {
        self.ann_edeps.get_or_init(|| {
            let mut map: std::collections::HashMap<u32, Vec<(u32, u32)>> = Default::default();
            for (i, r) in self.ann.iter().enumerate() {
                for (h, rel) in &r.eheads {
                    map.entry(*h).or_default().push((i as u32, *rel));
                }
            }
            map
        })
    }

    /// The mentions of the annotation, decoded once the rows carry
    /// their keys: a `ner` key's BIO / BIOES tags and CorefUD's
    /// `Entity=` brackets (see the module doc of the stored tier).
    pub(super) fn decode_mentions(&mut self, entity_header: Option<&str>) {
        self.mutated();
        self.mentions.clear();
        let rows: Vec<u32> = (0..self.ann.len() as u32).collect();
        let key = |m: &TextModel, row: u32, name: &str| m.a_key(row, name).map(str::to_string);
        // Named entities: BIO / BIOES.
        let mut open: Option<(usize, Option<String>, u32)> = None;
        for &t in &rows {
            let Some(tag) = key(self, t, "ner") else {
                open = None;
                continue;
            };
            let (prefix, etype) = match tag.split_once('-') {
                Some((p, e)) => (p.to_string(), Some(e.to_string())),
                None => (tag.clone(), None),
            };
            let sentence = self.ann[t as usize].sentence;
            let continues = matches!(prefix.as_str(), "I" | "E")
                && open
                    .as_ref()
                    .is_some_and(|(_, ty, s)| *ty == etype && *s == sentence);
            match prefix.as_str() {
                "B" | "S" | "I" | "E" => {
                    if continues {
                        let (idx, _, _) = open.as_ref().unwrap();
                        self.mentions[*idx].tokens.push(ann_id(t));
                    } else {
                        self.mentions.push(Mention {
                            tokens: vec![ann_id(t)],
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
        for &t in &rows {
            let id = ann_id(t);
            let Some(spec) = key(self, t, "Entity") else {
                for (_, idx) in &stack {
                    if self.mentions[*idx].tokens.last() != Some(&id) {
                        self.mentions[*idx].tokens.push(id);
                    }
                }
                continue;
            };
            let mut closings: Vec<String> = Vec::new();
            for entry in super::entity_entries(&spec) {
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
                    let cluster = eid.split('[').next().unwrap_or(&eid).to_string();
                    let idx = self.mentions.len();
                    self.mentions.push(Mention {
                        tokens: vec![id],
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
                if self.mentions[*idx].tokens.last() != Some(&id) {
                    self.mentions[*idx].tokens.push(id);
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
                if let Some(row) = ann_row(t) {
                    self.ann[row as usize].mentions.push(i as u32 + 1);
                }
            }
        }
    }

    /// A mention's text: its tokens' forms, a space between them
    /// unless the annotation says `SpaceAfter=No`.
    pub(super) fn mention_text(&self, m: &Mention) -> String {
        let mut out = String::new();
        for (i, &t) in m.tokens.iter().enumerate() {
            let Some(row) = ann_row(t) else {
                continue;
            };
            if i > 0
                && let Some(prev) = ann_row(m.tokens[i - 1])
            {
                let glued = self.a_key(prev, "SpaceAfter") == Some("No");
                if !glued {
                    out.push(' ');
                }
            }
            out.push_str(self.a_form(row));
        }
        out
    }

    /// One value or a list, for a token in one or several mentions.
    fn a_mention_values(&self, row: u32, f: impl Fn(&Mention) -> Option<Value>) -> Option<Value> {
        let vs: Vec<Value> = self.ann[row as usize]
            .mentions
            .iter()
            .filter_map(|&i| f(&self.mentions[i as usize - 1]))
            .collect();
        // A scalar for one mention, a list for several: the
        // containment comparison reads both shapes alike (a value
        // that is no container contains itself), and a grouping key
        // stays a scalar for the common token.
        match vs.len() {
            0 => None,
            1 => vs.into_iter().next(),
            _ => Some(Value::list(vs)),
        }
    }

    /// A row's field: the cast or places table's own annotation
    /// first, then the spans covering it, then the sentence's and
    /// its ancestors' fields (a derived sentence skipped).
    fn a_token_field(&self, row: u32, name: &str) -> Option<Value> {
        let id = ann_id(row);
        if let Some(f) = self.virt_fields.get(&id)
            && let Some(val) = super::field_values(f, name)
        {
            return Some(val);
        }
        let r = &self.ann[row as usize];
        for s in self
            .covering_spans(r.block as usize, r.at)
            .into_iter()
            .rev()
        {
            if let Some(val) = super::field_values(&self.nodes[s.0 as usize].fields, name) {
                return Some(val);
            }
        }
        let mut at = Some(NodeId(r.sentence_node as u64));
        while let Some(p) = at {
            let n = &self.nodes[p.0 as usize];
            if !(n.kind == Kind::Sentence && n.derived) {
                return super::field_values(&n.fields, name);
            }
            at = n.parent;
        }
        None
    }

    pub(super) fn a_parent(&self, row: u32) -> Option<NodeId> {
        self.ann
            .get(row as usize)
            .map(|r| NodeId(r.sentence_node as u64))
    }

    pub(super) fn a_traits(&self, row: u32) -> Vec<String> {
        let Some(r) = self.ann.get(row as usize) else {
            return vec!["token".to_string()];
        };
        // the kind first (spec: Traits), then the class
        let mut out = vec!["token".to_string()];
        if r.empty {
            out.push("empty".to_string());
        } else {
            out.push(r.class.name().to_string());
            if r.class == TokenClass::Punct && is_quote(self.a_form(row)) {
                out.push("quote".to_string());
            }
        }
        out
    }

    pub(super) fn a_default_value(&self, row: u32) -> Option<Value> {
        self.ann
            .get(row as usize)
            .map(|_| Value::Str(self.a_form(row).to_string()))
    }

    pub(super) fn a_cite_row(&self, row: u32) -> Option<String> {
        let r = self.ann.get(row as usize)?;
        self.cite_in_block(r.block as usize, r.at)
    }

    pub(super) fn a_property(&self, row: u32, name: &str) -> Option<Value> {
        let r = self.ann.get(row as usize)?;
        let names = &self.ann_names;
        let text = |k: Option<u32>| k.map(|k| Value::Str(names.get(k).to_string()));
        match name {
            "cite" => self.a_cite_row(row).map(Value::Str),
            "lemma" => text(r.lemma),
            "upos" => text(r.upos),
            "xpos" => text(r.xpos),
            "feats" => text(r.feats),
            "deprel" => text(r.deprel),
            "deps" => text(r.deps),
            "misc" => text(r.misc),
            "id" => text(Some(r.id)),
            "mwt" => r
                .mwt
                .map(|(_, form)| Value::Str(names.get(form).to_string())),
            "text" => Some(Value::Str(self.a_form(row).to_string())),
            "lower" => Some(Value::Str(self.lower(self.a_form(row)))),
            "modern" => Some(Value::Str(self.modern(self.a_form(row)))),
            "class" => Some(Value::Str(r.class.name().to_string())),
            "sentence" => Some(Value::Str(
                self.nodes[r.sentence_node as usize].prose.clone(),
            )),
            "mention" => self.a_mention_values(row, |m| Some(Value::Str(self.mention_text(m)))),
            "entity" => self.a_mention_values(row, |m| m.etype.clone().map(Value::Str)),
            "title" | "onym" | "hypograph" | "attribution" | "taxis" | "ord" | "grammata"
            | "body" => None,
            _ => self
                .a_key(row, name)
                .map(|v| Value::Str(v.to_string()))
                .or_else(|| self.a_token_field(row, name)),
        }
    }

    pub(super) fn a_metadata(&self, row: u32, key: &str) -> Option<Value> {
        let r = self.ann.get(row as usize)?;
        match key {
            "n" => (r.n > 0).then(|| Value::Int(r.n as i64)),
            "sentence" => Some(Value::Int(r.sentence as i64)),
            "at" | "end" if !r.empty => {
                let at = self.chars_to(r.block as usize, r.at);
                Some(Value::Int(if key == "at" {
                    at
                } else {
                    at + self.a_form(row).chars().count() as i64
                }))
            }
            "mention" => {
                let ords: Vec<Value> = r.mentions.iter().map(|&i| Value::Int(i as i64)).collect();
                match ords.len() {
                    0 => None,
                    1 => ords.into_iter().next(),
                    _ => Some(Value::list(ords)),
                }
            }
            "entity" => self.a_mention_values(row, |m| m.cluster.clone().map(Value::Str)),
            "mwt" => r
                .mwt
                .map(|(id, _)| Value::Str(self.ann_names.get(id).to_string())),
            _ => None,
        }
    }

    pub(super) fn a_links(&self, row: u32) -> Vec<(String, NodeId)> {
        let Some(r) = self.ann.get(row as usize) else {
            return Vec::new();
        };
        let id = ann_id(row);
        let mut out = Vec::new();
        if let Some(h) = r.head {
            out.push(("head".to_string(), ann_id(h)));
        }
        for (t, _) in &r.eheads {
            out.push(("ehead".to_string(), ann_id(*t)));
        }
        if let Some(c) = self.virt_character.get(&id) {
            out.push(("character".to_string(), *c));
        }
        if let Some(p) = self.virt_place.get(&id) {
            out.push(("place".to_string(), *p));
        }
        out
    }

    pub(super) fn a_backlinks(&self, row: u32) -> Vec<(String, NodeId)> {
        let mut out: Vec<(String, NodeId)> = self
            .ann_dependents()
            .get(&row)
            .into_iter()
            .flatten()
            .map(|&d| ("head".to_string(), ann_id(d)))
            .collect();
        out.extend(
            self.ann_edependents()
                .get(&row)
                .into_iter()
                .flatten()
                .map(|(d, _)| ("ehead".to_string(), ann_id(*d))),
        );
        out
    }

    /// `$-::rel` on a dependency edge from `row`.
    pub(super) fn a_link_property(&self, row: u32, label: &str, target: NodeId) -> Option<Value> {
        let r = self.ann.get(row as usize)?;
        let t = ann_row(target)?;
        match label {
            "head" if r.head == Some(t) => r
                .deprel
                .map(|k| Value::Str(self.ann_names.get(k).to_string())),
            "ehead" => r
                .eheads
                .iter()
                .find(|(h, _)| *h == t)
                .map(|(_, rel)| Value::Str(self.ann_names.get(*rel).to_string())),
            _ => None,
        }
    }

    pub(super) fn a_locator(&self, row: u32) -> String {
        let Some(r) = self.ann.get(row as usize) else {
            return String::new();
        };
        let base = self.locator(NodeId(r.sentence_node as u64));
        let rows = self.ann_of.get(&(r.sentence_node as usize));
        let (i, n) = rows
            .map(|rs| (rs.iter().position(|&x| x == row).unwrap_or(0), rs.len()))
            .unwrap_or((0, 1));
        if n > 1 {
            format!("{base}/token[{}]", i + 1)
        } else {
            format!("{base}/token")
        }
    }
}
