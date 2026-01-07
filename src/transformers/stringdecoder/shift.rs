use std::collections::{HashMap, HashSet};
use swc_ecma_ast::*;
use swc_ecma_visit::{VisitMut, VisitMutWith};

use crate::context::{DecoderFunction, DecoderFunctionType, DecoderReference, StringArray};

use super::core::{StringDecoder, eval_const_i64};

pub(super) struct ShiftFinder<'a> {
    pub(super) rotations: Vec<(String, usize)>,
    string_arrays: &'a [StringArray],
    string_decoders: &'a [DecoderFunction],
    decoder_references: &'a [DecoderReference],
}

/// Rotates string array literals in the AST to match detected rotations
pub(super) struct StringArrayRotator {
    rotations: HashMap<String, usize>,
}

impl StringArrayRotator {
    #[must_use]
    pub(super) fn new(rotations: &[(String, usize)]) -> Self {
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
pub(super) struct RotationIifeRemover {
    rotated_names: HashSet<String>,
}

impl RotationIifeRemover {
    #[must_use]
    pub(super) fn new(rotations: &[(String, usize)]) -> Self {
        let mut map = HashSet::new();
        for (name, _) in rotations {
            map.insert(name.clone());
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
        if !self.rotated_names.contains(&array_name) {
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
    pub(super) const fn new(
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
                MemberProp::PrivateName(_) => false,
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
                            DecoderFunctionType::Base91 => {
                                let charset = decoder.charset.as_ref()?;
                                StringDecoder::base91_decode(charset, encoded)
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
