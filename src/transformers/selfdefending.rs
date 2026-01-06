//! `SelfDefending` transformer
//!
//! Removes or neutralizes common "self-defending" anti-tamper patterns by
//! replacing guard calls with no-ops and removing IIFE guard statements.

use std::collections::HashSet;

use swc_common::GLOBALS;
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitMut, VisitMutWith, VisitWith};

use crate::context::Context;
use crate::error::Result;
use crate::scope::{Id, ScopeData, analyze};
use crate::transformers::Transformer;

/// `SelfDefending` transformer.
///
/// Removes or neutralizes self-defending and anti-debug patterns.
#[derive(Debug)]
pub struct SelfDefending;

impl SelfDefending {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for SelfDefending {
    fn default() -> Self {
        Self::new()
    }
}

impl Transformer for SelfDefending {
    fn name(&self) -> &'static str {
        "SelfDefending"
    }

    fn transform(&self, context: &mut Context) -> Result<()> {
        let mut collector = SelfDefendingCollector::default();
        context.ast.visit_with(&mut collector);

        let scope_data = GLOBALS.set(&Default::default(), || analyze(&context.ast));

        let mut remover = SelfDefendingRemover {
            constructors: collector.constructors,
            self_defending_vars: collector.self_defending_vars,
            declared_names: collector.declared_names,
            scope_data: Some(scope_data),
        };
        context.ast.visit_mut_with(&mut remover);
        Ok(())
    }
}

struct SelfDefendingRemover {
    constructors: HashSet<String>,
    self_defending_vars: HashSet<Id>,
    declared_names: HashSet<String>,
    scope_data: Option<ScopeData>,
}

impl VisitMut for SelfDefendingRemover {
    fn visit_mut_module_items(&mut self, items: &mut Vec<ModuleItem>) {
        items.iter_mut().for_each(|item| item.visit_mut_with(self));

        items.retain(|item| match item {
            ModuleItem::Stmt(Stmt::Decl(Decl::Fn(fn_decl))) => {
                !is_self_defending_function(&fn_decl.function)
                    && !self.constructors.contains(fn_decl.ident.sym.as_ref())
            }
            ModuleItem::Stmt(Stmt::Decl(Decl::Var(var_decl))) => !var_decl.decls.is_empty(),
            ModuleItem::Stmt(Stmt::Empty(_)) => false,
            _ => true,
        });
    }

    fn visit_mut_stmts(&mut self, stmts: &mut Vec<Stmt>) {
        stmts.iter_mut().for_each(|stmt| stmt.visit_mut_with(self));

        stmts.retain(|stmt| match stmt {
            Stmt::Decl(Decl::Fn(fn_decl)) => {
                !is_self_defending_function(&fn_decl.function)
                    && !self.constructors.contains(fn_decl.ident.sym.as_ref())
            }
            Stmt::Decl(Decl::Var(var_decl)) => !var_decl.decls.is_empty(),
            Stmt::Empty(_) => false,
            _ => true,
        });
    }

    fn visit_mut_stmt(&mut self, stmt: &mut Stmt) {
        if let Stmt::Expr(expr_stmt) = stmt {
            if is_self_defending_call_expr(&expr_stmt.expr) {
                *stmt = Stmt::Empty(EmptyStmt {
                    span: Default::default(),
                });
                return;
            }

            if let Expr::Call(call) = &*expr_stmt.expr {
                if let Callee::Expr(callee) = &call.callee
                    && let Expr::Ident(ident) = &**callee
                    && self
                        .self_defending_vars
                        .contains(&(ident.sym.clone(), ident.ctxt))
                {
                    *stmt = Stmt::Empty(EmptyStmt {
                        span: Default::default(),
                    });
                    return;
                }

                if let Callee::Expr(callee) = &call.callee
                    && let Expr::Ident(ident) = &**callee
                    && ident.sym.as_ref().starts_with("_0x")
                    && !self.declared_names.contains(ident.sym.as_ref())
                {
                    *stmt = Stmt::Empty(EmptyStmt {
                        span: Default::default(),
                    });
                    return;
                }

                if let Some(scope_data) = &self.scope_data
                    && is_undefined_obfuscated_call(call, scope_data)
                {
                    *stmt = Stmt::Empty(EmptyStmt {
                        span: Default::default(),
                    });
                    return;
                }

                if call_targets_self_defending_constructor(call, &self.constructors) {
                    *stmt = Stmt::Empty(EmptyStmt {
                        span: Default::default(),
                    });
                    return;
                }
            }
        }

        stmt.visit_mut_children_with(self);
    }

