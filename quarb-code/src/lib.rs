//! The code level for the Quarb query engine.
//!
//! Cross-language code navigation above the syntax level
//! (`quarb-tree-sitter`): **function names are node names, not
//! properties**. `/lexer/lex/is_name_char` descends module,
//! function, nested function — a filepath into the program —
//! where the syntax level spells the same question
//! `//function_item[::name = "lex"]`.
//!
//! - **Names.** A declaration's edge name is its declared
//!   identifier; every other construct in the vocabulary is named
//!   by its normalized keyword (`if`, `switch`, `for`, `call`);
//!   everything else dissolves — children hoist, as the text
//!   level dissolves markup soup. A nameless function-valued
//!   expression adopts the identifier of the binding receiving
//!   it (`const lex = () => {}` is a function named `lex`).
//! - **Traits** classify: `<function>`, `<type>`, `<module>`,
//!   `<loop>`, `<conditional>`, `<call>`, `<import>`.
//! - **Properties** are uniform: `::signature` (the declaration
//!   head), `::doc` (attached documentation), `::callee` (on
//!   calls); bare `::` is the node's source text.
//! - **Annotations**: `:::kind` (the raw backend kind — the only
//!   place tree-sitter vocabulary survives), `:::construct`,
//!   `:::start-line` / `:::end-line`, `:::lang`,
//!   `:::n-children`, `:::n-params` — every one aliased to
//!   `::` (the surface is closed).
//! - **Crosslinks**: every `call` carries `->definition` edges to
//!   the same-file declarations matching its callee;
//!   `//lex<-definition` is find-references.
//!
//! The vocabulary and the per-grammar lowering tables are ruled
//! in the spec (The Code Level) and doubled as
//! conformance fixtures in this crate's tests. Grammars: Rust,
//! Python, JavaScript, C — the syntax level's set, each nailed.

use quarb::{AstAdapter, NodeId, Value};
use std::collections::HashMap;

mod lower;

/// A grammar of the code level's set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Python,
    Javascript,
    C,
}

impl Lang {
    /// The `:::lang` spelling.
    pub fn name(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::Javascript => "javascript",
            Lang::C => "c",
        }
    }
}

/// The grammar for a file extension (lowercased).
pub fn lang_for_ext(ext: &str) -> Option<Lang> {
    match ext {
        "rs" => Some(Lang::Rust),
        "py" => Some(Lang::Python),
        "js" | "mjs" | "cjs" | "jsx" => Some(Lang::Javascript),
        "c" | "h" => Some(Lang::C),
        _ => None,
    }
}

/// Whether an extension has a code-level lowering (for dispatch
/// and grafting). Agrees with `quarb_tree_sitter::supported`.
pub fn supported(ext: &str) -> bool {
    lang_for_ext(ext).is_some()
}

/// An error reading a source file at the code level.
#[derive(Debug, thiserror::Error)]
pub enum CodeError {
    #[error("code: {0}")]
    Io(#[from] std::io::Error),
    #[error(
        "code: the code model has no reading for extension {0:?} (rs, py, js, mjs, cjs, jsx, c, h)"
    )]
    Language(String),
    #[error(transparent)]
    Backend(#[from] quarb_tree_sitter::TreeSitterError),
    #[error("code: {0}")]
    Encoding(String),
}

/// One lowered construct — the code level's producer seam, the
/// parallel of `quarb_text::Block`. A producer emits `Decl`s in
/// pre-order (a parent precedes its children);
/// [`CodeModel::build`] derives the arbor. The tree-sitter
/// producer lives in this crate; another backend supplies the
/// same stream and nothing above it moves.
#[derive(Debug)]
pub struct Decl {
    /// Index of the parent `Decl`, or `None` for a top-level one.
    pub parent: Option<usize>,
    /// The vocabulary word: `function`, `type`, `if`, `call`, …
    pub construct: &'static str,
    /// The declared (or adopted) identifier, where one exists.
    pub name: Option<String>,
    /// Curated trait set — never backend kinds.
    pub traits: &'static [&'static str],
    /// The raw backend kind; surfaces only as `:::kind`.
    pub kind: String,
    /// Byte range into the source.
    pub span: (usize, usize),
    /// 1-based start/end lines.
    pub lines: (usize, usize),
    /// The declaration head, whitespace-collapsed (`::signature`).
    pub signature: Option<String>,
    /// Attached documentation, markers stripped (`::doc`).
    pub doc: Option<String>,
    /// A call's callee text (`::callee`).
    pub callee: Option<String>,
    /// Declared parameter count, functions only (`:::n-params`).
    pub n_params: Option<i64>,
}

