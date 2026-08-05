//! The conformance fixtures: every lowering-table row is pinned
//! by an assertion over `fixtures/vocab.{rs,py,js,c}`, plus one
//! uniformity suite run over all four — the depth-over-breadth
//! gate every future language must clear.

use quarb_code::CodeModel;

const RS: &str = include_str!("fixtures/vocab.rs");
const PY: &str = include_str!("fixtures/vocab.py");
const JS: &str = include_str!("fixtures/vocab.js");
const C: &str = include_str!("fixtures/vocab.c");

fn model(ext: &str) -> CodeModel {
    let src = match ext {
        "rs" => RS,
        "py" => PY,
        "js" => JS,
        "c" => C,
        _ => unreachable!(),
    };
    CodeModel::parse(src, ext).unwrap()
}

fn v(m: &CodeModel, q: &str) -> Vec<String> {
    match quarb::run(q, m).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| m.locator(n)).collect(),
    }
}

/// Every construct word the code level may mint.
const VOCABULARY: &[&str] = &[
    "function", "lambda", "type", "impl", "module", "field", "constant", "if", "else", "switch",
    "for", "while", "loop", "call", "import",
];

#[test]
fn rust_naming_rule() {
    let m = model("rs");
    // Declared identifiers are edge names; pre-order.
    assert_eq!(
        v(&m, "//*<function>:::name"),
        ["lex", "is_name_char", "helper", "f", "run"]
    );
    // A filepath into the program: module / function / nested fn.
    assert_eq!(v(&m, "/lexer/lex/is_name_char @| count"), ["1"]);
    // The impl is the type's namespace continued: two nodes named
    // Lexer (struct, impl), and /Lexer/helper addresses the method.
    assert_eq!(v(&m, "//Lexer:::name @| count"), ["2"]);
    assert_eq!(v(&m, "/Lexer/helper:::name"), ["helper"]);
    assert_eq!(v(&m, "//helper"), ["/Lexer[2]/helper"]);
    // Named declarations across kinds.
    assert_eq!(v(&m, "//lexer::::construct"), ["module"]);
    assert_eq!(v(&m, "//Token::::construct"), ["type"]);
    assert_eq!(v(&m, "//Token/*:::name"), ["Word", "Space"]);
    assert_eq!(v(&m, "//pos::::construct"), ["field"]);
    assert_eq!(v(&m, "//LIMIT::::construct"), ["constant"]);
    assert_eq!(v(&m, "//import @| count"), ["1"]);
}

#[test]
fn rust_properties_and_metadata() {
    let m = model("rs");
    assert_eq!(v(&m, "//lex::doc"), ["Scan the input."]);
    assert_eq!(v(&m, "//Lexer<type>::doc"), ["A cursor over the input."]);
    assert_eq!(v(&m, "//lex::signature"), ["pub fn lex(input: &str) -> Vec<char>"]);
    // &mut self is a declared parameter, as written.
    assert_eq!(v(&m, "//helper::::n-params"), ["2"]);
    // The escape hatch, and its two-colon alias (ruling #29).
    assert_eq!(v(&m, "//lex::::kind"), ["function_item"]);
    assert_eq!(v(&m, "//lex::kind"), ["function_item"]);
    assert_eq!(v(&m, "//lex::::lang"), ["rust"]);
    // The ruled name is switch; the backend spelling survives in kind.
    assert_eq!(v(&m, "//switch::::kind"), ["match_expression"]);
}

#[test]
fn rust_adoption_and_definitions() {
    let m = model("rs");
    // let f = |x| … adopts the binding's name; no lambda remains.
    assert_eq!(v(&m, "//f::::construct"), ["function"]);
    assert_eq!(v(&m, "//f::::kind"), ["closure_expression"]);
    assert_eq!(v(&m, "//lambda @| count"), ["0"]);
    // Calls resolve to same-file declarations by identifier.
    assert_eq!(v(&m, "//*<call>->definition:::name"), ["is_name_char", "f", "helper"]);
    assert_eq!(v(&m, "//f<-definition @| count"), ["1"]);
    assert_eq!(v(&m, "//helper<-definition @| count"), ["1"]);
}