    fn visit_mut_var_decl(&mut self, decl: &mut VarDecl) {
        let mut decls_to_remove: Vec<usize> = Vec::new();

        for (idx, declarator) in decl.decls.iter().enumerate() {
            if let Pat::Ident(binding) = &declarator.name
                && let Some(init) = &declarator.init
            {
                let is_guard = match &**init {
                    Expr::Call(call) => {
                        is_self_defending_call(call)
                            || call_has_self_defending_arg(call)
                            || call_targets_self_defending_constructor(call, &self.constructors)
                    }
                    _ => false,
                };

                if is_guard || is_self_defending_expr(init) {
                    self.self_defending_vars
                        .insert((binding.id.sym.clone(), binding.id.ctxt));
                    decls_to_remove.push(idx);
                }
            }
        }

        decl.visit_mut_children_with(self);

        if !decls_to_remove.is_empty() {
            for idx in decls_to_remove.into_iter().rev() {
                decl.decls.remove(idx);
            }
        }
    }

    fn visit_mut_assign_expr(&mut self, expr: &mut AssignExpr) {
        expr.visit_mut_children_with(self);

        if let Some(base) = prototype_base_name(&expr.left)
            && is_self_defending_expr(&expr.right)
        {
            self.constructors.insert(base);
            *expr.right = make_noop_function_expr();
        }
    }

    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        expr.visit_mut_children_with(self);

        if let Expr::Call(call) = expr
            && (is_self_defending_call(call)
                || call_has_self_defending_arg(call)
                || call_targets_self_defending_constructor(call, &self.constructors))
        {
            *expr = make_noop_function_expr();
        }
    }
}

fn make_noop_function_expr() -> Expr {
    Expr::Arrow(ArrowExpr {
        span: Default::default(),
        ctxt: Default::default(),
        params: Vec::new(),
        body: Box::new(BlockStmtOrExpr::BlockStmt(BlockStmt {
            span: Default::default(),
            ctxt: Default::default(),
            stmts: Vec::new(),
        })),
        is_async: false,
        is_generator: false,
        type_params: None,
        return_type: None,
    })
}

fn is_self_defending_call_expr(expr: &Expr) -> bool {
    let Some(call) = extract_call_expr(expr) else {
        return false;
    };
    is_self_defending_call(call)
        || call_has_self_defending_arg(call)
        || call_callee_has_self_defending_arg(call)
}

fn extract_call_expr(expr: &Expr) -> Option<&CallExpr> {
    match expr {
        Expr::Call(call) => Some(call),
        Expr::Paren(paren) => extract_call_expr(&paren.expr),
        Expr::Seq(seq) => seq.exprs.last().and_then(|expr| extract_call_expr(expr)),
        _ => None,
    }
}

fn is_self_defending_call(call: &CallExpr) -> bool {
    if let Callee::Expr(callee) = &call.callee {
        return is_self_defending_expr(callee);
    }
    false
}

