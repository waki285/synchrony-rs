//! DeadCode transformer
//!
//! This transformer removes dead code from the AST:
//! - Remove if(false) branches
//! - Flip if(false) { A } else { B } to if(true) { B } else { A }
//! - Remove dead alternates when test is true
//! - Inline if(true) { ... } blocks
//! - Remove unused obfuscated declarations that are side-effect free

use std::collections::{HashMap, HashSet};
use swc_common::{GLOBALS, Span};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitMut, VisitMutWith, VisitWith};

use crate::context::Context;
use crate::error::Result;
use crate::scope::{Id, analyze};
use crate::transformers::Transformer;

/// DeadCode transformer - removes unreachable code.
///
/// Also prunes unused obfuscated bindings that are side-effect free.
#[derive(Debug)]
pub struct DeadCode;

impl DeadCode {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for DeadCode {
    fn default() -> Self {
        Self::new()
    }
}

/// DeadCodeSafe transformer - removes only unused declarations with pure initializers.
///
/// This is intended for a post-rename cleanup where we want minimal risk.
#[derive(Debug)]
pub struct DeadCodeSafe;

impl DeadCodeSafe {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for DeadCodeSafe {
    fn default() -> Self {
        Self::new()
    }
}

impl Transformer for DeadCode {
    fn name(&self) -> &'static str {
        "DeadCode"
    }

    fn transform(&self, context: &mut Context) -> Result<()> {
        // Run multiple passes
        let mut visitor = DeadCodeVisitor;
        context.ast.visit_mut_with(&mut visitor);

        // Run block cleanup
        let mut block_visitor = BlockCleanupVisitor;
        context.ast.visit_mut_with(&mut block_visitor);

        if context.remove_garbage && !context.rename_enabled {
            // Remove unused obfuscated identifiers after simplifications.
            for _ in 0..3 {
                let scope_data = GLOBALS.set(&Default::default(), || analyze(&context.ast));
                if scope_data.top.has_with_stmt || scope_data.top.has_eval_call {
                    break;
                }

                let mut remover = ObfuscatedDeadCodeRemover::new(&scope_data);
                context.ast.visit_mut_with(&mut remover);
                if !remover.changed {
                    break;
                }
            }
        }

        if context.remove_garbage {
            let scope_data = GLOBALS.set(&Default::default(), || analyze(&context.ast));
            if !scope_data.top.has_with_stmt && !scope_data.top.has_eval_call {
                let mut remover = UndefinedObfuscatedCallRemover::new(&scope_data);
                context.ast.visit_mut_with(&mut remover);
            }
        }

        // Note: removeDeadVariables is disabled by default in the original TypeScript implementation
        // Uncomment the following to enable dead variable removal:
        // if context.remove_garbage {
        //     self.remove_dead_variables(context)?;
        // }

        Ok(())
    }
}

impl Transformer for DeadCodeSafe {
    fn name(&self) -> &'static str {
        "DeadCodeSafe"
    }

    fn transform(&self, context: &mut Context) -> Result<()> {
        if !context.remove_garbage {
            return Ok(());
        }

        for _ in 0..3 {
            let scope_data = GLOBALS.set(&Default::default(), || analyze(&context.ast));
            if scope_data.top.has_with_stmt || scope_data.top.has_eval_call {
                break;
            }

            let mut decl_spans = DeclarationSpanCollector::default();
            context.ast.visit_with(&mut decl_spans);

            let mut usage =
                DirectUsageCollector::new(&decl_spans.decl_spans, context.source.as_deref());
            context.ast.visit_with(&mut usage);

            let mut remover = UnusedDeclarationRemover::new(&scope_data, &usage.counts);
            context.ast.visit_mut_with(&mut remover);

            let mut iife_remover = SafeEmptyIifeRemover::default();
            context.ast.visit_mut_with(&mut iife_remover);

            if !remover.changed && !iife_remover.changed {
                break;
            }
        }
        Ok(())
    }
}

