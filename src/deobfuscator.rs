//! Deobfuscator - Main entry point for deobfuscation
//!
//! This module provides the main `Deobfuscator` struct that orchestrates
//! the deobfuscation process.

use alloc::sync::Arc;
use core::fmt;

use swc_common::{
    FileName, GLOBALS, Globals, SourceMap, SourceMapper,
    errors::{ColorConfig, Handler},
    sync::Lrc,
};
use swc_ecma_ast::{EsVersion, Program};
use swc_ecma_codegen::{Config as CodegenConfig, Emitter, text_writer::JsWriter};
use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax, lexer::Lexer};

use crate::context::Context;
use crate::error::{DeobfuscateError, Result};
#[cfg(feature = "cli")]
use crate::transformers::TransformerConfig;
use crate::transformers::{self, TransformerBox};

/// Source type for parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[expect(
    clippy::exhaustive_enums,
    reason = "public API uses a closed enum for stable parsing options"
)]
pub enum SourceType {
    /// Parse as ES module
    Module,
    /// Parse as script
    Script,
    /// Try module first, then script
    #[default]
    Both,
}

/// Options for deobfuscation.
///
/// When `custom_transformers` is `None` or an empty list, the default
/// transformer pipeline is used.
#[derive(Clone)]
pub struct DeobfuscateOptions {
    /// Source type for parsing
    pub source_type: SourceType,

    /// Whether to rename identifiers
    pub rename: bool,

    /// ECMAScript version to use when parsing (None = latest)
    pub ecma_version: Option<EsVersion>,

    /// Whether to apply transformChainExpressions (not implemented in Rust yet)
    pub transform_chain_expressions: bool,

    /// Loose parsing (acorn-loose equivalent; not implemented in Rust yet)
    pub loose: bool,

    /// Custom transformer list (None or empty = default)
    pub custom_transformers: Option<Vec<TransformerBox>>,

    /// Custom transformers by name + options (TS config shape)
    #[cfg(feature = "cli")]
    pub custom_transformer_configs: Option<Vec<TransformerConfig>>,
}

impl fmt::Debug for DeobfuscateOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut ds = f.debug_struct("DeobfuscateOptions");
        ds.field("source_type", &self.source_type)
            .field("rename", &self.rename)
            .field("ecma_version", &self.ecma_version)
            .field(
                "transform_chain_expressions",
                &self.transform_chain_expressions,
            )
            .field("loose", &self.loose)
            .field(
                "custom_transformers",
                &self.custom_transformers.as_ref().map(|list| list.len()),
            );
        #[cfg(feature = "cli")]
        ds.field(
            "custom_transformer_configs",
            &self.custom_transformer_configs,
        );
        ds.finish()
    }
}

impl Default for DeobfuscateOptions {
    fn default() -> Self {
        Self {
            source_type: SourceType::Both,
            rename: false,
            ecma_version: None,
            transform_chain_expressions: true,
            loose: false,
            custom_transformers: None,
            #[cfg(feature = "cli")]
            custom_transformer_configs: None,
        }
    }
}

/// Compute a hash of the source code (used for consistent renaming)
#[must_use]
fn source_hash(s: &str) -> u32 {
    let mut key: u32 = 0x94a3_fa21;
    for &byte in s.as_bytes().iter().rev() {
        key = key.wrapping_mul(33) ^ u32::from(byte);
    }
    key
}

/// The main deobfuscator.
///
/// # Examples
/// ```rust
/// use synchrony_rs::{DeobfuscateOptions, Deobfuscator};
/// use synchrony_rs::transformers::Simplify;
/// use std::sync::Arc;
///
/// let deob = Deobfuscator::new();
/// let options = DeobfuscateOptions {
///     custom_transformers: Some(vec![Arc::new(Simplify::new())]),
///     ..Default::default()
/// };
/// let output = deob.deobfuscate_source("var a = 1;", Some(options)).unwrap();
/// assert!(output.contains("a"));
/// ```
#[derive(Debug)]
pub struct Deobfuscator {
    /// Default options
    pub default_options: DeobfuscateOptions,
}

impl Default for Deobfuscator {
    fn default() -> Self {
        Self::new()
    }
}

