//! Scope analysis helpers.
//!
//! This module wraps `swc_ecma_usage_analyzer` and exposes a lightweight
//! `ScopeData` view used by transformers for safe cleanups.

use std::collections::HashMap;

use swc_atoms::Atom;
use swc_common::SyntaxContext;
use swc_ecma_ast::*;
use swc_ecma_usage_analyzer::{
    alias::Access,
    analyzer::{
        Ctx, ScopeKind, UsageAnalyzer, analyze_with_custom_storage,
        storage::{ScopeDataLike, Storage, VarDataLike},
    },
};
use swc_ecma_utils::{Type, Value};
use swc_ecma_visit::VisitWith;

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
    pub has_with_stmt: bool,
    pub has_eval_call: bool,
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

// Implement VarDataLike trait for VarUsageInfo
impl VarDataLike for VarUsageInfo {
    fn mark_declared_as_fn_param(&mut self) {
        self.declared_as_fn_param = true;
        self.declared = true;
    }

    fn mark_as_lazy_init(&mut self) {
        self.lazy_init = true;
    }

    fn mark_declared_as_fn_decl(&mut self) {
        self.declared_as_fn_decl = true;
        self.declared = true;
    }

    fn mark_declared_as_fn_expr(&mut self) {
        self.declared_as_fn_expr = true;
        self.declared = true;
    }

    fn mark_declared_as_for_init(&mut self) {
        self.declared_as_for_init = true;
        self.declared = true;
    }

    fn mark_has_property_access(&mut self) {
        self.has_property_access = true;
    }

    fn mark_used_as_callee(&mut self) {
        self.used_as_callee = true;
    }

    fn mark_used_as_arg(&mut self) {
        self.used_as_arg = true;
        self.used_as_ref = true;
    }

    fn mark_indexed_with_dynamic_key(&mut self) {
        self.indexed_with_dynamic_key = true;
    }

    fn add_accessed_property(&mut self, name: swc_atoms::Wtf8Atom) {
        let atom: Atom = name.to_atom_lossy().into_owned();
        *self.accessed_props.entry(atom).or_default() += 1;
    }

    fn mark_used_as_ref(&mut self) {
        self.used_as_ref = true;
    }

    fn add_infects_to(&mut self, other: Access) {
        self.infects_to.push(other);
    }

    fn prevent_inline(&mut self) {
        self.inline_prevented = true;
    }

    fn mark_as_exported(&mut self) {
        self.exported = true;
    }

    fn mark_initialized_with_safe_value(&mut self) {
        self.initialized_with_safe_value = true;
    }

    fn mark_as_pure_fn(&mut self) {
        self.pure_fn = true;
    }

    fn mark_used_above_decl(&mut self) {
        self.used_above_decl = true;
    }

    fn mark_used_recursively(&mut self) {
        self.used_recursively = true;
    }

    fn is_declared(&self) -> bool {
        self.declared
    }

    fn mark_used_as_jsx_callee(&mut self) {
        self.used_as_jsx_callee = true;
    }
}

// Implement ScopeDataLike for ScopeInfo
impl ScopeDataLike for ScopeInfo {
    fn add_declared_symbol(&mut self, _id: &Ident) {
        // We don't track declared symbols at scope level
    }

    fn merge(&mut self, other: Self, _is_child: bool) {
        self.has_with_stmt |= other.has_with_stmt;
        self.has_eval_call |= other.has_eval_call;
        self.used_arguments |= other.used_arguments;
    }

    fn mark_used_arguments(&mut self) {
        self.used_arguments = true;
    }

    fn mark_eval_called(&mut self) {
        self.has_eval_call = true;
    }

    fn mark_with_stmt(&mut self) {
        self.has_with_stmt = true;
    }
}

// Implement Storage trait for ScopeData
impl Storage for ScopeData {
    type ScopeData = ScopeInfo;
    type VarData = VarUsageInfo;

    fn scopes(&self) -> &[Self::ScopeData] {
        &self.scopes
    }

    fn new_child(&mut self) -> Self {
        Self::default()
    }

    fn add_property_atom(&mut self, _atom: swc_atoms::Wtf8Atom) {
        // We don't collect property atoms
    }

    fn scope(&mut self, _ctxt: SyntaxContext) -> &mut Self::ScopeData {
        if self.scopes.is_empty() {
            self.scopes.push(ScopeInfo::default());
        }
        self.scopes
            .last_mut()
            .expect("scope stack should have at least one scope")
    }

