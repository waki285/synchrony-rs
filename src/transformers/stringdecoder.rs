//! StringDecoder transformer
//!
//! Decodes obfuscated strings using string arrays and decoder functions.
//! Supports Simple, Base64, and RC4 decoding methods.
//!
//! Main components:
//! - `StringArrayFinder`: Detects string arrays (both variable and function forms)
//! - `DecoderFunctionFinder`: Detects decoder functions with offset/charset
//! - `VariableReferenceFinder`: Detects variable aliases to decoders
//! - `FunctionReferenceFinder`: Detects function wrappers around decoders
//! - `StringDecoderReplacer`: Replaces decoder calls with actual strings

use std::collections::{HashMap, HashSet};
use swc_common::GLOBALS;
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitMut, VisitMutWith, VisitWith};

use crate::context::{
    Context, DecoderFunction, DecoderFunctionType, DecoderReference, StringArray, StringArrayType,
};
use crate::error::Result;
use crate::scope::{ScopeData, analyze};
use crate::transformers::Transformer;

// Evaluate simple constant numeric expressions (used by rotations/offsets).
fn eval_const_i64(expr: &Expr) -> Option<i64> {
    match expr {
        Expr::Lit(Lit::Num(n)) => Some(n.value as i64),
        Expr::Lit(Lit::Str(s)) => {
            let raw = s.value.as_str()?;
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return None;
            }
            let mut sign = 1i64;
            let mut rest = trimmed;
            if let Some(first) = rest.chars().next() {
                if first == '-' {
                    sign = -1;
                    rest = &rest[1..];
                } else if first == '+' {
                    rest = &rest[1..];
                }
            }
            if rest.starts_with("0x") || rest.starts_with("0X") {
                i64::from_str_radix(&rest[2..], 16).ok().map(|v| v * sign)
            } else {
                rest.parse::<i64>().ok().map(|v| v * sign)
            }
        }
        Expr::Unary(unary) => {
            let val = eval_const_i64(&unary.arg)?;
            match unary.op {
                UnaryOp::Minus => Some(-val),
                UnaryOp::Plus => Some(val),
                UnaryOp::Tilde => Some(!val),
                _ => None,
            }
        }
        Expr::Bin(bin) => {
            let left = eval_const_i64(&bin.left)?;
            let right = eval_const_i64(&bin.right)?;
            match bin.op {
                BinaryOp::Add => Some(left + right),
                BinaryOp::Sub => Some(left - right),
                BinaryOp::Mul => Some(left * right),
                BinaryOp::Div => {
                    if right == 0 {
                        None
                    } else {
                        Some(left / right)
                    }
                }
                BinaryOp::Mod => {
                    if right == 0 {
                        None
                    } else {
                        Some(left % right)
                    }
                }
                BinaryOp::BitOr => Some(left | right),
                BinaryOp::BitAnd => Some(left & right),
                BinaryOp::BitXor => Some(left ^ right),
                BinaryOp::LShift => Some(left << (right as u32)),
                BinaryOp::RShift => Some(left >> (right as u32)),
                BinaryOp::ZeroFillRShift => {
                    let l = left as u64;
                    Some((l >> (right as u32)) as i64)
                }
                _ => None,
            }
        }
        Expr::Paren(paren) => eval_const_i64(&paren.expr),
        Expr::Seq(seq) => seq.exprs.last().and_then(|e| eval_const_i64(e)),
        _ => None,
    }
}

/// StringDecoder transformer.
///
/// Finds and decodes obfuscated string patterns into literal strings.
#[derive(Debug)]
pub struct StringDecoder;

impl StringDecoder {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Base64 decode with custom charset to bytes
    #[must_use]
    fn base64_decode_bytes(charset: &str, input: &str) -> Option<Vec<u8>> {
        let mut output = Vec::new();
        let mut buffer = 0u32;
        let mut bits_collected = 0u32;

        for ch in input.chars() {
            // Skip padding character '='
            if ch == '=' {
                continue;
            }

            let value = match charset.find(ch) {
                Some(v) => v as u32,
                None => {
                    // Be permissive with URL-safe base64 variants
                    if ch == '-' {
                        charset.find('+')? as u32
                    } else if ch == '_' {
                        charset.find('/')? as u32
                    } else {
                        // Ignore unknown characters (JS decoder effectively skips them)
                        continue;
                    }
                }
            };
            buffer = (buffer << 6) | value;
            bits_collected += 6;

            while bits_collected >= 8 {
                bits_collected -= 8;
                output.push(((buffer >> bits_collected) & 0xFF) as u8);
            }
        }

        Some(output)
    }

    /// Base64 decode with custom charset
    #[must_use]
    fn base64_decode(charset: &str, input: &str) -> Option<String> {
        let output = Self::base64_decode_bytes(charset, input)?;

        // Try to decode as UTF-8, falling back to lossy conversion
        match String::from_utf8(output) {
            Ok(s) => Some(s),
            Err(e) => Some(String::from_utf8_lossy(e.as_bytes()).into_owned()),
        }
    }

    /// RC4 decrypt
    #[must_use]
    fn rc4_decrypt(charset: &str, input: &str, key: &str) -> Option<String> {
        // First decode from base64
        let decoded = Self::base64_decode(charset, input)?;
        let input_units: Vec<u16> = decoded.encode_utf16().collect();
        let key_units: Vec<u16> = key.encode_utf16().collect();
        if key_units.is_empty() {
            return None;
        }

        // RC4 key scheduling
        let mut s: Vec<u8> = (0..=255).collect();
        let mut j = 0u8;

        for i in 0..256 {
            let key_unit = key_units[i % key_units.len()] & 0xFF;
            j = j.wrapping_add(s[i]).wrapping_add(key_unit as u8);
            s.swap(i, j as usize);
        }

        // RC4 decryption
        let mut i = 0u8;
        j = 0;
        let mut output: Vec<u16> = Vec::with_capacity(input_units.len());

        for unit in input_units {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[i as usize]);
            s.swap(i as usize, j as usize);
            let k = s[(s[i as usize].wrapping_add(s[j as usize])) as usize];
            output.push(unit ^ (k as u16));
        }

        Some(String::from_utf16_lossy(&output))
    }
}

