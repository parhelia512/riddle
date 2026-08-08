use super::{
    BinaryOp, BodyCtx, FloatTy, HashMap, HashSet, HirAssocTypeConstraint, HirGenericBound,
    HirTypeRef, IntTy, LangItem, TraitBound, TraitId, Type, TypeChecker, bound_target_param,
    builtin_callable_kind,
};

impl TypeChecker<'_> {
    pub(crate) fn check_type_bounds(
        &mut self,
        ctx: &BodyCtx<'_>,
        ty: &Type,
        span: Option<rowan::TextRange>,
    ) {
        self.expect_sized_value(ty, span);
        self.check_type_bounds_inner(ctx, ty, span);
    }

    pub(crate) fn check_type_bounds_inner(
        &mut self,
        ctx: &BodyCtx<'_>,
        ty: &Type,
        span: Option<rowan::TextRange>,
    ) {
        match ty {
            Type::Slice(inner) => {
                self.expect_sized_value(inner, span);
                self.check_type_bounds_inner(ctx, inner, span);
            }
            Type::Ref(inner, _) | Type::Ptr { inner, .. } | Type::Array(inner, _) => {
                self.check_type_bounds_inner(ctx, inner, span);
            }
            Type::Tuple(elements) => {
                for element in elements {
                    self.check_type_bounds_inner(ctx, element, span);
                }
            }
            Type::CallableConstraint(signature)
            | Type::Closure { signature, .. }
            | Type::OpaqueCallable { signature, .. } => {
                for param in &signature.params {
                    self.check_type_bounds_inner(ctx, param, span);
                }
                self.check_type_bounds_inner(ctx, &signature.ret, span);
            }
            Type::FunctionItem { args, .. } => {
                for arg in args {
                    self.check_type_bounds_inner(ctx, arg, span);
                }
            }
            Type::Struct(struct_id, args) => {
                for arg in args {
                    self.check_type_bounds_inner(ctx, arg, span);
                }
                let strukt = self.hir.item_tree.structs[*struct_id].clone();
                let subst = strukt
                    .generics
                    .iter()
                    .chain(strukt.const_generics.iter())
                    .zip(args.iter())
                    .map(|(name, ty)| (name.0.clone(), ty.clone()))
                    .collect::<HashMap<_, _>>();
                self.check_item_bounds(ctx, &strukt.name.0, &strukt.generic_bounds, &subst, span);
            }
            Type::Enum(enum_id, args) => {
                for arg in args {
                    self.check_type_bounds_inner(ctx, arg, span);
                }
                let enum_data = self.hir.item_tree.enums[*enum_id].clone();
                let subst = enum_data
                    .generics
                    .iter()
                    .chain(enum_data.const_generics.iter())
                    .zip(args.iter())
                    .map(|(name, ty)| (name.0.clone(), ty.clone()))
                    .collect::<HashMap<_, _>>();
                self.check_item_bounds(
                    ctx,
                    &enum_data.name.0,
                    &enum_data.generic_bounds,
                    &subst,
                    span,
                );
            }
            Type::Param(_)
            | Type::Const(_)
            | Type::Unknown
            | Type::Error
            | Type::InferInt
            | Type::InferFloat
            | Type::InferVar(_)
            | Type::Int(_)
            | Type::Float(_)
            | Type::Bool
            | Type::Str
            | Type::Char
            | Type::Unit
            | Type::Never => {}
        }
    }

    pub(super) fn check_item_bounds(
        &mut self,
        ctx: &BodyCtx<'_>,
        item_name: &str,
        bounds: &[HirGenericBound],
        subst: &HashMap<String, Type>,
        span: Option<rowan::TextRange>,
    ) {
        for bound in bounds {
            let actual = self.lower_type_ref_with_params_at(
                &bound.target_ty,
                subst,
                Some(bound.target_range),
            );
            if actual.is_unknown_like() {
                continue;
            }
            if let Some(kind) = builtin_callable_kind(&bound.trait_ty) {
                let Some(signature) = bound.callable.as_ref() else {
                    self.diagnostic(
                        "E0047",
                        format!("{} bound requires a callable signature", kind.as_str()),
                        Some(bound.trait_range),
                    );
                    continue;
                };
                let required = self.lower_hir_callable_signature(
                    signature,
                    kind,
                    subst,
                    Some(bound.trait_range),
                );
                self.check_callable_requirement(ctx, &actual, &required, item_name, span);
                continue;
            }
            let Some(trait_id) = self.resolve_trait_ref(&bound.trait_ty) else {
                self.diagnostic(
                    "E0023",
                    format!(
                        "generic bound references unknown trait `{}`",
                        bound.trait_ty.display()
                    ),
                    Some(bound.trait_range),
                );
                continue;
            };
            if !self.type_satisfies_bound(
                ctx,
                &actual,
                trait_id,
                &bound.trait_ty,
                &bound.assoc_constraints,
                subst,
            ) {
                let trait_name = self.hir.item_tree.traits[trait_id].name.0.clone();
                self.diagnostic(
                    "E0035",
                    format!(
                        "type `{}` does not satisfy bound `{}` for `{}`",
                        actual.display(self.hir),
                        trait_name,
                        item_name
                    ),
                    span,
                );
            }
        }
    }

    pub(super) fn current_generic_bounds(&self, ctx: &BodyCtx<'_>) -> Vec<HirGenericBound> {
        let mut bounds = self
            .hir
            .item_tree
            .impls
            .iter()
            .find_map(|(_, imp)| {
                (ctx.function_id
                    .is_some_and(|function| imp.methods.contains(&function))
                    || ctx
                        .const_id
                        .is_some_and(|konst| imp.consts.contains(&konst)))
                .then(|| imp.generic_bounds.clone())
            })
            .unwrap_or_default();
        if let Some(function) = ctx.function {
            bounds.extend(function.generic_bounds.clone());
        }
        bounds
    }

    pub(super) fn current_trait_assumptions(&mut self, ctx: &BodyCtx<'_>) -> Vec<TraitBound> {
        let bounds = self.current_generic_bounds(ctx);
        self.lower_trait_env_bounds(&bounds, &ctx.generic_params)
    }

    pub(super) fn type_satisfies_bound(
        &mut self,
        ctx: &BodyCtx<'_>,
        actual: &Type,
        trait_id: TraitId,
        trait_ref: &HirTypeRef,
        assoc_constraints: &[HirAssocTypeConstraint],
        subst: &HashMap<String, Type>,
    ) -> bool {
        let actual = match actual {
            Type::InferInt => Type::Int(IntTy::I32),
            Type::InferFloat => Type::Float(crate::types::FloatTy::F64),
            actual => actual.clone(),
        };
        if actual.is_unknown_like() {
            return true;
        }
        let trait_args = self.trait_ref_args(trait_id, trait_ref, &actual, subst, None);
        if let Type::Param(param) = &actual {
            return self.param_has_trait_bound(
                ctx,
                param,
                trait_id,
                &trait_args,
                assoc_constraints,
                subst,
            );
        }
        let assumptions = self.current_trait_assumptions(ctx);
        self.result.trait_env.type_implements_with_args_assuming(
            &actual,
            trait_id,
            &trait_args,
            &assumptions,
        ) && self.assoc_constraints_match(&actual, trait_id, &trait_args, assoc_constraints, subst)
    }

    pub(super) fn type_has_lang_trait_with_args(
        &mut self,
        ctx: &BodyCtx<'_>,
        ty: &Type,
        trait_args: &[Type],
        item: LangItem,
    ) -> bool {
        let Some(trait_id) = self.result.trait_env.lang_items.get(item) else {
            return false;
        };
        let generic_count = self.hir.item_tree.traits[trait_id].generics.len();
        let trait_args = &trait_args[..trait_args.len().min(generic_count)];
        self.type_has_trait_id_with_args(ctx, ty, trait_id, trait_args)
    }

    pub(super) fn type_has_trait_id(
        &mut self,
        ctx: &BodyCtx<'_>,
        ty: &Type,
        trait_id: TraitId,
    ) -> bool {
        self.type_has_trait_id_with_args(ctx, ty, trait_id, &[])
    }

    pub(super) fn type_has_trait_id_with_args(
        &mut self,
        ctx: &BodyCtx<'_>,
        ty: &Type,
        trait_id: TraitId,
        trait_args: &[Type],
    ) -> bool {
        if ty.is_unknown_like() {
            return true;
        }
        if let Type::Param(param) = ty {
            return self.param_has_trait_bound(
                ctx,
                param,
                trait_id,
                trait_args,
                &[],
                &ctx.generic_params,
            );
        }
        let assumptions = self.current_trait_assumptions(ctx);
        self.result.trait_env.type_implements_with_args_assuming(
            ty,
            trait_id,
            trait_args,
            &assumptions,
        )
    }

    pub(super) fn associated_type_for(
        &mut self,
        ctx: &BodyCtx<'_>,
        ty: &Type,
        trait_id: TraitId,
        name: &str,
    ) -> Option<Type> {
        if let Type::Param(param) = ty {
            return self
                .current_generic_bounds(ctx)
                .into_iter()
                .find_map(|bound| {
                    if bound_target_param(&bound).is_none_or(|name| name != *param) {
                        return None;
                    }
                    let bound_trait = self.resolve_trait_ref(&bound.trait_ty)?;
                    self.trait_implies(bound_trait, trait_id)
                        .then(|| self.bound_assoc_type(ctx, &bound, name))
                        .flatten()
                });
        }
        self.result.trait_env.associated_type(ty, trait_id, name)
    }

    pub(super) fn param_has_trait_bound(
        &mut self,
        ctx: &BodyCtx<'_>,
        param: &str,
        required_trait: TraitId,
        required_args: &[Type],
        required_assoc: &[HirAssocTypeConstraint],
        subst: &HashMap<String, Type>,
    ) -> bool {
        self.current_generic_bounds(ctx).into_iter().any(|bound| {
            if bound_target_param(&bound).is_none_or(|name| name != param) {
                return false;
            }
            let Some(bound_trait) = self.resolve_trait_ref(&bound.trait_ty) else {
                return false;
            };
            if !self.trait_implies(bound_trait, required_trait) {
                return false;
            }
            if bound_trait == required_trait && !required_args.is_empty() {
                let self_ty = Type::Param(param.to_string());
                let bound_args = self.trait_ref_args(
                    bound_trait,
                    &bound.trait_ty,
                    &self_ty,
                    &ctx.generic_params,
                    Some(bound.trait_range),
                );
                if bound_args.len() != required_args.len()
                    || !bound_args
                        .iter()
                        .zip(required_args)
                        .all(|(actual, required)| Self::bound_types_match(required, actual))
                {
                    return false;
                }
            }
            required_assoc.iter().all(|required| {
                let expected =
                    self.lower_type_ref_with_params_at(&required.ty, subst, Some(required.range));
                self.bound_assoc_type(ctx, &bound, &required.name.0)
                    .is_some_and(|actual| Self::bound_types_match(&expected, &actual))
            })
        })
    }

    pub(super) fn assoc_constraints_match(
        &mut self,
        actual: &Type,
        trait_id: TraitId,
        trait_args: &[Type],
        assoc_constraints: &[HirAssocTypeConstraint],
        subst: &HashMap<String, Type>,
    ) -> bool {
        assoc_constraints.iter().all(|constraint| {
            let expected =
                self.lower_type_ref_with_params_at(&constraint.ty, subst, Some(constraint.range));
            self.result
                .trait_env
                .associated_type_with_args(actual, trait_id, trait_args, &constraint.name.0)
                .is_some_and(|actual| self.unify_types(&expected, &actual))
        })
    }

    pub(super) fn impl_bounds_satisfied(
        &mut self,
        imp: &hir::item_tree::HirImpl,
        subst: &HashMap<String, Type>,
        assumptions: &[TraitBound],
    ) -> bool {
        imp.generic_bounds.iter().all(|bound| {
            let actual = self.lower_type_ref_with_params_at(
                &bound.target_ty,
                subst,
                Some(bound.target_range),
            );
            let Some(trait_id) = self.resolve_trait_ref(&bound.trait_ty) else {
                return false;
            };
            let trait_args = self.trait_ref_args(
                trait_id,
                &bound.trait_ty,
                &actual,
                subst,
                Some(bound.trait_range),
            );
            self.result.trait_env.type_implements_with_args_assuming(
                &actual,
                trait_id,
                &trait_args,
                assumptions,
            ) && self.assoc_constraints_match(
                &actual,
                trait_id,
                &trait_args,
                &bound.assoc_constraints,
                subst,
            )
        })
    }

    pub(super) fn bound_assoc_type(
        &mut self,
        ctx: &BodyCtx<'_>,
        bound: &HirGenericBound,
        name: &str,
    ) -> Option<Type> {
        bound
            .assoc_constraints
            .iter()
            .find(|constraint| constraint.name.0 == name)
            .map(|constraint| {
                self.lower_type_ref_with_params_at(
                    &constraint.ty,
                    &ctx.generic_params,
                    Some(constraint.range),
                )
            })
    }

    pub(super) fn trait_implies(&self, actual: TraitId, required: TraitId) -> bool {
        self.supertrait_reaches(actual, required, &mut HashSet::new())
    }

    pub(crate) fn supertrait_reaches(
        &self,
        actual: TraitId,
        required: TraitId,
        visited: &mut HashSet<TraitId>,
    ) -> bool {
        if actual == required {
            return true;
        }
        if !visited.insert(actual) {
            return false;
        }
        self.hir.item_tree.traits[actual]
            .supertraits
            .iter()
            .filter_map(|bound| self.resolve_trait_ref(&bound.trait_ty))
            .any(|supertrait| self.supertrait_reaches(supertrait, required, visited))
    }

    pub(super) fn bound_types_match(expected: &Type, actual: &Type) -> bool {
        expected.is_unknown_like()
            || actual.is_unknown_like()
            || expected == actual
            || Self::numeric_assignable(expected, actual)
    }

    pub(super) fn is_builtin_equality(lhs_ty: &Type, rhs_ty: &Type) -> bool {
        Self::join_numeric_types(lhs_ty, rhs_ty).is_some()
            || matches!(
                (lhs_ty, rhs_ty),
                (Type::Bool, Type::Bool)
                    | (Type::Char, Type::Char)
                    | (Type::Str, Type::Str)
                    | (Type::Unit, Type::Unit)
            )
            || matches!(
                (lhs_ty, rhs_ty),
                (Type::Ref(lhs, false), Type::Ref(rhs, false))
                    if matches!(lhs.as_ref(), Type::Str)
                        && matches!(rhs.as_ref(), Type::Str)
            )
    }

    pub(super) fn is_builtin_binary_operator(op: BinaryOp, lhs_ty: &Type, rhs_ty: &Type) -> bool {
        match op {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div => {
                Self::join_numeric_types(lhs_ty, rhs_ty).is_some()
            }
            BinaryOp::Mod | BinaryOp::Shl | BinaryOp::Shr => {
                lhs_ty.is_integer() && rhs_ty.is_integer()
            }
            BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor => {
                (lhs_ty == &Type::Bool && rhs_ty == &Type::Bool)
                    || Self::join_numeric_types(lhs_ty, rhs_ty).is_some()
            }
            BinaryOp::Eq | BinaryOp::Neq => Self::is_builtin_equality(lhs_ty, rhs_ty),
            BinaryOp::Lt | BinaryOp::Gt | BinaryOp::LtEq | BinaryOp::GtEq => {
                Self::is_builtin_ordering(lhs_ty, rhs_ty)
            }
            _ => false,
        }
    }

    pub(super) fn default_inferred_numeric_type(ty: &Type) -> Type {
        match ty {
            Type::InferInt => Type::Int(IntTy::I32),
            Type::InferFloat => Type::Float(FloatTy::F64),
            _ => ty.clone(),
        }
    }

    pub(super) fn is_builtin_ordering(lhs_ty: &Type, rhs_ty: &Type) -> bool {
        lhs_ty.is_ordered_scalar()
            && rhs_ty.is_ordered_scalar()
            && (*lhs_ty == Type::Char) == (*rhs_ty == Type::Char)
    }
}
