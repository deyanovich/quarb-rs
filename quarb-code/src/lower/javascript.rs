//! The JavaScript lowering table (tree-sitter-javascript).

use super::*;

const COMMENTS: &[&str] = &["comment"];

fn jsdoc(ts: &TreeSitterAdapter, i: usize) -> Option<String> {
    preceding_comments(ts, i, COMMENTS, &["decorator"], |t| t.starts_with("/**"))
}

/// Binding adoption for function-valued expressions:
/// `const lex = () => {}` (variable_declarator) and
/// `lex = () => {}` (assignment_expression).
fn adopt(ts: &TreeSitterAdapter, i: usize) -> Option<String> {
    adopted_name(ts, i, "variable_declarator", "name", "value", &["identifier"]).or_else(
        || adopted_name(ts, i, "assignment_expression", "left", "right", &["identifier"]),
    )
}

pub(crate) fn lower_node(ts: &TreeSitterAdapter, i: usize) -> Option<Lowered> {
    match ts.nodes()[i].kind {
        "function_declaration" | "generator_function_declaration" | "method_definition" => {
            Some(Lowered {
                construct: "function",
                name: field_text(ts, i, "name"),
                traits: FUNCTION,
                signature: signature(ts, i),
                doc: jsdoc(ts, i),
                callee: None,
                n_params: field_child(ts, i, "parameters").map(|p| count_params(ts, p)),
            })
        }
        "arrow_function" | "function_expression" | "function" => {
            let adopted = adopt(ts, i);
            Some(Lowered {
                construct: if adopted.is_some() { "function" } else { "lambda" },
                name: adopted,
                signature: signature(ts, i),
                n_params: field_child(ts, i, "parameters").map(|p| count_params(ts, p)),
                ..Lowered::anon("lambda", FUNCTION)
            })
        }
        "class_declaration" | "class" | "class_expression" => Some(Lowered {
            construct: "type",
            name: field_text(ts, i, "name").or_else(|| adopt(ts, i)),
            traits: TYPE,
            signature: signature(ts, i),
            doc: jsdoc(ts, i),
            callee: None,
            n_params: None,
        }),
        "field_definition" => Some(Lowered {
            construct: "field",
            name: field_text(ts, i, "property"),
            traits: NONE,
            signature: None,
            doc: jsdoc(ts, i),
            callee: None,
            n_params: None,
        }),
        "if_statement" => Some(Lowered::anon("if", CONDITIONAL)),
        "else_clause" => Some(Lowered::anon("else", NONE)),
        "switch_statement" => Some(Lowered::anon("switch", CONDITIONAL)),
        "for_statement" | "for_in_statement" => Some(Lowered::anon("for", LOOP)),
        "while_statement" | "do_statement" => Some(Lowered::anon("while", LOOP)),
        "call_expression" => Some(Lowered {
            callee: field_text(ts, i, "function"),
            ..Lowered::anon("call", CALL)
        }),
        "new_expression" => Some(Lowered {
            callee: field_text(ts, i, "constructor"),
            ..Lowered::anon("call", CALL)
        }),
        "import_statement" => Some(Lowered::anon("import", IMPORT)),
        _ => None,
    }
}
