# qua

Python bindings for the [Quarb][quarb] query engine. Quarb reads
structured text as an *arbor* — a tree-spanned graph — and runs
one query language over all of it, generalizing what XPath does
for XML and jq does for JSON to JSON, YAML, TOML, CSV/TSV, XML,
HTML, and Markdown alike. Try it live in the
[playground][playground].

[quarb]: https://quarb.org/
[playground]: https://demo.quarb.org/

## Install

```bash
pip install quarb
```

This installs the `quarb` Python module and the `qua` command
(the language is Quarb; `qua` is its CLI).

Wheels are published for Linux (x86_64, aarch64), macOS (arm64,
x86_64), and Windows (x86_64). The extension is built against
PyO3's stable ABI (`abi3-py38`); a single wheel covers CPython
3.8 and later.

## The `qua` command

The entry point covers what the bindings cover — one query over
a text document:

```bash
qua '/books/*[/price:: > 20]/title::' store.json
echo '{"users":[{"name":"ada"}]}' | qua -f json '/users/*/name::'
```

Beyond text, `quarb.open(path)` dispatches on kind over the full
local fleet — SQLite, kaiv (typed units), the filesystem
(`descend=True` grafts parseable leaves), git (`git:PATH`),
archives, XLSX, and source code — and `quarb.mount([a, b, ...])`
mounts several sources under one root so a single query joins
across them (a YAML file `<=>` a SQLite database). The networked
adapters — Postgres, MySQL, BigQuery, Neo4j, object stores,
mail — and DuckDB live in the Rust binary: `cargo install qua`.

## Quick start

```python
import quarb

doc = quarb.loads("""{
  "books": [
    {"title": "Sapiens", "price": 25},
    {"title": "Cosmos",  "price": 18}
  ]
}""", "json")

doc.values('/books/*[/price:: > 20]/title::')
# ['Sapiens']

doc.value('/books/*/price:: @| mean')
# 21.5

doc.records('/books/* | rec("t", /title::, "p", /price::)')
# [{'t': 'Sapiens', 'p': 25}, {'t': 'Cosmos', 'p': 18}]
```

`loads(text, format)` / `load(path)` parse once into a
`Document`, queried many times. Results come back **typed**:
ints, floats, strings, `None` for null, tz-aware `datetime` for
instants, `timedelta` for durations, `Quantity` for unit-carrying
values, dicts for records. Formats: `json`, `yaml`, `toml`,
`csv`, `tsv`, `xml`, `html`, `markdown`.

The lower-level `quarb.run(query, text, format)` and
`quarb.run_file(query, path)` return the result *lines as
strings* — exactly what the `qua` CLI prints. All errors raise
`ValueError` with the engine's message.

For the query language itself — steps, criteria, readings,
patterns, joins — see the [user guide and spec][quarb].

## License

Licensed under either of Apache License, Version 2.0 or the MIT
license at your option. Both texts are bundled in the package.

## Jupyter

```
%load_ext quarb.ipython
%quarb_mount fleet.daiv
%quarb /@hosts/*[::power > 0.2kW]::name
```

`%%quarb [NAME]` runs a cell-sized query against a named mount;
results render as HTML tables, iterate as typed values
(quantities keep magnitude + unit), and convert with `.df` /
`.df_magnitudes()`.

For a notebook where *every* cell is a query, install the
dedicated kernel and pick **Quarb** in the launcher:

```
python -m quarb.kernel install
python -m quarb.kernel demo      # writes quarb-demo.ipynb to try
```

Cells are queries; `%mount`, `%connect`, `%use`, `%docs`,
`%translate`, and `%python` (an escape hatch — the rest of the
cell is Python, with the live `session` and the last result `_`
in scope) are the directives. Tab-completion offers the mounted
arbor's real child names. `%connect NAME PATH...` (magic:
`%quarb_connect`) attaches to a `qua --resident` daemon — for
arbors too large to rebuild per notebook, queried from a
standing session. `pip install quarb[jupyter]` pulls IPython,
pandas, and the kernel runtime.
