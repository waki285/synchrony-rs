//! Simplify transformer
//!
//! This transformer simplifies the AST by:
//! - Evaluating constant expressions (e.g., 1 + 2 -> 3)
//! - Converting !0/!1 to true/false
//! - Simplifying string concatenation
//! - Simplifying comparison expressions
//! - Converting single statements to blocks

use std::collections::{HashMap, HashSet};

use swc_ecma_ast::*;
use swc_ecma_visit::{VisitMut, VisitMutWith};

use crate::context::Context;
use crate::error::Result;
use crate::transformers::Transformer;

/// Simplify transformer.
///
/// Performs constant folding and small AST simplifications.
#[derive(Debug)]
pub struct Simplify;

impl Simplify {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for Simplify {
    fn default() -> Self {
        Self::new()
    }
}

impl Transformer for Simplify {
    fn name(&self) -> &'static str {
        "Simplify"
    }

    fn transform(&self, context: &mut Context) -> Result<()> {
        // Run simplification visitor
        let mut visitor = SimplifyVisitor;
        context.ast.visit_mut_with(&mut visitor);

        // Run fixup visitor to handle edge cases
        let mut fixup = FixupVisitor;
        context.ast.visit_mut_with(&mut fixup);

        // Run logical expression transformer
        let mut logical = LogicalExpressionVisitor;
        context.ast.visit_mut_with(&mut logical);

        // Run proxy/IIFE simplifier
        let mut fix_proxies = FixProxiesVisitor;
        context.ast.visit_mut_with(&mut fix_proxies);

        Ok(())
    }
}

/// Visitor that performs simplification transformations
struct SimplifyVisitor;

impl SimplifyVisitor {
    /// Check if a binary operator is a math operator
    #[must_use]
    const fn is_math_operator(op: &BinaryOp) -> bool {
        matches!(
            op,
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div
        )
    }

    /// Check if a binary operator is a comparison operator
    #[must_use]
    const fn is_comparison_operator(op: &BinaryOp) -> bool {
        matches!(
            op,
            BinaryOp::EqEq
                | BinaryOp::EqEqEq
                | BinaryOp::NotEq
                | BinaryOp::NotEqEq
                | BinaryOp::Gt
                | BinaryOp::Lt
                | BinaryOp::GtEq
                | BinaryOp::LtEq
        )
    }

    /// Evaluate a binary math operation
    #[must_use]
    const fn eval_math(left: f64, op: &BinaryOp, right: f64) -> Option<f64> {
        let result = match op {
            BinaryOp::Add => left + right,
            BinaryOp::Sub => left - right,
            BinaryOp::Mul => left * right,
            BinaryOp::Div => {
                if right == 0.0 {
                    return None;
                }
                left / right
            }
            _ => return None,
        };

        if result.is_nan() || result.is_infinite() {
            None
        } else {
            Some(result)
        }
    }

    /// Evaluate a comparison operation on numbers
    #[must_use]
    const fn eval_comparison_num(left: f64, op: &BinaryOp, right: f64) -> Option<bool> {
        Some(match op {
            BinaryOp::EqEq | BinaryOp::EqEqEq => left == right,
            BinaryOp::NotEq | BinaryOp::NotEqEq => left != right,
            BinaryOp::Gt => left > right,
            BinaryOp::Lt => left < right,
            BinaryOp::GtEq => left >= right,
            BinaryOp::LtEq => left <= right,
            _ => return None,
        })
    }

    /// Evaluate a comparison operation on strings
    #[must_use]
    fn eval_comparison_str(left: &str, op: &BinaryOp, right: &str) -> Option<bool> {
        Some(match op {
            BinaryOp::EqEq | BinaryOp::EqEqEq => left == right,
            BinaryOp::NotEq | BinaryOp::NotEqEq => left != right,
            BinaryOp::Gt => left > right,
            BinaryOp::Lt => left < right,
            BinaryOp::GtEq => left >= right,
            BinaryOp::LtEq => left <= right,
            _ => return None,
        })
    }

    /// Extract a numeric value from a literal
    #[must_use]
    const fn get_numeric_value(lit: &Lit) -> Option<f64> {
        match lit {
            Lit::Num(n) => Some(n.value),
            _ => None,
        }
    }

    /// Extract a string value from a literal
    #[must_use]
    fn get_string_value(lit: &Lit) -> Option<String> {
        match lit {
            Lit::Str(s) => s.value.as_str().map(|s| s.to_string()),
            _ => None,
        }
    }

