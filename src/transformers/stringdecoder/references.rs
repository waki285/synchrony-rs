use swc_ecma_ast::*;
use swc_ecma_visit::{VisitMut, VisitMutWith};

use crate::context::{DecoderFunction, DecoderReference};

use super::core::eval_const_i64;

/// Third pass: find variable references to decoders
///
/// Detects: `var alias = _0xDecoder;`
pub(super) struct VariableReferenceFinder<'a> {
    decoders: &'a [DecoderFunction],
    existing_refs: &'a [DecoderReference],
    pub(super) references: Vec<DecoderReference>,
}

impl<'a> VariableReferenceFinder<'a> {
    #[must_use]
    pub(super) const fn new(
        decoders: &'a [DecoderFunction],
        existing_refs: &'a [DecoderReference],
    ) -> Self {
        Self {
            decoders,
            existing_refs,
            references: Vec::new(),
        }
    }

    #[must_use]
    fn is_known_decoder(&self, name: &str) -> bool {
        self.decoders.iter().any(|d| d.identifier == name)
            || self.existing_refs.iter().any(|r| r.identifier == name)
            || self.references.iter().any(|r| r.identifier == name)
    }
}

impl VisitMut for VariableReferenceFinder<'_> {
    fn visit_mut_var_decl(&mut self, decl: &mut VarDecl) {
        // Collect aliases first so nested scopes can resolve them.
        for declarator in &decl.decls {
            if let Pat::Ident(binding) = &declarator.name
                && let Some(init) = &declarator.init
                && let Expr::Ident(ref_ident) = &**init
            {
                let ref_name = binding.id.sym.to_string();
                let val_name = ref_ident.sym.to_string();

                if self.is_known_decoder(&val_name) {
                    self.references.push(DecoderReference {
                        identifier: ref_name,
                        real_identifier: val_name,
                        additional_offset: 0,
                        index_argument: None,
                        key_argument: None,
                    });
                }
            }
        }

        // Then visit children
        decl.visit_mut_children_with(self);
    }
}

/// Function reference finder - detects wrapper functions around decoders
///
/// Detects patterns like:
/// ```js
/// function wrapper(a, b) {
///     return decoder(a - 100, b);
/// }
/// ```
pub(super) struct FunctionReferenceFinder<'a> {
    decoders: &'a [DecoderFunction],
    existing_refs: &'a [DecoderReference],
    pub(super) references: Vec<DecoderReference>,
}

impl<'a> FunctionReferenceFinder<'a> {
    #[must_use]
    pub(super) const fn new(
        decoders: &'a [DecoderFunction],
        existing_refs: &'a [DecoderReference],
    ) -> Self {
        Self {
            decoders,
            existing_refs,
            references: Vec::new(),
        }
    }

    #[must_use]
    fn is_known_decoder(&self, name: &str) -> bool {
        self.decoders.iter().any(|d| d.identifier == name)
            || self.existing_refs.iter().any(|r| r.identifier == name)
    }

    #[must_use]
    fn callee_arg_positions(&self, name: &str) -> (usize, usize) {
        if let Some(decoder) = self.decoders.iter().find(|d| d.identifier == name) {
            return (decoder.index_argument, decoder.key_argument);
        }
        if let Some(reference) = self.existing_refs.iter().find(|r| r.identifier == name) {
            return (
                reference.index_argument.unwrap_or(0),
                reference.key_argument.unwrap_or(1),
            );
        }
        (0, 1)
    }

    #[must_use]
    fn strip_parens(expr: &Expr) -> &Expr {
        match expr {
            Expr::Paren(paren) => Self::strip_parens(&paren.expr),
            _ => expr,
        }
    }

    /// Extract additional offset from call argument like (a - 100)
    #[must_use]
    fn extract_arg_offset(expr: &Expr, param_name: &str) -> Option<i32> {
        match Self::strip_parens(expr) {
            // Simple case: just the parameter (no offset)
            Expr::Ident(ident) if ident.sym == param_name => Some(0),
            // Offset case: param - offset or param + offset
            Expr::Bin(bin) => {
                // Check if left side is the parameter
                if let Expr::Ident(ident) = Self::strip_parens(&bin.left)
                    && ident.sym == param_name
                    && let Some(val) = eval_const_i64(&bin.right).map(|v| v as i32)
                {
                    return match bin.op {
                        BinaryOp::Sub => Some(-val), // param - val means offset of -val
                        BinaryOp::Add => Some(val),
                        _ => None,
                    };
                }
                None
            }
            _ => None,
        }
    }

    /// Find which parameter index is used for a call argument
    #[must_use]
    fn find_param_index(params: &[Param], name: &str) -> Option<usize> {
        params.iter().position(|p| {
            if let Pat::Ident(binding) = &p.pat {
                binding.id.sym == name
            } else {
                false
            }
        })
    }

    /// Extract parameter name from expression
    #[must_use]
    fn extract_param_name(expr: &Expr) -> Option<String> {
        match Self::strip_parens(expr) {
            Expr::Ident(ident) => Some(ident.sym.to_string()),
            Expr::Bin(bin) => match Self::strip_parens(&bin.left) {
                Expr::Ident(ident) => Some(ident.sym.to_string()),
                _ => None,
            },
            _ => None,
        }
    }
}

impl VisitMut for FunctionReferenceFinder<'_> {
    fn visit_mut_fn_decl(&mut self, func: &mut FnDecl) {
        func.visit_mut_children_with(self);

        let fn_name = func.ident.sym.to_string();
        let params = &func.function.params;

        // Look for simple wrapper pattern: function name(...) { return decoder(...); }
        if let Some(body) = &func.function.body {
            let non_empty: Vec<_> = body
                .stmts
                .iter()
                .filter(|s| !matches!(s, Stmt::Empty(_)))
                .collect();

            // Must have exactly one return statement
            if non_empty.len() == 1
                && let Stmt::Return(ret) = non_empty[0]
                && let Some(arg) = &ret.arg
                && let Expr::Call(call) = &**arg
                && let Callee::Expr(callee) = &call.callee
                && let Expr::Ident(callee_ident) = &**callee
            {
                let callee_name = callee_ident.sym.to_string();

                if self.is_known_decoder(&callee_name) {
                    let (callee_index_arg, callee_key_arg) =
                        self.callee_arg_positions(&callee_name);
                    // Found a wrapper! Extract argument mapping
                    let mut additional_offset = 0i32;
                    let mut index_argument = None;
                    let mut key_argument = None;

                    // Analyze arguments
                    for (i, arg) in call.args.iter().enumerate() {
                        if let Some(param_name) = Self::extract_param_name(&arg.expr)
                            && let Some(offset) = Self::extract_arg_offset(&arg.expr, &param_name)
                            && let Some(param_idx) = Self::find_param_index(params, &param_name)
                        {
                            if i == callee_index_arg {
                                index_argument = Some(param_idx);
                                additional_offset = offset;
                            }
                            if i == callee_key_arg {
                                key_argument = Some(param_idx);
                            }
                        }
                    }

                    self.references.push(DecoderReference {
                        identifier: fn_name,
                        real_identifier: callee_name,
                        additional_offset,
                        index_argument,
                        key_argument,
                    });
                }
            }
        }
    }
}
