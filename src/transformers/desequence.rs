//! Desequence transformer
//!
//! Splits sequence expressions into individual statements.
//! Example: `a = 1, b = 2, c = 3;` -> `a = 1; b = 2; c = 3;`

use swc_common::Span;
use swc_ecma_ast::*;
use swc_ecma_visit::{VisitMut, VisitMutWith as _};

use crate::context::Context;
use crate::error::Result;
use crate::transformers::Transformer;

/// Desequence transformer.
///
/// Converts sequence expressions in expression statements into
/// individual expression statements.
#[derive(Debug)]
pub struct Desequence;

impl Desequence {
    /// Creates a new transformer instance.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for Desequence {
    fn default() -> Self {
        Self::new()
    }
}

impl Transformer for Desequence {
    fn name(&self) -> &'static str {
        "Desequence"
    }

    fn transform(&self, context: &mut Context) -> Result<()> {
        let mut visitor = DesequenceVisitor;
        context.ast.visit_mut_with(&mut visitor);
        Ok(())
    }
}

struct DesequenceVisitor;

impl DesequenceVisitor {
    /// Process a list of statements, expanding sequence expressions
    fn process_stmts(stmts: &mut Vec<Stmt>) {
        let mut i = 0;
        while i < stmts.len() {
            if let Some(stmt) = stmts.get(i)
                && let Stmt::Decl(Decl::Var(var_decl)) = stmt
                && var_decl.decls.len() > 1
            {
                let var_decl = var_decl.clone();
                let new_stmts: Vec<Stmt> = var_decl
                    .decls
                    .iter()
                    .map(|decl| {
                        Stmt::Decl(Decl::Var(Box::new(VarDecl {
                            span: var_decl.span,
                            ctxt: var_decl.ctxt,
                            kind: var_decl.kind,
                            declare: var_decl.declare,
                            decls: vec![decl.clone()],
                        })))
                    })
                    .collect();

                let new_len = new_stmts.len();
                stmts.splice(i..=i, new_stmts);
                i += new_len;
                continue;
            }

            if let Some(stmt) = stmts.get(i)
                && let Stmt::Return(ret) = stmt
                && let Some(arg) = &ret.arg
                && let Some(seq) = extract_seq_expr(arg)
            {
                if seq.exprs.is_empty() {
                    i += 1;
                    continue;
                }

                let mut new_stmts = Vec::with_capacity(seq.exprs.len());
                for expr in seq.exprs.iter().take(seq.exprs.len() - 1) {
                    new_stmts.push(Stmt::Expr(ExprStmt {
                        span: Span::default(),
                        expr: expr.clone(),
                    }));
                }

                let last = seq.exprs.last().expect("sequence non-empty").clone();
                new_stmts.push(Stmt::Return(ReturnStmt {
                    span: ret.span,
                    arg: Some(last),
                }));

                let new_len = new_stmts.len();
                stmts.splice(i..=i, new_stmts);
                i += new_len;
                continue;
            }

            if let Some(stmt) = stmts.get(i)
                && let Stmt::Expr(expr_stmt) = stmt
                && let Expr::Seq(seq) = &*expr_stmt.expr
            {
                // Convert each expression in the sequence to a statement
                let new_stmts: Vec<Stmt> = seq
                    .exprs
                    .iter()
                    .map(|e| {
                        Stmt::Expr(ExprStmt {
                            span: Span::default(),
                            expr: e.clone(),
                        })
                    })
                    .collect();

                // Replace the current statement with the expanded statements
                let new_len = new_stmts.len();
                stmts.splice(i..=i, new_stmts);
                i += new_len;
                continue;
            }
            i += 1;
        }
    }
}

impl VisitMut for DesequenceVisitor {
    fn visit_mut_stmts(&mut self, stmts: &mut Vec<Stmt>) {
        // First visit children
        for stmt in stmts.iter_mut() {
            stmt.visit_mut_with(self);
        }

        // Then process this level
        Self::process_stmts(stmts);
    }

    fn visit_mut_module_items(&mut self, items: &mut Vec<ModuleItem>) {
        // First visit children
        for item in items.iter_mut() {
            item.visit_mut_with(self);
        }

        // Convert to statements, process, then convert back
        let mut stmts: Vec<Stmt> = items
            .iter()
            .filter_map(|item| match item {
                ModuleItem::Stmt(stmt) => Some(stmt.clone()),
                ModuleItem::ModuleDecl(_) => None,
            })
            .collect();

        Self::process_stmts(&mut stmts);

        // Rebuild items preserving non-statement items
        let mut new_items = Vec::new();
        let mut stmt_iter = stmts.into_iter();

        for item in items.iter() {
            match item {
                ModuleItem::Stmt(_) => {
                    if let Some(stmt) = stmt_iter.next() {
                        new_items.push(ModuleItem::Stmt(stmt));
                    }
                    // Handle additional statements from expansion
                    while new_items.len() < items.len() {
                        if let Some(stmt) = stmt_iter.next() {
                            new_items.push(ModuleItem::Stmt(stmt));
                        } else {
                            break;
                        }
                    }
                }
                ModuleItem::ModuleDecl(_) => new_items.push(item.clone()),
            }
        }

        // Add any remaining expanded statements
        for stmt in stmt_iter {
            new_items.push(ModuleItem::Stmt(stmt));
        }

        *items = new_items;
    }
}

#[must_use]
const fn extract_seq_expr(expr: &Expr) -> Option<&SeqExpr> {
    match expr {
        Expr::Seq(seq) => Some(seq),
        Expr::Paren(paren) => extract_seq_expr(&paren.expr),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desequence() {
        // Basic test to ensure the struct can be created
        let transformer = Desequence::new();
        assert_eq!(transformer.name(), "Desequence");
    }

    #[test]
    fn desequence_var_decl_split() {
        use crate::{DeobfuscateOptions, Deobfuscator};
        use std::sync::Arc;

        let deob = Deobfuscator::new();
        let code = "const a = 1, b = 2;";
        let options = DeobfuscateOptions {
            custom_transformers: Some(vec![Arc::new(Desequence::new())]),
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(result.contains("const a = 1"));
        assert!(result.contains("const b = 2"));
    }

    #[test]
    fn desequence_return_sequence() {
        use crate::{DeobfuscateOptions, Deobfuscator};
        use std::sync::Arc;

        let deob = Deobfuscator::new();
        let code = "function f(){ return (a = 1, b = 2); }";
        let options = DeobfuscateOptions {
            custom_transformers: Some(vec![Arc::new(Desequence::new())]),
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(result.contains("a = 1") || result.contains("a=1"));
        assert!(!result.contains(", b"));
    }
}
