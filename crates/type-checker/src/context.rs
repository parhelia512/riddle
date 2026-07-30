use std::collections::{HashMap, HashSet};

use hir::{
    body::{Body, BodyId, ExprId, PatternBindingId, ResolvedName, SourceMap, StmtId},
    item_tree::{ConstId, FunctionId, HirConst, HirFunction},
};
use rowan::TextRange;

use crate::{result::LambdaCapture, types::Type};

pub(crate) struct BodyCtx<'a> {
    pub(crate) body_id: BodyId,
    pub(crate) body: &'a Body,
    pub(crate) function_id: Option<FunctionId>,
    pub(crate) function: Option<&'a HirFunction>,
    pub(crate) const_id: Option<ConstId>,
    owner_range: TextRange,
    pub(crate) return_ty: Type,
    pub(crate) generic_params: HashMap<String, Type>,
    pub(crate) bindings: ScopedBindings,
    /// Bindings declared without an initializer. Their first assignment is
    /// allowed even when the binding is not `mut`; move checking enforces the
    /// definite-initialization and reassignment rules afterwards.
    pub(crate) delayed_bindings: HashSet<PatternBindingId>,
    pub(crate) loop_depth: usize,
    pub(crate) unsafe_depth: usize,
    pub(crate) lambdas: Vec<LambdaCtx>,
    source_map: &'a SourceMap,
}

impl<'a> BodyCtx<'a> {
    pub(crate) fn new(
        body_id: BodyId,
        body: &'a Body,
        function_id: FunctionId,
        function: &'a HirFunction,
        return_ty: Type,
        generic_params: HashMap<String, Type>,
    ) -> Self {
        Self {
            body_id,
            body,
            function_id: Some(function_id),
            function: Some(function),
            const_id: None,
            owner_range: function.name_range,
            return_ty,
            generic_params,
            bindings: ScopedBindings::default(),
            delayed_bindings: HashSet::new(),
            loop_depth: 0,
            unsafe_depth: 0,
            lambdas: Vec::new(),
            source_map: &body.source_map,
        }
    }

    pub(crate) fn new_const(
        body_id: BodyId,
        body: &'a Body,
        const_id: ConstId,
        konst: &'a HirConst,
        ty: Type,
        generic_params: HashMap<String, Type>,
    ) -> Self {
        Self {
            body_id,
            body,
            function_id: None,
            function: None,
            const_id: Some(const_id),
            owner_range: konst.name_range,
            return_ty: ty,
            generic_params,
            bindings: ScopedBindings::default(),
            delayed_bindings: HashSet::new(),
            loop_depth: 0,
            unsafe_depth: 0,
            lambdas: Vec::new(),
            source_map: &body.source_map,
        }
    }

    pub(crate) fn owner_range(&self) -> TextRange {
        self.owner_range
    }

    pub(crate) fn push_scope(&mut self) {
        self.bindings.push_scope();
    }

    pub(crate) fn pop_scope(&mut self) {
        self.bindings.pop_scope();
    }

    pub(crate) fn mark_delayed_binding(&mut self, id: PatternBindingId) {
        self.delayed_bindings.insert(id);
    }

    pub(crate) fn is_delayed_binding(&self, id: PatternBindingId) -> bool {
        self.delayed_bindings.contains(&id)
    }

    pub(crate) fn expr_range(&self, id: ExprId) -> Option<TextRange> {
        self.source_map.expr_ranges.get(&id).copied()
    }

    pub(crate) fn stmt_range(&self, id: StmtId) -> Option<TextRange> {
        self.source_map.stmt_ranges.get(&id).copied()
    }

    pub(crate) fn pat_range(&self, id: hir::body::PatId) -> Option<TextRange> {
        self.source_map.pat_ranges.get(&id).copied()
    }

    pub(crate) fn resolved_param_is_mut(&self, resolved: &ResolvedName) -> bool {
        match resolved {
            ResolvedName::Param(index) => self
                .function
                .and_then(|function| function.params.get(*index))
                .is_some_and(|param| param.is_mut),
            ResolvedName::LambdaParam { lambda, index } => self
                .lambdas
                .iter()
                .rev()
                .find(|ctx| ctx.expr == *lambda)
                .and_then(|ctx| ctx.param_mutability.get(*index))
                .copied()
                .unwrap_or(false),
            _ => false,
        }
    }
}

pub(crate) struct LambdaCtx {
    pub(crate) expr: ExprId,
    pub(crate) params: Vec<Type>,
    pub(crate) param_mutability: Vec<bool>,
    pub(crate) is_move: bool,
    pub(crate) outer_patterns: HashSet<PatternBindingId>,
    pub(crate) captures: Vec<LambdaCapture>,
}

/// One binding introduced by a pattern — from a `let`, a `match` arm, or a
/// `for` loop. `is_mut` comes from `mut` on the binding itself.
#[derive(Debug, Clone)]
pub(crate) struct BindingInfo {
    pub(crate) ty: Type,
    pub(crate) id: PatternBindingId,
    pub(crate) is_mut: bool,
}

/// Scoped name → binding info. Every local variable lives here, because `let`
/// bindings are pattern bindings too.
#[derive(Debug, Default)]
pub(crate) struct ScopedBindings {
    scopes: Vec<HashMap<String, BindingInfo>>,
}

impl ScopedBindings {
    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    pub(crate) fn insert(&mut self, name: String, ty: Type, id: PatternBindingId, is_mut: bool) {
        if self.scopes.is_empty() {
            self.push_scope();
        }
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name, BindingInfo { ty, id, is_mut });
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Type> {
        self.lookup(name).map(|binding| &binding.ty)
    }

    pub(crate) fn lookup(&self, name: &str) -> Option<&BindingInfo> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name))
    }

    pub(crate) fn set_type(&mut self, id: PatternBindingId, ty: Type) {
        if let Some(binding) = self
            .scopes
            .iter_mut()
            .rev()
            .flat_map(|scope| scope.values_mut())
            .find(|binding| binding.id == id)
        {
            binding.ty = ty;
        }
    }

    /// Whether `id` names a binding that was declared `mut`.
    pub(crate) fn is_mut(&self, id: PatternBindingId) -> bool {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.values())
            .find(|binding| binding.id == id)
            .is_some_and(|binding| binding.is_mut)
    }

    pub(crate) fn ids(&self) -> HashSet<PatternBindingId> {
        self.scopes
            .iter()
            .flat_map(|scope| scope.values().map(|binding| binding.id))
            .collect()
    }
}
