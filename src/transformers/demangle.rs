//! Demangle transformer
//!
//! Demangles proxy functions and fixes function structures.
//! This handles patterns commonly used by obfuscators for the string decoder.

use swc_ecma_ast::*;
use swc_ecma_visit::{VisitMut, VisitMutWith};

use crate::context::Context;
use crate::error::Result;
use crate::transformers::Transformer;

/// Demangle transformer.
///
/// Simplifies proxy function patterns commonly used in obfuscation.
#[derive(Debug)]
pub struct Demangle;

impl Demangle {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for Demangle {
    fn default() -> Self {
        Self::new()
    }
}

impl Transformer for Demangle {
    fn name(&self) -> &'static str {
        "Demangle"
    }

    fn transform(&self, context: &mut Context) -> Result<()> {
        // First pass: demangle proxy function patterns
        let mut proxy_visitor = DemangleProxyVisitor;
        context.ast.visit_mut_with(&mut proxy_visitor);

        // Second pass: demangle string decoder functions
        let mut string_func_visitor = DemangleStringFuncsVisitor;
        context.ast.visit_mut_with(&mut string_func_visitor);

        // Third pass: simplify IIFE patterns
        let mut iife_visitor = DemangleIIFEVisitor;
        context.ast.visit_mut_with(&mut iife_visitor);

        Ok(())
    }
}

/// Visitor that demangles proxy function patterns
/// Handles: return (`func_name` = `function()` { ... }, `func_name`(...))
struct DemangleProxyVisitor;

/// Visitor that demangles string decoder function patterns
/// Extracts offset setters and charset declarations
struct DemangleStringFuncsVisitor;

impl VisitMut for DemangleProxyVisitor {
    fn visit_mut_function(&mut self, func: &mut Function) {
        // First visit children
        func.visit_mut_children_with(self);

        if let Some(body) = &mut func.body {
            // Filter empty statements
            let non_empty: Vec<_> = body
                .stmts
                .iter()
                .filter(|s| !matches!(s, Stmt::Empty(_)))
                .cloned()
                .collect();

            // Looking for 2-statement functions with specific pattern
            if non_empty.len() != 2 {
                return;
            }

            // Last statement must be a return
            let last = non_empty
                .last()
                .expect("non_empty has length 2 after earlier check");
            if let Stmt::Return(ret) = last
                && let Some(arg) = &ret.arg
            {
                // Pattern 1: return (func_name = function() { ... })()
                if let Expr::Call(call) = &**arg
                    && let Callee::Expr(callee) = &call.callee
                    && let Expr::Assign(assign) = &**callee
                    && let (AssignTarget::Simple(SimpleAssignTarget::Ident(id)), Expr::Fn(_)) =
                        (&assign.left, &*assign.right)
                {
                    let func_name = id.id.sym.to_string();

                    // Reconstruct as:
                    // statement1
                    // func_name = function() { ... }
                    // return func_name(...)
                    let new_stmts = vec![
                        non_empty[0].clone(),
                        Stmt::Expr(ExprStmt {
                            span: Default::default(),
                            expr: Box::new(Expr::Assign(AssignExpr {
                                span: Default::default(),
                                op: AssignOp::Assign,
                                left: assign.left.clone(),
                                right: assign.right.clone(),
                            })),
                        }),
                        Stmt::Return(ReturnStmt {
                            span: Default::default(),
                            arg: Some(Box::new(Expr::Call(CallExpr {
                                span: Default::default(),
                                callee: Callee::Expr(Box::new(Expr::Ident(Ident::new(
                                    func_name.into(),
                                    Default::default(),
                                    Default::default(),
                                )))),
                                args: call.args.clone(),
                                ..Default::default()
                            }))),
                        }),
                    ];

                    body.stmts = new_stmts;
                    return;
                }

                // Pattern 2: return (assignment_expr, call_expr)
                if let Expr::Seq(seq) = &**arg
                    && seq.exprs.len() == 2
                    && let (Expr::Assign(assign), Expr::Call(call)) =
                        (&*seq.exprs[0], &*seq.exprs[1])
                    && let AssignTarget::Simple(SimpleAssignTarget::Ident(id)) = &assign.left
                    && let Expr::Fn(_) = &*assign.right
                {
                    let func_name = id.id.sym.to_string();

                    let new_stmts = vec![
                        non_empty[0].clone(),
                        Stmt::Expr(ExprStmt {
                            span: Default::default(),
                            expr: Box::new(Expr::Assign(assign.clone())),
                        }),
                        Stmt::Return(ReturnStmt {
                            span: Default::default(),
                            arg: Some(Box::new(Expr::Call(CallExpr {
                                span: Default::default(),
                                callee: Callee::Expr(Box::new(Expr::Ident(Ident::new(
                                    func_name.into(),
                                    Default::default(),
                                    Default::default(),
                                )))),
                                args: call.args.clone(),
                                ..Default::default()
                            }))),
                        }),
                    ];

                    body.stmts = new_stmts;
                }
            }
        }
    }
}

