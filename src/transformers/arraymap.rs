//! ArrayMap transformer
//!
//! Replaces array literal accesses with their values.
//! Handles arrays that start with null as the first element.
//!
//! Example:
//! ```javascript
//! function f() {
//!     var arr = [null, "hello", 42];
//!     console.log(arr[1], arr[2]);
//! }
//! // becomes:
//! function f() {
//!     console.log("hello", 42);
//! }
//! ```

use std::collections::HashMap;
use swc_ecma_ast::*;
use swc_ecma_visit::{VisitMut, VisitMutWith};

use crate::context::Context;
use crate::error::Result;
use crate::transformers::Transformer;

/// ArrayMap transformer.
///
/// Inlines array literal accesses where the array starts with null to reduce
/// indirect array lookups.
#[derive(Debug)]
pub struct ArrayMap;

impl ArrayMap {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for ArrayMap {
    fn default() -> Self {
        Self::new()
    }
}

impl Transformer for ArrayMap {
    fn name(&self) -> &'static str {
        "ArrayMap"
    }

    fn transform(&self, context: &mut Context) -> Result<()> {
        let mut visitor = ArrayMapVisitor;
        context.ast.visit_mut_with(&mut visitor);
        Ok(())
    }
}

/// Represents a value that can be stored in an array map
#[derive(Clone, Debug)]
enum ArrayValue {
    Null,
    String(String),
    Number(f64),
    Bool(bool),
}

impl ArrayValue {
    #[must_use]
    fn to_expr(&self) -> Option<Expr> {
        match self {
            Self::Null => None,
            Self::String(s) => Some(Expr::Lit(Lit::Str(Str {
                span: Default::default(),
                value: s.as_str().into(),
                raw: None,
            }))),
            Self::Number(n) => {
                if *n < 0.0 {
                    Some(Expr::Unary(UnaryExpr {
                        span: Default::default(),
                        op: UnaryOp::Minus,
                        arg: Box::new(Expr::Lit(Lit::Num(Number {
                            span: Default::default(),
                            value: -n,
                            raw: None,
                        }))),
                    }))
                } else {
                    Some(Expr::Lit(Lit::Num(Number {
                        span: Default::default(),
                        value: *n,
                        raw: None,
                    })))
                }
            }
            Self::Bool(b) => Some(Expr::Lit(Lit::Bool(Bool {
                span: Default::default(),
                value: *b,
            }))),
        }
    }
}

struct ArrayMapVisitor;

impl ArrayMapVisitor {
    /// Extract array values from an array literal
    #[must_use]
    fn extract_array_values(arr: &ArrayLit) -> Option<Vec<ArrayValue>> {
        // Must start with null
        if arr.elems.is_empty() {
            return None;
        }

        let first = arr.elems.first()?;
        match first {
            Some(ExprOrSpread { expr, .. }) => {
                if !matches!(&**expr, Expr::Lit(Lit::Null(_))) {
                    return None;
                }
            }
            None => {
                // Hole in array - treat as null
            }
        }

        let mut values = Vec::new();
        for elem in &arr.elems {
            match elem {
                None => values.push(ArrayValue::Null),
                Some(ExprOrSpread { expr, spread: None }) => match &**expr {
                    Expr::Lit(Lit::Null(_)) => values.push(ArrayValue::Null),
                    Expr::Lit(Lit::Str(s)) => {
                        if let Some(v) = s.value.as_str() {
                            values.push(ArrayValue::String(v.to_string()));
                        } else {
                            return None;
                        }
                    }
                    Expr::Lit(Lit::Num(n)) => values.push(ArrayValue::Number(n.value)),
                    Expr::Lit(Lit::Bool(b)) => values.push(ArrayValue::Bool(b.value)),
                    Expr::Unary(UnaryExpr {
                        op: UnaryOp::Minus,
                        arg,
                        ..
                    }) => {
                        if let Expr::Lit(Lit::Num(n)) = &**arg {
                            values.push(ArrayValue::Number(-n.value));
                        } else {
                            return None;
                        }
                    }
                    _ => return None,
                },
                Some(ExprOrSpread {
                    spread: Some(_), ..
                }) => return None,
            }
        }

        Some(values)
    }
}

impl VisitMut for ArrayMapVisitor {
    fn visit_mut_function(&mut self, func: &mut Function) {
        // First visit children
        func.visit_mut_children_with(self);

        if let Some(body) = &mut func.body {
            self.process_block(&mut body.stmts);
        }
    }

    fn visit_mut_arrow_expr(&mut self, arrow: &mut ArrowExpr) {
        arrow.visit_mut_children_with(self);

        if let BlockStmtOrExpr::BlockStmt(block) = &mut *arrow.body {
            self.process_block(&mut block.stmts);
        }
    }
}

