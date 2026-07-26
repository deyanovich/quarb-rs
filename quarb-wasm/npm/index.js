// @quarb/wasm — the Quarb engine compiled to WebAssembly.
//
// This wrapper adds environment-aware initialization (browsers
// fetch the .wasm relative to the module URL; Node reads it from
// disk) and a typed `query()` that unwraps the engine's JSON
// envelope. The raw wasm-bindgen surface stays exported for
// callers that want to manage initialization themselves.

import init, { run, version } from './quarb_wasm.js';

let ready;

/** Initialize the engine once; subsequent calls share the load. */
export function initQuarb(input) {
  if (!ready) {
    if (input === undefined && typeof process !== 'undefined' && process.versions?.node) {
      ready = import('node:fs/promises').then(async ({ readFile }) =>
        init(await readFile(new URL('./quarb_wasm_bg.wasm', import.meta.url)))
      );
    } else {
      ready = init(input);
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

export { run, version };
