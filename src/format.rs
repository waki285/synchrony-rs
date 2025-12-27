//! Biome-based JavaScript formatter helpers.

use std::error::Error;

use biome_formatter::{IndentStyle, IndentWidth};
use biome_js_formatter::{context::JsFormatOptions, format_node};
use biome_js_parser::{JsParserOptions, parse};
use biome_js_syntax::JsFileSource;

use crate::deobfuscator::SourceType;

pub fn format_js(source: &str, source_type: SourceType) -> Result<String, Box<dyn Error>> {
    let (syntax, file_source) = match source_type {
        SourceType::Module => {
            let file_source = JsFileSource::js_module();
            let parse = parse(source, file_source, JsParserOptions::default());
            if parse.has_errors() {
                return Err(format!(
                    "Biome parse failed (module) with {} diagnostics",
                    parse.diagnostics().len()
                )
                .into());
            }
            (parse.syntax(), file_source)
        }
        SourceType::Script => {
            let file_source = JsFileSource::js_script();
            let parse = parse(source, file_source, JsParserOptions::default());
            if parse.has_errors() {
                return Err(format!(
                    "Biome parse failed (script) with {} diagnostics",
                    parse.diagnostics().len()
                )
                .into());
            }
            (parse.syntax(), file_source)
        }
        SourceType::Both => {
            let module_source = JsFileSource::js_module();
            let module_parse = parse(source, module_source, JsParserOptions::default());
            if !module_parse.has_errors() {
                (module_parse.syntax(), module_source)
            } else {
                let script_source = JsFileSource::js_script();
                let script_parse = parse(source, script_source, JsParserOptions::default());
                if script_parse.has_errors() {
                    return Err(format!(
                        "Biome parse failed (module+script) with {} diagnostics",
                        module_parse
                            .diagnostics()
                            .len()
                            .saturating_add(script_parse.diagnostics().len())
                    )
                    .into());
                }
                (script_parse.syntax(), script_source)
            }
        }
    };

    let formatted = format_node(
        JsFormatOptions::new(file_source)
            .with_indent_style(IndentStyle::Space)
            .with_indent_width(IndentWidth::default()),
        &syntax,
    )?;
    let printed = formatted.print()?;
    Ok(printed.as_code().to_string())
}
