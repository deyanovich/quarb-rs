//! The protocol-agnostic core: one set of reading gestures, two
//! codecs. Both transports decode into these calls and encode
//! the same answers.
//!
//! The engine is the single source of truth: every answer is a
//! code-level read — the outline is the named-node walk,
//! definition and references are name resolution over the
//! workspace forest (by identifier, fan-out on ambiguity — the
//! honest answer, exactly as ruling #31 resolves
//! `->definition`), hover is `::signature` + `::doc`.
//!
//! This is a READING tool by design: no rename, no completion,
//! no diagnostics — the write side stays the classic language
//! server's job.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use quarb::{AstAdapter, NodeId, Value};
use quarb_code::CodeModel;
use quarb_compose::{ComposeAdapter, SourceGraft};
use quarb_fs::{FsAdapter, FsOptions};
use serde::Serialize;

/// One symbol, flat form (kaivrpc rows; the JSON-RPC codec nests
/// them back into DocumentSymbols by `depth`).
#[derive(Debug, Clone, Serialize)]
pub struct Symbol {
    pub name: String,
    /// The vocabulary word (`function`, `type`, `impl`, …).
    pub construct: String,
    /// LSP SymbolKind — the lowering tables' informative column,
    /// live.
    pub kind: u32,
    /// Nesting depth under the file root (0 = top level).
    pub depth: u32,
    /// 1-based lines.
    pub start_line: u32,
    pub end_line: u32,
    /// 0-based byte columns on the start/end lines.
    pub start_col: u32,
    pub end_col: u32,
    pub signature: Option<String>,
}

/// One resolved location in the workspace.
#[derive(Debug, Clone, Serialize)]
pub struct Location {
    pub file: String,
    /// 1-based.
    pub line: u32,
    /// The code-level locator (`/dataclass/wrap/call`) — the
    /// path that tells the story; kaiv-native clients print it.
    pub locator: String,
}

/// An arbitrary code-level query's answer: node results become
/// locations, value results become printed values, and an
/// engine refusal arrives verbatim — what refuses here refuses
/// in `qua`.
pub enum QueryAnswer {
    Locations(Vec<Location>),
    Values(Vec<String>),
    Refused(String),
}

/// A hover answer: the declaration head and its documentation.
#[derive(Debug, Clone, Serialize)]
pub struct HoverRow {
    pub name: String,
    pub construct: String,
    pub signature: Option<String>,
    pub doc: Option<String>,
    pub file: String,
    pub line: u32,
}

/// The server state: overlay documents (editor buffers win over
/// disk) and the lazily-built workspace forest.
pub struct Workspace {
    root: Option<PathBuf>,
    /// uri → (path, model) for open documents.
    docs: HashMap<String, (PathBuf, CodeModel)>,
    forest: RefCell<Option<ComposeAdapter<FsAdapter>>>,
}

impl Workspace {
    pub fn new(root: Option<PathBuf>) -> Self {
        Workspace {
            root,
            docs: HashMap::new(),
            forest: RefCell::new(None),
        }
    }

    pub fn open(&mut self, uri: &str, text: &str) {
        let path = uri_to_path(uri);
        if let Ok(m) = CodeModel::parse(text, ext_of(&path)) {
            self.docs.insert(uri.to_string(), (path, m));
        }
    }

    pub fn close(&mut self, uri: &str) {
        self.docs.remove(uri);
    }

    /// A save invalidates the forest: the next workspace answer
    /// re-reads disk (per-file parses come back from the AST
    /// cache, so the rebuild costs only the changed file).
    pub fn saved(&self) {
        *self.forest.borrow_mut() = None;
    }

    pub fn model(&self, uri: &str) -> Option<&CodeModel> {
        self.docs.get(uri).map(|(_, m)| m)
    }

