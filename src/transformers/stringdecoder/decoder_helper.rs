use std::collections::HashMap;
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use crate::context::DecoderFunctionType;

#[derive(Debug, Clone)]
pub(super) struct DecoderHelper {
    pub(super) decoder_type: DecoderFunctionType,
    pub(super) charset: Option<String>,
}

pub(super) struct DecoderHelperFinder {
    pub(super) helpers: HashMap<String, DecoderHelper>,
}

impl DecoderHelperFinder {
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            helpers: HashMap::new(),
        }
    }

    #[must_use]
    fn find_base91_charset(stmts: &[Stmt]) -> Option<String> {
        if let Some(inline) = Self::find_inline_base91_charset(stmts) {
            return Some(inline);
        }

        let mut candidates: Vec<(String, String)> = Vec::new();

        for stmt in stmts {
            if let Stmt::Decl(Decl::Var(var_decl)) = stmt {
                for decl in &var_decl.decls {
                    let Pat::Ident(binding) = &decl.name else {
                        continue;
                    };
                    let Some(init) = &decl.init else {
                        continue;
                    };
                    if let Expr::Lit(Lit::Str(s)) = &**init
                        && let Some(value) = s.value.as_str()
                        && value.len() == 91
                    {
                        candidates.push((binding.id.sym.to_string(), value.to_string()));
                    }
                }
            }
        }

        for (name, charset) in candidates {
            if Self::stmts_use_index_of(stmts, &name) {
                return Some(charset);
            }
        }

        None
    }

    #[must_use]
    fn find_inline_base91_charset(stmts: &[Stmt]) -> Option<String> {
        let mut finder = InlineBase91Finder::new();
        for stmt in stmts {
            stmt.visit_with(&mut finder);
            if finder.found.is_some() {
                return finder.found;
            }
        }
        None
    }

    #[must_use]
    fn stmts_use_index_of(stmts: &[Stmt], target: &str) -> bool {
        let mut finder = IndexOfFinder::new(target);
        for stmt in stmts {
            stmt.visit_with(&mut finder);
            if finder.found {
                return true;
            }
        }
        false
    }

    fn record_helper(&mut self, name: String, charset: String) {
        self.helpers.insert(
            name,
            DecoderHelper {
                decoder_type: DecoderFunctionType::Base91,
                charset: Some(charset),
            },
        );
    }
}

impl Visit for DecoderHelperFinder {
    fn visit_fn_decl(&mut self, func: &FnDecl) {
        if let Some(body) = &func.function.body
            && let Some(charset) = Self::find_base91_charset(&body.stmts)
        {
            self.record_helper(func.ident.sym.to_string(), charset);
        }

        func.visit_children_with(self);
    }

    fn visit_var_decl(&mut self, decl: &VarDecl) {
        for declarator in &decl.decls {
            let Pat::Ident(binding) = &declarator.name else {
                continue;
            };
            let Some(init) = &declarator.init else {
                continue;
            };

            match &**init {
                Expr::Fn(fn_expr) => {
                    if let Some(body) = &fn_expr.function.body
                        && let Some(charset) = Self::find_base91_charset(&body.stmts)
                    {
                        self.record_helper(binding.id.sym.to_string(), charset);
                    }
                }
                Expr::Arrow(arrow) => {
                    if let BlockStmtOrExpr::BlockStmt(body) = &*arrow.body
                        && let Some(charset) = Self::find_base91_charset(&body.stmts)
                    {
                        self.record_helper(binding.id.sym.to_string(), charset);
                    }
                }
                _ => {}
            }
        }

        decl.visit_children_with(self);
    }
}

struct IndexOfFinder<'a> {
    target: &'a str,
    found: bool,
}

impl<'a> IndexOfFinder<'a> {
    #[must_use]
    fn new(target: &'a str) -> Self {
        Self {
            target,
            found: false,
        }
    }

    fn check_member(&mut self, member: &MemberExpr) {
        if let Expr::Ident(obj_ident) = &*member.obj
            && obj_ident.sym == self.target
            && matches!(
                &member.prop,
                MemberProp::Ident(prop) if prop.sym == "indexOf"
            )
        {
            self.found = true;
        }
    }
}

impl Visit for IndexOfFinder<'_> {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        if self.found {
            return;
        }

        if let Callee::Expr(callee) = &call.callee
            && let Expr::Member(member) = &**callee
        {
            self.check_member(member);
            if self.found {
                return;
            }
        }

        call.visit_children_with(self);
    }

    fn visit_fn_decl(&mut self, _func: &FnDecl) {}
    fn visit_fn_expr(&mut self, _func: &FnExpr) {}
    fn visit_arrow_expr(&mut self, _func: &ArrowExpr) {}
}

struct InlineBase91Finder {
    found: Option<String>,
}

impl InlineBase91Finder {
    #[must_use]
    fn new() -> Self {
        Self { found: None }
    }

    fn check_member(&mut self, member: &MemberExpr) {
        let prop_is_index_of = match &member.prop {
            MemberProp::Ident(prop) => prop.sym == "indexOf",
            MemberProp::Computed(computed) => {
                if let Expr::Lit(Lit::Str(s)) = &*computed.expr {
                    s.value.as_str() == Some("indexOf")
                } else {
                    false
                }
            }
            _ => false,
        };

        if !prop_is_index_of {
            return;
        }

        if let Expr::Lit(Lit::Str(s)) = &*member.obj
            && let Some(value) = s.value.as_str()
            && value.len() == 91
        {
            self.found = Some(value.to_string());
        }
    }
}

impl Visit for InlineBase91Finder {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        if self.found.is_some() {
            return;
        }

        if let Callee::Expr(callee) = &call.callee
            && let Expr::Member(member) = &**callee
        {
            self.check_member(member);
            if self.found.is_some() {
                return;
            }
        }

        call.visit_children_with(self);
    }

    fn visit_fn_decl(&mut self, _func: &FnDecl) {}
    fn visit_fn_expr(&mut self, _func: &FnExpr) {}
    fn visit_arrow_expr(&mut self, _func: &ArrowExpr) {}
}