impl Deobfuscator {
    /// Create a new deobfuscator with default options
    #[must_use]
    pub fn new() -> Self {
        Self {
            default_options: DeobfuscateOptions::default(),
        }
    }

    /// Create a new deobfuscator with the given options
    #[must_use]
    pub const fn with_options(options: DeobfuscateOptions) -> Self {
        Self {
            default_options: options,
        }
    }

    /// Merge user options with defaults
    #[must_use]
    fn build_options(&self, options: Option<DeobfuscateOptions>) -> DeobfuscateOptions {
        options.unwrap_or_else(|| self.default_options.clone())
    }

    /// Parse JavaScript source code into an AST
    fn parse(
        &self,
        source: &str,
        source_type: SourceType,
        ecma_version: Option<EsVersion>,
    ) -> Result<(Program, Lrc<SourceMap>, bool)> {
        let cm: Lrc<SourceMap> = Lrc::new(SourceMap::default());
        let handler_cm: Lrc<dyn SourceMapper> = Lrc::<SourceMap>::clone(&cm);
        let handler = Handler::with_tty_emitter(ColorConfig::Auto, true, false, Some(handler_cm));

        let fm = cm.new_source_file(Lrc::new(FileName::Anon), source.to_owned());

        let syntax = Syntax::Es(EsSyntax {
            jsx: false,
            ..Default::default()
        });

        // Try to parse based on source type
        match source_type {
            SourceType::Module => self.parse_as_module(&fm, syntax, &handler, cm, ecma_version),
            SourceType::Script => self.parse_as_script(&fm, syntax, &handler, cm, ecma_version),
            SourceType::Both => {
                // Try module first, then script
                self.parse_as_module(&fm, syntax, &handler, Lrc::clone(&cm), ecma_version)
                    .or_else(|_| self.parse_as_script(&fm, syntax, &handler, cm, ecma_version))
            }
        }
    }

    fn parse_as_module(
        &self,
        fm: &swc_common::SourceFile,
        syntax: Syntax,
        _handler: &Handler,
        cm: Lrc<SourceMap>,
        ecma_version: Option<EsVersion>,
    ) -> Result<(Program, Lrc<SourceMap>, bool)> {
        let version = ecma_version.unwrap_or_else(EsVersion::latest);
        let lexer = Lexer::new(syntax, version, StringInput::from(fm), None);
        let mut parser = Parser::new_from(lexer);

        parser
            .parse_module()
            .map(|m| (Program::Module(m), cm, true))
            .map_err(|e| DeobfuscateError::ParseError(format!("{e:?}")))
    }

    fn parse_as_script(
        &self,
        fm: &swc_common::SourceFile,
        syntax: Syntax,
        _handler: &Handler,
        cm: Lrc<SourceMap>,
        ecma_version: Option<EsVersion>,
    ) -> Result<(Program, Lrc<SourceMap>, bool)> {
        let version = ecma_version.unwrap_or_else(EsVersion::latest);
        let lexer = Lexer::new(syntax, version, StringInput::from(fm), None);
        let mut parser = Parser::new_from(lexer);

        parser
            .parse_script()
            .map(|s| (Program::Script(s), cm, false))
            .map_err(|e| DeobfuscateError::ParseError(format!("{e:?}")))
    }

    /// Generate JavaScript code from an AST
    fn generate(
        &self,
        program: &Program,
        cm: Lrc<SourceMap>,
        ecma_version: Option<EsVersion>,
    ) -> Result<String> {
        let version = ecma_version.unwrap_or_else(EsVersion::latest);
        let mut buf = Vec::new();

        {
            let writer = JsWriter::new(Lrc::clone(&cm), "\n", &mut buf, None);
            let config = CodegenConfig::default()
                .with_target(version)
                .with_minify(false);

            let mut emitter = Emitter {
                cfg: config,
                cm,
                comments: None,
                wr: writer,
            };

            emitter
                .emit_program(program)
                .map_err(|e| DeobfuscateError::CodegenError(format!("{e:?}")))?;
        };

        String::from_utf8(buf)
            .map_err(|e| DeobfuscateError::CodegenError(format!("Invalid UTF-8: {e}")))
    }