fn call_targets_self_defending_constructor(
    call: &CallExpr,
    constructors: &HashSet<String>,
) -> bool {
    match &call.callee {
        Callee::Expr(callee) => match &**callee {
            Expr::Ident(ident) => constructors.contains(&ident.sym.to_string()),
            Expr::Member(member) => {
                if let Expr::New(new_expr) = &*member.obj
                    && let Expr::Ident(ident) = &*new_expr.callee
                {
                    return constructors.contains(&ident.sym.to_string());
                }
                false
            }
            Expr::New(new_expr) => {
                if let Expr::Ident(ident) = &*new_expr.callee {
                    constructors.contains(&ident.sym.to_string())
                } else {
                    false
                }
            }
            _ => false,
        },
        _ => false,
    }
}

fn call_has_self_defending_arg(call: &CallExpr) -> bool {
    call.args
        .iter()
        .any(|arg| is_self_defending_expr(&arg.expr))
}

fn call_callee_has_self_defending_arg(call: &CallExpr) -> bool {
    if let Callee::Expr(callee) = &call.callee
        && let Expr::Call(inner) = &**callee
    {
        return call_has_self_defending_arg(inner) || is_self_defending_call(inner);
    }
    false
}

fn is_self_defending_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Paren(paren) => is_self_defending_expr(&paren.expr),
        Expr::Seq(seq) => seq.exprs.iter().any(|expr| is_self_defending_expr(expr)),
        Expr::Fn(fn_expr) => is_self_defending_function(&fn_expr.function),
        Expr::Arrow(arrow) => is_self_defending_arrow(arrow),
        _ => false,
    }
}

fn is_undefined_obfuscated_call(call: &CallExpr, scope_data: &ScopeData) -> bool {
    let Callee::Expr(callee) = &call.callee else {
        return false;
    };
    let Expr::Ident(ident) = &**callee else {
        return false;
    };

    let name = ident.sym.as_ref();
    if !name.starts_with("_0x") {
        return false;
    }

    let id = (ident.sym.clone(), ident.ctxt);
    scope_data
        .vars
        .get(&id)
        .is_none_or(|info| !info.declared && !info.exported)
}

fn is_self_defending_arrow(arrow: &ArrowExpr) -> bool {
    let mut scan = SelfDefendingScan::default();
    match &*arrow.body {
        BlockStmtOrExpr::BlockStmt(block) => block.visit_with(&mut scan),
        BlockStmtOrExpr::Expr(expr) => expr.visit_with(&mut scan),
    }
    scan.is_self_defending()
}

fn is_self_defending_function(func: &Function) -> bool {
    let mut scan = SelfDefendingScan::default();
    if let Some(body) = &func.body {
        body.visit_with(&mut scan);
    }
    scan.is_self_defending()
}

#[derive(Default)]
struct SelfDefendingCollector {
    constructors: HashSet<String>,
    self_defending_vars: HashSet<Id>,
    declared_names: HashSet<String>,
}

impl Visit for SelfDefendingCollector {
    fn visit_fn_decl(&mut self, decl: &FnDecl) {
        if is_self_defending_function(&decl.function) {
            let name = decl.ident.sym.to_string();
            self.constructors.insert(name);
            self.self_defending_vars
                .insert((decl.ident.sym.clone(), decl.ident.ctxt));
        }
        self.declared_names.insert(decl.ident.sym.to_string());
        decl.visit_children_with(self);
    }

    fn visit_var_declarator(&mut self, decl: &VarDeclarator) {
        if let Pat::Ident(binding) = &decl.name
            && let Some(init) = &decl.init
            && is_self_defending_expr(init)
        {
            self.constructors.insert(binding.id.sym.to_string());
        }
        if let Pat::Ident(binding) = &decl.name {
            self.declared_names.insert(binding.id.sym.to_string());
        }
        if let Pat::Ident(binding) = &decl.name
            && let Some(init) = &decl.init
            && (is_self_defending_expr(init) || is_self_defending_call_expr(init))
        {
            self.self_defending_vars
                .insert((binding.id.sym.clone(), binding.id.ctxt));
        }
        decl.visit_children_with(self);
    }

