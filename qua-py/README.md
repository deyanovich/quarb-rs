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
pip install qua
```

Wheels are published for Linux (x86_64, aarch64), macOS (arm64,
x86_64), and Windows (x86_64). The extension is built against
PyO3's stable ABI (`abi3-py38`); a single wheel covers CPython
3.8 and later.

## Quick start

```python
import qua

doc = """{
  "books": [
    {"title": "Sapiens", "price": 25},
    {"title": "Cosmos",  "price": 18}
  ]
}"""

qua.run('/books/*[/price:: > 20]/title::', doc, 'json')
# ['Sapiens']

qua.run_file('/books/*/title::', 'books.json')
# ['Sapiens', 'Cosmos']
```

`run` takes the input as a string plus an explicit format name
(`json`, `yaml`, `toml`, `csv`, `tsv`, `xml`, `html`,
`markdown`); `run_file` reads a file and infers the format from
its extension. Both return the result lines as a list of strings
and raise `ValueError` with the engine's message on parse or
execution errors.

For the query language itself — steps, criteria, readings,
patterns, joins — see the [user guide and spec][quarb].

## License

Licensed under either of Apache License, Version 2.0 or the MIT
license at your option. Both texts are bundled in the package.
