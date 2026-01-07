use std::collections::HashSet;
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitMut, VisitMutWith as _, VisitWith as _};

use crate::scope::ScopeData;

pub(super) struct ObfuscationGarbageRemover<'a> {
    scope_data: &'a ScopeData,
    candidates: &'a HashSet<String>,
    candidate_functions: &'a HashSet<String>,
    external_function_uses: &'a HashSet<String>,
}

impl<'a> ObfuscationGarbageRemover<'a> {
    pub(super) const fn new(
        scope_data: &'a ScopeData,
        candidates: &'a HashSet<String>,
        candidate_functions: &'a HashSet<String>,
        external_function_uses: &'a HashSet<String>,
    ) -> Self {
        Self {
            scope_data,
            candidates,
            candidate_functions,
            external_function_uses,
        }
    }

    fn should_remove_ident(&self, ident: &Ident) -> bool {
        let name = ident.sym.as_ref();
        if !self.candidates.contains(name) {
            return false;
        }

        if self.candidate_functions.contains(name) && !self.external_function_uses.contains(name) {
            return true;
        }

        let id = (ident.sym.clone(), ident.ctxt);
        if let Some(info) = self.scope_data.vars.get(&id) {
            if info.exported {
                return false;
            }
            return info.usage_count == 0;
        }

        false
    }
}

pub(super) struct ObfuscationAliasCleaner<'a> {
    scope_data: &'a ScopeData,
    candidates: &'a HashSet<String>,
}

impl<'a> ObfuscationAliasCleaner<'a> {
    pub(super) const fn new(scope_data: &'a ScopeData, candidates: &'a HashSet<String>) -> Self {
        Self {
            scope_data,
            candidates,
        }
    }
}

impl VisitMut for ObfuscationAliasCleaner<'_> {
    fn visit_mut_var_decl(&mut self, decl: &mut VarDecl) {
        decl.visit_mut_children_with(self);

        decl.decls.retain(|declarator| {
            let Pat::Ident(binding) = &declarator.name else {
                return true;
            };
            let Some(init) = &declarator.init else {
                return true;
            };
            let Expr::Ident(init_ident) = &**init else {
                return true;
            };

            if !self.candidates.contains(init_ident.sym.as_ref()) {
                return true;
            }

            let id = (binding.id.sym.clone(), binding.id.ctxt);
            if let Some(info) = self.scope_data.vars.get(&id) {
                if info.exported {
                    return true;
                }
                return !info.is_unused();
            }

            true
        });
    }
}

pub(super) struct UnusedObfuscatedRemover<'a> {
    scope_data: &'a ScopeData,
    pub(super) changed: bool,
}

impl<'a> UnusedObfuscatedRemover<'a> {
    pub(super) const fn new(scope_data: &'a ScopeData) -> Self {
        Self {
            scope_data,
            changed: false,
        }
    }

    fn is_obfuscated_name(name: &str) -> bool {
        name.starts_with("_0x")
    }

    fn should_remove_ident(&self, ident: &Ident) -> bool {
        if !Self::is_obfuscated_name(ident.sym.as_ref()) {
            return false;
        }
        let id = (ident.sym.clone(), ident.ctxt);
        if let Some(info) = self.scope_data.vars.get(&id) {
            if info.exported {
                return false;
            }
            return info.ref_count == 0;
        }
        false
    }
}

