//! The Python lowering table (tree-sitter-python).

use super::*;

/// The docstring: the body block's first statement, when it is a
/// bare string — quotes stripped.
fn docstring(ts: &TreeSitterAdapter, i: usize) -> Option<String> {
    let body = field_child(ts, i, "body")?;
    let first = *ts.nodes()[body].children.first()?;
    let first = first.0 as usize;
    if ts.nodes()[first].kind != "expression_statement" {
        return None;
    }
    let s = *ts.nodes()[first].children.first()?;
    if ts.nodes()[s.0 as usize].kind != "string" {
        return None;
    }
    let text = ts.text(s);
    let stripped = text
        .trim_start_matches(['r', 'b', 'u', 'f', 'R', 'B', 'U', 'F'])
        .trim_start_matches(['"', '\''])
        .trim_end_matches(['"', '\'']);
    let doc = stripped
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_matches('\n')
        .to_string();
    (!doc.is_empty()).then_some(doc)
}

pub(crate) fn lower_node(ts: &TreeSitterAdapter, i: usize) -> Option<Lowered> {
    match ts.nodes()[i].kind {
        "function_definition" => Some(Lowered {
            construct: "function",
            name: field_text(ts, i, "name"),
            traits: FUNCTION,
            signature: signature(ts, i),
            doc: docstring(ts, i),
            callee: None,
            n_params: field_child(ts, i, "parameters").map(|p| count_params(ts, p)),
        }),
        "class_definition" => Some(Lowered {
            construct: "type",
            name: field_text(ts, i, "name"),
            traits: TYPE,
            signature: signature(ts, i),
            doc: docstring(ts, i),
            callee: None,
            n_params: None,
        }),
        "lambda" => {
            let adopted =
                adopted_name(ts, i, "assignment", "left", "right", &["identifier"]);
            Some(Lowered {
                construct: if adopted.is_some() { "function" } else { "lambda" },
                name: adopted,
                n_params: field_child(ts, i, "parameters").map(|p| count_params(ts, p)),
                ..Lowered::anon("lambda", FUNCTION)
            })
        }
        // Python's elif is an if-arm with its own condition.
        "if_statement" | "elif_clause" => Some(Lowered::anon("if", CONDITIONAL)),
        "else_clause" => Some(Lowered::anon("else", NONE)),
        "match_statement" => Some(Lowered::anon("switch", CONDITIONAL)),
        "for_statement" => Some(Lowered::anon("for", LOOP)),
        "while_statement" => Some(Lowered::anon("while", LOOP)),
        "call" => Some(Lowered {
            callee: field_text(ts, i, "function"),
            ..Lowered::anon("call", CALL)
        }),
        "import_statement" | "import_from_statement" => Some(Lowered::anon("import", IMPORT)),
        _ => None,
    }
}
