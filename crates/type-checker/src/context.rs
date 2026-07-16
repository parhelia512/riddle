use std::collections::{HashMap, HashSet};

use hir::{
    body::{Body, BodyId, ExprId, SourceMap, StmtId},
    item_tree::{FunctionId, HirFunction},
};
use rowan::TextRange;

use crate::{
    result::{ClosureKind, LambdaCapture},
    types::Type,
};

pub(crate) struct BodyCtx<'a> {
    pub(crate) body_id: BodyId,
    pub(crate) body: &'a Body,
    pub(crate) function_id: FunctionId,
    pub(crate) function: &'a HirFunction,
    pub(crate) return_ty: Type,
    pub(crate) generic_params: HashMap<String, Type>,
    pub(crate) locals: HashMap<StmtId, (Type, bool)>,
    pub(crate) local_closures: HashMap<StmtId, ClosureKind>,
    pub(crate) bindings: ScopedBindings,
    pub(crate) loop_depth: usize,
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
            function_id,
            function,
            return_ty,
            generic_params,
            locals: HashMap::new(),
            local_closures: HashMap::new(),
            bindings: ScopedBindings::default(),
            loop_depth: 0,
            lambdas: Vec::new(),
            source_map: &body.source_map,
        }
    }

    pub(crate) fn push_scope(&mut self) {
        self.bindings.push_scope();
    }

    pub(crate) fn pop_scope(&mut self) {
        self.bindings.pop_scope();
    }

    pub(crate) fn expr_range(&self, id: hir::body::ExprId) -> Option<TextRange> {
        self.source_map.expr_ranges.get(&id).copied()
    }

    pub(crate) fn stmt_range(&self, id: hir::body::StmtId) -> Option<TextRange> {
        self.source_map.stmt_ranges.get(&id).copied()
    }

    pub(crate) fn pat_range(&self, id: hir::body::PatId) -> Option<TextRange> {
        self.source_map.pat_ranges.get(&id).copied()
    }
}

pub(crate) struct LambdaCtx {
    pub(crate) expr: ExprId,
    pub(crate) params: Vec<Type>,
    pub(crate) outer_locals: HashSet<StmtId>,
    pub(crate) binding_depth: usize,
    pub(crate) captures: Vec<LambdaCapture>,
}

/// Scoped name → type bindings (from `match` patterns, `if let`, etc.).
#[derive(Debug, Default)]
pub(crate) struct ScopedBindings {
    scopes: Vec<HashMap<String, Type>>,
}

impl ScopedBindings {
    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    pub(crate) fn insert(&mut self, name: String, ty: Type) {
        if self.scopes.is_empty() {
            self.push_scope();
        }
        self.scopes.last_mut().unwrap().insert(name, ty);
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Type> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name))
    }

    pub(crate) fn depth(&self) -> usize {
        self.scopes.len()
    }

    pub(crate) fn is_before(&self, name: &str, depth: usize) -> bool {
        self.scopes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, scope)| scope.contains_key(name).then_some(index))
            .is_some_and(|index| index < depth)
    }
}
