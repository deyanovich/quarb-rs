//! Atrep adapter for Quarb.
//!
//! Mounts an atrep document (`.atd` source or `.atk` kanon) as an
//! arbor **tailored to its dialektos**: the dialect definition the
//! document declares supplies the node names, so a litogramma
//! section is `//section` and a koine emphasis `//emphasis` — never
//! the raw sim symbol. Quarb began life inside atrep as OQL, the
//! onym query language; this adapter closes the loop by turning
//! atrep's reference machinery into the engine's edge ontology.
//!
//! - Simmeres are the nodes, named by their dialektos sim name
//!   (block, inline, and monosim forms alike). Core constructs
//!   keep structural names: `paragraph`, `stichoi`/`strophe`/
//!   `stichos`, `diaphane`, `verbatim`, `englossis`, `enmedia`,
//!   `anaphor`, `axioma`, `axioma-ref`, `anchor`, `milestone`.
//!   Text runs are not nodes; they flatten into `::text` and the
//!   default projection, XML-adapter style.
//! - A deixis callout is named after the sim its symbol resolves
//!   to — callout and body share the name by construction (the
//!   litogramma F5 ruling), so `//footnote` gathers the whole
//!   apparatus; the `<deixis>` trait isolates the callouts and
//!   `<noted>` the bodies.
//! - A `lemma` (and `hypograph`) is both a flattened property and
//!   a child node exposing its inline structure, in document
//!   order: lemma, content, hypograph.
//! - References become typed crosslinks. A deixis or an
//!   onym-resolving monosim emits an outgoing edge labeled with
//!   its sim name (`->ref`, `->footnote`); `<-` answers "what
//!   references this" from a precomputed reverse index. A `cite`
//!   monosim (ostensive keyword `key`) resolves against an
//!   embedded bibliogramma englossis when one is present;
//!   otherwise its `::param` stays available for cross-mount
//!   joins. A reference whose target is missing keeps its node,
//!   carries `<dangling>` and `;;;resolved = false`, and emits no
//!   edge — `//*<dangling>` is a broken-cross-reference linter.
//! - The default projection of a resolved reference is its
//!   rendered form — the target's lemma (falling back to the
//!   target's text), degrading to the raw onym when dangling.
//! - Genoses surface as traits (vocabulary-canonicalized by the
//!   mount pass) and as `::genoses`; structural traits (`<block>`,
//!   `<inline>`, `<mono>`, plus the per-kind trait) and semantic
//!   traits (`<ref>`, `<target>`, `<autonym>`, `<deixis>`,
//!   `<noted>`, `<dangling>`) come from the model.
//! - Mounting parses as authored and runs `kanonizo::tasso` — the
//!   ordering subset of kanonizo — so `::taxis` is always settled
//!   and autonyms are pinned, while onyms stay as written and
//!   transclusion/axioma expansion do not run (anaphors and
//!   axiomata stay visible as nodes and edges). A kanon (`.atk`)
//!   mounts identically: tasso is idempotent on settled input.

use std::collections::HashMap;
use std::path::Path;

use atrep::dendron::{Block, Inline, Strophe, Taxis};
use atrep::dialektos::{Dialektos, SimForm};
use atrep::{Checked, dialektos, kanonizo};
use quarb::{AstAdapter, NodeId, Value};

/// An error building an [`AtrepAdapter`].
#[derive(Debug, thiserror::Error)]
pub enum AtrepError {
    #[error(transparent)]
    Atrep(#[from] atrep::Error),
    #[error("input is an atrep dialektos definition, not a document")]
    NotADocument,
}

/// The structural kind of a node — the core form behind the
/// dialektos name. Doubles as the kind trait.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Kind {
    #[default]
    Root,
    Para,
    Paragraph,
    Stichoi,
    Strophe,
    Stichos,
    Endo,
    Mono,
    Deixis,
    Diaphane,
    Verbatim,
    Englossis,
    Enmedia,
    Anaphor,
    Axioma,
    AxiomaRef,
    Anchor,
    Milestone,
    Lemma,
    Hypograph,
}

impl Kind {
    fn trait_name(self) -> Option<&'static str> {
        Some(match self {
            Kind::Root => return None,
            Kind::Para => "para",
            Kind::Paragraph => "paragraph",
            Kind::Stichoi => "stichoi",
            Kind::Strophe => "strophe",
            Kind::Stichos => "stichos",
            Kind::Endo => "endo",
            Kind::Mono => "mono",
            Kind::Deixis => "deixis",
            Kind::Diaphane => "diaphane",
            Kind::Verbatim => "verbatim",
            Kind::Englossis => "englossis",
            Kind::Enmedia => "enmedia",
            Kind::Anaphor => "anaphor",
            Kind::Axioma => "axioma",
            Kind::AxiomaRef => "axioma-ref",
            Kind::Anchor => "anchor",
            Kind::Milestone => "milestone",
            Kind::Lemma => "lemma",
            Kind::Hypograph => "hypograph",
        })
    }

    /// The grammatical-category trait: which context the form
    /// occupies. Strophes, stichos lines, and lemma/hypograph
    /// wrappers are sub-block structure and carry none.
    fn family(self) -> Option<&'static str> {
        match self {
            Kind::Para
            | Kind::Paragraph
            | Kind::Stichoi
            | Kind::Englossis
            | Kind::Enmedia
            | Kind::Anaphor => Some("block"),
            Kind::Endo | Kind::Deixis | Kind::Anchor | Kind::Milestone => Some("inline"),
            Kind::Mono => Some("mono"),
            _ => None,
        }
    }
}

