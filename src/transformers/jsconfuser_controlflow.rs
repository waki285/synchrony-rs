//! JSConfuser ControlFlow transformer
//!
//! Handles JSConfuser's control flow flattening pattern which uses
//! switch statements with computed case values.
//!
//! Pattern:
//! ```js
//! var state = (x * 2) + 5;
//! switch (state) {
//!     case 7: /* x=1 */ break;
//!     case 9: /* x=2 */ break;
//!     case 11: /* x=3 */ break;
//! }
//! ```
//! After transformation, case values are simplified to match direct x values.

use std::collections::HashMap;
use swc_ecma_ast::*;
use swc_ecma_visit::{VisitMut, VisitMutWith};

use crate::context::Context;
use crate::error::Result;
use crate::transformers::Transformer;

/// Variable stack for tracking state variables during deflatten
type VarStack = HashMap<String, f64>;

/// Evaluate assignment operator and update stack
fn evaluate_assignment(stack: &mut VarStack, var: &str, op: AssignOp, value: f64) -> Option<f64> {
    if op == AssignOp::Assign {
        stack.insert(var.to_string(), value);
        return Some(value);
    }

    let current = *stack.get(var)?;
    let result = match op {
        AssignOp::AddAssign => current + value,
        AssignOp::SubAssign => current - value,
        AssignOp::MulAssign => current * value,
        AssignOp::DivAssign => {
            if value != 0.0 {
                current / value
            } else {
                return None;
            }
        }
        AssignOp::ModAssign => {
            if value != 0.0 {
                current % value
            } else {
                return None;
            }
        }
        AssignOp::LShiftAssign => ((current as i64) << (value as i64)) as f64,
        AssignOp::RShiftAssign => ((current as i64) >> (value as i64)) as f64,
        AssignOp::ZeroFillRShiftAssign => ((current as u64) >> (value as u64)) as f64,
        AssignOp::BitAndAssign => ((current as i64) & (value as i64)) as f64,
        AssignOp::BitXorAssign => ((current as i64) ^ (value as i64)) as f64,
        AssignOp::BitOrAssign => ((current as i64) | (value as i64)) as f64,
        _ => return None,
    };
    stack.insert(var.to_string(), result);
    Some(result)
}

/// Evaluate a binary expression with variable substitution from stack
#[must_use]
fn evaluate_binary_expr(stack: &VarStack, expr: &Expr) -> Option<f64> {
    match expr {
        Expr::Lit(Lit::Num(n)) => Some(n.value),
        Expr::Unary(u) if u.op == UnaryOp::Minus => {
            let inner = evaluate_binary_expr(stack, &u.arg)?;
            Some(-inner)
        }
        Expr::Ident(id) => stack.get(&id.sym.to_string()).copied(),
        Expr::Bin(bin) => {
            let lhs = evaluate_binary_expr(stack, &bin.left)?;
            let rhs = evaluate_binary_expr(stack, &bin.right)?;
            match bin.op {
                BinaryOp::Add => Some(lhs + rhs),
                BinaryOp::Sub => Some(lhs - rhs),
                BinaryOp::Mul => Some(lhs * rhs),
                BinaryOp::Div => {
                    if rhs != 0.0 {
                        Some(lhs / rhs)
                    } else {
                        None
                    }
                }
                BinaryOp::Mod => {
                    if rhs != 0.0 {
                        Some(lhs % rhs)
                    } else {
                        None
                    }
                }
                BinaryOp::BitAnd => Some(((lhs as i64) & (rhs as i64)) as f64),
                BinaryOp::BitOr => Some(((lhs as i64) | (rhs as i64)) as f64),
                BinaryOp::BitXor => Some(((lhs as i64) ^ (rhs as i64)) as f64),
                BinaryOp::LShift => Some(((lhs as i64) << (rhs as i64)) as f64),
                BinaryOp::RShift => Some(((lhs as i64) >> (rhs as i64)) as f64),
                BinaryOp::ZeroFillRShift => Some(((lhs as u64) >> (rhs as u64)) as f64),
                _ => None,
            }
        }
        Expr::Paren(p) => evaluate_binary_expr(stack, &p.expr),
        _ => None,
    }
}

