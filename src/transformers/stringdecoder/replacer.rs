use std::collections::HashSet;
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitMut, VisitMutWith, VisitWith};

use crate::context::{DecoderFunction, DecoderFunctionType, DecoderReference, StringArray};

use super::core::StringDecoder;

/// Sixth pass: replace decoder calls
pub(super) struct StringDecoderReplacer<'a> {
    string_arrays: &'a [StringArray],
    string_decoders: &'a [DecoderFunction],
    decoder_references: &'a [DecoderReference],
}

impl<'a> StringDecoderReplacer<'a> {
    #[must_use]
    pub(super) const fn new(
        string_arrays: &'a [StringArray],
        string_decoders: &'a [DecoderFunction],
        decoder_references: &'a [DecoderReference],
    ) -> Self {
        Self {
            string_arrays,
            string_decoders,
            decoder_references,
        }
    }

    /// Resolve a reference to find the actual decoder and total offset
    #[must_use]
    fn resolve_decoder(&self, name: &str) -> Option<(&DecoderFunction, i32, usize, usize)> {
        // Direct decoder lookup
        if let Some(decoder) = self.string_decoders.iter().find(|d| d.identifier == name) {
            return Some((decoder, 0, decoder.index_argument, decoder.key_argument));
        }

        // Reference chain lookup
        let mut current_name = name.to_string();
        let mut total_offset = 0i32;
        let mut index_argument = None;
        let mut key_argument = None;
        let mut visited = vec![];

        loop {
            if visited.contains(&current_name) {
                return None; // Circular reference
            }
            visited.push(current_name.clone());

            if let Some(reference) = self
                .decoder_references
                .iter()
                .find(|r| r.identifier == current_name)
            {
                total_offset += reference.additional_offset;

                // Capture the call-site argument mapping once; deeper mappings are
                // relative to intermediate wrappers, not the original call.
                if index_argument.is_none() {
                    index_argument = reference.index_argument;
                }
                if key_argument.is_none() {
                    key_argument = reference.key_argument;
                }
                current_name = reference.real_identifier.clone();

                // Check if we've reached a decoder
                if let Some(decoder) = self
                    .string_decoders
                    .iter()
                    .find(|d| d.identifier == current_name)
                {
                    return Some((
                        decoder,
                        total_offset,
                        index_argument.unwrap_or(decoder.index_argument),
                        key_argument.unwrap_or(decoder.key_argument),
                    ));
                }
            } else {
                return None;
            }
        }
    }

    #[must_use]
    fn decode(&self, decoder_name: &str, args: &[ExprOrSpread]) -> Option<String> {
        let (decoder, extra_offset, index_argument, key_argument) =
            self.resolve_decoder(decoder_name)?;

        let string_array = self
            .string_arrays
            .iter()
            .find(|a| a.identifier == decoder.string_array_identifier)?;

        // Get the index argument
        let index = if index_argument < args.len() {
            match &*args[index_argument].expr {
                Expr::Lit(Lit::Num(n)) => n.value as i32,
                Expr::Lit(Lit::Str(s)) => parse_index_str(s.value.as_str()?)?,
                Expr::Unary(unary) if unary.op == UnaryOp::Minus => {
                    if let Expr::Lit(Lit::Num(n)) = &*unary.arg {
                        -(n.value as i32)
                    } else {
                        return None;
                    }
                }
                _ => return None,
            }
        } else {
            return None;
        };

        let actual_index = (index + decoder.offset + extra_offset) as usize;

        if actual_index >= string_array.strings.len() {
            return None;
        }

        let raw_string = &string_array.strings[actual_index];

        match decoder.decoder_type {
            DecoderFunctionType::Simple => Some(raw_string.clone()),
            DecoderFunctionType::Base64 => {
                let charset = decoder.charset.as_ref()?;
                StringDecoder::base64_decode(charset, raw_string)
            }
            DecoderFunctionType::Rc4 => {
                // Get the key argument
                let key = if key_argument < args.len() {
                    match &*args[key_argument].expr {
                        Expr::Lit(Lit::Str(s)) => s.value.as_str()?.to_string(),
                        _ => return None,
                    }
                } else {
                    return None;
                };

                let charset = decoder
                    .charset
                    .as_deref()
                    .unwrap_or("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=");
                StringDecoder::rc4_decrypt(charset, raw_string, &key)
            }
            DecoderFunctionType::Base91 => {
                let charset = decoder.charset.as_ref()?;
                StringDecoder::base91_decode(charset, raw_string)
            }
        }
    }
}

