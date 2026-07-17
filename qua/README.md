# qua

Structure-aware query CLI for the Quarb engine.

`qua` runs [Quarb](https://quarb.org) queries from the command
line over files and services — JSON, YAML, TOML, CSV, XML, HTML,
spreadsheets, archives, filesystems, git, SQL databases, graph
and document stores, mailboxes, and more — one query language for
all of them. Quarb is a query language for *arbors*: tree-spanned
graphs, a hierarchical backbone enriched with crosslink relations.

## Install

```sh
cargo install qua
```

## Usage

```sh
qua '//user[::age > 30]::name' data.json
qua '/row[::status = "open"]::id' tickets.csv
```

See [quarb.org](https://quarb.org) for the language specification,
the user guide, and per-tool cookbooks (XPath, jq, SQL, Cypher,
pandas, CSS).

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option.
