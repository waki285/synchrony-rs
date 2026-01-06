//! Error types for the deobfuscator.

use thiserror::Error;

/// Errors returned by the deobfuscator APIs.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum DeobfuscateError {
    #[error("Failed to parse JavaScript: {0}")]
    ParseError(String),

    #[error("Failed to generate code: {0}")]
    CodegenError(String),

    #[error("Transformer error: {0}")]
    TransformerError(String),

    #[error("Invalid transformer: {0}")]
    InvalidTransformer(String),
}

/// Result type used by the public API.
pub type Result<T> = std::result::Result<T, DeobfuscateError>;