    /// Deobfuscate an AST
    ///
    /// # Errors
    ///
    /// Returns an error if any transformer in the pipeline fails.
    pub fn deobfuscate_program(
        &self,
        program: Program,
        options: Option<DeobfuscateOptions>,
        transformers: Option<Vec<TransformerBox>>,
    ) -> Result<Program> {
        self.deobfuscate_program_internal(program, options, transformers, None)
    }

    /// Deobfuscate an AST with optional source string
    fn deobfuscate_program_internal(
        &self,
        program: Program,
        options: Option<DeobfuscateOptions>,
        transformers: Option<Vec<TransformerBox>>,
        source: Option<String>,
    ) -> Result<Program> {
        let options = self.build_options(options);
        let is_module = matches!(program, Program::Module(_));

        let transformer_list = self.build_transformers(&options, transformers)?;

        let source_hash_value = source.as_ref().map(|src| source_hash(src));
        let mut context = Context::new(program, transformer_list, is_module, source);
        context.rename_enabled = options.rename;
        if let Some(hash) = source_hash_value {
            context.hash = hash;
        }

        self.run_transformers(&mut context)?;

        // Handle renaming if requested
        if options.rename {
            let out_cm: Lrc<SourceMap> = Lrc::new(SourceMap::default());
            let regenerated_source = self.generate(&context.ast, out_cm, options.ecma_version)?;
            let hash = source_hash(&regenerated_source);

            let (program, _cm, is_module) = self.parse(
                &regenerated_source,
                options.source_type,
                options.ecma_version,
            )?;

            let mut rename_context = Context::new(
                program,
                vec![
                    Arc::new(transformers::Rename::new()),
                    Arc::new(transformers::DeadCodeSafe::new()),
                ],
                is_module,
                Some(regenerated_source),
            );
            rename_context.hash = hash;

            self.run_transformers(&mut rename_context)?;

            return Ok(rename_context.ast);
        }

        Ok(context.ast)
    }

    fn run_transformers(&self, context: &mut Context) -> Result<()> {
        let transformers = context.transformers.clone();
        for transformer in transformers {
            crate::log_info!("Running {} transformer", transformer.name());
            transformer.transform(context)?;
        }
        Ok(())
    }

    fn build_transformers(
        &self,
        options: &DeobfuscateOptions,
        transformers: Option<Vec<TransformerBox>>,
    ) -> Result<Vec<TransformerBox>> {
        if let Some(transformers) = transformers {
            return Ok(transformers);
        }

        if let Some(custom) = &options.custom_transformers {
            if custom.is_empty() {
                return Ok(transformers::default_transformers());
            }

            return Ok(custom.clone());
        }

        #[cfg(feature = "cli")]
        if let Some(configs) = &options.custom_transformer_configs {
            if configs.is_empty() {
                return Ok(transformers::default_transformers());
            }

            return transformers::transformers_from_configs(configs);
        }

        Ok(transformers::default_transformers())
    }