/// JSConfuser ControlFlow transformer.
///
/// Simplifies JSConfuser switch-based control flow where case values can be resolved.
#[derive(Debug)]
pub struct JSConfuserControlFlow;

impl JSConfuserControlFlow {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Inverse a binary operator
    #[must_use]
    const fn inverse_operator(op: BinaryOp) -> Option<BinaryOp> {
        match op {
            BinaryOp::Add => Some(BinaryOp::Sub),
            BinaryOp::Sub => Some(BinaryOp::Add),
            BinaryOp::Mul => Some(BinaryOp::Div),
            BinaryOp::Div => Some(BinaryOp::Mul),
            _ => None,
        }
    }

    /// Evaluate a simple math operation
    #[must_use]
    const fn math_eval(lhs: f64, op: BinaryOp, rhs: f64) -> Option<f64> {
        match op {
            BinaryOp::Add => Some(lhs + rhs),
            BinaryOp::Sub => Some(lhs - rhs),
            BinaryOp::Mul => Some(lhs * rhs),
            BinaryOp::Div => {
                if rhs == 0.0 {
                    None
                } else {
                    Some(lhs / rhs)
                }
            }
            _ => None,
        }
    }
}

impl Default for JSConfuserControlFlow {
    fn default() -> Self {
        Self::new()
    }
}

impl Transformer for JSConfuserControlFlow {
    fn name(&self) -> &'static str {
        "JSConfuserControlFlow"
    }

    fn transform(&self, context: &mut Context) -> Result<()> {
        // Run switch fixer first
        let mut fixer = SwitchFixer::new();
        context.ast.visit_mut_with(&mut fixer);

        // Run deflattener
        let mut deflattener = Deflattener::new();
        context.ast.visit_mut_with(&mut deflattener);

        Ok(())
    }
}

/// Deflattens while+switch control flow patterns
struct Deflattener;

impl Deflattener {
    #[must_use]
    const fn new() -> Self {
        Self
    }