impl Default for StringDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Transformer for StringDecoder {
    fn name(&self) -> &'static str {
        "StringDecoder"
    }

    fn transform(&self, context: &mut Context) -> Result<()> {
        // First pass: find string arrays
        let mut array_finder = StringArrayFinder::new();
        context.ast.visit_mut_with(&mut array_finder);

        if context.remove_garbage && !array_finder.arrays.is_empty() {
            let mut remover = StringArrayDeclarationRemover::new(&array_finder.arrays);
            context.ast.visit_mut_with(&mut remover);
        }

        // Store found arrays in context
        for (name, (array_type, strings)) in array_finder.arrays {
            crate::log_info!(
                "Found string array '{}' with {} strings",
                name,
                strings.len()
            );
            context.string_arrays.push(StringArray {
                identifier: name,
                array_type,
                strings,
            });
        }

        // Second pass: find decoder functions (needed for shift calculation)
        let mut decoder_finder = DecoderFunctionFinder::new(&context.string_arrays);
        context.ast.visit_mut_with(&mut decoder_finder);

        // Store found decoders in context
        for decoder in decoder_finder.decoders {
            crate::log_info!(
                "Found decoder function '{}' -> array '{}' (offset: {}, type: {:?})",
                decoder.identifier,
                decoder.string_array_identifier,
                decoder.offset,
                decoder.decoder_type
            );
            context.string_decoders.push(decoder);
        }

        // Third pass: find variable references
        let mut ref_finder = VariableReferenceFinder::new(
            &context.string_decoders,
            &context.string_decoder_references,
        );
        context.ast.visit_mut_with(&mut ref_finder);

        for reference in ref_finder.references {
            crate::log_debug!(
                "Found variable reference '{}' -> '{}'",
                reference.identifier,
                reference.real_identifier
            );
            context.string_decoder_references.push(reference);
        }

        // Fourth pass: find function references (wrapper functions around decoders).
        //
        // Obfuscators often build multiple wrapper layers (wrapper -> wrapper -> decoder).
        // We therefore run this pass to a fixpoint so newly discovered wrappers can be
        // used as inputs for the next iteration.
        let mut known: HashSet<String> = context
            .string_decoder_references
            .iter()
            .map(|r| r.identifier.clone())
            .collect();

        let mut rounds = 0usize;
        loop {
            rounds += 1;
            let mut fn_ref_finder = FunctionReferenceFinder::new(
                &context.string_decoders,
                &context.string_decoder_references,
            );
            context.ast.visit_mut_with(&mut fn_ref_finder);

            let mut added_any = false;
            for reference in fn_ref_finder.references {
                if known.insert(reference.identifier.clone()) {
                    crate::log_debug!(
                        "Found function reference '{}' -> '{}' (index_arg: {:?}, key_arg: {:?})",
                        reference.identifier,
                        reference.real_identifier,
                        reference.index_argument,
                        reference.key_argument
                    );
                    context.string_decoder_references.push(reference);
                    added_any = true;
                }
            }

            if !added_any || rounds >= 8 {
                break;
            }
        }

        // Fifth pass: find push/shift rotation patterns (IIFE that rotates array)
        // Now we have decoder info and references to properly calculate rotations
        let mut shift_finder = ShiftFinder::new(
            &context.string_arrays,
            &context.string_decoders,
            &context.string_decoder_references,
        );
        context.ast.visit_mut_with(&mut shift_finder);

        if !shift_finder.rotations.is_empty() {
            let mut rotator = StringArrayRotator::new(&shift_finder.rotations);
            context.ast.visit_mut_with(&mut rotator);

            let mut iife_remover = RotationIifeRemover::new(&shift_finder.rotations);
            context.ast.visit_mut_with(&mut iife_remover);

            // Apply rotations to string arrays in context
            for (array_name, rotation_count) in &shift_finder.rotations {
                if let Some(arr) = context
                    .string_arrays
                    .iter_mut()
                    .find(|a| a.identifier == *array_name)
                {
                    crate::log_info!(
                        "Rotating string array '{}' by {} positions",
                        array_name,
                        rotation_count
                    );
                    if !arr.strings.is_empty() {
                        let len = arr.strings.len();
                        arr.strings.rotate_left(rotation_count % len);
                    }
                }
            }
        }

        // Sixth pass: replace decoder calls
        if !context.string_arrays.is_empty() && !context.string_decoders.is_empty() {
            let mut replacer = StringDecoderReplacer::new(
                &context.string_arrays,
                &context.string_decoders,
                &context.string_decoder_references,
            );
            context.ast.visit_mut_with(&mut replacer);
        }

        if context.remove_garbage {
            let mut candidates: HashSet<String> = HashSet::new();
            for array in &context.string_arrays {
                candidates.insert(array.identifier.clone());
            }
            for decoder in &context.string_decoders {
                candidates.insert(decoder.identifier.clone());
            }
            for reference in &context.string_decoder_references {
                candidates.insert(reference.identifier.clone());
            }

            if !candidates.is_empty() {
                let mut scope_data = GLOBALS.set(&Default::default(), || analyze(&context.ast));
                if !scope_data.top.has_with_stmt && !scope_data.top.has_eval_call {
                    let mut alias_cleaner = ObfuscationAliasCleaner::new(&scope_data, &candidates);
                    context.ast.visit_mut_with(&mut alias_cleaner);

                    scope_data = GLOBALS.set(&Default::default(), || analyze(&context.ast));

                    for _ in 0..4 {
                        let mut remover = UnusedObfuscatedRemover::new(&scope_data);
                        context.ast.visit_mut_with(&mut remover);
                        if !remover.changed {
                            break;
                        }
                        scope_data = GLOBALS.set(&Default::default(), || analyze(&context.ast));
                    }

                    let mut candidate_functions: HashSet<String> = context
                        .string_arrays
                        .iter()
                        .filter(|array| array.array_type == StringArrayType::Function)
                        .map(|array| array.identifier.clone())
                        .collect();
                    for decoder in &context.string_decoders {
                        candidate_functions.insert(decoder.identifier.clone());
                    }

                    let external_function_uses = if candidate_functions.is_empty() {
                        HashSet::new()
                    } else {
                        let mut usage_finder = ExternalUsageFinder::new(&candidate_functions);
                        context.ast.visit_with(&mut usage_finder);
                        usage_finder.external_uses
                    };

                    let mut remover = ObfuscationGarbageRemover::new(
                        &scope_data,
                        &candidates,
                        &candidate_functions,
                        &external_function_uses,
                    );
                    context.ast.visit_mut_with(&mut remover);
                }
            }
        }

        Ok(())
    }
}

struct ObfuscationGarbageRemover<'a> {
    scope_data: &'a ScopeData,
    candidates: &'a HashSet<String>,
    candidate_functions: &'a HashSet<String>,
    external_function_uses: &'a HashSet<String>,
}

impl<'a> ObfuscationGarbageRemover<'a> {
    fn new(
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

struct ObfuscationAliasCleaner<'a> {
    scope_data: &'a ScopeData,
    candidates: &'a HashSet<String>,
}

impl<'a> ObfuscationAliasCleaner<'a> {
    fn new(scope_data: &'a ScopeData, candidates: &'a HashSet<String>) -> Self {
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

struct UnusedObfuscatedRemover<'a> {
    scope_data: &'a ScopeData,
    changed: bool,
}

impl<'a> UnusedObfuscatedRemover<'a> {
    fn new(scope_data: &'a ScopeData) -> Self {
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
        items
            .iter_mut()
            .for_each(|item| item.visit_mut_children_with(self));

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
        stmts
            .iter_mut()
            .for_each(|stmt| stmt.visit_mut_children_with(self));

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
        Expr::Ident(_) => true,
        Expr::Lit(_) => true,
        Expr::Fn(_) | Expr::Arrow(_) => true,
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
            _ => false,
        }),
        _ => false,
    }
}

struct ExternalUsageFinder<'a> {
    targets: &'a HashSet<String>,
    fn_stack: Vec<String>,
    external_uses: HashSet<String>,
}

impl<'a> ExternalUsageFinder<'a> {
    fn new(targets: &'a HashSet<String>) -> Self {
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
            .map(|name| name == ident.sym.as_ref())
            .unwrap_or(false);
        if !in_own_fn {
            self.external_uses.insert(ident.sym.to_string());
        }
    }
}