    /// Deobfuscate JavaScript source code
    ///
    /// # Errors
    ///
    /// Returns an error if parsing fails, a transformer fails, or code generation fails.
    pub fn deobfuscate_source(
        &self,
        source: &str,
        options: Option<DeobfuscateOptions>,
    ) -> Result<String> {
        let options = self.build_options(options);
        if options.loose {
            crate::log_info!("Loose parsing is not supported yet; using strict parser.");
        }
        let (program, _cm, _is_module) =
            self.parse(source, options.source_type, options.ecma_version)?;

        // Create new source map for output
        let out_cm: Lrc<SourceMap> = Lrc::new(SourceMap::default());

        let ecma_version = options.ecma_version;
        GLOBALS.set(&Globals::default(), || {
            let program = self.deobfuscate_program_internal(
                program,
                Some(options),
                None,
                Some(source.to_owned()),
            )?;
            self.generate(&program, out_cm, ecma_version)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transformers::Simplify;
    use alloc::sync::Arc;

    fn run_with_simplify(code: &str) -> String {
        let deob = Deobfuscator::new();
        let options = DeobfuscateOptions {
            custom_transformers: Some(vec![Arc::new(Simplify::new())]),
            ..Default::default()
        };
        deob.deobfuscate_source(code, Some(options)).unwrap()
    }

    #[test]
    fn source_hash_works() {
        let hash = source_hash("hello world");
        assert_ne!(hash, 0);

        // Same input should produce same hash
        assert_eq!(hash, source_hash("hello world"));

        // Different input should produce different hash
        assert_ne!(hash, source_hash("hello world!"));
    }

    #[test]
    fn parse_simple() {
        let deob = Deobfuscator::new();
        let result = deob.parse("const x = 1;", SourceType::Script, None);
        result.unwrap();
    }

    #[test]
    fn parse_module() {
        let deob = Deobfuscator::new();
        let result = deob.parse("export const x = 1;", SourceType::Module, None);
        result.unwrap();
    }

    #[test]
    fn deobfuscate_constant_folding() {
        let result = run_with_simplify("const x = 1 + 2 + 3;");
        assert!(result.contains("const x = 6"));
    }

    #[test]
    fn deobfuscate_boolean_simplification() {
        let result = run_with_simplify("const a = !0; const b = !1;");
        assert!(result.contains("true"));
        assert!(result.contains("false"));
    }

    #[test]
    fn deobfuscate_string_concat() {
        let result = run_with_simplify(r#"const s = "Hello" + "World";"#);
        assert!(result.contains("HelloWorld"));
    }

    #[test]
    fn deobfuscate_member_expression() {
        let deob = Deobfuscator::new();
        let result = deob
            .deobfuscate_source(r#"console["log"]("test");"#, None)
            .unwrap();
        assert!(result.contains("console.log"));
    }

    #[test]
    fn deobfuscate_dead_code() {
        let deob = Deobfuscator::new();
        let result = deob
            .deobfuscate_source("if (false) { dead(); } if (true) { alive(); }", None)
            .unwrap();
        assert!(!result.contains("dead"));
        assert!(result.contains("alive"));
    }

    #[test]
    fn deobfuscate_sequence() {
        let deob = Deobfuscator::new();
        let result = deob.deobfuscate_source("a = 1, b = 2;", None).unwrap();
        assert!(result.contains("a = 1;"));
        assert!(result.contains("b = 2;"));
    }

    #[test]
    fn deobfuscate_conditional() {
        let result = run_with_simplify(r"const x = true ? 'yes' : 'no';");
        assert!(result.contains("yes"));
        assert!(!result.contains("no"));
    }

    #[test]
    fn verbose_option() {
        let deob = Deobfuscator::new();
        let options = DeobfuscateOptions {
            source_type: SourceType::Script,
            rename: false,
            ..Default::default()
        };
        let result = deob.deobfuscate_source("var x = 1;", Some(options));
        result.unwrap();
    }

    #[test]
    fn rename_pass_applied() {
        let deob = Deobfuscator::new();
        let code = r"
function _0x123(a1) {
    var _0xabc = a1;
    return _0xabc;
}
_0x123(1);
";
        let options = DeobfuscateOptions {
            source_type: SourceType::Script,
            rename: true,
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(!result.contains("_0x123"));
        assert!(!result.contains("_0xabc"));
        assert!(!result.contains("a1"));
    }

    #[test]
    fn rename_pass_preserves_non_obfuscated() {
        let deob = Deobfuscator::new();
        let code = r"
function normalName(value) {
    var count = value + 1;
    return count;
}
normalName(1);
";
        let options = DeobfuscateOptions {
            source_type: SourceType::Script,
            rename: true,
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(result.contains("normalName"));
        assert!(result.contains("count"));
        assert!(result.contains("value"));
    }

    #[test]
    fn custom_transformers_override_default() {
        let deob = Deobfuscator::new();
        let code = r#"console["log"]("test");"#;
        let options = DeobfuscateOptions {
            source_type: SourceType::Script,
            rename: false,
            custom_transformers: Some(vec![Arc::new(transformers::Simplify::new())]),
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(!result.contains("console.log"));
    }

    #[test]
    fn ecma_version_option() {
        let deob = Deobfuscator::new();
        let options = DeobfuscateOptions {
            source_type: SourceType::Script,
            rename: false,
            ecma_version: Some(EsVersion::Es2020),
            ..Default::default()
        };
        let result = deob.deobfuscate_source("const x = 1;", Some(options));
        result.unwrap();
    }
}
