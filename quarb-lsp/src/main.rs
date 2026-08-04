//! quarb-lsp — the Quarb language server.
//!
//! One core, two codecs:
//!
//! ```text
//! quarb-lsp                      # LSP JSON-RPC on stdio (editors)
//! quarb-lsp --kaivrpc <socket>   # kaivrpc on a Unix socket
//! ```
//!
//! Diagnostics come from the engine's own parser (what refuses
//! here refuses in qua); completions from quarb::complete — the
//! stdlib registry both the highlighter and quai read.

mod core;
mod jsonrpc;
mod kaiv_srv;

use anyhow::Result;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--kaivrpc") => {
            let socket = args
                .get(1)
                .map(String::as_str)
                .unwrap_or("/tmp/quarb-lsp.sock");
            kaiv_srv::serve(socket)
        }
        Some("--version" | "-V") => {
            println!("quarb-lsp {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(other) => {
            eprintln!("quarb-lsp: unknown flag {other:?} (try --kaivrpc <socket>)");
            std::process::exit(2);
        }
        None => jsonrpc::serve(),
    }
}

#[cfg(test)]
mod tests {
    use crate::kaiv_srv::answer;

    /// A kaivrpc request is a kaiv document — build one by hand
    /// and run the whole decode→dispatch→encode path, no socket.
    #[test]
    fn kaivrpc_complete_round_trip() {
        // Untyped scalars decode by target type — `line=0` lands
        // in the u32 field.
        let req = "\
.!kaiv
method=quarb-lsp/complete
/params::text=/cf/entry @| co
/params::line=0
/params::col=15
";
        let out = answer(req.as_bytes());
        assert!(out.contains("::status=ok"), "{out}");
        assert!(out.contains("count"), "{out}");
        assert!(out.ends_with("!int'::n=2\n") || out.contains("::n="), "{out}");
    }

    #[test]
    fn kaivrpc_diagnostics_round_trip() {
        let req = "\
.!kaiv
method=quarb-lsp/diagnostics
/params::text=/tags/* | rec(:::)
";
        let out = answer(req.as_bytes());
        assert!(out.contains("::status=ok"), "{out}");
        assert!(out.contains("needs a key"), "{out}");
    }

    #[test]
    fn unknown_method_is_a_clean_refusal() {
        let req = ".!kaiv\nmethod=quarb-lsp/nope\n";
        let out = answer(req.as_bytes());
        assert!(out.contains("no-such-method"), "{out}");
    }
}
