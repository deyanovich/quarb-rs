# quarb

**Quarb** is a query language for *arbors* — tree-spanned graphs:
graphs with a primary hierarchical backbone enriched by
non-hierarchical ("crosslink") relations. File systems with
symlinks, HTML with anchor references, JSON/XML with `$ref`-style
links, and abstract syntax trees with name-resolution edges are
all arbors.

This crate is the **engine**: it lexes and parses a query, then
evaluates it against an adapter that maps a data source onto the
arbor model. Quarb generalizes and unifies XPath and jq, adding
first-class graph navigation, regex-style quantifiers over *path
elements* (not just characters), pipelines, register-based
breadcrumbs, and subcontexts.

## Example

Implement the `AstAdapter` trait for your own data, or use a
ready-made adapter such as
[`quarb-json`](https://crates.io/crates/quarb-json),
[`quarb-fs`](https://crates.io/crates/quarb-fs), or one of the
many [others](https://quarb.org):

```rust
use quarb::{run, QueryResult};

// `adapter` maps some data source onto the arbor model.
let result = run("//user[::age > 30]::name", &adapter).unwrap();
match result {
    QueryResult::Nodes(nodes) => { /* matched nodes */ }
    QueryResult::Values(values) => { /* projected scalars */ }
}
```

## The adapter ecosystem

One query language runs over many sources through separate
adapter crates: JSON, YAML, TOML, CSV, XML, HTML, Markdown,
spreadsheets, archives, filesystems, git, five SQL engines,
Neo4j, cloud document stores, mailboxes, source-code ASTs, and
more. The `qua` CLI ties them together. See
[quarb.org](https://quarb.org) for the full list.

## Documentation

- The [Quarb Language Specification and User
  Guide](https://quarb.org) — the authoritative reference and a
  tutorial introduction.
- Per-tool [cookbooks](https://quarb.org) mapping XPath, jq, SQL,
  Cypher, pandas, and CSS-selector idioms to Quarb.

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option.
