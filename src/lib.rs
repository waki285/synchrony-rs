//! # synchrony-rs
//!
//! A fast JavaScript deobfuscator written in Rust.
//!
//! ## Quick start
//! ```rust
//! use synchrony_rs::{DeobfuscateOptions, Deobfuscator};
//! use synchrony_rs::transformers::Simplify;
//! use std::sync::Arc;
//!
//! let deob = Deobfuscator::new();
//! let options = DeobfuscateOptions {
//!     custom_transformers: Some(vec![Arc::new(Simplify::new())]),
//!     ..Default::default()
//! };
//! let output = deob.deobfuscate_source("var a = 1;", Some(options)).unwrap();
//! assert!(output.contains("a"));
//! ```
//!
//! ## Custom options
//! ```rust
//! use synchrony_rs::{Deobfuscator, DeobfuscateOptions, SourceType};
//!
//! let deob = Deobfuscator::new();
//! let options = DeobfuscateOptions {
//!     source_type: SourceType::Script,
//!     rename: false,
//!     ..Default::default()
//! };
//! let _ = deob.deobfuscate_source("var a = 1;", Some(options)).unwrap();
//! ```
//!
//! ## Features
//! - `cli`: enables the `synchrony` binary (disabled in `no-default-features` builds).
//! - `tracing`: enables debug logging via the `tracing` crate.
//!
//! ## CLI
//! Build the CLI with default features and run `synchrony --help` for usage.

pub mod context;
pub mod deobfuscator;
pub mod error;
#[cfg(feature = "format")]
pub mod format;
pub mod options;
pub mod scope;
pub mod transformers;
mod visitor;
mod words;

#[cfg(feature = "wasm")]
pub mod wasm;
#[cfg(feature = "wasm")]
pub mod wasm_logger;

pub use context::Context;
pub use deobfuscator::{DeobfuscateOptions, Deobfuscator, SourceType};
pub use error::{DeobfuscateError, Result};

/// Logging macros. On WASM builds they forward to a JS log sink.
#[cfg(feature = "wasm")]
macro_rules! log_info {
    ($($arg:tt)*) => {
        crate::wasm_logger::log(
            crate::wasm_logger::LogLevel::Info,
            &format!($($arg)*),
        )
    }
}

#[cfg(all(not(feature = "wasm"), feature = "tracing"))]
macro_rules! log_info {
    ($($arg:tt)*) => { tracing::info!($($arg)*) }
}

#[cfg(all(not(feature = "wasm"), not(feature = "tracing")))]
macro_rules! log_info {
    ($($arg:tt)*) => { let _ = || { let _ = ::core::format_args!($($arg)*); }; };
}

#[cfg(feature = "wasm")]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        crate::wasm_logger::log(
            crate::wasm_logger::LogLevel::Debug,
            &format!($($arg)*),
        )
    }
}

#[cfg(all(not(feature = "wasm"), feature = "tracing"))]
macro_rules! log_debug {
    ($($arg:tt)*) => { tracing::debug!($($arg)*) }
}

#[cfg(all(not(feature = "wasm"), not(feature = "tracing")))]
macro_rules! log_debug {
    ($($arg:tt)*) => { let _ = || { let _ = ::core::format_args!($($arg)*); }; };
}

pub(crate) use log_debug;
pub(crate) use log_info;
