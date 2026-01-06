//! JSConfuser Calculator transformer
//!
//! Handles JSConfuser's calculator pattern which uses switch statements
//! to perform math operations.
//!
//! Pattern:
//! ```js
//! function calc(opcode, a, b) {
//!     switch (opcode) {
//!         case 0: return a + b;
//!         case 1: return a - b;
//!         case 2: return a * b;
//!         case 3: return a / b;
//!     }
//! }
//! calc(0, 5, 3) // -> 5 + 3 -> 8
//! ```

use std::collections::HashMap;
use swc_ecma_ast::*;
use swc_ecma_visit::{VisitMut, VisitMutWith};

use crate::context::Context;
use crate::error::Result;
use crate::transformers::Transformer;

/// JSConfuser Calculator transformer.
///
/// Evaluates calculator switch helpers into direct arithmetic where possible.
#[derive(Debug)]
pub struct JSConfuserCalculator;

impl JSConfuserCalculator {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for JSConfuserCalculator {
    fn default() -> Self {
        Self::new()
    }
}

impl Transformer for JSConfuserCalculator {
    fn name(&self) -> &'static str {
        "JSConfuserCalculator"
    }

    fn transform(&self, context: &mut Context) -> Result<()> {
        // First pass: find calculator functions
        let mut finder = CalculatorFinder::new();
        context.ast.visit_mut_with(&mut finder);

        #[cfg(feature = "tracing")]
        for func in &finder.functions {
            crate::log_info!(
                "Found calculator '{}' with {} operators",
                func.identifier,
                func.operators.len()
            );
        }

        // Second pass: replace calculator calls
        let mut replacer = CalculatorReplacer::new(finder.functions);
        context.ast.visit_mut_with(&mut replacer);

        Ok(())
    }
}

/// Allowed binary operators for calculator functions
#[derive(Debug, Clone, Copy)]
enum CalcOperator {
    Add,
    Sub,
    Mul,
    Div,
}

impl CalcOperator {
    #[must_use]
    const fn to_binary_op(self) -> BinaryOp {
        match self {
            Self::Add => BinaryOp::Add,
            Self::Sub => BinaryOp::Sub,
            Self::Mul => BinaryOp::Mul,
            Self::Div => BinaryOp::Div,
        }
    }

    #[must_use]
    const fn from_binary_op(op: &BinaryOp) -> Option<Self> {
        match op {
            BinaryOp::Add => Some(Self::Add),
            BinaryOp::Sub => Some(Self::Sub),
            BinaryOp::Mul => Some(Self::Mul),
            BinaryOp::Div => Some(Self::Div),
            _ => None,
        }
    }
}

/// Single operator case in a calculator function
#[derive(Debug, Clone)]
struct OperatorCase {
    test_value: i64,
    operator: CalcOperator,
    lhs_index: usize,
    rhs_index: usize,
}

/// A calculator function definition
#[derive(Debug, Clone)]
struct CalcFunction {
    identifier: String,
    operators: Vec<OperatorCase>,
    opcode_param_index: usize,
}

/// Finds calculator function patterns
struct CalculatorFinder {
    functions: Vec<CalcFunction>,
}

impl CalculatorFinder {
    #[must_use]
    const fn new() -> Self {
        Self {
            functions: Vec::new(),
        }
    }

