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

The full `qua` CLI — file systems, git, databases, mail,
adapter composition — is the Rust binary: `cargo install qua`.

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