/// Implementation for `DemangleStringFuncsVisitor`
/// This handles the pattern where string decoder functions have:
/// - An assignment that sets up the function
/// - A member expression with an offset subtraction like arr[idx -= offset]
impl VisitMut for DemangleStringFuncsVisitor {
    fn visit_mut_fn_decl(&mut self, decl: &mut FnDecl) {
        // First visit children
        decl.visit_mut_children_with(self);

        let func = &mut decl.function;
        let Some(body) = &mut func.body else { return };

        // Must have exactly 3 statements
        if body.stmts.len() != 3 {
            return;
        }

        // Second statement must be: funcName = function() { ... }
        let Stmt::Expr(expr_stmt) = &body.stmts[1] else {
            return;
        };
        let Expr::Assign(assign) = &*expr_stmt.expr else {
            return;
        };

        // Check left side is identifier matching function name
        let AssignTarget::Simple(SimpleAssignTarget::Ident(left_id)) = &assign.left else {
            return;
        };
        if left_id.id.sym != decl.ident.sym {
            return;
        }

        // Right side must be function expression
        let Expr::Fn(fn_expr) = &*assign.right else {
            return;
        };
        let Some(inner_body) = &fn_expr.function.body else {
            return;
        };

        if inner_body.stmts.is_empty() {
            return;
        }

        // First statement in inner function should be a variable declaration
        let Stmt::Decl(Decl::Var(var_decl)) = &inner_body.stmts[0] else {
            return;
        };
        if var_decl.decls.len() != 1 {
            return;
        }

        let declarator = &var_decl.decls[0];
        let Some(init) = &declarator.init else { return };

        // Should be a member expression with assignment in property
        let Expr::Member(member) = &**init else {
            return;
        };

        // Property should be an assignment expression like (idx -= offset)
        let MemberProp::Computed(computed) = &member.prop else {
            return;
        };
        let Expr::Assign(prop_assign) = &*computed.expr else {
            return;
        };

        if prop_assign.op != AssignOp::SubAssign {
            return;
        }

        // Get offset identifier and value
        let AssignTarget::Simple(SimpleAssignTarget::Ident(offset_id)) = &prop_assign.left else {
            return;
        };
        let Expr::Lit(Lit::Num(offset_val)) = &*prop_assign.right else {
            return;
        };

        let offset_name = offset_id.id.sym.to_string();
        let offset_value = offset_val.value;

        // Create new statement: offsetId = offsetId - offsetVal
        let offset_stmt = Stmt::Expr(ExprStmt {
            span: Default::default(),
            expr: Box::new(Expr::Assign(AssignExpr {
                span: Default::default(),
                op: AssignOp::Assign,
                left: AssignTarget::Simple(SimpleAssignTarget::Ident(BindingIdent {
                    id: Ident::new(
                        offset_name.clone().into(),
                        Default::default(),
                        Default::default(),
                    ),
                    type_ann: None,
                })),
                right: Box::new(Expr::Bin(BinExpr {
                    span: Default::default(),
                    op: BinaryOp::Sub,
                    left: Box::new(Expr::Ident(Ident::new(
                        offset_name.clone().into(),
                        Default::default(),
                        Default::default(),
                    ))),
                    right: Box::new(Expr::Lit(Lit::Num(Number {
                        span: Default::default(),
                        value: offset_value,
                        raw: None,
                    }))),
                })),
            })),
        });

        // Update the member expression property to just use the identifier
        // We need to clone and modify the inner function
        let mut new_inner_body_stmts = inner_body.stmts.clone();

        if let Stmt::Decl(Decl::Var(var_decl)) = &mut new_inner_body_stmts[0]
            && let Some(ref mut init) = var_decl.decls[0].init
            && let Expr::Member(member) = &mut **init
        {
            member.prop = MemberProp::Computed(ComputedPropName {
                span: Default::default(),
                expr: Box::new(Expr::Ident(Ident::new(
                    offset_name.into(),
                    Default::default(),
                    Default::default(),
                ))),
            });
        }

        // Prepend offset statement to inner function body
        let mut new_stmts = vec![offset_stmt];
        new_stmts.extend(new_inner_body_stmts);

        // Build new function expression with updated body
        let new_fn_expr = FnExpr {
            ident: fn_expr.ident.clone(),
            function: Box::new(Function {
                params: fn_expr.function.params.clone(),
                decorators: fn_expr.function.decorators.clone(),
                span: fn_expr.function.span,
                ctxt: fn_expr.function.ctxt,
                body: Some(BlockStmt {
                    span: inner_body.span,
                    ctxt: inner_body.ctxt,
                    stmts: new_stmts,
                }),
                is_generator: fn_expr.function.is_generator,
                is_async: fn_expr.function.is_async,
                type_params: fn_expr.function.type_params.clone(),
                return_type: fn_expr.function.return_type.clone(),
            }),
        };

        // Update the assignment
        body.stmts[1] = Stmt::Expr(ExprStmt {
            span: Default::default(),
            expr: Box::new(Expr::Assign(AssignExpr {
                span: assign.span,
                op: assign.op,
                left: assign.left.clone(),
                right: Box::new(Expr::Fn(new_fn_expr)),
            })),
        });
    }
}