    fn merge(&mut self, kind: ScopeKind, child: Self) {
        // Merge child scope data into this
        #[expect(
            clippy::iter_over_hash_type,
            reason = "merge order is intentionally irrelevant"
        )]
        for (id, var) in child.vars {
            let existing = self.vars.entry(id).or_default();
            existing.ref_count += var.ref_count;
            existing.assign_count += var.assign_count;
            existing.usage_count += var.usage_count;
            existing.declared |= var.declared;
            existing.declared_as_fn_param |= var.declared_as_fn_param;
            existing.declared_as_fn_decl |= var.declared_as_fn_decl;
            existing.declared_as_fn_expr |= var.declared_as_fn_expr;
            existing.used_as_ref |= var.used_as_ref;
            existing.used_as_callee |= var.used_as_callee;
            existing.used_as_arg |= var.used_as_arg;
            existing.inline_prevented |= var.inline_prevented;
            existing.exported |= var.exported;
        }

        // Merge scope info
        self.top.merge(child.top, matches!(kind, ScopeKind::Block));
        for scope in child.scopes {
            self.top.merge(scope, true);
        }
    }

    fn report_assign(&mut self, _ctx: Ctx, id: Id, _is_assign: bool, _init_type: Value<Type>) {
        let var = self.vars.entry(id).or_default();
        var.assign_count += 1;
    }

    fn top_scope(&mut self) -> &mut Self::ScopeData {
        &mut self.top
    }

    fn var_or_default(&mut self, id: Id) -> &mut Self::VarData {
        self.vars.entry(id).or_default()
    }

    fn report_usage(&mut self, _ctx: Ctx, id: Id) {
        let var = self.vars.entry(id).or_default();
        var.ref_count += 1;
        var.usage_count += 1;
    }

    fn declare_decl(
        &mut self,
        _ctx: Ctx,
        i: &Ident,
        init_type: Option<Value<Type>>,
        kind: Option<VarDeclKind>,
    ) -> &mut Self::VarData {
        let id = (i.sym.clone(), i.ctxt);
        let var = self.vars.entry(id.clone()).or_default();
        var.declared = true;
        var.assign_count += 1;
        var.var_kind = kind;
        if let Some(t) = init_type {
            var.merged_var_type = Some(t);
            self.initialized_vars.push(id);
        }
        var
    }

    fn get_initialized_cnt(&self) -> usize {
        self.initialized_vars.len()
    }

    fn truncate_initialized_cnt(&mut self, len: usize) {
        self.initialized_vars.truncate(len);
    }

    fn mark_property_mutation(&mut self, id: Id) {
        if let Some(var) = self.vars.get_mut(&id) {
            var.has_property_access = true;
        }
    }

    fn get_var_data(&self, id: Id) -> Option<&Self::VarData> {
        self.vars.get(&id)
    }
}

/// Analyze variable usage in the given AST
#[must_use]
pub fn analyze<N>(n: &N) -> ScopeData
where
    N: VisitWith<UsageAnalyzer<ScopeData>>,
{
    let data = ScopeData::default();
    analyze_with_custom_storage(data, n, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_and_analyze(code: &str) -> ScopeData {
        use swc_common::{GLOBALS, Globals, SourceMap, sync::Lrc};
        use swc_ecma_ast::EsVersion;
        use swc_ecma_parser::{EsSyntax, Syntax, parse_file_as_module};

        let cm: Lrc<SourceMap> = Lrc::default();
        let fm = cm.new_source_file(
            swc_common::FileName::Custom("test.js".to_owned()).into(),
            code.to_owned(),
        );

        let module = parse_file_as_module(
            &fm,
            Syntax::Es(EsSyntax::default()),
            EsVersion::default(),
            None,
            &mut vec![],
        )
        .unwrap();

        GLOBALS.set(&Globals::default(), || analyze(&module))
    }

    #[test]
    fn simple_var_usage() {
        let data = parse_and_analyze("var x = 1; console.log(x);");

        // Find the 'x' variable
        let x_var = data.vars.iter().find(|(id, _)| id.0.as_str() == "x");

        assert!(x_var.is_some(), "Variable 'x' should be found");
        let (_, info) = x_var.unwrap();
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
        let (_, info) = unused_var.unwrap();
        assert!(info.declared, "Variable 'unused' should be declared");
    }

    #[test]
    fn function_param() {
        let data = parse_and_analyze("function foo(a, b) { return a + b; }");

        let a_var = data.vars.iter().find(|(id, _)| id.0.as_str() == "a");

        assert!(a_var.is_some(), "Parameter 'a' should be found");
        let (_, info) = a_var.unwrap();
        assert!(
            info.declared_as_fn_param,
            "Parameter 'a' should be marked as function param"
        );
    }
}