impl DeadCode {
    /// Remove unreferenced variables
    /// Note: This is disabled by default in the original TypeScript implementation
    #[expect(dead_code)]
    fn remove_dead_variables(&self, context: &mut Context) -> Result<()> {
        // Analyze variable usage
        let scope_data = GLOBALS.set(&Default::default(), || analyze(&context.ast));

        // Find dead variables (declared but never referenced)
        let mut dead_vars: HashSet<Id> = HashSet::new();

        for (id, var_info) in &scope_data.vars {
            // Skip 'arguments'
            if id.0.as_str() == "arguments" {
                continue;
            }

            // A variable is dead if it's declared but never used (ref_count == 0)
            // Note: we check usage_count instead since ref_count includes the declaration
            if var_info.declared && var_info.ref_count == 0 {
                dead_vars.insert(id.clone());
            }
        }

        // Remove dead variable declarations
        let mut remover = DeadVariableRemover {
            dead_vars: &dead_vars,
        };
        context.ast.visit_mut_with(&mut remover);

        Ok(())
    }
}

/// Check if an expression is a boolean literal with a specific value
#[must_use]
const fn is_bool_literal(expr: &Expr, value: bool) -> bool {
    matches!(expr, Expr::Lit(Lit::Bool(b)) if b.value == value)
}

/// Check if an expression is a boolean literal
#[must_use]
const fn get_bool_value(expr: &Expr) -> Option<bool> {
    match expr {
        Expr::Lit(Lit::Bool(b)) => Some(b.value),
        _ => None,
    }
}

/// Visitor that performs dead code elimination
struct DeadCodeVisitor;

impl VisitMut for DeadCodeVisitor {
    fn visit_mut_stmt(&mut self, stmt: &mut Stmt) {
        // First, visit children
        stmt.visit_mut_children_with(self);

        // Handle if statements
        if let Stmt::If(if_stmt) = stmt {
            // Get the test value
            if let Some(test_value) = get_bool_value(&if_stmt.test) {
                if test_value {
                    // if(true) { A } else { B } -> A
                    // Remove the alternate
                    if_stmt.alt = None;
                } else {
                    // if(false) { A } else { B } -> if(true) { B }
                    // Flip the branches
                    if let Some(alt) = if_stmt.alt.take() {
                        *if_stmt.test = Expr::Lit(Lit::Bool(Bool {
                            span: Default::default(),
                            value: true,
                        }));
                        if_stmt.cons = alt;
                        if_stmt.alt = None;
                    } else {
                        // if(false) { A } with no else -> empty
                        *stmt = Stmt::Empty(EmptyStmt {
                            span: Default::default(),
                        });
                        return;
                    }
                }
            }
        }

        // Handle while(false) -> remove
        if let Stmt::While(while_stmt) = stmt
            && is_bool_literal(&while_stmt.test, false)
        {
            *stmt = Stmt::Empty(EmptyStmt {
                span: Default::default(),
            });
        }

        if let Stmt::Expr(expr_stmt) = stmt
            && let Some(call) = extract_call_expr(&expr_stmt.expr)
            && is_empty_iife_call(call)
        {
            *stmt = Stmt::Empty(EmptyStmt {
                span: Default::default(),
            });
        }
    }
}

/// Visitor that cleans up blocks by:
/// - Removing empty statements
/// - Inlining if(true) blocks into parent
/// - Removing empty variable declarations
struct BlockCleanupVisitor;

impl VisitMut for BlockCleanupVisitor {
    fn visit_mut_stmts(&mut self, stmts: &mut Vec<Stmt>) {
        // First, visit children
        stmts.iter_mut().for_each(|stmt| stmt.visit_mut_with(self));

        // Remove empty statements and empty variable declarations
        stmts.retain(|stmt| {
            match stmt {
                Stmt::Empty(_) => false,
                Stmt::Decl(Decl::Var(var_decl)) => {
                    // Remove variable declarations with no declarators
                    !var_decl.decls.is_empty()
                }
                _ => true,
            }
        });

        // Inline if(true) { ... } blocks
        let mut i = 0;
        while i < stmts.len() {
            if let Stmt::If(if_stmt) = &stmts[i]
                && is_bool_literal(&if_stmt.test, true)
                && if_stmt.alt.is_none()
                && let Stmt::Block(block) = &*if_stmt.cons
            {
                // Replace if(true) { stmts... } with stmts...
                let block_stmts = block.stmts.clone();
                stmts.splice(i..=i, block_stmts);
                continue; // Don't increment i, check the new statement at this position
            }
            i += 1;
        }
    }