impl VisitMut for UnusedObfuscatedRemover<'_> {
    fn visit_mut_module_items(&mut self, items: &mut Vec<ModuleItem>) {
        for item in items.iter_mut() {
            item.visit_mut_children_with(self);
        }

        items.retain(|item| match item {
            ModuleItem::Stmt(Stmt::Decl(Decl::Fn(fn_decl))) => {
                if self.should_remove_ident(&fn_decl.ident) {
                    self.changed = true;
                    false
                } else {
                    true
                }
            }
            ModuleItem::Stmt(Stmt::Decl(Decl::Var(var_decl))) => !var_decl.decls.is_empty(),
            ModuleItem::Stmt(Stmt::Empty(_)) => false,
            _ => true,
        });
    }

    fn visit_mut_stmts(&mut self, stmts: &mut Vec<Stmt>) {
        for stmt in stmts.iter_mut() {
            stmt.visit_mut_children_with(self);
        }

        stmts.retain(|stmt| match stmt {
            Stmt::Decl(Decl::Fn(fn_decl)) => {
                if self.should_remove_ident(&fn_decl.ident) {
                    self.changed = true;
                    false
                } else {
                    true
                }
            }
            Stmt::Decl(Decl::Var(var_decl)) => !var_decl.decls.is_empty(),
            Stmt::Empty(_) => false,
            _ => true,
        });
    }

    fn visit_mut_var_decl(&mut self, decl: &mut VarDecl) {
        decl.visit_mut_children_with(self);

        decl.decls.retain(|declarator| {
            let Pat::Ident(binding) = &declarator.name else {
                return true;
            };
            if !self.should_remove_ident(&binding.id) {
                return true;
            }
            let Some(init) = &declarator.init else {
                return true;
            };
            if !is_pure_expr(init) {
                return true;
            }
            self.changed = true;
            false
        });
    }
}

#[must_use]
fn is_pure_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Ident(_) | Expr::Lit(_) | Expr::Fn(_) | Expr::Arrow(_) => true,
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
                _ => false,
            },
            PropOrSpread::Spread(_) => false,
        }),
        _ => false,
    }
}

pub(super) struct ExternalUsageFinder<'a> {
    targets: &'a HashSet<String>,
    fn_stack: Vec<String>,
    pub(super) external_uses: HashSet<String>,
}

impl<'a> ExternalUsageFinder<'a> {
    pub(super) fn new(targets: &'a HashSet<String>) -> Self {
        Self {
            targets,
            fn_stack: Vec::new(),
            external_uses: HashSet::new(),
        }
    }
}

impl Visit for ExternalUsageFinder<'_> {
    fn visit_fn_decl(&mut self, func: &FnDecl) {
        let name = func.ident.sym.to_string();
        if self.targets.contains(&name) {
            self.fn_stack.push(name);
            if let Some(body) = &func.function.body {
                body.visit_with(self);
            }
            self.fn_stack.pop();
        } else {
            func.visit_children_with(self);
        }
    }

    fn visit_ident(&mut self, ident: &Ident) {
        if !self.targets.contains(ident.sym.as_ref()) {
            return;
        }

        let in_own_fn = self
            .fn_stack
            .last()
            .is_some_and(|name| name == ident.sym.as_ref());
        if !in_own_fn {
            self.external_uses.insert(ident.sym.to_string());
        }
    }
}

impl VisitMut for ObfuscationGarbageRemover<'_> {
    fn visit_mut_module_items(&mut self, items: &mut Vec<ModuleItem>) {
        for item in items.iter_mut() {
            item.visit_mut_children_with(self);
        }

        items.retain(|item| match item {
            ModuleItem::Stmt(Stmt::Decl(Decl::Fn(fn_decl))) => {
                !self.should_remove_ident(&fn_decl.ident)
            }
            ModuleItem::Stmt(Stmt::Decl(Decl::Var(var_decl))) => !var_decl.decls.is_empty(),
            ModuleItem::Stmt(Stmt::Empty(_)) => false,
            _ => true,
        });
    }

    fn visit_mut_stmts(&mut self, stmts: &mut Vec<Stmt>) {
        for stmt in stmts.iter_mut() {
            stmt.visit_mut_children_with(self);
        }

        stmts.retain(|stmt| match stmt {
            Stmt::Decl(Decl::Fn(fn_decl)) => !self.should_remove_ident(&fn_decl.ident),
            Stmt::Decl(Decl::Var(var_decl)) => !var_decl.decls.is_empty(),
            Stmt::Empty(_) => false,
            _ => true,
        });
    }

    fn visit_mut_var_decl(&mut self, decl: &mut VarDecl) {
        decl.visit_mut_children_with(self);

        decl.decls.retain(|declarator| {
            if let Pat::Ident(binding) = &declarator.name {
                !self.should_remove_ident(&binding.id)
            } else {
                true
            }
        });
    }
}
