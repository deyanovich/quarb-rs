// Type surface of @quarb/wasm/quai — the session engine.

import { QuaiSession } from './quai_wasm.js';

export { QuaiSession, highlight, version } from './quai_wasm.js';

/** A source to mount: text in one of the session formats, or a
 * SQLite database as base64 bytes. */
export interface Source {
  name: string;
  format:
    | 'json'
    | 'jsonl'
    | 'ndjson'
    | 'yaml'
    | 'toml'
    | 'csv'
    | 'tsv'
    | 'xml'
    | 'html'
    | 'markdown'
    | 'kaiv'
    | 'daiv'
    | 'sqlite';
  text?: string;
  bytes_b64?: string;
}

/**
 * Initialize the session engine once; subsequent calls share the
 * load. Browsers and bundlers fetch the .wasm beside the module;
 * Node reads it from disk. Pass `input` to override.
 */
export function initQuai(
  input?: BufferSource | WebAssembly.Module | Response | Promise<Response>
): Promise<unknown>;

/**
 * Mount a source set as a session. One source mounts path-bare;
 * two or more mount as named children of one root, so a single
 * query — including a `<=>` join — spans them all. Lines run via
 * `session.run(line)` (the REPL dispatch: queries, `def`s, `&N`
 * recalls) or `session.run_cell(text)` (a notebook cell as a
 * unit); both return a JSON envelope string
 * `{label, lines, note, error}`.
 */
export function mount(
  sources: Source[],
  opts?: {
    now?: number;
    wasm?: BufferSource | WebAssembly.Module | Response | Promise<Response>;
  }
): Promise<QuaiSession>;
