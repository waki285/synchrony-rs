use std::collections::HashMap;
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitMut, VisitMutWith as _, VisitWith as _};

use crate::context::{DecoderFunction, DecoderFunctionType, StringArray};

use super::core::eval_const_i64;
use super::decoder_helper::DecoderHelper;

/// Second pass: find decoder functions
///
/// Detects decoder functions that:
/// 1. Call a string array function
/// 2. Have an offset calculation
/// 3. Optionally have Base64/RC4 charset
pub(super) struct DecoderFunctionFinder<'a> {
    string_arrays: &'a [StringArray],
    helper_decoders: &'a HashMap<String, DecoderHelper>,
    pub(super) decoders: Vec<DecoderFunction>,
}

impl<'a> DecoderFunctionFinder<'a> {
    #[must_use]
    pub(super) const fn new(
        string_arrays: &'a [StringArray],
        helper_decoders: &'a HashMap<String, DecoderHelper>,
    ) -> Self {
        Self {
            string_arrays,
            helper_decoders,
            decoders: Vec::new(),
        }
    }

    #[must_use]
    fn get_string_array_names(&self) -> Vec<&str> {
        self.string_arrays
            .iter()
            .map(|a| a.identifier.as_str())
            .collect()
    }

    #[must_use]
    fn extract_seq_expr(expr: &Expr) -> Option<&SeqExpr> {
        match expr {
            Expr::Seq(seq) => Some(seq),
            Expr::Paren(paren) => Self::extract_seq_expr(&paren.expr),
            _ => None,
        }
    }

    #[must_use]
    fn strip_parens(expr: &Expr) -> &Expr {
        match expr {
            Expr::Paren(paren) => Self::strip_parens(&paren.expr),
            _ => expr,
        }
    }

    #[must_use]
    fn expr_is_ident_name(expr: &Expr, name: &str) -> bool {
        match Self::strip_parens(expr) {
            Expr::Ident(ident) => ident.sym == name,
            _ => false,
        }
    }

    #[must_use]
    fn eval_const_i32(expr: &Expr) -> Option<i32> {
        i32::try_from(eval_const_i64(expr)?).ok()
    }

    #[must_use]
    fn assign_left_name(assign: &AssignExpr) -> Option<&str> {
        assign
            .left
            .as_ident()
            .map(|binding| binding.id.sym.as_ref())
    }

    /// Extract offset from binary expression like `idx - 123` or `idx + 456`
    #[must_use]
    pub(super) fn extract_offset(expr: &Expr) -> Option<i32> {
        if let Expr::Bin(bin) = expr {
            // Check right side for number
            let num = Self::eval_const_i32(&bin.right)?;

            // Apply operator
            match bin.op {
                BinaryOp::Sub => Some(-num),
                BinaryOp::Add => Some(num),
                _ => None,
            }
        } else {
            None
        }
    }

    /// Extract offset from assignment expression (`=`/`+=`/`-=` forms)
    #[must_use]
    fn extract_offset_from_assign(assign: &AssignExpr) -> Option<i32> {
        let left_name = Self::assign_left_name(assign)?;
        match assign.op {
            AssignOp::Assign => {
                if let Expr::Bin(bin) = Self::strip_parens(&assign.right)
                    && !Self::expr_is_ident_name(&bin.left, left_name)
                {
                    return None;
                }
                Self::extract_offset(&assign.right)
            }
            AssignOp::AddAssign => Self::eval_const_i32(&assign.right),
            AssignOp::SubAssign => Self::eval_const_i32(&assign.right).map(|v| -v),
            _ => None,
        }
    }

    #[must_use]
    fn find_offset_in_stmts(stmts: &[Stmt]) -> Option<i32> {
        for stmt in stmts {
            if let Some(off) = Self::find_offset_in_stmt(stmt) {
                return Some(off);
            }
        }
        None
    }

    #[must_use]
    fn find_offset_in_stmt(stmt: &Stmt) -> Option<i32> {
        match stmt {
            Stmt::Expr(expr_stmt) => Self::find_offset_in_expr(&expr_stmt.expr),
            Stmt::Return(ret) => {
                let arg = ret.arg.as_ref()?;
                Self::find_offset_in_expr(arg)
            }
            Stmt::Block(block) => Self::find_offset_in_stmts(&block.stmts),
            Stmt::If(if_stmt) => Self::find_offset_in_stmt(&if_stmt.cons).or_else(|| {
                let alt = if_stmt.alt.as_ref()?;
                Self::find_offset_in_stmt(alt)
            }),
            Stmt::While(while_stmt) => Self::find_offset_in_stmt(&while_stmt.body),
            Stmt::For(for_stmt) => Self::find_offset_in_stmt(&for_stmt.body),
            Stmt::ForIn(for_in) => Self::find_offset_in_stmt(&for_in.body),
            Stmt::ForOf(for_of) => Self::find_offset_in_stmt(&for_of.body),
            Stmt::Try(try_stmt) => {
                let mut found = Self::find_offset_in_stmts(&try_stmt.block.stmts);
                if found.is_none()
                    && let Some(handler) = &try_stmt.handler
                {
                    found = Self::find_offset_in_stmts(&handler.body.stmts);
                }
                if found.is_none()
                    && let Some(finalizer) = &try_stmt.finalizer
                {
                    found = Self::find_offset_in_stmts(&finalizer.stmts);
                }
                found
            }
            _ => None,
        }
    }

