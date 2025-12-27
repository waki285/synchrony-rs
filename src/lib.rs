//! # synchrony-rs
//!
//! A fast JavaScript deobfuscator written in Rust.
//!
//! ## Quick start
//! ```rust
//! use synchrony_rs::Deobfuscator;
//!
//! let deob = Deobfuscator::new();
//! let output = deob.deobfuscate_source("var a = 1;", None).unwrap();
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
pub mod options;
pub mod scope;
pub mod transformers;
mod visitor;
mod words;

#[cfg(feature = "wasm")]
pub mod wasm;

pub use context::Context;
pub use deobfuscator::{DeobfuscateOptions, Deobfuscator, SourceType};
pub use error::{DeobfuscateError, Result};

/// Logging macros that are no-ops when tracing feature is disabled
#[cfg(feature = "tracing")]
macro_rules! log_info {
    ($($arg:tt)*) => { tracing::info!($($arg)*) }
}

#[cfg(not(feature = "tracing"))]
macro_rules! log_info {
    ($($arg:tt)*) => { let _ = || { let _ = ::core::format_args!($($arg)*); }; };
}

#[cfg(feature = "tracing")]
macro_rules! log_debug {
    ($($arg:tt)*) => { tracing::debug!($($arg)*) }
}

#[cfg(not(feature = "tracing"))]
macro_rules! log_debug {
    ($($arg:tt)*) => { let _ = || { let _ = ::core::format_args!($($arg)*); }; };
}

pub(crate) use log_debug;
pub(crate) use log_info;