    fn visit_mut_module_items(&mut self, items: &mut Vec<ModuleItem>) {
        // First, visit children
        items.iter_mut().for_each(|item| item.visit_mut_with(self));

        // Remove empty statements and empty variable declarations
        items.retain(|item| {
            match item {
                ModuleItem::Stmt(Stmt::Empty(_)) => false,
                ModuleItem::Stmt(Stmt::Decl(Decl::Var(var_decl))) => {
                    // Remove variable declarations with no declarators
                    !var_decl.decls.is_empty()
                }
                _ => true,
            }
        });
    }
}

struct ObfuscatedDeadCodeRemover<'a> {
    scope_data: &'a crate::scope::ScopeData,
    changed: bool,
}

impl<'a> ObfuscatedDeadCodeRemover<'a> {
    fn new(scope_data: &'a crate::scope::ScopeData) -> Self {
        Self {
            scope_data,
            changed: false,
        }
    }

    fn is_obfuscated_name(name: &str) -> bool {
        name.starts_with("_0x")
    }

    fn is_unused(&self, ident: &Ident) -> bool {
        let id = (ident.sym.clone(), ident.ctxt);
        self.scope_data
            .vars
            .get(&id)
            .map(|info| info.is_unused() && !info.exported)
            .unwrap_or(false)
    }
}

impl VisitMut for ObfuscatedDeadCodeRemover<'_> {
    fn visit_mut_stmt(&mut self, stmt: &mut Stmt) {
        stmt.visit_mut_children_with(self);

        if let Stmt::Expr(expr_stmt) = stmt
            && is_obfuscated_pure_iife_expr(&expr_stmt.expr)
        {
            self.changed = true;
            *stmt = Stmt::Empty(EmptyStmt {
                span: Default::default(),
            });
        }
    }

    fn visit_mut_var_decl(&mut self, decl: &mut VarDecl) {
        decl.visit_mut_children_with(self);

        decl.decls.retain(|declarator| {
            let Pat::Ident(binding) = &declarator.name else {
                return true;
            };
            let name = binding.id.sym.as_ref();
            if !Self::is_obfuscated_name(name) || !self.is_unused(&binding.id) {
                return true;
            }

            match &declarator.init {
                None => {
                    self.changed = true;
                    false
                }
                Some(init) if is_pure_expr(init) => {
                    self.changed = true;
                    false
                }
                _ => true,
            }
        });
    }

    fn visit_mut_module_items(&mut self, items: &mut Vec<ModuleItem>) {
        items
            .iter_mut()
            .for_each(|item| item.visit_mut_children_with(self));

        items.retain(|item| match item {
            ModuleItem::Stmt(Stmt::Decl(Decl::Fn(fn_decl))) => {
                let name = fn_decl.ident.sym.as_ref();
                if Self::is_obfuscated_name(name) && self.is_unused(&fn_decl.ident) {
                    self.changed = true;
                    false
                } else {
                    true
                }
            }
            ModuleItem::Stmt(Stmt::Empty(_)) => false,
            ModuleItem::Stmt(Stmt::Decl(Decl::Var(var_decl))) => !var_decl.decls.is_empty(),
            _ => true,
        });
    }

    fn visit_mut_stmts(&mut self, stmts: &mut Vec<Stmt>) {
        stmts
            .iter_mut()
            .for_each(|stmt| stmt.visit_mut_children_with(self));

        stmts.retain(|stmt| match stmt {
            Stmt::Decl(Decl::Fn(fn_decl)) => {
                let name = fn_decl.ident.sym.as_ref();
                if Self::is_obfuscated_name(name) && self.is_unused(&fn_decl.ident) {
                    self.changed = true;
                    false
                } else {
                    true
                }
            }
            Stmt::Empty(_) => false,
            Stmt::Decl(Decl::Var(var_decl)) => !var_decl.decls.is_empty(),
            _ => true,
        });
    }
}

