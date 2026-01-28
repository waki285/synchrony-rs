use std::collections::{HashMap, HashSet};
use swc_ecma_ast::*;
use swc_ecma_visit::{VisitMut, VisitMutWith as _};

use crate::context::StringArrayType;

pub(super) struct StringArrayFinder {
    pub(super) arrays: HashMap<String, (StringArrayType, Vec<String>)>,
}

impl StringArrayFinder {
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            arrays: HashMap::new(),
        }
    }

    #[must_use]
    pub(super) fn extract_string_array(arr: &ArrayLit) -> Option<Vec<String>> {
        let mut strings = Vec::new();

        for elem in &arr.elems {
            match elem {
                Some(ExprOrSpread { expr, spread: None }) => {
                    if let Expr::Lit(Lit::Str(s)) = &**expr {
                        if let Some(v) = s.value.as_str() {
                            strings.push(v.to_owned());
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    }
                }
                _ => return None,
            }
        }

        Some(strings)
    }

    #[must_use]
    fn strip_parens(expr: &Expr) -> &Expr {
        match expr {
            Expr::Paren(paren) => Self::strip_parens(&paren.expr),
            _ => expr,
        }
    }
}

impl VisitMut for StringArrayFinder {
    fn visit_mut_var_decl(&mut self, decl: &mut VarDecl) {
        decl.visit_mut_children_with(self);

        for declarator in &decl.decls {
            if let Pat::Ident(binding) = &declarator.name
                && let Some(init) = &declarator.init
                && let Expr::Array(arr) = &**init
            {
                // Check if this looks like a string array (many string elements)
                if arr.elems.len() >= 5
                    && let Some(strings) = Self::extract_string_array(arr)
                {
                    let name = binding.id.sym.to_string();
                    self.arrays.insert(name, (StringArrayType::Array, strings));
                }
            }
        }
    }

    fn visit_mut_fn_decl(&mut self, func: &mut FnDecl) {
        func.visit_mut_children_with(self);

        // Check for function form string array:
        // function _0x1234() {
        //   var arr = ["str1", "str2", ...];
        //   _0x1234 = function() { return arr; };
        //   return _0x1234();
        // }

        let fn_name = func.ident.sym.to_string();

        if let Some(body) = &func.function.body {
            let stmts: Vec<_> = body.stmts.iter().collect();

            // Must have exactly 3 statements
            if stmts.len() != 3 {
                return;
            }

            let [first, second, third] = stmts.as_slice() else {
                return;
            };

            // First statement: var declaration with array
            let array_strings = match first {
                Stmt::Decl(Decl::Var(var_decl)) if var_decl.decls.len() == 1 => {
                    var_decl.decls.first().and_then(|decl| {
                        let init = decl.init.as_ref()?;
                        if let Expr::Array(arr) = &**init {
                            Self::extract_string_array(arr)
                        } else {
                            None
                        }
                    })
                }
                _ => None,
            };

            let Some(strings) = array_strings else {
                return;
            };

            // Second statement: assignment expression _0x1234 = function() { return arr; }
            let is_valid_assignment = match second {
                Stmt::Expr(expr_stmt) => {
                    if let Expr::Assign(assign) = &*expr_stmt.expr {
                        if let AssignTarget::Simple(SimpleAssignTarget::Ident(target)) =
                            &assign.left
                        {
                            target.id.sym == fn_name
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                }
                _ => false,
            };

            if !is_valid_assignment {
                return;
            }

            // Third statement: return statement
            let is_return = matches!(third, Stmt::Return(_));

            if is_return {
                self.arrays
                    .insert(fn_name, (StringArrayType::Function, strings));
            }
        }
    }

    fn visit_mut_assign_expr(&mut self, assign: &mut AssignExpr) {
        assign.visit_mut_children_with(self);

        if assign.op != AssignOp::Assign {
            return;
        }

        let Some(binding) = assign.left.as_ident() else {
            return;
        };

        let Expr::Array(arr) = Self::strip_parens(&assign.right) else {
            return;
        };

        if arr.elems.len() >= 5
            && let Some(strings) = Self::extract_string_array(arr)
        {
            let name = binding.id.sym.to_string();
            self.arrays
                .entry(name)
                .or_insert((StringArrayType::Array, strings));
        }
    }
}

pub(super) struct StringArrayDeclarationRemover {
    variable_arrays: HashSet<String>,
    function_arrays: HashSet<String>,
}

impl StringArrayDeclarationRemover {
    pub(super) fn new(arrays: &HashMap<String, (StringArrayType, Vec<String>)>) -> Self {
        let mut variable_arrays = HashSet::new();
        let mut function_arrays = HashSet::new();

        #[expect(
            clippy::iter_over_hash_type,
            reason = "Array registration order is not significant"
        )]
        for (name, (array_type, _)) in arrays {
            match array_type {
                StringArrayType::Array => {
                    variable_arrays.insert(name.clone());
                }
                StringArrayType::Function => {
                    function_arrays.insert(name.clone());
                }
            }
        }

        Self {
            variable_arrays,
            function_arrays,
        }
    }
}

impl VisitMut for StringArrayDeclarationRemover {
    fn visit_mut_module_items(&mut self, items: &mut Vec<ModuleItem>) {
        for item in items.iter_mut() {
            item.visit_mut_children_with(self);
        }

        items.retain(|item| match item {
            ModuleItem::Stmt(Stmt::Decl(Decl::Fn(fn_decl))) => {
                !self.function_arrays.contains(fn_decl.ident.sym.as_ref())
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
                !self.function_arrays.contains(fn_decl.ident.sym.as_ref())
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
            if !self.variable_arrays.contains(binding.id.sym.as_ref()) {
                return true;
            }
            let Some(init) = &declarator.init else {
                return true;
            };
            let Expr::Array(arr) = &**init else {
                return true;
            };
            !(arr.elems.len() >= 5 && StringArrayFinder::extract_string_array(arr).is_some())
        });
    }
}
