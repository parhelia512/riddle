use std::collections::HashMap;

use hir::body::{BodyId, ExprId};

use crate::types::Type;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub message: String,
}

#[derive(Debug, Default, Clone)]
pub struct TypeCheckResult {
    pub diagnostics: Vec<Diagnostic>,
    pub expr_types: HashMap<(BodyId, ExprId), Type>,
}
