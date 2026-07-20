//! Source-code AST adapter for the Quarb query engine.
//!
//! The trait is named `AstAdapter`; this adapter takes the name
//! literally: a source file parses (tree-sitter) into its syntax
//! tree, and the tree is the arbor. Node kinds are the edge names
//! (`//function_item`, `//call_expression`), a node's value
//! (`::`) is its source text, and tree-sitter's *fields* become
//! properties — `::name` on a `function_item` is the function's
//! name, `::body` its block — so
//! `//function_item[::name = "main"]` reads as it should.
//!
//! Only *named* nodes appear (punctuation and keywords are
//! syntax, not structure). Metadata: `;;;kind`, `;;;field` (this
//! node's field name in its parent), `;;;start-line` /
//! `;;;end-line` (1-based), `;;;n-children`. //! Python, JavaScript, by extension (`rs`, `py`, `js`); the
//! grammar set is compile-time and easily grown.
//!
//! Composed (`qua --descend`), source files graft like JSON does
//! — `//function_item::name` over a whole directory tree is one
//! query across every parsed file.

use quarb::{AstAdapter, NodeId, Value};

mod ast_cache;
pub use ast_cache::Cache;

thread_local! {
    /// The AST cache for this thread, or `None` (uncached). Set once
    /// by the CLI from `--cache`; consulted by every `parse` call, so
    /// single-file and `--descend` (via quarb-compose) both benefit.
    static CACHE: std::cell::RefCell<Option<Cache>> = const { std::cell::RefCell::new(None) };
}

/// Enable or disable the persistent AST cache for this thread.
pub fn set_cache(cache: Option<Cache>) {
    CACHE.with(|c| *c.borrow_mut() = cache);
}

/// An error parsing a source file.
#[derive(Debug, thiserror::Error)]
pub enum CodeError {
    #[error("code: {0}")]
    Io(#[from] std::io::Error),
    #[error("code: no grammar for extension {0:?}")]
    Language(String),
    #[error("code: parse produced no tree")]
    Parse,
}

#[derive(Debug, PartialEq)]
pub(crate) struct Node {
    pub(crate) kind: &'static str,
    pub(crate) field: Option<&'static str>,
    pub(crate) parent: Option<NodeId>,
    pub(crate) children: Vec<NodeId>,
    /// Byte range into the source.
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
    /// Field name → child index, for `::field` properties.
    pub(crate) fields: Vec<(&'static str, usize)>,
}

/// A parsed source file, exposed as its syntax tree.
pub struct CodeAdapter {
    source: String,
    nodes: Vec<Node>,
}

/// The grammar for a file extension.
fn language(ext: &str) -> Option<tree_sitter::Language> {
    match ext {
        "rs" => Some(tree_sitter_rust::LANGUAGE.into()),
        "py" => Some(tree_sitter_python::LANGUAGE.into()),
        "js" | "mjs" | "cjs" | "jsx" => Some(tree_sitter_javascript::LANGUAGE.into()),
        "c" | "h" => Some(tree_sitter_c::LANGUAGE.into()),
        _ => None,
    }
}

/// Whether an extension has a grammar (for dispatch and grafting).
pub fn supported(ext: &str) -> bool {
    language(ext).is_some()
}

impl CodeAdapter {
    /// Parse source text as `ext`'s language. When the thread's AST
    /// cache is enabled (see [`set_cache`]), a cache hit for this
    /// exact content skips tree-sitter entirely; a miss parses and
    /// stores. Caching is transparent — the returned adapter is
    /// identical either way.
    pub fn parse(text: &str, ext: &str) -> Result<Self, CodeError> {
        if let Some(cache) = CACHE.with(|c| c.borrow().clone())
            && let Some(lang) = language(ext)
        {
            let tag = ast_cache::lang_tag(ext);
            if tag != 0 {
                let hash = quarb::sha256(text.as_bytes());
                if let Some(nodes) = ast_cache::load(&cache, tag, &lang, &hash, text) {
                    return Ok(CodeAdapter {
                        source: text.to_string(),
                        nodes,
                    });
                }
                let adapter = Self::parse_raw(text, ext)?;
                ast_cache::store(&cache, &adapter.nodes, tag, &lang, &hash, text.len() as u64);
                return Ok(adapter);
            }
        }
        Self::parse_raw(text, ext)
    }

