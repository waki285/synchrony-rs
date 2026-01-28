//! `LiteralDecoder` transformer
//!
//! Replaces literal base64 decoder calls (atob wrappers) with decoded strings.

use std::collections::HashSet;

use swc_common::{GLOBALS, Globals, Span};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitMut, VisitMutWith as _, VisitWith as _};

use crate::context::Context;
use crate::error::Result;
use crate::scope::{Id, ScopeData, analyze};
use crate::transformers::Transformer;

const BASE64_ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=";
const GLOBAL_NAMES: [&str; 4] = ["window", "globalThis", "self", "global"];

/// `LiteralDecoder` transformer.
///
/// Decodes base64 literal calls for simple atob wrappers.
#[derive(Debug)]
pub struct LiteralDecoder;

impl LiteralDecoder {
    /// Creates a new transformer instance.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for LiteralDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Transformer for LiteralDecoder {
    fn name(&self) -> &'static str {
        "LiteralDecoder"
    }

    fn transform(&self, context: &mut Context) -> Result<()> {
        let scope_data = GLOBALS.set(&Globals::default(), || analyze(&context.ast));

        let mut collector = DecoderCollector::new(&scope_data);
        context.ast.visit_with(&mut collector);

        let mut replacer = DecoderReplacer {
            base64_wrappers: collector.base64_wrappers,
            atob_aliases: collector.atob_aliases,
            scope_data: &scope_data,
            global_aliases: collector.global_aliases,
        };
        context.ast.visit_mut_with(&mut replacer);
        Ok(())
    }
}

struct DecoderCollector<'a> {
    scope_data: &'a ScopeData,
    global_aliases: HashSet<String>,
    atob_aliases: HashSet<Id>,
    base64_wrappers: HashSet<Id>,
}

impl<'a> DecoderCollector<'a> {
    fn new(scope_data: &'a ScopeData) -> Self {
        let mut global_aliases = HashSet::new();
        for name in GLOBAL_NAMES {
            global_aliases.insert(name.to_owned());
        }
        Self {
            scope_data,
            global_aliases,
            atob_aliases: HashSet::new(),
            base64_wrappers: HashSet::new(),
        }
    }

    fn is_global_ident(&self, ident: &Ident) -> bool {
        if self.global_aliases.contains(ident.sym.as_ref()) {
            return true;
        }
        let id: Id = (ident.sym.clone(), ident.ctxt);
        self.scope_data
            .vars
            .get(&id)
            .is_none_or(|info| !info.declared)
    }

    fn is_atob_member_expr(&self, member: &MemberExpr) -> bool {
        let MemberProp::Ident(prop) = &member.prop else {
            return false;
        };
        if prop.sym.as_ref() != "atob" {
            return false;
        }
        match &*member.obj {
            Expr::Ident(obj) => self.is_global_ident(obj),
            Expr::This(_) => true,
            _ => false,
        }
    }

    fn is_atob_expr(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Ident(ident) => ident.sym.as_ref() == "atob" && self.is_global_ident(ident),
            Expr::Member(member) => self.is_atob_member_expr(member),
            _ => false,
        }
    }

    fn maybe_register_atob_alias_ident(&mut self, ident: &Ident, init: &Expr) {
        if self.is_atob_expr(init) {
            let id: Id = (ident.sym.clone(), ident.ctxt);
            self.atob_aliases.insert(id);
        }
    }

    fn maybe_register_global_alias_ident(&mut self, ident: &Ident, init: &Expr) {
        let is_global = match init {
            Expr::Ident(ident) => self.is_global_ident(ident),
            Expr::This(_) => true,
            _ => false,
        };
        if is_global {
            self.global_aliases.insert(ident.sym.to_string());
        }
    }

    fn maybe_register_base64_wrapper_ident(&mut self, ident: &Ident, func: &Function) {
        if func.params.len() != 1 {
            return;
        }
        let [param] = func.params.as_slice() else {
            return;
        };
        let Pat::Ident(param) = &param.pat else {
            return;
        };
        if is_base64_decoder_function(
            func,
            param.id.sym.as_ref(),
            &self.atob_aliases,
            &self.global_aliases,
        ) {
            let id: Id = (ident.sym.clone(), ident.ctxt);
            self.base64_wrappers.insert(id);
        }
    }

    fn maybe_register_base64_wrapper_arrow_ident(&mut self, ident: &Ident, arrow: &ArrowExpr) {
        if arrow.params.len() != 1 {
            return;
        }
        let [param] = arrow.params.as_slice() else {
            return;
        };
        let Pat::Ident(param) = param else {
            return;
        };
        if is_base64_decoder_arrow(
            arrow,
            param.id.sym.as_ref(),
            &self.atob_aliases,
            &self.global_aliases,
        ) {
            let id: Id = (ident.sym.clone(), ident.ctxt);
            self.base64_wrappers.insert(id);
        }
    }
}