impl VisitMut for StringDecoderReplacer<'_> {
    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        expr.visit_mut_children_with(self);

        // Look for call expressions to decoder functions
        if let Expr::Call(call) = expr
            && let Callee::Expr(callee) = &call.callee
            && let Expr::Ident(ident) = &**callee
        {
            let name = ident.sym.to_string();

            if let Some(decoded) = self.decode(&name, &call.args) {
                *expr = Expr::Lit(Lit::Str(Str {
                    span: Default::default(),
                    value: decoded.into(),
                    raw: None,
                }));
            }
        }
    }
}

#[must_use]
pub(super) fn parse_index_str(value: &str) -> Option<i32> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (neg, rest) =
        trimmed
            .strip_prefix('-')
            .map_or((false, trimmed), |stripped| (true, stripped));

    let parsed = if let Some(hex) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        i32::from_str_radix(hex, 16).ok()?
    } else {
        rest.parse::<i32>().ok()?
    };

    Some(if neg { -parsed } else { parsed })
}

struct DecoderCallFinder<'a> {
    decoder_names: &'a HashSet<String>,
    found: bool,
}

impl<'a> DecoderCallFinder<'a> {
    const fn new(decoder_names: &'a HashSet<String>) -> Self {
        Self {
            decoder_names,
            found: false,
        }
    }
}

impl Visit for DecoderCallFinder<'_> {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        if self.found {
            return;
        }

        if let Callee::Expr(callee) = &call.callee
            && let Expr::Ident(ident) = &**callee
            && self.decoder_names.contains(ident.sym.as_ref())
        {
            self.found = true;
            return;
        }

        call.visit_children_with(self);
    }
}

pub(super) fn remove_tainted_statements(
    program: &mut Program,
    roots: &HashSet<String>,
    decoder_names: &HashSet<String>,
) {
    if roots.is_empty() {
        return;
    }

    let declared = collect_top_level_declared_names(program);
    if declared.is_empty() {
        return;
    }

    match program {
        Program::Script(script) => {
            let tainted = mark_tainted_stmts(&script.body, roots, &declared);
            if tainted.iter().any(|v| *v) {
                if has_decoder_call_outside_taint(&script.body, &tainted, decoder_names) {
                    return;
                }
                let mut next = Vec::with_capacity(script.body.len());
                for (idx, stmt) in script.body.drain(..).enumerate() {
                    if !tainted[idx] {
                        next.push(stmt);
                    }
                }
                script.body = next;
            }
        }
        Program::Module(module) => {
            let tainted = mark_tainted_module_items(&module.body, roots, &declared);
            if tainted.iter().any(|v| *v) {
                if has_decoder_call_outside_taint_items(&module.body, &tainted, decoder_names) {
                    return;
                }
                let mut next = Vec::with_capacity(module.body.len());
                for (idx, item) in module.body.drain(..).enumerate() {
                    if !tainted[idx] {
                        next.push(item);
                    }
                }
                module.body = next;
            }
        }
    }
}

