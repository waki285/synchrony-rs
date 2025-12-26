# wasm-web example

This example loads synchrony-rs in a browser using wasm-pack (target: web).

## Build

1) Install wasm-pack
   - macOS: brew install wasm-pack
   - Other: cargo install wasm-pack

2) Add the wasm target

   rustup target add wasm32-unknown-unknown

3) Build the wasm package from the repo root (stable Rust does not support
   wasm-pack's `--out-dir`, so copy `pkg/` instead)

   wasm-pack build --target web --no-default-features --features wasm
   cp -R pkg examples/wasm-web/

This produces ./examples/wasm-web/pkg.

## Run

Serve this folder with a static server and open index.html.
Example (recommended):

  npx http-server -p 8080 -c-1

If you prefer Python, make sure `.wasm` is served as `application/wasm`:

  python3 - <<'PY'
  import http.server, socketserver, mimetypes
  mimetypes.add_type("application/wasm", ".wasm")
  handler = http.server.SimpleHTTPRequestHandler
  with socketserver.TCPServer(("", 8080), handler) as httpd:
      httpd.serve_forever()
  PY

Then open:
  [http://localhost:8080]

## Troubleshooting

- If the browser says `text/html` for a JS file, you are likely serving from the
  wrong folder or the `pkg/` directory is missing.
- If the browser complains about `application/wasm` or ES module loading, you
  probably built with `--target bundler` instead of `--target web`.

## Files

- index.html: minimal page
- main.js: loads the wasm package and calls deobfuscate()