impl Visit for DecoderCollector<'_> {
    fn visit_var_declarator(&mut self, decl: &VarDeclarator) {
        if let Pat::Ident(binding) = &decl.name
            && let Some(init) = &decl.init
        {
            self.maybe_register_global_alias_ident(&binding.id, init);
            self.maybe_register_atob_alias_ident(&binding.id, init);
            match &**init {
                Expr::Fn(fn_expr) => {
                    self.maybe_register_base64_wrapper_ident(&binding.id, &fn_expr.function);
                }
                Expr::Arrow(arrow) => {
                    self.maybe_register_base64_wrapper_arrow_ident(&binding.id, arrow);
                }
                _ => {}
            }
        }

        decl.visit_children_with(self);
    }

    fn visit_fn_decl(&mut self, decl: &FnDecl) {
        if decl.function.params.len() == 1 {
            let [param] = decl.function.params.as_slice() else {
                decl.visit_children_with(self);
                return;
            };
            let Pat::Ident(param) = &param.pat else {
                decl.visit_children_with(self);
                return;
            };
            if is_base64_decoder_function(
                &decl.function,
                param.id.sym.as_ref(),
                &self.atob_aliases,
                &self.global_aliases,
            ) {
                let id: Id = (decl.ident.sym.clone(), decl.ident.ctxt);
                self.base64_wrappers.insert(id);
            }
        }

        decl.visit_children_with(self);
    }

    fn visit_assign_expr(&mut self, expr: &AssignExpr) {
        if let AssignTarget::Simple(SimpleAssignTarget::Ident(binding)) = &expr.left {
            match &*expr.right {
                Expr::Fn(fn_expr) => {
                    self.maybe_register_base64_wrapper_ident(&binding.id, &fn_expr.function);
                }
                Expr::Arrow(arrow) => {
                    self.maybe_register_base64_wrapper_arrow_ident(&binding.id, arrow);
                }
                Expr::Ident(_) | Expr::Member(_) => {
                    self.maybe_register_atob_alias_ident(&binding.id, &expr.right);
                }
                _ => {}
            }
        }
        expr.visit_children_with(self);
    }
}

struct DecoderReplacer<'a> {
    base64_wrappers: HashSet<Id>,
    atob_aliases: HashSet<Id>,
    scope_data: &'a ScopeData,
    global_aliases: HashSet<String>,
}

impl DecoderReplacer<'_> {
    fn is_global_ident(&self, ident: &Ident) -> bool {
        if self.global_aliases.contains(ident.sym.as_ref()) {
            return true;
        }
        let id: Id = (ident.sym.clone(), ident.ctxt);
        self.scope_data
            .vars
            .get(&id)
            .is_none_or(|info| !info.declared)
    }

    fn is_atob_member_expr(&self, member: &MemberExpr) -> bool {
        let MemberProp::Ident(prop) = &member.prop else {
            return false;
        };
        if prop.sym.as_ref() != "atob" {
            return false;
        }
        match &*member.obj {
            Expr::Ident(obj) => self.is_global_ident(obj),
            Expr::This(_) => true,
            _ => false,
        }
    }

    fn is_atob_ident(&self, ident: &Ident) -> bool {
        if ident.sym.as_ref() == "atob" && self.is_global_ident(ident) {
            return true;
        }
        let id: Id = (ident.sym.clone(), ident.ctxt);
        self.atob_aliases.contains(&id)
    }

    fn is_base64_wrapper_ident(&self, ident: &Ident) -> bool {
        let id: Id = (ident.sym.clone(), ident.ctxt);
        self.base64_wrappers.contains(&id)
    }

    fn decode_base64_literal(&self, expr: &Expr) -> Option<Expr> {
        let Expr::Lit(Lit::Str(s)) = expr else {
            return None;
        };
        let raw = s.value.as_str()?;
        let decoded = decode_base64_binary_string(raw)?;
        Some(Expr::Lit(Lit::Str(Str {
            span: Span::default(),
            value: decoded.into(),
            raw: None,
        })))
    }
}