#[derive(Default)]
struct DeclarationSpanCollector {
    decl_spans: HashMap<Id, HashSet<Span>>,
}

impl Visit for DeclarationSpanCollector {
    fn visit_binding_ident(&mut self, ident: &BindingIdent) {
        let id: Id = (ident.id.sym.clone(), ident.id.ctxt);
        self.decl_spans.entry(id).or_default().insert(ident.id.span);
        if let Ok(name) = std::env::var("SYNCHRONY_DEBUG_DEADCODE_NAME")
            && name == ident.id.sym.as_ref()
        {
            crate::log_info!(
                "[DeadCodeSafe] decl span for '{}': lo={} hi={}",
                ident.id.sym,
                ident.id.span.lo.0,
                ident.id.span.hi.0
            );
        }
    }
}

struct DirectUsageCollector<'a> {
    counts: HashMap<Id, u32>,
    decl_spans: &'a HashMap<Id, HashSet<Span>>,
    source: Option<&'a str>,
}

impl<'a> DirectUsageCollector<'a> {
    fn new(decl_spans: &'a HashMap<Id, HashSet<Span>>, source: Option<&'a str>) -> Self {
        Self {
            counts: HashMap::new(),
            decl_spans,
            source,
        }
    }
}

impl Visit for DirectUsageCollector<'_> {
    fn visit_pat(&mut self, pat: &Pat) {
        match pat {
            Pat::Ident(_) => {}
            _ => pat.visit_children_with(self),
        }
    }

    fn visit_ident(&mut self, ident: &Ident) {
        if let Some(spans) = self.decl_spans.get(&(ident.sym.clone(), ident.ctxt))
            && spans.contains(&ident.span)
        {
            return;
        }
        if let Ok(name) = std::env::var("SYNCHRONY_DEBUG_DEADCODE_NAME")
            && name == ident.sym.as_ref()
        {
            crate::log_info!(
                "[DeadCodeSafe] use span for '{}': lo={} hi={}",
                ident.sym,
                ident.span.lo.0,
                ident.span.hi.0
            );
            if let Some(source) = self.source {
                let lo = ident.span.lo.0 as usize;
                let hi = ident.span.hi.0 as usize;
                if lo < source.len() && hi <= source.len() && lo < hi {
                    let start = lo.saturating_sub(40);
                    let end = (hi + 40).min(source.len());
                    let snippet = &source[start..end];
                    crate::log_info!(
                        "[DeadCodeSafe] use snippet: {}",
                        snippet.replace('\n', "\\n")
                    );
                }
            }
        }
        let id: Id = (ident.sym.clone(), ident.ctxt);
        *self.counts.entry(id).or_insert(0) += 1;
    }

    fn visit_binding_ident(&mut self, _ident: &BindingIdent) {}

    fn visit_labeled_stmt(&mut self, stmt: &LabeledStmt) {
        stmt.body.visit_with(self);
    }

    fn visit_break_stmt(&mut self, _stmt: &BreakStmt) {}

    fn visit_continue_stmt(&mut self, _stmt: &ContinueStmt) {}
}

struct UnusedDeclarationRemover<'a> {
    scope_data: &'a crate::scope::ScopeData,
    direct_usage: &'a HashMap<Id, u32>,
    in_for_in_of_head: bool,
    debug_name: Option<String>,
    changed: bool,
}

#[derive(Default)]
struct SafeEmptyIifeRemover {
    changed: bool,
}

