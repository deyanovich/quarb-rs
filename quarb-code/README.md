# quarb-code

The code level for the Quarb query engine — cross-language code
navigation where **function names are node names, not
properties**: `/lexer/lex/is_name_char` descends module,
function, nested function — a filepath into the program — where
the syntax level spells the same question
`//function_item[::name = "lex"]`.

A declaration's edge name is its declared identifier; the
remaining constructs keep a small normalized vocabulary (`if`,
`switch`, `for`, `call`, …); everything else dissolves. Curated
traits (`<function>`, `<type>`, `<loop>`, …) classify across
languages, `::signature` / `::doc` / `::callee` are uniform
properties, `->definition` crosslinks resolve calls to their
same-file declarations, and the raw grammar kind survives only
as `::::kind`. Built on the
[`quarb-tree-sitter`](https://crates.io/crates/quarb-tree-sitter)
parse (the syntax level, which remains the default reading of
source files; the `code:` target prefix opts up). Grammars:
Rust, Python, JavaScript, C.

**History note:** through 0.21.0 this crate name hosted the
tree-sitter syntax-level adapter, now published as
`quarb-tree-sitter`. From 0.22.0, `quarb-code` is the code
level.

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
