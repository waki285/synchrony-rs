//! WASM bindings (wasm-bindgen).

use serde::Deserialize;
use wasm_bindgen::prelude::*;

use crate::deobfuscator::{DeobfuscateOptions, Deobfuscator};
use crate::options::{parse_es_version_str, parse_source_type_str};
use crate::wasm_logger;

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct WasmOptions {
    rename: Option<bool>,
    source_type: Option<String>,
    ecma_version: Option<String>,
}

fn js_error(message: impl AsRef<str>) -> JsValue {
    JsValue::from_str(message.as_ref())
}

fn build_options(options: Option<WasmOptions>) -> Result<DeobfuscateOptions, JsValue> {
    let mut out = DeobfuscateOptions::default();
    if let Some(opts) = options {
        if let Some(rename) = opts.rename {
            out.rename = rename;
        }
        if let Some(source_type) = opts.source_type {
            out.source_type = parse_source_type_str(&source_type).map_err(|e| js_error(e))?;
        }
        if let Some(ecma_version) = opts.ecma_version {
            out.ecma_version = Some(parse_es_version_str(&ecma_version).map_err(js_error)?);
        }
    }
    Ok(out)
}

/// Deobfuscate JavaScript source code.
///
/// `options` accepts `{ rename?: boolean, sourceType?: "script"|"module"|"both",
/// ecmaVersion?: string }`.
#[wasm_bindgen]
pub fn deobfuscate(source: &str, options: JsValue) -> Result<String, JsValue> {
    let opts = if options.is_null() || options.is_undefined() {
        None
    } else {
        Some(serde_wasm_bindgen::from_value(options).map_err(|e| js_error(e.to_string()))?)
    };

    let options = build_options(opts)?;
    let deob = Deobfuscator::new();
    deob.deobfuscate_source(source, Some(options))
        .map_err(|e| js_error(e.to_string()))
}

/// Set the log level for Rust-side logs forwarded to JS.
///
/// Accepted values: "off", "error", "warn", "info", "debug".
#[wasm_bindgen]
pub fn set_log_level(level: &str) -> Result<(), JsValue> {
    wasm_logger::set_log_level(level)
}

/// Set a JS callback to receive Rust-side logs.
///
/// The callback signature is: `(level: string, message: string) => void`.
#[wasm_bindgen]
pub fn set_log_sink(callback: JsValue) -> Result<(), JsValue> {
    wasm_logger::set_log_sink_from_value(callback)
}
