# quarb-kaiv

kaiv typed-data adapter for the Quarb query engine.

Part of [Quarb](https://quarb.org), a query language for *arbors*
— tree-spanned graphs. This crate is an adapter: it maps kaiv
documents onto the arbor model so the shared
[`quarb`](https://crates.io/crates/quarb) engine can query them,
and the [`qua`](https://quarb.org) CLI can reach them alongside
every other source.

See [quarb.org](https://quarb.org) for the language specification,
the user guide, and the kaiv cookbook.

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option.
