use std::collections::HashMap;

use hir::{
    body::{Body, BodyId, StmtId},
    item_tree::{FunctionId, HirFunction},
};

use crate::types::Type;

pub(crate) struct BodyCtx<'a> {
    pub(crate) body_id: BodyId,
    pub(crate) body: &'a Body,
    pub(crate) function: &'a HirFunction,
    pub(crate) return_ty: Type,
    pub(crate) locals: HashMap<StmtId, Type>,
    pub(crate) bindings: ScopedBindings,
}

impl<'a> BodyCtx<'a> {
    pub(crate) fn new(
        body_id: BodyId,
        body: &'a Body,
        _fid: FunctionId,
        function: &'a HirFunction,
        return_ty: Type,
    ) -> Self {
        Self {
            body_id,
            body,
            function,
            return_ty,
            locals: HashMap::new(),
            bindings: ScopedBindings::default(),
        }
    }

    pub(crate) fn push_scope(&mut self) {
        self.bindings.push_scope();
    }

    pub(crate) fn pop_scope(&mut self) {
        self.bindings.pop_scope();
    }
}

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
}
