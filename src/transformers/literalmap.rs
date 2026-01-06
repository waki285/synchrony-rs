//! LiteralMap transformer
//!
//! Replaces references to object properties with their literal values
//! when the object contains only literal values.
//!
//! Example:
//! ```javascript
//! const map = { a: 1, b: "hello" };
//! console.log(map.a, map["b"]);
//! // becomes:
//! console.log(1, "hello");
//! ```

use std::collections::HashMap;
use swc_common::GLOBALS;
use swc_ecma_ast::*;
use swc_ecma_visit::{VisitMut, VisitMutWith};

use crate::context::Context;
use crate::error::Result;
use crate::scope::{Id, analyze};
use crate::transformers::Transformer;

/// LiteralMap transformer.
///
/// Inlines literal values from object literal maps used as lookup tables.
#[derive(Debug)]
pub struct LiteralMap;

impl LiteralMap {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for LiteralMap {
    fn default() -> Self {
        Self::new()
    }
}

impl Transformer for LiteralMap {
    fn name(&self) -> &'static str {
        "LiteralMap"
    }

    fn transform(&self, context: &mut Context) -> Result<()> {
        // First, do the demap transformation
        let mut visitor = LiteralMapVisitor::new();
        context.ast.visit_mut_with(&mut visitor);

        // Then, do the literals transformation (replace read-only variables)
        self.literals(context)?;

        Ok(())
    }
}

impl LiteralMap {
    /// Replace read-only variables with their literal values in functions
    /// This only applies to variables declared within functions, not global scope
    fn literals(&self, context: &mut Context) -> Result<()> {
        // Apply only within functions
        let mut func_visitor = FunctionLiteralsVisitor {
            remove_garbage: context.remove_garbage,
        };
        context.ast.visit_mut_with(&mut func_visitor);

        Ok(())
    }
}

/// Visitor that applies literals transformation only within functions
struct FunctionLiteralsVisitor {
    remove_garbage: bool,
}

impl VisitMut for FunctionLiteralsVisitor {
    fn visit_mut_function(&mut self, func: &mut Function) {
        // First visit children recursively
        func.visit_mut_children_with(self);

        // Then apply literals transformation to this function's body
        if let Some(body) = &mut func.body {
            self.process_function_body(body);
        }
    }

    fn visit_mut_arrow_expr(&mut self, arrow: &mut ArrowExpr) {
        // First visit children recursively
        arrow.visit_mut_children_with(self);

        // Apply to arrow function body if it's a block
        if let BlockStmtOrExpr::BlockStmt(block) = &mut *arrow.body {
            self.process_function_body(block);
        }
    }
}

impl FunctionLiteralsVisitor {
    fn process_function_body(&mut self, body: &mut BlockStmt) {
        // Create a temporary module to analyze the function body
        let temp_module = Module {
            span: Default::default(),
            body: body.stmts.iter().cloned().map(ModuleItem::Stmt).collect(),
            shebang: None,
        };

        // Analyze variable usage within this function
        let scope_data = GLOBALS.set(&Default::default(), || analyze(&temp_module));

        // Find read-only variables that are initialized with literals
        let mut read_only_literals: HashMap<Id, Expr> = HashMap::new();
        let mut vars_to_remove: Vec<Id> = Vec::new();

        // First pass: find literal variable declarations
        let mut finder = ReadOnlyLiteralFinder {
            scope_data: &scope_data,
            literals: &mut read_only_literals,
            vars_to_remove: &mut vars_to_remove,
            remove_garbage: self.remove_garbage,
        };
        body.visit_mut_with(&mut finder);

        // Second pass: replace references and remove declarations
        let mut replacer = LiteralReplacer {
            literals: &read_only_literals,
            vars_to_remove: &vars_to_remove,
            remove_garbage: self.remove_garbage,
        };
        body.visit_mut_with(&mut replacer);
    }
}

/// Represents a literal value that can be stored in a map
#[derive(Clone, Debug)]
enum LiteralValue {
    String(String),
    Number(f64),
    Bool(bool),
    Null,
}

impl LiteralValue {
    #[must_use]
    fn to_expr(&self) -> Expr {
        match self {
            LiteralValue::String(s) => Expr::Lit(Lit::Str(Str {
                span: Default::default(),
                value: s.as_str().into(),
                raw: None,
            })),
            LiteralValue::Number(n) => {
                if *n < 0.0 {
                    Expr::Unary(UnaryExpr {
                        span: Default::default(),
                        op: UnaryOp::Minus,
                        arg: Box::new(Expr::Lit(Lit::Num(Number {
                            span: Default::default(),
                            value: -n,
                            raw: None,
                        }))),
                    })
                } else {
                    Expr::Lit(Lit::Num(Number {
                        span: Default::default(),
                        value: *n,
                        raw: None,
                    }))
                }
            }
            LiteralValue::Bool(b) => Expr::Lit(Lit::Bool(Bool {
                span: Default::default(),
                value: *b,
            })),
            LiteralValue::Null => Expr::Lit(Lit::Null(Null {
                span: Default::default(),
            })),
        }
    }
}

