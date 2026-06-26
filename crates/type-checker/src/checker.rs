use hir::{
    HirFile,
    body::{Body, BodyId, Expr},
    item_tree::{FunctionId, HirFunction},
};

use crate::{
    context::BodyCtx,
    result::{Diagnostic, TypeCheckResult},
    types::{FloatTy, IntTy, Type},
};

pub struct TypeChecker<'a> {
    pub(crate) hir: &'a HirFile,
    pub(crate) result: TypeCheckResult,
}

pub fn check_hir(hir: &HirFile) -> TypeCheckResult {
    TypeChecker::new(hir).check()
}

impl<'a> TypeChecker<'a> {
    pub fn new(hir: &'a HirFile) -> Self {
        Self {
            hir,
            result: TypeCheckResult::default(),
        }
    }

    pub fn check(mut self) -> TypeCheckResult {
        self.check_traits();
        self.check_impls();
        self.build_trait_env();
        self.check_function_bodies();
        self.result
    }

    // ── Trait environment construction ──────────────────────────────

    pub(crate) fn build_trait_env(&mut self) {
        for (tid, tr) in self.hir.item_tree.traits.iter() {
            if tr.name.0 == "Copy" {
                self.result.trait_env.set_copy_trait(tid);
                break;
            }
        }
        for (_, imp) in self.hir.item_tree.impls.iter() {
            let Some(trait_ty) = &imp.trait_ty else {
                continue;
            };
            let Some(trait_id) = self.resolve_trait_ref(trait_ty) else {
                continue;
            };
            let self_ty = self.lower_type_ref(&imp.self_ty);
            self.result.trait_env.insert_impl(trait_id, self_ty);
        }
    }

    // ── Function body checking ──────────────────────────────────────

    pub(crate) fn check_function_bodies(&mut self) {
        for (fid, function) in self.hir.item_tree.functions.iter() {
            if let Some(body_id) = self.hir.function_bodies.get(&fid).copied() {
                self.check_function(fid, function, body_id);
            }
        }
    }

    pub(crate) fn check_function(
        &mut self,
        fid: FunctionId,
        function: &HirFunction,
        body_id: BodyId,
    ) {
        let body = &self.hir.bodies[body_id];
        let return_ty = function
            .ret_type
            .as_ref()
            .map(|ty| self.lower_type_ref(ty))
            .unwrap_or(Type::Unit);
        let mut ctx = BodyCtx::new(body_id, body, fid, function, return_ty.clone());
        let actual = self.check_expr_expected(&mut ctx, body.root_block, &return_ty);

        if self.block_has_tail(body, body.root_block) && !actual.is_never() {
            self.expect_assignable(&return_ty, &actual, "function return type");
        }
    }

    pub(crate) fn block_has_tail(&self, body: &Body, expr: hir::body::ExprId) -> bool {
        matches!(&body.exprs[expr], Expr::Block { tail: Some(_), .. })
    }

    pub(crate) fn join_branch_types(&mut self, lhs: Type, rhs: Type, context: &str) -> Type {
        if lhs.is_never() {
            return rhs;
        }
        if rhs.is_never() {
            return lhs;
        }
        if lhs.is_unknown_like() {
            return rhs;
        }
        if rhs.is_unknown_like() {
            return lhs;
        }
        if let Some(ty) = self.join_numeric_types(&lhs, &rhs) {
            ty
        } else if lhs == rhs {
            lhs
        } else {
            self.diagnostic(format!(
                "{} have incompatible types: {} and {}",
                context,
                lhs.display(self.hir),
                rhs.display(self.hir)
            ));
            Type::Error
        }
    }

    pub(crate) fn expect_numeric(&mut self, ty: &Type, context: &str) {
        if ty.is_unknown_like() || ty.is_numeric() {
            return;
        }
        self.diagnostic(format!(
            "{} must be numeric, got {}",
            context,
            ty.display(self.hir)
        ));
    }

    pub(crate) fn expect_assignable(&mut self, expected: &Type, actual: &Type, context: &str) {
        if expected.is_unknown_like()
            || actual.is_unknown_like()
            || expected == actual
            || self.numeric_assignable(expected, actual)
        {
            return;
        }
        if actual.is_never() {
            return;
        }
        self.diagnostic(format!(
            "{} type mismatch: expected {}, got {}",
            context,
            expected.display(self.hir),
            actual.display(self.hir)
        ));
    }

    pub(crate) fn diagnostic(&mut self, message: impl Into<String>) {
        self.result.diagnostics.push(Diagnostic {
            message: message.into(),
        });
    }

    pub(crate) fn int_literal_type(
        &mut self,
        suffix: Option<&str>,
        expected: Option<&Type>,
    ) -> Type {
        if let Some(suffix) = suffix {
            return IntTy::parse(suffix).map(Type::Int).unwrap_or_else(|| {
                self.diagnostic(format!("unknown integer literal suffix `{suffix}`"));
                Type::Error
            });
        }
        match expected {
            Some(Type::Int(ty)) => Type::Int(*ty),
            Some(Type::InferInt) => Type::InferInt,
            _ => Type::InferInt,
        }
    }

    pub(crate) fn float_literal_type(
        &mut self,
        suffix: Option<&str>,
        expected: Option<&Type>,
    ) -> Type {
        if let Some(suffix) = suffix {
            return FloatTy::parse(suffix).map(Type::Float).unwrap_or_else(|| {
                self.diagnostic(format!("unknown float literal suffix `{suffix}`"));
                Type::Error
            });
        }
        match expected {
            Some(Type::Float(ty)) => Type::Float(*ty),
            Some(Type::InferFloat) => Type::InferFloat,
            _ => Type::InferFloat,
        }
    }

    pub(crate) fn numeric_assignable(&self, expected: &Type, actual: &Type) -> bool {
        matches!(
            (expected, actual),
            (Type::Int(_), Type::InferInt)
                | (Type::InferInt, Type::Int(_))
                | (Type::Float(_), Type::InferFloat)
                | (Type::InferFloat, Type::Float(_))
                | (Type::InferInt, Type::InferInt)
                | (Type::InferFloat, Type::InferFloat)
        )
    }

    pub(crate) fn join_numeric_types(&self, lhs: &Type, rhs: &Type) -> Option<Type> {
        match (lhs, rhs) {
            (Type::Int(a), Type::Int(b)) if a == b => Some(Type::Int(*a)),
            (Type::Float(a), Type::Float(b)) if a == b => Some(Type::Float(*a)),
            (Type::Int(ty), Type::InferInt) | (Type::InferInt, Type::Int(ty)) => {
                Some(Type::Int(*ty))
            }
            (Type::Float(ty), Type::InferFloat) | (Type::InferFloat, Type::Float(ty)) => {
                Some(Type::Float(*ty))
            }
            (Type::InferInt, Type::InferInt) => Some(Type::InferInt),
            (Type::InferFloat, Type::InferFloat) => Some(Type::InferFloat),
            _ => None,
        }
    }
}