    /// Extract a numeric value from an expression (handles unary minus)
    #[must_use]
    fn get_expr_numeric_value(expr: &Expr) -> Option<f64> {
        match expr {
            Expr::Lit(lit) => Self::get_numeric_value(lit),
            Expr::Unary(UnaryExpr {
                op: UnaryOp::Minus,
                arg,
                ..
            }) => {
                if let Expr::Lit(lit) = &**arg {
                    Self::get_numeric_value(lit).map(|n| -n)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Create a number literal expression
    #[must_use]
    fn create_number_lit(value: f64) -> Expr {
        if value < 0.0 {
            Expr::Unary(UnaryExpr {
                span: Default::default(),
                op: UnaryOp::Minus,
                arg: Box::new(Expr::Lit(Lit::Num(Number {
                    span: Default::default(),
                    value: -value,
                    raw: None,
                }))),
            })
        } else {
            Expr::Lit(Lit::Num(Number {
                span: Default::default(),
                value,
                raw: None,
            }))
        }
    }

    /// Create a boolean literal expression
    #[must_use]
    fn create_bool_lit(value: bool) -> Expr {
        Expr::Lit(Lit::Bool(Bool {
            span: Default::default(),
            value,
        }))
    }

    /// Create a string literal expression
    #[must_use]
    fn create_string_lit(value: String) -> Expr {
        Expr::Lit(Lit::Str(Str {
            span: Default::default(),
            value: value.into(),
            raw: None,
        }))
    }

    fn normalize_string_raw(s: &mut Str) {
        let Some(raw) = &s.raw else {
            return;
        };
        let raw_str = raw.as_ref();
        if !raw_str.contains("\\x") {
            return;
        }
        if let Some(value) = s.value.as_str()
            && value.is_ascii()
        {
            s.raw = None;
        }
    }

    #[must_use]
    fn parse_int_like(input: &str, radix: Option<i32>) -> Option<f64> {
        let trimmed = input.trim_start();
        if trimmed.is_empty() {
            return None;
        }

        let mut rest = trimmed;
        let mut sign = 1.0;
        if let Some(stripped) = rest.strip_prefix('-') {
            sign = -1.0;
            rest = stripped;
        } else if let Some(stripped) = rest.strip_prefix('+') {
            rest = stripped;
        }

        let mut radix = radix.unwrap_or(0);
        if radix == 0 {
            if rest.starts_with("0x") || rest.starts_with("0X") {
                radix = 16;
                rest = &rest[2..];
            } else {
                radix = 10;
            }
        } else if radix == 16 && (rest.starts_with("0x") || rest.starts_with("0X")) {
            rest = &rest[2..];
        }

        if !(2..=36).contains(&radix) {
            return None;
        }

        let mut value = 0.0;
        let mut any = false;
        for ch in rest.chars() {
            let digit = match ch.to_digit(36) {
                Some(d) if (d as i32) < radix => d as f64,
                _ => break,
            };
            any = true;
            value = value * (radix as f64) + digit;
        }

        if !any {
            return None;
        }

        Some(sign * value)
    }
}

impl VisitMut for SimplifyVisitor {
    fn visit_mut_str(&mut self, s: &mut Str) {
        Self::normalize_string_raw(s);
    }

    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        // First, visit children
        expr.visit_mut_children_with(self);

        // Normalize hex numeric literals to decimal output
        if let Expr::Lit(Lit::Num(num)) = expr
            && let Some(raw) = &num.raw
        {
            let raw_str = raw.as_ref();
            let trimmed = raw_str
                .strip_prefix('-')
                .or_else(|| raw_str.strip_prefix('+'))
                .unwrap_or(raw_str);
            if trimmed.starts_with("0x") || trimmed.starts_with("0X") {
                num.raw = None;
            }
        }

        // Then, try to simplify this expression
        match expr {
            // Simplify parseInt("...") to number when possible
            Expr::Call(call) => {
                if let Callee::Expr(callee) = &call.callee
                    && let Expr::Ident(ident) = &**callee
                    && ident.sym == "parseInt"
                    && !call.args.is_empty()
                    && let Expr::Lit(Lit::Str(s)) = &*call.args[0].expr
                    && let Some(text) = s.value.as_str()
                {
                    let radix = if call.args.len() >= 2 {
                        if let Expr::Lit(Lit::Num(n)) = &*call.args[1].expr {
                            let r = n.value as i32;
                            Some(r)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(num) = Self::parse_int_like(text, radix)
                        && num.is_finite()
                    {
                        *expr = Self::create_number_lit(num);
                    }
                }
            }
            // Simplify binary expressions
            Expr::Bin(bin) => {
                // Math operations on numbers
                if Self::is_math_operator(&bin.op)
                    && let (Some(left), Some(right)) = (
                        Self::get_expr_numeric_value(&bin.left),
                        Self::get_expr_numeric_value(&bin.right),
                    )
                    && let Some(result) = Self::eval_math(left, &bin.op, right)
                {
                    *expr = Self::create_number_lit(result);
                    return;
                }

                // String concatenation
                if bin.op == BinaryOp::Add
                    && let (Expr::Lit(left_lit), Expr::Lit(right_lit)) = (&*bin.left, &*bin.right)
                    && let (Some(left), Some(right)) = (
                        Self::get_string_value(left_lit),
                        Self::get_string_value(right_lit),
                    )
                {
                    *expr = Self::create_string_lit(format!("{}{}", left, right));
                    return;
                }

                // Comparison operations
                if Self::is_comparison_operator(&bin.op) {
                    // Numeric comparison
                    if let (Some(left), Some(right)) = (
                        Self::get_expr_numeric_value(&bin.left),
                        Self::get_expr_numeric_value(&bin.right),
                    ) && let Some(result) = Self::eval_comparison_num(left, &bin.op, right)
                    {
                        *expr = Self::create_bool_lit(result);
                        return;
                    }

                    // String comparison
                    if let (Expr::Lit(left_lit), Expr::Lit(right_lit)) = (&*bin.left, &*bin.right)
                        && let (Some(left), Some(right)) = (
                            Self::get_string_value(left_lit),
                            Self::get_string_value(right_lit),
                        )
                        && let Some(result) = Self::eval_comparison_str(&left, &bin.op, &right)
                    {
                        *expr = Self::create_bool_lit(result);
                        return;
                    }
                }

                // typeof simplification: typeof undefined === "undefined" -> true
                if Self::is_comparison_operator(&bin.op) {
                    // Check for typeof x === "type" or "type" === typeof x
                    let (typeof_expr, type_str) = match (&*bin.left, &*bin.right) {
                        (
                            Expr::Unary(UnaryExpr {
                                op: UnaryOp::TypeOf,
                                arg,
                                ..
                            }),
                            Expr::Lit(Lit::Str(s)),
                        ) => (Some(arg), s.value.as_str()),
                        (
                            Expr::Lit(Lit::Str(s)),
                            Expr::Unary(UnaryExpr {
                                op: UnaryOp::TypeOf,
                                arg,
                                ..
                            }),
                        ) => (Some(arg), s.value.as_str()),
                        _ => (None, None),
                    };

                    if let (Some(arg), Some(type_str)) = (typeof_expr, type_str) {
                        // typeof undefined === "undefined"
                        if let Expr::Ident(ident) = &**arg
                            && ident.sym == "undefined"
                        {
                            let matches = type_str == "undefined";
                            let result = match bin.op {
                                BinaryOp::EqEq | BinaryOp::EqEqEq => matches,
                                BinaryOp::NotEq | BinaryOp::NotEqEq => !matches,
                                _ => return,
                            };
                            *expr = Self::create_bool_lit(result);
                        }
                    }
                }
            }

            // Simplify unary expressions
            Expr::Unary(unary) => {
                match unary.op {
                    // !0 -> true, !1 -> false
                    UnaryOp::Bang => {
                        match &*unary.arg {
                            // !0 -> true, !1 -> false
                            Expr::Lit(Lit::Num(n)) => {
                                if n.value == 0.0 {
                                    *expr = Self::create_bool_lit(true);
                                } else if n.value == 1.0 {
                                    *expr = Self::create_bool_lit(false);
                                }
                            }
                            // !true -> false, !false -> true
                            Expr::Lit(Lit::Bool(b)) => {
                                *expr = Self::create_bool_lit(!b.value);
                            }
                            // ![] -> false (empty array is truthy, so ![] is false)
                            Expr::Array(arr) if arr.elems.is_empty() => {
                                *expr = Self::create_bool_lit(false);
                            }
                            _ => {}
                        }
                    }
                    // -"0x123" -> -291 (hex string to negative number)
                    UnaryOp::Minus => {
                        if let Expr::Lit(Lit::Str(s)) = &*unary.arg
                            && let Some(str_val) = s.value.as_str()
                            && (str_val.starts_with("0x") || str_val.starts_with("0X"))
                        {
                            // Parse hex string
                            if let Ok(num) = i64::from_str_radix(&str_val[2..], 16) {
                                *expr = Self::create_number_lit(-(num as f64));
                            }
                        }
                    }
                    // +"0x123" -> 291 (hex string to number)
                    UnaryOp::Plus => {
                        if let Expr::Lit(Lit::Str(s)) = &*unary.arg
                            && let Some(str_val) = s.value.as_str()
                        {
                            if str_val.starts_with("0x") || str_val.starts_with("0X") {
                                // Parse hex string
                                if let Ok(num) = i64::from_str_radix(&str_val[2..], 16) {
                                    *expr = Self::create_number_lit(num as f64);
                                }
                            } else if let Ok(num) = str_val.parse::<f64>() {
                                // Regular numeric string
                                *expr = Self::create_number_lit(num);
                            }
                        }
                    }
                    _ => {}
                }
            }

            // Simplify conditional expressions with constant test
            Expr::Cond(cond) => {
                if let Expr::Lit(Lit::Bool(b)) = &*cond.test {
                    if b.value {
                        *expr = *cond.cons.clone();
                    } else {
                        *expr = *cond.alt.clone();
                    }
                }
            }

            _ => {}
        }
    }

    fn visit_mut_stmt(&mut self, stmt: &mut Stmt) {
        // First, visit children
        stmt.visit_mut_children_with(self);

        // Convert single statements to blocks for if/for/while
        match stmt {
            Stmt::If(if_stmt) => {
                // Convert consequent to block if needed
                if !matches!(&*if_stmt.cons, Stmt::Block(_)) {
                    let cons = std::mem::replace(
                        &mut *if_stmt.cons,
                        Stmt::Empty(EmptyStmt {
                            span: Default::default(),
                        }),
                    );
                    *if_stmt.cons = Stmt::Block(BlockStmt {
                        span: Default::default(),
                        stmts: vec![cons],
                        ..Default::default()
                    });
                }

                // Convert alternate to block if needed
                if let Some(alt) = &mut if_stmt.alt
                    && !matches!(&**alt, Stmt::Block(_) | Stmt::If(_))
                {
                    let alt_stmt = std::mem::replace(
                        &mut **alt,
                        Stmt::Empty(EmptyStmt {
                            span: Default::default(),
                        }),
                    );
                    **alt = Stmt::Block(BlockStmt {
                        span: Default::default(),
                        stmts: vec![alt_stmt],
                        ..Default::default()
                    });
                }
            }

            Stmt::For(for_stmt) => {
                if !matches!(&*for_stmt.body, Stmt::Block(_)) {
                    let body = std::mem::replace(
                        &mut *for_stmt.body,
                        Stmt::Empty(EmptyStmt {
                            span: Default::default(),
                        }),
                    );
                    *for_stmt.body = Stmt::Block(BlockStmt {
                        span: Default::default(),
                        stmts: vec![body],
                        ..Default::default()
                    });
                }
            }

            Stmt::While(while_stmt) => {
                if !matches!(&*while_stmt.body, Stmt::Block(_)) {
                    let body = std::mem::replace(
                        &mut *while_stmt.body,
                        Stmt::Empty(EmptyStmt {
                            span: Default::default(),
                        }),
                    );
                    *while_stmt.body = Stmt::Block(BlockStmt {
                        span: Default::default(),
                        stmts: vec![body],
                        ..Default::default()
                    });
                }
            }

            _ => {}
        }
    }
}


/// Fixup visitor - handles edge cases and cleanup
struct FixupVisitor;

impl VisitMut for FixupVisitor {
    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        expr.visit_mut_children_with(self);

        // Convert negative number literals to UnaryExpression (prevents codegen errors)
        if let Expr::Lit(Lit::Num(n)) = expr
            && n.value < 0.0
        {
            *expr = Expr::Unary(UnaryExpr {
                span: Default::default(),
                op: UnaryOp::Minus,
                arg: Box::new(Expr::Lit(Lit::Num(Number {
                    span: Default::default(),
                    value: n.value.abs(),
                    raw: None,
                }))),
            });
        }
    }

    fn visit_mut_var_decl(&mut self, decl: &mut VarDecl) {
        decl.visit_mut_children_with(self);

        // Remove declarators with EmptyStatement init
        decl.decls.retain(|d| {
            if let Some(init) = &d.init {
                // Can't check for EmptyStatement directly in expression context
                // This is handled elsewhere
                !matches!(&**init, Expr::Invalid(_))
            } else {
                true
            }
        });
    }
}

/// Logical expression transformer
/// Converts patterns like: a == b && (c(), d()) to if (a == b) { c(); d(); }
struct LogicalExpressionVisitor;

impl VisitMut for LogicalExpressionVisitor {
    fn visit_mut_stmt(&mut self, stmt: &mut Stmt) {
        stmt.visit_mut_children_with(self);

        // Check for ExpressionStatement with LogicalExpression
        if let Stmt::Expr(expr_stmt) = stmt
            && let Expr::Bin(bin) = &*expr_stmt.expr
            && bin.op == BinaryOp::LogicalAnd
        {
            // Check if right is a sequence expression (possibly wrapped in parens)
            let right = match &*bin.right {
                Expr::Paren(paren) => &*paren.expr,
                other => other,
            };

            if let Expr::Seq(seq) = right {
                // Convert: test && (a, b, c) -> if (test) { a; b; c; }
                let stmts: Vec<Stmt> = seq
                    .exprs
                    .iter()
                    .map(|e| {
                        Stmt::Expr(ExprStmt {
                            span: Default::default(),
                            expr: e.clone(),
                        })
                    })
                    .collect();

                *stmt = Stmt::If(IfStmt {
                    span: Default::default(),
                    test: bin.left.clone(),
                    cons: Box::new(Stmt::Block(BlockStmt {
                        span: Default::default(),
                        stmts,
                        ..Default::default()
                    })),
                    alt: None,
                });
            }
        }
    }
}

/// Fixes proxy IIFE patterns.
///
/// This mirrors the `fixProxies` step from the original TypeScript implementation.
///
/// - `(function () { return fn; }())` => `fn`
/// - `(function (a, b) { return b(a()); }(f, h))` => `h(f())` (only when args are literals/idents)
struct FixProxiesVisitor;

impl VisitMut for FixProxiesVisitor {
    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        expr.visit_mut_children_with(self);

        let Expr::Call(call) = expr else {
            return;
        };

        let callee_expr = match &call.callee {
            Callee::Expr(e) => &**e,
            _ => return,
        };

        let callee_expr = match callee_expr {
            Expr::Paren(paren) => &*paren.expr,
            other => other,
        };

        let (params, body_stmts) = match callee_expr {
            Expr::Fn(fn_expr) => {
                let Some(body) = fn_expr.function.body.as_ref() else {
                    return;
                };
                (
                    fn_expr.function.params.iter().map(|p| &p.pat).collect::<Vec<_>>(),
                    body.stmts.as_slice(),
                )
            }
            Expr::Arrow(arrow) => {
                let BlockStmtOrExpr::BlockStmt(block) = &*arrow.body else {
                    return;
                };
                (arrow.params.iter().collect::<Vec<_>>(), block.stmts.as_slice())
            }
            _ => return,
        };

        let Some(mut return_expr) = extract_single_return_expr(body_stmts) else {
            return;
        };
        return_expr = unwrap_paren_expr(return_expr);

        // Case 1: Unwrap IIFE returning a function.
        if matches!(return_expr, Expr::Fn(_) | Expr::Arrow(_)) {
            *expr = return_expr;
            return;
        }

        // Case 2: Inline call-returning proxy IIFE (beta-reduce).
        let Expr::Call(_) = &return_expr else {
            return;
        };

        // Only handle safe argument expressions (TS requires Literal or Identifier)
        if !call.args.iter().all(|arg| {
            arg.spread.is_none() && matches!(&*arg.expr, Expr::Lit(_) | Expr::Ident(_))
        }) {
            return;
        }

        // Only handle simple identifier parameters.
        if params.len() > call.args.len() {
            return;
        }
        let Some(param_names) = params_to_ident_names(&params) else {
            return;
        };

        let mut param_map: HashMap<String, Expr> = HashMap::new();
        for (idx, name) in param_names.into_iter().enumerate() {
            let arg_expr = call.args.get(idx).expect("checked args length").expr.clone();
            param_map.insert(name, *arg_expr);
        }

        let mut inlined = return_expr;
        let mut substitutor = ScopeAwareProxySubstitutor::new(param_map);
        inlined.visit_mut_with(&mut substitutor);

        *expr = inlined;
    }
}

#[must_use]
fn unwrap_paren_expr(expr: Expr) -> Expr {
    match expr {
        Expr::Paren(paren) => unwrap_paren_expr(*paren.expr),
        other => other,
    }
}

#[must_use]
fn extract_single_return_expr(stmts: &[Stmt]) -> Option<Expr> {
    let non_empty: Vec<&Stmt> = stmts.iter().filter(|s| !matches!(s, Stmt::Empty(_))).collect();
    if non_empty.len() != 1 {
        return None;
    }
    match non_empty[0] {
        Stmt::Return(ret) => ret.arg.as_ref().map(|e| (**e).clone()),
        _ => None,
    }
}

#[must_use]
fn params_to_ident_names(params: &[&Pat]) -> Option<Vec<String>> {
    let mut out = Vec::with_capacity(params.len());
    for pat in params {
        if let Pat::Ident(binding) = pat {
            out.push(binding.id.sym.to_string());
        } else {
            return None;
        }
    }
    Some(out)
}

struct ScopeAwareProxySubstitutor {
    param_map: HashMap<String, Expr>,
    scope_stack: Vec<HashSet<String>>,
}

impl ScopeAwareProxySubstitutor {
    #[must_use]
    fn new(param_map: HashMap<String, Expr>) -> Self {
        let declared: HashSet<String> = param_map.keys().cloned().collect();
        Self {
            param_map,
            scope_stack: vec![declared],
        }
    }

    fn push_scope(&mut self, declared: HashSet<String>) {
        self.scope_stack.push(declared);
    }

    fn pop_scope(&mut self) {
        if self.scope_stack.len() > 1 {
            self.scope_stack.pop();
        }
    }

    #[must_use]
    fn resolves_to_param(&self, name: &str) -> bool {
        for idx in (0..self.scope_stack.len()).rev() {
            if self.scope_stack[idx].contains(name) {
                return idx == 0;
            }
        }
        false
    }
}

impl VisitMut for ScopeAwareProxySubstitutor {
    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        if let Expr::Ident(ident) = expr {
            let name = ident.sym.as_ref();
            if self.resolves_to_param(name)
                && let Some(replacement) = self.param_map.get(name)
            {
                *expr = replacement.clone();
            }
            return;
        }

        expr.visit_mut_children_with(self);
    }

    fn visit_mut_fn_decl(&mut self, decl: &mut FnDecl) {
        let mut declared = HashSet::new();
        declared.insert(decl.ident.sym.to_string());
        collect_function_declared_names(&decl.function, &mut declared);
        self.push_scope(declared);
        decl.visit_mut_children_with(self);
        self.pop_scope();
    }

    fn visit_mut_fn_expr(&mut self, expr: &mut FnExpr) {
        let mut declared = HashSet::new();
        if let Some(ident) = &expr.ident {
            declared.insert(ident.sym.to_string());
        }
        collect_function_declared_names(&expr.function, &mut declared);
        self.push_scope(declared);
        expr.visit_mut_children_with(self);
        self.pop_scope();
    }

    fn visit_mut_arrow_expr(&mut self, arrow: &mut ArrowExpr) {
        let mut declared = HashSet::new();
        for pat in &arrow.params {
            collect_pat_bindings(pat, &mut declared);
        }
        if let BlockStmtOrExpr::BlockStmt(block) = &*arrow.body {
            collect_function_scoped_names_from_stmts(&block.stmts, &mut declared);
        }
        self.push_scope(declared);
        arrow.visit_mut_children_with(self);
        self.pop_scope();
    }

    fn visit_mut_block_stmt(&mut self, block: &mut BlockStmt) {
        let declared = collect_block_scoped_names(block);
        self.push_scope(declared);
        block.visit_mut_children_with(self);
        self.pop_scope();
    }

    fn visit_mut_for_stmt(&mut self, for_stmt: &mut ForStmt) {
        let mut declared = HashSet::new();
        if let Some(init) = &for_stmt.init
            && let VarDeclOrExpr::VarDecl(v) = init
                && v.kind != VarDeclKind::Var
            {
                for d in &v.decls {
                    collect_pat_bindings(&d.name, &mut declared);
                }
            }
        self.push_scope(declared);
        for_stmt.visit_mut_children_with(self);
        self.pop_scope();
    }

    fn visit_mut_for_in_stmt(&mut self, for_in: &mut ForInStmt) {
        let mut declared = HashSet::new();
        if let ForHead::VarDecl(v) = &for_in.left
            && v.kind != VarDeclKind::Var
        {
            for d in &v.decls {
                collect_pat_bindings(&d.name, &mut declared);
            }
        }
        self.push_scope(declared);
        for_in.visit_mut_children_with(self);
        self.pop_scope();
    }

    fn visit_mut_for_of_stmt(&mut self, for_of: &mut ForOfStmt) {
        let mut declared = HashSet::new();
        if let ForHead::VarDecl(v) = &for_of.left
            && v.kind != VarDeclKind::Var
        {
            for d in &v.decls {
                collect_pat_bindings(&d.name, &mut declared);
            }
        }
        self.push_scope(declared);
        for_of.visit_mut_children_with(self);
        self.pop_scope();
    }

    fn visit_mut_catch_clause(&mut self, catch: &mut CatchClause) {
        let mut declared = HashSet::new();
        if let Some(param) = &catch.param {
            collect_pat_bindings(param, &mut declared);
        }
        self.push_scope(declared);
        catch.visit_mut_children_with(self);
        self.pop_scope();
    }
}

fn collect_function_declared_names(func: &Function, declared: &mut HashSet<String>) {
    for p in &func.params {
        collect_pat_bindings(&p.pat, declared);
    }
    if let Some(body) = &func.body {
        collect_function_scoped_names_from_stmts(&body.stmts, declared);
    }
}

fn collect_function_scoped_names_from_stmts(stmts: &[Stmt], declared: &mut HashSet<String>) {
    for stmt in stmts {
        match stmt {
            Stmt::Decl(Decl::Var(var_decl)) if var_decl.kind == VarDeclKind::Var => {
                for d in &var_decl.decls {
                    collect_pat_bindings(&d.name, declared);
                }
            }
            Stmt::Decl(Decl::Fn(fn_decl)) => {
                declared.insert(fn_decl.ident.sym.to_string());
            }
            Stmt::Block(block) => collect_function_scoped_names_from_stmts(&block.stmts, declared),
            Stmt::If(if_stmt) => {
                collect_function_scoped_names_from_stmt(&if_stmt.cons, declared);
                if let Some(alt) = &if_stmt.alt {
                    collect_function_scoped_names_from_stmt(alt, declared);
                }
            }
            Stmt::While(while_stmt) => {
                collect_function_scoped_names_from_stmt(&while_stmt.body, declared);
            }
            Stmt::DoWhile(do_while_stmt) => {
                collect_function_scoped_names_from_stmt(&do_while_stmt.body, declared);
            }
            Stmt::For(for_stmt) => {
                if let Some(init) = &for_stmt.init
                    && let VarDeclOrExpr::VarDecl(v) = init
                    && v.kind == VarDeclKind::Var
                {
                    for d in &v.decls {
                        collect_pat_bindings(&d.name, declared);
                    }
                }
                collect_function_scoped_names_from_stmt(&for_stmt.body, declared);
            }
            Stmt::ForIn(for_in) => {
                if let ForHead::VarDecl(v) = &for_in.left
                    && v.kind == VarDeclKind::Var
                {
                    for d in &v.decls {
                        collect_pat_bindings(&d.name, declared);
                    }
                }
                collect_function_scoped_names_from_stmt(&for_in.body, declared);
            }
            Stmt::ForOf(for_of) => {
                if let ForHead::VarDecl(v) = &for_of.left
                    && v.kind == VarDeclKind::Var
                {
                    for d in &v.decls {
                        collect_pat_bindings(&d.name, declared);
                    }
                }
                collect_function_scoped_names_from_stmt(&for_of.body, declared);
            }
            Stmt::Try(try_stmt) => {
                collect_function_scoped_names_from_stmts(&try_stmt.block.stmts, declared);
                if let Some(handler) = &try_stmt.handler {
                    collect_function_scoped_names_from_stmts(&handler.body.stmts, declared);
                }
                if let Some(finalizer) = &try_stmt.finalizer {
                    collect_function_scoped_names_from_stmts(&finalizer.stmts, declared);
                }
            }
            Stmt::Switch(switch_stmt) => {
                for case in &switch_stmt.cases {
                    collect_function_scoped_names_from_stmts(&case.cons, declared);
                }
            }
            Stmt::Labeled(labeled) => {
                collect_function_scoped_names_from_stmt(&labeled.body, declared);
            }
            _ => {}
        }
    }
}

fn collect_function_scoped_names_from_stmt(stmt: &Stmt, declared: &mut HashSet<String>) {
    match stmt {
        Stmt::Block(block) => collect_function_scoped_names_from_stmts(&block.stmts, declared),
        Stmt::If(if_stmt) => {
            collect_function_scoped_names_from_stmt(&if_stmt.cons, declared);
            if let Some(alt) = &if_stmt.alt {
                collect_function_scoped_names_from_stmt(alt, declared);
            }
        }
        Stmt::While(while_stmt) => collect_function_scoped_names_from_stmt(&while_stmt.body, declared),
        Stmt::DoWhile(do_while_stmt) => {
            collect_function_scoped_names_from_stmt(&do_while_stmt.body, declared);
        }
        Stmt::For(for_stmt) => collect_function_scoped_names_from_stmt(&for_stmt.body, declared),
        Stmt::ForIn(for_in) => collect_function_scoped_names_from_stmt(&for_in.body, declared),
        Stmt::ForOf(for_of) => collect_function_scoped_names_from_stmt(&for_of.body, declared),
        Stmt::Try(try_stmt) => {
            collect_function_scoped_names_from_stmts(&try_stmt.block.stmts, declared);
            if let Some(handler) = &try_stmt.handler {
                collect_function_scoped_names_from_stmts(&handler.body.stmts, declared);
            }
            if let Some(finalizer) = &try_stmt.finalizer {
                collect_function_scoped_names_from_stmts(&finalizer.stmts, declared);
            }
        }
        Stmt::Switch(switch_stmt) => {
            for case in &switch_stmt.cases {
                collect_function_scoped_names_from_stmts(&case.cons, declared);
            }
        }
        Stmt::Labeled(labeled) => collect_function_scoped_names_from_stmt(&labeled.body, declared),
        _ => {}
    }
}

fn collect_block_scoped_names(block: &BlockStmt) -> HashSet<String> {
    let mut declared = HashSet::new();
    for stmt in &block.stmts {
        match stmt {
            Stmt::Decl(Decl::Var(var_decl)) if var_decl.kind != VarDeclKind::Var => {
                for d in &var_decl.decls {
                    collect_pat_bindings(&d.name, &mut declared);
                }
            }
            Stmt::Decl(Decl::Fn(fn_decl)) => {
                declared.insert(fn_decl.ident.sym.to_string());
            }
            Stmt::Decl(Decl::Class(class_decl)) => {
                declared.insert(class_decl.ident.sym.to_string());
            }
            _ => {}
        }
    }
    declared
}

fn collect_pat_bindings(pat: &Pat, declared: &mut HashSet<String>) {
    match pat {
        Pat::Ident(binding) => {
            declared.insert(binding.id.sym.to_string());
        }
        Pat::Array(arr) => {
            for pat in arr.elems.iter().flatten() {
                collect_pat_bindings(pat, declared);
            }
        }
        Pat::Object(obj) => {
            for prop in &obj.props {
                match prop {
                    ObjectPatProp::KeyValue(kv) => collect_pat_bindings(&kv.value, declared),
                    ObjectPatProp::Assign(assign) => {
                        declared.insert(assign.key.sym.to_string());
                    }
                    ObjectPatProp::Rest(rest) => collect_pat_bindings(&rest.arg, declared),
                }
            }
        }
        Pat::Rest(rest) => collect_pat_bindings(&rest.arg, declared),
        Pat::Assign(assign) => collect_pat_bindings(&assign.left, declared),
        Pat::Expr(_) | Pat::Invalid(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eval_math() {
        assert_eq!(
            SimplifyVisitor::eval_math(1.0, &BinaryOp::Add, 2.0),
            Some(3.0)
        );
        assert_eq!(
            SimplifyVisitor::eval_math(5.0, &BinaryOp::Sub, 3.0),
            Some(2.0)
        );
        assert_eq!(
            SimplifyVisitor::eval_math(2.0, &BinaryOp::Mul, 3.0),
            Some(6.0)
        );
        assert_eq!(
            SimplifyVisitor::eval_math(6.0, &BinaryOp::Div, 2.0),
            Some(3.0)
        );
        assert_eq!(SimplifyVisitor::eval_math(1.0, &BinaryOp::Div, 0.0), None);
    }

    #[test]
    fn test_eval_comparison() {
        assert_eq!(
            SimplifyVisitor::eval_comparison_num(1.0, &BinaryOp::EqEq, 1.0),
            Some(true)
        );
        assert_eq!(
            SimplifyVisitor::eval_comparison_num(1.0, &BinaryOp::EqEq, 2.0),
            Some(false)
        );
        assert_eq!(
            SimplifyVisitor::eval_comparison_num(1.0, &BinaryOp::Lt, 2.0),
            Some(true)
        );
        assert_eq!(
            SimplifyVisitor::eval_comparison_num(2.0, &BinaryOp::Gt, 1.0),
            Some(true)
        );
    }

    #[test]
    fn test_negative_hex_string() {
        use crate::Deobfuscator;

        let deob = Deobfuscator::new();
        // -"0x123" should become -291
        let code = r#"var x = -"0x123";"#;
        let result = deob.deobfuscate_source(code, None).unwrap();
        assert!(result.contains("-291"));
    }

    #[test]
    fn test_positive_hex_string() {
        use crate::Deobfuscator;

        let deob = Deobfuscator::new();
        // +"0x100" should become 256
        let code = r#"var x = +"0x100";"#;
        let result = deob.deobfuscate_source(code, None).unwrap();
        assert!(result.contains("256"));
    }

    #[test]
    fn test_parse_int_string_literal() {
        use crate::Deobfuscator;

        let deob = Deobfuscator::new();
        let code = r#"var x = parseInt("13FKWwew");"#;
        let result = deob.deobfuscate_source(code, None).unwrap();
        assert!(result.contains("13"));
    }

    #[test]
    fn test_parse_int_radix_literal() {
        use crate::Deobfuscator;

        let deob = Deobfuscator::new();
        let code = r#"var x = parseInt("ff", 16);"#;
        let result = deob.deobfuscate_source(code, None).unwrap();
        assert!(result.contains("255"));
    }

    #[test]
    fn test_hex_string_escape_decoding() {
        use crate::Deobfuscator;

        let deob = Deobfuscator::new();
        let code = r#"var x = "\x57\x65\x62";"#;
        let result = deob.deobfuscate_source(code, None).unwrap();
        assert!(result.contains("\"Web\""));
    }

    #[test]
    fn test_typeof_undefined() {
        use crate::Deobfuscator;

        let deob = Deobfuscator::new();
        // typeof undefined === "undefined" should become true
        let code = r#"var x = typeof undefined === "undefined";"#;
        let result = deob.deobfuscate_source(code, None).unwrap();
        assert!(result.contains("true"));
    }

    #[test]
    fn test_fix_proxies_inlines_call_proxy_iife() {
        use crate::{DeobfuscateOptions, Deobfuscator};
        use std::sync::Arc;

        let deob = Deobfuscator::new();
        let code = r#"
function f(){ return 1; }
function h(x){ return x; }
(function(a, b){ return b(a()); })(f, h);
"#;
        let options = DeobfuscateOptions {
            custom_transformers: Some(vec![Arc::new(Simplify::new())]),
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(result.contains("h(f())") || result.contains("h(f());"));
    }

    #[test]
    fn test_fix_proxies_requires_safe_args() {
        use crate::{DeobfuscateOptions, Deobfuscator};
        use std::sync::Arc;

        let deob = Deobfuscator::new();
        let code = r#"(function(a){ return a(); })(foo.bar);"#;
        let options = DeobfuscateOptions {
            custom_transformers: Some(vec![Arc::new(Simplify::new())]),
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        // Should not inline because `foo.bar` is not a literal/identifier.
        assert!(result.contains("function"));
    }

    #[test]
    fn test_fix_proxies_respects_shadowing_in_nested_functions() {
        use crate::{DeobfuscateOptions, Deobfuscator};
        use std::sync::Arc;

        let deob = Deobfuscator::new();
        let code = r#"
var x = 123;
function y(fn){ return fn(1); }
(function(a, b){ return b(function(a){ return a; }); })(x, y);
"#;
        let options = DeobfuscateOptions {
            custom_transformers: Some(vec![Arc::new(Simplify::new())]),
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(result.contains("y(function(a)") || result.contains("y(function (a)"));
        assert!(result.contains("return a") || result.contains("return a;"));
    }
}
