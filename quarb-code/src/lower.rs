//! The tree-sitter producer: lower a backend parse into the code
//! level's `Decl` stream.
//!
//! One O(n) pass over the pre-order node vector. A node whose
//! kind is absent from its grammar's table contributes no `Decl`;
//! its children hoist to its code-level parent in source order —
//! the dissolve rule. ERROR and MISSING kinds are unmapped by
//! construction, so broken parses degrade gracefully.

use crate::{Decl, Lang};
use quarb_tree_sitter::TreeSitterAdapter;

mod c;
mod javascript;
mod python;
mod rust;

pub(crate) const FUNCTION: &[&str] = &["function"];
pub(crate) const TYPE: &[&str] = &["type"];
pub(crate) const MODULE: &[&str] = &["module"];
pub(crate) const LOOP: &[&str] = &["loop"];
pub(crate) const CONDITIONAL: &[&str] = &["conditional"];
pub(crate) const CALL: &[&str] = &["call"];
pub(crate) const IMPORT: &[&str] = &["import"];
pub(crate) const NONE: &[&str] = &[];

/// A grammar table's verdict on one node.
pub(crate) struct Lowered {
    pub construct: &'static str,
    pub name: Option<String>,
    pub traits: &'static [&'static str],
    pub signature: Option<String>,
    pub doc: Option<String>,
    pub callee: Option<String>,
    pub n_params: Option<i64>,
}