fn mark_tainted_module_items(
    items: &[ModuleItem],
    roots: &HashSet<String>,
    declared: &HashSet<String>,
) -> Vec<bool> {
    let mut tainted = roots.clone();
    let mut flags = vec![false; items.len()];

    loop {
        let mut changed = false;
        for (idx, item) in items.iter().enumerate() {
            let ModuleItem::Stmt(stmt) = item else {
                continue;
            };

            let names = collect_stmt_idents(stmt);
            if names.is_empty() {
                continue;
            }

            if names.iter().any(|name| tainted.contains(name)) {
                if !flags[idx] {
                    flags[idx] = true;
                }
                for name in names {
                    if declared.contains(&name) && tainted.insert(name) {
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    flags
}

fn has_decoder_call_outside_taint(
    stmts: &[Stmt],
    tainted: &[bool],
    decoder_names: &HashSet<String>,
) -> bool {
    if decoder_names.is_empty() {
        return false;
    }

    for (idx, stmt) in stmts.iter().enumerate() {
        if !tainted[idx] && stmt_has_decoder_call(stmt, decoder_names) {
            return true;
        }
    }

    false
}

fn has_decoder_call_outside_taint_items(
    items: &[ModuleItem],
    tainted: &[bool],
    decoder_names: &HashSet<String>,
) -> bool {
    if decoder_names.is_empty() {
        return false;
    }

    for (idx, item) in items.iter().enumerate() {
        if !tainted[idx] && module_item_has_decoder_call(item, decoder_names) {
            return true;
        }
    }

    false
}

fn stmt_has_decoder_call(stmt: &Stmt, decoder_names: &HashSet<String>) -> bool {
    let mut finder = DecoderCallFinder::new(decoder_names);
    stmt.visit_with(&mut finder);
    finder.found
}

fn module_item_has_decoder_call(item: &ModuleItem, decoder_names: &HashSet<String>) -> bool {
    let ModuleItem::Stmt(stmt) = item else {
        return false;
    };
    stmt_has_decoder_call(stmt, decoder_names)
}

fn mark_tainted_stmts(
    stmts: &[Stmt],
    roots: &HashSet<String>,
    declared: &HashSet<String>,
) -> Vec<bool> {
    let mut tainted = roots.clone();
    let mut flags = vec![false; stmts.len()];

    loop {
        let mut changed = false;
        for (idx, stmt) in stmts.iter().enumerate() {
            let names = collect_stmt_idents(stmt);
            if names.is_empty() {
                continue;
            }

            if names.iter().any(|name| tainted.contains(name)) {
                if !flags[idx] {
                    flags[idx] = true;
                }
                for name in names {
                    if declared.contains(&name) && tainted.insert(name) {
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    flags
}

fn collect_top_level_declared_names(program: &Program) -> HashSet<String> {
    let mut names = HashSet::new();
    match program {
        Program::Script(script) => {
            for stmt in &script.body {
                collect_decl_names_from_stmt(stmt, &mut names);
            }
        }
        Program::Module(module) => {
            for item in &module.body {
                if let ModuleItem::Stmt(stmt) = item {
                    collect_decl_names_from_stmt(stmt, &mut names);
                }
            }
        }
    }
    names
}

fn collect_decl_names_from_stmt(stmt: &Stmt, names: &mut HashSet<String>) {
    match stmt {
        Stmt::Decl(Decl::Var(var_decl)) => {
            for decl in &var_decl.decls {
                collect_pat_names(&decl.name, names);
            }
        }
        Stmt::Decl(Decl::Fn(fn_decl)) => {
            names.insert(fn_decl.ident.sym.to_string());
        }
        Stmt::Decl(Decl::Class(class_decl)) => {
            names.insert(class_decl.ident.sym.to_string());
        }
        _ => {}
    }
}

fn collect_pat_names(pat: &Pat, names: &mut HashSet<String>) {
    match pat {
        Pat::Ident(binding) => {
            names.insert(binding.id.sym.to_string());
        }
        Pat::Array(arr) => {
            for pat in arr.elems.iter().flatten() {
                collect_pat_names(pat, names);
            }
        }
        Pat::Object(obj) => {
            for prop in &obj.props {
                match prop {
                    ObjectPatProp::KeyValue(kv) => {
                        collect_pat_names(&kv.value, names);
                    }
                    ObjectPatProp::Assign(assign) => {
                        names.insert(assign.key.sym.to_string());
                    }
                    ObjectPatProp::Rest(rest) => {
                        collect_pat_names(&rest.arg, names);
                    }
                }
            }
        }
        Pat::Assign(assign) => {
            collect_pat_names(&assign.left, names);
        }
        Pat::Rest(rest) => {
            collect_pat_names(&rest.arg, names);
        }
        Pat::Expr(_) | Pat::Invalid(_) => {}
    }
}

fn collect_stmt_idents(stmt: &Stmt) -> HashSet<String> {
    let mut collector = StmtIdentCollector::default();
    stmt.visit_with(&mut collector);
    collector.names
}

#[derive(Default)]
struct StmtIdentCollector {
    names: HashSet<String>,
}

impl Visit for StmtIdentCollector {
    fn visit_ident(&mut self, ident: &Ident) {
        self.names.insert(ident.sym.to_string());
    }

    fn visit_binding_ident(&mut self, ident: &BindingIdent) {
        self.names.insert(ident.id.sym.to_string());
    }
}
