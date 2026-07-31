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