    fn visit_class_decl(&mut self, decl: &ClassDecl) {
        self.declared_names.insert(decl.ident.sym.to_string());
        decl.visit_children_with(self);
    }

    fn visit_import_decl(&mut self, decl: &ImportDecl) {
        for specifier in &decl.specifiers {
            match specifier {
                ImportSpecifier::Named(named) => {
                    self.declared_names.insert(named.local.sym.to_string());
                }
                ImportSpecifier::Default(default) => {
                    self.declared_names.insert(default.local.sym.to_string());
                }
                ImportSpecifier::Namespace(ns) => {
                    self.declared_names.insert(ns.local.sym.to_string());
                }
            }
        }
        decl.visit_children_with(self);
    }

    fn visit_assign_expr(&mut self, expr: &AssignExpr) {
        if let Some(base) = prototype_base_name(&expr.left)
            && is_self_defending_expr(&expr.right)
        {
            self.constructors.insert(base);
        }
        if let AssignTarget::Simple(SimpleAssignTarget::Ident(ident)) = &expr.left
            && (is_self_defending_expr(&expr.right) || is_self_defending_call_expr(&expr.right))
        {
            self.self_defending_vars
                .insert((ident.id.sym.clone(), ident.id.ctxt));
        }
        expr.visit_children_with(self);
    }
}

fn prototype_base_name(target: &AssignTarget) -> Option<String> {
    let member = match target {
        AssignTarget::Simple(SimpleAssignTarget::Member(member)) => member,
        _ => return None,
    };

    let Expr::Member(inner) = &*member.obj else {
        return None;
    };

    if let MemberProp::Ident(prop) = &inner.prop {
        if prop.sym.as_ref() != "prototype" {
            return None;
        }
    } else {
        return None;
    }

    if let Expr::Ident(base_ident) = &*inner.obj {
        Some(base_ident.sym.to_string())
    } else {
        None
    }
}

#[derive(Default)]
struct SelfDefendingScan {
    has_regex: bool,
    has_to_string: bool,
    has_search: bool,
    has_regex_like: bool,
    has_constructor: bool,
    has_debugger_str: bool,
    has_state_str: bool,
    has_chain_str: bool,
    has_input_str: bool,
    has_new_state_str: bool,
    has_while_true: bool,
    has_console: bool,
    has_return_this: bool,
    regexp_vars: HashSet<String>,
    has_regexp_negated_or_test: bool,
}

impl SelfDefendingScan {
    fn expr_ident_name(expr: &Expr) -> Option<String> {
        match expr {
            Expr::Ident(ident) => Some(ident.sym.to_string()),
            Expr::Paren(paren) => Self::expr_ident_name(&paren.expr),
            Expr::Seq(seq) => seq.exprs.last().and_then(|e| Self::expr_ident_name(e)),
            _ => None,
        }
    }

