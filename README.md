# The Quarb engine

**Quarb** is a query language for *arbors* — tree-spanned graphs:
graphs with a primary hierarchical backbone enriched by
non-hierarchical ("crosslink") relations. File systems with
symlinks, HTML with anchor references, JSON/XML with `$ref`-style
links, and abstract syntax trees with name-resolution edges are
all arbors. Quarb generalizes and unifies XPath and jq, adding
first-class graph navigation, regex-style quantifiers over *path
elements* (not just characters), pipelines, register-based
breadcrumbs, and subcontexts.

This repository is the Rust implementation: the engine crate, the
CLIs, and the adapter ecosystem that maps concrete data sources
onto the arbor model.

## Crates

**Core**

- [`quarb`](quarb/) — the engine: lexer, parser, executor,
  standard library, and the `AstAdapter` trait every adapter
  implements.

**CLIs**

- [`qua`](qua/) — structure-aware query CLI: one tool, every
  adapter. `cargo install qua`.
- [`qm`](qm/) — a mailbox interface for the terminal, on the
  Quarb engine.

**Format adapters** — [`quarb-json`](quarb-json/),
[`quarb-yaml`](quarb-yaml/), [`quarb-toml`](quarb-toml/),
[`quarb-csv`](quarb-csv/), [`quarb-xml`](quarb-xml/),
[`quarb-html`](quarb-html/), [`quarb-markdown`](quarb-markdown/),
[`quarb-xlsx`](quarb-xlsx/) (xlsx/docx),
[`quarb-archive`](quarb-archive/) (zip/tar).

**Relational adapters** —
[`quarb-relational`](quarb-relational/) (the shared model),
[`quarb-sqlite`](quarb-sqlite/), [`quarb-duckdb`](quarb-duckdb/),
[`quarb-postgres`](quarb-postgres/), [`quarb-mysql`](quarb-mysql/),
[`quarb-bigquery`](quarb-bigquery/).

**Graph** — [`quarb-neo4j`](quarb-neo4j/).

**Source adapters** — [`quarb-fs`](quarb-fs/) (file systems, with
symlink crosslinks), [`quarb-git`](quarb-git/) (commit graphs),
[`quarb-code`](quarb-code/) (source-code ASTs via tree-sitter),
[`quarb-kaiv`](quarb-kaiv/) (kaiv typed data),
[`quarb-imap`](quarb-imap/) and [`quarb-maildir`](quarb-maildir/)
(mailboxes), [`quarb-gsheet`](quarb-gsheet/) (Google Sheets),
[`quarb-firebase`](quarb-firebase/),
[`quarb-firestore`](quarb-firestore/),
[`quarb-datastore`](quarb-datastore/) (cloud document stores),
[`quarb-objstore`](quarb-objstore/) (GCS/S3 object stores).

**Composition and protocol** — [`quarb-mount`](quarb-mount/)
(compose adapters under one root),
[`quarb-compose`](quarb-compose/) (parseable leaf content grafts
an inner arbor), [`quarb-serve`](quarb-serve/) (expose any
adapter to `qua` over a child process).

**Importers** — translate foreign queries to Quarb:
[`quarb-xpath`](quarb-xpath/) (XPath 1.0),
[`quarb-jq`](quarb-jq/) (jq filters), [`quarb-sql`](quarb-sql/)
(SQL).

## Quick start

```sh
cargo install qua

# JSON, like jq:
qua '/books/*[/price:: > 20]/title::' store.json

# A filesystem, like find — but with graph navigation:
qua '//*[::;size > 1e6]' ~/logs

# A SQLite database, no SQL:
qua '/albums/*::title' music.db
```

Or embed the engine:

```rust
use quarb::{run, QueryResult};

let result = run("//user[::age > 30]::name", &adapter)?;
```

## Documentation

- The [Quarb Language Specification and User
  Guide](https://quarb.org) — the authoritative reference and a
  tutorial introduction.
- Per-tool [cookbooks](https://quarb.org) mapping XPath, jq, SQL,
  Cypher, pandas, and CSS-selector idioms to Quarb.

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option.
