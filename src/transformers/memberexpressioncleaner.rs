//! MemberExpressionCleaner transformer
//!
//! Converts computed member expressions with string literals to dot notation.
//! Example: `obj["property"]` -> `obj.property`

use regex::Regex;
use std::sync::LazyLock;
use swc_ecma_ast::*;
use swc_ecma_visit::{VisitMut, VisitMutWith};

use crate::context::Context;
use crate::error::Result;
use crate::transformers::Transformer;

// Only replace if the property accessor matches this regex
// Will not match things like "content-type" or "123"
static VALID_DOT_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z_$][a-zA-Z0-9_$]*$").expect("valid identifier regex should compile")
});

/// MemberExpressionCleaner transformer.
///
/// Converts `obj["prop"]` to `obj.prop` when the property is a valid identifier.
#[derive(Debug)]
pub struct MemberExpressionCleaner;

impl MemberExpressionCleaner {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for MemberExpressionCleaner {
    fn default() -> Self {
        Self::new()
    }
}

impl Transformer for MemberExpressionCleaner {
    fn name(&self) -> &'static str {
        "MemberExpressionCleaner"
    }

    fn transform(&self, context: &mut Context) -> Result<()> {
        let mut visitor = MemberExpressionCleanerVisitor;
        context.ast.visit_mut_with(&mut visitor);
        Ok(())
    }
}

struct MemberExpressionCleanerVisitor;

impl VisitMut for MemberExpressionCleanerVisitor {
    fn visit_mut_member_expr(&mut self, expr: &mut MemberExpr) {
        // First visit children
        expr.visit_mut_children_with(self);

        // Only process computed member expressions with string literal properties
        if !expr.prop.is_computed() {
            return;
        }

        if let MemberProp::Computed(computed) = &expr.prop
            && let Expr::Lit(Lit::Str(s)) = &*computed.expr
        {
            // Check if the string is a valid identifier
            if let Some(value) = s.value.as_str()
                && VALID_DOT_REGEX.is_match(value)
            {
                // Convert to dot notation
                expr.prop = MemberProp::Ident(IdentName {
                    span: s.span,
                    sym: value.into(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_identifier_regex() {
        assert!(VALID_DOT_REGEX.is_match("property"));
        assert!(VALID_DOT_REGEX.is_match("camelCase"));
        assert!(VALID_DOT_REGEX.is_match("_private"));
        assert!(VALID_DOT_REGEX.is_match("$jquery"));
        assert!(VALID_DOT_REGEX.is_match("prop123"));

        assert!(!VALID_DOT_REGEX.is_match("content-type"));
        assert!(!VALID_DOT_REGEX.is_match("123"));
        assert!(!VALID_DOT_REGEX.is_match("has space"));
        assert!(!VALID_DOT_REGEX.is_match(""));
    }
}
