//! `AntiDebug` transformer
//!
//! Neutralizes time-based anti-debug/VM guard functions by replacing their
//! bodies with empty blocks when they match a strict heuristic.

use std::collections::HashSet;

use swc_common::{Span, SyntaxContext};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitMut, VisitMutWith as _, VisitWith as _};

use crate::context::Context;
use crate::error::Result;
use crate::scope::Id;
use crate::transformers::Transformer;

/// `AntiDebug` transformer.
///
/// Detects time-based guard functions and no-ops them.
#[derive(Debug)]
pub struct AntiDebug;

impl AntiDebug {
    /// Creates a new transformer instance.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for AntiDebug {
    fn default() -> Self {
        Self::new()
    }
}

impl Transformer for AntiDebug {
    fn name(&self) -> &'static str {
        "AntiDebug"
    }

    fn transform(&self, context: &mut Context) -> Result<()> {
        let mut collector = AntiDebugCollector::default();
        context.ast.visit_with(&mut collector);

        if collector.candidates.is_empty() {
            return Ok(());
        }

        let mut remover = AntiDebugRemover {
            candidates: collector.candidates,
        };
        context.ast.visit_mut_with(&mut remover);

        Ok(())
    }
}

#[derive(Default)]
struct AntiDebugCollector {
    candidates: HashSet<Id>,
}

impl AntiDebugCollector {
    fn consider_function(&mut self, id: Id, func: &Function) {
        if is_antidebug_function(func) {
            self.candidates.insert(id);
        }
    }

    fn consider_arrow(&mut self, id: Id, arrow: &ArrowExpr) {
        if is_antidebug_arrow(arrow) {
            self.candidates.insert(id);
        }
    }
}

impl Visit for AntiDebugCollector {
    fn visit_fn_decl(&mut self, decl: &FnDecl) {
        self.consider_function((decl.ident.sym.clone(), decl.ident.ctxt), &decl.function);
        decl.visit_children_with(self);
    }

    fn visit_var_declarator(&mut self, decl: &VarDeclarator) {
        if let Pat::Ident(binding) = &decl.name
            && let Some(init) = &decl.init
        {
            match &**init {
                Expr::Fn(fn_expr) => {
                    self.consider_function(
                        (binding.id.sym.clone(), binding.id.ctxt),
                        &fn_expr.function,
                    );
                }
                Expr::Arrow(arrow) => {
                    self.consider_arrow((binding.id.sym.clone(), binding.id.ctxt), arrow);
                }
                _ => {}
            }
        }
        decl.visit_children_with(self);
    }

    fn visit_assign_expr(&mut self, expr: &AssignExpr) {
        if let AssignTarget::Simple(SimpleAssignTarget::Ident(ident)) = &expr.left {
            match &*expr.right {
                Expr::Fn(fn_expr) => {
                    self.consider_function(
                        (ident.id.sym.clone(), ident.id.ctxt),
                        &fn_expr.function,
                    );
                }
                Expr::Arrow(arrow) => {
                    self.consider_arrow((ident.id.sym.clone(), ident.id.ctxt), arrow);
                }
                _ => {}
            }
        }
        expr.visit_children_with(self);
    }
}

struct AntiDebugRemover {
    candidates: HashSet<Id>,
}

impl AntiDebugRemover {
    fn clear_function_body(func: &mut Function) {
        if let Some(body) = &mut func.body {
            body.stmts.clear();
        } else {
            func.body = Some(BlockStmt {
                span: Span::default(),
                ctxt: SyntaxContext::default(),
                stmts: Vec::new(),
            });
        }
    }

    fn clear_arrow_body(arrow: &mut ArrowExpr) {
        *arrow.body = BlockStmtOrExpr::BlockStmt(BlockStmt {
            span: Span::default(),
            ctxt: SyntaxContext::default(),
            stmts: Vec::new(),
        });
    }
}

impl VisitMut for AntiDebugRemover {
    fn visit_mut_fn_decl(&mut self, decl: &mut FnDecl) {
        if self
            .candidates
            .contains(&(decl.ident.sym.clone(), decl.ident.ctxt))
        {
            Self::clear_function_body(&mut decl.function);
        }

        decl.visit_mut_children_with(self);
    }

    fn visit_mut_var_declarator(&mut self, decl: &mut VarDeclarator) {
        if let Pat::Ident(binding) = &decl.name
            && self
                .candidates
                .contains(&(binding.id.sym.clone(), binding.id.ctxt))
            && let Some(init) = &mut decl.init
        {
            match &mut **init {
                Expr::Fn(fn_expr) => Self::clear_function_body(&mut fn_expr.function),
                Expr::Arrow(arrow) => Self::clear_arrow_body(arrow),
                _ => {}
            }
        }

        decl.visit_mut_children_with(self);
    }

    fn visit_mut_assign_expr(&mut self, expr: &mut AssignExpr) {
        if let AssignTarget::Simple(SimpleAssignTarget::Ident(ident)) = &expr.left
            && self
                .candidates
                .contains(&(ident.id.sym.clone(), ident.id.ctxt))
        {
            match &mut *expr.right {
                Expr::Fn(fn_expr) => Self::clear_function_body(&mut fn_expr.function),
                Expr::Arrow(arrow) => Self::clear_arrow_body(arrow),
                _ => {}
            }
        }

        expr.visit_mut_children_with(self);
    }
}

fn is_antidebug_function(func: &Function) -> bool {
    if !func.params.is_empty() {
        return false;
    }
    let Some(body) = &func.body else {
        return false;
    };
    let mut scan = AntiDebugScan::default();
    body.visit_with(&mut scan);
    scan.is_match()
}

