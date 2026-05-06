//! Scope analysis helpers.
//!
//! This module exposes a lightweight `ScopeData` view used by transformers for
//! safe cleanups. The implementation is intentionally conservative: if we are
//! unsure whether something is a safe local binding, we keep it marked as used.

use std::collections::HashMap;

use swc_atoms::Atom;
use swc_common::SyntaxContext;
use swc_ecma_ast::*;
use swc_ecma_usage_analyzer::alias::Access;
use swc_ecma_utils::{Type, Value};
use swc_ecma_visit::{Visit, VisitWith};

/// Variable identifier (name, syntax context)
pub type Id = (Atom, SyntaxContext);

/// Analyzed program data
#[derive(Debug, Default)]
pub struct ScopeData {
    /// Variable usage information
    pub vars: HashMap<Id, VarUsageInfo>,
    /// Top-level scope data
    pub top: ScopeInfo,
    /// Child scopes
    pub scopes: Vec<ScopeInfo>,
    /// Initialized variables
    pub initialized_vars: Vec<Id>,
}

/// Information about a scope
#[derive(Debug, Default, Clone)]
pub struct ScopeInfo {
    /// Whether the scope contains a `with` statement.
    pub has_with_stmt: bool,
    /// Whether the scope contains an `eval` call.
    pub has_eval_call: bool,
    /// Whether the scope references `arguments`.
    pub used_arguments: bool,
}

/// Information about variable usage
#[derive(Debug, Default, Clone)]
pub struct VarUsageInfo {
    /// Number of references to this variable
    pub ref_count: u32,
    /// Number of assignments to this variable
    pub assign_count: u32,
    /// Number of times used (including indirect)
    pub usage_count: u32,
    /// Is this variable declared?
    pub declared: bool,
    /// Declared as function parameter?
    pub declared_as_fn_param: bool,
    /// Declared as function declaration?
    pub declared_as_fn_decl: bool,
    /// Declared as function expression?
    pub declared_as_fn_expr: bool,
    /// Declared in for loop init?
    pub declared_as_for_init: bool,
    /// Has property access on this variable?
    pub has_property_access: bool,
    /// Used as callee?
    pub used_as_callee: bool,
    /// Used as argument?
    pub used_as_arg: bool,
    /// Used as reference?
    pub used_as_ref: bool,
    /// Prevent inlining?
    pub inline_prevented: bool,
    /// Is exported?
    pub exported: bool,
    /// Initialized with safe value?
    pub initialized_with_safe_value: bool,
    /// Is pure function?
    pub pure_fn: bool,
    /// Used above declaration?
    pub used_above_decl: bool,
    /// Used recursively?
    pub used_recursively: bool,
    /// Lazy initialization?
    pub lazy_init: bool,
    /// Indexed with dynamic key?
    pub indexed_with_dynamic_key: bool,
    /// Used as JSX callee?
    pub used_as_jsx_callee: bool,
    /// Variable declaration kind
    pub var_kind: Option<VarDeclKind>,
    /// Merged variable type
    pub merged_var_type: Option<Value<Type>>,
    /// Infects to other variables
    pub infects_to: Vec<Access>,
    /// Accessed properties
    pub accessed_props: HashMap<Atom, u32>,
}

impl VarUsageInfo {
    /// Check if this variable is read-only (only read, never assigned after init)
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.declared && self.assign_count <= 1 && !self.used_as_ref
    }

    /// Check if this variable is unused
    #[must_use]
    pub const fn is_unused(&self) -> bool {
        self.ref_count == 0 && self.usage_count == 0
    }
}

#[derive(Copy, Clone)]
enum DeclKind {
    Var(VarDeclKind),
    Function,
    FunctionExpr,
    Class,
    ClassExpr,
    Param,
    Import,
}

struct BindingCollector {
    ids: Vec<Id>,
}

impl BindingCollector {
    fn collect(pat: &Pat) -> Vec<Id> {
        let mut collector = Self { ids: Vec::new() };
        pat.visit_with(&mut collector);
        collector.ids
    }

    fn collect_assign_target_pat(pat: &AssignTargetPat) -> Vec<Id> {
        let mut collector = Self { ids: Vec::new() };
        pat.visit_with(&mut collector);
        collector.ids
    }
}

impl Visit for BindingCollector {
    fn visit_binding_ident(&mut self, ident: &BindingIdent) {
        self.ids.push((ident.id.sym.clone(), ident.id.ctxt));
    }

