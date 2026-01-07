use std::collections::HashSet;
use swc_common::{Globals, GLOBALS};
use swc_ecma_ast::*;
use swc_ecma_visit::{VisitMutWith, VisitWith};

use crate::context::{Context, StringArray, StringArrayType};
use crate::error::Result;
use crate::scope::analyze;
use crate::transformers::Transformer;

use super::arrays::{StringArrayDeclarationRemover, StringArrayFinder};
use super::decoder_finder::DecoderFunctionFinder;
use super::decoder_helper::DecoderHelperFinder;
use super::garbage::{
    ExternalUsageFinder, ObfuscationAliasCleaner, ObfuscationGarbageRemover,
    UnusedObfuscatedRemover,
};
use super::references::{FunctionReferenceFinder, VariableReferenceFinder};
use super::replacer::{StringDecoderReplacer, remove_tainted_statements};
use super::shift::{RotationIifeRemover, ShiftFinder, StringArrayRotator};

// Evaluate simple constant numeric expressions (used by rotations/offsets).
pub(super) fn eval_const_i64(expr: &Expr) -> Option<i64> {
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
                    #[expect(
                        clippy::cast_possible_wrap,
                        reason = "JS >>> uses unsigned 32-bit semantics; wrap is intentional"
                    )]
                    let shifted = (l >> (right as u32)) as i64;
                    Some(shifted)
                }
                _ => None,
            }
        }
        Expr::Paren(paren) => eval_const_i64(&paren.expr),
        Expr::Seq(seq) => seq.exprs.last().and_then(|e| eval_const_i64(e)),
        _ => None,
    }
}

/// `StringDecoder` transformer.
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
    pub(super) fn base64_decode(charset: &str, input: &str) -> Option<String> {
        let output = Self::base64_decode_bytes(charset, input)?;

        // Try to decode as UTF-8, falling back to lossy conversion
        match String::from_utf8(output) {
            Ok(s) => Some(s),
            Err(e) => Some(String::from_utf8_lossy(e.as_bytes()).into_owned()),
        }
    }

    /// Base91 decode with custom charset to bytes
    #[must_use]
    fn base91_decode_bytes(charset: &str, input: &str) -> Vec<u8> {
        let mut output = Vec::new();
        let mut buffer = 0u32;
        let mut bits_collected = 0u32;
        let mut value = -1i32;

        for ch in input.chars() {
            let Some(idx) = charset
                .find(ch)
                .and_then(|v| i32::try_from(v).ok())
            else {
                continue;
            };

            if value < 0 {
                value = idx;
            } else {
                let combined = value + idx * 91;
                buffer |= (combined as u32) << bits_collected;
                bits_collected += if (combined & 0x1fff) > 88 { 13 } else { 14 };

                while bits_collected > 7 {
                    output.push((buffer & 0xFF) as u8);
                    buffer >>= 8;
                    bits_collected -= 8;
                }
                value = -1;
            }
        }

        if value > -1 {
            output.push(((buffer | ((value as u32) << bits_collected)) & 0xFF) as u8);
        }

        output
    }

    /// Base91 decode with custom charset
    #[must_use]
    #[expect(
        clippy::unnecessary_wraps,
        reason = "kept as Option for consistency with other decoder helpers"
    )]
    pub(super) fn base91_decode(charset: &str, input: &str) -> Option<String> {
        let output = Self::base91_decode_bytes(charset, input);

        match String::from_utf8(output) {
            Ok(s) => Some(s),
            Err(e) => Some(String::from_utf8_lossy(e.as_bytes()).into_owned()),
        }
    }

    /// RC4 decrypt
    #[must_use]
    pub(super) fn rc4_decrypt(charset: &str, input: &str, key: &str) -> Option<String> {
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

        // Second pass: find decoder helper functions (e.g., base91)
        let mut helper_finder = DecoderHelperFinder::new();
        context.ast.visit_with(&mut helper_finder);
        let helper_names: HashSet<String> = helper_finder.helpers.keys().cloned().collect();

        // Third pass: find decoder functions (needed for shift calculation)
        let mut decoder_finder =
            DecoderFunctionFinder::new(&context.string_arrays, &helper_finder.helpers);
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

        // Fourth pass: find variable references
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

        // Fifth pass: find function references (wrapper functions around decoders).
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

        // Sixth pass: find push/shift rotation patterns (IIFE that rotates array)
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

        // Seventh pass: replace decoder calls
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
            for helper in &helper_names {
                candidates.insert(helper.clone());
            }

            if !candidates.is_empty() {
                let mut scope_data = GLOBALS.set(&Globals::default(), || analyze(&context.ast));
                if !scope_data.top.has_with_stmt && !scope_data.top.has_eval_call {
                    let mut alias_cleaner = ObfuscationAliasCleaner::new(&scope_data, &candidates);
                    context.ast.visit_mut_with(&mut alias_cleaner);

                    scope_data = GLOBALS.set(&Globals::default(), || analyze(&context.ast));

                    for _ in 0..4 {
                        let mut remover = UnusedObfuscatedRemover::new(&scope_data);
                        context.ast.visit_mut_with(&mut remover);
                        if !remover.changed {
                            break;
                        }
                        scope_data = GLOBALS.set(&Globals::default(), || analyze(&context.ast));
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

            if !candidates.is_empty() {
                let mut decoder_names: HashSet<String> = HashSet::new();
                for decoder in &context.string_decoders {
                    decoder_names.insert(decoder.identifier.clone());
                }
                for reference in &context.string_decoder_references {
                    decoder_names.insert(reference.identifier.clone());
                }

                remove_tainted_statements(&mut context.ast, &candidates, &decoder_names);
            }
        }

        Ok(())
    }
}