fn is_antidebug_arrow(arrow: &ArrowExpr) -> bool {
    if !arrow.params.is_empty() {
        return false;
    }
    let mut scan = AntiDebugScan::default();
    match &*arrow.body {
        BlockStmtOrExpr::BlockStmt(block) => block.visit_with(&mut scan),
        BlockStmtOrExpr::Expr(expr) => {
            // Treat expression body as a return value.
            if !is_undefined_expr(expr) {
                scan.return_with_value = true;
            }
        }
    }
    scan.is_match()
}

#[derive(Default)]
struct AntiDebugScan {
    has_now: bool,
    has_threshold: bool,
    has_scramble_loop: bool,
    return_with_value: bool,
}

impl AntiDebugScan {
    const THRESHOLD: f64 = 1_000.0;

    const fn is_match(&self) -> bool {
        self.has_now && self.has_threshold && self.has_scramble_loop && !self.return_with_value
    }
}

impl Visit for AntiDebugScan {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        if is_now_call(call) {
            self.has_now = true;
        }
        call.visit_children_with(self);
    }

    fn visit_bin_expr(&mut self, expr: &BinExpr) {
        if matches!(expr.op, BinaryOp::Gt | BinaryOp::GtEq) {
            let left_num = extract_number_literal(&expr.left);
            let right_num = extract_number_literal(&expr.right);
            if left_num >= Self::THRESHOLD || right_num >= Self::THRESHOLD {
                self.has_threshold = true;
            }
        }
        expr.visit_children_with(self);
    }

    fn visit_return_stmt(&mut self, stmt: &ReturnStmt) {
        if let Some(arg) = &stmt.arg
            && !is_undefined_expr(arg)
        {
            self.return_with_value = true;
        }
        stmt.visit_children_with(self);
    }

    fn visit_for_in_stmt(&mut self, stmt: &ForInStmt) {
        if for_in_scrambles_object(stmt) {
            self.has_scramble_loop = true;
        }
        stmt.visit_children_with(self);
    }
}

const fn extract_number_literal(expr: &Expr) -> f64 {
    match expr {
        Expr::Lit(Lit::Num(num)) => num.value,
        _ => -1.0,
    }
}

fn is_now_call(call: &CallExpr) -> bool {
    let Callee::Expr(callee) = &call.callee else {
        return false;
    };
    let Expr::Member(member) = &**callee else {
        return false;
    };

    if !member.prop.is_computed() {
        if let MemberProp::Ident(prop) = &member.prop
            && prop.sym.as_ref() != "now"
        {
            return false;
        }
    } else if let MemberProp::Computed(comp) = &member.prop {
        if let Expr::Lit(Lit::Str(s)) = &*comp.expr {
            if s.value.as_str() != Some("now") {
                return false;
            }
        } else {
            return false;
        }
    }

    match &*member.obj {
        Expr::Ident(obj) => matches!(obj.sym.as_ref(), "Date" | "performance"),
        Expr::Member(inner) => {
            if let Expr::Ident(base) = &*inner.obj {
                base.sym.as_ref() == "performance"
            } else {
                false
            }
        }
        _ => false,
    }
}

fn is_undefined_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Ident(ident) => ident.sym.as_ref() == "undefined",
        Expr::Unary(unary) => unary.op == UnaryOp::Void,
        _ => false,
    }
}

fn for_in_scrambles_object(stmt: &ForInStmt) -> bool {
    let (iter_name, obj_name) = match &stmt.left {
        ForHead::VarDecl(var_decl) => {
            let Some(decl) = var_decl.decls.first() else {
                return false;
            };
            let Pat::Ident(binding) = &decl.name else {
                return false;
            };
            let iter = binding.id.sym.as_ref();
            let Expr::Ident(obj) = &*stmt.right else {
                return false;
            };
            (iter.to_owned(), obj.sym.to_string())
        }
        ForHead::Pat(pat) => {
            let Pat::Ident(ident) = &**pat else {
                return false;
            };
            let Expr::Ident(obj) = &*stmt.right else {
                return false;
            };
            (ident.sym.to_string(), obj.sym.to_string())
        }
        ForHead::UsingDecl(_) => return false,
    };

    let mut finder = ForInAssignFinder {
        iter_name,
        obj_name,
        found: false,
    };
    stmt.body.visit_with(&mut finder);
    finder.found
}

struct ForInAssignFinder {
    iter_name: String,
    obj_name: String,
    found: bool,
}

impl Visit for ForInAssignFinder {
    fn visit_assign_expr(&mut self, expr: &AssignExpr) {
        if let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = &expr.left
            && is_member_obj_prop(member, &self.obj_name, &self.iter_name)
        {
            self.found = true;
            return;
        }

        expr.visit_children_with(self);
    }
}

fn is_member_obj_prop(member: &MemberExpr, obj_name: &str, prop_name: &str) -> bool {
    let Expr::Ident(obj) = &*member.obj else {
        return false;
    };
    if obj.sym.as_ref() != obj_name {
        return false;
    }
    if let MemberProp::Computed(comp) = &member.prop
        && let Expr::Ident(prop) = &*comp.expr
    {
        return prop.sym.as_ref() == prop_name;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::{DeobfuscateOptions, Deobfuscator};

    #[test]
    fn antidebug_noops_time_guard() {
        let code = r"
function guard() {
  let t = Date.now();
  if (t > 5000) {
    for (let k in tbl) {
      tbl[k] = tbl[k] + 1 & 511;
    }
  }
}
function ok() { return 1; }
";
        let deob = Deobfuscator::new();
        let options = DeobfuscateOptions {
            custom_transformers: Some(vec![Arc::new(AntiDebug::new())]),
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(result.contains("function guard()"));
        assert!(!result.contains("Date.now"));
        assert!(!result.contains("tbl[k]"));
        assert!(result.contains("function ok()"));
    }
}