struct LiteralMapVisitor {
    /// Map from variable name to property map
    maps: HashMap<String, HashMap<String, LiteralValue>>,
}

impl LiteralMapVisitor {
    #[must_use]
    fn new() -> Self {
        Self {
            maps: HashMap::new(),
        }
    }

    /// Extract literal value from an expression
    #[must_use]
    fn extract_literal(expr: &Expr) -> Option<LiteralValue> {
        match expr {
            Expr::Lit(lit) => match lit {
                Lit::Str(s) => s
                    .value
                    .as_str()
                    .map(|v| LiteralValue::String(v.to_string())),
                Lit::Num(n) => Some(LiteralValue::Number(n.value)),
                Lit::Bool(b) => Some(LiteralValue::Bool(b.value)),
                Lit::Null(_) => Some(LiteralValue::Null),
                _ => None,
            },
            Expr::Unary(UnaryExpr {
                op: UnaryOp::Minus,
                arg,
                ..
            }) => {
                if let Expr::Lit(Lit::Num(n)) = &**arg {
                    Some(LiteralValue::Number(-n.value))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Get key string from property key
    #[must_use]
    fn get_key_string(key: &PropName) -> Option<String> {
        match key {
            PropName::Ident(id) => Some(id.sym.to_string()),
            PropName::Str(s) => s.value.as_str().map(|v| v.to_string()),
            PropName::Num(n) => Some(n.value.to_string()),
            _ => None,
        }
    }

    /// Check if an object expression contains only literal properties
    #[must_use]
    fn is_literal_object(obj: &ObjectLit) -> bool {
        obj.props.iter().all(|prop| match prop {
            PropOrSpread::Prop(prop) => {
                if let Prop::KeyValue(kv) = &**prop {
                    Self::extract_literal(&kv.value).is_some()
                } else {
                    false
                }
            }
            PropOrSpread::Spread(_) => false,
        })
    }

    /// Extract all literal properties from an object
    #[must_use]
    fn extract_literal_map(obj: &ObjectLit) -> HashMap<String, LiteralValue> {
        let mut map = HashMap::new();

        for prop in &obj.props {
            if let PropOrSpread::Prop(prop) = prop
                && let Prop::KeyValue(kv) = &**prop
                && let (Some(key), Some(value)) = (
                    Self::get_key_string(&kv.key),
                    Self::extract_literal(&kv.value),
                )
            {
                map.insert(key, value);
            }
        }

        map
    }
}

impl VisitMut for LiteralMapVisitor {
    fn visit_mut_var_decl(&mut self, decl: &mut VarDecl) {
        // First collect literal object declarations so same-statement uses can be replaced.
        for declarator in &decl.decls {
            if let Pat::Ident(binding) = &declarator.name
                && let Some(init) = &declarator.init
                && let Expr::Object(obj) = &**init
                && !obj.props.is_empty()
                && Self::is_literal_object(obj)
            {
                let name = binding.id.sym.to_string();
                let map = Self::extract_literal_map(obj);
                self.maps.insert(name.clone(), map);
            }
        }

        // Then visit children so member expressions can be replaced.
        decl.visit_mut_children_with(self);
    }

    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        // First visit children
        expr.visit_mut_children_with(self);

        // Replace member expressions that reference our maps
        if let Expr::Member(member) = expr
            && let Expr::Ident(obj) = &*member.obj
        {
            let obj_name = obj.sym.to_string();

            if let Some(map) = self.maps.get(&obj_name) {
                let key = match &member.prop {
                    MemberProp::Ident(id) => Some(id.sym.to_string()),
                    MemberProp::Computed(computed) => {
                        if let Expr::Lit(Lit::Str(s)) = &*computed.expr {
                            s.value.as_str().map(|v| v.to_string())
                        } else {
                            None
                        }
                    }
                    _ => None,
                };

                if let Some(key) = key
                    && let Some(value) = map.get(&key)
                {
                    *expr = value.to_expr();
                }
            }
        }
    }
}

/// Visitor to find read-only variables initialized with literals
struct ReadOnlyLiteralFinder<'a> {
    scope_data: &'a crate::scope::ScopeData,
    literals: &'a mut HashMap<Id, Expr>,
    vars_to_remove: &'a mut Vec<Id>,
    remove_garbage: bool,
}

impl<'a> VisitMut for ReadOnlyLiteralFinder<'a> {
    fn visit_mut_var_decl(&mut self, decl: &mut VarDecl) {
        decl.visit_mut_children_with(self);

        for declarator in &decl.decls {
            if let Pat::Ident(binding) = &declarator.name {
                let id: Id = (binding.id.sym.clone(), binding.id.ctxt);

                // Check if this variable is read-only
                if let Some(var_info) = self.scope_data.vars.get(&id) {
                    // Skip if not declared or if it's reassigned (assign_count > 1 means more than init)
                    if !var_info.declared || var_info.assign_count > 1 || var_info.used_as_ref {
                        continue;
                    }

                    // Skip 'arguments'
                    if binding.id.sym.as_str() == "arguments" {
                        continue;
                    }

                    // Check if initialized with a literal
                    if let Some(init) = &declarator.init
                        && let Some(lit_expr) = extract_literal_expr(init)
                    {
                        // Skip long strings (65 chars, likely hashes)
                        if let Expr::Lit(Lit::Str(s)) = &lit_expr
                            && s.value.len() == 65
                        {
                            continue;
                        }

                        self.literals.insert(id.clone(), lit_expr);
                        if self.remove_garbage {
                            self.vars_to_remove.push(id);
                        }
                    }
                }
            }
        }
    }
}

/// Extract literal expression from an expression
#[must_use]
fn extract_literal_expr(expr: &Expr) -> Option<Expr> {
    match expr {
        Expr::Lit(Lit::Str(_) | Lit::Num(_) | Lit::Bool(_) | Lit::Null(_)) => Some(expr.clone()),
        Expr::Unary(UnaryExpr {
            op: UnaryOp::Minus,
            arg,
            ..
        }) => {
            if matches!(&**arg, Expr::Lit(Lit::Num(_))) {
                Some(expr.clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Visitor to replace references with literals and remove declarations
struct LiteralReplacer<'a> {
    literals: &'a HashMap<Id, Expr>,
    vars_to_remove: &'a Vec<Id>,
    remove_garbage: bool,
}

impl<'a> VisitMut for LiteralReplacer<'a> {
    fn visit_mut_var_decl(&mut self, decl: &mut VarDecl) {
        decl.visit_mut_children_with(self);

        // Remove variable declarations for read-only literals
        if self.remove_garbage {
            decl.decls.retain(|declarator| {
                if let Pat::Ident(binding) = &declarator.name {
                    let id: Id = (binding.id.sym.clone(), binding.id.ctxt);
                    !self.vars_to_remove.contains(&id)
                } else {
                    true
                }
            });
        }
    }

    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        expr.visit_mut_children_with(self);

        // Replace identifier references with literals
        if let Expr::Ident(ident) = expr {
            let id: Id = (ident.sym.clone(), ident.ctxt);
            if let Some(lit_expr) = self.literals.get(&id) {
                *expr = lit_expr.clone();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Deobfuscator;
    use crate::deobfuscator::DeobfuscateOptions;
    use std::sync::Arc;

    #[test]
    fn test_literal_value_to_expr() {
        let string_val = LiteralValue::String("hello".to_string());
        assert!(matches!(string_val.to_expr(), Expr::Lit(Lit::Str(_))));

        let num_val = LiteralValue::Number(42.0);
        assert!(matches!(num_val.to_expr(), Expr::Lit(Lit::Num(_))));

        let bool_val = LiteralValue::Bool(true);
        assert!(matches!(bool_val.to_expr(), Expr::Lit(Lit::Bool(_))));

        let null_val = LiteralValue::Null;
        assert!(matches!(null_val.to_expr(), Expr::Lit(Lit::Null(_))));
    }

    #[test]
    fn test_literal_map_replaces_same_decl_usage() {
        let code = r#"
function demo() {
  const map = { a: "x" }, obj = { val: map.a };
  return obj;
}
"#;
        let mut options = DeobfuscateOptions::default();
        options.custom_transformers = Some(vec![Arc::new(LiteralMap::new())]);
        let deob = Deobfuscator::new();
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(!result.contains("map.a"));
        assert!(result.contains("\"x\""));
    }

    #[test]
    fn test_literal_map_keeps_object_when_used_as_value() {
        let deob = Deobfuscator::new();
        let code = r#"
function use(obj) { return obj.a; }
const map = { a: "x", b: "y" };
console.log(map.a, use(map));
"#;
        let mut options = DeobfuscateOptions::default();
        options.custom_transformers = Some(vec![Arc::new(LiteralMap::new())]);
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(result.contains("\"x\""));
        assert!(result.contains("use(map)"));
        assert!(result.contains("const map"));
    }
}
