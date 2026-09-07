// Type surface of @quarb/wasm.

/** Text formats the bundled adapters parse. */
export type Format =
  | 'json'
  | 'yaml'
  | 'toml'
  | 'csv'
  | 'tsv'
  | 'xml'
  | 'html'
  | 'markdown'
  | 'text-html'
  | 'text-markdown'
  | 'text';

/**
 * Initialize the engine once; subsequent calls share the load.
 * In browsers and bundlers the .wasm is fetched relative to the
 * module URL; in Node it is read from disk. Pass `input` (bytes,
 * a compiled module, or a Response) to override.
 */
export function initQuarb(
  input?: BufferSource | WebAssembly.Module | Response | Promise<Response>
): Promise<unknown>;

/**
 * Run a Quarb query over a text document parsed as `format`.
 * Resolves to the result lines (one string per result row);
 * rejects with the engine's parse or execution error.
 *
 * `opts.now` pins the instant `now()` denotes (default
 * `Date.now()`); `opts.wasm` forwards to {@link initQuarb} when
 * the engine is not yet initialized.
 */
export function query(
  format: Format,
  input: string,
  q: string,
  opts?: {
    now?: number;
    wasm?: BufferSource | WebAssembly.Module | Response | Promise<Response>;
  }
): Promise<string[]>;

/**
 * The raw wasm-bindgen entry point: returns the engine's JSON
 * envelope as a string — `{"ok":true,"lines":[...]}` or
 * `{"ok":false,"error":"..."}` — and never throws. Requires
 * {@link initQuarb} to have completed.
 */
export function run(
  format: string,
  input: string,
  query: string,
  now_millis: number
): string;

/** The engine version, e.g. `"0.12.0"`. */
export function version(): string;

/**
 * A website loaded once and queried many times at the web level:
 * every page under `/sites/<host>/pages/<path>`, the tags and
 * categories the pages declare as rows of their own, every
 * hyperlink resolved (`->link`, `<-link`, `::in_degree`,
 * `::pagerank`), each page readable at the text level
 * (`//section`, `//paragraph`, `| link`). Construct with
 * {@link openSite}.
 */
export class Site {
  /**
   * Run a Quarb query over the site. Returns the result lines
   * (node results as locators such as
   * `/sites/example.org/pages/about.html!/section[2]`); throws on
   * a parse or execution error. `opts.now` pins `now()`.
   */
  query(q: string, opts?: { now?: number }): string[];
  /** The raw JSON envelope of a query; never throws. */
  run(q: string, now?: number): string;
  /** Every page as a locator, sorted. */
  entries(): string[];
  /** Every page's locator with its declared `<title>`. */
  titles(): Record<string, string>;
  /** Release the wasm memory the site holds. */
  free(): void;
}

/**
 * Open a site: a tarball of its pages (`.tar` or `.tar.gz`, as
 * bytes), or an object of site-relative path to HTML for a site
 * small enough to hold in memory.
 *
 * `opts.base` is the site's root URL (`https://example.org/`)
 * that page paths and relative links join against; by default it
 * is read off a page's canonical URL, and is `https://localhost/`
 * when no page declares one. `opts.model` is a Quarb model file
 * over the site (aliases, derived nodes, edges). `opts.wasm`
 * forwards to {@link initQuarb} when the engine is not yet
 * initialized.
 */
export function openSite(
  source: Uint8Array | ArrayBuffer | Record<string, string>,
  opts?: {
    base?: string;
    model?: string;
    wasm?: BufferSource | WebAssembly.Module | Response | Promise<Response>;
  }
): Promise<Site>;