    /// The document outline: every named node, pre-order, with
    /// its nesting depth — Function becomes Method inside a type,
    /// straight from the lowering tables' LSP column.
    pub fn symbols(&self, uri: &str) -> Vec<Symbol> {
        let Some(model) = self.model(uri) else {
            return Vec::new();
        };
        let lines = LineIndex::new(model.source());
        let mut out = Vec::new();
        walk_symbols(model, model.root(), 0, false, &lines, &mut out);
        out
    }

    /// Declarations of `word` — the open buffer first (overlay
    /// wins), then the workspace forest. Fan-out is the honest
    /// answer for homonyms.
    pub fn definition(&self, uri: &str, word: &str) -> Vec<Location> {
        let mut out = Vec::new();
        if let Some((path, model)) = self.docs.get(uri) {
            for n in named_nodes(model) {
                if model.ident(n) == Some(word) {
                    out.push(Location {
                        file: path.display().to_string(),
                        line: model.line_span(n).0 as u32,
                        locator: model.locator(n),
                    });
                }
            }
        }
        if out.is_empty() {
            out = self.forest_query(&format!("//{}", quoted(word)), uri);
        }
        out
    }

    /// Call sites whose callee resolves to `word`, workspace-wide
    /// — find-references as a filter, not an index.
    pub fn references(&self, uri: &str, word: &str) -> Vec<Location> {
        let re = regex_escape(word);
        self.forest_query(
            &format!("//*<call>[::callee =~ /(^|[^A-Za-z0-9_$]){re}$/]"),
            uri,
        )
    }

    /// Workspace symbols: declarations matching `query` as a
    /// name prefix (empty query lists nothing — the census is a
    /// qua one-liner, not an editor payload).
    pub fn workspace_symbols(&self, query: &str, uri: &str) -> Vec<Location> {
        if query.is_empty() {
            return Vec::new();
        }
        self.forest_query(&format!("//~({}.*)", regex_escape(query)), uri)
    }

    /// Hover: the declaration head and doc of `word`, buffer
    /// first, forest second.
    pub fn hover(&self, uri: &str, word: &str) -> Vec<HoverRow> {
        if let Some((path, model)) = self.docs.get(uri) {
            let rows: Vec<HoverRow> = named_nodes(model)
                .into_iter()
                .filter(|&n| model.ident(n) == Some(word))
                .map(|n| HoverRow {
                    name: word.to_string(),
                    construct: model.construct(n).to_string(),
                    signature: str_prop(model, n, "signature"),
                    doc: str_prop(model, n, "doc"),
                    file: path.display().to_string(),
                    line: model.line_span(n).0 as u32,
                })
                .collect();
            if !rows.is_empty() {
                return rows;
            }
        }
        self.forest_hover(word, uri)
    }

    /// The query door: any code-level query, over the open file
    /// (`file_only`) or the workspace forest. This is the whole
    /// product in one method — the gestures above are the four
    /// questions every editor asks; this is the rest of them.
    pub fn query(&self, uri: &str, q: &str, file_only: bool) -> QueryAnswer {
        if file_only {
            let Some((path, model)) = self.docs.get(uri) else {
                return QueryAnswer::Refused("no file to query".into());
            };
            return match quarb::run(q, model) {
                Err(e) => QueryAnswer::Refused(e.to_string()),
                Ok(quarb::QueryResult::Values(vs)) => {
                    QueryAnswer::Values(vs.iter().map(|v| v.to_string()).collect())
                }
                Ok(quarb::QueryResult::Nodes(ns)) => QueryAnswer::Locations(
                    ns.into_iter()
                        .map(|n| Location {
                            file: path.display().to_string(),
                            line: model.line_span(n).0 as u32,
                            locator: model.locator(n),
                        })
                        .collect(),
                ),
            };
        }
        self.with_forest(uri, |forest| match quarb::run(q, forest) {
            Err(e) => QueryAnswer::Refused(e.to_string()),
            Ok(quarb::QueryResult::Values(vs)) => {
                QueryAnswer::Values(vs.iter().map(|v| v.to_string()).collect())
            }
            Ok(quarb::QueryResult::Nodes(ns)) => QueryAnswer::Locations(
                ns.into_iter()
                    .map(|n| {
                        let locator = forest
                            .locator(n, |o| forest.outer().path(o).display().to_string());
                        let (file, tail) = split_locator(&locator);
                        Location {
                            file,
                            line: int_meta(forest, n, "start-line"),
                            locator: tail,
                        }
                    })
                    .collect(),
            ),
        })
        .unwrap_or(QueryAnswer::Refused("no workspace root".into()))
    }

