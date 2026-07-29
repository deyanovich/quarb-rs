#!/usr/bin/env bash
# Assemble the @quarb/wasm npm package: wasm-pack build (web
# target) + the hand-written wrapper, types, README, licenses.
# Output: quarb-wasm/npm/dist — `npm publish` from there.
set -eu
cd "$(dirname "$0")/.."

wasm-pack build --target web --release
(cd ../quai-wasm && wasm-pack build --target web --release)

DIST=npm/dist
rm -rf "$DIST"
mkdir -p "$DIST"
cp pkg/quarb_wasm.js pkg/quarb_wasm_bg.wasm pkg/quarb_wasm.d.ts "$DIST/"
cp ../quai-wasm/pkg/quai_wasm.js ../quai-wasm/pkg/quai_wasm_bg.wasm \
   ../quai-wasm/pkg/quai_wasm.d.ts "$DIST/"
cp npm/index.js npm/index.d.ts \
   npm/quai.js npm/quai.d.ts npm/README.md "$DIST/"
# The manifest lives as a template so `npm publish` in npm/ has no
# package.json to publish — only the assembled dist is a package
# (0.12.0 shipped broken from here; npm 12 ignored private:true).
cp npm/package.tmpl.json "$DIST/package.json"
cp ../LICENSE-MIT ../LICENSE-APACHE "$DIST/"

# The package version tracks the workspace version.
V=$(grep -m1 '^version' ../Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
sed -i "s/\"version\": \".*\"/\"version\": \"$V\"/" "$DIST/package.json"

echo "assembled $DIST at $V:"
ls -la "$DIST"
