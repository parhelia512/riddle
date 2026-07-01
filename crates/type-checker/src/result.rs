use std::collections::HashMap;

use hir::{
    body::{BodyId, ExprId},
    item_tree::{FunctionId, TraitId},
};
use rowan::TextRange;

use crate::{TraitEnv, types::Type};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub severity: Severity,
    pub message: String,
    pub labels: Vec<SourceLabel>,
    pub help: Option<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLabel {
    pub range: TextRange,
    pub message: String,
    pub style: LabelStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelStyle {
    Primary,
    Secondary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Note,
    Help,
}

#[derive(Debug, Clone, Default)]
pub struct TypeCheckResult {
    pub diagnostics: Vec<Diagnostic>,
    pub expr_types: HashMap<(BodyId, ExprId), Type>,
    pub generic_calls: HashMap<(BodyId, ExprId), GenericCall>,
    pub trait_method_calls: HashMap<(BodyId, ExprId), TraitMethodCall>,
    pub operator_calls: HashMap<(BodyId, ExprId), OperatorCall>,
    /// Trait implementation environment, built during type checking.
    /// Available for downstream passes like move checking.
    pub trait_env: TraitEnv,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericCall {
    pub args: Vec<Type>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraitMethodCall {
    pub trait_id: TraitId,
    pub method: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorCall {
    pub function: FunctionId,
}
