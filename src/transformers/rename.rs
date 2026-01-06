//! Rename transformer
//!
//! Renames variables to more readable names.
//! Uses scope tracking to properly handle variable shadowing.

use std::collections::{HashMap, HashSet};
use swc_ecma_ast::*;
use swc_ecma_visit::{VisitMut, VisitMutWith};

use crate::context::Context;
use crate::error::Result;
use crate::transformers::Transformer;
use crate::words::{MersenneTwister, generate_random_words};

/// Rename transformer.
///
/// Renames obfuscated variable names to more readable ones and respects scopes.
#[derive(Debug)]
pub struct Rename;

impl Rename {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Check if a name looks obfuscated (short hex-like names)
    #[must_use]
    fn is_obfuscated_name(name: &str) -> bool {
        // Match patterns like _0x1234, _0xabc, etc.
        if name.starts_with("_0x") && name.len() <= 10 {
            return true;
        }

        // Match single letter + numbers like a1, b2, etc.
        if name.len() <= 3
            && name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && name.len() > 1
            && name[1..].chars().all(|c| c.is_ascii_digit())
        {
            return true;
        }

        false
    }

    /// Names that should never be renamed (builtins, globals)
    #[must_use]
    fn is_builtin_name(name: &str) -> bool {
        matches!(
            name,
            "console"
                | "window"
                | "document"
                | "global"
                | "globalThis"
                | "Object"
                | "Array"
                | "String"
                | "Number"
                | "Boolean"
                | "Function"
                | "Symbol"
                | "BigInt"
                | "Math"
                | "Date"
                | "RegExp"
                | "Error"
                | "TypeError"
                | "RangeError"
                | "SyntaxError"
                | "ReferenceError"
                | "Promise"
                | "Map"
                | "Set"
                | "WeakMap"
                | "WeakSet"
                | "JSON"
                | "Reflect"
                | "Proxy"
                | "Intl"
                | "parseInt"
                | "parseFloat"
                | "isNaN"
                | "isFinite"
                | "encodeURI"
                | "decodeURI"
                | "encodeURIComponent"
                | "decodeURIComponent"
                | "eval"
                | "setTimeout"
                | "setInterval"
                | "clearTimeout"
                | "clearInterval"
                | "require"
                | "module"
                | "exports"
                | "__dirname"
                | "__filename"
                | "undefined"
                | "NaN"
                | "Infinity"
                | "arguments"
                | "this"
        )
    }
}

impl Default for Rename {
    fn default() -> Self {
        Self::new()
    }
}

impl Transformer for Rename {
    fn name(&self) -> &'static str {
        "Rename"
    }

    fn transform(&self, context: &mut Context) -> Result<()> {
        // Use scope-aware renamer
        let mut renamer = ScopeAwareRenamer::new(context.hash);
        context.ast.visit_mut_with(&mut renamer);
        Ok(())
    }
}

/// Scope-aware renaming with proper scope tracking
struct ScopeAwareRenamer {
    /// Stack of scopes (each scope has its own name mappings)
    scope_stack: Vec<Scope>,
    /// Mersenne Twister for deterministic random name generation
    mt: MersenneTwister,
}

/// A scope containing variable bindings
#[derive(Default)]
struct Scope {
    /// Maps old names to new names within this scope
    bindings: HashMap<String, String>,
    /// Set of names declared in this scope
    declared: HashSet<String>,
}

impl ScopeAwareRenamer {
    #[must_use]
    fn new(seed: u32) -> Self {
        Self {
            scope_stack: vec![Scope::default()], // Global scope
            mt: MersenneTwister::new(seed),
        }
    }

    fn push_scope(&mut self) {
        self.scope_stack.push(Scope::default());
    }

    fn pop_scope(&mut self) {
        if self.scope_stack.len() > 1 {
            self.scope_stack.pop();
        }
    }

    fn current_scope(&mut self) -> &mut Scope {
        self.scope_stack
            .last_mut()
            .expect("scope stack should have at least one scope")
    }

    fn generate_name(&mut self, prefix: &str) -> String {
        let words = generate_random_words(&mut self.mt, 2);
        format!("{}{}", prefix, words.join(""))
    }

    fn generate_var_name(&mut self) -> String {
        self.generate_name("var")
    }

    fn generate_func_name(&mut self) -> String {
        self.generate_name("func")
    }

