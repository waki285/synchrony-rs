//! ControlFlow transformer
//!
//! Handles control flow flattening deobfuscation.
//! This includes:
//! - Finding and resolving control flow storage objects
//! - Deflattening switch-based control flow

use std::collections::HashMap;

use swc_common::{Span, SyntaxContext};
use swc_ecma_ast::*;
use swc_ecma_visit::{VisitMut, VisitMutWith};

use crate::context::{Context, ControlFlowFunction, ControlFlowLiteral, ControlFlowStorage};
use crate::error::Result;
use crate::transformers::Transformer;

/// ControlFlow transformer.
///
/// Resolves flattened switch-based control flow and related storage helpers.
#[derive(Debug)]
pub struct ControlFlow;

impl ControlFlow {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for ControlFlow {
    fn default() -> Self {
        Self::new()
    }
}

impl Transformer for ControlFlow {
    fn name(&self) -> &'static str {
        "ControlFlow"
    }

    fn transform(&self, context: &mut Context) -> Result<()> {
        // First pass: populate empty objects with setters in same block
        let mut populator = EmptyObjectPopulator;
        context.ast.visit_mut_with(&mut populator);

        // Second pass: find control flow storage nodes, aliases, and replace usages
        let (ast, storage_nodes) = (&mut context.ast, &mut context.control_flow_storage_nodes);
        // Be conservative: keep storage objects to avoid removing unresolved references.
        let mut storage_pass = ControlFlowStoragePass::new(storage_nodes, false);
        ast.visit_mut_with(&mut storage_pass);

        // Third pass: deflatten while-switch patterns
        let mut deflattener = ControlFlowDeflattener::new();
        ast.visit_mut_with(&mut deflattener);

        Ok(())
    }
}

fn block_id(block: &BlockStmt) -> String {
    format!("{}!{}", block.span.lo.0, block.span.hi.0)
}

/// Populates empty object declarations with setters from the same block
///
/// Transforms:
/// ```js
/// var obj = {};
/// obj.a = 1;
/// obj.b = "hello";
/// ```
/// Into:
/// ```js
/// var obj = { a: 1, b: "hello" };
/// ```
struct EmptyObjectPopulator;

impl VisitMut for EmptyObjectPopulator {
    fn visit_mut_block_stmt(&mut self, block: &mut BlockStmt) {
        block.visit_mut_children_with(self);

        // Find empty object declarations
        let mut obj_names: Vec<String> = Vec::new();

        for stmt in &block.stmts {
            if let Stmt::Decl(Decl::Var(var_decl)) = stmt {
                for decl in &var_decl.decls {
                    if let Pat::Ident(binding) = &decl.name
                        && let Some(init) = &decl.init
                        && let Expr::Object(obj) = &**init
                        && obj.props.is_empty()
                    {
                        obj_names.push(binding.id.sym.to_string());
                    }
                }
            }
        }

        if obj_names.is_empty() {
            return;
        }

        // Collect setters and properties for each empty object
        let mut obj_props: HashMap<String, Vec<PropOrSpread>> = HashMap::new();
        let mut stmts_to_remove: Vec<usize> = Vec::new();

        for (idx, stmt) in block.stmts.iter().enumerate() {
            if let Stmt::Expr(expr_stmt) = stmt
                && let Expr::Assign(assign) = &*expr_stmt.expr
                && assign.op == AssignOp::Assign
                && let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = &assign.left
                && let Expr::Ident(obj_ident) = &*member.obj
            {
                let obj_name = obj_ident.sym.to_string();
                if obj_names.contains(&obj_name) {
                    // Get property name
                    if let MemberProp::Ident(prop_ident) = &member.prop {
                        let prop = PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
                            key: PropName::Ident(IdentName {
                                span: Default::default(),
                                sym: prop_ident.sym.clone(),
                            }),
                            value: assign.right.clone(),
                        })));

                        obj_props.entry(obj_name).or_default().push(prop);
                        stmts_to_remove.push(idx);
                    }
                }
            }
        }

        // Add properties to empty objects
        for stmt in &mut block.stmts {
            if let Stmt::Decl(Decl::Var(var_decl)) = stmt {
                for decl in &mut var_decl.decls {
                    if let Pat::Ident(binding) = &decl.name {
                        let name = binding.id.sym.to_string();
                        if let Some(props) = obj_props.remove(&name)
                            && let Some(init) = &mut decl.init
                            && let Expr::Object(obj) = &mut **init
                        {
                            obj.props = props;
                        }
                    }
                }
            }
        }

        // Remove setter statements (in reverse order)
        for idx in stmts_to_remove.into_iter().rev() {
            block.stmts[idx] = Stmt::Empty(EmptyStmt {
                span: Default::default(),
            });
        }

        // Clean up empty statements
        block.stmts.retain(|s| !matches!(s, Stmt::Empty(_)));
    }
}

