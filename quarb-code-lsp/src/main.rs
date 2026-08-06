//! quarb-code-lsp — the code level as a language server.
//!
//! One core, two codecs:
//!
//! ```text
//! quarb-code-lsp                      # LSP JSON-RPC on stdio (editors)
//! quarb-code-lsp --kaivrpc <socket>   # kaivrpc on a Unix socket
//! ```
//!
//! A READING tool, by design: outline (documentSymbol, the
//! lowering tables' SymbolKind column live), definition,
//! references, hover, workspace symbols — every answer a
//! code-level query, no resident index. No rename, no
//! completion, no diagnostics: the write side stays the classic
//! language server's job.
//!
//! The AST cache is enabled by default (the syntax level's
//! content-addressed store), so workspace answers warm to
//! subsecond exactly as `qua --cache` does.

mod core;
mod jsonrpc;
mod kaiv_srv;

use anyhow::Result;

fn main() -> Result<()> {
    quarb_tree_sitter::set_cache(Some(quarb_tree_sitter::Cache::new(
        quarb_tree_sitter::Cache::default_dir(),
    )));
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--kaivrpc") => {
            let socket = args
                .get(1)
                .map(String::as_str)
                .unwrap_or("/tmp/quarb-code-lsp.sock");
            kaiv_srv::serve(socket)
        }
        Some("--version" | "-V") => {
            println!("quarb-code-lsp {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(other) => {
            eprintln!("quarb-code-lsp: unknown flag {other:?} (try --kaivrpc <socket>)");
            std::process::exit(2);
        }
        None => jsonrpc::serve(),
    }
}

#[cfg(test)]
mod tests {
    use crate::core::{Workspace, symbol_kind, word_at};
    use crate::kaiv_srv::answer;

    const RS: &str = "\
mod lexer {
    /// Scan the input.
    pub fn lex(input: &str) -> usize {
        fn is_name_char(c: char) -> bool {
            c.is_alphanumeric()
        }
        input.chars().filter(|c| is_name_char(*c)).count()
    }
}

struct Lexer {
    pos: usize,
}

impl Lexer {
    fn helper(&mut self, n: usize) -> usize {
        self.pos += n;
        lexer::lex(\"x\")
    }
}
";

    fn ws() -> Workspace {
        let mut w = Workspace::new(None);
        w.open("file:///t/lexer.rs", RS);
        w
    }

    #[test]
    fn outline_nests_and_kinds_follow_the_table() {
        let syms = ws().symbols("file:///t/lexer.rs");
        let names: Vec<(&str, u32, u32)> = syms
            .iter()
            .map(|s| (s.name.as_str(), s.depth, s.kind))
            .collect();
        assert_eq!(
            names,
            [
                ("lexer", 0, 2),         // Module
                ("lex", 1, 12),          // Function
                ("is_name_char", 2, 12), // Function (nested)
                ("Lexer", 0, 23),        // Struct
                ("pos", 1, 8),           // Field
                ("Lexer", 0, 19),        // Object (the impl)
                ("helper", 1, 6),        // Method — inside a type
            ]
        );
        // The outline carries the hover: signatures ride along.
        assert_eq!(
            syms[1].signature.as_deref(),
            Some("pub fn lex(input: &str) -> usize")
        );
    }

    #[test]
    fn definition_and_hover_answer_from_the_overlay() {
        let w = ws();
        let defs = w.definition("file:///t/lexer.rs", "lex");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].locator, "/lexer/lex");
        assert_eq!(defs[0].line, 3);
        let h = w.hover("file:///t/lexer.rs", "is_name_char");
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].construct, "function");
        assert_eq!(
            h[0].signature.as_deref(),
            Some("fn is_name_char(c: char) -> bool")
        );
        // Homonym fan-out is the honest answer: struct + impl.
        assert_eq!(w.definition("file:///t/lexer.rs", "Lexer").len(), 2);
    }

    #[test]
    fn word_extraction_finds_the_identifier_under_the_cursor() {
        assert_eq!(word_at(RS, 2, 12).as_deref(), Some("lex"));
        assert_eq!(word_at("a.b(c)", 0, 2).as_deref(), Some("b"));
        assert_eq!(word_at("x", 0, 0).as_deref(), Some("x"));
        assert_eq!(word_at("  ", 0, 1), None);
    }

    #[test]
    fn kind_table_matches_the_spec_column() {
        assert_eq!(symbol_kind("function", "function_item", false), 12);
        assert_eq!(symbol_kind("function", "method_definition", true), 6);
        assert_eq!(symbol_kind("type", "struct_specifier", false), 23);
        assert_eq!(symbol_kind("type", "enum_item", false), 10);
        assert_eq!(symbol_kind("type", "trait_item", false), 11);
        assert_eq!(symbol_kind("field", "enumerator", false), 22);
        assert_eq!(symbol_kind("impl", "impl_item", false), 19);
    }

    /// A kaivrpc request is a kaiv document — the whole
    /// decode→dispatch→encode path, no socket.
    #[test]
    fn kaivrpc_symbols_round_trip() {
        let req = format!(
            ".!kaiv\nmethod=quarb-code-lsp/symbols\n/params::lang=rs\n/params::text={}\n",
            "fn lex() {}"
        );
        let out = answer(req.as_bytes());
        assert!(out.contains("::status=ok"), "{out}");
        assert!(out.contains("lex"), "{out}");
    }

    /// The query door: values, locations, and the engine's own
    /// refusal, each through the full wire.
    #[test]
    fn kaivrpc_query_round_trips() {
        let fx = concat!(env!("CARGO_MANIFEST_DIR"), "/../quarb-code/tests/fixtures/vocab.rs");
        let ask = |q: &str| {
            answer(
                format!(
                    ".!kaiv\nmethod=quarb-code-lsp/query\n/params::file={fx}\n/params::scope=file\n/params::query={q}\n"
                )
                .as_bytes(),
            )
        };
        let out = ask("//*<function> @| count");
        assert!(out.contains("::status=ok"), "{out}");
        assert!(out.contains("::value=5"), "{out}");
        let out = ask("//helper");
        assert!(out.contains("/Lexer[2]/helper"), "{out}");
        let out = ask("//helper[");
        assert!(out.contains("refused"), "{out}");
    }

    #[test]
    fn kaivrpc_unknown_method_is_a_clean_refusal() {
        let req = ".!kaiv\nmethod=quarb-code-lsp/nope\n";
        let out = answer(req.as_bytes());
        assert!(out.contains("no-such-method"), "{out}");
    }
}