    #[must_use]
    fn find_offset_in_expr(expr: &Expr) -> Option<i32> {
        match expr {
            Expr::Assign(assign) => {
                if let Some(off) = Self::extract_offset_from_assign(assign) {
                    return Some(off);
                }
                Self::find_offset_in_expr(&assign.right)
            }
            Expr::Seq(seq) => seq
                .exprs
                .iter()
                .find_map(|expr| Self::find_offset_in_expr(expr)),
            Expr::Paren(paren) => Self::find_offset_in_expr(&paren.expr),
            Expr::Cond(cond) => Self::find_offset_in_expr(&cond.test)
                .or_else(|| Self::find_offset_in_expr(&cond.cons))
                .or_else(|| Self::find_offset_in_expr(&cond.alt)),
            Expr::Bin(bin) => Self::find_offset_in_expr(&bin.left)
                .or_else(|| Self::find_offset_in_expr(&bin.right)),
            Expr::Unary(unary) => Self::find_offset_in_expr(&unary.arg),
            Expr::Call(call) => {
                for arg in &call.args {
                    if let Some(off) = Self::find_offset_in_expr(&arg.expr) {
                        return Some(off);
                    }
                }
                None
            }
            _ => None,
        }
    }

    #[must_use]
    fn find_offset_in_self_assignment(stmts: &[Stmt], fn_name: &str) -> Option<i32> {
        for stmt in stmts {
            if let Some(off) = Self::find_offset_in_self_stmt(stmt, fn_name) {
                return Some(off);
            }
        }
        None
    }

    #[must_use]
    fn find_offset_in_self_stmt(stmt: &Stmt, fn_name: &str) -> Option<i32> {
        match stmt {
            Stmt::Expr(expr_stmt) => Self::find_offset_in_self_expr(&expr_stmt.expr, fn_name),
            Stmt::Return(ret) => {
                let arg = ret.arg.as_ref()?;
                Self::find_offset_in_self_expr(arg, fn_name)
            }
            Stmt::Block(block) => Self::find_offset_in_self_assignment(&block.stmts, fn_name),
            Stmt::If(if_stmt) => {
                Self::find_offset_in_self_stmt(&if_stmt.cons, fn_name).or_else(|| {
                    let alt = if_stmt.alt.as_ref()?;
                    Self::find_offset_in_self_stmt(alt, fn_name)
                })
            }
            Stmt::While(while_stmt) => Self::find_offset_in_self_stmt(&while_stmt.body, fn_name),
            Stmt::For(for_stmt) => Self::find_offset_in_self_stmt(&for_stmt.body, fn_name),
            Stmt::ForIn(for_in) => Self::find_offset_in_self_stmt(&for_in.body, fn_name),
            Stmt::ForOf(for_of) => Self::find_offset_in_self_stmt(&for_of.body, fn_name),
            Stmt::Try(try_stmt) => {
                let mut found =
                    Self::find_offset_in_self_assignment(&try_stmt.block.stmts, fn_name);
                if found.is_none()
                    && let Some(handler) = &try_stmt.handler
                {
                    found = Self::find_offset_in_self_assignment(&handler.body.stmts, fn_name);
                }
                if found.is_none()
                    && let Some(finalizer) = &try_stmt.finalizer
                {
                    found = Self::find_offset_in_self_assignment(&finalizer.stmts, fn_name);
                }
                found
            }
            _ => None,
        }
    }