struct Node {
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    construct: &'static str,
    name: Option<String>,
    traits: &'static [&'static str],
    kind: String,
    span: (usize, usize),
    lines: (usize, usize),
    signature: Option<String>,
    doc: Option<String>,
    callee: Option<String>,
    n_params: Option<i64>,
}

/// A source file read at the code level.
pub struct CodeModel {
    source: String,
    lang: Lang,
    nodes: Vec<Node>,
    /// Identifier → the named function and type declarations, in
    /// document order: a call's `->definition` targets. One list
    /// per identifier, shared by every call of it, so a file with
    /// a thousand same-named declarations and a thousand calls
    /// holds two thousand ids, not a million.
    defs_by_ident: HashMap<String, Vec<NodeId>>,
    /// Identifier → the calls whose callee ends in it: a
    /// declaration's `<-definition` sources.
    calls_by_ident: HashMap<String, Vec<NodeId>>,
}

/// Every annotation key answers at `::` too: the property
/// surface is closed (a source file cannot mint a property —
/// identifiers become names), so the alias rule applies in full.
/// Three colons stay the portable spelling (the adapter rung).
const ALIASED: &[&str] = &[
    "kind",
    "construct",
    "start-line",
    "end-line",
    "lang",
    "n-params",
];

impl CodeModel {
    /// Derive the arbor from a producer's `Decl` stream — the
    /// seam. `decls` must be pre-order: a parent precedes its
    /// children.
    pub fn build(source: String, lang: Lang, decls: Vec<Decl>) -> Self {
        let mut nodes = Vec::with_capacity(decls.len() + 1);
        // nodes[0]: the unnamed file root.
        nodes.push(Node {
            parent: None,
            children: Vec::new(),
            construct: "",
            name: None,
            traits: &[],
            kind: String::new(),
            span: (0, source.len()),
            lines: (1, source.lines().count().max(1)),
            signature: None,
            doc: None,
            callee: None,
            n_params: None,
        });
        for d in decls {
            let id = NodeId(nodes.len() as u64);
            let parent = NodeId(d.parent.map_or(0, |p| p as u64 + 1));
            nodes.push(Node {
                parent: Some(parent),
                children: Vec::new(),
                construct: d.construct,
                name: d.name,
                traits: d.traits,
                kind: d.kind,
                span: d.span,
                lines: d.lines,
                signature: d.signature,
                doc: d.doc,
                callee: d.callee,
                n_params: d.n_params,
            });
            nodes[parent.0 as usize].children.push(id);
        }
        let mut model = CodeModel {
            source,
            lang,
            nodes,
            defs_by_ident: HashMap::new(),
            calls_by_ident: HashMap::new(),
        };
        model.link_definitions();
        model
    }

    /// Resolve every call's callee against the file's named
    /// function and type declarations — `->definition`, by
    /// identifier. Unresolved callees carry no edge; an ambiguous
    /// identifier fans out to every match.
    fn link_definitions(&mut self) {
        for (i, n) in self.nodes.iter().enumerate() {
            if matches!(n.construct, "function" | "type")
                && let Some(name) = &n.name
            {
                self.defs_by_ident
                    .entry(name.clone())
                    .or_default()
                    .push(NodeId(i as u64));
            }
            if let Some(callee) = &n.callee
                && let Some(ident) = trailing_ident(callee)
            {
                self.calls_by_ident
                    .entry(ident.to_string())
                    .or_default()
                    .push(NodeId(i as u64));
            }
        }
        // Only calls that resolve count as sources.
        self.calls_by_ident
            .retain(|ident, _| self.defs_by_ident.contains_key(ident));
    }

    /// Read `text` as `ext`'s language at the code level: the
    /// backend parse (cached when the thread's AST cache is
    /// enabled — see `quarb_tree_sitter::set_cache`) lowers
    /// through the grammar's table.
    pub fn parse(text: &str, ext: &str) -> Result<Self, CodeError> {
        let ext = ext.to_ascii_lowercase();
        let lang = lang_for_ext(&ext).ok_or_else(|| CodeError::Language(ext.clone()))?;
        let ts = quarb_tree_sitter::TreeSitterAdapter::parse(text, &ext)?;
        let decls = lower::lower(&ts, lang);
        Ok(Self::build(text.to_string(), lang, decls))
    }