impl VisitMut for ObfuscationGarbageRemover<'_> {
    fn visit_mut_module_items(&mut self, items: &mut Vec<ModuleItem>) {
        items
            .iter_mut()
            .for_each(|item| item.visit_mut_children_with(self));

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
        stmts
            .iter_mut()
            .for_each(|stmt| stmt.visit_mut_children_with(self));

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

/// Push/Shift finder - detects array rotation patterns in IIFE
///
/// Detects patterns like:
/// ```js
/// (function(arr, expectedValue) {
///     var calc = function(idx) { return decoder(idx); };
///     while (true) {
///         try {
///             var result = parseInt(calc(0)) * 2 + parseInt(calc(1));
///             if (result === expectedValue) break;
///             arr.push(arr.shift());
///         } catch(e) {
///             arr.push(arr.shift());
///         }
///     }
/// })(stringArray, 123456);
/// ```
struct ShiftFinder<'a> {
    rotations: Vec<(String, usize)>,
    string_arrays: &'a [StringArray],
    string_decoders: &'a [DecoderFunction],
    decoder_references: &'a [DecoderReference],
}

/// Rotates string array literals in the AST to match detected rotations
struct StringArrayRotator {
    rotations: HashMap<String, usize>,
}

impl StringArrayRotator {
    #[must_use]
    fn new(rotations: &[(String, usize)]) -> Self {
        let mut map = HashMap::new();
        for (name, count) in rotations {
            map.insert(name.clone(), *count);
        }
        Self { rotations: map }
    }

    fn rotate_array_lit(arr: &mut ArrayLit, count: usize) {
        if arr.elems.is_empty() {
            return;
        }
        let shift = count % arr.elems.len();
        if shift == 0 {
            return;
        }
        arr.elems.rotate_left(shift);
    }
}

impl VisitMut for StringArrayRotator {
    fn visit_mut_var_decl(&mut self, decl: &mut VarDecl) {
        for declarator in &mut decl.decls {
            if let Pat::Ident(binding) = &declarator.name
                && let Some(init) = &mut declarator.init
                && let Expr::Array(arr) = &mut **init
                && let Some(count) = self.rotations.get(binding.id.sym.as_ref())
            {
                Self::rotate_array_lit(arr, *count);
            }
        }
        decl.visit_mut_children_with(self);
    }

    fn visit_mut_fn_decl(&mut self, func: &mut FnDecl) {
        let fn_name = func.ident.sym.to_string();
        if let Some(count) = self.rotations.get(&fn_name).copied()
            && let Some(body) = &mut func.function.body
        {
            for stmt in &mut body.stmts {
                if let Stmt::Decl(Decl::Var(var_decl)) = stmt {
                    for declarator in &mut var_decl.decls {
                        if let Some(init) = &mut declarator.init
                            && let Expr::Array(arr) = &mut **init
                        {
                            Self::rotate_array_lit(arr, count);
                            break;
                        }
                    }
                    break;
                }
            }
        }
        func.visit_mut_children_with(self);
    }
}

/// Removes rotation IIFEs once the array has been rotated in the AST
struct RotationIifeRemover {
    rotated_names: HashMap<String, ()>,
}

impl RotationIifeRemover {
    #[must_use]
    fn new(rotations: &[(String, usize)]) -> Self {
        let mut map = HashMap::new();
        for (name, _) in rotations {
            map.insert(name.clone(), ());
        }
        Self { rotated_names: map }
    }

    fn is_rotation_iife(&self, stmt: &Stmt) -> bool {
        let Stmt::Expr(expr_stmt) = stmt else {
            return false;
        };
        let Some(call) = Self::extract_call_expr(&expr_stmt.expr) else {
            return false;
        };
        if call.args.len() < 2 {
            return false;
        }
        let Some(array_name) = Self::extract_array_name(call) else {
            return false;
        };
        if !self.rotated_names.contains_key(&array_name) {
            return false;
        }
        let Callee::Expr(callee) = &call.callee else {
            return false;
        };
        Self::extract_fn_expr(callee).is_some()
    }

    fn extract_call_expr(expr: &Expr) -> Option<&CallExpr> {
        match expr {
            Expr::Call(call) => Some(call),
            Expr::Paren(paren) => Self::extract_call_expr(&paren.expr),
            Expr::Seq(seq) => seq.exprs.last().and_then(|e| Self::extract_call_expr(e)),
            _ => None,
        }
    }

    fn extract_fn_expr(expr: &Expr) -> Option<&FnExpr> {
        match expr {
            Expr::Fn(fn_expr) => Some(fn_expr),
            Expr::Paren(paren) => Self::extract_fn_expr(&paren.expr),
            Expr::Seq(seq) => seq.exprs.last().and_then(|e| Self::extract_fn_expr(e)),
            _ => None,
        }
    }

    fn extract_array_name(call: &CallExpr) -> Option<String> {
        if let Some(first) = call.args.first()
            && let Expr::Ident(ident) = &*first.expr
        {
            return Some(ident.sym.to_string());
        }
        None
    }
}

impl VisitMut for RotationIifeRemover {
    fn visit_mut_script(&mut self, script: &mut Script) {
        script.body.retain(|stmt| !self.is_rotation_iife(stmt));
        script.visit_mut_children_with(self);
    }

    fn visit_mut_module(&mut self, module: &mut Module) {
        module.body.retain(|item| {
            if let ModuleItem::Stmt(stmt) = item {
                !self.is_rotation_iife(stmt)
            } else {
                true
            }
        });
        module.visit_mut_children_with(self);
    }

    fn visit_mut_block_stmt(&mut self, block: &mut BlockStmt) {
        block.stmts.retain(|stmt| !self.is_rotation_iife(stmt));
        block.visit_mut_children_with(self);
    }
}

impl<'a> ShiftFinder<'a> {
    #[must_use]
    const fn new(
        string_arrays: &'a [StringArray],
        string_decoders: &'a [DecoderFunction],
        decoder_references: &'a [DecoderReference],
    ) -> Self {
        Self {
            rotations: Vec::new(),
            string_arrays,
            string_decoders,
            decoder_references,
        }
    }

    /// Check if an expression is a push(shift()) pattern on given array
    #[must_use]
    fn is_push_shift_pattern(expr: &Expr) -> bool {
        fn prop_is_name(prop: &MemberProp, name: &str) -> bool {
            match prop {
                MemberProp::Ident(ident) => ident.sym == name,
                MemberProp::Computed(comp) => {
                    if let Expr::Lit(Lit::Str(s)) = &*comp.expr {
                        s.value.as_str() == Some(name)
                    } else {
                        false
                    }
                }
                _ => false,
            }
        }

        // obj.push(obj.shift())
        let Expr::Call(call) = expr else {
            return false;
        };
        let Callee::Expr(callee) = &call.callee else {
            return false;
        };
        let Expr::Member(member) = &**callee else {
            return false;
        };
        let Expr::Ident(obj) = &*member.obj else {
            return false;
        };
        if !prop_is_name(&member.prop, "push") {
            return false;
        }
        if call.args.len() != 1 {
            return false;
        }
        let Expr::Call(shift_call) = &*call.args[0].expr else {
            return false;
        };
        let Callee::Expr(shift_callee) = &shift_call.callee else {
            return false;
        };
        let Expr::Member(shift_member) = &**shift_callee else {
            return false;
        };
        let Expr::Ident(shift_obj) = &*shift_member.obj else {
            return false;
        };
        if shift_obj.sym != obj.sym {
            return false;
        }
        prop_is_name(&shift_member.prop, "shift")
    }