    fn visit_assign_pat_prop(&mut self, prop: &AssignPatProp) {
        self.ids.push((prop.key.sym.clone(), prop.key.ctxt));
    }

    fn visit_expr(&mut self, _expr: &Expr) {}
}

#[derive(Debug)]
#[doc(hidden)]
pub struct ScopeAnalyzer {
    data: ScopeData,
    in_call_arg: bool,
    in_callee: bool,
}

impl ScopeAnalyzer {
    fn new() -> Self {
        Self {
            data: ScopeData::default(),
            in_call_arg: false,
            in_callee: false,
        }
    }

    fn finish(self) -> ScopeData {
        self.data
    }

    fn ensure_var(&mut self, id: Id) -> &mut VarUsageInfo {
        self.data.vars.entry(id).or_default()
    }

    fn ensure_ident(&mut self, ident: &Ident) -> &mut VarUsageInfo {
        self.ensure_var((ident.sym.clone(), ident.ctxt))
    }

    fn declare_id(&mut self, id: Id, kind: DeclKind, initialized: bool, exported: bool) {
        let info = self.ensure_var(id.clone());
        info.declared = true;
        info.exported |= exported;

        match kind {
            DeclKind::Var(var_kind) => {
                info.assign_count += 1;
                info.var_kind = Some(var_kind);
            }
            DeclKind::Function => {
                info.assign_count += 1;
                info.declared_as_fn_decl = true;
            }
            DeclKind::FunctionExpr => {
                info.declared_as_fn_expr = true;
            }
            DeclKind::Class | DeclKind::ClassExpr | DeclKind::Import => {
                info.assign_count += 1;
            }
            DeclKind::Param => {
                info.declared_as_fn_param = true;
            }
        }

        if initialized {
            self.data.initialized_vars.push(id);
        }
    }

    fn declare_ident(&mut self, ident: &Ident, kind: DeclKind, initialized: bool, exported: bool) {
        self.declare_id((ident.sym.clone(), ident.ctxt), kind, initialized, exported);
    }

    fn declare_pat(&mut self, pat: &Pat, kind: DeclKind, initialized: bool, exported: bool) {
        for id in BindingCollector::collect(pat) {
            self.declare_id(id, kind, initialized, exported);
        }
    }

    fn record_usage(&mut self, ident: &Ident) {
        if ident.sym.as_ref() == "arguments" {
            self.data.top.used_arguments = true;
        }

        let in_call_arg = self.in_call_arg;
        let in_callee = self.in_callee;
        let info = self.ensure_ident(ident);
        info.ref_count += 1;
        info.usage_count += 1;
        if in_call_arg {
            info.used_as_arg = true;
        }
        if in_callee {
            info.used_as_callee = true;
        }
    }

    fn record_write_id(&mut self, id: Id) {
        let info = self.ensure_var(id);
        info.assign_count += 1;
    }

    fn record_read_write_id(&mut self, id: Id) {
        let info = self.ensure_var(id);
        info.ref_count += 1;
        info.usage_count += 1;
        info.assign_count += 1;
        info.used_as_ref = true;
    }

    fn record_export_module_name(&mut self, name: &ModuleExportName) {
        if let ModuleExportName::Ident(ident) = name {
            self.ensure_ident(ident).exported = true;
        }
    }
}

impl Visit for ScopeAnalyzer {
    fn visit_binding_ident(&mut self, _ident: &BindingIdent) {}

    fn visit_ident(&mut self, ident: &Ident) {
        self.record_usage(ident);
    }

    fn visit_member_expr(&mut self, expr: &MemberExpr) {
        expr.obj.visit_with(self);
        match &expr.prop {
            MemberProp::Computed(computed) => {
                if let Expr::Ident(obj_ident) = &*expr.obj {
                    self.ensure_ident(obj_ident).indexed_with_dynamic_key = true;
                }
                computed.visit_with(self);
            }
            MemberProp::Ident(_) => {
                if let Expr::Ident(obj_ident) = &*expr.obj {
                    self.ensure_ident(obj_ident).has_property_access = true;
                }
            }
            MemberProp::PrivateName(_) => {}
        }
    }

    fn visit_prop_name(&mut self, name: &PropName) {
        if let PropName::Computed(computed) = name {
            computed.visit_with(self);
        }
    }

    fn visit_labeled_stmt(&mut self, stmt: &LabeledStmt) {
        stmt.body.visit_with(self);
    }

    fn visit_break_stmt(&mut self, _stmt: &BreakStmt) {}