impl VisitMut for DecoderReplacer<'_> {
    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        expr.visit_mut_children_with(self);

        let Expr::Call(call) = expr else {
            return;
        };

        if call.args.len() != 1 {
            return;
        }
        let Some(arg_expr) = call.args.first().map(|arg| &arg.expr) else {
            return;
        };

        let should_decode = match &call.callee {
            Callee::Expr(callee) => match &**callee {
                Expr::Ident(ident) => {
                    self.is_atob_ident(ident) || self.is_base64_wrapper_ident(ident)
                }
                Expr::Member(member) => self.is_atob_member_expr(member),
                _ => false,
            },
            _ => false,
        };

        if !should_decode {
            return;
        }

        if let Some(decoded) = self.decode_base64_literal(arg_expr) {
            *expr = decoded;
        }
    }
}

fn is_base64_decoder_arrow(
    arrow: &ArrowExpr,
    param_name: &str,
    atob_aliases: &HashSet<Id>,
    global_aliases: &HashSet<String>,
) -> bool {
    let mut scan = Base64DecoderScan::new(param_name, atob_aliases, global_aliases);
    match &*arrow.body {
        BlockStmtOrExpr::BlockStmt(block) => block.visit_with(&mut scan),
        BlockStmtOrExpr::Expr(expr) => expr.visit_with(&mut scan),
    }
    scan.is_match()
}

fn is_base64_decoder_function(
    func: &Function,
    param_name: &str,
    atob_aliases: &HashSet<Id>,
    global_aliases: &HashSet<String>,
) -> bool {
    let Some(body) = &func.body else {
        return false;
    };
    let mut scan = Base64DecoderScan::new(param_name, atob_aliases, global_aliases);
    body.visit_with(&mut scan);
    scan.is_match()
}

struct Base64DecoderScan<'a> {
    param_name: &'a str,
    atob_alias_names: HashSet<String>,
    global_aliases: &'a HashSet<String>,
    has_atob_call: bool,
    has_alphabet: bool,
    has_index_of: bool,
    has_from_char_code: bool,
}

impl<'a> Base64DecoderScan<'a> {
    fn new(
        param_name: &'a str,
        atob_aliases: &HashSet<Id>,
        global_aliases: &'a HashSet<String>,
    ) -> Self {
        let atob_alias_names = atob_aliases
            .iter()
            .map(|(sym, _)| sym.to_string())
            .collect();
        Self {
            param_name,
            atob_alias_names,
            global_aliases,
            has_atob_call: false,
            has_alphabet: false,
            has_index_of: false,
            has_from_char_code: false,
        }
    }

    fn is_atob_call(&self, call: &CallExpr) -> bool {
        let Callee::Expr(callee) = &call.callee else {
            return false;
        };
        let is_atob = match &**callee {
            Expr::Ident(ident) => {
                ident.sym.as_ref() == "atob" || self.atob_alias_names.contains(ident.sym.as_ref())
            }
            Expr::Member(member) => {
                let MemberProp::Ident(prop) = &member.prop else {
                    return false;
                };
                if prop.sym.as_ref() != "atob" {
                    return false;
                }
                match &*member.obj {
                    Expr::Ident(obj) => self.global_aliases.contains(obj.sym.as_ref()),
                    Expr::This(_) => true,
                    _ => false,
                }
            }
            _ => false,
        };
        if !is_atob {
            return false;
        }
        call.args
            .first()
            .and_then(|arg| match &*arg.expr {
                Expr::Ident(ident) => Some(ident.sym.as_ref() == self.param_name),
                Expr::Paren(paren) => match &*paren.expr {
                    Expr::Ident(ident) => Some(ident.sym.as_ref() == self.param_name),
                    _ => None,
                },
                _ => None,
            })
            .unwrap_or(false)
    }

