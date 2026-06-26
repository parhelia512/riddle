use std::collections::HashMap;

use hir::body::{BodyId, ExprId};

use crate::{TraitEnv, types::Type};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct TypeCheckResult {
    pub diagnostics: Vec<Diagnostic>,
    pub expr_types: HashMap<(BodyId, ExprId), Type>,
    /// Trait implementation environment, built during type checking.
    /// Available for downstream passes like move checking.
    pub trait_env: TraitEnv,
}

impl Default for TypeCheckResult {
    fn default() -> Self {
        Self {
            diagnostics: Vec::new(),
            expr_types: HashMap::new(),
            trait_env: TraitEnv::default(),
        }
    }
}
