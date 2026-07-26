#!/usr/bin/env bash
# Assemble the @quarb/wasm npm package: wasm-pack build (web
# target) + the hand-written wrapper, types, README, licenses.
# Output: quarb-wasm/npm/dist — `npm publish` from there.
set -eu
cd "$(dirname "$0")/.."

wasm-pack build --target web --release

DIST=npm/dist
rm -rf "$DIST"
mkdir -p "$DIST"
cp pkg/quarb_wasm.js pkg/quarb_wasm_bg.wasm pkg/quarb_wasm.d.ts "$DIST/"
cp npm/package.json npm/index.js npm/index.d.ts npm/README.md "$DIST/"
cp ../LICENSE-MIT ../LICENSE-APACHE "$DIST/"

# The package version tracks the workspace version.
V=$(grep -m1 '^version' ../Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
sed -i "s/\"version\": \".*\"/\"version\": \"$V\"/" "$DIST/package.json"

echo "assembled $DIST at $V:"
ls -la "$DIST"
