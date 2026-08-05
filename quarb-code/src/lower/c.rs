//! The C lowering table (tree-sitter-c).

use super::*;

const COMMENTS: &[&str] = &["comment"];

fn doc(ts: &TreeSitterAdapter, i: usize) -> Option<String> {
    preceding_comments(ts, i, COMMENTS, &[], |_| true)
}

/// The declarator chain — C's name extraction, nailed: from the
/// `declarator` field, descend through `pointer_declarator`,
/// `function_declarator`, `array_declarator`, and
/// `parenthesized_declarator` (each via its own `declarator`
/// field, else its sole child) to the leaf identifier. So
/// `static int *helper(int a, int b)` names its node `helper` —
/// never the whole `helper(int a, int b)` the syntax level's
/// `::declarator` answers.
fn declarator_leaf(ts: &TreeSitterAdapter, start: usize) -> Option<usize> {
    let mut i = field_child(ts, start, "declarator")?;
    loop {
        if matches!(
            ts.nodes()[i].kind,
            "identifier" | "field_identifier" | "type_identifier"
        ) {
            return Some(i);
        }
        if let Some(d) = field_child(ts, i, "declarator") {
            i = d;
        } else if ts.nodes()[i].kind.ends_with("_declarator")
            && ts.nodes()[i].children.len() == 1
        {
            i = ts.nodes()[i].children[0].0 as usize;
        } else {
            return None;
        }
    }
}

fn chained_name(ts: &TreeSitterAdapter, i: usize) -> Option<String> {
    declarator_leaf(ts, i).map(|l| ts.text(quarb::NodeId(l as u64)).to_string())
}

/// The parameter list behind the declarator chain, for
/// `::::n-params`.
fn chained_params(ts: &TreeSitterAdapter, i: usize) -> Option<i64> {
    let mut d = field_child(ts, i, "declarator")?;
    loop {
        if ts.nodes()[d].kind == "function_declarator" {
            return field_child(ts, d, "parameters").map(|p| count_params(ts, p));
        }
        d = field_child(ts, d, "declarator")?;
    }
}

/// A named specifier with a body declares the type; a bodiless
/// one (`struct foo x;`) is a *reference* and dissolves.
fn specifier(ts: &TreeSitterAdapter, i: usize) -> Option<Lowered> {
    let name = field_text(ts, i, "name")?;
    field_child(ts, i, "body")?;
    Some(Lowered {
        construct: "type",
        name: Some(name),
        traits: TYPE,
        signature: None,
        doc: doc(ts, i),
        callee: None,
        n_params: None,
    })
}

pub(crate) fn lower_node(ts: &TreeSitterAdapter, i: usize) -> Option<Lowered> {
    match ts.nodes()[i].kind {
        "function_definition" => Some(Lowered {
            construct: "function",
            name: chained_name(ts, i),
            traits: FUNCTION,
            signature: signature(ts, i),
            doc: doc(ts, i),
            callee: None,
            n_params: chained_params(ts, i),
        }),
        "struct_specifier" | "enum_specifier" | "union_specifier" => specifier(ts, i),
        "type_definition" => Some(Lowered {
            construct: "type",
            name: chained_name(ts, i),
            traits: TYPE,
            signature: None,
            doc: doc(ts, i),
            callee: None,
            n_params: None,
        }),
        "enumerator" => Some(Lowered {
            construct: "field",
            name: field_text(ts, i, "name"),
            traits: NONE,
            signature: None,
            doc: None,
            callee: None,
            n_params: None,
        }),
        "field_declaration" => Some(Lowered {
            construct: "field",
            name: chained_name(ts, i),
            traits: NONE,
            signature: None,
            doc: doc(ts, i),
            callee: None,
            n_params: None,
        }),
        "preproc_def" | "preproc_function_def" => Some(Lowered {
            construct: "constant",
            name: field_text(ts, i, "name"),
            traits: NONE,
            signature: None,
            doc: doc(ts, i),
            callee: None,
            n_params: None,
        }),
        "preproc_include" => Some(Lowered::anon("import", IMPORT)),
        "if_statement" => Some(Lowered::anon("if", CONDITIONAL)),
        "else_clause" => Some(Lowered::anon("else", NONE)),
        "switch_statement" => Some(Lowered::anon("switch", CONDITIONAL)),
        "for_statement" => Some(Lowered::anon("for", LOOP)),
        "while_statement" | "do_statement" => Some(Lowered::anon("while", LOOP)),
        "call_expression" => Some(Lowered {
            callee: field_text(ts, i, "function"),
            ..Lowered::anon("call", CALL)
        }),
        _ => None,
    }
}
