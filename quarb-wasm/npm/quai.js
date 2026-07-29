// @quarb/wasm/quai — the session engine. Where the default entry
// runs one query over one document, this one holds a session:
// multiple named sources mounted as children of one root (json,
// yaml, toml, csv, xml, html, markdown, kaiv, SQLite bytes),
// cross-source `<=>` joins, `&N` line history, and notebook-cell
// execution. The same build behind demo.quarb.org/quai and the
// Quarb Chrome extension.

import init, { QuaiSession, highlight, version } from './quai_wasm.js';

let ready;

/** Initialize the session engine once; subsequent calls share it. */
export function initQuai(input) {
  if (!ready) {
    if (input === undefined && typeof process !== 'undefined' && process.versions?.node) {
      ready = import('node:fs/promises').then(async ({ readFile }) =>
        init(await readFile(new URL('./quai_wasm_bg.wasm', import.meta.url)))
      );
    } else {
      ready = init(input);
    }
  }
  return ready;
}

/**
 * Mount a source set as a session. One source mounts path-bare;
 * two or more mount as named children of one root, so a single
 * query — including a `<=>` join — spans them all.
 */
export async function mount(sources, opts = {}) {
  await initQuai(opts.wasm);
  return QuaiSession.mount(JSON.stringify(sources), opts.now ?? Date.now());
}

export { QuaiSession, highlight, version };
