import init, { deobfuscate, set_log_level, set_log_sink } from "./pkg/synchrony_rs.js";
import { EditorState } from "https://esm.sh/@codemirror/state";
import { EditorView } from "https://esm.sh/@codemirror/view";
import { placeholder } from "https://esm.sh/@codemirror/view";
import { javascript } from "https://esm.sh/@codemirror/lang-javascript";
import { oneDark } from "https://esm.sh/@codemirror/theme-one-dark";
import { basicSetup } from "https://esm.sh/codemirror";

const inputHost = document.getElementById("input-editor");
const outputHost = document.getElementById("output-editor");
const runBtn = document.getElementById("run");
const sampleBtn = document.getElementById("sample");
const clearBtn = document.getElementById("clear");
const statusEl = document.getElementById("status");
const logEl = document.getElementById("log");
const renameEl = document.getElementById("rename");
const formatEl = document.getElementById("format");
const sourceTypeEl = document.getElementById("sourceType");
const ecmaEl = document.getElementById("ecma");
const copyBtn = document.getElementById("copy");
const downloadBtn = document.getElementById("download");
const logLevelEl = document.getElementById("logLevel");
const clearLogBtn = document.getElementById("clearLog");

function logLine(label, detail) {
  const line = document.createElement("div");
  line.className = "log-line";
  const strong = document.createElement("strong");
  strong.textContent = label;
  line.appendChild(strong);
  const span = document.createElement("span");
  span.textContent = detail;
  line.appendChild(span);
  logEl.prepend(line);
}

function clearLog() {
  logEl.innerHTML = "";
}

function setStatus(text, type) {
  statusEl.textContent = text;
  statusEl.style.color = type === "error" ? "#ff8b8b" : "var(--muted)";
}

function buildOptions() {
  const opts = {
    rename: renameEl.checked,
    format: formatEl.checked,
    sourceType: sourceTypeEl.value,
  };
  const ecma = ecmaEl.value.trim();
  if (ecma.length > 0) {
    opts.ecmaVersion = ecma;
  }
  return opts;
}

const inputEditor = new EditorView({
  state: EditorState.create({
    doc: "",
    extensions: [
      basicSetup,
      javascript(),
      oneDark,
      EditorView.lineWrapping,
      placeholder("Paste obfuscated JavaScript here..."),
    ],
  }),
  parent: inputHost,
});

const outputEditor = new EditorView({
  state: EditorState.create({
    doc: "",
    extensions: [
      basicSetup,
      javascript(),
      oneDark,
      EditorView.lineWrapping,
      EditorState.readOnly.of(true),
      EditorView.editable.of(false),
    ],
  }),
  parent: outputHost,
});

function setOutput(value) {
  outputEditor.dispatch({
    changes: { from: 0, to: outputEditor.state.doc.length, insert: value },
  });
  const hasOutput = value.trim().length > 0;
  copyBtn.disabled = !hasOutput;
  downloadBtn.disabled = !hasOutput;
}

function handleCopy() {
  const value = outputEditor.state.doc.toString();
  if (!value) return;
  navigator.clipboard.writeText(value).then(() => {
    logLine("copy", "output copied to clipboard");
  });
}

function handleDownload() {
  const value = outputEditor.state.doc.toString();
  if (!value) return;
  const blob = new Blob([value], { type: "text/plain" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = "deobfuscated.js";
  anchor.click();
  URL.revokeObjectURL(url);
  logLine("save", "downloaded deobfuscated.js");
}

async function run() {
  const source = inputEditor.state.doc.toString();
  if (!source.trim()) {
    setStatus("Input is empty.", "error");
    return;
  }
  setStatus("Deobfuscating…");
  const start = performance.now();
  try {
    const output = deobfuscate(source, buildOptions());
    const elapsed = Math.round(performance.now() - start);
    setOutput(output);
    setStatus(`Done in ${elapsed} ms`);
    logLine("done", `completed in ${elapsed} ms`);
  } catch (err) {
    setStatus("Failed to deobfuscate.", "error");
    logLine("error", err?.message ?? String(err));
  }
}

const sample = `function _0x123(a1){var _0xabc=a1;return _0xabc;}\nconsole["log"](_0x123(42));`;

sampleBtn.addEventListener("click", () => {
  inputEditor.dispatch({
    changes: { from: 0, to: inputEditor.state.doc.length, insert: sample },
  });
  setStatus("Sample loaded");
});

clearBtn.addEventListener("click", () => {
  inputEditor.dispatch({
    changes: { from: 0, to: inputEditor.state.doc.length, insert: "" },
  });
  setOutput("");
  setStatus("Cleared");
});

runBtn.addEventListener("click", run);
copyBtn.addEventListener("click", handleCopy);
downloadBtn.addEventListener("click", handleDownload);
clearLogBtn.addEventListener("click", clearLog);

logLevelEl.addEventListener("change", () => {
  const level = logLevelEl.value;
  set_log_level(level).catch((err) => {
    logLine("error", err?.message ?? String(err));
  });
});

setOutput("");
setStatus("Loading WASM…");
logLine("init", "initializing wasm module");

init()
  .then(() => {
    set_log_sink((level, message) => {
      logLine(level, message);
    });
    set_log_level(logLevelEl.value);
    runBtn.disabled = false;
    setStatus("WASM ready");
    logLine("ready", "wasm module initialized");
  })
  .catch((err) => {
    setStatus("WASM init failed", "error");
    logLine("error", err?.message ?? String(err));
  });