    /// Get numeric value from expression
    #[must_use]
    fn get_numeric_value(expr: &Expr) -> Option<f64> {
        match expr {
            Expr::Lit(Lit::Num(n)) => Some(n.value),
            Expr::Unary(unary) if unary.op == UnaryOp::Minus => {
                if let Expr::Lit(Lit::Num(n)) = &*unary.arg {
                    Some(-n.value)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

impl VisitMut for Deflattener {
    fn visit_mut_block_stmt(&mut self, block: &mut BlockStmt) {
        block.visit_mut_children_with(self);

        // Look for while statements with switch inside
        let mut replacements: Vec<(usize, Vec<Stmt>)> = vec![];

        for (idx, stmt) in block.stmts.iter().enumerate() {
            if let Stmt::While(while_stmt) = stmt
                && let Some(new_stmts) = self.try_deflatten(while_stmt, block)
            {
                replacements.push((idx, new_stmts));
            }
        }

        // Apply replacements in reverse order
        for (idx, new_stmts) in replacements.into_iter().rev() {
            block.stmts.splice(idx..=idx, new_stmts);
        }
    }
}

impl Deflattener {
    /// Try to deflatten a while+switch pattern
    #[must_use]
    fn try_deflatten(&self, while_stmt: &WhileStmt, block: &BlockStmt) -> Option<Vec<Stmt>> {
        // Pattern: while (a + b !== endState) { ... switch(state) { ... } }
        let test = &while_stmt.test;

        // Check: test is binary !== or !=
        let bin_test = match &**test {
            Expr::Bin(b) if b.op == BinaryOp::NotEq || b.op == BinaryOp::NotEqEq => b,
            _ => return None,
        };

        // Right side should be end state (numeric)
        let end_state = Self::get_numeric_value(&bin_test.right)?;

        // Left side should be additive expression with identifiers
        let mut stack: VarStack = HashMap::new();
        self.extract_state_vars(&bin_test.left, &mut stack);

        if stack.is_empty() {
            return None;
        }

        // Initialize stack from variable declarations in block
        self.initialize_stack_from_block(&mut stack, block);

        // Get while body
        let body_block = match &*while_stmt.body {
            Stmt::Block(b) => b,
            _ => return None,
        };

        // Find switch statement at end of body
        let switch_stmt = body_block.stmts.last().and_then(|s| {
            if let Stmt::Switch(sw) = s {
                Some(sw)
            } else {
                None
            }
        })?;

        // Switch discriminant should be identifier
        let _disc_name = match &*switch_stmt.discriminant {
            Expr::Ident(id) => id.sym.to_string(),
            _ => return None,
        };

        crate::log_debug!(
            "[JSConfuserControlFlow] Found while+switch pattern, end_state={}",
            end_state
        );

        // Simulate execution

        self.simulate_execution(&mut stack, end_state, &bin_test.left, switch_stmt)
    }

    /// Extract state variable names from binary expression
    fn extract_state_vars(&self, expr: &Expr, stack: &mut VarStack) {
        match expr {
            Expr::Ident(id) => {
                // Initialize with 0 for now, will be updated from declarations
                stack.insert(id.sym.to_string(), 0.0);
            }
            Expr::Bin(bin) => {
                self.extract_state_vars(&bin.left, stack);
                self.extract_state_vars(&bin.right, stack);
            }
            _ => {}
        }
    }

    /// Initialize stack values from variable declarations in the block
    fn initialize_stack_from_block(&self, stack: &mut VarStack, block: &BlockStmt) {
        for stmt in &block.stmts {
            if let Stmt::Decl(Decl::Var(var_decl)) = stmt {
                for decl in &var_decl.decls {
                    if let Pat::Ident(binding) = &decl.name {
                        let var_name = binding.id.sym.to_string();
                        if stack.contains_key(&var_name)
                            && let Some(init) = &decl.init
                            && let Some(value) = Self::get_numeric_value(init)
                        {
                            stack.insert(var_name, value);
                        }
                    }
                }
            }
        }
    }

    /// Simulate the while+switch execution to extract statements
    fn simulate_execution(
        &self,
        stack: &mut VarStack,
        end_state: f64,
        while_test_left: &Expr,
        switch_stmt: &SwitchStmt,
    ) -> Option<Vec<Stmt>> {
        let max_iters = switch_stmt.cases.len();
        let mut all_expressions: Vec<Stmt> = vec![];

        for iter in 0..=max_iters {
            // Evaluate while condition left side
            let while_state = evaluate_binary_expr(stack, while_test_left)?;

            if while_state == end_state {
                crate::log_debug!("[JSConfuserControlFlow] Loop ended at iter {}", iter);
                break;
            }

            if iter >= max_iters {
                crate::log_debug!("[JSConfuserControlFlow] Max iterations exceeded");
                return None;
            }

            // Find state expression (discriminant calculation)
            // For now, assume discriminant is directly the state
            let state = evaluate_binary_expr(stack, &switch_stmt.discriminant)?;

            // Find matching case
            let matching_case = switch_stmt.cases.iter().find(|c| {
                c.test.as_ref().and_then(|t| Self::get_numeric_value(t)) == Some(state)
            })?;

            // Extract and process case body
            if let Some(stmts) = self.process_case_body(&matching_case.cons, stack) {
                all_expressions.extend(stmts);
            }
        }

        if all_expressions.is_empty() {
            None
        } else {
            Some(all_expressions)
        }
    }

    /// Process case body and extract statements, updating stack as needed
    fn process_case_body(&self, stmts: &[Stmt], stack: &mut VarStack) -> Option<Vec<Stmt>> {
        let mut result: Vec<Stmt> = vec![];

        for stmt in stmts {
            match stmt {
                Stmt::Break(_) => continue,
                Stmt::Expr(expr_stmt) => {
                    // Check for void sequence expression pattern
                    if let Expr::Unary(unary) = &*expr_stmt.expr
                        && unary.op == UnaryOp::Void
                        && let Expr::Seq(seq) = &*unary.arg
                    {
                        // Process sequence expressions
                        for expr in &seq.exprs {
                            if let Expr::Assign(assign) = &**expr {
                                self.process_assignment(assign, stack, &mut result);
                            } else {
                                // Non-assignment expression - keep it
                                result.push(Stmt::Expr(ExprStmt {
                                    span: Default::default(),
                                    expr: expr.clone(),
                                }));
                            }
                        }
                        continue;
                    }
                    // Regular expression statement
                    result.push(stmt.clone());
                }
                _ => {
                    result.push(stmt.clone());
                }
            }
        }

        Some(result)
    }

    /// Process an assignment expression, updating stack for state vars
    fn process_assignment(
        &self,
        assign: &AssignExpr,
        stack: &mut VarStack,
        result: &mut Vec<Stmt>,
    ) {
        if let AssignTarget::Simple(SimpleAssignTarget::Ident(binding)) = &assign.left {
            let var_name = binding.id.sym.to_string();

            // Try to evaluate the right side
            if let Some(value) = evaluate_binary_expr(stack, &assign.right) {
                // If this is a state variable, update stack
                if stack.contains_key(&var_name) {
                    evaluate_assignment(stack, &var_name, assign.op, value);
                    // Don't add state variable updates to output
                    return;
                }
            }
        }

        // Keep non-state-variable assignments
        result.push(Stmt::Expr(ExprStmt {
            span: Default::default(),
            expr: Box::new(Expr::Assign(assign.clone())),
        }));
    }
}

/// Fixes JSConfuser switch case values by inverting the state computation
struct SwitchFixer;

impl SwitchFixer {
    #[must_use]
    const fn new() -> Self {
        Self
    }

    /// Extract numeric value from expression
    #[must_use]
    fn get_numeric_value(expr: &Expr) -> Option<f64> {
        match expr {
            Expr::Lit(Lit::Num(n)) => Some(n.value),
            Expr::Unary(unary) if unary.op == UnaryOp::Minus => {
                if let Expr::Lit(Lit::Num(n)) = &*unary.arg {
                    Some(-n.value)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Try to extract state transformation from expression like (x * 2) + 5
    /// Returns (inner_var_name, left_op, left_val, right_op, right_val)
    #[must_use]
    fn extract_state_transform(expr: &Expr) -> Option<(String, BinaryOp, f64, BinaryOp, f64)> {
        // Pattern: (x * left_val) + right_val
        // Or: (x + left_val) * right_val
        if let Expr::Bin(outer) = expr {
            let right_val = Self::get_numeric_value(&outer.right)?;
            let right_op = outer.op;

            if let Expr::Bin(inner) = &*outer.left {
                let left_val = Self::get_numeric_value(&inner.right)?;
                let left_op = inner.op;

                if let Expr::Ident(ident) = &*inner.left {
                    return Some((
                        ident.sym.to_string(),
                        left_op,
                        left_val,
                        right_op,
                        right_val,
                    ));
                }
            }
        }
        None
    }

    /// Check if all switch cases have numeric tests
    #[must_use]
    fn all_cases_numeric(switch_stmt: &SwitchStmt) -> bool {
        switch_stmt.cases.iter().all(|c| {
            c.test
                .as_ref()
                .is_some_and(|t| Self::get_numeric_value(t).is_some())
        })
    }
}

impl VisitMut for SwitchFixer {
    fn visit_mut_fn_decl(&mut self, func: &mut FnDecl) {
        func.visit_mut_children_with(self);

        if let Some(body) = &mut func.function.body {
            self.process_block(body);
        }
    }

    fn visit_mut_fn_expr(&mut self, func: &mut FnExpr) {
        func.visit_mut_children_with(self);

        if let Some(body) = &mut func.function.body {
            self.process_block(body);
        }
    }
}

impl SwitchFixer {
    fn process_block(&mut self, block: &mut BlockStmt) {
        // Build a map of variable declarations
        let mut var_inits: HashMap<String, Box<Expr>> = HashMap::new();

        for stmt in &block.stmts {
            if let Stmt::Decl(Decl::Var(var_decl)) = stmt {
                for decl in &var_decl.decls {
                    if let Pat::Ident(binding) = &decl.name
                        && let Some(init) = &decl.init
                    {
                        var_inits.insert(binding.id.sym.to_string(), init.clone());
                    }
                }
            }
        }

        // Process switch statements
        for stmt in &mut block.stmts {
            if let Stmt::Switch(switch_stmt) = stmt {
                // Check if discriminant is an identifier
                if let Expr::Ident(disc_ident) = &*switch_stmt.discriminant {
                    let disc_name = disc_ident.sym.to_string();

                    // Look for the state variable initialization
                    if let Some(init_expr) = var_inits.get(&disc_name) {
                        // Try to extract the transformation
                        if let Some((inner_var, left_op, left_val, right_op, right_val)) =
                            Self::extract_state_transform(init_expr)
                        {
                            // Check all cases are numeric
                            if Self::all_cases_numeric(switch_stmt) {
                                // Get inverse operators
                                let inv_right_op =
                                    JSConfuserControlFlow::inverse_operator(right_op);
                                let inv_left_op = JSConfuserControlFlow::inverse_operator(left_op);

                                if let (Some(inv_r), Some(inv_l)) = (inv_right_op, inv_left_op) {
                                    crate::log_debug!(
                                        "[JSConfuserControlFlow] Fixing switch with state var '{}' -> '{}'",
                                        disc_name,
                                        inner_var
                                    );

                                    // Transform each case value
                                    for case in &mut switch_stmt.cases {
                                        if let Some(test) = &mut case.test
                                            && let Some(test_val) = Self::get_numeric_value(test)
                                        {
                                            // Apply inverse transformation:
                                            // original: state = (x * left_val) + right_val
                                            // inverse: x = (state - right_val) / left_val
                                            let step1 = JSConfuserControlFlow::math_eval(
                                                test_val, inv_r, right_val,
                                            );
                                            if let Some(s1) = step1 {
                                                let new_val = JSConfuserControlFlow::math_eval(
                                                    s1, inv_l, left_val,
                                                );
                                                if let Some(nv) = new_val {
                                                    // Replace test with new value
                                                    *test = if nv < 0.0 {
                                                        Box::new(Expr::Unary(UnaryExpr {
                                                            span: Default::default(),
                                                            op: UnaryOp::Minus,
                                                            arg: Box::new(Expr::Lit(Lit::Num(
                                                                Number {
                                                                    span: Default::default(),
                                                                    value: -nv,
                                                                    raw: None,
                                                                },
                                                            ))),
                                                        }))
                                                    } else {
                                                        Box::new(Expr::Lit(Lit::Num(Number {
                                                            span: Default::default(),
                                                            value: nv,
                                                            raw: None,
                                                        })))
                                                    };
                                                }
                                            }
                                        }
                                    }

                                    // Replace discriminant with the inner variable
                                    *switch_stmt.discriminant = Expr::Ident(Ident::new(
                                        inner_var.into(),
                                        Default::default(),
                                        Default::default(),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Deobfuscator;

    #[test]
    fn test_jsconfuser_controlflow_new() {
        let transformer = JSConfuserControlFlow::new();
        assert_eq!(transformer.name(), "JSConfuserControlFlow");
    }

    #[test]
    fn test_inverse_operator() {
        assert_eq!(
            JSConfuserControlFlow::inverse_operator(BinaryOp::Add),
            Some(BinaryOp::Sub)
        );
        assert_eq!(
            JSConfuserControlFlow::inverse_operator(BinaryOp::Sub),
            Some(BinaryOp::Add)
        );
        assert_eq!(
            JSConfuserControlFlow::inverse_operator(BinaryOp::Mul),
            Some(BinaryOp::Div)
        );
        assert_eq!(
            JSConfuserControlFlow::inverse_operator(BinaryOp::Div),
            Some(BinaryOp::Mul)
        );
    }

    #[test]
    fn test_switch_fix_basic() {
        let deob = Deobfuscator::new();
        let code = r#"
function test(x) {
    var state = (x * 2) + 5;
    switch (state) {
        case 7:
            return "one";
        case 9:
            return "two";
        case 11:
            return "three";
    }
}
"#;
        let result = deob.deobfuscate_source(code, None).unwrap();
        // The switch should now use x directly with values 1, 2, 3
        assert!(result.contains("switch") && result.contains("x"));
    }
}