    fn generate_param_name(&mut self) -> String {
        self.generate_name("arg")
    }

    fn rename_array_pat(&mut self, pat: &mut ArrayPat) {
        for elem in pat.elems.iter_mut() {
            let Some(elem) = elem else { continue };
            self.rename_array_pat_elem(elem);
        }
    }

    fn rename_array_pat_elem(&mut self, pat: &mut Pat) {
        match pat {
            Pat::Ident(binding) => {
                let old_name = binding.id.sym.to_string();
                let new_name = self.declare_name(&old_name, NameKind::Variable);
                binding.id.sym = new_name.into();
            }
            Pat::Array(array) => self.rename_array_pat(array),
            Pat::Assign(assign) => self.rename_array_pat_elem(&mut assign.left),
            Pat::Rest(rest) => self.rename_array_pat_elem(&mut rest.arg),
            _ => {}
        }
    }

    /// Declare a name in the current scope and get its new name
    fn declare_name(&mut self, old_name: &str, kind: NameKind) -> String {
        // Skip builtins
        if Rename::is_builtin_name(old_name) {
            return old_name.to_string();
        }

        // Skip non-obfuscated names
        if !Rename::is_obfuscated_name(old_name) {
            return old_name.to_string();
        }

        if let Some(existing) = self.current_scope().bindings.get(old_name) {
            return existing.clone();
        }

        let new_name = match kind {
            NameKind::Variable => self.generate_var_name(),
            NameKind::Function => self.generate_func_name(),
            NameKind::Parameter => self.generate_param_name(),
        };

        let scope = self.current_scope();
        scope
            .bindings
            .insert(old_name.to_string(), new_name.clone());
        scope.declared.insert(old_name.to_string());
        new_name
    }

    fn predeclare_functions_in_stmts(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            if let Stmt::Decl(Decl::Fn(fn_decl)) = stmt {
                let old_name = fn_decl.ident.sym.to_string();
                self.declare_name(&old_name, NameKind::Function);
            }
        }
    }

    fn predeclare_functions_in_module_items(&mut self, items: &[ModuleItem]) {
        for item in items {
            if let ModuleItem::Stmt(Stmt::Decl(Decl::Fn(fn_decl))) = item {
                let old_name = fn_decl.ident.sym.to_string();
                self.declare_name(&old_name, NameKind::Function);
            }
        }
    }

    /// Look up a name in all scopes (from innermost to outermost)
    #[must_use]
    fn lookup_name(&self, name: &str) -> Option<String> {
        // Skip builtins - they should never be renamed
        if Rename::is_builtin_name(name) {
            return None;
        }

        // Search from innermost to outermost scope
        for scope in self.scope_stack.iter().rev() {
            if let Some(new_name) = scope.bindings.get(name) {
                return Some(new_name.clone());
            }
        }
        None
    }
}

#[derive(Debug, Clone, Copy)]
enum NameKind {
    Variable,
    Function,
    Parameter,
}

impl VisitMut for ScopeAwareRenamer {
    fn visit_mut_script(&mut self, script: &mut Script) {
        self.predeclare_functions_in_stmts(&script.body);
        script.visit_mut_children_with(self);
    }

    fn visit_mut_module(&mut self, module: &mut Module) {
        self.predeclare_functions_in_module_items(&module.body);
        module.visit_mut_children_with(self);
    }

    fn visit_mut_function(&mut self, func: &mut Function) {
        // Create new scope for function body
        self.push_scope();

        // Rename parameters
        for param in func.params.iter_mut() {
            if let Pat::Ident(binding) = &mut param.pat {
                let old_name = binding.id.sym.to_string();
                let new_name = self.declare_name(&old_name, NameKind::Parameter);
                binding.id.sym = new_name.into();
            } else if let Pat::Array(array) = &mut param.pat {
                self.rename_array_pat(array);
            } else if let Pat::Assign(assign) = &mut param.pat {
                self.rename_array_pat_elem(&mut assign.left);
            } else if let Pat::Rest(rest) = &mut param.pat {
                self.rename_array_pat_elem(&mut rest.arg);
            }
        }

        // Visit function body
        if let Some(body) = &mut func.body {
            body.visit_mut_with(self);
        }

        // Pop scope after function
        self.pop_scope();
    }