    // ---- the workspace forest ------------------------------------

    /// The root the forest mounts: the initialize rootUri, else
    /// the open document's parent directory.
    fn forest_root(&self, uri: &str) -> Option<PathBuf> {
        self.root.clone().or_else(|| {
            self.docs
                .get(uri)
                .and_then(|(p, _)| p.parent().map(Path::to_path_buf))
        })
    }

    fn with_forest<T>(
        &self,
        uri: &str,
        f: impl FnOnce(&ComposeAdapter<FsAdapter>) -> T,
    ) -> Option<T> {
        let mut slot = self.forest.borrow_mut();
        if slot.is_none() {
            let root = self.forest_root(uri)?;
            let fs = FsAdapter::with_options(&root, FsOptions::default()).ok()?;
            *slot = Some(
                ComposeAdapter::with_source_paths(fs, |fs, n| Some(fs.path(n)))
                    .with_source_graft(SourceGraft::Code),
            );
        }
        slot.as_ref().map(f)
    }

    fn forest_query(&self, query: &str, uri: &str) -> Vec<Location> {
        self.with_forest(uri, |forest| {
            let Ok(quarb::QueryResult::Nodes(ns)) = quarb::run(query, forest) else {
                return Vec::new();
            };
            ns.into_iter()
                .map(|n| {
                    let locator =
                        forest.locator(n, |o| forest.outer().path(o).display().to_string());
                    let (file, tail) = split_locator(&locator);
                    Location {
                        file,
                        line: int_meta(forest, n, "start-line"),
                        locator: tail,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
    }

    fn forest_hover(&self, word: &str, uri: &str) -> Vec<HoverRow> {
        self.with_forest(uri, |forest| {
            let q = format!("//{}", quoted(word));
            let Ok(quarb::QueryResult::Nodes(ns)) = quarb::run(&q, forest) else {
                return Vec::new();
            };
            ns.into_iter()
                .take(8)
                .map(|n| {
                    let locator =
                        forest.locator(n, |o| forest.outer().path(o).display().to_string());
                    let (file, _) = split_locator(&locator);
                    HoverRow {
                        name: word.to_string(),
                        construct: str_meta(forest, n, "construct"),
                        signature: value_str(forest.property(n, "signature")),
                        doc: value_str(forest.property(n, "doc")),
                        file,
                        line: int_meta(forest, n, "start-line"),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
    }
}

// ---- helpers --------------------------------------------------------

/// The identifier under the cursor: scan the line for the word
/// containing 0-based character `col` (columns as Unicode
/// scalars; identifiers are ASCII in the grammars we lower, so
/// UTF-16 drift cannot split one).
pub fn word_at(text: &str, line: u32, col: u32) -> Option<String> {
    let line = text.lines().nth(line as usize)?;
    let chars: Vec<char> = line.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut i = (col as usize).min(chars.len());
    if i >= chars.len() || !is_word(chars[i]) {
        i = i.checked_sub(1)?;
    }
    if !is_word(*chars.get(i)?) {
        return None;
    }
    let mut start = i;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = i;
    while end + 1 < chars.len() && is_word(chars[end + 1]) {
        end += 1;
    }
    Some(chars[start..=end].iter().collect())
}

/// LSP SymbolKind from the lowering tables' informative column.
/// Function is Method inside a type or impl — computed here, by
/// position, exactly as the spec's one sentence says.
pub fn symbol_kind(construct: &str, kind: &str, inside_type: bool) -> u32 {
    match construct {
        "function" | "lambda" if inside_type => 6, // Method
        "function" | "lambda" => 12,               // Function
        "impl" => 19,                              // Object
        "module" => 2,                             // Module
        "constant" => 14,                          // Constant
        "field" => match kind {
            "enum_variant" | "enumerator" => 22, // EnumMember
            _ => 8,                              // Field
        },
        "type" => match kind {
            k if k.starts_with("struct") || k.starts_with("union") => 23, // Struct
            k if k.starts_with("enum") => 10,                             // Enum
            "trait_item" => 11,                                           // Interface
            _ => 5,                                                       // Class
        },
        _ => 13, // Variable — unreachable for vocabulary constructs
    }
}

fn walk_symbols(
    model: &CodeModel,
    node: NodeId,
    depth: u32,
    inside_type: bool,
    lines: &LineIndex,
    out: &mut Vec<Symbol>,
) {
    for child in model.children(node) {
        let named = model.ident(child).is_some();
        let container = matches!(model.construct(child), "type" | "impl");
        if let Some(ident) = model.ident(child) {
            let (sl, el) = model.line_span(child);
            let (start, end) = model.span(child);
            out.push(Symbol {
                name: ident.to_string(),
                construct: model.construct(child).to_string(),
                kind: symbol_kind(
                    model.construct(child),
                    &str_meta(model, child, "kind"),
                    inside_type,
                ),
                depth,
                start_line: sl as u32,
                end_line: el as u32,
                start_col: lines.col(start, sl) as u32,
                end_col: lines.col(end, el) as u32,
                signature: str_prop(model, child, "signature"),
            });
        }
        walk_symbols(
            model,
            child,
            depth + u32::from(named),
            container || (inside_type && !named),
            lines,
            out,
        );
    }
}

fn named_nodes(model: &CodeModel) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![model.root()];
    while let Some(n) = stack.pop() {
        if model.ident(n).is_some() {
            out.push(n);
        }
        stack.extend(model.children(n));
    }
    out.sort();
    out
}

/// Byte-offset → column resolution for one source text.
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        LineIndex { starts }
    }

    /// 0-based byte column of `offset` on 1-based line `line`.
    fn col(&self, offset: usize, line: usize) -> usize {
        self.starts
            .get(line - 1)
            .map_or(0, |s| offset.saturating_sub(*s))
    }
}

fn ext_of(path: &Path) -> &str {
    path.extension().and_then(|e| e.to_str()).unwrap_or("")
}

pub fn uri_to_path(uri: &str) -> PathBuf {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    // Percent-decoding for the common case (spaces); full URI
    // handling is the editor's job — file URIs from real editors
    // are plain paths.
    PathBuf::from(raw.replace("%20", " "))
}

/// Split a forest locator at the graft bang: file, code path.
fn split_locator(locator: &str) -> (String, String) {
    match locator.split_once('!') {
        Some((file, tail)) => (file.to_string(), tail.to_string()),
        None => (locator.to_string(), String::new()),
    }
}

/// An identifier as a quoted literal name step.
fn quoted(word: &str) -> String {
    format!("'{}'", word.replace('\'', ""))
}

fn regex_escape(word: &str) -> String {
    word.chars()
        .flat_map(|c| {
            if c.is_alphanumeric() || c == '_' {
                vec![c]
            } else {
                vec!['\\', c]
            }
        })
        .collect()
}

fn value_str(v: Option<Value>) -> Option<String> {
    match v {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    }
}

fn str_prop(a: &impl AstAdapter, n: NodeId, key: &str) -> Option<String> {
    value_str(a.property(n, key))
}

fn str_meta(a: &impl AstAdapter, n: NodeId, key: &str) -> String {
    match a.metadata(n, key) {
        Some(Value::Str(s)) => s,
        _ => String::new(),
    }
}

fn int_meta(a: &impl AstAdapter, n: NodeId, key: &str) -> u32 {
    match a.metadata(n, key) {
        Some(Value::Int(i)) => i as u32,
        _ => 0,
    }
}