    /// Check for push/shift in try-catch body
    #[must_use]
    fn find_push_shift_in_try(try_stmt: &TryStmt) -> bool {
        if Self::contains_push_shift_in_stmts(&try_stmt.block.stmts) {
            return true;
        }
        if let Some(catch) = &try_stmt.handler
            && Self::contains_push_shift_in_stmts(&catch.body.stmts)
        {
            return true;
        }
        false
    }

    fn contains_push_shift_in_stmts(stmts: &[Stmt]) -> bool {
        for stmt in stmts {
            if Self::contains_push_shift_in_stmt(stmt) {
                return true;
            }
        }
        false
    }

    fn contains_push_shift_in_stmt(stmt: &Stmt) -> bool {
        match stmt {
            Stmt::Expr(expr_stmt) => Self::is_push_shift_pattern(&expr_stmt.expr),
            Stmt::Block(block) => Self::contains_push_shift_in_stmts(&block.stmts),
            Stmt::If(if_stmt) => {
                if Self::contains_push_shift_in_stmt(&if_stmt.cons) {
                    return true;
                }
                if let Some(alt) = &if_stmt.alt {
                    return Self::contains_push_shift_in_stmt(alt);
                }
                false
            }
            Stmt::While(while_stmt) => Self::contains_push_shift_in_stmt(&while_stmt.body),
            Stmt::For(for_stmt) => Self::contains_push_shift_in_stmt(&for_stmt.body),
            Stmt::ForIn(for_in) => Self::contains_push_shift_in_stmt(&for_in.body),
            Stmt::ForOf(for_of) => Self::contains_push_shift_in_stmt(&for_of.body),
            Stmt::Try(try_stmt) => Self::find_push_shift_in_try(try_stmt),
            _ => false,
        }
    }

    /// Extract call expression from nested parentheses / sequence expressions
    #[must_use]
    fn extract_call_expr(expr: &Expr) -> Option<&CallExpr> {
        match expr {
            Expr::Call(call) => Some(call),
            Expr::Paren(paren) => Self::extract_call_expr(&paren.expr),
            Expr::Seq(seq) => seq.exprs.last().and_then(|e| Self::extract_call_expr(e)),
            _ => None,
        }
    }

    /// Extract function expression from nested parentheses / sequence expressions
    #[must_use]
    fn extract_fn_expr(expr: &Expr) -> Option<&FnExpr> {
        match expr {
            Expr::Fn(fn_expr) => Some(fn_expr),
            Expr::Paren(paren) => Self::extract_fn_expr(&paren.expr),
            Expr::Seq(seq) => seq.exprs.last().and_then(|e| Self::extract_fn_expr(e)),
            _ => None,
        }
    }

    /// Extract break condition number from IIFE call
    #[must_use]
    fn extract_break_condition(call: &CallExpr) -> Option<i64> {
        if call.args.len() >= 2 {
            return eval_const_i64(&call.args[1].expr);
        }
        None
    }

    /// Extract array name from IIFE arguments
    #[must_use]
    fn extract_array_name(call: &CallExpr) -> Option<String> {
        if !call.args.is_empty()
            && let Expr::Ident(ident) = &*call.args[0].expr
        {
            return Some(ident.sym.to_string());
        }
        None
    }