/// Dialektos-sourced facts about a sim node, absent on structural
/// nodes.
#[derive(Debug, Default, Clone)]
struct SimMeta {
    short_desc: Option<String>,
    long_desc: Option<String>,
    form: Option<&'static str>,
    keyword: Option<String>,
    bracket_matching: Option<bool>,
    autonym: bool,
}

/// An outgoing reference recorded on a node.
#[derive(Debug, Clone)]
struct Reference {
    /// The onym / axioma name / cite key as written.
    key: String,
    /// The edge label (the reference sim's resolved name, or
    /// `axioma` for axioma references).
    label: String,
    /// Whether the dialektos marks this as an onym reference (the
    /// ostensive keyword, or the form itself for deixes) — an
    /// unresolved intended reference is `<dangling>`.
    intent: bool,
    resolved: Option<NodeId>,
}

#[derive(Debug, Default)]
struct Node {
    /// The resolved display name; `None` only for the root.
    name: Option<String>,
    kind: Kind,
    symbol: Option<String>,
    onym: Option<String>,
    genoses: Vec<String>,
    taxis: Option<u64>,
    lemma: Option<String>,
    hypograph: Option<String>,
    /// Flattened descendant text, in document order.
    text: String,
    /// The raw parameter: a monosim's param, a deixis onym, an
    /// anaphor/enmedia target, an englossis dialect id.
    param: Option<String>,
    scheme: Option<String>,
    scheme_value: Option<String>,
    sim: Option<Box<SimMeta>>,
    reference: Option<Reference>,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
}

/// A Quarb adapter over a parsed atrep document.
#[derive(Debug)]
pub struct AtrepAdapter {
    nodes: Vec<Node>,
    dialect_id: String,
    dialect_version: Option<String>,
    /// Onym (as present in the mounted document) → its node.
    onyms: HashMap<String, NodeId>,
    /// Reverse crosslink index: target → (label, source).
    backrefs: HashMap<NodeId, Vec<(String, NodeId)>>,
    /// Targets of at least one deixis edge — the `<noted>` set.
    noted: std::collections::HashSet<NodeId>,
}

/// Walk-scope state: the arena plus the registries filled during
/// pass A.
struct Builder {
    nodes: Vec<Node>,
    onyms: HashMap<String, NodeId>,
    axiomata: HashMap<String, NodeId>,
    /// Bibliogramma entry key → entry node, from an embedded
    /// `@@@!(bibliogramma)` englossis.
    cite_keys: HashMap<String, NodeId>,
}

impl AtrepAdapter {
    /// Mount a document file. `.atd` and `.atk` both parse with the
    /// same pipeline; `kanonizo::tasso` settles taxis, autonyms,
    /// and vocabularies in place (idempotent on a kanon) without
    /// canonicalizing onyms or expanding transclusions.
    pub fn parse_file(path: &Path) -> Result<Self, AtrepError> {
        match atrep::check_any(path)? {
            Checked::Dialektos(_) => Err(AtrepError::NotADocument),
            Checked::Document(mut doc) => {
                let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
                let dial = dialektos::resolve(&base, &doc.dialect_id)?;
                kanonizo::tasso(&mut doc, &dial, &base)?;
                Ok(Self::from_document(doc, &dial, &base))
            }
        }
    }

    /// Mount document text. `dir` anchors dialektos resolution (and
    /// nested-englossis resolution): pass the document's directory,
    /// or `.` for stdin — the std dialektoi resolve regardless.
    pub fn parse_str(source: &str, dir: &Path) -> Result<Self, AtrepError> {
        match atrep::check_source(source, &dir.join("<memory>.atd"))? {
            Checked::Dialektos(_) => Err(AtrepError::NotADocument),
            Checked::Document(mut doc) => {
                let dial = dialektos::resolve(dir, &doc.dialect_id)?;
                kanonizo::tasso(&mut doc, &dial, dir)?;
                Ok(Self::from_document(doc, &dial, dir))
            }
        }
    }

    /// Build the adapter from an already-settled document (run
    /// `kanonizo::tasso` first — [`parse_file`](Self::parse_file)
    /// and [`parse_str`](Self::parse_str) do). `base` resolves the
    /// embedded dialektos of any englossis block.
    pub fn from_document(doc: atrep::Document, dial: &Dialektos, base: &Path) -> Self {
        let mut b = Builder {
            nodes: vec![Node::default()],
            onyms: HashMap::new(),
            axiomata: HashMap::new(),
            cite_keys: HashMap::new(),
        };
        let root = NodeId(0);
        for block in &doc.blocks {
            b.walk_block(block, root, dial, base);
        }
        b.nodes[0].text = join_blocks(&doc.blocks);

        // Pass B: resolve the recorded references and build the
        // reverse index.
        let mut backrefs: HashMap<NodeId, Vec<(String, NodeId)>> = HashMap::new();
        let mut noted = std::collections::HashSet::new();
        for idx in 0..b.nodes.len() {
            let source = NodeId(idx as u64);
            let Some(r) = &b.nodes[idx].reference else {
                continue;
            };
            let target = match b.nodes[idx].kind {
                Kind::AxiomaRef => b.axiomata.get(&r.key).copied(),
                Kind::Anaphor => None,
                _ if b.nodes[idx].sim.as_ref().is_some_and(|s| {
                    s.keyword.as_deref() == Some("key")
                }) =>
                {
                    b.cite_keys.get(&r.key).copied()
                }
                _ => b.onyms.get(&r.key).copied(),
            };
            if let Some(t) = target {
                let label = r.label.clone();
                backrefs.entry(t).or_default().push((label, source));
                if b.nodes[idx].kind == Kind::Deixis {
                    noted.insert(t);
                }
            }
            b.nodes[idx].reference.as_mut().expect("checked").resolved = target;
        }

        AtrepAdapter {
            nodes: b.nodes,
            dialect_id: doc.dialect_id,
            dialect_version: doc.dialect_version,
            onyms: b.onyms,
            backrefs,
            noted,
        }
    }

    fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    /// A locator path to `node`, like `/section(intro)/paragraph[2]`:
    /// sim names as segments, the onym in atrep's own annotation
    /// form when the node carries one, a `[n]` index only to
    /// disambiguate same-name siblings.
    pub fn locator(&self, node: NodeId) -> String {
        let mut segments = Vec::new();
        let mut cur = Some(node);
        while let Some(id) = cur {
            let n = self.node(id);
            if let Some(name) = &n.name {
                segments.push(self.segment(id, name));
            }
            cur = n.parent;
        }
        segments.reverse();
        format!("/{}", segments.join("/"))
    }

    fn segment(&self, node: NodeId, name: &str) -> String {
        let n = self.node(node);
        if let Some(onym) = &n.onym {
            return format!("{name}({onym})");
        }
        let Some(parent) = n.parent else {
            return name.to_string();
        };
        let siblings = &self.node(parent).children;
        let same: Vec<NodeId> = siblings
            .iter()
            .copied()
            .filter(|&s| self.node(s).name.as_deref() == Some(name))
            .collect();
        if same.len() > 1 {
            let i = same.iter().position(|&s| s == node).unwrap() + 1;
            format!("{name}[{i}]")
        } else {
            name.to_string()
        }
    }
}

impl Builder {
    fn push(&mut self, parent: NodeId, node: Node) -> NodeId {
        let id = NodeId(self.nodes.len() as u64);
        self.nodes.push(node);
        self.nodes[parent.0 as usize].children.push(id);
        if let Some(onym) = self.nodes[id.0 as usize].onym.clone() {
            self.onyms.entry(onym).or_insert(id);
        }
        id
    }

