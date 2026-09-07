// @quarb/wasm — the Quarb engine compiled to WebAssembly.
//
// This wrapper adds environment-aware initialization (browsers
// fetch the .wasm relative to the module URL; Node reads it from
// disk) and a typed `query()` that unwraps the engine's JSON
// envelope. The raw wasm-bindgen surface stays exported for
// callers that want to manage initialization themselves.

import init, { run, version, Site as WasmSite } from './quarb_wasm.js';

let ready;

/** Initialize the engine once; subsequent calls share the load. */
export function initQuarb(input) {
  if (!ready) {
    if (input === undefined && typeof process !== 'undefined' && process.versions?.node) {
      ready = import('node:fs/promises').then(async ({ readFile }) =>
        init({ module_or_path: await readFile(new URL('./quarb_wasm_bg.wasm', import.meta.url)) })
      );
    } else {
      ready = init(input === undefined ? undefined : { module_or_path: input });
    }
  }
  return ready;
}

/**
 * Run a Quarb query over a text document. Resolves to the result
 * lines; rejects with the engine's parse/execution error.
 */
export async function query(format, input, q, opts = {}) {
  await initQuarb(opts.wasm);
  const r = JSON.parse(run(format, input, q, opts.now ?? Date.now()));
  if (!r.ok) throw new Error(r.error);
  return r.lines;
}

/**
 * A website loaded once and queried many times at the web level:
 * every page under `/sites/<host>/pages/<path>`, the tags and
 * categories the pages declare as rows of their own, every
 * hyperlink resolved (`->link`, `<-link`), each page readable at
 * the text level (`//section`, `//paragraph`). Construct with
 * {@link openSite}.
 */
export class Site {
  #inner;
  constructor(inner) { this.#inner = inner; }

  /** Run a query over the site; the result lines, or a throw. */
  query(q, opts = {}) {
    const r = JSON.parse(this.#inner.query(q, opts.now ?? Date.now()));
    if (!r.ok) throw new Error(r.error);
    return r.lines;
  }

  /** The raw JSON envelope of a query; never throws. */
  run(q, now = Date.now()) { return this.#inner.query(q, now); }

  /** Every page as a locator, sorted. */
  entries() { return JSON.parse(this.#inner.entries()); }

  /** Every page's locator with its declared `<title>`. */
  titles() { return JSON.parse(this.#inner.titles()); }

  /** Release the wasm memory the site holds. */
  free() { this.#inner.free(); }
}

/**
 * Open a site. `source` is a tarball of its pages (`.tar` or
 * `.tar.gz`, as bytes) or a plain object of site-relative path to
 * HTML for a site small enough to hold in memory. `opts.base` is
 * the site's root URL that paths and relative links join against
 * (default: read off a page's canonical URL, else
 * `https://localhost/`); `opts.model` a Quarb model file over the
 * site; `opts.wasm` forwards to {@link initQuarb}.
 */
export async function openSite(source, opts = {}) {
  await initQuarb(opts.wasm);
  const base = opts.base ?? '';
  const model = opts.model ?? '';
  const inner =
    source instanceof Uint8Array || source instanceof ArrayBuffer
      ? WasmSite.open(source instanceof Uint8Array ? source : new Uint8Array(source), base, model)
      : WasmSite.from_pages(JSON.stringify(source), base, model);
  return new Site(inner);
}

export { run, version };