impl VisitMut for SafeEmptyIifeRemover {
    fn visit_mut_stmt(&mut self, stmt: &mut Stmt) {
        stmt.visit_mut_children_with(self);

        if let Stmt::Expr(expr_stmt) = stmt
            && let Some(call) = extract_call_expr(&expr_stmt.expr)
            && is_empty_iife_call(call)
        {
            *stmt = Stmt::Empty(EmptyStmt {
                span: Default::default(),
            });
            self.changed = true;
        }
    }
}

impl<'a> UnusedDeclarationRemover<'a> {
    fn new(scope_data: &'a crate::scope::ScopeData, direct_usage: &'a HashMap<Id, u32>) -> Self {
        Self {
            scope_data,
            direct_usage,
            in_for_in_of_head: false,
            debug_name: std::env::var("SYNCHRONY_DEBUG_DEADCODE_NAME").ok(),
            changed: false,
        }
    }

    fn is_unused_decl(&self, ident: &Ident) -> bool {
        if ident.sym.as_ref() == "arguments" {
            return false;
        }
        let id = (ident.sym.clone(), ident.ctxt);
        let Some(info) = self.scope_data.vars.get(&id) else {
            return false;
        };
        if !info.declared || info.exported {
            return false;
        }
        if info.declared_as_fn_param || info.declared_as_for_init {
            return false;
        }
        let direct_count = self
            .direct_usage
            .get(&(ident.sym.clone(), ident.ctxt))
            .copied()
            .unwrap_or(0);
        let unused = info.is_unused() || direct_count == 0;
        if self
            .debug_name
            .as_ref()
            .is_some_and(|name| name == ident.sym.as_ref())
        {
            crate::log_info!(
                "[DeadCodeSafe] '{}' usage: declared={} exported={} ref_count={} usage_count={} assign_count={} used_as_ref={} used_as_arg={} used_as_callee={} declared_as_fn_param={} declared_as_for_init={} direct_usage_count={} unused={}",
                ident.sym,
                info.declared,
                info.exported,
                info.ref_count,
                info.usage_count,
                info.assign_count,
                info.used_as_ref,
                info.used_as_arg,
                info.used_as_callee,
                info.declared_as_fn_param,
                info.declared_as_for_init,
                direct_count,
                unused
            );
        }
        unused
    }
}