/// Pass that finds control flow storage nodes, aliases, and replaces usages.
struct ControlFlowStoragePass<'a> {
    storage_nodes: &'a mut HashMap<String, ControlFlowStorage>,
    remove_garbage: bool,
}

impl<'a> ControlFlowStoragePass<'a> {
    fn new(
        storage_nodes: &'a mut HashMap<String, ControlFlowStorage>,
        remove_garbage: bool,
    ) -> Self {
        Self {
            storage_nodes,
            remove_garbage,
        }
    }

    fn is_control_flow_object(obj: &ObjectLit) -> bool {
        if obj.props.is_empty() {
            return false;
        }

        obj.props.iter().all(|prop| match prop {
            PropOrSpread::Prop(prop) => {
                if let Prop::KeyValue(kv) = &**prop {
                    let key = match &kv.key {
                        PropName::Ident(id) => Some(id.sym.to_string()),
                        PropName::Str(s) => s.value.as_str().map(|v| v.to_string()),
                        PropName::Num(n) => Some(n.value.to_string()),
                        _ => None,
                    };

                    let key_len = key.as_ref().map_or(0, |s| s.len());
                    if key_len != 5 {
                        return false;
                    }

                    matches!(&*kv.value, Expr::Lit(_) | Expr::Fn(_))
                } else {
                    false
                }
            }
            PropOrSpread::Spread(_) => false,
        })
    }

    fn extract_simple_function(func: &Function) -> Option<Box<Function>> {
        let body = func.body.as_ref()?;
        let mut stmts: Vec<Stmt> = body
            .stmts
            .iter()
            .filter(|s| !matches!(s, Stmt::Empty(_)))
            .cloned()
            .collect();

        if stmts.len() != 1 {
            return None;
        }

        if !matches!(stmts[0], Stmt::Return(_)) {
            return None;
        }

        let mut new_func = func.clone();
        new_func.body = Some(BlockStmt {
            span: body.span,
            ctxt: body.ctxt,
            stmts: std::mem::take(&mut stmts),
        });

        Some(Box::new(new_func))
    }