    fn is_regexp_init(expr: &Expr) -> bool {
        match expr {
            Expr::Lit(Lit::Regex(_)) => true,
            Expr::New(new_expr) => {
                if let Expr::Ident(ident) = &*new_expr.callee {
                    ident.sym.as_ref() == "RegExp"
                } else {
                    false
                }
            }
            Expr::Call(call) => {
                if let Callee::Expr(callee) = &call.callee
                    && let Expr::Ident(ident) = &**callee
                {
                    ident.sym.as_ref() == "RegExp"
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn is_regexp_method_call(&self, expr: &Expr) -> Option<String> {
        let Expr::Call(call) = expr else {
            return None;
        };
        let Callee::Expr(callee) = &call.callee else {
            return None;
        };
        let Expr::Member(member) = &**callee else {
            return None;
        };
        let obj_name = Self::expr_ident_name(&member.obj)?;
        if self.regexp_vars.contains(&obj_name) {
            Some(obj_name)
        } else {
            None
        }
    }

    fn is_negated_regexp_method_call(&self, expr: &Expr) -> Option<String> {
        let Expr::Unary(unary) = expr else {
            return None;
        };
        if unary.op != UnaryOp::Bang {
            return None;
        }
        self.is_regexp_method_call(&unary.arg)
    }

    const fn is_self_defending(&self) -> bool {
        if self.has_regex
            && (self.has_to_string
                || self.has_constructor
                || self.has_debugger_str
                || self.has_state_str
                || self.has_chain_str
                || self.has_input_str
                || self.has_new_state_str
                || self.has_while_true)
        {
            return true;
        }

        if self.has_debugger_str && (self.has_constructor || self.has_to_string) {
            return true;
        }

        if self.has_debugger_str && self.has_while_true {
            return true;
        }

        if self.has_while_true && (self.has_constructor || self.has_to_string) {
            return true;
        }

        if self.has_to_string && self.has_search && self.has_regex_like {
            return true;
        }

        if self.has_console && self.has_to_string && (self.has_constructor || self.has_return_this)
        {
            return true;
        }

        // Catch self-defending RegExp guard patterns even when method names are
        // obfuscated via computed properties (so "toString"/"search"/etc. are not
        // visible as identifiers or string literals yet).
        if self.has_regex && self.has_regexp_negated_or_test {
            return true;
        }

        false
    }
}

impl Visit for SelfDefendingScan {
    fn visit_fn_decl(&mut self, _decl: &FnDecl) {
        // Skip nested functions to avoid false positives from inner guards.
    }

    fn visit_fn_expr(&mut self, _expr: &FnExpr) {
        // Skip nested functions to avoid false positives from inner guards.
    }

    fn visit_arrow_expr(&mut self, _expr: &ArrowExpr) {
        // Skip nested functions to avoid false positives from inner guards.
    }

    fn visit_lit(&mut self, lit: &Lit) {
        match lit {
            Lit::Regex(_) => {
                self.has_regex = true;
            }
            Lit::Str(s) => {
                if let Some(value) = s.value.as_str() {
                    if value.contains("debugger")
                        || value.contains("debu")
                        || value.contains("gger")
                    {
                        self.has_debugger_str = true;
                    }
                    if value.contains("stateObject") {
                        self.has_state_str = true;
                    }
                    if value.contains("newState") {
                        self.has_new_state_str = true;
                    }
                    if value.contains("chain") {
                        self.has_chain_str = true;
                    }
                    if value.contains("input") {
                        self.has_input_str = true;
                    }
                    if value.contains("while (true)") {
                        self.has_while_true = true;
                    }
                    if value.contains("return this") {
                        self.has_return_this = true;
                    }
                    if value.contains("(((.+)+)+)+$") {
                        self.has_regex_like = true;
                    }
                    if value.contains("constructor") {
                        self.has_constructor = true;
                    }
                    if value.contains("toString") {
                        self.has_to_string = true;
                    }
                }
            }
            _ => {}
        }

        lit.visit_children_with(self);
    }

    fn visit_var_declarator(&mut self, decl: &VarDeclarator) {
        if let Pat::Ident(binding) = &decl.name
            && let Some(init) = &decl.init
            && Self::is_regexp_init(init)
        {
            self.has_regex = true;
            self.regexp_vars.insert(binding.id.sym.to_string());
        }

        decl.visit_children_with(self);
    }

    fn visit_bin_expr(&mut self, expr: &BinExpr) {
        if expr.op == BinaryOp::LogicalOr
            && self.is_negated_regexp_method_call(&expr.left).is_some()
            && self.is_negated_regexp_method_call(&expr.right).is_some()
        {
            self.has_regexp_negated_or_test = true;
        }

        expr.visit_children_with(self);
    }

    fn visit_new_expr(&mut self, expr: &NewExpr) {
        if let Expr::Ident(ident) = &*expr.callee
            && ident.sym.as_ref() == "RegExp"
        {
            self.has_regex = true;
        }
        expr.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, expr: &CallExpr) {
        if let Callee::Expr(callee) = &expr.callee
            && let Expr::Member(member) = &**callee
            && let Some(obj_name) = Self::expr_ident_name(&member.obj)
            && self.regexp_vars.contains(&obj_name)
        {
            self.has_regex = true;
        }
        if let Callee::Expr(callee) = &expr.callee
            && let Expr::Ident(ident) = &**callee
            && ident.sym.as_ref() == "RegExp"
        {
            self.has_regex = true;
        }
        expr.visit_children_with(self);
    }

    fn visit_member_expr(&mut self, expr: &MemberExpr) {
        if let MemberProp::Ident(prop) = &expr.prop {
            let name = prop.sym.as_ref();
            if name == "toString" {
                self.has_to_string = true;
            } else if name == "search" {
                self.has_search = true;
            } else if name == "constructor" {
                self.has_constructor = true;
            } else if name == "console" {
                self.has_console = true;
            }
        }

        expr.visit_children_with(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::deobfuscator::{DeobfuscateOptions, Deobfuscator};

    #[test]
    fn test_self_defending_removal() {
        let deob = Deobfuscator::new();
        let code = r#"
const guard = (function() {
  const re = new RegExp("function *\\( *\\)");
  function f() { return "debugger"; }
  re.test(f.toString());
  return function() {};
}());
guard();
const x = 1;
"#;
        let options = DeobfuscateOptions {
            custom_transformers: Some(vec![Arc::new(SelfDefending::new())]),
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(!result.contains("RegExp"));
        assert!(result.contains("const x") || result.contains("var x"));
    }

    #[test]
    fn test_self_defending_new_instance_call_removed() {
        let deob = Deobfuscator::new();
        let code = r#"
function Guard() {}
Guard.prototype.test = function() {
  const re = new RegExp("function *\\( *\\)");
  re.test("debugger");
};
new Guard().test();
const y = 2;
"#;
        let options = DeobfuscateOptions {
            custom_transformers: Some(vec![Arc::new(SelfDefending::new())]),
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(!result.contains("new Guard"));
        assert!(result.contains("const y") || result.contains("var y"));
    }

    #[test]
    fn test_self_defending_recursive_guard_removed() {
        let deob = Deobfuscator::new();
        let code = r#"
function guard(n) {
  if (typeof n === "string") {
    return function(){}.constructor("while (true) {}").apply("counter");
  } else {
    (function(){ return true; }.constructor("debugger").call("action"));
  }
  guard(++n);
}
guard(0);
"#;
        let options = DeobfuscateOptions {
            custom_transformers: Some(vec![Arc::new(SelfDefending::new())]),
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(!result.contains("guard(0)"));
    }

    #[test]
    fn test_self_defending_wrapped_arg_removed() {
        let deob = Deobfuscator::new();
        let code = r#"
const wrap = function(_ctx, fn) { return fn; };
const guard = wrap(this, (0, function() {
  return guard.toString().search("(((.+)+)+)+$");
}));
guard();
const ok = 1;
"#;
        let options = DeobfuscateOptions {
            custom_transformers: Some(vec![Arc::new(SelfDefending::new())]),
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(!result.contains("guard()"));
        assert!(result.contains("const ok") || result.contains("var ok"));
    }

    #[test]
    fn test_self_defending_regexp_guard_pattern_removed() {
        let deob = Deobfuscator::new();
        let code = r#"
const wrap = function(ctx, fn) {
  return function() {
    return fn.apply(ctx, arguments);
  };
};
wrap(this, function() {
  const re = new RegExp("a");
  const re2 = new RegExp("b", "i");
  if (!re["test"]("x") || !re2["test"]("y")) {
    return;
  }
})();
const ok = 1;
"#;
        let options = DeobfuscateOptions {
            custom_transformers: Some(vec![Arc::new(SelfDefending::new())]),
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(!result.contains("wrap(this"));
        assert!(result.contains("const ok") || result.contains("var ok"));
    }
}