impl VisitMut for UnusedDeclarationRemover<'_> {
    fn visit_mut_for_stmt(&mut self, stmt: &mut ForStmt) {
        stmt.visit_mut_children_with(self);

        if let Some(VarDeclOrExpr::VarDecl(var_decl)) = &mut stmt.init
            && var_decl.decls.is_empty()
        {
            stmt.init = None;
        }
    }

    fn visit_mut_for_in_stmt(&mut self, stmt: &mut ForInStmt) {
        stmt.right.visit_mut_with(self);
        stmt.body.visit_mut_with(self);

        match &mut stmt.left {
            ForHead::VarDecl(var_decl) => {
                let prev = self.in_for_in_of_head;
                self.in_for_in_of_head = true;
                var_decl.visit_mut_with(self);
                self.in_for_in_of_head = prev;
            }
            ForHead::UsingDecl(using_decl) => {
                using_decl.visit_mut_with(self);
            }
            ForHead::Pat(pat) => {
                pat.visit_mut_with(self);
            }
        }
    }

    fn visit_mut_for_of_stmt(&mut self, stmt: &mut ForOfStmt) {
        stmt.right.visit_mut_with(self);
        stmt.body.visit_mut_with(self);

        match &mut stmt.left {
            ForHead::VarDecl(var_decl) => {
                let prev = self.in_for_in_of_head;
                self.in_for_in_of_head = true;
                var_decl.visit_mut_with(self);
                self.in_for_in_of_head = prev;
            }
            ForHead::UsingDecl(using_decl) => {
                using_decl.visit_mut_with(self);
            }
            ForHead::Pat(pat) => {
                pat.visit_mut_with(self);
            }
        }
    }

    fn visit_mut_var_decl(&mut self, decl: &mut VarDecl) {
        decl.visit_mut_children_with(self);

        if self.in_for_in_of_head {
            return;
        }

        decl.decls.retain(|declarator| {
            let Pat::Ident(binding) = &declarator.name else {
                return true;
            };
            let unused = self.is_unused_decl(&binding.id);
            if !unused {
                return true;
            }

            match &declarator.init {
                None => {
                    if self
                        .debug_name
                        .as_ref()
                        .is_some_and(|name| name == binding.id.sym.as_ref())
                    {
                        crate::log_info!(
                            "[DeadCodeSafe] '{}' removed: unused and no initializer",
                            binding.id.sym
                        );
                    }
                    self.changed = true;
                    false
                }
                Some(init) if is_pure_expr(init) => {
                    if self
                        .debug_name
                        .as_ref()
                        .is_some_and(|name| name == binding.id.sym.as_ref())
                    {
                        crate::log_info!(
                            "[DeadCodeSafe] '{}' removed: unused and pure initializer",
                            binding.id.sym
                        );
                    }
                    self.changed = true;
                    false
                }
                _ => {
                    if self
                        .debug_name
                        .as_ref()
                        .is_some_and(|name| name == binding.id.sym.as_ref())
                    {
                        crate::log_info!(
                            "[DeadCodeSafe] '{}' kept: initializer not pure",
                            binding.id.sym
                        );
                    }
                    true
                }
            }
        });
    }

    fn visit_mut_module_items(&mut self, items: &mut Vec<ModuleItem>) {
        items
            .iter_mut()
            .for_each(|item| item.visit_mut_children_with(self));

        items.retain(|item| match item {
            ModuleItem::Stmt(Stmt::Empty(_)) => false,
            ModuleItem::Stmt(Stmt::Decl(Decl::Var(var_decl))) => !var_decl.decls.is_empty(),
            ModuleItem::Stmt(Stmt::Decl(Decl::Fn(fn_decl))) => !self.is_unused_decl(&fn_decl.ident),
            ModuleItem::Stmt(Stmt::Decl(Decl::Class(class_decl))) => {
                !self.is_unused_decl(&class_decl.ident)
            }
            _ => true,
        });
    }

    fn visit_mut_stmts(&mut self, stmts: &mut Vec<Stmt>) {
        stmts
            .iter_mut()
            .for_each(|stmt| stmt.visit_mut_children_with(self));

        stmts.retain(|stmt| match stmt {
            Stmt::Empty(_) => false,
            Stmt::Decl(Decl::Var(var_decl)) => !var_decl.decls.is_empty(),
            Stmt::Decl(Decl::Fn(fn_decl)) => !self.is_unused_decl(&fn_decl.ident),
            Stmt::Decl(Decl::Class(class_decl)) => !self.is_unused_decl(&class_decl.ident),
            _ => true,
        });
    }
}

struct UndefinedObfuscatedCallRemover<'a> {
    scope_data: &'a crate::scope::ScopeData,
}

impl<'a> UndefinedObfuscatedCallRemover<'a> {
    fn new(scope_data: &'a crate::scope::ScopeData) -> Self {
        Self { scope_data }
    }

    fn is_undefined_obfuscated_ident(&self, ident: &Ident) -> bool {
        let name = ident.sym.as_ref();
        if !name.starts_with("_0x") {
            return false;
        }
        let id = (ident.sym.clone(), ident.ctxt);
        match self.scope_data.vars.get(&id) {
            None => true,
            Some(info) => !info.declared && !info.exported,
        }
    }
}

impl VisitMut for UndefinedObfuscatedCallRemover<'_> {
    fn visit_mut_stmt(&mut self, stmt: &mut Stmt) {
        stmt.visit_mut_children_with(self);

        if let Stmt::Expr(expr_stmt) = stmt
            && let Expr::Call(call) = &*expr_stmt.expr
            && let Callee::Expr(callee) = &call.callee
            && let Expr::Ident(ident) = &**callee
            && self.is_undefined_obfuscated_ident(ident)
        {
            *stmt = Stmt::Empty(EmptyStmt {
                span: Default::default(),
            });
        }
    }
}

