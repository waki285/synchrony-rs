# wasm-workers example

This example runs synchrony-rs inside Cloudflare Workers.

## Build

1) Install wasm-pack
   - macOS: brew install wasm-pack
   - Other: cargo install wasm-pack

2) Add the wasm target

   rustup target add wasm32-unknown-unknown

3) Build the wasm package from the repo root (stable Rust does not support
   wasm-pack's `--out-dir`, so copy `pkg/` instead)

   wasm-pack build --target bundler --no-default-features --features wasm
   cp -R pkg examples/wasm-workers/

This produces ./examples/wasm-workers/pkg.

Note: the bundler target is for Workers and bundlers. Do not use it for the
browser example (use `--target web` there).

## Run

From this folder:

  npm install -D wrangler
  npx wrangler dev

Then open the dev URL shown by wrangler.

## Files

- wrangler.toml: Workers config
- src/worker.js: fetch handler using deobfuscate()