    /// Extract the binary expression (parseInt chain) from try block
    #[must_use]
    fn extract_parse_int_chain(try_stmt: &TryStmt) -> Option<Box<Expr>> {
        for stmt in &try_stmt.block.stmts {
            match stmt {
                // var result = parseInt(...) + ...
                Stmt::Decl(Decl::Var(var_decl)) => {
                    for decl in &var_decl.decls {
                        if let Some(init) = &decl.init
                            && Self::contains_parse_int(init)
                        {
                            return Some(init.clone());
                        }
                    }
                }
                // if (parseInt(...) === breakCond) break;
                Stmt::If(if_stmt) => {
                    if let Expr::Bin(bin) = &*if_stmt.test
                        && matches!(bin.op, BinaryOp::EqEq | BinaryOp::EqEqEq)
                    {
                        // Check which side has the parseInt chain
                        if Self::contains_parse_int(&bin.left) {
                            return Some(bin.left.clone());
                        }
                        if Self::contains_parse_int(&bin.right) {
                            return Some(bin.right.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Check if expression contains parseInt call
    #[must_use]
    fn contains_parse_int(expr: &Expr) -> bool {
        match expr {
            Expr::Call(call) => {
                if let Callee::Expr(callee) = &call.callee
                    && let Expr::Ident(ident) = &**callee
                    && ident.sym == "parseInt"
                {
                    return true;
                }
                // Check arguments
                for arg in &call.args {
                    if Self::contains_parse_int(&arg.expr) {
                        return true;
                    }
                }
                false
            }
            Expr::Bin(bin) => {
                Self::contains_parse_int(&bin.left) || Self::contains_parse_int(&bin.right)
            }
            Expr::Unary(unary) => Self::contains_parse_int(&unary.arg),
            Expr::Paren(paren) => Self::contains_parse_int(&paren.expr),
            _ => false,
        }
    }

    /// Calculate the rotation count by simulating the push/shift loop
    #[must_use]
    fn calc_shift(
        &self,
        break_condition: i64,
        string_array_name: &str,
        parse_int_chain: &Expr,
    ) -> Option<usize> {
        // Find the string array
        let string_array = self
            .string_arrays
            .iter()
            .find(|a| a.identifier == string_array_name)?;

        // Make a mutable copy of the strings
        let mut strings = string_array.strings.clone();
        let max_iterations = strings.len() * 2;

        let target = break_condition as f64;
        for iteration in 0..max_iterations {
            // Try to evaluate the parseInt chain with current array order
            if let Some(result) = self.evaluate_parse_int_chain(parse_int_chain, &strings)
                && (result - target).abs() < 1e-6
            {
                return Some(iteration);
            }

            // Rotate: push(shift())
            if !strings.is_empty() {
                let first = strings.remove(0);
                strings.push(first);
            }
        }

        None
    }

    /// Evaluate a parseInt chain expression
    #[must_use]
    fn evaluate_parse_int_chain(&self, expr: &Expr, rotated_strings: &[String]) -> Option<f64> {
        match expr {
            Expr::Lit(Lit::Num(n)) => Some(n.value),

            Expr::Unary(unary) if unary.op == UnaryOp::Minus => self
                .evaluate_parse_int_chain(&unary.arg, rotated_strings)
                .map(|n| -n),

            Expr::Bin(bin) => {
                let left = self.evaluate_parse_int_chain(&bin.left, rotated_strings)?;
                let right = self.evaluate_parse_int_chain(&bin.right, rotated_strings)?;

                Some(match bin.op {
                    BinaryOp::Add => left + right,
                    BinaryOp::Sub => left - right,
                    BinaryOp::Mul => left * right,
                    BinaryOp::Div => {
                        if right == 0.0 {
                            return None;
                        }
                        left / right
                    }
                    BinaryOp::Mod => {
                        if right == 0.0 {
                            return None;
                        }
                        left % right
                    }
                    BinaryOp::BitOr => ((left as i64) | (right as i64)) as f64,
                    BinaryOp::BitAnd => ((left as i64) & (right as i64)) as f64,
                    BinaryOp::BitXor => ((left as i64) ^ (right as i64)) as f64,
                    _ => return None,
                })
            }

            Expr::Paren(paren) => self.evaluate_parse_int_chain(&paren.expr, rotated_strings),

            // parseInt(decoderFunc(idx))
            Expr::Call(call) => {
                if let Callee::Expr(callee) = &call.callee
                    && let Expr::Ident(ident) = &**callee
                    && ident.sym == "parseInt"
                    && !call.args.is_empty()
                {
                    // Get the decoded string and parse as int
                    let decoded =
                        self.evaluate_decoder_call(&call.args[0].expr, rotated_strings)?;
                    return Self::parse_int_like(&decoded).map(|v| v as f64);
                }
                None
            }

            _ => None,
        }
    }

    /// Evaluate a decoder function call
    #[must_use]
    fn evaluate_decoder_call(&self, expr: &Expr, rotated_strings: &[String]) -> Option<String> {
        if let Expr::Call(call) = expr {
            if let Callee::Expr(callee) = &call.callee {
                if let Expr::Ident(func_ident) = &**callee {
                    let func_name = func_ident.sym.to_string();

                    let (decoder, extra_offset, index_argument, key_argument) =
                        self.resolve_decoder(&func_name)?;

                    // Get the index argument
                    if call.args.len() <= index_argument {
                        return None;
                    }

                    let index = self.get_numeric_arg(&call.args[index_argument].expr)?;
                    let final_index = (index + decoder.offset + extra_offset) as usize;

                    // Get from rotated strings
                    if final_index < rotated_strings.len() {
                        let encoded = &rotated_strings[final_index];

                        // Decode based on type
                        match decoder.decoder_type {
                            DecoderFunctionType::Simple => Some(encoded.clone()),
                            DecoderFunctionType::Base64 => {
                                let charset = decoder.charset.as_deref()
                                    .unwrap_or("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=");
                                StringDecoder::base64_decode(charset, encoded)
                            }
                            DecoderFunctionType::Rc4 => {
                                // RC4 needs a key from the second argument
                                if call.args.len() > key_argument
                                    && let Expr::Lit(Lit::Str(key_str)) =
                                        &*call.args[key_argument].expr
                                    && let Some(key) = key_str.value.as_str()
                                {
                                    let charset = decoder.charset.as_deref()
                                                .unwrap_or("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=");
                                    return StringDecoder::rc4_decrypt(charset, encoded, key);
                                }
                                None
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    }

    #[must_use]
    fn parse_int_like(input: &str) -> Option<i64> {
        let trimmed = input.trim_start();
        if trimmed.is_empty() {
            return None;
        }

        let mut chars = trimmed.chars().peekable();
        let mut sign = 1i64;

        if let Some(&c) = chars.peek() {
            if c == '-' {
                sign = -1;
                chars.next();
            } else if c == '+' {
                chars.next();
            }
        }

        let mut rest: String = chars.collect();
        let mut radix = 10u32;

        if rest.starts_with("0x") || rest.starts_with("0X") {
            radix = 16;
            rest = rest[2..].to_string();
        }

        let digits: String = rest
            .chars()
            .take_while(|c| {
                if radix == 16 {
                    c.is_ascii_hexdigit()
                } else {
                    c.is_ascii_digit()
                }
            })
            .collect();

        if digits.is_empty() {
            return None;
        }

        i64::from_str_radix(&digits, radix).ok().map(|v| v * sign)
    }

    #[must_use]
    fn resolve_decoder(&self, name: &str) -> Option<(&DecoderFunction, i32, usize, usize)> {
        if let Some(decoder) = self.string_decoders.iter().find(|d| d.identifier == name) {
            return Some((decoder, 0, decoder.index_argument, decoder.key_argument));
        }

        let mut current_name = name.to_string();
        let mut total_offset = 0i32;
        let mut index_argument = None;
        let mut key_argument = None;
        let mut visited = Vec::new();

        loop {
            if visited.contains(&current_name) {
                return None;
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

    /// Extract numeric value from expression
    #[must_use]
    fn get_numeric_arg(&self, expr: &Expr) -> Option<i32> {
        eval_const_i64(expr).map(|v| v as i32)
    }
}

impl VisitMut for ShiftFinder<'_> {
    fn visit_mut_expr_stmt(&mut self, stmt: &mut ExprStmt) {
        stmt.visit_mut_children_with(self);

        // Look for IIFE: (function(arr, breakCond) { ... })(stringArray, 123456)
        if let Some(call) = Self::extract_call_expr(&stmt.expr)
            && let Callee::Expr(callee) = &call.callee
            && let Some(fn_expr) = Self::extract_fn_expr(callee)
            && let Some(body) = &fn_expr.function.body
        {
            // Get array name and break condition from arguments
            let Some(array_name) = Self::extract_array_name(call) else {
                return;
            };
            let Some(break_condition) = Self::extract_break_condition(call) else {
                return;
            };

            // Look for while(true) or for(;;) loop with try-catch containing push/shift
            for body_stmt in &body.stmts {
                let loop_body = match body_stmt {
                    Stmt::While(while_stmt) => Some(&while_stmt.body),
                    Stmt::For(for_stmt) => Some(&for_stmt.body),
                    _ => None,
                };

                if let Some(loop_body) = loop_body
                    && let Stmt::Block(block) = &**loop_body
                {
                    for inner_stmt in &block.stmts {
                        if let Stmt::Try(try_stmt) = inner_stmt
                            && Self::find_push_shift_in_try(try_stmt)
                        {
                            // Found a push/shift rotation pattern
                            // Extract the parseInt chain and calculate rotations
                            if let Some(parse_int_chain) = Self::extract_parse_int_chain(try_stmt) {
                                if let Some(rotation_count) =
                                    self.calc_shift(break_condition, &array_name, &parse_int_chain)
                                {
                                    crate::log_debug!(
                                        "Found push/shift IIFE for array '{}': {} rotations needed",
                                        array_name,
                                        rotation_count
                                    );
                                    self.rotations.push((array_name.clone(), rotation_count));
                                } else {
                                    crate::log_debug!(
                                        "Found push/shift IIFE for array '{}' but couldn't calculate rotations",
                                        array_name
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// First pass: find string arrays
///
/// Detects two forms:
/// 1. Variable form: `var _0x1234 = ["str1", "str2", ...]`
/// 2. Function form: `function _0x1234() { var arr = [...]; _0x1234 = function() { return arr; }; return _0x1234(); }`
struct StringArrayFinder {
    arrays: HashMap<String, (StringArrayType, Vec<String>)>,
}

impl StringArrayFinder {
    #[must_use]
    fn new() -> Self {
        Self {
            arrays: HashMap::new(),
        }
    }

    #[must_use]
    fn extract_string_array(arr: &ArrayLit) -> Option<Vec<String>> {
        let mut strings = Vec::new();

        for elem in &arr.elems {
            match elem {
                Some(ExprOrSpread { expr, spread: None }) => {
                    if let Expr::Lit(Lit::Str(s)) = &**expr {
                        if let Some(v) = s.value.as_str() {
                            strings.push(v.to_string());
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

            // First statement: var declaration with array
            let array_strings = match &stmts[0] {
                Stmt::Decl(Decl::Var(var_decl)) if var_decl.decls.len() == 1 => {
                    let decl = &var_decl.decls[0];
                    if let Some(init) = &decl.init {
                        if let Expr::Array(arr) = &**init {
                            Self::extract_string_array(arr)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                _ => None,
            };

            let Some(strings) = array_strings else {
                return;
            };

            // Second statement: assignment expression _0x1234 = function() { return arr; }
            let is_valid_assignment = match &stmts[1] {
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
            let is_return = matches!(&stmts[2], Stmt::Return(_));

            if is_return {
                self.arrays
                    .insert(fn_name, (StringArrayType::Function, strings));
            }
        }
    }
}

struct StringArrayDeclarationRemover {
    variable_arrays: HashSet<String>,
    function_arrays: HashSet<String>,
}

impl StringArrayDeclarationRemover {
    fn new(arrays: &HashMap<String, (StringArrayType, Vec<String>)>) -> Self {
        let mut variable_arrays = HashSet::new();
        let mut function_arrays = HashSet::new();

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
        items
            .iter_mut()
            .for_each(|item| item.visit_mut_children_with(self));

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
        stmts
            .iter_mut()
            .for_each(|stmt| stmt.visit_mut_children_with(self));

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
            !self.variable_arrays.contains(binding.id.sym.as_ref())
        });
    }
}

/// Second pass: find decoder functions
///
/// Detects decoder functions that:
/// 1. Call a string array function
/// 2. Have an offset calculation
/// 3. Optionally have Base64/RC4 charset
struct DecoderFunctionFinder<'a> {
    string_arrays: &'a [StringArray],
    decoders: Vec<DecoderFunction>,
}

impl<'a> DecoderFunctionFinder<'a> {
    #[must_use]
    fn new(string_arrays: &'a [StringArray]) -> Self {
        Self {
            string_arrays,
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
    fn assign_left_name(assign: &AssignExpr) -> Option<&str> {
        assign
            .left
            .as_ident()
            .map(|binding| binding.id.sym.as_ref())
    }

    /// Extract offset from binary expression like `idx - 123` or `idx + 456`
    #[must_use]
    fn extract_offset(expr: &Expr) -> Option<i32> {
        if let Expr::Bin(bin) = expr {
            // Check right side for number
            let num = eval_const_i64(&bin.right)? as i32;

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
            AssignOp::AddAssign => eval_const_i64(&assign.right).map(|v| v as i32),
            AssignOp::SubAssign => eval_const_i64(&assign.right).map(|v| -(v as i32)),
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
            Stmt::Return(ret) => ret
                .arg
                .as_ref()
                .and_then(|arg| Self::find_offset_in_expr(arg)),
            Stmt::Block(block) => Self::find_offset_in_stmts(&block.stmts),
            Stmt::If(if_stmt) => Self::find_offset_in_stmt(&if_stmt.cons).or_else(|| {
                if_stmt
                    .alt
                    .as_ref()
                    .and_then(|alt| Self::find_offset_in_stmt(alt))
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
            Stmt::Return(ret) => ret
                .arg
                .as_ref()
                .and_then(|arg| Self::find_offset_in_self_expr(arg, fn_name)),
            Stmt::Block(block) => Self::find_offset_in_self_assignment(&block.stmts, fn_name),
            Stmt::If(if_stmt) => {
                Self::find_offset_in_self_stmt(&if_stmt.cons, fn_name).or_else(|| {
                    if_stmt
                        .alt
                        .as_ref()
                        .and_then(|alt| Self::find_offset_in_self_stmt(alt, fn_name))
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
                        return Some(v.to_string());
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
                Self::scan_decoder_helpers_in_expr(&unary.arg, charset, rc4_found)
            }
            Expr::Paren(paren) => {
                Self::scan_decoder_helpers_in_expr(&paren.expr, charset, rc4_found)
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
                    .map(|init| Self::expr_contains_member_prop(init, prop))
                    .unwrap_or(false)
            }),
            Stmt::Decl(Decl::Fn(fn_decl)) => fn_decl
                .function
                .body
                .as_ref()
                .map(|body| {
                    body.stmts
                        .iter()
                        .any(|s| Self::stmt_contains_member_prop(s, prop))
                })
                .unwrap_or(false),
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
                        .map(|alt| Self::stmt_contains_member_prop(alt, prop))
                        .unwrap_or(false)
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
                            .map(|init| Self::expr_contains_member_prop(init, prop))
                            .unwrap_or(false)
                    }),
                    Some(VarDeclOrExpr::Expr(expr)) => Self::expr_contains_member_prop(expr, prop),
                    None => false,
                };
                init_has
                    || for_stmt
                        .test
                        .as_ref()
                        .map(|test| Self::expr_contains_member_prop(test, prop))
                        .unwrap_or(false)
                    || for_stmt
                        .update
                        .as_ref()
                        .map(|update| Self::expr_contains_member_prop(update, prop))
                        .unwrap_or(false)
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
                .map(|arg| Self::expr_contains_member_prop(arg, prop))
                .unwrap_or(false),
            Stmt::Try(try_stmt) => {
                try_stmt
                    .block
                    .stmts
                    .iter()
                    .any(|s| Self::stmt_contains_member_prop(s, prop))
                    || try_stmt
                        .handler
                        .as_ref()
                        .map(|handler| {
                            handler
                                .body
                                .stmts
                                .iter()
                                .any(|s| Self::stmt_contains_member_prop(s, prop))
                        })
                        .unwrap_or(false)
                    || try_stmt
                        .finalizer
                        .as_ref()
                        .map(|finalizer| {
                            finalizer
                                .stmts
                                .iter()
                                .any(|s| Self::stmt_contains_member_prop(s, prop))
                        })
                        .unwrap_or(false)
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
                    _ => false,
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
                    Prop::Method(method) => method
                        .function
                        .body
                        .as_ref()
                        .map(|body| {
                            body.stmts
                                .iter()
                                .any(|s| Self::stmt_contains_member_prop(s, prop_name))
                        })
                        .unwrap_or(false),
                    Prop::Getter(getter) => getter
                        .body
                        .as_ref()
                        .map(|body| {
                            body.stmts
                                .iter()
                                .any(|s| Self::stmt_contains_member_prop(s, prop_name))
                        })
                        .unwrap_or(false),
                    Prop::Setter(setter) => setter
                        .body
                        .as_ref()
                        .map(|body| {
                            body.stmts
                                .iter()
                                .any(|s| Self::stmt_contains_member_prop(s, prop_name))
                        })
                        .unwrap_or(false),
                    _ => false,
                },
                _ => false,
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
                    .map(|init| Self::expr_contains_bitxor(init))
                    .unwrap_or(false)
            }),
            Stmt::Decl(Decl::Fn(fn_decl)) => fn_decl
                .function
                .body
                .as_ref()
                .map(|body| body.stmts.iter().any(Self::stmt_contains_bitxor))
                .unwrap_or(false),
            Stmt::Block(block) => block.stmts.iter().any(Self::stmt_contains_bitxor),
            Stmt::If(if_stmt) => {
                Self::expr_contains_bitxor(&if_stmt.test)
                    || Self::stmt_contains_bitxor(&if_stmt.cons)
                    || if_stmt
                        .alt
                        .as_ref()
                        .map(|alt| Self::stmt_contains_bitxor(alt))
                        .unwrap_or(false)
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
                            .map(|init| Self::expr_contains_bitxor(init))
                            .unwrap_or(false)
                    }),
                    Some(VarDeclOrExpr::Expr(expr)) => Self::expr_contains_bitxor(expr),
                    None => false,
                };
                init_has
                    || for_stmt
                        .test
                        .as_ref()
                        .map(|test| Self::expr_contains_bitxor(test))
                        .unwrap_or(false)
                    || for_stmt
                        .update
                        .as_ref()
                        .map(|update| Self::expr_contains_bitxor(update))
                        .unwrap_or(false)
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
                .map(|arg| Self::expr_contains_bitxor(arg))
                .unwrap_or(false),
            Stmt::Try(try_stmt) => {
                try_stmt.block.stmts.iter().any(Self::stmt_contains_bitxor)
                    || try_stmt
                        .handler
                        .as_ref()
                        .map(|handler| handler.body.stmts.iter().any(Self::stmt_contains_bitxor))
                        .unwrap_or(false)
                    || try_stmt
                        .finalizer
                        .as_ref()
                        .map(|finalizer| finalizer.stmts.iter().any(Self::stmt_contains_bitxor))
                        .unwrap_or(false)
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
                        AssignTarget::Simple(SimpleAssignTarget::Ident(_)) => false,
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
                        .map(|body| body.stmts.iter().any(Self::stmt_contains_bitxor))
                        .unwrap_or(false),
                    Prop::Getter(getter) => getter
                        .body
                        .as_ref()
                        .map(|body| body.stmts.iter().any(Self::stmt_contains_bitxor))
                        .unwrap_or(false),
                    Prop::Setter(setter) => setter
                        .body
                        .as_ref()
                        .map(|body| body.stmts.iter().any(Self::stmt_contains_bitxor))
                        .unwrap_or(false),
                    _ => false,
                },
                _ => false,
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
                    .map(|init| Self::expr_contains_number(init, value))
                    .unwrap_or(false)
            }),
            Stmt::Decl(Decl::Fn(fn_decl)) => fn_decl
                .function
                .body
                .as_ref()
                .map(|body| {
                    body.stmts
                        .iter()
                        .any(|s| Self::stmt_contains_number(s, value))
                })
                .unwrap_or(false),
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
                        .map(|alt| Self::stmt_contains_number(alt, value))
                        .unwrap_or(false)
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
                            .map(|init| Self::expr_contains_number(init, value))
                            .unwrap_or(false)
                    }),
                    Some(VarDeclOrExpr::Expr(expr)) => Self::expr_contains_number(expr, value),
                    None => false,
                };
                init_has
                    || for_stmt
                        .test
                        .as_ref()
                        .map(|test| Self::expr_contains_number(test, value))
                        .unwrap_or(false)
                    || for_stmt
                        .update
                        .as_ref()
                        .map(|update| Self::expr_contains_number(update, value))
                        .unwrap_or(false)
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
                .map(|arg| Self::expr_contains_number(arg, value))
                .unwrap_or(false),
            Stmt::Try(try_stmt) => {
                try_stmt
                    .block
                    .stmts
                    .iter()
                    .any(|s| Self::stmt_contains_number(s, value))
                    || try_stmt
                        .handler
                        .as_ref()
                        .map(|handler| {
                            handler
                                .body
                                .stmts
                                .iter()
                                .any(|s| Self::stmt_contains_number(s, value))
                        })
                        .unwrap_or(false)
                    || try_stmt
                        .finalizer
                        .as_ref()
                        .map(|finalizer| {
                            finalizer
                                .stmts
                                .iter()
                                .any(|s| Self::stmt_contains_number(s, value))
                        })
                        .unwrap_or(false)
            }
            _ => false,
        }
    }

    #[must_use]
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
                    Prop::Method(method) => method
                        .function
                        .body
                        .as_ref()
                        .map(|body| {
                            body.stmts
                                .iter()
                                .any(|s| Self::stmt_contains_number(s, value))
                        })
                        .unwrap_or(false),
                    Prop::Getter(getter) => getter
                        .body
                        .as_ref()
                        .map(|body| {
                            body.stmts
                                .iter()
                                .any(|s| Self::stmt_contains_number(s, value))
                        })
                        .unwrap_or(false),
                    Prop::Setter(setter) => setter
                        .body
                        .as_ref()
                        .map(|body| {
                            body.stmts
                                .iter()
                                .any(|s| Self::stmt_contains_number(s, value))
                        })
                        .unwrap_or(false),
                    _ => false,
                },
                _ => false,
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
        let mut offset = 0i32;
        let mut offset_found = false;
        let mut decoder_type = DecoderFunctionType::Simple;
        let mut charset = None;
        let mut rc4_found = false;

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

        // If we found a string array reference, create decoder
        if let Some(str_array_id) = string_array_identifier {
            self.decoders.push(DecoderFunction {
                identifier: fn_name,
                string_array_identifier: str_array_id,
                decoder_type,
                offset,
                index_argument: 0,
                key_argument: 1,
                charset,
            });
        }
    }
}

/// Third pass: find variable references to decoders
///
/// Detects: `var alias = _0xDecoder;`
struct VariableReferenceFinder<'a> {
    decoders: &'a [DecoderFunction],
    existing_refs: &'a [DecoderReference],
    references: Vec<DecoderReference>,
}

impl<'a> VariableReferenceFinder<'a> {
    #[must_use]
    const fn new(decoders: &'a [DecoderFunction], existing_refs: &'a [DecoderReference]) -> Self {
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
struct FunctionReferenceFinder<'a> {
    decoders: &'a [DecoderFunction],
    existing_refs: &'a [DecoderReference],
    references: Vec<DecoderReference>,
}

impl<'a> FunctionReferenceFinder<'a> {
    #[must_use]
    const fn new(decoders: &'a [DecoderFunction], existing_refs: &'a [DecoderReference]) -> Self {
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
                        identifier: fn_name.clone(),
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

/// Sixth pass: replace decoder calls
struct StringDecoderReplacer<'a> {
    string_arrays: &'a [StringArray],
    string_decoders: &'a [DecoderFunction],
    decoder_references: &'a [DecoderReference],
}

impl<'a> StringDecoderReplacer<'a> {
    #[must_use]
    const fn new(
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
fn parse_index_str(value: &str) -> Option<i32> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (neg, rest) = if let Some(stripped) = trimmed.strip_prefix('-') {
        (true, stripped)
    } else {
        (false, trimmed)
    };

    let parsed = if let Some(hex) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        i32::from_str_radix(hex, 16).ok()?
    } else {
        rest.parse::<i32>().ok()?
    };

    Some(if neg { -parsed } else { parsed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Deobfuscator;

    use crate::deobfuscator::DeobfuscateOptions;
    use std::sync::Arc;

    #[test]
    fn test_stringdecoder_new() {
        let transformer = StringDecoder::new();
        assert_eq!(transformer.name(), "StringDecoder");
    }

    #[test]
    fn test_base64_decode() {
        let charset = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=";
        let result = StringDecoder::base64_decode(charset, "SGVsbG8=");
        assert_eq!(result, Some("Hello".to_string()));
    }

    #[test]
    fn test_base64_decode_world() {
        let charset = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=";
        let result = StringDecoder::base64_decode(charset, "V29ybGQ=");
        assert_eq!(result, Some("World".to_string()));
    }

    #[test]
    fn test_base64_decode_no_padding() {
        let charset = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=";
        // "Hi" without padding
        let result = StringDecoder::base64_decode(charset, "SGk");
        assert_eq!(result, Some("Hi".to_string()));
    }

    #[test]
    fn test_rc4_decrypt() {
        let charset = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=";
        // RC4 decryption requires specific encoded input
        // This is a basic test to ensure the function doesn't panic
        let result = StringDecoder::rc4_decrypt(charset, "SGVsbG8=", "key");
        assert!(result.is_some());
    }

    #[test]
    fn test_string_array_finder_variable_form() {
        let deob = Deobfuscator::new();
        let code = r#"
var _0x1234 = ["hello", "world", "test", "foo", "bar"];
console.log(_0x1234[0]);
"#;
        // This tests that the parser doesn't crash on string arrays
        let result = deob.deobfuscate_source(code, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_unused_string_array_removed() {
        let deob = Deobfuscator::new();
        let code = r#"
function _0x2237() {
  var _0x18c20a = ["a", "b", "c", "d", "e"];
  _0x2237 = function() { return _0x18c20a; };
  return _0x2237();
}
const ok = 1;
"#;
        let result = deob.deobfuscate_source(code, None).unwrap();
        assert!(!result.contains("_0x2237"));
        assert!(result.contains("const ok") || result.contains("var ok"));
    }

    #[test]
    fn test_unused_string_array_with_alias_removed() {
        let deob = Deobfuscator::new();
        let code = r#"
function _0x2237() {
  var _0x18c20a = ["a", "b", "c", "d", "e"];
  _0x2237 = function() { return _0x18c20a; };
  return _0x2237();
}
var alias = _0x2237;
const ok = 1;
"#;
        let result = deob.deobfuscate_source(code, None).unwrap();
        assert!(!result.contains("_0x2237"));
        assert!(result.contains("const ok") || result.contains("var ok"));
    }

    #[test]
    fn test_parse_index_str_hex() {
        assert_eq!(parse_index_str("0x10"), Some(16));
        assert_eq!(parse_index_str("-0x1a"), Some(-26));
        assert_eq!(parse_index_str("42"), Some(42));
        assert_eq!(parse_index_str("-7"), Some(-7));
    }

    #[test]
    fn test_extract_string_array() {
        use swc_common::{FileName, SourceMap, sync::Lrc};
        use swc_ecma_parser::{Parser, StringInput, Syntax};

        let cm: Lrc<SourceMap> = Default::default();
        let fm = cm.new_source_file(
            FileName::Custom("test.js".into()).into(),
            r#"["a", "b", "c"]"#,
        );

        let mut parser = Parser::new(
            Syntax::Es(Default::default()),
            StringInput::from(&*fm),
            None,
        );

        let expr = parser.parse_expr().unwrap();
        if let Expr::Array(arr) = &*expr {
            let result = StringArrayFinder::extract_string_array(arr);
            assert!(result.is_some());
            let strings = result.unwrap();
            assert_eq!(strings.len(), 3);
            assert_eq!(strings[0], "a");
            assert_eq!(strings[1], "b");
            assert_eq!(strings[2], "c");
        } else {
            panic!("Expected array expression");
        }
    }

    #[test]
    fn test_decoder_offset_extraction() {
        // Test the offset extraction logic
        use swc_common::{FileName, SourceMap, sync::Lrc};
        use swc_ecma_parser::{Parser, StringInput, Syntax};

        let cm: Lrc<SourceMap> = Default::default();
        let fm = cm.new_source_file(FileName::Custom("test.js".into()).into(), r#"x - 123"#);

        let mut parser = Parser::new(
            Syntax::Es(Default::default()),
            StringInput::from(&*fm),
            None,
        );

        let expr = parser.parse_expr().unwrap();
        let offset = DecoderFunctionFinder::extract_offset(&expr);
        assert_eq!(offset, Some(-123));
    }

    #[test]
    fn test_decoder_offset_extraction_add() {
        use swc_common::{FileName, SourceMap, sync::Lrc};
        use swc_ecma_parser::{Parser, StringInput, Syntax};

        let cm: Lrc<SourceMap> = Default::default();
        let fm = cm.new_source_file(FileName::Custom("test.js".into()).into(), r#"x + 456"#);

        let mut parser = Parser::new(
            Syntax::Es(Default::default()),
            StringInput::from(&*fm),
            None,
        );

        let expr = parser.parse_expr().unwrap();
        let offset = DecoderFunctionFinder::extract_offset(&expr);
        assert_eq!(offset, Some(456));
    }

    #[test]
    fn test_decoder_function_finder_extracts_self_redef_offset() {
        use swc_common::{FileName, SourceMap, sync::Lrc};
        use swc_ecma_parser::{Parser, StringInput, Syntax};
        use swc_ecma_visit::VisitMutWith;

        let code = r#"
function _0xbba6(_0x9779e0,_0x3727db){
  const _0x2a129a=_0x9fd6();
  return _0xbba6=function(_0x5e4e8c,_0x244e6b){
    _0x5e4e8c=_0x5e4e8c-(-0x2315*0x1+0x1938+0xa62);
    let _0x172341=_0x2a129a[_0x5e4e8c];
    return _0x172341;
  },_0xbba6(_0x9779e0,_0x3727db);
}
"#;

        let cm: Lrc<SourceMap> = Default::default();
        let fm = cm.new_source_file(FileName::Custom("test.js".into()).into(), code);

        let mut parser = Parser::new(
            Syntax::Es(Default::default()),
            StringInput::from(&*fm),
            None,
        );

        let script = parser.parse_script().unwrap();
        let mut program = Program::Script(script);

        let string_arrays = vec![StringArray {
            identifier: "_0x9fd6".to_string(),
            array_type: StringArrayType::Function,
            strings: Vec::new(),
        }];

        let mut finder = DecoderFunctionFinder::new(&string_arrays);

        GLOBALS.set(&Default::default(), || {
            program.visit_mut_with(&mut finder);
        });

        assert_eq!(finder.decoders.len(), 1);
        let decoder = &finder.decoders[0];
        assert_eq!(decoder.identifier, "_0xbba6");
        assert_eq!(decoder.string_array_identifier, "_0x9fd6");
        assert_eq!(decoder.offset, -133);
    }

    #[test]
    fn test_stringdecoder_decodes_multi_level_wrapper_calls() {
        let deob = Deobfuscator::new();
        let code = r#"
function _0xarr() {
  const _0x2a129a = ["foo", "bar", "baz"];
  _0xarr = function() { return _0x2a129a; };
  return _0xarr();
}
function _0xbba6(_0x9779e0, _0x3727db) {
  const _0x2a129a = _0xarr();
  _0xbba6 = function(_0x5e4e8c, _0x244e6b) {
    _0x5e4e8c = _0x5e4e8c - (0);
    let _0x172341 = _0x2a129a[_0x5e4e8c];
    return _0x172341;
  };
  return _0xbba6(_0x9779e0, _0x3727db);
}
function _0x40e6ec(_0x16b97b, _0x25372d, _0x291b16, _0x20a4c7, _0x29edc4) {
  return _0xbba6(_0x25372d - -0, _0x16b97b);
}
function _0x4f33c7(_0x6fbb93, _0x51a83f, _0x4c30c6, _0xf312d4, _0x39d538) {
  return _0x40e6ec(_0x6fbb93, _0x4c30c6 - 1, 0, 0, 0);
}
const x = _0x4f33c7(0, 0, 1, 0, 0);
"#;

        let options = DeobfuscateOptions {
            custom_transformers: Some(vec![Arc::new(StringDecoder::new())]),
            ..Default::default()
        };

        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(result.contains("\"foo\""));
        assert!(result.contains("const x") || result.contains("var x"));
    }
}