    /// Read a file at the code level, language by extension.
    pub fn open(path: &std::path::Path) -> Result<Self, CodeError> {
        Self::open_with_decoding(path, &quarb::source::SourceDecoding::default())
    }

    /// [`open`](Self::open) under a mount's source-boundary rule:
    /// the file decodes by `?encoding=`, a byte-order mark, else
    /// strict UTF-8 — refused with the byte offset otherwise.
    pub fn open_with_decoding(
        path: &std::path::Path,
        decoding: &quarb::source::SourceDecoding,
    ) -> Result<Self, CodeError> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let bytes = std::fs::read(path)?;
        let name = path.display().to_string();
        let text = decoding
            .decode(&name, &bytes)
            .map_err(|e| CodeError::Encoding(format!("{name}: {e}")))?
            .text;
        Self::parse(&text, &ext)
    }

    /// A human-readable locator: name-or-construct segments, a
    /// `[n]` index only among same-label siblings —
    /// `/lexer/lex/is_name_char`, `/main/for/call[3]`.
    pub fn locator(&self, node: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = node;
        while let Some(parent) = self.nodes[cur.0 as usize].parent {
            parts.push(self.segment(parent, cur));
            cur = parent;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    fn label(&self, node: NodeId) -> &str {
        let n = &self.nodes[node.0 as usize];
        n.name.as_deref().unwrap_or(n.construct)
    }

    fn segment(&self, parent: NodeId, child: NodeId) -> String {
        let label = self.label(child);
        let same: Vec<NodeId> = self.nodes[parent.0 as usize]
            .children
            .iter()
            .copied()
            .filter(|&c| self.label(c) == label)
            .collect();
        if same.len() > 1 {
            let pos = same.iter().position(|&c| c == child).unwrap() + 1;
            format!("{label}[{pos}]")
        } else {
            label.to_string()
        }
    }

    fn text_of(&self, n: &Node) -> &str {
        &self.source[n.span.0.min(self.source.len())..n.span.1.min(self.source.len())]
    }

    // ---- inspection API (the door quarb-code-lsp reads through) ----

    /// The parsed source text.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The grammar this model was lowered from.
    pub fn lang(&self) -> Lang {
        self.lang
    }

    /// The declared (or adopted) identifier — `None` on anonymous
    /// constructs and the file root. Distinguishes a function
    /// named `switch` from the construct: the identifier is a
    /// stored fact, not a name-string comparison.
    pub fn ident(&self, node: NodeId) -> Option<&str> {
        self.nodes[node.0 as usize].name.as_deref()
    }

    /// The vocabulary word (`""` on the file root).
    pub fn construct(&self, node: NodeId) -> &str {
        self.nodes[node.0 as usize].construct
    }

    /// Byte range into the source.
    pub fn span(&self, node: NodeId) -> (usize, usize) {
        self.nodes[node.0 as usize].span
    }

    /// 1-based start/end lines.
    pub fn line_span(&self, node: NodeId) -> (usize, usize) {
        self.nodes[node.0 as usize].lines
    }
}

/// The trailing identifier of a callee text — `Type::method`,
/// `obj.method`, and `path.to.f` all resolve by their last
/// segment.
/// The segment before the callee's last one (`Type` in
/// `Type::new`, `obj` in `obj.method`), when the callee is
/// qualified; the receiver keywords (`self`, `this`, `super`,
/// `crate`) qualify nothing.
fn qualifier(callee: &str) -> Option<&str> {
    let end = callee.trim_end_matches(['!', '?']);
    let last = trailing_ident(end)?;
    let head = end[..end.len() - last.len()]
        .trim_end_matches("::")
        .trim_end_matches('.');
    if head.len() == end.len() - last.len() {
        return None;
    }
    let q = trailing_ident(head)?;
    (!matches!(q, "self" | "this" | "super" | "crate" | "Self")).then_some(q)
}

fn trailing_ident(callee: &str) -> Option<&str> {
    let end = callee.trim_end_matches(['!', '?']);
    let start = end
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_' || *c == '$')
        .last()
        .map(|(i, _)| i)?;
    Some(&end[start..])
}

impl AstAdapter for CodeModel {
    /// This source defines no references to resolve.
    fn resolves_references(&self) -> bool {
        false
    }

    fn root(&self) -> NodeId {
        NodeId(0)
    }

    /// In memory: an unscoped reverse resolution or value match may
    /// walk the arbor.
    fn walks_for_references(&self) -> bool {
        true
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.nodes[node.0 as usize].children.clone()
    }

    /// The declared identifier, else the construct word; the
    /// file root stays unnamed.
    fn name(&self, node: NodeId) -> Option<String> {
        let n = &self.nodes[node.0 as usize];
        n.parent?;
        Some(n.name.clone().unwrap_or_else(|| n.construct.to_string()))
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes[node.0 as usize].parent
    }

    /// The kind first (spec: Traits): the construct word (`function`,
    /// `if`, `else`, `impl`, `field`, …), the file root's being
    /// `file`; then the category the grammar table gives it
    /// (`loop`, `conditional`).
    fn traits(&self, node: NodeId) -> Vec<String> {
        let n = &self.nodes[node.0 as usize];
        let kind = if n.construct.is_empty() {
            "file"
        } else {
            n.construct
        };
        let mut out = vec![kind.to_string()];
        for t in n.traits {
            if *t != kind {
                out.push(t.to_string());
            }
        }
        out
    }

    /// The uniform property set: `::signature`, `::doc`,
    /// `::callee` — the identifier is NOT a property (it is the
    /// name; `:name` answers it). Aliased annotation keys fall
    /// through to `metadata` via [`AstAdapter::aliased_metadata`].
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        match name {
            "signature" => n.signature.clone().map(Value::Str),
            "doc" => n.doc.clone().map(Value::Str),
            "callee" => n.callee.clone().map(Value::Str),
            _ => None,
        }
    }

    /// A node's source text.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        Some(Value::Str(
            self.text_of(&self.nodes[node.0 as usize]).to_string(),
        ))
    }

    /// The `:::` keys this adapter answers (`:meta` lists the ones

    /// a node answers).

    fn metadata_catalogue(&self) -> &'static [&'static str] {
        &[
            "kind",
            "construct",
            "start-line",
            "end-line",
            "lang",
            "n-params",
            "function",
        ]
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        match key {
            // The raw backend kind — the escape hatch, and the
            // only place backend vocabulary survives.
            "kind" => (!n.kind.is_empty()).then(|| Value::Str(n.kind.clone())),
            "construct" => (!n.construct.is_empty()).then(|| Value::Str(n.construct.to_string())),
            "start-line" => Some(Value::Int(n.lines.0 as i64)),
            "end-line" => Some(Value::Int(n.lines.1 as i64)),
            "lang" => Some(Value::Str(self.lang.name().to_string())),
            "n-params" => n.n_params.map(Value::Int),
            _ => None,
        }
    }

    fn aliased_metadata(&self, _node: NodeId) -> &'static [&'static str] {
        ALIASED
    }

    /// `->definition`: a call's edges to the declarations its
    /// callee resolves to (same file, by identifier).
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let n = &self.nodes[node.0 as usize];
        let Some(callee) = n.callee.as_deref() else {
            return Vec::new();
        };
        let Some(ident) = trailing_ident(callee) else {
            return Vec::new();
        };
        let Some(targets) = self.defs_by_ident.get(ident) else {
            return Vec::new();
        };
        // A qualified callee (`Type::new`, `module.helper`,
        // `self.run`) prefers the declarations whose enclosing
        // declaration bears the qualifier's last segment; only an
        // unqualified callee, or a qualifier nothing in the file
        // answers to, fans out to every same-named declaration.
        let qualified: Vec<NodeId> = match qualifier(callee) {
            Some(q) => targets
                .iter()
                .copied()
                .filter(|&t| {
                    self.nodes[t.0 as usize]
                        .parent
                        .and_then(|p| self.nodes[p.0 as usize].name.as_deref())
                        == Some(q)
                })
                .collect(),
            None => Vec::new(),
        };
        let chosen: &[NodeId] = if qualified.is_empty() {
            targets
        } else {
            &qualified
        };
        chosen
            .iter()
            .map(|&t| ("definition".to_string(), t))
            .collect()
    }

    /// `<-definition`: find-references — every call site whose
    /// callee resolves here.
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let n = &self.nodes[node.0 as usize];
        if !matches!(n.construct, "function" | "type") {
            return Vec::new();
        }
        n.name
            .as_deref()
            .and_then(|name| self.calls_by_ident.get(name))
            .map(|sources| {
                sources
                    .iter()
                    .map(|&s| ("definition".to_string(), s))
                    .collect()
            })
            .unwrap_or_default()
    }
}