    fn is_string_from_char_code(call: &CallExpr) -> bool {
        let Callee::Expr(callee) = &call.callee else {
            return false;
        };
        match &**callee {
            Expr::Member(member) => {
                let MemberProp::Ident(prop) = &member.prop else {
                    return false;
                };
                if prop.sym.as_ref() != "fromCharCode" {
                    return false;
                }
                matches!(&*member.obj, Expr::Ident(ident) if ident.sym.as_ref() == "String")
            }
            Expr::Ident(ident) => ident.sym.as_ref() == "fromCharCode",
            _ => false,
        }
    }

    const fn is_match(&self) -> bool {
        if self.has_atob_call {
            return true;
        }
        self.has_alphabet && self.has_index_of && self.has_from_char_code
    }
}

impl Visit for Base64DecoderScan<'_> {
    fn visit_lit(&mut self, lit: &Lit) {
        if let Lit::Str(s) = lit
            && s.value.as_str() == Some(BASE64_ALPHABET)
        {
            self.has_alphabet = true;
        }
        lit.visit_children_with(self);
    }

    fn visit_member_expr(&mut self, expr: &MemberExpr) {
        if let MemberProp::Ident(prop) = &expr.prop
            && prop.sym.as_ref() == "indexOf"
        {
            self.has_index_of = true;
        }
        expr.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        if self.is_atob_call(call) {
            self.has_atob_call = true;
        }
        if Self::is_string_from_char_code(call) {
            self.has_from_char_code = true;
        }
        call.visit_children_with(self);
    }
}

fn decode_base64_binary_string(input: &str) -> Option<String> {
    let bytes = decode_base64_bytes(input)?;
    let mut out = String::with_capacity(bytes.len());
    for b in bytes {
        out.push(char::from(b));
    }
    Some(out)
}

fn decode_base64_bytes(input: &str) -> Option<Vec<u8>> {
    const PADDING: u8 = 64;
    const SHIFT_2: u32 = 2;
    const SHIFT_4: u32 = 4;
    const SHIFT_6: u32 = 6;
    const MASK_LOW: u8 = 0x0f;
    const MASK_TWO: u8 = 0x03;

    let mut out = Vec::new();
    let mut chunk: [u8; 4] = [0; 4];
    let mut count: usize = 0;

    for ch in input.chars() {
        if ch == '=' {
            let slot = chunk.get_mut(count)?;
            *slot = PADDING;
            count += 1;
        } else if ch.is_ascii_whitespace() {
            continue;
        } else if let Some(idx) = BASE64_ALPHABET.find(ch) {
            let slot = chunk.get_mut(count)?;
            *slot = u8::try_from(idx).ok()?;
            count += 1;
        } else {
            return None;
        }

        if count == 4 {
            let [c0, c1, c2, c3] = chunk;
            if c0 == PADDING || c1 == PADDING {
                return None;
            }
            let b0 = (c0 << SHIFT_2) | (c1 >> SHIFT_4);
            out.push(b0);

            if c2 != PADDING {
                let b1 = ((c1 & MASK_LOW) << SHIFT_4) | (c2 >> SHIFT_2);
                out.push(b1);

                if c3 != PADDING {
                    let b2 = ((c2 & MASK_TWO) << SHIFT_6) | c3;
                    out.push(b2);
                }
            } else if c3 != PADDING {
                return None;
            }

            count = 0;
        }
    }

    if count != 0 {
        return None;
    }

    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::deobfuscator::{DeobfuscateOptions, Deobfuscator};

    #[test]
    fn literal_decoder_atob() {
        let deob = Deobfuscator::new();
        let code = "const x = atob(\"SGVsbG8=\");";
        let options = DeobfuscateOptions {
            custom_transformers: Some(vec![Arc::new(LiteralDecoder::new())]),
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(result.contains("Hello"));
    }

    #[test]
    fn literal_decoder_wrapper() {
        let deob = Deobfuscator::new();
        let code = r#"
var G = window;
var L = G.atob;
function j(t) { return L(t); }
const x = j("SGVsbG8=");
"#;
        let options = DeobfuscateOptions {
            custom_transformers: Some(vec![Arc::new(LiteralDecoder::new())]),
            ..Default::default()
        };
        let result = deob.deobfuscate_source(code, Some(options)).unwrap();
        assert!(result.contains("Hello"));
    }
}
