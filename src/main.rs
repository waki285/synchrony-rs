//! Synchrony - JavaScript Deobfuscator
//!
//! A fast JavaScript deobfuscator written in Rust.

#![cfg(feature = "cli")]

use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use serde::Deserialize;
use serde_json::Value;
use swc_ecma_ast::EsVersion;
use synchrony_rs::deobfuscator::{DeobfuscateOptions, Deobfuscator, SourceType};
#[cfg(feature = "format")]
use synchrony_rs::format::format_js;
use synchrony_rs::options::{map_es_version_num, parse_es_version_str, parse_source_type_str};
use synchrony_rs::transformers::TransformerConfig;
#[cfg(feature = "tracing")]
use tracing_subscriber::{EnvFilter, fmt};

/// Synchrony - A fast JavaScript deobfuscator
#[derive(Parser)]
#[command(name = "synchrony")]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// File to deobfuscate (shorthand for `synchrony deobfuscate <file>`)
    #[arg(value_name = "FILE")]
    file: Option<PathBuf>,

    /// Rename obfuscated identifiers to readable names
    #[arg(short, long, default_value_t = false)]
    rename: bool,

    /// Format output using the Biome formatter
    #[arg(long, default_value_t = false)]
    format: bool,

    /// Output file path (defaults to <input>.cleaned.js)
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Load options from a JSON config file
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// ECMAScript version (e.g. es2020, 2020, latest)
    #[arg(long, value_name = "VERSION")]
    ecma_version: Option<String>,

    /// Enable loose parsing (not implemented yet)
    #[arg(long, default_value_t = false)]
    loose: bool,

    /// Source type for parsing
    #[arg(short = 't', long, value_enum, default_value_t = SourceTypeArg::Both)]
    source_type: SourceTypeArg,

    /// Print verbose output
    #[arg(short, long, default_value_t = false)]
    verbose: bool,

    /// Evaluate JavaScript expression directly (for testing)
    #[arg(short, long, value_name = "CODE")]
    eval: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Deobfuscate a JavaScript file
    Deobfuscate {
        /// File to deobfuscate
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// Rename obfuscated identifiers to readable names
        #[arg(short, long, default_value_t = false)]
        rename: bool,

        /// Format output using the Biome formatter
        #[arg(long, default_value_t = false)]
        format: bool,

        /// Output file path (defaults to <input>.cleaned.js)
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,

        /// Load options from a JSON config file
        #[arg(long, value_name = "FILE")]
        config: Option<PathBuf>,

        /// ECMAScript version (e.g. es2020, 2020, latest)
        #[arg(long, value_name = "VERSION")]
        ecma_version: Option<String>,

        /// Enable loose parsing (not implemented yet)
        #[arg(long, default_value_t = false)]
        loose: bool,

        /// Source type for parsing
        #[arg(short = 't', long, value_enum, default_value_t = SourceTypeArg::Both)]
        source_type: SourceTypeArg,

        /// Print verbose output
        #[arg(short, long, default_value_t = false)]
        verbose: bool,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, ValueEnum)]
enum SourceTypeArg {
    /// Parse as ES module
    Module,
    /// Parse as script
    Script,
    /// Try module first, then script (default)
    Both,
}

impl From<SourceTypeArg> for SourceType {
    fn from(arg: SourceTypeArg) -> Self {
        match arg {
            SourceTypeArg::Module => Self::Module,
            SourceTypeArg::Script => Self::Script,
            SourceTypeArg::Both => Self::Both,
        }
    }
}

fn build_options_from_cli(
    source_type: SourceTypeArg,
    rename: bool,
    ecma_version: Option<&str>,
    loose: bool,
) -> Result<DeobfuscateOptions, Box<dyn std::error::Error>> {
    let ecma_version = match ecma_version {
        Some(value) => Some(parse_es_version_str(value)?),
        None => None,
    };

    Ok(DeobfuscateOptions {
        source_type: source_type.into(),
        rename,
        ecma_version,
        loose,
        ..Default::default()
    })
}

