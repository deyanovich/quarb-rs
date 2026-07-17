# quarb-toml

TOML adapter for the Quarb query engine.

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