#[must_use]
fn is_obfuscated_pure_iife_expr(expr: &Expr) -> bool {
    if let Expr::Call(call) = expr {
        return is_obfuscated_pure_iife_call(call);
    }
    false
}

#[must_use]
fn is_pure_iife_call(call: &CallExpr) -> bool {
    if !call.args.iter().all(|arg| is_pure_expr(&arg.expr)) {
        return false;
    }

    match extract_iife_callee(&call.callee) {
        Some(IifeCallee::Function(func)) => func
            .body
            .as_ref()
            .map(|body| is_pure_stmt_list(&body.stmts))
            .unwrap_or(true),
        Some(IifeCallee::Arrow(arrow)) => match &*arrow.body {
            BlockStmtOrExpr::BlockStmt(block) => is_pure_stmt_list(&block.stmts),
            BlockStmtOrExpr::Expr(expr) => is_pure_expr(expr),
        },
        None => false,
    }
}

#[must_use]
fn is_obfuscated_pure_iife_call(call: &CallExpr) -> bool {
    if !is_pure_iife_call(call) {
        return false;
    }

    match extract_iife_callee(&call.callee) {
        Some(IifeCallee::Function(func)) => func
            .body
            .as_ref()
            .map(|body| body_contains_obfuscated_ident(&body.stmts))
            .unwrap_or(false),
        Some(IifeCallee::Arrow(arrow)) => match &*arrow.body {
            BlockStmtOrExpr::BlockStmt(block) => body_contains_obfuscated_ident(&block.stmts),
            BlockStmtOrExpr::Expr(expr) => expr_contains_obfuscated_ident(expr),
        },
        None => false,
    }
}

enum IifeCallee<'a> {
    Function(&'a Function),
    Arrow(&'a ArrowExpr),
}

#[must_use]
fn extract_iife_callee<'a>(callee: &'a Callee) -> Option<IifeCallee<'a>> {
    match callee {
        Callee::Expr(expr) => extract_iife_expr(expr),
        _ => None,
    }
}