/// Visitor that simplifies IIFE patterns
struct DemangleIIFEVisitor;

impl DemangleIIFEVisitor {
    /// Check if this is a simple proxy function that just returns another expression
    #[must_use]
    fn is_simple_proxy(func: &Function) -> Option<Expr> {
        let body = func.body.as_ref()?;

        // Filter out empty statements
        let stmts: Vec<_> = body
            .stmts
            .iter()
            .filter(|s| !matches!(s, Stmt::Empty(_)))
            .collect();

        // Must have exactly one statement
        if stmts.len() != 1 {
            return None;
        }

        // Must be a return statement
        if let Stmt::Return(ret) = stmts[0] {
            ret.arg.as_ref().map(|e| (**e).clone())
        } else {
            None
        }
    }
}

impl VisitMut for DemangleIIFEVisitor {
    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        // First visit children
        expr.visit_mut_children_with(self);

        // Simplify IIFE patterns: (function() { return X; })() -> X
        if let Expr::Call(call) = expr {
            // Check if callee is a function expression or arrow function
            let func = match &call.callee {
                Callee::Expr(e) => match &**e {
                    Expr::Fn(fn_expr) => Some(&fn_expr.function),
                    Expr::Paren(paren) => {
                        if let Expr::Fn(fn_expr) = &*paren.expr {
                            Some(&fn_expr.function)
                        } else {
                            None
                        }
                    }
                    _ => None,
                },
                _ => None,
            };

            if let Some(func) = func {
                // Only simplify if no parameters are used
                if func.params.is_empty()
                    && call.args.is_empty()
                    && let Some(return_expr) = Self::is_simple_proxy(func)
                {
                    *expr = return_expr;
                    return;
                }
            }

            // Handle arrow functions too
            if let Callee::Expr(e) = &call.callee
                && let Expr::Arrow(arrow) = &**e
                && arrow.params.is_empty()
                && call.args.is_empty()
            {
                // Simple arrow: () => expr
                if let BlockStmtOrExpr::Expr(return_expr) = &*arrow.body {
                    *expr = (**return_expr).clone();
                    return;
                }

                // Block arrow: () => { return expr; }
                if let BlockStmtOrExpr::BlockStmt(block) = &*arrow.body {
                    let non_empty: Vec<_> = block
                        .stmts
                        .iter()
                        .filter(|s| !matches!(s, Stmt::Empty(_)))
                        .collect();

                    if non_empty.len() == 1
                        && let Stmt::Return(ret) = non_empty[0]
                        && let Some(arg) = &ret.arg
                    {
                        *expr = (**arg).clone();
                    }
                }
            }
        }
    }

    fn visit_mut_stmts(&mut self, stmts: &mut Vec<Stmt>) {
        // Visit children first
        stmts.iter_mut().for_each(|stmt| stmt.visit_mut_with(self));

        // Remove empty statements
        stmts.retain(|stmt| !matches!(stmt, Stmt::Empty(_)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Deobfuscator;

    #[test]
    fn test_demangle_new() {
        let transformer = Demangle::new();
        assert_eq!(transformer.name(), "Demangle");
    }

    #[test]
    fn test_demangle_iife() {
        let deob = Deobfuscator::new();
        let code = "(function() { return 42; })();";
        let result = deob.deobfuscate_source(code, None).unwrap();
        assert!(result.contains("42"));
    }

    #[test]
    fn test_demangle_arrow_iife() {
        let deob = Deobfuscator::new();
        let code = "(() => 123)();";
        let result = deob.deobfuscate_source(code, None).unwrap();
        assert!(result.contains("123"));
    }
}