    fn visit_continue_stmt(&mut self, _stmt: &ContinueStmt) {}

    fn visit_call_expr(&mut self, call: &CallExpr) {
        let prev_callee = self.in_callee;
        self.in_callee = true;
        if let Callee::Expr(callee) = &call.callee {
            if let Expr::Ident(ident) = &**callee
                && ident.sym.as_ref() == "eval"
            {
                self.data.top.has_eval_call = true;
            }
            callee.visit_with(self);
        }
        self.in_callee = prev_callee;

        let prev_arg = self.in_call_arg;
        self.in_call_arg = true;
        for arg in &call.args {
            arg.visit_with(self);
        }
        self.in_call_arg = prev_arg;

        if let Some(type_args) = &call.type_args {
            type_args.visit_with(self);
        }
    }

    fn visit_new_expr(&mut self, expr: &NewExpr) {
        let prev_callee = self.in_callee;
        self.in_callee = true;
        expr.callee.visit_with(self);
        self.in_callee = prev_callee;

        let prev_arg = self.in_call_arg;
        self.in_call_arg = true;
        if let Some(args) = &expr.args {
            for arg in args {
                arg.visit_with(self);
            }
        }
        self.in_call_arg = prev_arg;

        if let Some(type_args) = &expr.type_args {
            type_args.visit_with(self);
        }
    }

    fn visit_assign_expr(&mut self, expr: &AssignExpr) {
        expr.left.visit_with(self);
        expr.right.visit_with(self);

        let ids = match &expr.left {
            AssignTarget::Simple(simple) => match simple {
                SimpleAssignTarget::Ident(ident) => vec![(ident.id.sym.clone(), ident.id.ctxt)],
                _ => Vec::new(),
            },
            AssignTarget::Pat(pat) => BindingCollector::collect_assign_target_pat(pat),
        };

        if matches!(expr.op, AssignOp::Assign) {
            for id in ids {
                self.record_write_id(id);
            }
        } else {
            for id in ids {
                self.record_read_write_id(id);
            }
        }
    }

    fn visit_update_expr(&mut self, expr: &UpdateExpr) {
        expr.arg.visit_with(self);
        if let Expr::Ident(ident) = &*expr.arg {
            self.record_read_write_id((ident.sym.clone(), ident.ctxt));
        }
    }

    fn visit_with_stmt(&mut self, stmt: &WithStmt) {
        self.data.top.has_with_stmt = true;
        stmt.obj.visit_with(self);
        stmt.body.visit_with(self);
    }

    fn visit_var_decl(&mut self, decl: &VarDecl) {
        for declarator in &decl.decls {
            self.declare_pat(
                &declarator.name,
                DeclKind::Var(decl.kind),
                declarator.init.is_some(),
                false,
            );
            declarator.name.visit_with(self);
            if let Some(init) = &declarator.init {
                init.visit_with(self);
            }
        }
    }

    fn visit_param(&mut self, param: &Param) {
        self.declare_pat(&param.pat, DeclKind::Param, false, false);
        param.pat.visit_with(self);
    }

    fn visit_fn_decl(&mut self, decl: &FnDecl) {
        self.declare_ident(&decl.ident, DeclKind::Function, false, false);
        decl.function.visit_with(self);
    }

    fn visit_fn_expr(&mut self, expr: &FnExpr) {
        if let Some(ident) = &expr.ident {
            self.declare_ident(ident, DeclKind::FunctionExpr, false, false);
        }
        expr.function.visit_with(self);
    }

    fn visit_class_decl(&mut self, decl: &ClassDecl) {
        self.declare_ident(&decl.ident, DeclKind::Class, false, false);
        decl.class.visit_with(self);
    }

    fn visit_class_expr(&mut self, expr: &ClassExpr) {
        if let Some(ident) = &expr.ident {
            self.declare_ident(ident, DeclKind::ClassExpr, false, false);
        }
        expr.class.visit_with(self);
    }

    fn visit_import_decl(&mut self, decl: &ImportDecl) {
        for specifier in &decl.specifiers {
            match specifier {
                ImportSpecifier::Named(named) => {
                    self.declare_ident(&named.local, DeclKind::Import, false, false);
                }
                ImportSpecifier::Default(default) => {
                    self.declare_ident(&default.local, DeclKind::Import, false, false);
                }
                ImportSpecifier::Namespace(namespace) => {
                    self.declare_ident(&namespace.local, DeclKind::Import, false, false);
                }
            }
        }
    }

