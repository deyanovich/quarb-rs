//! The Rust lowering table (tree-sitter-rust).

use super::*;

const DOC_COMMENTS: &[&str] = &["line_comment", "block_comment"];
const SKIP: &[&str] = &["attribute_item"];

fn doc(ts: &TreeSitterAdapter, i: usize) -> Option<String> {
    preceding_comments(ts, i, DOC_COMMENTS, SKIP, |t| {
        t.starts_with("///") || t.starts_with("/**")
    })
}

fn named(
    ts: &TreeSitterAdapter,
    i: usize,
    construct: &'static str,
    traits: &'static [&'static str],
) -> Option<Lowered> {
    Some(Lowered {
        construct,
        name: field_text(ts, i, "name"),
        traits,
        signature: signature(ts, i),
        doc: doc(ts, i),
        callee: None,
        n_params: None,
    })
}

pub(crate) fn lower_node(ts: &TreeSitterAdapter, i: usize) -> Option<Lowered> {
    match ts.nodes()[i].kind {
        "function_item" => Some(Lowered {
            n_params: field_child(ts, i, "parameters").map(|p| count_params(ts, p)),
            ..named(ts, i, "function", FUNCTION)?
        }),
        "closure_expression" => {
            let adopted = adopted_name(ts, i, "let_declaration", "pattern", "value", &["identifier"]);
            Some(Lowered {
                construct: if adopted.is_some() { "function" } else { "lambda" },
                name: adopted,
                n_params: field_child(ts, i, "parameters").map(|p| count_params(ts, p)),
                ..Lowered::anon("lambda", FUNCTION)
            })
        }
        "struct_item" | "enum_item" | "union_item" | "trait_item" | "type_item" => {
            named(ts, i, "type", TYPE)
        }
        // An impl is the type's namespace continued: named by the
        // implemented type (`/Lexer/lex` is the program's own
        // address), generics stripped — names pass the filename
        // test.
        "impl_item" => Some(Lowered {
            construct: "impl",
            name: impl_type_name(ts, i),
            traits: NONE,
            signature: signature(ts, i),
            doc: doc(ts, i),
            callee: None,
            n_params: None,
        }),
        "mod_item" => named(ts, i, "module", MODULE),
        "field_declaration" | "enum_variant" => Some(Lowered {
            construct: "field",
            name: field_text(ts, i, "name"),
            traits: NONE,
            signature: None,
            doc: doc(ts, i),
            callee: None,
            n_params: None,
        }),
        "const_item" | "static_item" => Some(Lowered {
            construct: "constant",
            name: field_text(ts, i, "name"),
            traits: NONE,
            signature: None,
            doc: doc(ts, i),
            callee: None,
            n_params: None,
        }),
        "use_declaration" => Some(Lowered::anon("import", IMPORT)),
        "if_expression" => Some(Lowered::anon("if", CONDITIONAL)),
        "else_clause" => Some(Lowered::anon("else", NONE)),
        "match_expression" => Some(Lowered::anon("switch", CONDITIONAL)),
        "for_expression" => Some(Lowered::anon("for", LOOP)),
        "while_expression" => Some(Lowered::anon("while", LOOP)),
        "loop_expression" => Some(Lowered::anon("loop", LOOP)),
        "call_expression" => Some(Lowered {
            callee: field_text(ts, i, "function"),
            ..Lowered::anon("call", CALL)
        }),
        "macro_invocation" => Some(Lowered {
            callee: field_text(ts, i, "macro"),
            ..Lowered::anon("call", CALL)
        }),
        _ => None,
    }
}

/// The implemented type's name: field `type`, descended through
/// `generic_type` to its bare identifier (`impl Foo<T>` names
/// `Foo`).
fn impl_type_name(ts: &TreeSitterAdapter, i: usize) -> Option<String> {
    let mut t = field_child(ts, i, "type")?;
    if ts.nodes()[t].kind == "generic_type"
        && let Some(inner) = field_child(ts, t, "type")
    {
        t = inner;
    }
    Some(ts.text(quarb::NodeId(t as u64)).to_string())
}
