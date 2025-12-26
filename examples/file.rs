//! Deobfuscate a file
//!
//! Run with: cargo run --example file <filepath>

use std::env;
use std::fs;
use synchrony_rs::Deobfuscator;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: cargo run --example file <filepath>");
        eprintln!("Example: cargo run --example file obfuscated.js");
        std::process::exit(1);
    }

    let file_path = &args[1];

    // Read the file
    let source = match fs::read_to_string(file_path) {
        Ok(content) => content,
        Err(e) => {
            eprintln!("Error reading file: {}", e);
            std::process::exit(1);
        }
    };

    println!("=== Input file: {} ===", file_path);
    println!("Size: {} bytes", source.len());
    println!();

    // Deobfuscate
    let deobfuscator = Deobfuscator::new();

    match deobfuscator.deobfuscate_source(&source, None) {
        Ok(result) => {
            println!("=== Output ===");
            println!("{}", result);
        }
        Err(e) => {
            eprintln!("Deobfuscation error: {}", e);
            std::process::exit(1);
        }
    }
}
