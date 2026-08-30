# @quarb/wasm

[Quarb](https://quarb.org) — the arboreal query engine —
compiled to WebAssembly, with TypeScript types. One path
language over JSON, YAML, TOML, CSV/TSV, XML, HTML, and
Markdown — including the text-level reading of HTML, Markdown,
and plain text (sections, paragraphs, quotes, lists) — running
entirely client-side: no server, no native
dependency, the same engine that powers the
[playground](https://demo.quarb.org) and the Quarb Scraper
browser extension.

## Install

```sh
npm install @quarb/wasm
```

## Use

```ts
import { query } from '@quarb/wasm';

const rows = await query(
  'json',
  '{"users": [{"name": "ada", "age": 36}, {"name": "lin", "age": 7}]}',
  '/users/*[::age >= 18]::name'
);
// ["ada"]
```

`query()` initializes the engine on first call — browsers and
bundlers fetch the `.wasm` next to the module, Node reads it
from disk — and returns the result lines, throwing on a parse
or execution error. Scrape HTML the same way:

```ts
const links = await query('html', html, '//a::href');
```

For explicit control (custom wasm location, one-time init, the
raw result envelope):

```ts
import { initQuarb, run, version } from '@quarb/wasm';

await initQuarb();                     // or initQuarb(bytes)
version();                             // "0.12.0"
const envelope = JSON.parse(run('csv', csv, '/row @| count', Date.now()));
// {ok: true, lines: ["891"]} — or {ok: false, error: "..."}
```

## Sessions: `@quarb/wasm/quai`

The second entry point is the **session engine** — the build
behind the [quai playground](https://demo.quarb.org/quai/) and
the Quarb Chrome extension. Mount several named sources (json,
yaml, toml, csv, xml, html, markdown, **kaiv**, SQLite bytes)
as children of one root and join across them; every line
becomes `&N` and is reusable:

```ts
import { mount } from '@quarb/wasm/quai';

const session = await mount([
  { name: 'page',   format: 'html', text: html },
  { name: 'orders', format: 'json', text: ordersJson },
]);
const r = JSON.parse(session.run(
  '/page//a <=> /orders/rows/*[::url = _::href] | %(url = ::href; total = $$1::total)'
));
// r = {label: "&1", lines: [...], note, error}
```

`session.run(line)` is the REPL dispatch (queries, `def`s,
`&N`/`&N#` recalls, `= expr` scalars); `session.run_cell(text)`
runs a notebook cell as a unit; `state()`/`restore()` carry
the macro table across remounts.

## The language in one breath

Paths navigate (`/users/*`, `//a`), predicates filter
(`[::age >= 18]`), `::key` projects values, `|` pipes each
result through transforms, `@|` aggregates across all of them,
`<=>` joins across sources, and `= expr` opens a scalar
expression with no document at all. The
[user guide](https://quarb.org/guide.html) walks the whole
language on real transcripts; the
[cookbooks](https://quarb.org/cookbooks/) translate from jq,
XPath, pandas, CSS selectors, and BeautifulSoup idioms; the
[specification](https://quarb.org/spec/latest) is the
authoritative reference.

## Scope

This package bundles the text-format adapters listed above.
The full engine — 40+ adapters from SQLite and Postgres to
Kafka, S3, and cloud log services, plus the `qua` CLI and the
`quai` interactive session — ships as
[Rust crates](https://crates.io/crates/quarb) and a
[Python package](https://pypi.org/project/quarb/).

## License

MIT or Apache-2.0, at your option.