    /// The dialektos-resolved display name of a sim symbol, falling
    /// back to the symbol itself for anything the (resolved)
    /// dialektos does not define.
    fn sim_name(dial: &Dialektos, symbol: &str) -> String {
        dial.sims
            .get(symbol)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| symbol.to_string())
    }

    fn sim_meta(dial: &Dialektos, symbol: &str) -> Box<SimMeta> {
        let Some(def) = dial.sims.get(symbol) else {
            return Box::new(SimMeta::default());
        };
        let (form, keyword, autonym) = match &def.form {
            SimForm::Endo => ("endo", None, false),
            SimForm::Para {
                stichoi, autonym, ..
            } => (if *stichoi { "stichoi" } else { "para" }, None, *autonym),
            SimForm::Mono { keyword } => ("mono", Some(keyword.clone()), false),
        };
        Box::new(SimMeta {
            short_desc: def.short_desc.clone(),
            long_desc: def.long_desc.clone(),
            form: Some(form),
            keyword,
            bracket_matching: Some(def.bracket_matching),
            autonym,
        })
    }

    /// Create the `lemma` / `hypograph` wrapper child when the
    /// fragment is non-empty, exposing its inline structure.
    fn wrap_inlines(
        &mut self,
        parent: NodeId,
        kind: Kind,
        name: &str,
        inlines: &[Inline],
        dial: &Dialektos,
    ) {
        if inlines.is_empty() {
            return;
        }
        let id = self.push(
            parent,
            Node {
                name: Some(name.to_string()),
                kind,
                text: flatten_inlines(inlines),
                parent: Some(parent),
                ..Node::default()
            },
        );
        for inline in inlines {
            self.walk_inline(inline, id, dial);
        }
    }

    fn walk_block(&mut self, block: &Block, parent: NodeId, dial: &Dialektos, base: &Path) {
        match block {
            Block::Paragraph(inlines) => {
                let id = self.push(
                    parent,
                    Node {
                        name: Some("paragraph".to_string()),
                        kind: Kind::Paragraph,
                        text: flatten_inlines(inlines),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
                for inline in inlines {
                    self.walk_inline(inline, id, dial);
                }
            }
            Block::Para {
                symbol,
                taxis,
                lemma,
                children,
                hypograph,
                ann,
                ..
            } => {
                let id = self.push(
                    parent,
                    Node {
                        name: Some(Self::sim_name(dial, symbol)),
                        kind: Kind::Para,
                        symbol: Some(symbol.clone()),
                        onym: ann.onym.clone(),
                        genoses: ann.genoses.clone(),
                        taxis: explicit(taxis),
                        lemma: nonempty(flatten_inlines(lemma)),
                        hypograph: nonempty(flatten_inlines(hypograph)),
                        text: {
                            let mut t = flatten_inlines(lemma);
                            let body = join_blocks(children);
                            join_para(&mut t, &body);
                            join_para(&mut t, &flatten_inlines(hypograph));
                            t
                        },
                        sim: Some(Self::sim_meta(dial, symbol)),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
                self.wrap_inlines(id, Kind::Lemma, "lemma", lemma, dial);
                for child in children {
                    self.walk_block(child, id, dial, base);
                }
                self.wrap_inlines(id, Kind::Hypograph, "hypograph", hypograph, dial);
            }
            Block::Stichoi {
                symbol,
                taxis,
                lemma,
                strophes,
                hypograph,
                ann,
                ..
            } => {
                let name = match symbol {
                    Some(sym) => Self::sim_name(dial, sym),
                    None => "stichoi".to_string(),
                };
                let id = self.push(
                    parent,
                    Node {
                        name: Some(name),
                        kind: Kind::Stichoi,
                        symbol: symbol.clone(),
                        onym: ann.onym.clone(),
                        genoses: ann.genoses.clone(),
                        taxis: explicit(taxis),
                        lemma: nonempty(flatten_inlines(lemma)),
                        hypograph: nonempty(flatten_inlines(hypograph)),
                        text: stichoi_text(lemma, strophes, hypograph),
                        sim: symbol.as_deref().map(|s| Self::sim_meta(dial, s)),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
                self.wrap_inlines(id, Kind::Lemma, "lemma", lemma, dial);
                for strophe in strophes {
                    self.walk_strophe(strophe, id, dial);
                }
                self.wrap_inlines(id, Kind::Hypograph, "hypograph", hypograph, dial);
            }
            Block::ParaDiaphane { children, ann } => {
                let id = self.push(
                    parent,
                    Node {
                        name: Some("diaphane".to_string()),
                        kind: Kind::Diaphane,
                        onym: ann.onym.clone(),
                        genoses: ann.genoses.clone(),
                        text: join_blocks(children),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
                for child in children {
                    self.walk_block(child, id, dial, base);
                }
            }
            Block::VerbatimBlock { content, ann } => {
                self.push(
                    parent,
                    Node {
                        name: Some("verbatim".to_string()),
                        kind: Kind::Verbatim,
                        onym: ann.onym.clone(),
                        genoses: ann.genoses.clone(),
                        text: content.clone(),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
            }
            Block::MonadEnglossis {
                dialect,
                children,
                ann,
            } => {
                let id = self.push(
                    parent,
                    Node {
                        name: Some("englossis".to_string()),
                        kind: Kind::Englossis,
                        onym: ann.onym.clone(),
                        genoses: ann.genoses.clone(),
                        param: Some(dialect.clone()),
                        text: join_blocks(children),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
                // The embedded subtree names through its own
                // dialektos; on resolution failure the enclosing
                // one still gives structural navigation.
                let inner = if *dialect != dial.id {
                    dialektos::resolve(base, dialect).ok()
                } else {
                    None
                };
                let inner_dial = inner.as_ref().unwrap_or(dial);
                for child in children {
                    self.walk_block(child, id, inner_dial, base);
                }
                if dialect == "bibliogramma" {
                    self.index_bibliogramma(id);
                }
            }
            Block::Enmedia { param } => {
                self.push(
                    parent,
                    Node {
                        name: Some("enmedia".to_string()),
                        kind: Kind::Enmedia,
                        param: Some(param.clone()),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
            }
            Block::EnmediaHashed { sha256 } => {
                self.push(
                    parent,
                    Node {
                        name: Some("enmedia".to_string()),
                        kind: Kind::Enmedia,
                        scheme_value: Some(sha256.clone()),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
            }
            Block::AnaphorEnglossis { target } | Block::AnaphorEnlexis { target } => {
                self.push(
                    parent,
                    Node {
                        name: Some("anaphor".to_string()),
                        kind: Kind::Anaphor,
                        param: Some(target.clone()),
                        reference: Some(Reference {
                            key: target.clone(),
                            label: "anaphor".to_string(),
                            intent: false,
                            resolved: None,
                        }),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
            }
            Block::ParaAxioma { onym, children } => {
                let id = self.push(
                    parent,
                    Node {
                        name: Some("axioma".to_string()),
                        kind: Kind::Axioma,
                        onym: Some(onym.clone()),
                        text: join_blocks(children),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
                self.axiomata.entry(onym.clone()).or_insert(id);
                for child in children {
                    self.walk_block(child, id, dial, base);
                }
            }
            Block::AxiomaRefBlock { onym, .. } => {
                self.push(
                    parent,
                    Node {
                        name: Some("axioma-ref".to_string()),
                        kind: Kind::AxiomaRef,
                        param: Some(onym.clone()),
                        reference: Some(Reference {
                            key: onym.clone(),
                            label: "axioma".to_string(),
                            intent: true,
                            resolved: None,
                        }),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
            }
        }
    }

    fn walk_strophe(&mut self, strophe: &Strophe, parent: NodeId, dial: &Dialektos) {
        let id = self.push(
            parent,
            Node {
                name: Some("strophe".to_string()),
                kind: Kind::Strophe,
                text: strophe
                    .0
                    .iter()
                    .map(|line| flatten_inlines(line))
                    .collect::<Vec<_>>()
                    .join("\n"),
                parent: Some(parent),
                ..Node::default()
            },
        );
        for line in &strophe.0 {
            let line_id = self.push(
                id,
                Node {
                    name: Some("stichos".to_string()),
                    kind: Kind::Stichos,
                    text: flatten_inlines(line),
                    parent: Some(id),
                    ..Node::default()
                },
            );
            for inline in line {
                self.walk_inline(inline, line_id, dial);
            }
        }
    }

    fn walk_inline(&mut self, inline: &Inline, parent: NodeId, dial: &Dialektos) {
        match inline {
            Inline::Text(_) => {}
            Inline::Endo {
                symbol,
                content,
                ann,
                ..
            } => {
                let id = self.push(
                    parent,
                    Node {
                        name: Some(Self::sim_name(dial, symbol)),
                        kind: Kind::Endo,
                        symbol: Some(symbol.clone()),
                        onym: ann.onym.clone(),
                        genoses: ann.genoses.clone(),
                        text: flatten_inlines(content),
                        sim: Some(Self::sim_meta(dial, symbol)),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
                for c in content {
                    self.walk_inline(c, id, dial);
                }
            }
            Inline::VerbatimInline { content, ann } => {
                self.push(
                    parent,
                    Node {
                        name: Some("verbatim".to_string()),
                        kind: Kind::Verbatim,
                        onym: ann.onym.clone(),
                        genoses: ann.genoses.clone(),
                        text: content.clone(),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
            }
            Inline::EndoDiaphane { content, ann } => {
                let id = self.push(
                    parent,
                    Node {
                        name: Some("diaphane".to_string()),
                        kind: Kind::Diaphane,
                        onym: ann.onym.clone(),
                        genoses: ann.genoses.clone(),
                        text: flatten_inlines(content),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
                for c in content {
                    self.walk_inline(c, id, dial);
                }
            }
            Inline::Monosim { symbol, param, ann } => {
                let meta = Self::sim_meta(dial, symbol);
                let name = Self::sim_name(dial, symbol);
                // The dialektos marks intent through the ostensive
                // keyword; the pilot's kanonizo rule (spec gap 3)
                // additionally treats any param that names an onym
                // as a reference, so both resolve.
                let keyword = meta.keyword.as_deref();
                let reference = match keyword {
                    Some("onym") => Some(Reference {
                        key: param.clone(),
                        label: name.clone(),
                        intent: true,
                        resolved: None,
                    }),
                    Some("key") => Some(Reference {
                        key: param.clone(),
                        label: name.clone(),
                        // A cite may point at a separate mounted
                        // bibliography; unresolved is not dangling.
                        intent: false,
                        resolved: None,
                    }),
                    _ => self.onyms.contains_key(param).then(|| Reference {
                        key: param.clone(),
                        label: name.clone(),
                        intent: false,
                        resolved: None,
                    }),
                };
                self.push(
                    parent,
                    Node {
                        name: Some(name),
                        kind: Kind::Mono,
                        symbol: Some(symbol.clone()),
                        onym: ann.onym.clone(),
                        genoses: ann.genoses.clone(),
                        param: Some(param.clone()),
                        sim: Some(meta),
                        reference,
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
            }
            Inline::OnymAnchor(onym) => {
                self.push(
                    parent,
                    Node {
                        name: Some("anchor".to_string()),
                        kind: Kind::Anchor,
                        onym: Some(onym.clone()),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
            }
            Inline::Milestone { scheme, value, ann } => {
                self.push(
                    parent,
                    Node {
                        name: Some("milestone".to_string()),
                        kind: Kind::Milestone,
                        onym: ann.onym.clone(),
                        genoses: ann.genoses.clone(),
                        scheme: Some(scheme.clone()),
                        scheme_value: Some(value.clone()),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
            }
            Inline::Deixis { symbol, onym, ann } => {
                self.push(
                    parent,
                    Node {
                        // The callout is named after the sim it
                        // points into — shared by construction.
                        name: Some(Self::sim_name(dial, symbol)),
                        kind: Kind::Deixis,
                        symbol: Some(symbol.clone()),
                        genoses: ann.genoses.clone(),
                        param: Some(onym.clone()),
                        sim: Some(Self::sim_meta(dial, symbol)),
                        reference: Some(Reference {
                            key: onym.clone(),
                            label: Self::sim_name(dial, symbol),
                            intent: true,
                            resolved: None,
                        }),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
            }
            Inline::EndoAxioma { onym, content } => {
                let id = self.push(
                    parent,
                    Node {
                        name: Some("axioma".to_string()),
                        kind: Kind::Axioma,
                        onym: Some(onym.clone()),
                        text: flatten_inlines(content),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
                self.axiomata.entry(onym.clone()).or_insert(id);
                for c in content {
                    self.walk_inline(c, id, dial);
                }
            }
            Inline::AxiomaRef { onym, .. } => {
                self.push(
                    parent,
                    Node {
                        name: Some("axioma-ref".to_string()),
                        kind: Kind::AxiomaRef,
                        param: Some(onym.clone()),
                        reference: Some(Reference {
                            key: onym.clone(),
                            label: "axioma".to_string(),
                            intent: true,
                            resolved: None,
                        }),
                        parent: Some(parent),
                        ..Node::default()
                    },
                );
            }
        }
    }

    /// Index an embedded bibliogramma englossis: entries are paras
    /// whose lemma is the citation key.
    fn index_bibliogramma(&mut self, englossis: NodeId) {
        let entries: Vec<(String, NodeId)> = self.nodes[englossis.0 as usize]
            .children
            .iter()
            .filter_map(|&c| {
                let n = &self.nodes[c.0 as usize];
                (n.kind == Kind::Para)
                    .then(|| n.lemma.clone().map(|k| (k.trim().to_string(), c)))
                    .flatten()
            })
            .collect();
        for (key, id) in entries {
            self.cite_keys.entry(key).or_insert(id);
        }
    }
}

fn explicit(taxis: &Option<Taxis>) -> Option<u64> {
    match taxis {
        Some(Taxis::Explicit(n)) => Some(*n),
        _ => None,
    }
}

fn nonempty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

/// Append a block-level fragment with a newline separator.
fn join_para(out: &mut String, part: &str) {
    if part.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(part);
}

/// Flattened text of an inline run: text and verbatim content, with
/// wrappers recursed; references, anchors, and milestones contribute
/// nothing (their rendered form is a projection, not content).
fn flatten_inlines(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(t) => out.push_str(t),
            Inline::Endo { content, .. }
            | Inline::EndoDiaphane { content, .. }
            | Inline::EndoAxioma { content, .. } => out.push_str(&flatten_inlines(content)),
            Inline::VerbatimInline { content, .. } => out.push_str(content),
            _ => {}
        }
    }
    out
}

fn block_text(block: &Block) -> String {
    match block {
        Block::Paragraph(inlines) => flatten_inlines(inlines),
        Block::Para {
            lemma,
            children,
            hypograph,
            ..
        } => {
            let mut t = flatten_inlines(lemma);
            join_para(&mut t, &join_blocks(children));
            join_para(&mut t, &flatten_inlines(hypograph));
            t
        }
        Block::Stichoi {
            lemma,
            strophes,
            hypograph,
            ..
        } => stichoi_text(lemma, strophes, hypograph),
        Block::ParaDiaphane { children, .. } | Block::MonadEnglossis { children, .. } => {
            join_blocks(children)
        }
        Block::VerbatimBlock { content, .. } => content.clone(),
        Block::ParaAxioma { children, .. } => join_blocks(children),
        Block::Enmedia { .. }
        | Block::EnmediaHashed { .. }
        | Block::AnaphorEnglossis { .. }
        | Block::AnaphorEnlexis { .. }
        | Block::AxiomaRefBlock { .. } => String::new(),
    }
}

fn join_blocks(blocks: &[Block]) -> String {
    let mut out = String::new();
    for block in blocks {
        join_para(&mut out, &block_text(block));
    }
    out
}

fn stichoi_text(lemma: &[Inline], strophes: &[Strophe], hypograph: &[Inline]) -> String {
    let mut t = flatten_inlines(lemma);
    for strophe in strophes {
        for line in &strophe.0 {
            join_para(&mut t, &flatten_inlines(line));
        }
    }
    join_para(&mut t, &flatten_inlines(hypograph));
    t
}

impl AstAdapter for AtrepAdapter {
    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.node(node).children.clone()
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.node(node).name.clone()
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.node(node).parent
    }

    fn traits(&self, node: NodeId) -> Vec<String> {
        let n = self.node(node);
        let mut out = Vec::new();
        if let Some(family) = n.kind.family() {
            out.push(family.to_string());
        }
        if let Some(kind) = n.kind.trait_name() {
            out.push(kind.to_string());
        }
        if let Some(r) = &n.reference {
            out.push("ref".to_string());
            if r.intent && r.resolved.is_none() {
                out.push("dangling".to_string());
            }
        }
        if n.onym.is_some() || matches!(n.kind, Kind::Milestone) {
            out.push("target".to_string());
        }
        if self.noted.contains(&node) {
            out.push("noted".to_string());
        }
        if n.sim.as_ref().is_some_and(|s| s.autonym) {
            out.push("autonym".to_string());
        }
        out.extend(n.genoses.iter().cloned());
        out
    }

    /// `::onym`, `::taxis`, `::lemma`, `::hypograph`, `::text`,
    /// `::symbol`, `::sim`, `::param`, `::target`, `::genoses`,
    /// `::dialect`, `::scheme`, `::value`.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let n = self.node(node);
        match name {
            "onym" => n.onym.clone().map(Value::Str),
            "taxis" => n.taxis.map(|t| Value::Int(t as i64)),
            "lemma" => n.lemma.clone().map(Value::Str),
            "hypograph" => n.hypograph.clone().map(Value::Str),
            "text" => Some(Value::Str(n.text.clone())),
            "symbol" => n.symbol.clone().map(Value::Str),
            "sim" => n.name.clone().map(Value::Str),
            "param" => n.param.clone().map(Value::Str),
            "target" => n.reference.as_ref().map(|r| Value::Str(r.key.clone())),
            "genoses" => Some(Value::List(
                n.genoses.iter().cloned().map(Value::Str).collect(),
            )),
            "dialect" => match n.kind {
                Kind::Root => Some(Value::Str(self.dialect_id.clone())),
                Kind::Englossis => n.param.clone().map(Value::Str),
                _ => None,
            },
            "scheme" => n.scheme.clone().map(Value::Str),
            "value" => n.scheme_value.clone().map(Value::Str),
            _ => None,
        }
    }

    /// A resolved reference projects its rendered form — the
    /// target's lemma, falling back to the target's text — and a
    /// dangling one its raw onym. Everything else projects its
    /// flattened text (a monosim its param).
    fn default_value(&self, node: NodeId) -> Option<Value> {
        let n = self.node(node);
        if let Some(r) = &n.reference {
            let rendered = r.resolved.map(|t| {
                let target = self.node(t);
                match &target.lemma {
                    Some(lemma) => lemma.clone(),
                    None => target.text.clone(),
                }
            });
            return Some(Value::Str(rendered.unwrap_or_else(|| r.key.clone())));
        }
        if n.kind == Kind::Mono {
            return n.param.clone().map(Value::Str);
        }
        Some(Value::Str(n.text.clone()))
    }

    /// Root: `;;;dialect`, `;;;dialect-version`. Sim nodes:
    /// `;;;short-desc`, `;;;long-desc`, `;;;form`, `;;;keyword`,
    /// `;;;bracket-matching`. References: `;;;resolved`. Verbatim:
    /// `;;;lang`. Enmedia: `;;;media`, `;;;sha256`. Englossis:
    /// `;;;dialect`.
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let n = self.node(node);
        match key {
            "dialect" if n.kind == Kind::Root => Some(Value::Str(self.dialect_id.clone())),
            "dialect-version" if n.kind == Kind::Root => {
                self.dialect_version.clone().map(Value::Str)
            }
            "dialect" if n.kind == Kind::Englossis => n.param.clone().map(Value::Str),
            "short-desc" => n.sim.as_ref().and_then(|s| s.short_desc.clone()).map(Value::Str),
            "long-desc" => n.sim.as_ref().and_then(|s| s.long_desc.clone()).map(Value::Str),
            "form" => n
                .sim
                .as_ref()
                .and_then(|s| s.form)
                .map(|f| Value::Str(f.to_string())),
            "keyword" => n.sim.as_ref().and_then(|s| s.keyword.clone()).map(Value::Str),
            "bracket-matching" => n
                .sim
                .as_ref()
                .and_then(|s| s.bracket_matching)
                .map(Value::Bool),
            "resolved" => n
                .reference
                .as_ref()
                .map(|r| Value::Bool(r.resolved.is_some())),
            "lang" if n.kind == Kind::Verbatim => {
                n.genoses.first().cloned().map(Value::Str)
            }
            "media" if n.kind == Kind::Enmedia => n.param.clone().map(Value::Str),
            "sha256" if n.kind == Kind::Enmedia => n.scheme_value.clone().map(Value::Str),
            _ => None,
        }
    }

    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let n = self.node(node);
        match &n.reference {
            Some(r) => r
                .resolved
                .map(|t| vec![(r.label.clone(), t)])
                .unwrap_or_default(),
            None => Vec::new(),
        }
    }

    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.backrefs.get(&node).cloned().unwrap_or_default()
    }

    /// `::param~>` / `::target~>` follows the node's reference; a
    /// hint restricts to targets of that name. `::onym~>` on any
    /// onym value resolves through the registry.
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let n = self.node(node);
        let target = match property {
            "param" | "target" => n.reference.as_ref().and_then(|r| r.resolved).or_else(|| {
                n.param.as_ref().and_then(|p| self.onyms.get(p).copied())
            }),
            "onym" => n.onym.as_ref().and_then(|o| self.onyms.get(o).copied()),
            _ => None,
        }?;
        match hint {
            Some(h) if self.node(target).name.as_deref() != Some(h) => None,
            _ => Some(target),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(a: &AtrepAdapter, query: &str) -> Vec<String> {
        match quarb::run(query, a).expect(query) {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
            quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
        }
    }

    const DOC: &str = "\
@@@!koine

@#() Introduction
Quarb began as @/OQL/@ here.@^(n1) See @>(methods).
#@(intro)

@#() Methods
Beta @*text*@ and a dangling @>(ghost).
#@(methods)

@\"\"strange loops\"\"@.said

@@@=
Happy the man, whose wish and care
A few paternal acres bound
=@@@

@^
The onym query language.
^@(n1)
";

    fn mount() -> AtrepAdapter {
        AtrepAdapter::parse_str(DOC, Path::new(".")).unwrap()
    }

    #[test]
    fn koine_sections_mount_as_nodes() {
        let a = mount();
        assert_eq!(values(&a, "//section::lemma"), ["Introduction", "Methods"]);
        assert_eq!(values(&a, "//section::taxis"), ["1", "2"]);
        assert_eq!(values(&a, "/section[::onym = \"intro\"]::sim"), ["section"]);
        // The raw symbol stays addressable.
        assert_eq!(values(&a, "//section[1]::symbol"), ["#"]);
    }

    #[test]
    fn dialektos_tailored_naming() {
        let a = mount();
        // The emphasis endo names through the dialektos, not `/`.
        assert_eq!(values(&a, "//emphasis::text"), ["OQL"]);
        assert_eq!(values(&a, "//strong::text"), ["text"]);
        // Root metadata carries the declaration.
        assert_eq!(values(&a, ";;;dialect"), ["koine"]);
    }

    #[test]
    fn paragraph_default_projection() {
        let a = mount();
        assert_eq!(
            values(&a, "/section[1]/paragraph::"),
            ["Quarb began as OQL here. See ."]
        );
    }

    #[test]
    fn deixis_named_by_target_sim() {
        let a = mount();
        // //footnote gathers the whole apparatus: callout + body.
        assert_eq!(values(&a, "//footnote @| count"), ["2"]);
        assert_eq!(values(&a, "//footnote<deixis> @| count"), ["1"]);
        assert_eq!(values(&a, "//footnote<para> @| count"), ["1"]);
    }

    #[test]
    fn deixis_forward_edge() {
        let a = mount();
        assert_eq!(
            values(&a, "//footnote<deixis>->footnote::text"),
            ["The onym query language."]
        );
    }

    #[test]
    fn deixis_backlink_and_noted() {
        let a = mount();
        // The body is <noted> and its backlink leads to the callout's
        // enclosing paragraph context.
        assert_eq!(values(&a, "//footnote<noted>::onym"), ["n1"]);
        assert_eq!(values(&a, "//footnote<noted><-footnote @| count"), ["1"]);
    }

    #[test]
    fn ref_edge_and_backlink() {
        let a = mount();
        assert_eq!(values(&a, "//ref->ref::lemma"), ["Methods"]);
        assert_eq!(
            values(&a, "/section[::onym = \"methods\"]<-ref @| count"),
            ["1"]
        );
    }

    #[test]
    fn ref_renders_target_lemma() {
        let a = mount();
        // The OQL homecoming: a reference projects what it renders.
        assert_eq!(values(&a, "//ref::"), ["Methods", "ghost"]);
    }

    #[test]
    fn dangling_ref_trait_and_no_edge() {
        let a = mount();
        assert_eq!(values(&a, "//*<dangling>::target"), ["ghost"]);
        assert_eq!(values(&a, "//*<dangling>->* @| count"), ["0"]);
        assert_eq!(values(&a, "//*<dangling>;;;resolved"), ["false"]);
        assert_eq!(values(&a, "//ref[1];;;resolved"), ["true"]);
    }

    #[test]
    fn genoses_as_traits() {
        let a = mount();
        assert_eq!(values(&a, "//quotation<said>::text"), ["strange loops"]);
        assert_eq!(values(&a, "//quotation::genoses"), ["said"]);
    }

    #[test]
    fn stichoi_strophes_and_lines() {
        let a = mount();
        assert_eq!(values(&a, "//stichoi/strophe/stichos @| count"), ["2"]);
        assert_eq!(
            values(&a, "//stichoi/strophe/stichos[2]::"),
            ["A few paternal acres bound"]
        );
    }

    #[test]
    fn onym_locator_round_trip() {
        let a = mount();
        let hits = values(&a, "//section[::onym = \"intro\"]");
        assert_eq!(hits, ["/section(intro)"]);
    }

    #[test]
    fn dialektos_definition_rejected() {
        let err = AtrepAdapter::parse_str("@@@!atrep\n", Path::new(".")).unwrap_err();
        assert!(matches!(err, AtrepError::NotADocument));
    }

    #[test]
    fn unknown_dialect_is_a_clean_error() {
        let err = AtrepAdapter::parse_str("@@@!no-such-dialect\n\nHi.\n", Path::new("."))
            .unwrap_err();
        assert!(matches!(err, AtrepError::Atrep(_)));
    }

    #[test]
    fn autonym_heading_trait() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("auto.dia"),
            "@@@!atrep\n\n@=== entry\n@![(taxis)] lemma\nautonym\ngrammata\n!@\n@\"an entry\"@\n===@\n",
        )
        .unwrap();
        let doc = "@@@!auto\n\n@! Rose\nA flower.\n!@\n";
        std::fs::write(tmp.path().join("doc.atd"), doc).unwrap();
        let a = AtrepAdapter::parse_file(&tmp.path().join("doc.atd")).unwrap();
        // The autonym pinned the lemma as onym at mount.
        assert_eq!(values(&a, "//*<autonym>::onym"), ["Rose"]);
        assert_eq!(values(&a, "//entry::lemma"), ["Rose"]);
    }

    #[test]
    fn kanon_mounts_identically() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("doc.atd"), DOC).unwrap();
        // koine resolves from the embedded std, so kanonizo works
        // in the bare tempdir — except our doc has a dangling
        // deixis-free ref; full kanonizo rejects dangling refs, so
        // canonicalize a reduced doc instead.
        let reduced = "\
@@@!koine

@#() Intro
One.@^(n1)
#@(intro)

@^
Note text.
^@(n1)
";
        std::fs::write(tmp.path().join("r.atd"), reduced).unwrap();
        let kanon = atrep::kanonizo::kanonizo_file(&tmp.path().join("r.atd")).unwrap();
        let atk = atrep::dendron::serialize(&kanon.document);
        let a = AtrepAdapter::parse_str(&atk, tmp.path()).unwrap();
        // Canonical onyms (o1, ...) resolve exactly like authored
        // ones; taxis is already explicit in the kanon.
        assert_eq!(values(&a, "//section::taxis"), ["1"]);
        assert_eq!(values(&a, "//footnote<deixis>->footnote @| count"), ["1"]);
    }
}
