//! WASM-only logging bridge for JS UIs.

use std::cell::RefCell;

use js_sys::Function;
use wasm_bindgen::prelude::*;

/// Log verbosity level for WASM log forwarding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum LogLevel {
    /// Disable logging.
    Off = 0,
    /// Error-level logs.
    Error = 1,
    /// Warning-level logs.
    Warn = 2,
    /// Informational logs.
    Info = 3,
    /// Debug-level logs.
    Debug = 4,
}

impl LogLevel {
    /// Returns the canonical string form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }

    /// Parse a string into a log level.
    ///
    /// # Errors
    ///
    /// Returns a JS error if the value is not a known log level.
    pub fn parse(raw: &str) -> Result<Self, JsValue> {
        match raw.trim().to_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "error" => Ok(Self::Error),
            "warn" | "warning" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            other => Err(JsValue::from_str(&format!("Unknown log level: {other}"))),
        }
    }
}

thread_local! {
    static LOG_LEVEL: RefCell<LogLevel> = const { RefCell::new(LogLevel::Info) };
    static LOG_SINK: RefCell<Option<Function>> = const { RefCell::new(None) };
}

/// Update the current log level.
///
/// # Errors
///
/// Returns a JS error if `level` is not recognized.
pub fn set_log_level(level: &str) -> Result<(), JsValue> {
    let parsed = LogLevel::parse(level)?;
    LOG_LEVEL.with(|lvl| *lvl.borrow_mut() = parsed);
    Ok(())
}

/// Set the JS log callback, or clear it with `None`.
pub fn set_log_sink(callback: Option<Function>) {
    LOG_SINK.with(|sink| {
        *sink.borrow_mut() = callback;
    });
}

fn should_log(level: LogLevel) -> bool {
    LOG_LEVEL.with(|lvl| level <= *lvl.borrow())
}

/// Emit a log message to the configured JS sink.
pub fn log(level: LogLevel, message: &str) {
    if !should_log(level) {
        return;
    }

    LOG_SINK.with(|sink| {
        let callback = sink.borrow().clone();
        let Some(callback) = callback else { return };
        let level_value = JsValue::from_str(level.as_str());
        let msg_value = JsValue::from_str(message);
        let _result = callback.call2(&JsValue::NULL, &level_value, &msg_value);
    });
}

/// Set the log sink from a raw JS value.
///
/// # Errors
///
/// Returns a JS error if the value is neither null/undefined nor a function.
pub fn set_log_sink_from_value(value: JsValue) -> Result<(), JsValue> {
    if value.is_null() || value.is_undefined() {
        set_log_sink(None);
        return Ok(());
    }
    let func: Function = value
        .dyn_into()
        .map_err(|err| JsValue::from_str(&format!("log sink must be a function: {err:?}")))?;
    set_log_sink(Some(func));
    Ok(())
}
