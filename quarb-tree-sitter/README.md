# quarb-tree-sitter

Tree-sitter syntax-level adapter for the Quarb query engine.

A source file parses (tree-sitter) into its syntax tree, and the
tree is the arbor, taken literally: node kinds are the edge names,
tree-sitter's fields are properties, a node's value is its source
text. This is *the syntax level* — the default reading of source
files. The [`quarb-code`](https://crates.io/crates/quarb-code)
crate builds *the code level* on top of this parse, where declared
identifiers become node names.

Published as `quarb-code` through 0.21.0; from 0.22.0 that name
hosts the code level built on this crate.

Not to be confused with `tree-sitter-quarb`:
`quarb-tree-sitter` mounts *other* languages' source through
their tree-sitter grammars; `tree-sitter-quarb` is the
tree-sitter grammar for Quarb's own query files.

Part of [Quarb](https://quarb.org), a query language for *arbors*
— tree-spanned graphs (a hierarchical backbone enriched with
non-hierarchical "crosslink" relations). This crate is an adapter:
it maps its data source onto the arbor model so the shared
[`quarb`](https://crates.io/crates/quarb) engine can query it, and
the [`qua`](https://quarb.org) CLI can reach it alongside every
other source.

See [quarb.org](https://quarb.org) for the language specification,
the user guide, and the full list of adapters.

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option.