    #[must_use]
    fn find_offset_in_self_expr(expr: &Expr, fn_name: &str) -> Option<i32> {
        match expr {
            Expr::Assign(assign) => {
                if let Some(left_name) = Self::assign_left_name(assign)
                    && left_name == fn_name
                    && let Expr::Fn(fn_expr) = Self::strip_parens(&assign.right)
                    && let Some(fn_body) = &fn_expr.function.body
                    && let Some(off) = Self::find_offset_in_stmts(&fn_body.stmts)
                {
                    return Some(off);
                }
                Self::find_offset_in_self_expr(&assign.right, fn_name)
            }
            Expr::Seq(seq) => seq
                .exprs
                .iter()
                .find_map(|expr| Self::find_offset_in_self_expr(expr, fn_name)),
            Expr::Paren(paren) => Self::find_offset_in_self_expr(&paren.expr, fn_name),
            Expr::Call(call) => {
                for arg in &call.args {
                    if let Some(off) = Self::find_offset_in_self_expr(&arg.expr, fn_name) {
                        return Some(off);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Extract charset from function body (65 chars for Base64)
    #[must_use]
    fn extract_charset(stmts: &[Stmt]) -> Option<String> {
        for stmt in stmts {
            if let Stmt::Decl(Decl::Var(var_decl)) = stmt {
                for decl in &var_decl.decls {
                    if let Some(init) = &decl.init
                        && let Expr::Lit(Lit::Str(s)) = &**init
                        && let Some(v) = s.value.as_str()
                        && v.len() == 65
                    {
                        return Some(v.to_owned());
                    }
                }
            }
        }
        None
    }

    fn scan_decoder_helpers_in_stmts(
        stmts: &[Stmt],
        charset: &mut Option<String>,
        rc4_found: &mut bool,
    ) {
        for stmt in stmts {
            Self::scan_decoder_helpers_in_stmt(stmt, charset, rc4_found);
        }
    }

    fn scan_decoder_helpers_in_stmt(
        stmt: &Stmt,
        charset: &mut Option<String>,
        rc4_found: &mut bool,
    ) {
        match stmt {
            Stmt::Expr(expr_stmt) => {
                Self::scan_decoder_helpers_in_expr(&expr_stmt.expr, charset, rc4_found);
            }
            Stmt::Decl(Decl::Var(var_decl)) => {
                for decl in &var_decl.decls {
                    if let Some(init) = &decl.init {
                        Self::scan_decoder_helpers_in_expr(init, charset, rc4_found);
                    }
                }
            }
            Stmt::Decl(Decl::Fn(fn_decl)) => {
                if let Some(body) = &fn_decl.function.body {
                    Self::scan_decoder_helpers_in_stmts(&body.stmts, charset, rc4_found);
                }
            }
            Stmt::Block(block) => {
                Self::scan_decoder_helpers_in_stmts(&block.stmts, charset, rc4_found);
            }
            Stmt::If(if_stmt) => {
                Self::scan_decoder_helpers_in_expr(&if_stmt.test, charset, rc4_found);
                Self::scan_decoder_helpers_in_stmt(&if_stmt.cons, charset, rc4_found);
                if let Some(alt) = &if_stmt.alt {
                    Self::scan_decoder_helpers_in_stmt(alt, charset, rc4_found);
                }
            }
            Stmt::While(while_stmt) => {
                Self::scan_decoder_helpers_in_expr(&while_stmt.test, charset, rc4_found);
                Self::scan_decoder_helpers_in_stmt(&while_stmt.body, charset, rc4_found);
            }
            Stmt::For(for_stmt) => {
                if let Some(init) = &for_stmt.init {
                    match init {
                        VarDeclOrExpr::VarDecl(var_decl) => {
                            for decl in &var_decl.decls {
                                if let Some(init) = &decl.init {
                                    Self::scan_decoder_helpers_in_expr(init, charset, rc4_found);
                                }
                            }
                        }
                        VarDeclOrExpr::Expr(expr) => {
                            Self::scan_decoder_helpers_in_expr(expr, charset, rc4_found);
                        }
                    }
                }
                if let Some(test) = &for_stmt.test {
                    Self::scan_decoder_helpers_in_expr(test, charset, rc4_found);
                }
                if let Some(update) = &for_stmt.update {
                    Self::scan_decoder_helpers_in_expr(update, charset, rc4_found);
                }
                Self::scan_decoder_helpers_in_stmt(&for_stmt.body, charset, rc4_found);
            }
            Stmt::ForIn(for_in) => {
                Self::scan_decoder_helpers_in_expr(&for_in.right, charset, rc4_found);
                Self::scan_decoder_helpers_in_stmt(&for_in.body, charset, rc4_found);
            }
            Stmt::ForOf(for_of) => {
                Self::scan_decoder_helpers_in_expr(&for_of.right, charset, rc4_found);
                Self::scan_decoder_helpers_in_stmt(&for_of.body, charset, rc4_found);
            }
            Stmt::Return(ret) => {
                if let Some(arg) = &ret.arg {
                    Self::scan_decoder_helpers_in_expr(arg, charset, rc4_found);
                }
            }
            Stmt::Try(try_stmt) => {
                Self::scan_decoder_helpers_in_stmts(&try_stmt.block.stmts, charset, rc4_found);
                if let Some(handler) = &try_stmt.handler {
                    Self::scan_decoder_helpers_in_stmts(&handler.body.stmts, charset, rc4_found);
                }
                if let Some(finalizer) = &try_stmt.finalizer {
                    Self::scan_decoder_helpers_in_stmts(&finalizer.stmts, charset, rc4_found);
                }
            }
            _ => {}
        }
    }

    fn scan_decoder_helpers_in_expr(
        expr: &Expr,
        charset: &mut Option<String>,
        rc4_found: &mut bool,
    ) {
        match expr {
            Expr::Fn(fn_expr) => {
                if let Some(body) = &fn_expr.function.body {
                    if charset.is_none()
                        && let Some(cs) = Self::extract_charset(&body.stmts)
                    {
                        *charset = Some(cs);
                    }
                    if !*rc4_found && Self::looks_like_rc4(&body.stmts) {
                        *rc4_found = true;
                    }
                    Self::scan_decoder_helpers_in_stmts(&body.stmts, charset, rc4_found);
                }
            }
            Expr::Call(call) => {
                if let Callee::Expr(callee) = &call.callee {
                    Self::scan_decoder_helpers_in_expr(callee, charset, rc4_found);
                }
                for arg in &call.args {
                    Self::scan_decoder_helpers_in_expr(&arg.expr, charset, rc4_found);
                }
            }
            Expr::Member(member) => {
                Self::scan_decoder_helpers_in_expr(&member.obj, charset, rc4_found);
                if let MemberProp::Computed(computed) = &member.prop {
                    Self::scan_decoder_helpers_in_expr(&computed.expr, charset, rc4_found);
                }
            }
            Expr::Assign(assign) => {
                Self::scan_decoder_helpers_in_expr(&assign.right, charset, rc4_found);
                if let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = &assign.left {
                    Self::scan_decoder_helpers_in_expr(&member.obj, charset, rc4_found);
                }
            }
            Expr::Bin(bin) => {
                Self::scan_decoder_helpers_in_expr(&bin.left, charset, rc4_found);
                Self::scan_decoder_helpers_in_expr(&bin.right, charset, rc4_found);
            }
            Expr::Unary(unary) => {
                Self::scan_decoder_helpers_in_expr(&unary.arg, charset, rc4_found);
            }
            Expr::Paren(paren) => {
                Self::scan_decoder_helpers_in_expr(&paren.expr, charset, rc4_found);
            }
            Expr::Seq(seq) => {
                for item in &seq.exprs {
                    Self::scan_decoder_helpers_in_expr(item, charset, rc4_found);
                }
            }
            Expr::Cond(cond) => {
                Self::scan_decoder_helpers_in_expr(&cond.test, charset, rc4_found);
                Self::scan_decoder_helpers_in_expr(&cond.cons, charset, rc4_found);
                Self::scan_decoder_helpers_in_expr(&cond.alt, charset, rc4_found);
            }
            Expr::Array(arr) => {
                for elem in arr.elems.iter().flatten() {
                    Self::scan_decoder_helpers_in_expr(&elem.expr, charset, rc4_found);
                }
            }
            Expr::Object(obj) => {
                for prop in &obj.props {
                    if let PropOrSpread::Prop(prop) = prop {
                        match &**prop {
                            Prop::KeyValue(kv) => {
                                Self::scan_decoder_helpers_in_expr(&kv.value, charset, rc4_found);
                            }
                            Prop::Method(method) => {
                                if let Some(body) = &method.function.body {
                                    Self::scan_decoder_helpers_in_stmts(
                                        &body.stmts,
                                        charset,
                                        rc4_found,
                                    );
                                }
                            }
                            Prop::Getter(getter) => {
                                if let Some(body) = &getter.body {
                                    Self::scan_decoder_helpers_in_stmts(
                                        &body.stmts,
                                        charset,
                                        rc4_found,
                                    );
                                }
                            }
                            Prop::Setter(setter) => {
                                if let Some(body) = &setter.body {
                                    Self::scan_decoder_helpers_in_stmts(
                                        &body.stmts,
                                        charset,
                                        rc4_found,
                                    );
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }

    #[must_use]
    fn stmt_contains_member_prop(stmt: &Stmt, prop: &str) -> bool {
        match stmt {
            Stmt::Expr(expr_stmt) => Self::expr_contains_member_prop(&expr_stmt.expr, prop),
            Stmt::Decl(Decl::Var(var_decl)) => var_decl.decls.iter().any(|decl| {
                decl.init
                    .as_ref()
                    .is_some_and(|init| Self::expr_contains_member_prop(init, prop))
            }),
            Stmt::Decl(Decl::Fn(fn_decl)) => fn_decl.function.body.as_ref().is_some_and(|body| {
                body.stmts
                    .iter()
                    .any(|s| Self::stmt_contains_member_prop(s, prop))
            }),
            Stmt::Block(block) => block
                .stmts
                .iter()
                .any(|s| Self::stmt_contains_member_prop(s, prop)),
            Stmt::If(if_stmt) => {
                Self::expr_contains_member_prop(&if_stmt.test, prop)
                    || Self::stmt_contains_member_prop(&if_stmt.cons, prop)
                    || if_stmt
                        .alt
                        .as_ref()
                        .is_some_and(|alt| Self::stmt_contains_member_prop(alt, prop))
            }
            Stmt::While(while_stmt) => {
                Self::expr_contains_member_prop(&while_stmt.test, prop)
                    || Self::stmt_contains_member_prop(&while_stmt.body, prop)
            }
            Stmt::For(for_stmt) => {
                let init_has = match &for_stmt.init {
                    Some(VarDeclOrExpr::VarDecl(var_decl)) => var_decl.decls.iter().any(|decl| {
                        decl.init
                            .as_ref()
                            .is_some_and(|init| Self::expr_contains_member_prop(init, prop))
                    }),
                    Some(VarDeclOrExpr::Expr(expr)) => Self::expr_contains_member_prop(expr, prop),
                    None => false,
                };
                init_has
                    || for_stmt
                        .test
                        .as_ref()
                        .is_some_and(|test| Self::expr_contains_member_prop(test, prop))
                    || for_stmt
                        .update
                        .as_ref()
                        .is_some_and(|update| Self::expr_contains_member_prop(update, prop))
                    || Self::stmt_contains_member_prop(&for_stmt.body, prop)
            }
            Stmt::ForIn(for_in) => {
                Self::expr_contains_member_prop(&for_in.right, prop)
                    || Self::stmt_contains_member_prop(&for_in.body, prop)
            }
            Stmt::ForOf(for_of) => {
                Self::expr_contains_member_prop(&for_of.right, prop)
                    || Self::stmt_contains_member_prop(&for_of.body, prop)
            }
            Stmt::Return(ret) => ret
                .arg
                .as_ref()
                .is_some_and(|arg| Self::expr_contains_member_prop(arg, prop)),
            Stmt::Try(try_stmt) => {
                try_stmt
                    .block
                    .stmts
                    .iter()
                    .any(|s| Self::stmt_contains_member_prop(s, prop))
                    || try_stmt.handler.as_ref().is_some_and(|handler| {
                        handler
                            .body
                            .stmts
                            .iter()
                            .any(|s| Self::stmt_contains_member_prop(s, prop))
                    })
                    || try_stmt.finalizer.as_ref().is_some_and(|finalizer| {
                        finalizer
                            .stmts
                            .iter()
                            .any(|s| Self::stmt_contains_member_prop(s, prop))
                    })
            }
            _ => false,
        }
    }

    #[must_use]
    fn expr_contains_member_prop(expr: &Expr, prop_name: &str) -> bool {
        match expr {
            Expr::Member(member) => {
                let prop_match = match &member.prop {
                    MemberProp::Ident(ident) => ident.sym == prop_name,
                    MemberProp::Computed(computed) => {
                        if let Expr::Lit(Lit::Str(s)) = &*computed.expr {
                            s.value.as_str() == Some(prop_name)
                        } else {
                            false
                        }
                    }
                    MemberProp::PrivateName(_) => false,
                };
                prop_match || Self::expr_contains_member_prop(&member.obj, prop_name)
            }
            Expr::Call(call) => {
                let callee_match = match &call.callee {
                    Callee::Expr(callee) => Self::expr_contains_member_prop(callee, prop_name),
                    _ => false,
                };
                if callee_match {
                    return true;
                }
                call.args
                    .iter()
                    .any(|arg| Self::expr_contains_member_prop(&arg.expr, prop_name))
            }
            Expr::Assign(assign) => {
                Self::expr_contains_member_prop(&assign.right, prop_name)
                    || match &assign.left {
                        AssignTarget::Simple(SimpleAssignTarget::Member(member)) => {
                            Self::expr_contains_member_prop(&member.obj, prop_name)
                        }
                        _ => false,
                    }
            }
            Expr::Bin(bin) => {
                Self::expr_contains_member_prop(&bin.left, prop_name)
                    || Self::expr_contains_member_prop(&bin.right, prop_name)
            }
            Expr::Unary(unary) => Self::expr_contains_member_prop(&unary.arg, prop_name),
            Expr::Paren(paren) => Self::expr_contains_member_prop(&paren.expr, prop_name),
            Expr::Seq(seq) => seq
                .exprs
                .iter()
                .any(|e| Self::expr_contains_member_prop(e, prop_name)),
            Expr::Cond(cond) => {
                Self::expr_contains_member_prop(&cond.test, prop_name)
                    || Self::expr_contains_member_prop(&cond.cons, prop_name)
                    || Self::expr_contains_member_prop(&cond.alt, prop_name)
            }
            Expr::Array(arr) => arr
                .elems
                .iter()
                .flatten()
                .any(|elem| Self::expr_contains_member_prop(&elem.expr, prop_name)),
            Expr::Object(obj) => obj.props.iter().any(|prop_item| match prop_item {
                PropOrSpread::Prop(prop) => match &**prop {
                    Prop::KeyValue(kv) => Self::expr_contains_member_prop(&kv.value, prop_name),
                    Prop::Method(method) => method.function.body.as_ref().is_some_and(|body| {
                        body.stmts
                            .iter()
                            .any(|s| Self::stmt_contains_member_prop(s, prop_name))
                    }),
                    Prop::Getter(getter) => getter.body.as_ref().is_some_and(|body| {
                        body.stmts
                            .iter()
                            .any(|s| Self::stmt_contains_member_prop(s, prop_name))
                    }),
                    Prop::Setter(setter) => setter.body.as_ref().is_some_and(|body| {
                        body.stmts
                            .iter()
                            .any(|s| Self::stmt_contains_member_prop(s, prop_name))
                    }),
                    _ => false,
                },
                PropOrSpread::Spread(_) => false,
            }),
            _ => false,
        }
    }

    #[must_use]
    fn stmt_contains_bitxor(stmt: &Stmt) -> bool {
        match stmt {
            Stmt::Expr(expr_stmt) => Self::expr_contains_bitxor(&expr_stmt.expr),
            Stmt::Decl(Decl::Var(var_decl)) => var_decl.decls.iter().any(|decl| {
                decl.init
                    .as_ref()
                    .is_some_and(|init| Self::expr_contains_bitxor(init))
            }),
            Stmt::Decl(Decl::Fn(fn_decl)) => fn_decl
                .function
                .body
                .as_ref()
                .is_some_and(|body| body.stmts.iter().any(Self::stmt_contains_bitxor)),
            Stmt::Block(block) => block.stmts.iter().any(Self::stmt_contains_bitxor),
            Stmt::If(if_stmt) => {
                Self::expr_contains_bitxor(&if_stmt.test)
                    || Self::stmt_contains_bitxor(&if_stmt.cons)
                    || if_stmt
                        .alt
                        .as_ref()
                        .is_some_and(|alt| Self::stmt_contains_bitxor(alt))
            }
            Stmt::While(while_stmt) => {
                Self::expr_contains_bitxor(&while_stmt.test)
                    || Self::stmt_contains_bitxor(&while_stmt.body)
            }
            Stmt::For(for_stmt) => {
                let init_has = match &for_stmt.init {
                    Some(VarDeclOrExpr::VarDecl(var_decl)) => var_decl.decls.iter().any(|decl| {
                        decl.init
                            .as_ref()
                            .is_some_and(|init| Self::expr_contains_bitxor(init))
                    }),
                    Some(VarDeclOrExpr::Expr(expr)) => Self::expr_contains_bitxor(expr),
                    None => false,
                };
                init_has
                    || for_stmt
                        .test
                        .as_ref()
                        .is_some_and(|test| Self::expr_contains_bitxor(test))
                    || for_stmt
                        .update
                        .as_ref()
                        .is_some_and(|update| Self::expr_contains_bitxor(update))
                    || Self::stmt_contains_bitxor(&for_stmt.body)
            }
            Stmt::ForIn(for_in) => {
                Self::expr_contains_bitxor(&for_in.right)
                    || Self::stmt_contains_bitxor(&for_in.body)
            }
            Stmt::ForOf(for_of) => {
                Self::expr_contains_bitxor(&for_of.right)
                    || Self::stmt_contains_bitxor(&for_of.body)
            }
            Stmt::Return(ret) => ret
                .arg
                .as_ref()
                .is_some_and(|arg| Self::expr_contains_bitxor(arg)),
            Stmt::Try(try_stmt) => {
                try_stmt.block.stmts.iter().any(Self::stmt_contains_bitxor)
                    || try_stmt.handler.as_ref().is_some_and(|handler| {
                        handler.body.stmts.iter().any(Self::stmt_contains_bitxor)
                    })
                    || try_stmt.finalizer.as_ref().is_some_and(|finalizer| {
                        finalizer.stmts.iter().any(Self::stmt_contains_bitxor)
                    })
            }
            _ => false,
        }
    }

    #[must_use]
    fn expr_contains_bitxor(expr: &Expr) -> bool {
        match expr {
            Expr::Bin(bin) => {
                bin.op == BinaryOp::BitXor
                    || Self::expr_contains_bitxor(&bin.left)
                    || Self::expr_contains_bitxor(&bin.right)
            }
            Expr::Unary(unary) => Self::expr_contains_bitxor(&unary.arg),
            Expr::Paren(paren) => Self::expr_contains_bitxor(&paren.expr),
            Expr::Seq(seq) => seq
                .exprs
                .iter()
                .any(|expr| Self::expr_contains_bitxor(expr)),
            Expr::Cond(cond) => {
                Self::expr_contains_bitxor(&cond.test)
                    || Self::expr_contains_bitxor(&cond.cons)
                    || Self::expr_contains_bitxor(&cond.alt)
            }
            Expr::Call(call) => {
                if let Callee::Expr(callee) = &call.callee
                    && Self::expr_contains_bitxor(callee)
                {
                    return true;
                }
                call.args
                    .iter()
                    .any(|arg| Self::expr_contains_bitxor(&arg.expr))
            }
            Expr::Assign(assign) => {
                Self::expr_contains_bitxor(&assign.right)
                    || match &assign.left {
                        AssignTarget::Simple(SimpleAssignTarget::Member(member)) => {
                            Self::expr_contains_bitxor(&member.obj)
                        }
                        _ => false,
                    }
            }
            Expr::Member(member) => Self::expr_contains_bitxor(&member.obj),
            Expr::Array(arr) => arr
                .elems
                .iter()
                .flatten()
                .any(|elem| Self::expr_contains_bitxor(&elem.expr)),
            Expr::Object(obj) => obj.props.iter().any(|prop| match prop {
                PropOrSpread::Prop(prop) => match &**prop {
                    Prop::KeyValue(kv) => Self::expr_contains_bitxor(&kv.value),
                    Prop::Method(method) => method
                        .function
                        .body
                        .as_ref()
                        .is_some_and(|body| body.stmts.iter().any(Self::stmt_contains_bitxor)),
                    Prop::Getter(getter) => getter
                        .body
                        .as_ref()
                        .is_some_and(|body| body.stmts.iter().any(Self::stmt_contains_bitxor)),
                    Prop::Setter(setter) => setter
                        .body
                        .as_ref()
                        .is_some_and(|body| body.stmts.iter().any(Self::stmt_contains_bitxor)),
                    _ => false,
                },
                PropOrSpread::Spread(_) => false,
            }),
            _ => false,
        }
    }

    #[must_use]
    fn stmt_contains_number(stmt: &Stmt, value: f64) -> bool {
        match stmt {
            Stmt::Expr(expr_stmt) => Self::expr_contains_number(&expr_stmt.expr, value),
            Stmt::Decl(Decl::Var(var_decl)) => var_decl.decls.iter().any(|decl| {
                decl.init
                    .as_ref()
                    .is_some_and(|init| Self::expr_contains_number(init, value))
            }),
            Stmt::Decl(Decl::Fn(fn_decl)) => fn_decl.function.body.as_ref().is_some_and(|body| {
                body.stmts
                    .iter()
                    .any(|s| Self::stmt_contains_number(s, value))
            }),
            Stmt::Block(block) => block
                .stmts
                .iter()
                .any(|s| Self::stmt_contains_number(s, value)),
            Stmt::If(if_stmt) => {
                Self::expr_contains_number(&if_stmt.test, value)
                    || Self::stmt_contains_number(&if_stmt.cons, value)
                    || if_stmt
                        .alt
                        .as_ref()
                        .is_some_and(|alt| Self::stmt_contains_number(alt, value))
            }
            Stmt::While(while_stmt) => {
                Self::expr_contains_number(&while_stmt.test, value)
                    || Self::stmt_contains_number(&while_stmt.body, value)
            }
            Stmt::For(for_stmt) => {
                let init_has = match &for_stmt.init {
                    Some(VarDeclOrExpr::VarDecl(var_decl)) => var_decl.decls.iter().any(|decl| {
                        decl.init
                            .as_ref()
                            .is_some_and(|init| Self::expr_contains_number(init, value))
                    }),
                    Some(VarDeclOrExpr::Expr(expr)) => Self::expr_contains_number(expr, value),
                    None => false,
                };
                init_has
                    || for_stmt
                        .test
                        .as_ref()
                        .is_some_and(|test| Self::expr_contains_number(test, value))
                    || for_stmt
                        .update
                        .as_ref()
                        .is_some_and(|update| Self::expr_contains_number(update, value))
                    || Self::stmt_contains_number(&for_stmt.body, value)
            }
            Stmt::ForIn(for_in) => {
                Self::expr_contains_number(&for_in.right, value)
                    || Self::stmt_contains_number(&for_in.body, value)
            }
            Stmt::ForOf(for_of) => {
                Self::expr_contains_number(&for_of.right, value)
                    || Self::stmt_contains_number(&for_of.body, value)
            }
            Stmt::Return(ret) => ret
                .arg
                .as_ref()
                .is_some_and(|arg| Self::expr_contains_number(arg, value)),
            Stmt::Try(try_stmt) => {
                try_stmt
                    .block
                    .stmts
                    .iter()
                    .any(|s| Self::stmt_contains_number(s, value))
                    || try_stmt.handler.as_ref().is_some_and(|handler| {
                        handler
                            .body
                            .stmts
                            .iter()
                            .any(|s| Self::stmt_contains_number(s, value))
                    })
                    || try_stmt.finalizer.as_ref().is_some_and(|finalizer| {
                        finalizer
                            .stmts
                            .iter()
                            .any(|s| Self::stmt_contains_number(s, value))
                    })
            }
            _ => false,
        }
    }

    #[must_use]
    #[expect(
        clippy::float_arithmetic,
        reason = "JS numeric comparisons use f64 epsilon"
    )]
    fn expr_contains_number(expr: &Expr, value: f64) -> bool {
        match expr {
            Expr::Lit(Lit::Num(n)) => (n.value - value).abs() < f64::EPSILON,
            Expr::Unary(unary) => Self::expr_contains_number(&unary.arg, value),
            Expr::Paren(paren) => Self::expr_contains_number(&paren.expr, value),
            Expr::Bin(bin) => {
                Self::expr_contains_number(&bin.left, value)
                    || Self::expr_contains_number(&bin.right, value)
            }
            Expr::Seq(seq) => seq
                .exprs
                .iter()
                .any(|e| Self::expr_contains_number(e, value)),
            Expr::Cond(cond) => {
                Self::expr_contains_number(&cond.test, value)
                    || Self::expr_contains_number(&cond.cons, value)
                    || Self::expr_contains_number(&cond.alt, value)
            }
            Expr::Call(call) => {
                if let Callee::Expr(callee) = &call.callee
                    && Self::expr_contains_number(callee, value)
                {
                    return true;
                }
                call.args
                    .iter()
                    .any(|arg| Self::expr_contains_number(&arg.expr, value))
            }
            Expr::Assign(assign) => {
                Self::expr_contains_number(&assign.right, value)
                    || match &assign.left {
                        AssignTarget::Simple(SimpleAssignTarget::Member(member)) => {
                            Self::expr_contains_number(&member.obj, value)
                        }
                        _ => false,
                    }
            }
            Expr::Member(member) => Self::expr_contains_number(&member.obj, value),
            Expr::Array(arr) => arr
                .elems
                .iter()
                .flatten()
                .any(|elem| Self::expr_contains_number(&elem.expr, value)),
            Expr::Object(obj) => obj.props.iter().any(|prop| match prop {
                PropOrSpread::Prop(prop) => match &**prop {
                    Prop::KeyValue(kv) => Self::expr_contains_number(&kv.value, value),
                    Prop::Method(method) => method.function.body.as_ref().is_some_and(|body| {
                        body.stmts
                            .iter()
                            .any(|s| Self::stmt_contains_number(s, value))
                    }),
                    Prop::Getter(getter) => getter.body.as_ref().is_some_and(|body| {
                        body.stmts
                            .iter()
                            .any(|s| Self::stmt_contains_number(s, value))
                    }),
                    Prop::Setter(setter) => setter.body.as_ref().is_some_and(|body| {
                        body.stmts
                            .iter()
                            .any(|s| Self::stmt_contains_number(s, value))
                    }),
                    _ => false,
                },
                PropOrSpread::Spread(_) => false,
            }),
            _ => false,
        }
    }

    #[must_use]
    fn looks_like_rc4(stmts: &[Stmt]) -> bool {
        let has_xor = stmts.iter().any(Self::stmt_contains_bitxor);
        let has_char_code_at = stmts
            .iter()
            .any(|s| Self::stmt_contains_member_prop(s, "charCodeAt"));
        let has_from_char_code = stmts
            .iter()
            .any(|s| Self::stmt_contains_member_prop(s, "fromCharCode"));
        let has_256 = stmts.iter().any(|s| Self::stmt_contains_number(s, 256.0));
        has_xor && has_char_code_at && (has_from_char_code || has_256)
    }
}

impl VisitMut for DecoderFunctionFinder<'_> {
    fn visit_mut_fn_decl(&mut self, func: &mut FnDecl) {
        func.visit_mut_children_with(self);

        let fn_name = func.ident.sym.to_string();
        let string_array_names = self.get_string_array_names();

        let Some(body) = &func.function.body else {
            return;
        };

        // Filter empty statements
        let stmts: Vec<_> = body
            .stmts
            .iter()
            .filter(|s| !matches!(s, Stmt::Empty(_)))
            .collect();

        if stmts.is_empty() {
            return;
        }

        // Look for pattern:
        // var strArr = _0x1234();  // or strArr = _0x1234()
        // idx = idx - OFFSET;
        // return strArr[idx];

        let mut string_array_identifier = None;
        let mut offset: i32 = 0;
        let mut offset_found = false;
        let mut decoder_type = DecoderFunctionType::Simple;
        let mut charset = None;
        let mut rc4_found = false;
        let mut index_argument: usize = 0;
        let key_argument: usize = 1;

        Self::scan_decoder_helpers_in_stmts(&body.stmts, &mut charset, &mut rc4_found);

        for stmt in &stmts {
            match stmt {
                // Variable declaration: var x = stringArrayFn()
                Stmt::Decl(Decl::Var(var_decl)) => {
                    for decl in &var_decl.decls {
                        if let Some(init) = &decl.init {
                            // Call to string array function
                            if let Expr::Call(call) = &**init
                                && let Callee::Expr(callee) = &call.callee
                                && let Expr::Ident(ident) = &**callee
                            {
                                let name = ident.sym.to_string();
                                if string_array_names.contains(&name.as_str()) {
                                    string_array_identifier = Some(name);
                                }
                            }
                        }
                    }
                }
                // Expression statement: idx = idx - OFFSET
                Stmt::Expr(expr_stmt) => {
                    if let Expr::Assign(assign) = &*expr_stmt.expr
                        && let Some(off) = Self::extract_offset_from_assign(assign)
                    {
                        offset = off;
                        offset_found = true;
                    }
                }
                // Return statement
                Stmt::Return(ret) => {
                    // Check if return value contains sequence with function assignment
                    if let Some(arg) = &ret.arg
                        && let Some(seq) = Self::extract_seq_expr(arg)
                    {
                        for expr in &seq.exprs {
                            let expr = Self::strip_parens(expr);
                            if let Expr::Assign(assign) = expr
                                && let Expr::Fn(fn_expr) = Self::strip_parens(&assign.right)
                                && let Some(fn_body) = &fn_expr.function.body
                            {
                                // Look for offset in function body
                                for inner_stmt in &fn_body.stmts {
                                    if let Stmt::Expr(inner_expr) = inner_stmt
                                        && let Expr::Assign(inner_assign) = &*inner_expr.expr
                                        && let Some(off) =
                                            Self::extract_offset_from_assign(inner_assign)
                                    {
                                        offset = off;
                                        offset_found = true;
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if rc4_found {
            decoder_type = DecoderFunctionType::Rc4;
        } else if charset.is_some() {
            decoder_type = DecoderFunctionType::Base64;
        }

        if !offset_found
            && let Some(off) = Self::find_offset_in_self_assignment(&body.stmts, &fn_name)
        {
            offset = off;
        }

        if string_array_identifier.is_none() {
            if let Some(helper_match) = HelperCallFinder::find(
                self.string_arrays,
                self.helper_decoders,
                &func.function.params,
                body,
            ) {
                string_array_identifier = Some(helper_match.array_name);
                decoder_type = helper_match.decoder_type;
                charset = helper_match.charset;
                index_argument = helper_match.index_argument;
            } else if let Some(array_match) =
                ArrayAccessFinder::find(self.string_arrays, &func.function.params, body)
            {
                string_array_identifier = Some(array_match.array_name);
                index_argument = array_match.index_argument;
            }
        }

        // If we found a string array reference, create decoder
        if let Some(str_array_id) = string_array_identifier {
            self.decoders.push(DecoderFunction {
                identifier: fn_name,
                string_array_identifier: str_array_id,
                decoder_type,
                offset,
                index_argument,
                key_argument,
                charset,
            });
        }
    }
}

struct HelperCallMatch {
    array_name: String,
    decoder_type: DecoderFunctionType,
    charset: Option<String>,
    index_argument: usize,
}

struct HelperCallFinder<'a> {
    string_arrays: &'a [StringArray],
    helper_decoders: &'a HashMap<String, DecoderHelper>,
    params: &'a [Param],
    found: Option<HelperCallMatch>,
}

impl<'a> HelperCallFinder<'a> {
    #[must_use]
    fn find(
        string_arrays: &'a [StringArray],
        helper_decoders: &'a HashMap<String, DecoderHelper>,
        params: &'a [Param],
        body: &BlockStmt,
    ) -> Option<HelperCallMatch> {
        let mut finder = Self {
            string_arrays,
            helper_decoders,
            params,
            found: None,
        };
        body.visit_with(&mut finder);
        finder.found
    }

    #[must_use]
    fn param_index(&self, name: &str) -> Option<usize> {
        self.params.iter().position(|p| {
            if let Pat::Ident(binding) = &p.pat {
                binding.id.sym == name
            } else {
                false
            }
        })
    }

    fn resolve_string_array(&self, ident: &Ident) -> Option<&StringArray> {
        self.string_arrays
            .iter()
            .find(|array| ident.sym == array.identifier)
    }
}

impl Visit for HelperCallFinder<'_> {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        if self.found.is_some() {
            return;
        }

        if let Callee::Expr(callee) = &call.callee
            && let Expr::Ident(callee_ident) = &**callee
            && let Some(helper) = self.helper_decoders.get(callee_ident.sym.as_ref())
            && let Some(first_arg) = call.args.first()
            && let Expr::Member(member) = &*first_arg.expr
            && let Expr::Ident(obj_ident) = &*member.obj
            && let Some(array) = self.resolve_string_array(obj_ident)
            && let MemberProp::Computed(computed) = &member.prop
            && let Expr::Ident(index_ident) = &*computed.expr
            && let Some(index_argument) = self.param_index(index_ident.sym.as_ref())
        {
            self.found = Some(HelperCallMatch {
                array_name: array.identifier.clone(),
                decoder_type: helper.decoder_type,
                charset: helper.charset.clone(),
                index_argument,
            });
            return;
        }

        call.visit_children_with(self);
    }

    fn visit_fn_decl(&mut self, _func: &FnDecl) {}
    fn visit_fn_expr(&mut self, _func: &FnExpr) {}
    fn visit_arrow_expr(&mut self, _func: &ArrowExpr) {}
}

struct ArrayAccessMatch {
    array_name: String,
    index_argument: usize,
}

struct ArrayAccessFinder<'a> {
    string_arrays: &'a [StringArray],
    params: &'a [Param],
    found: Option<ArrayAccessMatch>,
}

impl<'a> ArrayAccessFinder<'a> {
    #[must_use]
    fn find(
        string_arrays: &'a [StringArray],
        params: &'a [Param],
        body: &BlockStmt,
    ) -> Option<ArrayAccessMatch> {
        let mut finder = Self {
            string_arrays,
            params,
            found: None,
        };
        body.visit_with(&mut finder);
        finder.found
    }

    #[must_use]
    fn param_index(&self, name: &str) -> Option<usize> {
        self.params.iter().position(|p| {
            if let Pat::Ident(binding) = &p.pat {
                binding.id.sym == name
            } else {
                false
            }
        })
    }

    fn resolve_string_array(&self, ident: &Ident) -> Option<&StringArray> {
        self.string_arrays
            .iter()
            .find(|array| ident.sym == array.identifier)
    }
}

impl Visit for ArrayAccessFinder<'_> {
    fn visit_member_expr(&mut self, member: &MemberExpr) {
        if self.found.is_some() {
            return;
        }

        if let Expr::Ident(obj_ident) = &*member.obj
            && let Some(array) = self.resolve_string_array(obj_ident)
            && let MemberProp::Computed(computed) = &member.prop
            && let Expr::Ident(index_ident) = &*computed.expr
            && let Some(index_argument) = self.param_index(index_ident.sym.as_ref())
        {
            self.found = Some(ArrayAccessMatch {
                array_name: array.identifier.clone(),
                index_argument,
            });
            return;
        }

        member.visit_children_with(self);
    }

    fn visit_fn_decl(&mut self, _func: &FnDecl) {}
    fn visit_fn_expr(&mut self, _func: &FnExpr) {}
    fn visit_arrow_expr(&mut self, _func: &ArrowExpr) {}
}
