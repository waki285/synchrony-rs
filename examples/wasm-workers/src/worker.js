import init, { deobfuscate } from "../pkg/synchrony_rs.js";

let initPromise;

function ensureInit() {
  if (!initPromise) {
    initPromise = init();
  }
  return initPromise;
}

export default {
  async fetch(request) {
    await ensureInit();

    const input = "var a = 1 + 2 + 3;";
    const output = deobfuscate(input, {
      rename: false,
      sourceType: "script",
      ecmaVersion: "es2020",
    });

    return new Response(output, {
      headers: { "content-type": "text/plain; charset=utf-8" },
    });
  },
};
