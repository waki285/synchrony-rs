import init, { deobfuscate } from "./pkg/synchrony_rs.js";

async function run() {
  await init();

  const input = "var a = 1 + 2 + 3;";
  const output = deobfuscate(input, {
    rename: false,
    sourceType: "script",
    ecmaVersion: "es2020",
  });

  console.log("input:", input);
  console.log("output:", output);
}

run().catch((err) => console.error(err));
