# @quarb/wasm

[Quarb](https://quarb.org) — the arboreal query engine —
compiled to WebAssembly, with TypeScript types. One path
language over JSON, YAML, TOML, CSV/TSV, XML, HTML, and
Markdown — including the text-level reading of HTML, Markdown,
and plain text (sections, paragraphs, quotes, lists) — and the
web level, a whole website as one queryable tree, running
entirely client-side: no server, no native dependency, the same
engine that powers the [playground](https://demo.quarb.org), the
[search box on quarb.org](https://quarb.org/search.html), and
the Quarb Scraper browser extension.

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

## A website as one tree

Pack a static site's pages into a tarball and the engine reads
it at the web level: every page under
`/sites/<host>/pages/<path>`, the links between pages resolved
into `->link` / `<-link`, tags and categories the pages declare
as rows of their own, and each page readable at the text level
(sections, paragraphs). That is a site search with no server:

```sh
tar czf site.tgz -C public .      # every .html under public/
```

```ts
import { openSite } from '@quarb/wasm';

const bytes = await fetch('/site.tgz').then((r) => r.arrayBuffer());
const site = await openSite(bytes, { base: 'https://example.org/' });

// Full-text search: the paragraphs that mention both words,
// each as a link record — title, href to the block, the text.
site.query('//paragraph[:: == (/arbor.*?query/i)] | link | json');

// The site's own structure.
site.query('//page::title');                         // every page
site.query('//page[::in_degree = 0]::path');         // orphans
site.query('/sites/example.org/pages/about.html | <-link::path');
site.query('//page @| top(5; ::pagerank) | %(::path; ::pagerank)');
```

`base` is the site's root URL; without it the engine reads the
base off a page's `<link rel="canonical">` and falls back to
`https://localhost/`. A site small enough to hold in memory
needs no tarball: pass an object of path to HTML
(`openSite({ 'index.html': html, 'about.html': html2 })`). Load
once and query many times; a Web Worker keeps a slow query off
the page thread (the search box on quarb.org is
[built this way](https://quarb.org/articles/the-site-that-ranks-itself.html)).
Pass `model` for a Quarb model file over the site (aliases,
derived nodes, edges).

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

This package bundles the text-format adapters listed above, the
text level, and the web level over a tarball. The full engine — 40+ adapters from SQLite and Postgres to
Kafka, S3, and cloud log services, plus the `qua` CLI and the
`quai` interactive session — ships as
[Rust crates](https://crates.io/crates/quarb) and a
[Python package](https://pypi.org/project/quarb/).

## License

MIT or Apache-2.0, at your option.