#[must_use]
fn extract_iife_expr<'a>(expr: &'a Expr) -> Option<IifeCallee<'a>> {
    match expr {
        Expr::Fn(fn_expr) => Some(IifeCallee::Function(&fn_expr.function)),
        Expr::Arrow(arrow) => Some(IifeCallee::Arrow(arrow)),
        Expr::Paren(paren) => extract_iife_expr(&paren.expr),
        Expr::Seq(seq) => {
            if let Some(last) = seq.exprs.last() {
                if seq.exprs[..seq.exprs.len().saturating_sub(1)]
                    .iter()
                    .all(|expr| is_pure_expr(expr))
                {
                    extract_iife_expr(last)
                } else {
                    None
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

#[must_use]
fn is_pure_stmt_list(stmts: &[Stmt]) -> bool {
    stmts.iter().all(is_pure_stmt)
}

#[must_use]
fn is_pure_stmt(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Empty(_) => true,
        Stmt::Decl(Decl::Var(var_decl)) => var_decl.decls.iter().all(|decl| {
            decl.init
                .as_ref()
                .map(|init| is_pure_expr(init))
                .unwrap_or(true)
        }),
        Stmt::Decl(Decl::Fn(_)) => true,
        Stmt::Block(block) => is_pure_stmt_list(&block.stmts),
        Stmt::Return(ret) => ret
            .arg
            .as_ref()
            .map(|arg| is_pure_expr(arg))
            .unwrap_or(true),
        _ => false,
    }
}

#[must_use]
fn is_pure_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Ident(_) => true,
        Expr::Lit(_) => true,
        Expr::Fn(_) | Expr::Arrow(_) => true,
        Expr::Paren(paren) => is_pure_expr(&paren.expr),
        Expr::Unary(unary) => {
            matches!(
                unary.op,
                UnaryOp::Plus | UnaryOp::Minus | UnaryOp::Bang | UnaryOp::Tilde
            ) && is_pure_expr(&unary.arg)
        }
        Expr::Array(arr) => arr
            .elems
            .iter()
            .flatten()
            .all(|elem| is_pure_expr(&elem.expr)),
        Expr::Object(obj) => obj.props.iter().all(|prop| match prop {
            PropOrSpread::Prop(prop) => match &**prop {
                Prop::KeyValue(kv) => is_pure_expr(&kv.value),
                Prop::Method(_) | Prop::Getter(_) | Prop::Setter(_) => true,
                Prop::Shorthand(_) => true,
                _ => false,
            },
            _ => false,
        }),
        Expr::Call(call) => is_pure_iife_call(call),
        _ => false,
    }
}

#[must_use]
fn is_empty_iife_call(call: &CallExpr) -> bool {
    if !call.args.iter().all(|arg| is_pure_expr(&arg.expr)) {
        return false;
    }

    match extract_iife_callee(&call.callee) {
        Some(IifeCallee::Function(func)) => func
            .body
            .as_ref()
            .map(|body| body.stmts.iter().all(|stmt| matches!(stmt, Stmt::Empty(_))))
            .unwrap_or(true),
        Some(IifeCallee::Arrow(arrow)) => match &*arrow.body {
            BlockStmtOrExpr::BlockStmt(block) => block
                .stmts
                .iter()
                .all(|stmt| matches!(stmt, Stmt::Empty(_))),
            BlockStmtOrExpr::Expr(_) => false,
        },
        None => false,
    }
}

#[must_use]
fn extract_call_expr(expr: &Expr) -> Option<&CallExpr> {
    match expr {
        Expr::Call(call) => Some(call),
        Expr::Paren(paren) => extract_call_expr(&paren.expr),
        _ => None,
    }
}

#[must_use]
fn body_contains_obfuscated_ident(stmts: &[Stmt]) -> bool {
    let mut finder = ObfuscatedIdentFinder::default();
    for stmt in stmts {
        stmt.visit_with(&mut finder);
        if finder.found {
            return true;
        }
    }
    false
}

#[must_use]
fn expr_contains_obfuscated_ident(expr: &Expr) -> bool {
    let mut finder = ObfuscatedIdentFinder::default();
    expr.visit_with(&mut finder);
    finder.found
}

#[derive(Default)]
struct ObfuscatedIdentFinder {
    found: bool,
}

impl Visit for ObfuscatedIdentFinder {
    fn visit_ident(&mut self, ident: &Ident) {
        if ident.sym.as_ref().starts_with("_0x") {
            self.found = true;
        }
    }

    fn visit_binding_ident(&mut self, ident: &BindingIdent) {
        if ident.id.sym.as_ref().starts_with("_0x") {
            self.found = true;
        }
    }
}

/// Visitor to remove dead variable declarations
struct DeadVariableRemover<'a> {
    dead_vars: &'a HashSet<Id>,
}

impl<'a> VisitMut for DeadVariableRemover<'a> {
    fn visit_mut_var_decl(&mut self, decl: &mut VarDecl) {
        decl.visit_mut_children_with(self);

        // Remove dead variable declarations
        decl.decls.retain(|declarator| {
            if let Pat::Ident(binding) = &declarator.name {
                let id: Id = (binding.id.sym.clone(), binding.id.ctxt);
                // Keep if not in dead_vars set
                !self.dead_vars.contains(&id)
            } else {
                true
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_bool_literal() {
        let true_lit = Expr::Lit(Lit::Bool(Bool {
            span: Default::default(),
            value: true,
        }));
        let false_lit = Expr::Lit(Lit::Bool(Bool {
            span: Default::default(),
            value: false,
        }));
        let num_lit = Expr::Lit(Lit::Num(Number {
            span: Default::default(),
            value: 1.0,
            raw: None,
        }));

        assert!(is_bool_literal(&true_lit, true));
        assert!(!is_bool_literal(&true_lit, false));
        assert!(is_bool_literal(&false_lit, false));
        assert!(!is_bool_literal(&false_lit, true));
        assert!(!is_bool_literal(&num_lit, true));
        assert!(!is_bool_literal(&num_lit, false));
    }
}