    fn visit_mut_fn_decl(&mut self, decl: &mut FnDecl) {
        // Declare function name in current scope
        let old_name = decl.ident.sym.to_string();
        let new_name = self.declare_name(&old_name, NameKind::Function);
        decl.ident.sym = new_name.into();

        // Visit function (this creates a new scope)
        self.visit_mut_function(&mut decl.function);
    }

    fn visit_mut_var_declarator(&mut self, decl: &mut VarDeclarator) {
        // First visit init expression (before declaring the variable)
        if let Some(init) = &mut decl.init {
            init.visit_mut_with(self);
        }

        // Then declare the variable
        if let Pat::Ident(binding) = &mut decl.name {
            let old_name = binding.id.sym.to_string();
            let new_name = self.declare_name(&old_name, NameKind::Variable);
            binding.id.sym = new_name.into();
        } else if let Pat::Array(array) = &mut decl.name {
            self.rename_array_pat(array);
        } else if let Pat::Assign(assign) = &mut decl.name {
            self.rename_array_pat_elem(&mut assign.left);
        } else if let Pat::Rest(rest) = &mut decl.name {
            self.rename_array_pat_elem(&mut rest.arg);
        }
    }

    fn visit_mut_block_stmt(&mut self, block: &mut BlockStmt) {
        // Create new scope for block
        self.push_scope();
        self.predeclare_functions_in_stmts(&block.stmts);
        block.visit_mut_children_with(self);
        self.pop_scope();
    }

    fn visit_mut_for_stmt(&mut self, for_stmt: &mut ForStmt) {
        // Create new scope for for loop
        self.push_scope();
        for_stmt.visit_mut_children_with(self);
        self.pop_scope();
    }

    fn visit_mut_for_in_stmt(&mut self, for_in: &mut ForInStmt) {
        self.push_scope();
        for_in.visit_mut_children_with(self);
        self.pop_scope();
    }

    fn visit_mut_for_of_stmt(&mut self, for_of: &mut ForOfStmt) {
        self.push_scope();
        for_of.visit_mut_children_with(self);
        self.pop_scope();
    }

    fn visit_mut_catch_clause(&mut self, catch: &mut CatchClause) {
        self.push_scope();

        // Rename catch parameter
        if let Some(param) = &mut catch.param
            && let Pat::Ident(binding) = param
        {
            let old_name = binding.id.sym.to_string();
            let new_name = self.declare_name(&old_name, NameKind::Variable);
            binding.id.sym = new_name.into();
        }

        catch.body.visit_mut_with(self);
        self.pop_scope();
    }

    fn visit_mut_arrow_expr(&mut self, arrow: &mut ArrowExpr) {
        self.push_scope();

        // Rename parameters
        for param in arrow.params.iter_mut() {
            if let Pat::Ident(binding) = param {
                let old_name = binding.id.sym.to_string();
                let new_name = self.declare_name(&old_name, NameKind::Parameter);
                binding.id.sym = new_name.into();
            } else if let Pat::Array(array) = param {
                self.rename_array_pat(array);
            } else if let Pat::Assign(assign) = param {
                self.rename_array_pat_elem(&mut assign.left);
            } else if let Pat::Rest(rest) = param {
                self.rename_array_pat_elem(&mut rest.arg);
            }
        }

        // Visit body
        arrow.body.visit_mut_with(self);

        self.pop_scope();
    }

    fn visit_mut_ident(&mut self, ident: &mut Ident) {
        let name = ident.sym.to_string();
        if let Some(new_name) = self.lookup_name(&name) {
            ident.sym = new_name.into();
        }
    }

    // Prevent renaming property access names
    fn visit_mut_member_prop(&mut self, prop: &mut MemberProp) {
        // Don't rename computed property names - they should be visited
        if let MemberProp::Computed(computed) = prop {
            computed.visit_mut_with(self);
        }
        // For Ident props, don't visit (don't rename property names)
    }

