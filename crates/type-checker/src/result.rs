use std::collections::HashMap;

use hir::{
    body::{BodyId, ExprId, PatId, PatternBindingId},
    item_tree::{FunctionId, TraitId},
};
use rowan::TextRange;

use crate::{
    TraitEnv,
    types::{ClosureKind, OpaqueCallableId, Type},
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
    pub expr_coercions: HashMap<(BodyId, ExprId), Type>,
    pub generic_calls: HashMap<(BodyId, ExprId), GenericCall>,
    pub trait_method_calls: HashMap<(BodyId, ExprId), TraitMethodCall>,
    pub operator_calls: HashMap<(BodyId, ExprId), OperatorCall>,
    pub for_loops: HashMap<(BodyId, ExprId), ForLoopInfo>,
    pub lambda_infos: HashMap<(BodyId, ExprId), LambdaInfo>,
    pub pattern_types: HashMap<(BodyId, PatId), Type>,
    pub pattern_binding_types: HashMap<(BodyId, PatternBindingId), Type>,
    pub pattern_binding_modes: HashMap<(BodyId, PatternBindingId), PatternBindingMode>,
    pub value_uses: HashMap<(BodyId, ExprId), ValueUse>,
    pub opaque_hidden_types: HashMap<OpaqueCallableId, Type>,
    /// Trait implementation environment, built during type checking.
    /// Available for downstream passes like move checking.
    pub trait_env: TraitEnv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PatternBindingMode {
    Move,
    Ref,
    RefMut,
}

impl PatternBindingMode {
    pub(crate) fn through_reference(self, mutable: bool) -> Self {
        match (self, mutable) {
            (Self::Ref, _) | (Self::RefMut, false) => Self::Ref,
            (_, true) => Self::RefMut,
            (Self::Move, false) => Self::Ref,
        }
    }

    pub(crate) fn binding_type(self, ty: Type) -> Type {
        match self {
            Self::Move => ty,
            Self::Ref => Type::Ref(Box::new(ty), false),
            Self::RefMut => Type::Ref(Box::new(ty), true),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptureMode {
    Shared,
    Mutable,
    Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueUse {
    Shared,
    Mutable,
    Copy,
    Move,
}

impl ValueUse {
    pub(crate) fn merge(self, other: Self) -> Self {
        use ValueUse::{Copy, Move, Mutable, Shared};
        match (self, other) {
            (Move, _) | (_, Move) => Move,
            (Mutable, _) | (_, Mutable) => Mutable,
            (Copy, _) | (_, Copy) => Copy,
            _ => Shared,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CaptureSource {
    Pattern(PatternBindingId),
    Param(usize),
    LambdaParam { lambda: ExprId, index: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CapturePlace {
    pub source: CaptureSource,
    pub projections: Vec<hir::place::Projection>,
}

impl CapturePlace {
    pub fn root(source: CaptureSource) -> Self {
        Self {
            source,
            projections: Vec::new(),
        }
    }

    pub fn is_prefix_of(&self, other: &Self) -> bool {
        self.source == other.source
            && self.projections.len() <= other.projections.len()
            && self
                .projections
                .iter()
                .zip(&other.projections)
                .all(|(left, right)| match (left, right) {
                    (hir::place::Projection::Index(None), hir::place::Projection::Index(_))
                    | (hir::place::Projection::Index(_), hir::place::Projection::Index(None)) => {
                        true
                    }
                    _ => left == right,
                })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LambdaCapture {
    pub place: CapturePlace,
    pub name: String,
    pub ty: Type,
    pub mode: CaptureMode,
    pub use_kind: ValueUse,
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