fn apply_config_if_present(
    config: Option<&Path>,
    options: &mut DeobfuscateOptions,
    output: &mut Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(path) = config {
        return apply_config(path, options, output);
    }
    Ok(())
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ConfigFile {
    #[serde(alias = "ecma_version")]
    ecma_version: Option<ConfigEsVersion>,
    transform_chain_expressions: Option<bool>,
    #[serde(alias = "custom_transformers")]
    custom_transformers: Option<Vec<ConfigTransformerEntry>>,
    rename: Option<bool>,
    #[serde(alias = "source_type")]
    source_type: Option<String>,
    loose: Option<bool>,
    output: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ConfigEsVersion {
    Number(u32),
    Text(String),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ConfigTransformerEntry {
    Pair(String, Value),
    NameOnly(String),
    Object {
        name: String,
        #[serde(default)]
        options: Value,
    },
}

fn config_to_transformers(entries: Vec<ConfigTransformerEntry>) -> Vec<TransformerConfig> {
    entries
        .into_iter()
        .map(|entry| match entry {
            ConfigTransformerEntry::Pair(name, options) => TransformerConfig { name, options },
            ConfigTransformerEntry::NameOnly(name) => TransformerConfig {
                name,
                options: Value::Null,
            },
            ConfigTransformerEntry::Object { name, options } => TransformerConfig { name, options },
        })
        .collect()
}

fn parse_es_version_config(value: ConfigEsVersion) -> Result<EsVersion, String> {
    match value {
        ConfigEsVersion::Number(num) => {
            map_es_version_num(num).ok_or_else(|| format!("Unknown ECMAScript version: {}", num))
        }
        ConfigEsVersion::Text(text) => parse_es_version_str(&text),
    }
}

fn apply_config(
    path: &Path,
    options: &mut DeobfuscateOptions,
    output: &mut Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let json = fs::read_to_string(path)?;
    let config: ConfigFile = serde_json::from_str(&json)?;
    eprintln!("Loaded config from \"{}\"", path.display());

    if let Some(rename) = config.rename {
        options.rename = rename;
    }
    if let Some(source_type) = config.source_type {
        options.source_type = parse_source_type_str(&source_type)?;
    }
    if let Some(ecma) = config.ecma_version {
        options.ecma_version = Some(parse_es_version_config(ecma)?);
    }
    if let Some(loose) = config.loose {
        options.loose = loose;
        eprintln!("loose is currently ignored by the Rust implementation.");
    }
    if let Some(flag) = config.transform_chain_expressions {
        options.transform_chain_expressions = flag;
        eprintln!("transformChainExpressions is currently ignored by the Rust implementation.");
    }
    if let Some(entries) = config.custom_transformers {
        options.custom_transformer_configs = Some(config_to_transformers(entries));
    }
    if let Some(out) = config.output {
        *output = Some(PathBuf::from(out));
    }

    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Initialize tracing subscriber (only when tracing feature is enabled)
    #[cfg(feature = "tracing")]
    {
        // --verbose flag takes precedence, otherwise use RUST_LOG env var (default: warn)
        let filter = if cli.verbose {
            EnvFilter::new("debug")
        } else if std::env::var("RUST_LOG").is_ok() {
            EnvFilter::from_default_env()
        } else {
            EnvFilter::new("warn")
        };

        let subscriber = fmt::Subscriber::builder()
            .with_env_filter(filter)
            .with_target(false)
            .with_thread_ids(false)
            .without_time()
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    }

    // Handle --eval flag for quick testing
    if let Some(code) = cli.eval {
        let mut options = build_options_from_cli(
            cli.source_type,
            cli.rename,
            cli.ecma_version.as_deref(),
            cli.loose,
        )?;
        let mut output = cli.output.clone();
        apply_config_if_present(cli.config.as_deref(), &mut options, &mut output)?;
        return deobfuscate_code(&code, options, cli.format);
    }

    // Determine which mode we're in
    match cli.command {
        Some(Commands::Deobfuscate {
            file,
            rename,
            format,
            output,
            config,
            ecma_version,
            loose,
            source_type,
            verbose: _,
        }) => {
            let mut options =
                build_options_from_cli(source_type, rename, ecma_version.as_deref(), loose)?;
            let mut output = output;
            apply_config_if_present(config.as_deref(), &mut options, &mut output)?;
            deobfuscate_file(&file, output, options, format)?;
        }
        None => {
            // Check if file argument was provided directly
            if let Some(ref file) = cli.file {
                // Check for "-" which means stdin
                if file.as_os_str() == "-" {
                    // Read from stdin
                    let mut source = String::new();
                    io::stdin().read_to_string(&mut source)?;

                    let mut options = build_options_from_cli(
                        cli.source_type,
                        cli.rename,
                        cli.ecma_version.as_deref(),
                        cli.loose,
                    )?;
                    let mut output = cli.output.clone();
                    apply_config_if_present(cli.config.as_deref(), &mut options, &mut output)?;
                    let result = deobfuscate_source(&source, options, cli.format)?;

                    // Output to stdout or file
                    if let Some(output) = output {
                        fs::write(&output, &result)?;
                        eprintln!("Deobfuscated code written to {}", output.display());
                    } else {
                        io::stdout().write_all(result.as_bytes())?;
                    }
                } else {
                    let mut options = build_options_from_cli(
                        cli.source_type,
                        cli.rename,
                        cli.ecma_version.as_deref(),
                        cli.loose,
                    )?;
                    let mut output = cli.output.clone();
                    apply_config_if_present(cli.config.as_deref(), &mut options, &mut output)?;
                    deobfuscate_file(file, output, options, cli.format)?;
                }
            } else {
                // Check if stdin has data
                if !std::io::stdin().is_terminal() {
                    // Read from stdin
                    let mut source = String::new();
                    io::stdin().read_to_string(&mut source)?;

                    let mut options = build_options_from_cli(
                        cli.source_type,
                        cli.rename,
                        cli.ecma_version.as_deref(),
                        cli.loose,
                    )?;
                    let mut output = cli.output.clone();
                    apply_config_if_present(cli.config.as_deref(), &mut options, &mut output)?;
                    let result = deobfuscate_source(&source, options, cli.format)?;

                    // Output to stdout or file
                    if let Some(output) = output {
                        fs::write(&output, &result)?;
                        eprintln!("Deobfuscated code written to {}", output.display());
                    } else {
                        io::stdout().write_all(result.as_bytes())?;
                    }
                } else {
                    // No input provided, show help
                    eprintln!("Usage: synchrony [OPTIONS] <FILE>");
                    eprintln!("       synchrony deobfuscate [OPTIONS] <FILE>");
                    eprintln!("       synchrony --eval <CODE>");
                    eprintln!("       cat file.js | synchrony > output.js");
                    eprintln!();
                    eprintln!("For more information, try '--help'.");
                    std::process::exit(1);
                }
            }
        }
    }

    Ok(())
}

fn deobfuscate_code(
    code: &str,
    options: DeobfuscateOptions,
    format: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let result = deobfuscate_source(code, options, format)?;
    println!("{}", result);
    Ok(())
}

fn deobfuscate_file(
    input: &Path,
    output: Option<PathBuf>,
    options: DeobfuscateOptions,
    format: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Check if file exists
    if !input.exists() {
        return Err(format!("File not found: {}", input.display()).into());
    }

    // Read input file
    let source = fs::read_to_string(input)?;

    eprintln!("Reading from: {}", input.display());
    eprintln!("Source size: {} bytes", source.len());

    // Deobfuscate
    let result = deobfuscate_source(&source, options, format)?;

    // Determine output path
    let output_path = output.unwrap_or_else(|| {
        let stem = input.file_stem().unwrap_or_default().to_string_lossy();
        let ext = input.extension().unwrap_or_default().to_string_lossy();
        let parent = input.parent().unwrap_or_else(|| Path::new("."));

        if ext.is_empty() {
            parent.join(format!("{}.cleaned", stem))
        } else {
            parent.join(format!("{}.cleaned.{}", stem, ext))
        }
    });

    // Write output
    fs::write(&output_path, &result)?;

    eprintln!("Deobfuscated code written to {}", output_path.display());

    Ok(())
}

fn deobfuscate_source(
    source: &str,
    options: DeobfuscateOptions,
    format: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let deobfuscator = Deobfuscator::new();
    let source_type = options.source_type;

    let result = deobfuscator
        .deobfuscate_source(source, Some(options))
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    if format {
        #[cfg(feature = "format")]
        {
            return format_js(&result, source_type);
        }
        #[cfg(not(feature = "format"))]
        {
            return Err("format feature is not enabled in this build".into());
        }
    }

    Ok(result)
}
