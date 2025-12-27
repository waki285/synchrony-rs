#!/usr/bin/env bash
set -euo pipefail

if ! command -v wasm-pack >/dev/null 2>&1; then
  echo "wasm-pack not found. Install with: cargo install wasm-pack" >&2
  exit 1
fi

wasm-pack build --release --target web --no-default-features --features wasm

mkdir -p public/pkg
cp -R pkg/* public/pkg/

echo "Built web UI into ./public (pkg copied)."