#[test]
fn rust_dissolve() {
    let m = model("rs");
    // No backend vocabulary in the namespace: wrappers are gone,
    // identifiers never became nodes.
    assert_eq!(v(&m, "//identifier @| count"), ["0"]);
    assert_eq!(v(&m, "//block @| count"), ["0"]);
    assert_eq!(v(&m, "//function_item @| count"), ["0"]);
    // Blocks dissolved + else kept: the ladder routes through else.
    assert_eq!(v(&m, "//if/else/if @| count"), ["1"]);
}

#[test]
fn python_table() {
    let m = model("py");
    assert_eq!(v(&m, "//*<function>:::name"), ["__init__", "lex", "helper", "f", "run"]);
    assert_eq!(v(&m, "/Lexer/lex:::name"), ["lex"]);
    assert_eq!(v(&m, "//lex::doc"), ["Scan the input."]);
    assert_eq!(v(&m, "//Lexer::doc"), ["A lexer."]);
    assert_eq!(v(&m, "//lex::signature"), ["def lex(self)"]);
    assert_eq!(v(&m, "//helper::::n-params"), ["2"]);
    // elif is an if-arm with its own condition.
    assert_eq!(v(&m, "//if @| count"), ["2"]);
    assert_eq!(v(&m, "//else @| count"), ["1"]);
    assert_eq!(v(&m, "//switch::::kind"), ["match_statement"]);
    // f = lambda x: … adopts; the call to it resolves.
    assert_eq!(v(&m, "//f::::construct"), ["function"]);
    assert_eq!(v(&m, "//lambda @| count"), ["0"]);
    assert_eq!(v(&m, "//f<-definition @| count"), ["1"]);
    assert_eq!(v(&m, "//import @| count"), ["2"]);
    assert_eq!(v(&m, "//lex::::lang"), ["python"]);
}

#[test]
fn javascript_table() {
    let m = model("js");
    assert_eq!(
        v(&m, "//*<function>:::name"),
        ["constructor", "lex", "length", "helper", "main"]
    );
    assert_eq!(v(&m, "/Lexer/lex:::name"), ["lex"]);
    assert_eq!(v(&m, "//Lexer<type>::doc"), ["A lexer."]);
    assert_eq!(v(&m, "//lex::doc"), ["Scan the input."]);
    // A class field, named by its property.
    assert_eq!(v(&m, "//size::::construct"), ["field"]);
    // const helper = (a, b) => {} adopts; no lambda remains.
    assert_eq!(v(&m, "//helper::::construct"), ["function"]);
    assert_eq!(v(&m, "//helper::::kind"), ["arrow_function"]);
    assert_eq!(v(&m, "//helper::::n-params"), ["2"]);
    assert_eq!(v(&m, "//lambda @| count"), ["0"]);
    // do-while is a while; the backend spelling survives in kind.
    assert_eq!(v(&m, "//while @| count"), ["2"]);
    assert_eq!(v(&m, "//while[2]::::kind"), ["do_statement"]);
    assert_eq!(v(&m, "//switch @| count"), ["1"]);
    // new Lexer(…) is a call whose callee resolves to the type.
    assert_eq!(
        v(&m, "//*<call>->definition:::name @| unique @| sort"),
        ["Lexer", "helper"]
    );
    assert_eq!(v(&m, "//Lexer<-definition @| count"), ["2"]);
    assert_eq!(v(&m, "//import @| count"), ["1"]);
    assert_eq!(v(&m, "//lex::::lang"), ["javascript"]);
}

