//! Transformer pipeline and exports.
//!
//! This module defines the `Transformer` trait and re-exports the built-in
//! deobfuscation passes used by the default pipeline.

mod antidebug;
mod arraymap;
mod controlflow;
mod deadcode;
mod demangle;
mod desequence;
mod jsconfuser_calculator;
mod jsconfuser_controlflow;
mod literalmap;
mod memberexpressioncleaner;
mod rename;
mod selfdefending;
mod simplify;
mod stringdecoder;

#[cfg(feature = "cli")]
use serde_json::Value;

pub use antidebug::AntiDebug;
pub use arraymap::ArrayMap;
pub use controlflow::ControlFlow;
pub use deadcode::{DeadCode, DeadCodeSafe};
pub use demangle::Demangle;
pub use desequence::Desequence;
pub use jsconfuser_calculator::JSConfuserCalculator;
pub use jsconfuser_controlflow::JSConfuserControlFlow;
pub use literalmap::LiteralMap;
pub use memberexpressioncleaner::MemberExpressionCleaner;
pub use rename::Rename;
pub use selfdefending::SelfDefending;
pub use simplify::Simplify;
pub use stringdecoder::StringDecoder;

use std::sync::Arc;

use crate::context::Context;
use crate::error::{DeobfuscateError, Result};

/// A shared transformer
pub type TransformerBox = Arc<dyn Transformer>;

/// Trait for all transformers
pub trait Transformer: Send + Sync {
    /// Get the name of this transformer
    fn name(&self) -> &'static str;

    /// Transform the AST in the given context
    ///
    /// # Errors
    ///
    /// Returns an error if the transformer fails to apply.
    fn transform(&self, context: &mut Context) -> Result<()>;
}

/// Options that can be passed to transformers (currently unused by Rust impl)
#[cfg(feature = "cli")]
pub type TransformerOptions = Value;

/// Options that can be passed to transformers (placeholder when config feature is off)
#[cfg(not(feature = "cli"))]
#[derive(Debug, Clone, Default)]
pub struct TransformerOptions;

/// Name + options for a transformer, matches the TypeScript config shape.
#[derive(Debug, Clone)]
pub struct TransformerConfig {
    pub name: String,
    pub options: TransformerOptions,
}

/// Create a transformer by name
///
/// # Errors
///
/// Returns an error if the transformer name is unknown.
pub fn create_transformer(name: &str) -> Result<TransformerBox> {
    match name.to_lowercase().as_str() {
        "antidebug" | "anti_debug" => Ok(Arc::new(AntiDebug::new())),
        "simplify" => Ok(Arc::new(Simplify::new())),
        "deadcode" => Ok(Arc::new(DeadCode::new())),
        "memberexpressioncleaner" => Ok(Arc::new(MemberExpressionCleaner::new())),
        "desequence" => Ok(Arc::new(Desequence::new())),
        "literalmap" => Ok(Arc::new(LiteralMap::new())),
        "demangle" => Ok(Arc::new(Demangle::new())),
        "controlflow" => Ok(Arc::new(ControlFlow::new())),
        "arraymap" => Ok(Arc::new(ArrayMap::new())),
        "stringdecoder" => Ok(Arc::new(StringDecoder::new())),
        "selfdefending" | "self_defending" => Ok(Arc::new(SelfDefending::new())),
        "rename" => Ok(Arc::new(Rename::new())),
        "jsconfusercalculator" | "jsconfuser_calculator" | "calculator" => {
            Ok(Arc::new(JSConfuserCalculator::new()))
        }
        "jsconfusercontrolflow" | "jsconfuser_controlflow" => {
            Ok(Arc::new(JSConfuserControlFlow::new()))
        }
        _ => Err(DeobfuscateError::InvalidTransformer(format!(
            "Unknown transformer: {name}"
        ))),
    }
}

#[cfg(feature = "cli")]
fn transformer_options_empty(options: &TransformerOptions) -> bool {
    match options {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        Value::Array(arr) => arr.is_empty(),
        _ => false,
    }
}

#[cfg(not(feature = "cli"))]
fn transformer_options_empty(_options: &TransformerOptions) -> bool {
    true
}

/// Create a transformer from config (options are currently ignored).
///
/// # Errors
///
/// Returns an error if the transformer name is unknown or options are invalid.
pub fn create_transformer_from_config(config: &TransformerConfig) -> Result<TransformerBox> {
    let transformer = create_transformer(&config.name)?;
    if !transformer_options_empty(&config.options) {
        crate::log_info!(
            "Transformer options for '{}' are currently ignored",
            config.name
        );
    }
    Ok(transformer)
}

/// Create transformers from config list (options are currently ignored).
///
/// # Errors
///
/// Returns an error if any transformer name is unknown or options are invalid.
pub fn transformers_from_configs(configs: &[TransformerConfig]) -> Result<Vec<TransformerBox>> {
    let mut out = Vec::with_capacity(configs.len());
    for cfg in configs {
        out.push(create_transformer_from_config(cfg)?);
    }
    Ok(out)
}

/// Create the default list of transformers
/// This mirrors the original TypeScript implementation
#[must_use]
pub fn default_transformers() -> Vec<TransformerBox> {
    vec![
        Arc::new(Simplify::new()),
        Arc::new(MemberExpressionCleaner::new()),
        Arc::new(LiteralMap::new()),
        Arc::new(DeadCode::new()),
        Arc::new(Demangle::new()),
        Arc::new(StringDecoder::new()),
        Arc::new(SelfDefending::new()),
        Arc::new(JSConfuserCalculator::new()), // JSConfuser support
        Arc::new(JSConfuserControlFlow::new()), // JSConfuser control flow
        Arc::new(Simplify::new()),
        Arc::new(MemberExpressionCleaner::new()),
        Arc::new(Desequence::new()),
        Arc::new(ControlFlow::new()),
        Arc::new(Desequence::new()),
        Arc::new(MemberExpressionCleaner::new()),
        Arc::new(ArrayMap::new()),
        Arc::new(SelfDefending::new()),
        Arc::new(Simplify::new()),
        Arc::new(DeadCode::new()),
        Arc::new(Simplify::new()),
        Arc::new(DeadCode::new()),
    ]
}

/// Create transformers with rename pass at the end
#[must_use]
pub fn transformers_with_rename() -> Vec<TransformerBox> {
    let mut transformers = default_transformers();
    if transformers
        .last()
        .is_some_and(|transformer| transformer.name() == "DeadCodeSafe")
    {
        transformers.pop();
    }
    transformers.push(Arc::new(Rename::new()));
    transformers.push(Arc::new(DeadCodeSafe::new()));
    transformers
}
