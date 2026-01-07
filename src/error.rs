//! Error types for the deobfuscator.

use std::result::Result as CoreResult;
use thiserror::Error;

/// Errors returned by the deobfuscator APIs.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum DeobfuscateError {
    #[error("Failed to parse JavaScript: {0}")]
    /// Parsing failed for the provided JavaScript source.
    ParseError(String),

    #[error("Failed to generate code: {0}")]
    /// Code generation failed after transforming the AST.
    CodegenError(String),

    #[error("Transformer error: {0}")]
    /// A transformer reported an error.
    TransformerError(String),

    #[error("Invalid transformer: {0}")]
    /// A transformer could not be constructed or validated.
    InvalidTransformer(String),
}

/// Result type used by the public API.
pub type Result<T> = CoreResult<T, DeobfuscateError>;