    /// Parse without consulting the cache — the direct tree-sitter
    /// path, and the miss path of [`parse`].
    fn parse_raw(text: &str, ext: &str) -> Result<Self, CodeError> {
        let lang = language(ext).ok_or_else(|| CodeError::Language(ext.to_string()))?;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&lang)
            .map_err(|_| CodeError::Language(ext.to_string()))?;
        let tree = parser.parse(text, None).ok_or(CodeError::Parse)?;
        let mut nodes = Vec::new();
        build(&mut nodes, tree.root_node());
        Ok(CodeAdapter {
            source: text.to_string(),
            nodes,
        })
    }

    /// Parse a file, language by extension.
    pub fn open(path: &std::path::Path) -> Result<Self, CodeError> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let text = std::fs::read_to_string(path)?;
        Self::parse(&text, &ext)
    }

    /// A human-readable locator: `/kind[start-line]` chain.
    pub fn locator(&self, node: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(node);
        while let Some(n) = cur {
            let nd = &self.nodes[n.0 as usize];
            if nd.parent.is_some() {
                parts.push(format!("{}:{}", nd.kind, nd.start_line));
            }
            cur = nd.parent;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    fn text_of(&self, n: &Node) -> &str {
        &self.source[n.start.min(self.source.len())..n.end.min(self.source.len())]
    }
}

/// Intern the named nodes. Uses an explicit stack rather than
/// recursion so a deeply nested source file (thousands of levels)
/// can't overflow the call stack. Nodes are interned in the same
/// pre-order the recursive walk produced.
fn build(nodes: &mut Vec<Node>, root: tree_sitter::Node<'_>) {
    // (tree-sitter node, parent id, this node's field in its parent)
    let mut stack: Vec<(tree_sitter::Node<'_>, Option<NodeId>, Option<&'static str>)> =
        vec![(root, None, None)];
    while let Some((ts, parent, field)) = stack.pop() {
        let id = NodeId(nodes.len() as u64);
        nodes.push(Node {
            kind: ts.kind(),
            field,
            parent,
            children: Vec::new(),
            start: ts.start_byte(),
            end: ts.end_byte(),
            start_line: ts.start_position().row + 1,
            end_line: ts.end_position().row + 1,
            fields: Vec::new(),
        });
        // Record this node with its parent, preserving child order
        // and the field-name → child-index map.
        if let Some(p) = parent {
            let pnode = &mut nodes[p.0 as usize];
            if let Some(f) = field {
                pnode.fields.push((f, pnode.children.len()));
            }
            pnode.children.push(id);
        }
        // Collect named children, then push them reversed so they
        // pop — and get interned — in source order (a pre-order DFS,
        // matching the previous recursive numbering).
        let mut cursor = ts.walk();
        let mut kids = Vec::new();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.is_named() {
                    kids.push((child, cursor.field_name()));
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        for (child, f) in kids.into_iter().rev() {
            stack.push((child, Some(id), f));
        }
    }
}

impl AstAdapter for CodeAdapter {
    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.nodes[node.0 as usize].children.clone()
    }

    /// The node kind (`function_item`, `identifier`, ...); the
    /// root (source_file/module/program) stays unnamed so `/`
    /// starts at its children.
    fn name(&self, node: NodeId) -> Option<String> {
        let n = &self.nodes[node.0 as usize];
        n.parent.map(|_| n.kind.to_string())
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes[node.0 as usize].parent
    }

    /// The kind, as a trait too (`<function_item>` where a trait
    /// filter reads better than a name step).
    fn traits(&self, node: NodeId) -> Vec<String> {
        vec![self.nodes[node.0 as usize].kind.to_string()]
    }

    /// Tree-sitter fields: `::name` on a function is the name
    /// child's source text.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        let (_, idx) = n.fields.iter().find(|(f, _)| *f == name)?;
        let child = &self.nodes[n.children[*idx].0 as usize];
        Some(Value::Str(self.text_of(child).to_string()))
    }

    /// A node's source text.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        Some(Value::Str(
            self.text_of(&self.nodes[node.0 as usize]).to_string(),
        ))
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        match key {
            "kind" => Some(Value::Str(n.kind.to_string())),
            "field" => n.field.map(|f| Value::Str(f.to_string())),
            "start-line" => Some(Value::Int(n.start_line as i64)),
            "end-line" => Some(Value::Int(n.end_line as i64)),
            "n-children" => Some(Value::Int(n.children.len() as i64)),
            _ => None,
        }
    }
}
