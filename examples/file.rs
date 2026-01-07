//! Deobfuscate a file
//!
//! Run with: cargo run --example file <filepath>

#![expect(
    clippy::print_stdout,
    reason = "examples intentionally print to stdout"
)]
#![expect(
    clippy::print_stderr,
    reason = "examples intentionally print to stderr"
)]

use std::env;
use std::fs;
use std::process;
use synchrony_rs::Deobfuscator;

fn main() {
    let args: Vec<String> = env::args().collect();

    let Some(file_path) = args.get(1) else {
        eprintln!("Usage: cargo run --example file <filepath>");
        eprintln!("Example: cargo run --example file obfuscated.js");
        process::exit(1);
    };

    // Read the file
    let source = match fs::read_to_string(file_path) {
        Ok(content) => content,
        Err(e) => {
            eprintln!("Error reading file: {e}");
            process::exit(1);
        }
    };

    println!("=== Input file: {file_path} ===");
    println!("Size: {size} bytes", size = source.len());
    println!();

    // Deobfuscate
    let deobfuscator = Deobfuscator::new();

    match deobfuscator.deobfuscate_source(&source, None) {
        Ok(result) => {
            println!("=== Output ===");
            println!("{result}");
        }
        Err(e) => {
            eprintln!("Deobfuscation error: {e}");
            process::exit(1);
        }
    }
}
