//! AST visitor utilities
//!
//! Provides utilities for walking and transforming the AST.

use swc_ecma_ast::*;
use swc_ecma_visit::{VisitMut, VisitMutWith};

/// A trait for AST transformations that can be applied during a visit.
#[expect(dead_code)]
pub trait AstTransform: VisitMut {
    /// Get the name of this transform
    fn name(&self) -> &'static str;
}

/// Walk the AST and apply a visitor
#[expect(dead_code)]
pub fn walk_mut<V: VisitMut>(program: &mut Program, visitor: &mut V) {
    program.visit_mut_with(visitor);
}
