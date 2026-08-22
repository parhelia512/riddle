use super::{
    BodyCtx, Expr, ExprId, FunctionId, HashMap, HashSet, HirFunction, HirTypeRef, LangItem, Name,
    PatternBindingId, PendingGenericCall, ResolvedMethod, ResolvedName, TraitId, TraitMethodCall,
    Type, TypeChecker, UnaryOp, ValueUse, bound_target_param, callable_signature_type,
    collect_subst, expected_has_param, generic_param_map_with_consts, method_is_visible_for_owner,
    record_generic_arg_spans, substitute_type, type_has_unresolved_inference,
};

impl TypeChecker<'_> {
    pub(super) fn check_method_call(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        call: (ExprId, ExprId, &[ExprId], &[HirTypeRef]),
        expected_span: (Option<&Type>, Option<rowan::TextRange>),
    ) -> Type {
        let (expr_id, callee, args, type_args) = call;
        let (expected, span) = expected_span;
        let Expr::FieldAccess { base, field } = &ctx.body.exprs[callee] else {
            unreachable!("method call callee must be a field access");
        };
        let base = *base;
        let method_name = field.clone();
        let base_ty = self.check_place_expr(ctx, base);
        let method = match self.find_method(ctx, &base_ty, &method_name) {
            Ok(Some(method)) => method,
            Ok(None) => {
                for arg in args {
                    self.check_expr(ctx, *arg);
                    self.record_value_use(ctx, *arg, ValueUse::Move);
                }
                if !base_ty.is_unknown_like() {
                    self.diagnostic(
                        "E0013",
                        format!(
                            "unknown method `{}` on type {}",
                            method_name.0,
                            base_ty.display(self.hir)
                        ),
                        span,
                    );
                }
                return Type::Error;
            }
            Err(private_method) => {
                for arg in args {
                    self.check_expr(ctx, *arg);
                    self.record_value_use(ctx, *arg, ValueUse::Move);
                }
                let owner = match &base_ty {
                    Type::Struct(id, _) => Some(*id),
                    Type::Ref(inner, _) => match inner.as_ref() {
                        Type::Struct(id, _) => Some(*id),
                        _ => None,
                    },
                    _ => None,
                };
                let owner = owner.map_or_else(
                    || format!("type `{}`", base_ty.display(self.hir)),
                    |id| format!("struct `{}`", self.hir.item_tree.structs[id].name.0),
                );
                self.diagnostic(
                    "E0054",
                    format!(
                        "method `{}` of {owner} is private",
                        self.hir.item_tree.functions[private_method].name.0
                    ),
                    span,
                );
                return Type::Error;
            }
        };

        self.check_resolved_method_call(
            ctx,
            (expr_id, callee, base, args, type_args),
            (expected, span),
            (base_ty, method),
        )
    }

    fn check_resolved_method_call(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        call: (ExprId, ExprId, ExprId, &[ExprId], &[HirTypeRef]),
        expected_span: (Option<&Type>, Option<rowan::TextRange>),
        resolved: (Type, ResolvedMethod),
    ) -> Type {
        let (expr_id, callee, base, args, type_args) = call;
        let (expected, span) = expected_span;
        let (base_ty, method) = resolved;
        if method.function.is_unsafe {
            self.require_unsafe(ctx, "calling an unsafe function", span);
        }
        let (impl_generics, impl_const_generics) = if method.from_trait_bound {
            (Vec::new(), Vec::new())
        } else {
            (
                self.impl_generic_names(method.fid),
                self.impl_const_generic_names(method.fid),
            )
        };
        let mut params = method.subst.clone();
        params.extend(generic_param_map_with_consts(
            method
                .function
                .generics
                .iter()
                .chain(method.function.implicit_generics.iter())
                .map(|name| name.0.as_str()),
            method
                .function
                .const_generics
                .iter()
                .map(|name| name.0.as_str()),
        ));
        let mut subst = HashMap::new();
        self.apply_method_type_args(ctx, &method, type_args, &mut subst, span);
        self.seed_type_inference(
            method
                .function
                .generics
                .iter()
                .chain(&method.function.implicit_generics)
                .map(|name| name.0.as_str()),
            &mut subst,
        );
        self.record_method_metadata(ctx, expr_id, callee, &base_ty, &method, span);
        let generic_arg_spans = self.check_method_arguments(
            ctx,
            (base, args),
            &method,
            (&base_ty, &params, &mut subst),
            span,
        );
        if !method.from_trait_bound
            && (!impl_generics.is_empty()
                || !impl_const_generics.is_empty()
                || !method.function.generics.is_empty()
                || !method.function.implicit_generics.is_empty()
                || !method.function.const_generics.is_empty())
        {
            self.record_method_generic_call(
                ctx,
                (callee, method.fid),
                &method,
                (&impl_generics, &impl_const_generics),
                (&subst, generic_arg_spans),
                span,
            );
        }
        let return_ty = method.function.ret_type.as_ref().map_or(Type::Unit, |ty| {
            substitute_type(
                &self.lower_type_ref_with_params_at(
                    ty,
                    &params,
                    method
                        .function
                        .ret_type_range
                        .or(Some(method.function.name_range)),
                ),
                &subst,
            )
        });
        if let Some(expected) = expected {
            let _ = self.unify_types(&return_ty, expected);
            self.last_occurs_error = None;
        }
        return_ty
    }

    fn apply_method_type_args(
        &mut self,
        ctx: &BodyCtx<'_>,
        method: &ResolvedMethod,
        type_args: &[HirTypeRef],
        subst: &mut HashMap<String, Type>,
        span: Option<rowan::TextRange>,
    ) {
        if type_args.is_empty() {
            return;
        }
        if type_args.len() != method.function.generics.len() {
            self.diagnostic(
                "E0005",
                format!(
                    "method `{}` expects {} type argument(s), got {}",
                    method.function.name.0,
                    method.function.generics.len(),
                    type_args.len()
                ),
                span,
            );
        }
        for (param_name, type_arg) in method.function.generics.iter().zip(type_args) {
            let lowered = self.lower_type_ref_with_params_at(type_arg, &ctx.generic_params, span);
            subst.insert(param_name.0.clone(), lowered);
        }
    }

    fn record_method_metadata(
        &mut self,
        ctx: &BodyCtx<'_>,
        expr_id: ExprId,
        callee: ExprId,
        base_ty: &Type,
        method: &ResolvedMethod,
        span: Option<rowan::TextRange>,
    ) {
        if method.trait_id.is_some_and(|trait_id| {
            self.result.trait_env.lang_items.get(LangItem::Drop) == Some(trait_id)
        }) {
            self.diagnostic(
                "E0056",
                "explicit destructor calls are not allowed; use `drop(value)` instead",
                span,
            );
        }
        if method.from_trait_bound {
            self.result
                .expr_types
                .insert((ctx.body_id, callee), base_ty.clone());
        } else {
            self.result.expr_types.insert(
                (ctx.body_id, callee),
                Type::FunctionItem {
                    function: method.fid,
                    args: Vec::new(),
                },
            );
        }
        if let Some(trait_id) = method.trait_id {
            let call = TraitMethodCall {
                trait_id,
                method: method.function.name.0.clone(),
                dynamic: matches!(
                    base_ty,
                    Type::Ref(inner, _) if matches!(inner.as_ref(), Type::DynTrait { .. })
                ),
            };
            self.result
                .trait_method_calls
                .insert((ctx.body_id, callee), call.clone());
            if method.from_trait_bound {
                self.result
                    .trait_method_calls
                    .insert((ctx.body_id, expr_id), call);
            }
        }
    }

    fn check_method_arguments(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        call: (ExprId, &[ExprId]),
        method: &ResolvedMethod,
        types: (&Type, &HashMap<String, Type>, &mut HashMap<String, Type>),
        span: Option<rowan::TextRange>,
    ) -> HashMap<String, rowan::TextRange> {
        let (base, args) = call;
        let (base_ty, params, subst) = types;
        let receiver_count = usize::from(!method.function.params.is_empty());
        let expected_arg_count = method.function.params.len().saturating_sub(receiver_count);
        if args.len() != expected_arg_count {
            self.diagnostic(
                "E0005",
                format!(
                    "method `{}` expects {} argument(s), got {}",
                    method.function.name.0,
                    expected_arg_count,
                    args.len()
                ),
                span,
            );
        }
        if let Some(receiver) = method.function.params.first() {
            let expected = self.lower_type_ref_with_params_at(
                &receiver.ty,
                &method.subst,
                Some(receiver.ty_range),
            );
            let actual = Self::receiver_argument_type(base_ty, &expected);
            if matches!(expected, Type::Ref(_, true)) && !matches!(base_ty, Type::Ref(_, true)) {
                self.check_assign_mut(ctx, base, ctx.expr_range(base));
            }
            self.record_value_use(ctx, base, Self::hir_parameter_value_use(&receiver.ty));
            self.expect_assignable(&expected, &actual, "method receiver", ctx.expr_range(base));
        }
        let mut generic_arg_spans = HashMap::new();
        for (index, arg) in args.iter().enumerate() {
            let Some(param) = method.function.params.get(index + receiver_count) else {
                self.check_expr(ctx, *arg);
                self.record_value_use(ctx, *arg, ValueUse::Move);
                continue;
            };
            let pattern =
                self.lower_type_ref_with_params_at(&param.ty, params, Some(param.ty_range));
            record_generic_arg_spans(
                &pattern,
                params,
                ctx.expr_range(*arg),
                &mut generic_arg_spans,
            );
            let expected = substitute_type(&pattern, subst);
            let callable_expected = self
                .callable_bound_for_function_type(&method.function, &pattern, params)
                .map(callable_signature_type)
                .map(|ty| substitute_type(&ty, subst));
            let actual = match callable_expected.as_ref() {
                Some(expected) => self.check_expr_expected(ctx, *arg, expected),
                None if expected_has_param(&expected) => self.check_expr(ctx, *arg),
                None => self.check_expr_expected(ctx, *arg, &expected),
            };
            if let Some(expected) = callable_expected.as_ref() {
                let _ = self.unify_types(expected, &actual);
                self.last_occurs_error = None;
            }
            collect_subst(&pattern, &actual, subst);
            let expected = substitute_type(&pattern, subst);
            self.expect_assignable(&expected, &actual, "method argument", ctx.expr_range(*arg));
            self.record_value_use(ctx, *arg, Self::hir_parameter_value_use(&param.ty));
        }
        generic_arg_spans
    }

    fn record_method_generic_call(
        &mut self,
        ctx: &BodyCtx<'_>,
        call: (ExprId, FunctionId),
        method: &ResolvedMethod,
        impl_generics: (&[String], &[String]),
        inference: (&HashMap<String, Type>, HashMap<String, rowan::TextRange>),
        span: Option<rowan::TextRange>,
    ) {
        let (callee, function_id) = call;
        let (impl_generics, impl_const_generics) = impl_generics;
        let (subst, generic_arg_spans) = inference;
        let inferred_names = method
            .function
            .generics
            .iter()
            .chain(&method.function.implicit_generics)
            .chain(&method.function.const_generics)
            .map(|name| name.0.as_str())
            .map(str::to_string)
            .collect::<Vec<_>>();
        let mut bound_subst = method.subst.clone();
        bound_subst.extend(subst.clone());
        let mut generic_args = impl_generics
            .iter()
            .map(|name| method.subst.get(name).cloned().unwrap_or(Type::Unknown))
            .collect::<Vec<_>>();
        generic_args.extend(
            method
                .function
                .generics
                .iter()
                .map(|name| subst.get(&name.0).cloned().unwrap_or(Type::Unknown)),
        );
        generic_args.extend(
            method
                .function
                .implicit_generics
                .iter()
                .map(|name| subst.get(&name.0).cloned().unwrap_or(Type::Unknown)),
        );
        generic_args.extend(
            impl_const_generics
                .iter()
                .map(|name| method.subst.get(name).cloned().unwrap_or(Type::Unknown)),
        );
        generic_args.extend(
            method
                .function
                .const_generics
                .iter()
                .map(|name| subst.get(&name.0).cloned().unwrap_or(Type::Unknown)),
        );
        self.result.generic_calls.insert(
            (ctx.body_id, callee),
            crate::result::GenericCall { args: generic_args },
        );
        self.pending_generic_calls.push(PendingGenericCall {
            body_id: ctx.body_id,
            callee,
            function: function_id,
            inferred_names,
            subst: bound_subst,
            generic_arg_spans,
            callee_span: ctx.expr_range(callee),
            span,
            kind: "method",
            caller: None,
            check_sized: false,
        });
    }

    pub(super) fn find_method(
        &mut self,
        ctx: &BodyCtx<'_>,
        receiver_ty: &Type,
        method_name: &Name,
    ) -> Result<Option<ResolvedMethod>, FunctionId> {
        if let Some(method) = self.find_inherent_method(ctx, receiver_ty, method_name)? {
            return Ok(Some(method));
        }
        if let Some(method) = self.find_trait_bound_method(ctx, receiver_ty, method_name) {
            return Ok(Some(method));
        }
        if let Some(method) = self.find_dyn_trait_method(ctx, receiver_ty, method_name) {
            return Ok(Some(method));
        }
        Ok(self.find_trait_impl_method_by_name(ctx, receiver_ty, method_name))
    }

    fn find_dyn_trait_method(
        &self,
        ctx: &BodyCtx<'_>,
        receiver_ty: &Type,
        method_name: &Name,
    ) -> Option<ResolvedMethod> {
        let Type::Ref(inner, _) = receiver_ty else {
            return None;
        };
        let Type::DynTrait { trait_id, args } = inner.as_ref() else {
            return None;
        };
        let trait_data = &self.hir.item_tree.traits[*trait_id];
        let (fid, function) = if let Some(fid) = trait_data
            .default_methods
            .iter()
            .copied()
            .find(|fid| self.hir.item_tree.functions[*fid].name == *method_name)
        {
            (fid, self.hir.item_tree.functions[fid].clone())
        } else {
            let function = trait_data
                .methods
                .iter()
                .find(|function| function.name == *method_name)
                .cloned()?;
            let fid = ctx.function_id?;
            (fid, function)
        };
        if !function
            .params
            .first()
            .is_some_and(|param| matches!(param.ty, HirTypeRef::Ref(_, _)))
            || !function.generics.is_empty()
            || !function.implicit_generics.is_empty()
            || !function.const_generics.is_empty()
            || function
                .params
                .iter()
                .skip(1)
                .any(|param| hir_type_mentions_self(&param.ty))
            || function
                .ret_type
                .as_ref()
                .is_some_and(hir_type_mentions_self)
        {
            return None;
        }
        let receiver = Type::DynTrait {
            trait_id: *trait_id,
            args: args.clone(),
        };
        let mut subst = HashMap::new();
        subst.insert("Self".into(), receiver);
        for (name, arg) in trait_data.generics.iter().zip(args) {
            subst.insert(name.0.clone(), arg.clone());
        }
        Some(ResolvedMethod {
            fid,
            function,
            subst,
            trait_id: Some(*trait_id),
            from_trait_bound: true,
        })
    }

    pub(super) fn find_inherent_method(
        &mut self,
        ctx: &BodyCtx<'_>,
        receiver_ty: &Type,
        method_name: &Name,
    ) -> Result<Option<ResolvedMethod>, FunctionId> {
        let receiver_self_ty = match receiver_ty {
            Type::Ref(inner, _) => inner.as_ref(),
            other => other,
        };
        let impls = self
            .hir
            .item_tree
            .impls
            .iter()
            .map(|(_, imp)| imp.clone())
            .collect::<Vec<_>>();

        let mut private = None;
        for imp in impls {
            if imp.trait_ty.is_some() {
                continue;
            }
            let Some(mut subst) = self.impl_subst_from_self_ty(&imp, receiver_self_ty) else {
                continue;
            };
            let assumptions = self.current_trait_assumptions(ctx);
            if !self.impl_bounds_satisfied(&imp, &subst, &assumptions) {
                continue;
            }
            subst.insert("Self".into(), receiver_self_ty.clone());
            for fid in imp.methods {
                let function = &self.hir.item_tree.functions[fid];
                if function.name == *method_name {
                    if !method_is_visible_for_owner(
                        self.hir,
                        ctx.owner_range(),
                        ctx.function_id,
                        ctx.const_id,
                        fid,
                        &function.visibility,
                    ) {
                        private.get_or_insert(fid);
                        continue;
                    }
                    return Ok(Some(ResolvedMethod {
                        fid,
                        function: function.clone(),
                        subst,
                        trait_id: None,
                        from_trait_bound: false,
                    }));
                }
            }
        }

        private.map_or(Ok(None), Err)
    }

    pub(super) fn find_trait_impl_method_by_name(
        &mut self,
        ctx: &BodyCtx<'_>,
        receiver_ty: &Type,
        method_name: &Name,
    ) -> Option<ResolvedMethod> {
        let receiver_self_ty = match receiver_ty {
            Type::Ref(inner, _) => inner.as_ref(),
            other => other,
        };
        let impls = self
            .hir
            .item_tree
            .impls
            .iter()
            .map(|(_, imp)| imp.clone())
            .collect::<Vec<_>>();

        for imp in impls {
            let Some(trait_ty) = imp.trait_ty.as_ref() else {
                continue;
            };
            let Some(trait_id) = self.resolve_trait_ref(trait_ty) else {
                continue;
            };
            let Some(mut subst) = self.impl_subst_from_self_ty(&imp, receiver_self_ty) else {
                continue;
            };
            let assumptions = self.current_trait_assumptions(ctx);
            if !self.impl_bounds_satisfied(&imp, &subst, &assumptions) {
                continue;
            }
            subst.insert("Self".into(), receiver_self_ty.clone());
            let fid = imp
                .methods
                .iter()
                .copied()
                .find(|fid| self.hir.item_tree.functions[*fid].name == *method_name)
                .or_else(|| self.default_method(trait_id, &method_name.0));
            let Some(fid) = fid else { continue };
            return Some(ResolvedMethod {
                fid,
                function: self.hir.item_tree.functions[fid].clone(),
                subst,
                trait_id: Some(trait_id),
                from_trait_bound: false,
            });
        }

        None
    }

    pub(super) fn find_trait_impl_method(
        &mut self,
        receiver_ty: &Type,
        trait_arg_ty: Option<&Type>,
        method_arg_ty: Option<&Type>,
        trait_id: TraitId,
        method_name: &str,
    ) -> Option<ResolvedMethod> {
        self.find_trait_impl_method_with_output(
            receiver_ty,
            trait_arg_ty,
            method_arg_ty,
            None,
            trait_id,
            method_name,
        )
    }

    pub(super) fn find_trait_impl_method_with_output(
        &mut self,
        receiver_ty: &Type,
        trait_arg_ty: Option<&Type>,
        method_arg_ty: Option<&Type>,
        output_ty: Option<&Type>,
        trait_id: TraitId,
        method_name: &str,
    ) -> Option<ResolvedMethod> {
        let impls = self
            .hir
            .item_tree
            .impls
            .iter()
            .map(|(_, imp)| imp.clone())
            .collect::<Vec<_>>();

        for imp in impls {
            let Some(trait_ty) = imp.trait_ty.as_ref() else {
                continue;
            };
            if self.resolve_trait_ref(trait_ty) != Some(trait_id) {
                continue;
            }
            let Some(mut subst) = self.impl_subst_from_self_ty(&imp, receiver_ty) else {
                continue;
            };
            if !self.impl_bounds_satisfied(&imp, &subst, &[]) {
                continue;
            }
            let fid = imp
                .methods
                .iter()
                .copied()
                .find(|fid| self.hir.item_tree.functions[*fid].name.0 == method_name)
                .or_else(|| self.default_method(trait_id, method_name));
            let Some(fid) = fid else { continue };
            subst =
                self.trait_ref_subst(trait_id, trait_ty, receiver_ty, &subst, imp.trait_ty_range);
            if let Some(trait_arg_ty) = trait_arg_ty
                && let Some(trait_arg) = self.hir.item_tree.traits[trait_id].generics.first()
            {
                let expected = subst.get(&trait_arg.0).cloned().unwrap_or(Type::Unknown);
                if !Self::bound_types_match(&expected, trait_arg_ty) {
                    continue;
                }
            }
            if let Some(rhs_ty) = method_arg_ty {
                let function = &self.hir.item_tree.functions[fid];
                let Some(rhs_param) = function.params.get(1) else {
                    continue;
                };
                let expected = self.lower_type_ref_with_params_at(
                    &rhs_param.ty,
                    &subst,
                    Some(rhs_param.ty_range),
                );
                let actual = Self::receiver_argument_type(rhs_ty, &expected);
                if !Self::bound_types_match(&expected, &actual) {
                    continue;
                }
            }
            if let Some(output_ty) = output_ty.filter(|ty| !ty.is_unknown_like()) {
                let function = &self.hir.item_tree.functions[fid];
                let actual = function.ret_type.as_ref().map_or(Type::Unit, |ty| {
                    self.lower_type_ref_with_params_at(
                        ty,
                        &subst,
                        function.ret_type_range.or(Some(function.name_range)),
                    )
                });
                if !Self::bound_types_match(output_ty, &actual) {
                    continue;
                }
            }
            return Some(ResolvedMethod {
                fid,
                function: self.hir.item_tree.functions[fid].clone(),
                subst,
                trait_id: Some(trait_id),
                from_trait_bound: false,
            });
        }

        None
    }

    pub(super) fn default_method(
        &self,
        trait_id: TraitId,
        method_name: &str,
    ) -> Option<FunctionId> {
        self.hir.item_tree.traits[trait_id]
            .default_methods
            .iter()
            .copied()
            .find(|fid| self.hir.item_tree.functions[*fid].name.0 == method_name)
    }

    pub(super) fn find_trait_bound_method(
        &mut self,
        ctx: &BodyCtx<'_>,
        receiver_ty: &Type,
        method_name: &Name,
    ) -> Option<ResolvedMethod> {
        let receiver_param = match receiver_ty {
            Type::Param(param) => param,
            Type::Ref(inner, _) => match inner.as_ref() {
                Type::Param(param) => param,
                _ => return None,
            },
            _ => return None,
        };
        let param = receiver_param;
        let bounds = self
            .current_generic_bounds(ctx)
            .into_iter()
            .filter(|bound| bound_target_param(bound).is_some_and(|name| name == *param))
            .collect::<Vec<_>>();

        for bound in bounds {
            let Some(trait_id) = self.resolve_trait_ref(&bound.trait_ty) else {
                continue;
            };
            let Some((method_trait_id, function)) =
                self.find_supertrait_method(trait_id, method_name, &mut HashSet::new())
            else {
                continue;
            };
            let mut subst = generic_param_map_with_consts(
                function.generics.iter().map(|name| name.0.as_str()),
                function.const_generics.iter().map(|name| name.0.as_str()),
            );
            let bound_subst = self.trait_ref_subst(
                trait_id,
                &bound.trait_ty,
                &Type::Param(param.clone()),
                &ctx.generic_params,
                Some(bound.trait_range),
            );
            let Some(mut supertrait_subst) = self.supertrait_subst(
                trait_id,
                method_trait_id,
                &Type::Param(param.clone()),
                &bound_subst,
                &mut HashSet::new(),
            ) else {
                continue;
            };
            for constraint in &bound.assoc_constraints {
                let ty = self.lower_type_ref_with_params_at(
                    &constraint.ty,
                    &ctx.generic_params,
                    Some(constraint.range),
                );
                supertrait_subst.insert(format!("Self::{}", constraint.name.0), ty);
            }
            subst.extend(supertrait_subst);
            return Some(ResolvedMethod {
                fid: ctx.function_id?,
                function,
                subst,
                trait_id: Some(method_trait_id),
                from_trait_bound: true,
            });
        }
        None
    }

    pub(super) fn find_supertrait_method(
        &self,
        trait_id: TraitId,
        method_name: &Name,
        visited: &mut HashSet<TraitId>,
    ) -> Option<(TraitId, HirFunction)> {
        if !visited.insert(trait_id) {
            return None;
        }
        if let Some(method) = self.hir.item_tree.traits[trait_id]
            .methods
            .iter()
            .find(|method| method.name == *method_name)
            .cloned()
        {
            return Some((trait_id, method));
        }
        self.hir.item_tree.traits[trait_id]
            .supertraits
            .iter()
            .filter_map(|bound| self.resolve_trait_ref(&bound.trait_ty))
            .find_map(|supertrait| self.find_supertrait_method(supertrait, method_name, visited))
    }

    pub(super) fn supertrait_subst(
        &mut self,
        current_trait: TraitId,
        target_trait: TraitId,
        self_ty: &Type,
        subst: &HashMap<String, Type>,
        visited: &mut HashSet<TraitId>,
    ) -> Option<HashMap<String, Type>> {
        if current_trait == target_trait {
            return Some(subst.clone());
        }
        if !visited.insert(current_trait) {
            return None;
        }

        let supertraits = self.hir.item_tree.traits[current_trait].supertraits.clone();
        for supertrait in supertraits {
            let Some(supertrait_id) = self.resolve_trait_ref(&supertrait.trait_ty) else {
                continue;
            };
            let next_subst = self.trait_ref_subst(
                supertrait_id,
                &supertrait.trait_ty,
                self_ty,
                subst,
                Some(supertrait.trait_range),
            );
            if let Some(result) =
                self.supertrait_subst(supertrait_id, target_trait, self_ty, &next_subst, visited)
            {
                return Some(result);
            }
        }
        None
    }

    pub(super) fn receiver_argument_type(base_ty: &Type, expected: &Type) -> Type {
        match (base_ty, expected) {
            (Type::Ref(actual, true), Type::Ref(expected, false)) if actual == expected => {
                Type::Ref(actual.clone(), false)
            }
            (base_ty, Type::Ref(inner, mutable)) if inner.as_ref() == base_ty => {
                Type::Ref(Box::new(base_ty.clone()), *mutable)
            }
            _ => base_ty.clone(),
        }
    }

    pub(super) fn check_field_access(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        base: ExprId,
        field: &Name,
        expected: Option<&Type>,
        span: Option<rowan::TextRange>,
    ) -> Type {
        let base_ty = self.check_expr(ctx, base);
        let base_value_ty = match &base_ty {
            Type::Ref(inner, _) => inner.as_ref(),
            ty => ty,
        };
        if let Type::Tuple(elements) = base_value_ty {
            let Ok(index) = field.0.parse::<usize>() else {
                self.diagnostic(
                    "E0006",
                    format!("tuple type has no field `{}`", field.0),
                    span,
                );
                return Type::Error;
            };
            let Some(field_ty) = elements.get(index).cloned() else {
                self.diagnostic(
                    "E0006",
                    format!(
                        "tuple index `{}` is out of bounds for type {}",
                        field.0,
                        base_ty.display(self.hir)
                    ),
                    span,
                );
                return Type::Error;
            };
            if let Some(expected) = expected
                && type_has_unresolved_inference(&field_ty)
            {
                let _ = self.unify_types(&field_ty, expected);
                self.last_occurs_error = None;
            }
            return field_ty;
        }
        let struct_ref = match &base_ty {
            Type::Struct(id, args) => Some((*id, args.as_slice())),
            Type::Ref(inner, _) => match inner.as_ref() {
                Type::Struct(id, args) => Some((*id, args.as_slice())),
                _ => None,
            },
            _ => None,
        };

        let Some((struct_id, args)) = struct_ref else {
            if !base_ty.is_unknown_like() {
                self.diagnostic(
                    "E0006",
                    format!(
                        "cannot access field `{}` on type {}",
                        field.0,
                        base_ty.display(self.hir)
                    ),
                    span,
                );
            }
            return Type::Error;
        };

        let strukt = self.hir.item_tree.structs[struct_id].clone();
        let subst = self.struct_subst(struct_id, args);
        let Some(field) = strukt
            .fields
            .iter()
            .find(|candidate| candidate.name == *field)
        else {
            self.diagnostic(
                "E0006",
                format!("unknown field `{}` on struct `{}`", field.0, strukt.name.0),
                span,
            );
            return Type::Error;
        };
        self.check_struct_field_visibility(ctx, struct_id, field, span);

        let field_ty = self.lower_type_ref_with_params_at(&field.ty, &subst, Some(field.ty_range));
        if let Some(expected) = expected
            && type_has_unresolved_inference(&field_ty)
        {
            let _ = self.unify_types(&field_ty, expected);
            self.last_occurs_error = None;
        }
        field_ty
    }

    /// Check that the LHS of an assignment targets a mutable binding.
    pub(super) fn check_assign_mut(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        lhs: ExprId,
        span: Option<rowan::TextRange>,
    ) {
        if let Expr::Unary {
            operand,
            op: UnaryOp::Deref,
        } = &ctx.body.exprs[lhs]
        {
            if matches!(
                self.result.expr_types.get(&(ctx.body_id, *operand)),
                Some(Type::Ref(_, false) | Type::Ptr { mutable: false, .. })
            ) {
                self.diagnostic(
                    "E0031",
                    "cannot mutate through a shared reference or const pointer",
                    span,
                );
            }
            return;
        }
        if let Expr::IndexAccess { base, index } = &ctx.body.exprs[lhs]
            && self
                .result
                .trait_method_calls
                .get(&(ctx.body_id, lhs))
                .is_some_and(|call| call.method == "index")
            && !self.check_mutable_index(ctx, lhs, *base, *index, span)
        {
            return;
        }
        self.record_value_use(ctx, lhs, ValueUse::Mutable);
        if let Some((id, name)) = Self::root_binding_of_expr(ctx, lhs)
            && !ctx.bindings.is_mut(id)
            && !ctx.is_delayed_binding(id)
        {
            self.diagnostic(
                "E0031",
                format!("cannot assign to `{name}`, as it is not declared as mutable"),
                span,
            );
            return;
        }
        if let Expr::Path {
            resolved: Some(resolved @ (ResolvedName::Param(_) | ResolvedName::LambdaParam { .. })),
            ..
        } = &ctx.body.exprs[lhs]
            && !ctx.resolved_param_is_mut(resolved)
        {
            self.diagnostic("E0031", "cannot assign to an immutable parameter", span);
        }
    }

    fn check_mutable_index(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        lhs: ExprId,
        base: ExprId,
        index: ExprId,
        span: Option<rowan::TextRange>,
    ) -> bool {
        let base_ty = self
            .result
            .expr_types
            .get(&(ctx.body_id, base))
            .cloned()
            .unwrap_or(Type::Error);
        let index_ty = self
            .result
            .expr_types
            .get(&(ctx.body_id, index))
            .cloned()
            .unwrap_or(Type::Error);
        let receiver_ty = match &base_ty {
            Type::Ref(inner, _) => inner.as_ref(),
            _ => &base_ty,
        };
        let Some(trait_id) = self.result.trait_env.lang_items.get(LangItem::IndexMut) else {
            self.diagnostic("E0036", "missing `IndexMut` trait", span);
            return false;
        };
        if self
            .check_trait_bound_index(
                ctx,
                lhs,
                base,
                index,
                receiver_ty,
                &index_ty,
                trait_id,
                "index_mut",
                None,
            )
            .is_some()
        {
            return true;
        }
        let Some(method) = self.find_trait_impl_method(
            receiver_ty,
            Some(&index_ty),
            Some(&index_ty),
            trait_id,
            "index_mut",
        ) else {
            self.diagnostic(
                "E0036",
                format!(
                    "type `{}` cannot be mutably indexed by `{}`",
                    base_ty.display(self.hir),
                    index_ty.display(self.hir)
                ),
                span,
            );
            return false;
        };
        let receiver = &method.function.params[0];
        let expected_receiver = self.lower_type_ref_with_params_at(
            &receiver.ty,
            &method.subst,
            Some(receiver.ty_range),
        );
        let actual_receiver = Self::receiver_argument_type(&base_ty, &expected_receiver);
        self.expect_assignable(
            &expected_receiver,
            &actual_receiver,
            "mutable index receiver",
            ctx.expr_range(base),
        );
        self.record_value_use(ctx, base, Self::hir_parameter_value_use(&receiver.ty));
        let index_param = &method.function.params[1];
        let expected_index = self.lower_type_ref_with_params_at(
            &index_param.ty,
            &method.subst,
            Some(index_param.ty_range),
        );
        self.expect_assignable(&expected_index, &index_ty, "index", ctx.expr_range(index));
        self.constrain_index_type(ctx, index, &index_ty, &expected_index);
        self.record_value_use(ctx, index, Self::hir_parameter_value_use(&index_param.ty));
        self.result.trait_method_calls.insert(
            (ctx.body_id, lhs),
            TraitMethodCall {
                trait_id,
                method: "index_mut".into(),
                dynamic: false,
            },
        );
        true
    }

    /// Walk the expression to find the root local `PatternBindingId` (ignoring dereferences).
    pub(super) fn root_binding_of_expr(
        ctx: &BodyCtx<'_>,
        expr_id: ExprId,
    ) -> Option<(PatternBindingId, String)> {
        match &ctx.body.exprs[expr_id] {
            Expr::Path {
                resolved: Some(ResolvedName::PatternBinding(id)),
                path,
            } => Some((
                *id,
                path.as_single_name()
                    .map(|name| name.0.clone())
                    .unwrap_or_default(),
            )),
            Expr::FieldAccess { base, .. } | Expr::IndexAccess { base, .. } => {
                Self::root_binding_of_expr(ctx, *base)
            }
            _ => None,
        }
    }
}

fn hir_type_mentions_self(ty: &HirTypeRef) -> bool {
    match ty {
        HirTypeRef::Named(path) => {
            path.segments.first().is_some_and(|name| name.0 == "Self")
                || path
                    .segment_type_args
                    .iter()
                    .flat_map(|(_, args)| args)
                    .any(hir_type_mentions_self)
                || path.type_args.iter().any(hir_type_mentions_self)
        }
        HirTypeRef::Ref(inner, _)
        | HirTypeRef::Ptr { inner, .. }
        | HirTypeRef::Slice(inner)
        | HirTypeRef::Array(inner, _) => hir_type_mentions_self(inner),
        HirTypeRef::Tuple(elements) => elements.iter().any(hir_type_mentions_self),
        HirTypeRef::ImplTrait { trait_ty, .. } | HirTypeRef::DynTrait { trait_ty, .. } => {
            hir_type_mentions_self(trait_ty)
        }
        HirTypeRef::Never | HirTypeRef::Const(_) | HirTypeRef::Unknown | HirTypeRef::Error => false,
    }
}