impl Lowered {
    pub(crate) fn anon(construct: &'static str, traits: &'static [&'static str]) -> Self {
        Lowered {
            construct,
            name: None,
            traits,
            signature: None,
            doc: None,
            callee: None,
            n_params: None,
        }
    }
}

pub(crate) fn lower(ts: &TreeSitterAdapter, lang: Lang) -> Vec<Decl> {
    let nodes = ts.nodes();
    let mut decls: Vec<Decl> = Vec::new();
    // The code-level parent governing each ts node's children:
    // a mapped node's entry is its own decl, a dissolved node
    // inherits its parent's — hoisting in one lookup.
    let mut code_parent: Vec<Option<usize>> = vec![None; nodes.len()];
    for i in 1..nodes.len() {
        let ts_parent = nodes[i].parent.expect("non-root has a parent").0 as usize;
        let inherited = code_parent[ts_parent];
        let lowered = match lang {
            Lang::Rust => rust::lower_node(ts, i),
            Lang::Python => python::lower_node(ts, i),
            Lang::Javascript => javascript::lower_node(ts, i),
            Lang::C => c::lower_node(ts, i),
        };
        match lowered {
            None => code_parent[i] = inherited,
            Some(l) => {
                let n = &nodes[i];
                decls.push(Decl {
                    parent: inherited,
                    construct: l.construct,
                    name: l.name,
                    traits: l.traits,
                    kind: n.kind.to_string(),
                    span: (n.start, n.end),
                    lines: (n.start_line, n.end_line),
                    signature: l.signature,
                    doc: l.doc,
                    callee: l.callee,
                    n_params: l.n_params,
                });
                code_parent[i] = Some(decls.len() - 1);
            }
        }
    }
    decls
}

// ---- shared helpers -------------------------------------------------

/// The child index behind field `name` of node `i`, if any.
pub(crate) fn field_child(ts: &TreeSitterAdapter, i: usize, name: &str) -> Option<usize> {
    let n = &ts.nodes()[i];
    let (_, idx) = n.fields.iter().find(|(f, _)| *f == name)?;
    Some(n.children[*idx].0 as usize)
}

/// The source text behind field `name` of node `i`.
pub(crate) fn field_text(ts: &TreeSitterAdapter, i: usize, name: &str) -> Option<String> {
    field_child(ts, i, name).map(|c| ts.text(quarb::NodeId(c as u64)).to_string())
}

/// Whitespace-collapsed text: every run of whitespace becomes one
/// space.
pub(crate) fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The declaration head (`::signature`): the node's text up to
/// its `body` field, whitespace-collapsed, trailing `:`
/// stripped (Python's block colon). `None` when the node has no
/// body.
pub(crate) fn signature(ts: &TreeSitterAdapter, i: usize) -> Option<String> {
    let body = field_child(ts, i, "body")?;
    let n = &ts.nodes()[i];
    let head_end = ts.nodes()[body].start.max(n.start).min(n.end);
    let head = &ts.source()[n.start..head_end];
    let collapsed = collapse(head);
    let trimmed = collapsed.trim_end().trim_end_matches(':').trim_end();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Declared parameter count for the parameter-list node `params`:
/// its named children, minus C's bare `void`.
pub(crate) fn count_params(ts: &TreeSitterAdapter, params: usize) -> i64 {
    ts.nodes()[params]
        .children
        .iter()
        .filter(|c| {
            let n = &ts.nodes()[c.0 as usize];
            !(n.kind == "parameter_declaration" && ts.text(**c).trim() == "void")
        })
        .count() as i64
}

/// The comment run directly above node `i` (each piece ending on
/// the line above the next), markers stripped per line, joined
/// with newlines. `comment_kinds` filters which sibling kinds
/// count; `skip_kinds` (attributes, decorators) may sit between
/// the run and the declaration.
pub(crate) fn preceding_comments(
    ts: &TreeSitterAdapter,
    i: usize,
    comment_kinds: &[&str],
    skip_kinds: &[&str],
    keep: impl Fn(&str) -> bool,
) -> Option<String> {
    let parent = ts.nodes()[i].parent?;
    let siblings = &ts.nodes()[parent.0 as usize].children;
    let pos = siblings.iter().position(|c| c.0 as usize == i)?;
    let mut expected_line = ts.nodes()[i].start_line;
    let mut pieces: Vec<&str> = Vec::new();
    for sib in siblings[..pos].iter().rev() {
        let s = &ts.nodes()[sib.0 as usize];
        if skip_kinds.contains(&s.kind) {
            expected_line = s.start_line;
            continue;
        }
        if !comment_kinds.contains(&s.kind) || s.end_line + 1 < expected_line {
            break;
        }
        let text = ts.text(*sib);
        if !keep(text) {
            break;
        }
        pieces.push(text);
        expected_line = s.start_line;
    }
    if pieces.is_empty() {
        return None;
    }
    pieces.reverse();
    let doc = strip_comment_markers(&pieces.join("\n"));
    (!doc.is_empty()).then_some(doc)
}

/// Strip comment markers per line (`///`, `//!`, `//`, `/*`,
/// `*/`, leading `*`, `#`), trim, and drop blank edges.
pub(crate) fn strip_comment_markers(text: &str) -> String {
    let lines: Vec<String> = text
        .lines()
        .map(|l| {
            let l = l.trim();
            let l = l.strip_suffix("*/").unwrap_or(l);
            let l = ["///", "//!", "/**", "/*", "//", "#"]
                .iter()
                .find_map(|m| l.strip_prefix(m))
                .unwrap_or(l);
            let l = l.trim_start_matches('*');
            l.trim().to_string()
        })
        .collect();
    let start = lines.iter().position(|l| !l.is_empty()).unwrap_or(0);
    let end = lines.iter().rposition(|l| !l.is_empty()).map_or(0, |e| e + 1);
    lines[start..end.max(start)].join("\n")
}

/// Binding adoption: when node `i` sits directly in a binding —
/// `binding_kind`'s `value_field` — with an identifier on its
/// `name_field`, that identifier is adopted as the node's name.
pub(crate) fn adopted_name(
    ts: &TreeSitterAdapter,
    i: usize,
    binding_kind: &str,
    name_field: &str,
    value_field: &str,
    name_kinds: &[&str],
) -> Option<String> {
    let parent = ts.nodes()[i].parent?.0 as usize;
    if ts.nodes()[parent].kind != binding_kind
        || field_child(ts, parent, value_field) != Some(i)
    {
        return None;
    }
    let name = field_child(ts, parent, name_field)?;
    name_kinds
        .contains(&ts.nodes()[name].kind)
        .then(|| ts.text(quarb::NodeId(name as u64)).to_string())
}
