//! WASM-only logging bridge for JS UIs.

use std::cell::RefCell;

use js_sys::Function;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Off = 0,
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
}

impl LogLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            LogLevel::Off => "off",
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, JsValue> {
        match raw.trim().to_lowercase().as_str() {
            "off" => Ok(LogLevel::Off),
            "error" => Ok(LogLevel::Error),
            "warn" | "warning" => Ok(LogLevel::Warn),
            "info" => Ok(LogLevel::Info),
            "debug" => Ok(LogLevel::Debug),
            other => Err(JsValue::from_str(&format!(
                "Unknown log level: {}",
                other
            ))),
        }
    }
}

thread_local! {
    static LOG_LEVEL: RefCell<LogLevel> = RefCell::new(LogLevel::Info);
    static LOG_SINK: RefCell<Option<Function>> = RefCell::new(None);
}

pub fn set_log_level(level: &str) -> Result<(), JsValue> {
    let parsed = LogLevel::parse(level)?;
    LOG_LEVEL.with(|lvl| *lvl.borrow_mut() = parsed);
    Ok(())
}

pub fn set_log_sink(callback: Option<Function>) {
    LOG_SINK.with(|sink| {
        *sink.borrow_mut() = callback;
    });
}

fn should_log(level: LogLevel) -> bool {
    LOG_LEVEL.with(|lvl| level <= *lvl.borrow())
}

pub fn log(level: LogLevel, message: &str) {
    if !should_log(level) {
        return;
    }

    LOG_SINK.with(|sink| {
        let callback = sink.borrow().clone();
        let Some(callback) = callback else { return };
        let level_value = JsValue::from_str(level.as_str());
        let msg_value = JsValue::from_str(message);
        let _ = callback.call2(&JsValue::NULL, &level_value, &msg_value);
    });
}

pub fn set_log_sink_from_value(value: JsValue) -> Result<(), JsValue> {
    if value.is_null() || value.is_undefined() {
        set_log_sink(None);
        return Ok(());
    }
    let func: Function = value
        .dyn_into()
        .map_err(|_| JsValue::from_str("log sink must be a function"))?;
    set_log_sink(Some(func));
    Ok(())
}