    // Prevent renaming object property keys
    fn visit_mut_prop_name(&mut self, _prop: &mut PropName) {
        // Don't rename property keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Deobfuscator;

    #[test]
    fn test_rename_new() {
        let transformer = Rename::new();
        assert_eq!(transformer.name(), "Rename");
    }

    #[test]
    fn test_is_obfuscated_name() {
        // Hex-style names
        assert!(Rename::is_obfuscated_name("_0x123"));
        assert!(Rename::is_obfuscated_name("_0xabc"));
        assert!(Rename::is_obfuscated_name("_0xABCDEF"));

        // Short letter+number names
        assert!(Rename::is_obfuscated_name("a1"));
        assert!(Rename::is_obfuscated_name("b2"));
        assert!(Rename::is_obfuscated_name("x99"));

        // Normal names should NOT be obfuscated
        assert!(!Rename::is_obfuscated_name("console"));
        assert!(!Rename::is_obfuscated_name("myVariable"));
        assert!(!Rename::is_obfuscated_name("document"));
        assert!(!Rename::is_obfuscated_name("window"));
        assert!(!Rename::is_obfuscated_name("Array"));

        // Edge cases
        assert!(!Rename::is_obfuscated_name(""));
        assert!(!Rename::is_obfuscated_name("a")); // single letter
        assert!(!Rename::is_obfuscated_name("abc123")); // too long
    }

    #[test]
    fn test_rename_variables() {
        let deob = Deobfuscator::new();
        let code = r#"
function _0x123() {
    var _0xabc = 1;
    return _0xabc;
}
_0x123();
"#;
        let options = crate::DeobfuscateOptions {
            source_type: crate::SourceType::Script,
            rename: true,
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(!result.contains("_0x123"));
        assert!(!result.contains("_0xabc"));
        assert!(!result.contains("_0x"));
    }

    #[test]
    fn test_rename_function_parameters() {
        let deob = Deobfuscator::new();
        let code = r#"
function _0x123(a1, b2) {
    return a1 + b2;
}
_0x123(1, 2);
"#;
        let options = crate::DeobfuscateOptions {
            source_type: crate::SourceType::Script,
            rename: true,
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(!result.contains("_0x123"));
        assert!(!result.contains("a1"));
        assert!(!result.contains("b2"));
    }

    #[test]
    fn test_rename_preserves_normal_names() {
        let deob = Deobfuscator::new();
        let code = r#"
function normalFunc() {
    var normalVar = 1;
    console.log(normalVar);
}
normalFunc();
"#;
        let options = crate::DeobfuscateOptions {
            source_type: crate::SourceType::Script,
            rename: true,
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        // Normal names should be preserved
        assert!(result.contains("normalFunc"));
        assert!(result.contains("normalVar"));
        assert!(result.contains("console"));
    }

    #[test]
    fn test_rename_multiple_functions() {
        let deob = Deobfuscator::new();
        let code = r#"
function _0x111() { return 1; }
function _0x222() { return 2; }
_0x111();
_0x222();
"#;
        let options = crate::DeobfuscateOptions {
            source_type: crate::SourceType::Script,
            rename: true,
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(!result.contains("_0x111"));
        assert!(!result.contains("_0x222"));
        assert!(!result.contains("_0x"));
    }

    #[test]
    fn test_rename_references_updated() {
        let deob = Deobfuscator::new();
        let code = r#"
function _0x123() {
    var _0xabc = 5;
    return _0xabc;
}
_0x123();
"#;
        let options = crate::DeobfuscateOptions {
            source_type: crate::SourceType::Script,
            rename: true,
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(!result.contains("_0x123"));
        assert!(!result.contains("_0xabc"));
        assert!(!result.contains("_0x"));
    }

    #[test]
    fn test_simple_renamer_generation() {
        let mut renamer = ScopeAwareRenamer::new(12345);

        // Now generates word-based names
        let var_name = renamer.generate_var_name();
        assert!(var_name.starts_with("var"));
        assert!(var_name.len() > 3); // Should have words appended

        let func_name = renamer.generate_func_name();
        assert!(func_name.starts_with("func"));

        let param_name = renamer.generate_param_name();
        assert!(param_name.starts_with("arg"));
    }

    #[test]
    fn test_is_builtin_name() {
        assert!(Rename::is_builtin_name("console"));
        assert!(Rename::is_builtin_name("window"));
        assert!(Rename::is_builtin_name("document"));
        assert!(Rename::is_builtin_name("Math"));
        assert!(Rename::is_builtin_name("parseInt"));
        assert!(!Rename::is_builtin_name("_0x123"));
        assert!(!Rename::is_builtin_name("myVar"));
    }
}