    fn visit_export_decl(&mut self, decl: &ExportDecl) {
        match &decl.decl {
            Decl::Class(class_decl) => {
                self.declare_ident(&class_decl.ident, DeclKind::Class, false, true);
                class_decl.class.visit_with(self);
            }
            Decl::Fn(fn_decl) => {
                self.declare_ident(&fn_decl.ident, DeclKind::Function, false, true);
                fn_decl.function.visit_with(self);
            }
            Decl::Var(var_decl) => {
                for declarator in &var_decl.decls {
                    self.declare_pat(
                        &declarator.name,
                        DeclKind::Var(var_decl.kind),
                        declarator.init.is_some(),
                        true,
                    );
                    declarator.name.visit_with(self);
                    if let Some(init) = &declarator.init {
                        init.visit_with(self);
                    }
                }
            }
            _ => decl.decl.visit_with(self),
        }
    }

    fn visit_named_export(&mut self, export: &NamedExport) {
        for specifier in &export.specifiers {
            match specifier {
                ExportSpecifier::Named(named) => {
                    self.record_export_module_name(&named.orig);
                    if let Some(exported) = &named.exported {
                        self.record_export_module_name(exported);
                    }
                }
                ExportSpecifier::Default(default) => {
                    self.ensure_ident(&default.exported).exported = true;
                }
                ExportSpecifier::Namespace(namespace) => match &namespace.name {
                    ModuleExportName::Ident(ident) => {
                        self.ensure_ident(ident).exported = true;
                    }
                    ModuleExportName::Str(_) => {}
                },
            }
        }
    }

    fn visit_export_default_decl(&mut self, decl: &ExportDefaultDecl) {
        match &decl.decl {
            DefaultDecl::Class(class_expr) => {
                if let Some(ident) = &class_expr.ident {
                    self.declare_ident(ident, DeclKind::ClassExpr, false, true);
                }
                class_expr.class.visit_with(self);
            }
            DefaultDecl::Fn(fn_expr) => {
                if let Some(ident) = &fn_expr.ident {
                    self.declare_ident(ident, DeclKind::FunctionExpr, false, true);
                }
                fn_expr.function.visit_with(self);
            }
            _ => decl.decl.visit_children_with(self),
        }
    }
}

/// Analyze variable usage in the given AST
#[must_use]
pub fn analyze<N>(n: &N) -> ScopeData
where
    N: VisitWith<ScopeAnalyzer>,
{
    let mut analyzer = ScopeAnalyzer::new();
    n.visit_with(&mut analyzer);
    analyzer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_and_analyze(code: &str) -> ScopeData {
        use std::sync::Arc;
        use swc_common::{FileName, GLOBALS, Globals, SourceMap};
        use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax};

        let cm = Arc::new(SourceMap::default());
        let fm = cm.new_source_file(
            FileName::Custom("test.js".to_owned()).into(),
            code.to_owned(),
        );

        let mut parser = Parser::new(
            Syntax::Es(EsSyntax::default()),
            StringInput::from(&*fm),
            None,
        );
        let module = parser.parse_module().unwrap();

        GLOBALS.set(&Globals::default(), || analyze(&module))
    }

    #[test]
    fn simple_var_usage() {
        let data = parse_and_analyze("var x = 1; console.log(x);");

        let x_var = data.vars.iter().find(|(id, _)| id.0.as_str() == "x");

        assert!(x_var.is_some(), "Variable 'x' should be found");
        let (_, info) = x_var.expect("x var");
        assert!(info.declared, "Variable 'x' should be declared");
        assert!(
            info.ref_count >= 1,
            "Variable 'x' should have at least 1 reference"
        );
    }

    #[test]
    fn unused_var() {
        let data = parse_and_analyze("var unused = 1;");

        let unused_var = data.vars.iter().find(|(id, _)| id.0.as_str() == "unused");

        assert!(unused_var.is_some(), "Variable 'unused' should be found");
        let (_, info) = unused_var.expect("unused var");
        assert!(info.declared, "Variable 'unused' should be declared");
    }

    #[test]
    fn function_param() {
        let data = parse_and_analyze("function foo(a, b) { return a + b; }");

        let a_var = data.vars.iter().find(|(id, _)| id.0.as_str() == "a");

        assert!(a_var.is_some(), "Parameter 'a' should be found");
        let (_, info) = a_var.expect("a var");
        assert!(
            info.declared_as_fn_param,
            "Parameter 'a' should be marked as function param"
        );
    }
}
