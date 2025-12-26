//! Simple deobfuscation examples
//!
//! Run with: cargo run --example simple

use synchrony_rs::{DeobfuscateOptions, Deobfuscator, SourceType};

fn main() {
    let deobfuscator = Deobfuscator::new();

    // Example 1: Constant folding
    println!("=== Example 1: Constant Folding ===");
    let code = "const x = 1 + 2 + 3;";
    println!("Input: {}", code);
    match deobfuscator.deobfuscate_source(code, None) {
        Ok(result) => println!("Output: {}", result),
        Err(e) => println!("Error: {}", e),
    }

    // Example 2: Boolean simplification (!0 -> true, !1 -> false)
    println!("\n=== Example 2: Boolean Simplification ===");
    let code = "const a = !0; const b = !1;";
    println!("Input: {}", code);
    match deobfuscator.deobfuscate_source(code, None) {
        Ok(result) => println!("Output: {}", result),
        Err(e) => println!("Error: {}", e),
    }

    // Example 3: String concatenation
    println!("\n=== Example 3: String Concatenation ===");
    let code = r#"const msg = "Hello, " + "World!";"#;
    println!("Input: {}", code);
    match deobfuscator.deobfuscate_source(code, None) {
        Ok(result) => println!("Output: {}", result),
        Err(e) => println!("Error: {}", e),
    }

    // Example 4: Dead code elimination
    println!("\n=== Example 4: Dead Code Elimination ===");
    let code = r#"
function test() {
    if (false) {
        console.log("dead code");
    }
    if (true) {
        console.log("alive");
    }
}
"#;
    println!("Input: {}", code.trim());
    match deobfuscator.deobfuscate_source(code, None) {
        Ok(result) => println!("Output:\n{}", result),
        Err(e) => println!("Error: {}", e),
    }

    // Example 5: Conditional expression evaluation
    println!("\n=== Example 5: Conditional Expression Evaluation ===");
    let code = "const result = 5 > 3 ? 'yes' : 'no';";
    println!("Input: {}", code);
    match deobfuscator.deobfuscate_source(code, None) {
        Ok(result) => println!("Output: {}", result),
        Err(e) => println!("Error: {}", e),
    }

    // Example 6: MemberExpression cleanup
    println!("\n=== Example 6: MemberExpression Cleanup ===");
    let code = r#"console["log"]("hello"); obj["property"] = 1;"#;
    println!("Input: {}", code);
    match deobfuscator.deobfuscate_source(code, None) {
        Ok(result) => println!("Output: {}", result),
        Err(e) => println!("Error: {}", e),
    }

    // Example 7: Sequence expression splitting
    println!("\n=== Example 7: Sequence Expression Splitting ===");
    let code = "a = 1, b = 2, c = 3;";
    println!("Input: {}", code);
    match deobfuscator.deobfuscate_source(code, None) {
        Ok(result) => println!("Output: {}", result),
        Err(e) => println!("Error: {}", e),
    }

    // Example 8: Literal map expansion
    println!("\n=== Example 8: Literal Map Expansion ===");
    let code = r#"
const map = { a: 1, b: "hello", c: true };
console.log(map.a, map["b"], map.c);
"#;
    println!("Input: {}", code.trim());
    match deobfuscator.deobfuscate_source(code, None) {
        Ok(result) => println!("Output:\n{}", result),
        Err(e) => println!("Error: {}", e),
    }

    // Example 9: Control flow storage function inlining
    println!("\n=== Example 9: Control Flow Storage Function Inlining ===");
    let code = r#"
var _0xabc = {
    "ABcDe": function(a, b) { return a + b; },
    "FGhIj": "hello"
};
console.log(_0xabc.ABcDe(1, 2), _0xabc["FGhIj"]);
"#;
    println!("Input: {}", code.trim());
    match deobfuscator.deobfuscate_source(code, None) {
        Ok(result) => println!("Output:\n{}", result),
        Err(e) => println!("Error: {}", e),
    }

    // Example 10: Logical expression to if statement
    println!("\n=== Example 10: Logical Expression to If Statement ===");
    let code = "x == 1 && (a(), b(), c());";
    println!("Input: {}", code);
    match deobfuscator.deobfuscate_source(code, None) {
        Ok(result) => println!("Output: {}", result),
        Err(e) => println!("Error: {}", e),
    }

    // Example 11: IIFE simplification
    println!("\n=== Example 11: IIFE Simplification ===");
    let code = "(function() { return 42; })();";
    println!("Input: {}", code);
    match deobfuscator.deobfuscate_source(code, None) {
        Ok(result) => println!("Output: {}", result),
        Err(e) => println!("Error: {}", e),
    }

    // Example 12: Verbose mode
    // Example 12: Logging via RUST_LOG
    // Set RUST_LOG=info or RUST_LOG=debug to see transformer logs
    println!("\n=== Example 12: Logging Mode (use RUST_LOG=info) ===");
    let code = "var x = 1 + 2;";
    println!("Input: {}", code);
    let options = DeobfuscateOptions {
        source_type: SourceType::Script,
        rename: false,
        ..Default::default()
    };
    match deobfuscator.deobfuscate_source(code, Some(options)) {
        Ok(result) => println!("Output: {}", result),
        Err(e) => println!("Error: {}", e),
    }
}