    fn collect_storage_for_block(&mut self, block: &mut BlockStmt) -> Option<String> {
        if block.stmts.is_empty() {
            return None;
        }

        let bid = block_id(block);
        if self.storage_nodes.contains_key(&bid) {
            return Some(bid);
        }

        let mut last_identifier: Option<String> = None;

        for stmt in &mut block.stmts {
            let Stmt::Decl(Decl::Var(var_decl)) = stmt else {
                continue;
            };
            let mut decls_to_remove: Vec<usize> = Vec::new();

            for (idx, decl) in var_decl.decls.iter_mut().enumerate() {
                let Pat::Ident(binding) = &decl.name else {
                    continue;
                };
                let Some(init) = &decl.init else { continue };
                let Expr::Object(obj) = &**init else { continue };

                if !Self::is_control_flow_object(obj) {
                    continue;
                }

                let identifier = binding.id.sym.to_string();
                let mut storage = ControlFlowStorage {
                    identifier: identifier.clone(),
                    aliases: vec![identifier.clone()],
                    functions: Vec::new(),
                    literals: Vec::new(),
                };

                for prop in &obj.props {
                    let PropOrSpread::Prop(prop) = prop else {
                        continue;
                    };
                    let Prop::KeyValue(kv) = &**prop else {
                        continue;
                    };

                    let key = match &kv.key {
                        PropName::Ident(id) => Some(id.sym.to_string()),
                        PropName::Str(s) => s.value.as_str().map(|v| v.to_string()),
                        PropName::Num(n) => Some(n.value.to_string()),
                        _ => None,
                    };
                    let Some(key) = key else { continue };

                    match &*kv.value {
                        Expr::Lit(lit) => {
                            if let Some(existing) =
                                storage.literals.iter_mut().find(|l| l.identifier == key)
                            {
                                existing.value = lit.clone();
                            } else {
                                storage.literals.push(ControlFlowLiteral {
                                    identifier: key,
                                    value: lit.clone(),
                                });
                            }
                        }
                        Expr::Fn(fn_expr) => {
                            if let Some(node) = Self::extract_simple_function(&fn_expr.function) {
                                if let Some(existing) =
                                    storage.functions.iter_mut().find(|f| f.identifier == key)
                                {
                                    existing.node = node;
                                } else {
                                    storage.functions.push(ControlFlowFunction {
                                        identifier: key,
                                        node,
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }

                self.storage_nodes.insert(bid.clone(), storage);
                last_identifier = Some(identifier);

                if self.remove_garbage {
                    decls_to_remove.push(idx);
                }
            }

            if self.remove_garbage && !decls_to_remove.is_empty() {
                for idx in decls_to_remove.into_iter().rev() {
                    var_decl.decls.remove(idx);
                }
                if var_decl.decls.is_empty() {
                    *stmt = Stmt::Empty(EmptyStmt {
                        span: Default::default(),
                    });
                }
            }
        }

        if last_identifier.is_some() {
            Some(bid)
        } else {
            None
        }
    }

    fn find_aliases_in_block(
        remove_garbage: bool,
        block: &mut BlockStmt,
        storage: &mut ControlFlowStorage,
    ) {
        for stmt in &mut block.stmts {
            let Stmt::Decl(Decl::Var(var_decl)) = stmt else {
                continue;
            };
            let mut decls_to_remove: Vec<usize> = Vec::new();

            for (idx, decl) in var_decl.decls.iter().enumerate() {
                let (Pat::Ident(binding), Some(init)) = (&decl.name, &decl.init) else {
                    continue;
                };
                let Expr::Ident(init_ident) = &**init else {
                    continue;
                };
                if storage.aliases.contains(&init_ident.sym.to_string()) {
                    let alias = binding.id.sym.to_string();
                    if !storage.aliases.contains(&alias) {
                        storage.aliases.push(alias);
                    }
                    if remove_garbage {
                        decls_to_remove.push(idx);
                    }
                }
            }

            if remove_garbage && !decls_to_remove.is_empty() {
                for idx in decls_to_remove.into_iter().rev() {
                    var_decl.decls.remove(idx);
                }
                if var_decl.decls.is_empty() {
                    *stmt = Stmt::Empty(EmptyStmt {
                        span: Default::default(),
                    });
                }
            }
        }

        block.stmts.retain(|stmt| !matches!(stmt, Stmt::Empty(_)));
    }
}

impl VisitMut for ControlFlowStoragePass<'_> {
    fn visit_mut_script(&mut self, script: &mut Script) {
        script.visit_mut_children_with(self);
        self.process_statement_list(script.span, &mut script.body);
    }

    fn visit_mut_module(&mut self, module: &mut Module) {
        module.visit_mut_children_with(self);
        self.process_module_items(module.span, &mut module.body);
    }

    fn visit_mut_block_stmt(&mut self, block: &mut BlockStmt) {
        block.visit_mut_children_with(self);

        let bid = match self.collect_storage_for_block(block) {
            Some(bid) => bid,
            None => return,
        };

        let Some(mut storage) = self.storage_nodes.remove(&bid) else {
            return;
        };

        Self::find_aliases_in_block(self.remove_garbage, block, &mut storage);

        let snapshot = storage.clone();
        self.storage_nodes.insert(bid, storage);

        let mut replacer = ControlFlowReplacer::new(snapshot);
        block.visit_mut_with(&mut replacer);
    }
}

impl ControlFlowStoragePass<'_> {
    fn process_statement_list(&mut self, span: Span, stmts: &mut Vec<Stmt>) {
        let mut block = BlockStmt {
            span,
            ctxt: SyntaxContext::empty(),
            stmts: std::mem::take(stmts),
        };
        self.visit_mut_block_stmt(&mut block);
        *stmts = block.stmts;
    }

    fn process_module_items(&mut self, span: Span, items: &mut Vec<ModuleItem>) {
        let mut stmts: Vec<Stmt> = items
            .iter()
            .filter_map(|item| match item {
                ModuleItem::Stmt(stmt) => Some(stmt.clone()),
                _ => None,
            })
            .collect();

        self.process_statement_list(span, &mut stmts);

        let mut stmt_iter = stmts.into_iter();
        let mut new_items: Vec<ModuleItem> = Vec::with_capacity(items.len());

        for item in items.drain(..) {
            match item {
                ModuleItem::Stmt(_) => {
                    if let Some(stmt) = stmt_iter.next() {
                        new_items.push(ModuleItem::Stmt(stmt));
                    }
                }
                other => new_items.push(other),
            }
        }

        for stmt in stmt_iter {
            new_items.push(ModuleItem::Stmt(stmt));
        }

        *items = new_items;
    }
}

struct ControlFlowReplacer {
    storage: ControlFlowStorage,
}

impl ControlFlowReplacer {
    fn new(storage: ControlFlowStorage) -> Self {
        Self { storage }
    }

    fn translate_call_expr(&self, func: &Function, call: &CallExpr) -> Option<Expr> {
        let body = func.body.as_ref()?;
        let stmts: Vec<&Stmt> = body
            .stmts
            .iter()
            .filter(|s| !matches!(s, Stmt::Empty(_)))
            .collect();

        if stmts.len() != 1 {
            return None;
        }

        let Stmt::Return(ret) = stmts[0] else {
            return None;
        };
        let return_expr = ret.arg.as_ref()?.clone();

        let mut param_map: HashMap<String, Expr> = HashMap::new();
        for (idx, param) in func.params.iter().enumerate() {
            let Pat::Ident(binding) = &param.pat else {
                continue;
            };
            if let Some(arg) = call.args.get(idx) {
                param_map.insert(binding.id.sym.to_string(), (*arg.expr).clone());
            }
        }

        let mut result = *return_expr;
        let mut substitutor = ParameterSubstitutor { param_map };
        result.visit_mut_with(&mut substitutor);
        Some(result)
    }
}

impl VisitMut for ControlFlowReplacer {
    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        expr.visit_mut_children_with(self);

        if let Expr::Member(member) = expr
            && let Expr::Ident(obj) = &*member.obj
            && self.storage.aliases.contains(&obj.sym.to_string())
        {
            let prop_name = match &member.prop {
                MemberProp::Ident(prop) => Some(prop.sym.to_string()),
                MemberProp::Computed(comp) => match &*comp.expr {
                    Expr::Lit(Lit::Str(s)) => s.value.as_str().map(|v| v.to_string()),
                    Expr::Lit(Lit::Num(n)) => Some(n.value.to_string()),
                    _ => None,
                },
                _ => None,
            };

            if let Some(prop_name) = prop_name
                && let Some(lit) = self
                    .storage
                    .literals
                    .iter()
                    .find(|l| l.identifier == prop_name)
            {
                *expr = Expr::Lit(lit.value.clone());
                return;
            }
        }

        if let Expr::Call(call) = expr
            && let Callee::Expr(callee) = &call.callee
            && let Expr::Member(member) = &**callee
            && let Expr::Ident(obj) = &*member.obj
            && self.storage.aliases.contains(&obj.sym.to_string())
        {
            let prop_name = match &member.prop {
                MemberProp::Ident(prop) => Some(prop.sym.to_string()),
                MemberProp::Computed(comp) => match &*comp.expr {
                    Expr::Lit(Lit::Str(s)) => s.value.as_str().map(|v| v.to_string()),
                    Expr::Lit(Lit::Num(n)) => Some(n.value.to_string()),
                    _ => None,
                },
                _ => None,
            };

            if let Some(prop_name) = prop_name
                && let Some(func) = self
                    .storage
                    .functions
                    .iter()
                    .find(|f| f.identifier == prop_name)
                && let Some(new_expr) = self.translate_call_expr(&func.node, call)
            {
                *expr = new_expr;
            }
        }
    }
}

struct ParameterSubstitutor {
    param_map: HashMap<String, Expr>,
}

impl VisitMut for ParameterSubstitutor {
    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        if let Expr::Ident(ident) = expr
            && let Some(replacement) = self.param_map.get(&ident.sym.to_string())
        {
            let mut replacement = replacement.clone();
            // Wrap complex replacements to preserve operator precedence.
            if !matches!(replacement, Expr::Lit(_) | Expr::Ident(_) | Expr::Paren(_)) {
                replacement = Expr::Paren(ParenExpr {
                    span: Default::default(),
                    expr: Box::new(replacement),
                });
            }
            *expr = replacement;
            return;
        }
        expr.visit_mut_children_with(self);
    }
}

/// Deflattens while+switch control flow patterns
struct ControlFlowDeflattener;

impl ControlFlowDeflattener {
    fn new() -> Self {
        Self
    }

    fn extract_shuffle_array(expr: &Expr) -> Option<Vec<String>> {
        let Expr::Call(call) = expr else { return None };
        let Callee::Expr(callee) = &call.callee else {
            return None;
        };
        let Expr::Member(member) = &**callee else {
            return None;
        };

        let MemberProp::Ident(prop) = &member.prop else {
            return None;
        };
        if prop.sym.as_ref() != "split" {
            return None;
        }

        let Expr::Lit(Lit::Str(split_target)) = &*member.obj else {
            return None;
        };
        let arg = call.args.first()?;
        let Expr::Lit(Lit::Str(sep)) = &*arg.expr else {
            return None;
        };

        let target = split_target.value.as_str()?;
        let sep = sep.value.as_str()?;
        Some(target.split(sep).map(|s| s.to_string()).collect())
    }
}

impl VisitMut for ControlFlowDeflattener {
    fn visit_mut_script(&mut self, script: &mut Script) {
        script.visit_mut_children_with(self);
        self.process_statement_list(&mut script.body);
    }

    fn visit_mut_module(&mut self, module: &mut Module) {
        module.visit_mut_children_with(self);
        self.process_module_items(&mut module.body);
    }

    fn visit_mut_block_stmt(&mut self, block: &mut BlockStmt) {
        block.visit_mut_children_with(self);
        self.process_statement_list(&mut block.stmts);
    }
}

impl ControlFlowDeflattener {
    fn process_statement_list(&mut self, stmts: &mut Vec<Stmt>) {
        let mut replacements: Vec<(usize, Vec<Stmt>)> = Vec::new();
        let mut remove_decl_indices: Vec<usize> = Vec::new();

        let stmts_snapshot = stmts.clone();
        for (idx, stmt) in stmts_snapshot.iter().enumerate() {
            let Stmt::While(while_stmt) = stmt else {
                continue;
            };
            let Expr::Lit(Lit::Bool(test)) = &*while_stmt.test else {
                continue;
            };
            if !test.value {
                continue;
            }

            let Stmt::Block(body_block) = &*while_stmt.body else {
                continue;
            };
            let Some(Stmt::Switch(switch_stmt)) = body_block.stmts.first() else {
                continue;
            };

            let Expr::Member(discriminant) = &*switch_stmt.discriminant else {
                continue;
            };
            let Expr::Ident(shuffle_ident) = &*discriminant.obj else {
                continue;
            };
            let MemberProp::Computed(computed) = &discriminant.prop else {
                continue;
            };
            let Expr::Update(update) = &*computed.expr else {
                continue;
            };
            if update.op != UpdateOp::PlusPlus || update.prefix {
                continue;
            }
            let Expr::Ident(index_ident) = &*update.arg else {
                continue;
            };

            let shuffle_id = shuffle_ident.sym.to_string();
            let index_id = index_ident.sym.to_string();

            let mut shuffle_arr: Vec<String> = Vec::new();
            let mut start_idx: Option<i64> = None;

            for (decl_stmt_idx, stmt) in stmts.iter_mut().enumerate() {
                let Stmt::Decl(Decl::Var(var_decl)) = stmt else {
                    continue;
                };
                let mut decls_to_remove: Vec<usize> = Vec::new();

                for (decl_idx, decl) in var_decl.decls.iter().enumerate() {
                    let Pat::Ident(binding) = &decl.name else {
                        continue;
                    };
                    let Some(init) = &decl.init else { continue };

                    if binding.id.sym == shuffle_id {
                        if let Some(arr) = Self::extract_shuffle_array(init) {
                            shuffle_arr = arr;
                            decls_to_remove.push(decl_idx);
                        }
                    } else if binding.id.sym == index_id
                        && let Expr::Lit(Lit::Num(num)) = &**init
                    {
                        start_idx = Some(num.value as i64);
                        decls_to_remove.push(decl_idx);
                    }
                }

                if !decls_to_remove.is_empty() {
                    for decl_idx in decls_to_remove.into_iter().rev() {
                        var_decl.decls.remove(decl_idx);
                    }
                    if var_decl.decls.is_empty() {
                        remove_decl_indices.push(decl_stmt_idx);
                    }
                }
            }

            if shuffle_arr.is_empty() || start_idx.is_none() {
                continue;
            }

            let start_idx = start_idx.unwrap();
            if start_idx < 0 {
                continue;
            }
            let start_idx = start_idx as usize;
            if start_idx >= shuffle_arr.len() {
                continue;
            }

            let mut nodes: Vec<Stmt> = Vec::new();
            let mut ok = true;

            for case_num in shuffle_arr.iter().skip(start_idx) {
                let mut found = None;

                for case in &switch_stmt.cases {
                    if let Some(test) = &case.test
                        && let Expr::Lit(lit) = &**test
                        && let Lit::Str(s) = lit
                        && s.value.as_str().is_some_and(|v| v == case_num)
                    {
                        found = Some(case);
                        break;
                    }
                }

                let Some(caze) = found else {
                    ok = false;
                    break;
                };

                for stmt in &caze.cons {
                    if !matches!(stmt, Stmt::Continue(_)) {
                        nodes.push(stmt.clone());
                    }
                }
            }

            if !ok || nodes.is_empty() {
                continue;
            }

            replacements.push((idx, nodes));
        }

        for idx in remove_decl_indices.into_iter().rev() {
            stmts[idx] = Stmt::Empty(EmptyStmt {
                span: Default::default(),
            });
        }

        for (idx, nodes) in replacements.into_iter().rev() {
            stmts.splice(idx..=idx, nodes);
        }

        stmts.retain(|stmt| !matches!(stmt, Stmt::Empty(_)));
    }

    fn process_module_items(&mut self, items: &mut Vec<ModuleItem>) {
        let mut stmts: Vec<Stmt> = items
            .iter()
            .filter_map(|item| match item {
                ModuleItem::Stmt(stmt) => Some(stmt.clone()),
                _ => None,
            })
            .collect();

        self.process_statement_list(&mut stmts);

        let mut stmt_iter = stmts.into_iter();
        let mut new_items: Vec<ModuleItem> = Vec::with_capacity(items.len());

        for item in items.drain(..) {
            match item {
                ModuleItem::Stmt(_) => {
                    if let Some(stmt) = stmt_iter.next() {
                        new_items.push(ModuleItem::Stmt(stmt));
                    }
                }
                other => new_items.push(other),
            }
        }

        for stmt in stmt_iter {
            new_items.push(ModuleItem::Stmt(stmt));
        }

        *items = new_items;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Deobfuscator;
    use crate::deobfuscator::DeobfuscateOptions;
    use crate::transformers::Simplify;
    use std::sync::Arc;

    fn deob_with_controlflow(code: &str) -> String {
        let deob = Deobfuscator::new();
        let options = DeobfuscateOptions {
            custom_transformers: Some(vec![
                Arc::new(ControlFlow::new()),
                Arc::new(Simplify::new()),
            ]),
            ..Default::default()
        };
        deob.deobfuscate_source(code, Some(options)).unwrap()
    }

    #[test]
    fn test_controlflow_new() {
        let transformer = ControlFlow::new();
        assert_eq!(transformer.name(), "ControlFlow");
    }

    #[test]
    fn test_controlflow_literal_replacement() {
        let code = r#"var _0x = { "ABcDe": "hello" }; console.log(_0x.ABcDe);"#;
        let result = deob_with_controlflow(code);
        assert!(result.contains("\"hello\""));
    }

    #[test]
    fn test_controlflow_literal_replacement_computed() {
        let code = r#"var _0x = { "ABcDe": "hello" }; console.log(_0x["ABcDe"]);"#;
        let result = deob_with_controlflow(code);
        assert!(result.contains("\"hello\""));
    }

    #[test]
    fn test_controlflow_function_inline() {
        let code = r#"var _0x = { "ABcDe": function(a, b) { return a + b; } }; _0x.ABcDe(1, 2);"#;
        let result = deob_with_controlflow(code);
        assert!(result.contains("3"));
    }

    #[test]
    fn test_controlflow_alias_replacement() {
        let code = r#"
var _0xabcde = { "ABCDE": "hello", "FGhIj": function(a, b) { return a + b; } };
var _alias = _0xabcde;
console.log(_alias.ABCDE);
_alias.FGhIj(1, 2);
"#;
        let result = deob_with_controlflow(code);
        assert!(result.contains("\"hello\""));
        assert!(result.contains("3"));
    }

    #[test]
    fn test_populate_empty_objects() {
        let code = r#"
function test() {
    var obj = {};
    obj.a = 1;
    obj.b = "hello";
    return obj;
}
"#;
        let result = deob_with_controlflow(code);
        // The object should be populated inline
        assert!(result.contains("a:") || result.contains("a :"));
        assert!(result.contains("b:") || result.contains("b :"));
        assert!(!result.contains("obj.a ="));
        assert!(!result.contains("obj.b ="));
    }

    #[test]
    fn test_controlflow_deflatten_switch() {
        let code = r#"
var _arr = "0|1|2".split("|");
var _idx = 0;
while (true) {
  switch (_arr[_idx++]) {
    case '0': a(); continue;
    case '1': b(); continue;
    case '2': c(); break;
  }
    }
"#;
        let result = deob_with_controlflow(code);
        assert!(result.contains("a()"));
        assert!(result.contains("b()"));
        assert!(result.contains("c()"));
        assert!(!result.contains("switch"));
    }
}