    /// Extract numeric value from expression (handles unary minus)
    #[must_use]
    fn get_numeric_value(expr: &Expr) -> Option<i64> {
        match expr {
            Expr::Lit(Lit::Num(n)) => Some(n.value as i64),
            Expr::Unary(unary) if unary.op == UnaryOp::Minus => {
                if let Expr::Lit(Lit::Num(n)) = &*unary.arg {
                    Some(-(n.value as i64))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Try to parse a function as a calculator function
    #[must_use]
    fn try_parse_calculator(
        &self,
        fn_name: &str,
        params: &[Param],
        body: &BlockStmt,
    ) -> Option<CalcFunction> {
        // Filter empty statements
        let stmts: Vec<_> = body
            .stmts
            .iter()
            .filter(|s| !matches!(s, Stmt::Empty(_)))
            .collect();

        // Must have exactly one statement (the switch)
        if stmts.len() != 1 {
            return None;
        }

        // Must be a switch statement
        let switch_stmt = match stmts[0] {
            Stmt::Switch(s) => s,
            _ => return None,
        };

        // Discriminant must be an identifier (the opcode parameter)
        let opcode_param_name = match &*switch_stmt.discriminant {
            Expr::Ident(ident) => ident.sym.to_string(),
            _ => return None,
        };

        // Build parameter name to index mapping
        let param_names: Vec<Option<String>> = params
            .iter()
            .map(|p| {
                if let Pat::Ident(binding) = &p.pat {
                    Some(binding.id.sym.to_string())
                } else {
                    None
                }
            })
            .collect();

        // Find opcode parameter index
        let opcode_param_index = param_names
            .iter()
            .position(|n| n.as_ref() == Some(&opcode_param_name))?;

        // Parse each case
        let mut operators = Vec::new();

        for case in &switch_stmt.cases {
            // Get test value
            let test_value = case
                .test
                .as_ref()
                .and_then(|t| Self::get_numeric_value(t))?;

            // Must have exactly one consequent statement
            let non_empty: Vec<_> = case
                .cons
                .iter()
                .filter(|s| !matches!(s, Stmt::Empty(_)))
                .collect();

            if non_empty.len() != 1 {
                return None;
            }

            // Must be a return statement with binary expression
            let ret_stmt = match non_empty[0] {
                Stmt::Return(r) => r,
                _ => return None,
            };

            let bin_expr = match ret_stmt.arg.as_ref() {
                Some(arg) => match &**arg {
                    Expr::Bin(b) => b,
                    _ => return None,
                },
                None => return None,
            };

            // Must be an allowed operator
            let operator = CalcOperator::from_binary_op(&bin_expr.op)?;

            // Left and right must be identifiers (parameters)
            let lhs_name = match &*bin_expr.left {
                Expr::Ident(ident) => ident.sym.to_string(),
                _ => return None,
            };

            let rhs_name = match &*bin_expr.right {
                Expr::Ident(ident) => ident.sym.to_string(),
                _ => return None,
            };

            // Find parameter indices
            let lhs_index = param_names
                .iter()
                .position(|n| n.as_ref() == Some(&lhs_name))?;

            let rhs_index = param_names
                .iter()
                .position(|n| n.as_ref() == Some(&rhs_name))?;

            operators.push(OperatorCase {
                test_value,
                operator,
                lhs_index,
                rhs_index,
            });
        }

        if operators.is_empty() {
            return None;
        }

        Some(CalcFunction {
            identifier: fn_name.to_string(),
            operators,
            opcode_param_index,
        })
    }
}

impl VisitMut for CalculatorFinder {
    fn visit_mut_fn_decl(&mut self, func: &mut FnDecl) {
        func.visit_mut_children_with(self);

        let fn_name = func.ident.sym.to_string();

        if let Some(body) = &func.function.body
            && let Some(calc_fn) = self.try_parse_calculator(&fn_name, &func.function.params, body)
        {
            self.functions.push(calc_fn);
        }
    }

    fn visit_mut_var_declarator(&mut self, decl: &mut VarDeclarator) {
        decl.visit_mut_children_with(self);

        // Check for var calc = function(...) { switch... }
        if let Pat::Ident(binding) = &decl.name {
            let fn_name = binding.id.sym.to_string();

            if let Some(init) = &decl.init {
                let (params, body) = match &**init {
                    Expr::Fn(fn_expr) => {
                        if let Some(body) = &fn_expr.function.body {
                            (&fn_expr.function.params, body)
                        } else {
                            return;
                        }
                    }
                    _ => return,
                };

                if let Some(calc_fn) = self.try_parse_calculator(&fn_name, params, body) {
                    self.functions.push(calc_fn);
                }
            }
        }
    }
}

/// Replaces calculator function calls with direct binary expressions
struct CalculatorReplacer {
    functions: HashMap<String, CalcFunction>,
}

impl CalculatorReplacer {
    #[must_use]
    fn new(functions: Vec<CalcFunction>) -> Self {
        let map: HashMap<_, _> = functions
            .into_iter()
            .map(|f| (f.identifier.clone(), f))
            .collect();
        Self { functions: map }
    }
}

impl VisitMut for CalculatorReplacer {
    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        expr.visit_mut_children_with(self);

        if let Expr::Call(call) = expr
            && let Callee::Expr(callee) = &call.callee
            && let Expr::Ident(ident) = &**callee
        {
            let fn_name = ident.sym.to_string();

            if let Some(calc_fn) = self.functions.get(&fn_name) {
                // Get the opcode value
                if call.args.len() <= calc_fn.opcode_param_index {
                    return;
                }

                let opcode = CalculatorFinder::get_numeric_value(
                    &call.args[calc_fn.opcode_param_index].expr,
                );

                if let Some(opcode_val) = opcode {
                    // Find matching operator
                    if let Some(op_case) = calc_fn
                        .operators
                        .iter()
                        .find(|o| o.test_value == opcode_val)
                    {
                        // Get left and right arguments
                        if call.args.len() > op_case.lhs_index
                            && call.args.len() > op_case.rhs_index
                        {
                            let left = call.args[op_case.lhs_index].expr.clone();
                            let right = call.args[op_case.rhs_index].expr.clone();

                            // Replace with binary expression
                            *expr = Expr::Bin(BinExpr {
                                span: Default::default(),
                                op: op_case.operator.to_binary_op(),
                                left,
                                right,
                            });
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
    use crate::deobfuscator::DeobfuscateOptions;
    use std::sync::Arc;

    fn deob_with_calculator(code: &str) -> String {
        let deob = Deobfuscator::new();
        let options = DeobfuscateOptions {
            custom_transformers: Some(vec![Arc::new(JSConfuserCalculator::new())]),
            ..Default::default()
        };
        deob.deobfuscate_source(code, Some(options)).unwrap()
    }

    #[test]
    fn test_jsconfuser_calculator_new() {
        let transformer = JSConfuserCalculator::new();
        assert_eq!(transformer.name(), "JSConfuserCalculator");
    }

    #[test]
    fn test_calculator_basic() {
        let code = r#"
function calc(op, a, b) {
    switch (op) {
        case 0: return a + b;
        case 1: return a - b;
        case 2: return a * b;
        case 3: return a / b;
    }
}
var result = calc(0, 5, 3);
"#;
        let result = deob_with_calculator(code);
        // The calculator call should be replaced with direct operation
        assert!(result.contains("5 + 3") || result.contains("8"));
        assert!(!result.contains("calc(0"));
    }

    #[test]
    fn test_calculator_negative_opcode() {
        let code = r#"
function calc(op, a, b) {
    switch (op) {
        case -1: return a + b;
        case -2: return a - b;
    }
}
var result = calc(-1, 10, 5);
"#;
        let result = deob_with_calculator(code);
        assert!(result.contains("10 + 5") || result.contains("15"));
        assert!(!result.contains("calc(-1"));
    }
}