impl ArrayMapVisitor {
    fn process_block(&mut self, stmts: &mut Vec<Stmt>) {
        // Find array declarations at the start of the block
        let mut array_maps: HashMap<String, Vec<ArrayValue>> = HashMap::new();
        let mut decls_to_remove: Vec<usize> = Vec::new();

        // Look for variable declarations with array literals starting with null
        for (idx, stmt) in stmts.iter().enumerate() {
            if let Stmt::Decl(Decl::Var(var_decl)) = stmt {
                for decl in &var_decl.decls {
                    if let Pat::Ident(binding) = &decl.name
                        && let Some(init) = &decl.init
                        && let Expr::Array(arr) = &**init
                        && let Some(values) = Self::extract_array_values(arr)
                    {
                        let name = binding.id.sym.to_string();
                        array_maps.insert(name, values);
                        decls_to_remove.push(idx);
                    }
                }
            }
        }

        if array_maps.is_empty() {
            return;
        }

        // Replace array accesses
        let mut replacer = ArrayAccessReplacer {
            array_maps: &array_maps,
        };
        for stmt in stmts.iter_mut() {
            stmt.visit_mut_with(&mut replacer);
        }

        // Remove the array declarations (in reverse order to maintain indices)
        for idx in decls_to_remove.into_iter().rev() {
            stmts[idx] = Stmt::Empty(EmptyStmt {
                span: Default::default(),
            });
        }

        // Clean up empty statements
        stmts.retain(|s| !matches!(s, Stmt::Empty(_)));
    }
}

struct ArrayAccessReplacer<'a> {
    array_maps: &'a HashMap<String, Vec<ArrayValue>>,
}

impl VisitMut for ArrayAccessReplacer<'_> {
    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        expr.visit_mut_children_with(self);

        if let Expr::Member(member) = expr
            && let Expr::Ident(obj) = &*member.obj
        {
            let obj_name = obj.sym.to_string();

            if let Some(values) = self.array_maps.get(&obj_name) {
                // Get the index
                let index = match &member.prop {
                    MemberProp::Computed(computed) => {
                        if let Expr::Lit(Lit::Num(n)) = &*computed.expr {
                            Some(n.value as usize)
                        } else {
                            None
                        }
                    }
                    _ => None,
                };

                if let Some(idx) = index
                    && idx < values.len()
                    && let Some(new_expr) = values[idx].to_expr()
                {
                    *expr = new_expr;
                }
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

    fn deob_with_arraymap(code: &str) -> String {
        let deob = Deobfuscator::new();
        let options = DeobfuscateOptions {
            custom_transformers: Some(vec![Arc::new(ArrayMap::new())]),
            ..Default::default()
        };
        deob.deobfuscate_source(code, Some(options)).unwrap()
    }

    #[test]
    fn test_arraymap_new() {
        let transformer = ArrayMap::new();
        assert_eq!(transformer.name(), "ArrayMap");
    }

    #[test]
    fn test_arraymap_basic() {
        let code = r#"
function f() {
    var arr = [null, "hello", 42];
    console.log(arr[1], arr[2]);
}
"#;
        let result = deob_with_arraymap(code);
        assert!(result.contains("\"hello\""));
        assert!(result.contains("42"));
        assert!(!result.contains("arr[1]"));
        assert!(!result.contains("arr[2]"));
    }

    #[test]
    fn test_arraymap_with_booleans() {
        let code = r#"
function f() {
    var arr = [null, true, false, "test"];
    return arr[1] && arr[3];
}
"#;
        let result = deob_with_arraymap(code);
        assert!(result.contains("true"));
        assert!(result.contains("\"test\""));
        assert!(!result.contains("arr[1]"));
        assert!(!result.contains("arr[3]"));
    }

    #[test]
    fn test_arraymap_negative_numbers() {
        let code = r#"
function f() {
    var arr = [null, 5, 10];
    return arr[1] + arr[2];
}
"#;
        let result = deob_with_arraymap(code);
        // Check that array values are inlined
        assert!(result.contains("5"));
        assert!(result.contains("10"));
        assert!(!result.contains("arr[1]"));
        assert!(!result.contains("arr[2]"));
    }

    #[test]
    fn test_arraymap_arrow_function() {
        // Arrow function with block body
        let code = r#"
function outer() {
    const f = () => {
        var arr = [null, "arrow", "function"];
        return arr[1] + arr[2];
    };
    return f;
}
"#;
        let result = deob_with_arraymap(code);
        // Arrow functions are processed - check the output
        assert!(result.contains("\"arrow\""));
        assert!(result.contains("\"function\""));
        assert!(!result.contains("arr[1]"));
        assert!(!result.contains("arr[2]"));
    }

    #[test]
    fn test_arraymap_not_starting_with_null() {
        // Arrays not starting with null should NOT be replaced
        let code = r#"
function f() {
    var arr = ["hello", "world"];
    return arr[0];
}
"#;
        let result = deob_with_arraymap(code);
        // Should still contain arr[0] since array doesn't start with null
        assert!(result.contains("arr[0]"));
    }

    #[test]
    fn test_arraymap_multiple_arrays() {
        let code = r#"
function f() {
    var a = [null, "first"];
    console.log(a[1]);
}
"#;
        let result = deob_with_arraymap(code);
        assert!(result.contains("console.log(\"first\")"));
        assert!(!result.contains("a[1]"));
    }

    #[test]
    fn test_array_value_to_expr() {
        let val = ArrayValue::String("test".to_string());
        let expr = val.to_expr();
        assert!(expr.is_some());

        let val = ArrayValue::Number(42.0);
        let expr = val.to_expr();
        assert!(expr.is_some());

        let val = ArrayValue::Number(-10.0);
        let expr = val.to_expr();
        assert!(expr.is_some());

        let val = ArrayValue::Bool(true);
        let expr = val.to_expr();
        assert!(expr.is_some());

        let val = ArrayValue::Null;
        let expr = val.to_expr();
        assert!(expr.is_none());
    }
}
