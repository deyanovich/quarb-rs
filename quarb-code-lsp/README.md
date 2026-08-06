# quarb-code-lsp

The [code level](https://quarb.org/articles/the-ide-that-isnt-running.html)
as a language server — a *reading* tool: outline, go-to-definition,
find-references, hover, and workspace symbols over Rust, Python,
JavaScript, and C source, with no resident index. Every answer is
a code-level query; the lowering tables' LSP SymbolKind column is
the outline's kind mapping, definition and references resolve by
declared identifier (fan-out on homonyms — the honest answer), and
hover is `::signature` + `::doc`.

One core, two codecs:

```text
quarb-code-lsp                      # LSP JSON-RPC on stdio (editors)
quarb-code-lsp --kaivrpc <socket>   # kaivrpc on a Unix socket
```

The kaivrpc door (`quarb-code-lsp/symbols`, `/definition`,
`/references`, `/hover`, `/query` — word-based, one request per
connection) is what kaiv-native editor extensions speak; the
JSON-RPC door is standard LSP for everything else — plus the
same query door as the vendor method `quarb/query` (params
`query`, optional `scope: "file"`, optional `textDocument`;
result rows of `{value}` or `{file, line, locator, location}`;
a refusal is a `RequestFailed` error carrying the engine's
message verbatim). Both codecs, one story: any client picks by
transport taste, not by capability. The syntax
level's AST cache is on by default, so workspace answers warm to
roughly what `qua --cache` pays.

Deliberately not provided: rename, completion,
diagnostics-as-you-type — the write-side gestures where resident
state is the entire product. Pair with `quarb-lsp` (the Quarb
language's own server) rather than replacing your language's
classic LSP.

Part of [Quarb](https://quarb.org), a query language for *arbors*
— tree-spanned graphs (a hierarchical backbone enriched with
non-hierarchical "crosslink" relations).

See [quarb.org](https://quarb.org) for the language specification,
the user guide, and the full list of adapters.

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option.