#[test]
fn c_table() {
    let m = model("c");
    // The declarator chain: a pointer-returning function is named
    // by its leaf identifier, never the whole declarator.
    assert_eq!(v(&m, "//*<function>:::name"), ["helper", "main"]);
    assert_eq!(v(&m, "//helper::signature"), ["static int *helper(int a, int b)"]);
    assert_eq!(v(&m, "//helper::doc"), ["Advance to the limit."]);
    // The prototype dissolves: one helper, not decl + def.
    assert_eq!(v(&m, "//helper<function> @| count"), ["1"]);
    assert_eq!(v(&m, "//helper::::n-params"), ["2"]);
    // (void) declares no parameters.
    assert_eq!(v(&m, "//main::::n-params"), ["0"]);
    // Named specifiers with bodies declare types; enumerators are
    // fields; a function-pointer typedef resolves through the chain.
    assert_eq!(v(&m, "//*<type>:::name"), ["lexer", "token", "step_fn"]);
    assert_eq!(v(&m, "//token/*:::name"), ["WORD", "SPACE"]);
    assert_eq!(v(&m, "//pos::::construct"), ["field"]);
    // Both #define forms are constants.
    assert_eq!(v(&m, "//LIMIT::::construct"), ["constant"]);
    assert_eq!(v(&m, "//STEP::::construct"), ["constant"]);
    assert_eq!(v(&m, "//import @| count"), ["1"]);
    // The else-if ladder routes through else.
    assert_eq!(v(&m, "//if/else/if @| count"), ["1"]);
    assert_eq!(v(&m, "//switch::::kind"), ["switch_statement"]);
    assert_eq!(v(&m, "//helper<-definition @| count"), ["1"]);
    assert_eq!(v(&m, "//lex @| count"), ["0"]);
    assert_eq!(v(&m, "//main::::lang"), ["c"]);
}

#[test]
fn uniformity_across_languages() {
    for ext in ["rs", "py", "js", "c"] {
        let m = model(ext);
        // One query shape everywhere.
        let functions = v(&m, "//*<function>:::name");
        assert!(functions.iter().any(|n| n == "helper"), "{ext}: {functions:?}");
        assert_eq!(v(&m, "//helper<-definition @| count"), ["1"], "{ext}");
        assert_ne!(v(&m, "//*<loop> @| count"), ["0"], "{ext}");
        assert_ne!(v(&m, "//*<conditional> @| count"), ["0"], "{ext}");
        assert_ne!(v(&m, "//*<call> @| count"), ["0"], "{ext}");
        assert_ne!(v(&m, "//*<import> @| count"), ["0"], "{ext}");
        // The dissolve proof: every minted construct is vocabulary.
        for construct in v(&m, "//*::::construct @| unique") {
            assert!(VOCABULARY.contains(&construct.as_str()), "{ext}: {construct}");
        }
        // No backend vocabulary in the namespace.
        assert_eq!(v(&m, "//identifier @| count"), ["0"], "{ext}");
        assert_eq!(v(&m, "//expression_statement @| count"), ["0"], "{ext}");
    }
}

#[test]
fn unsupported_extension_and_parity() {
    assert!(CodeModel::parse("x", "zig").is_err());
    for ext in ["rs", "py", "js", "mjs", "cjs", "jsx", "c", "h", "zig", "", "txt"] {
        assert_eq!(
            quarb_code::supported(ext),
            quarb_tree_sitter::supported(ext),
            "{ext}"
        );
    }
}

#[test]
fn cache_reuse() {
    // The code level rides the syntax level's AST cache: parse
    // twice with a cache mounted; the second read hits.
    let dir = std::env::temp_dir().join(format!("quarb-code-cache-{}", std::process::id()));
    quarb_tree_sitter::set_cache(Some(quarb_tree_sitter::Cache::new(dir.clone())));
    let a = CodeModel::parse(RS, "rs").unwrap();
    let b = CodeModel::parse(RS, "rs").unwrap();
    quarb_tree_sitter::set_cache(None);
    assert_eq!(v(&a, "//*<function>:::name"), v(&b, "//*<function>:::name"));
    assert!(std::fs::read_dir(&dir).map(|d| d.count() > 0).unwrap_or(false));
    let _ = std::fs::remove_dir_all(dir);
}
