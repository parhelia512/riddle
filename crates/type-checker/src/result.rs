use std::collections::HashMap;

use hir::{
    body::{BodyId, ExprId, PatternBindingId, StmtId},
    item_tree::{FunctionId, TraitId},
};
use rowan::TextRange;

use crate::{
    TraitEnv,
    types::{ClosureKind, Type},
};

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
    pub for_loops: HashMap<(BodyId, ExprId), ForLoopInfo>,
    pub lambda_infos: HashMap<(BodyId, ExprId), LambdaInfo>,
    /// Trait implementation environment, built during type checking.
    /// Available for downstream passes like move checking.
    pub trait_env: TraitEnv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptureMode {
    Shared,
    Mutable,
    Value,
}

impl CaptureMode {
    pub(crate) fn merge(self, other: Self) -> Self {
        use CaptureMode::{Mutable, Shared, Value};
        match (self, other) {
            (Value, _) | (_, Value) => Value,
            (Mutable, _) | (_, Mutable) => Mutable,
            _ => Shared,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CaptureSource {
    Local(StmtId),
    Pattern(PatternBindingId),
    Param(usize),
    LambdaParam { lambda: ExprId, index: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LambdaCapture {
    pub source: CaptureSource,
    pub name: String,
    pub ty: Type,
    pub mode: CaptureMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LambdaInfo {
    pub captures: Vec<LambdaCapture>,
    pub kind: ClosureKind,
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
pub enum OperatorCall {
    Function(FunctionId),
    Trait(TraitMethodCall),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForLoopInfo {
    pub into_iter: TraitMethodCall,
    pub next: TraitMethodCall,
    pub item_ty: Type,
    pub iter_ty: Type,
    pub next_ty: Type,
    pub some_variant: usize,
}
