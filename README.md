# synchrony-rs

[![crates.io](https://img.shields.io/crates/v/synchrony-rs.svg)](https://crates.io/crates/synchrony-rs)
[![docs.rs](https://img.shields.io/docsrs/synchrony-rs.svg)](https://docs.rs/synchrony-rs)
[![downloads](https://img.shields.io/crates/dv/synchrony-rs.svg)](https://crates.io/crates/synchrony-rs)
[![license](https://img.shields.io/crates/l/synchrony-rs.svg)](https://crates.io/crates/synchrony-rs)
[![CI](https://img.shields.io/github/actions/workflow/status/waki285/synchrony-rs/ci.yml?branch=main)](https://github.com/waki285/synchrony-rs/actions/workflows/ci.yml)

![rip javascript-obfuscator](/.github/hm.png)

This project is a Rust port of [relative/synchrony](https://github.com/relative/synchrony) with additional
enhancements for performance and analysis robustness. It targets
obfuscation outputs primarily from
[javascript-obfuscator](https://github.com/javascript-obfuscator/javascript-obfuscator) /
[obfuscator.io](https://obfuscator.io).

## Features

- Multiple transformer passes (string decoder, array map, control-flow recovery)
- Fast Rust implementation
- Available as both a CLI and a library

## Usage (CLI)

```shell
# Install
cargo install synchrony-rs
# cargo install synchrony-rs --locked # (alternative)

# Build from source
cargo build --release

# Run
synchrony ./input.js
# ./target/release/synchrony ./input.js

# Output defaults to ./input.cleaned.js
```

## Usage (Library)

```rust
use synchrony_rs::Deobfuscator;

let deob = Deobfuscator::new();
let output = deob.deobfuscate_source("var a = 1;", None).unwrap();
println!("{output}");
```

## Usage (WASM: Browser / Workers)

Build the wasm package (no CLI, wasm-only bindings). On stable Rust, avoid
`--out-dir` and copy the generated `pkg/` instead:

```shell
# Browser (web target)
wasm-pack build --target web --no-default-features --features wasm
cp -R pkg examples/wasm-web/

# Cloudflare Workers (bundler target)
wasm-pack build --target bundler --no-default-features --features wasm
cp -R pkg examples/wasm-workers/
```

Note: do not mix web/bundler outputs. The web target is for browsers, the
bundler target is for Workers (and other bundlers).

JS example (browser or Workers):

```js
import init, { deobfuscate } from "./pkg/synchrony_rs.js";

await init();
const output = deobfuscate("var a = 1 + 2 + 3;", {
  rename: false,
  sourceType: "script",
  ecmaVersion: "es2020",
});
console.log(output);
```

See the ready-to-run examples:

- `examples/wasm-web`
- `examples/wasm-workers`

## Web UI (Cloudflare Pages)

A static web UI lives in `public/` and is meant to be deployed with Cloudflare Pages.

Build command (from repo root):

```shell
./scripts/build-pages.sh
```

Pages settings:
- Build command: `cargo install wasm-pack --locked && ./scripts/build-pages.sh`
- Build output directory: `public`

Notes:
- The script builds the WASM web target and copies `pkg/` into `public/pkg/`.
- Use `--target web` for the browser UI. Do not use the bundler output here.

## Notes

- Outputs from very old versions of javascript-obfuscator may not deobfuscate
  correctly. If that happens, try another synchrony version or a different
  deobfuscator.
- The pipeline runs automatically; user configuration is intentionally minimal.

## Troubleshooting

If a transformer fails, you will see a message like:

```txt
Error: Transformer error: ...
```

Please share the full terminal output and the input file (plus a minimal
reproduction if possible). Avoid screenshots or partial logs.

Tip: build with the `tracing` feature (enabled by default) and set
`RUST_LOG=debug` to get more detail about which transformer ran last.
