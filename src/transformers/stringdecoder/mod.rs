//! StringDecoder transformer
//!
//! Decodes obfuscated strings using string arrays and decoder functions.
//! Supports Simple, Base64, and RC4 decoding methods.
//!
//! Main components:
//! - `StringArrayFinder`: Detects string arrays (both variable and function forms)
//! - `DecoderFunctionFinder`: Detects decoder functions with offset/charset
//! - `VariableReferenceFinder`: Detects variable aliases to decoders
//! - `FunctionReferenceFinder`: Detects function wrappers around decoders
//! - `StringDecoderReplacer`: Replaces decoder calls with actual strings

mod arrays;
mod core;
mod decoder_finder;
mod decoder_helper;
mod garbage;
mod references;
mod replacer;
mod shift;

#[cfg(test)]
mod tests;

pub use core::StringDecoder;
