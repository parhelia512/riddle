use super::*;

impl TypeChecker<'_> {
    pub(super) fn check_call(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        expr_id: ExprId,
        callee: ExprId,
        args: &[ExprId],
        type_args: &[HirTypeRef],
        expected: Option<&Type>,
    ) -> Type {
        let span = ctx.expr_range(expr_id);
        let impl_type_args = match &ctx.body.exprs[callee] {
            Expr::Path { path, .. } => path
                .segments
                .len()
                .checked_sub(2)
                .map(|index| path.type_args_for_segment(index).to_vec())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        if matches!(&ctx.body.exprs[callee], Expr::FieldAccess { .. }) {
            return self.check_method_call(ctx, callee, args, type_args, expected, span);
        }

        if let Expr::Path {
            resolved: Some(ResolvedName::EnumVariant(enum_id, variant_index)),
            ..
        } = &ctx.body.exprs[callee]
        {
            return self.check_enum_variant_call(
                ctx,
                *enum_id,
                *variant_index,
                args,
                expected,
                span,
            );
        }

        if let Expr::Path {
            path,
            resolved: Some(ResolvedName::Trait(trait_id)),
        } = &ctx.body.exprs[callee]
            && path.segments.last().is_some_and(|name| name.0 == "default")
        {
            return self.check_static_trait_call(ctx, expr_id, *trait_id, args, expected, span);
        }

        let callee_ty = self.check_expr(ctx, callee);
        let resolved_callee = self.resolve_type(&callee_ty);
        let signature = if matches!(resolved_callee, Type::FunctionItem { .. }) {
            None
        } else {
            self.callable_signature_for_type(&resolved_callee)
                .or_else(|| self.callable_bound_for_type(ctx, &resolved_callee))
        };
        if let Some(signature) = signature {
            self.record_value_use(
                ctx,
                callee,
                match signature.kind {
                    ClosureKind::Fn => ValueUse::Shared,
                    ClosureKind::FnMut => ValueUse::Mutable,
                    ClosureKind::FnOnce => ValueUse::Move,
                },
            );
            if signature.kind == ClosureKind::FnMut {
                self.check_mutable_closure_binding(ctx, callee);
            }
            if signature.is_unsafe {
                self.require_unsafe(ctx, "calling an unsafe function", span);
            }
            if args.len() != signature.params.len() {
                self.diagnostic(
                    "E0005",
                    format!(
                        "function value expects {} argument(s), got {}",
                        signature.params.len(),
                        args.len()
                    ),
                    span,
                );
            }
            for (index, arg) in args.iter().enumerate() {
                if let Some(param) = signature.params.get(index) {
                    let actual = self.check_expr_expected(ctx, *arg, param);
                    self.expect_assignable_with_occurs_span(
                        param,
                        &actual,
                        "function argument",
                        ctx.expr_range(*arg),
                        span,
                    );
                    self.record_value_use(ctx, *arg, Self::parameter_value_use(param));
                } else {
                    self.check_expr(ctx, *arg);
                    self.record_value_use(ctx, *arg, ValueUse::Move);
                }
            }
            return self.resolve_type(&signature.ret);
        }
        let Type::FunctionItem { function: fid, .. } = callee_ty else {
            for arg in args {
                self.check_expr(ctx, *arg);
                self.record_value_use(ctx, *arg, ValueUse::Move);
            }
            if !callee_ty.is_unknown_like() {
                self.diagnostic(
                    "E0004",
                    format!("cannot call value of type {}", callee_ty.display(self.hir)),
                    ctx.expr_range(callee),
                );
            }
            return Type::Error;
        };

        let function = self.hir.item_tree.functions[fid].clone();
        if function.is_unsafe {
            self.require_unsafe(ctx, "calling an unsafe function", span);
        }
        let impl_generics = self.impl_generic_names(fid);
        let impl_const_generics = self.impl_const_generic_names(fid);
        let params = generic_param_map_with_consts(
            impl_generics
                .iter()
                .map(String::as_str)
                .chain(function.generics.iter().map(|name| name.0.as_str()))
                .chain(
                    function
                        .implicit_generics
                        .iter()
                        .map(|name| name.0.as_str()),
                ),
            impl_const_generics
                .iter()
                .map(String::as_str)
                .chain(function.const_generics.iter().map(|name| name.0.as_str())),
        );
        let mut subst = HashMap::new();
        let mut generic_arg_spans = HashMap::new();

        if !impl_type_args.is_empty() {
            if impl_type_args.len() != impl_generics.len() {
                self.diagnostic(
                    "E0005",
                    format!(
                        "associated function `{}` expects {} type argument(s) on its type, got {}",
                        function.name.0,
                        impl_generics.len(),
                        impl_type_args.len()
                    ),
                    span,
                );
            }
            for (param_name, type_arg) in impl_generics.iter().zip(&impl_type_args) {
                let lowered =
                    self.lower_type_ref_with_params_at(type_arg, &ctx.generic_params, span);
                subst.insert(param_name.clone(), lowered);
            }
        }

        if !type_args.is_empty() {
            let type_param_names: Vec<_> = function.generics.iter().map(|name| &name.0).collect();
            if type_args.len() != type_param_names.len() {
                self.diagnostic(
                    "E0005",
                    format!(
                        "function `{}` expects {} type argument(s), got {}",
                        function.name.0,
                        type_param_names.len(),
                        type_args.len()
                    ),
                    span,
                );
            }
            for (param_name, type_arg) in type_param_names.iter().zip(type_args.iter()) {
                let lowered =
                    self.lower_type_ref_with_params_at(type_arg, &ctx.generic_params, span);
                subst.insert((*param_name).clone(), lowered);
            }
        }

        self.seed_type_inference(
            impl_generics
                .iter()
                .map(String::as_str)
                .chain(function.generics.iter().map(|name| name.0.as_str()))
                .chain(
                    function
                        .implicit_generics
                        .iter()
                        .map(|name| name.0.as_str()),
                ),
            &mut subst,
        );

        if args.len() != function.params.len() {
            self.diagnostic(
                "E0005",
                format!(
                    "function `{}` expects {} argument(s), got {}",
                    function.name.0,
                    function.params.len(),
                    args.len()
                ),
                span,
            );
        }

        for (index, arg) in args.iter().enumerate() {
            if let Some(param) = function.params.get(index) {
                let pattern =
                    self.lower_type_ref_with_params_at(&param.ty, &params, Some(param.ty_range));
                record_generic_arg_spans(
                    &pattern,
                    &params,
                    ctx.expr_range(*arg),
                    &mut generic_arg_spans,
                );
                let expected = substitute_type(&pattern, &subst);
                let callable_expected = self
                    .callable_bound_for_function_type(&function, &pattern, &params)
                    .map(callable_signature_type)
                    .map(|ty| substitute_type(&ty, &subst));
                let actual = match callable_expected.as_ref() {
                    Some(expected) => self.check_expr_expected(ctx, *arg, expected),
                    None if expected_has_param(&expected) => self.check_expr(ctx, *arg),
                    None => self.check_expr_expected(ctx, *arg, &expected),
                };
                if let Some(expected) = callable_expected.as_ref() {
                    let _ = self.unify_types(expected, &actual);
                    self.last_occurs_error = None;
                }
                collect_subst(&pattern, &actual, &mut subst);
                let expected = substitute_type(&pattern, &subst);
                self.expect_assignable_with_occurs_span(
                    &expected,
                    &actual,
                    "function argument",
                    ctx.expr_range(*arg),
                    span,
                );
                self.record_value_use(ctx, *arg, Self::hir_parameter_value_use(&param.ty));
            } else {
                self.check_expr(ctx, *arg);
                self.record_value_use(ctx, *arg, ValueUse::Move);
            }
        }

        if let (Some(expected), Some(return_ty)) = (expected, function.ret_type.as_ref()) {
            let return_pattern = self.lower_type_ref_with_params_at(
                return_ty,
                &params,
                function.ret_type_range.or(Some(function.name_range)),
            );
            collect_subst(&return_pattern, expected, &mut subst);
        }

        if !impl_generics.is_empty()
            || !impl_const_generics.is_empty()
            || !function.generics.is_empty()
            || !function.implicit_generics.is_empty()
            || !function.const_generics.is_empty()
        {
            let inferred_names = impl_generics
                .iter()
                .map(String::as_str)
                .chain(function.generics.iter().map(|name| name.0.as_str()))
                .chain(
                    function
                        .implicit_generics
                        .iter()
                        .map(|name| name.0.as_str()),
                )
                .chain(impl_const_generics.iter().map(String::as_str))
                .chain(function.const_generics.iter().map(|name| name.0.as_str()))
                .map(str::to_string)
                .collect::<Vec<_>>();
            let generic_args = inferred_names
                .iter()
                .map(|name| subst.get(name).cloned().unwrap_or(Type::Unknown))
                .collect::<Vec<_>>();
            self.result.generic_calls.insert(
                (ctx.body_id, callee),
                crate::result::GenericCall { args: generic_args },
            );
            self.pending_generic_calls.push(PendingGenericCall {
                body_id: ctx.body_id,
                callee,
                function: fid,
                inferred_names,
                subst: subst.clone(),
                generic_arg_spans,
                callee_span: ctx.expr_range(callee),
                span,
                kind: "function",
                caller: ctx.function_id,
                check_sized: true,
            });
        }

        let output_ty = function
            .ret_type
            .as_ref()
            .map(|ty| {
                substitute_type(
                    &self.lower_type_ref_with_params_at(
                        ty,
                        &params,
                        function.ret_type_range.or(Some(function.name_range)),
                    ),
                    &subst,
                )
            })
            .unwrap_or(Type::Unit);
        let return_ty = output_ty;
        if let Some(expected) = expected {
            let _ = self.unify_types(&return_ty, expected);
            self.last_occurs_error = None;
        }
        return_ty
    }

    pub(super) fn check_static_trait_call(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        expr_id: ExprId,
        trait_id: TraitId,
        args: &[ExprId],
        expected: Option<&Type>,
        span: Option<rowan::TextRange>,
    ) -> Type {
        if !args.is_empty() {
            for arg in args {
                self.check_expr(ctx, *arg);
                self.record_value_use(ctx, *arg, ValueUse::Move);
            }
            self.diagnostic(
                "E0005",
                "static trait functions do not take arguments",
                span,
            );
            return Type::Error;
        }
        let Some(expected) = expected.filter(|ty| !ty.is_unknown_like()) else {
            return Type::Unknown;
        };
        let Some(method) = self.find_trait_impl_method(expected, None, None, trait_id, "default")
        else {
            self.diagnostic(
                "E0013",
                format!(
                    "no `default` implementation for `{}`",
                    expected.display(self.hir)
                ),
                span,
            );
            return Type::Error;
        };
        if !method.function.params.is_empty() {
            self.diagnostic("E0013", "`default` must be a static trait function", span);
            return Type::Error;
        }
        let return_ty = method
            .function
            .ret_type
            .as_ref()
            .map(|ty| {
                self.lower_type_ref_with_params_at(
                    ty,
                    &method.subst,
                    method
                        .function
                        .ret_type_range
                        .or(Some(method.function.name_range)),
                )
            })
            .unwrap_or(Type::Unit);
        if !self.bound_types_match(expected, &return_ty) {
            self.diagnostic(
                "E0013",
                format!(
                    "`default` returns `{}`, expected `{}`",
                    return_ty.display(self.hir),
                    expected.display(self.hir)
                ),
                span,
            );
            return Type::Error;
        }
        self.result.trait_method_calls.insert(
            (ctx.body_id, expr_id),
            TraitMethodCall {
                trait_id,
                method: "default".into(),
            },
        );
        return_ty
    }

    pub(super) fn check_enum_variant_call(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        enum_id: hir::item_tree::EnumId,
        variant_index: usize,
        args: &[ExprId],
        expected: Option<&Type>,
        span: Option<rowan::TextRange>,
    ) -> Type {
        let enum_data = &self.hir.item_tree.enums[enum_id];
        let Some(variant) = enum_data.variants.get(variant_index) else {
            for arg in args {
                self.check_expr(ctx, *arg);
                self.record_value_use(ctx, *arg, ValueUse::Move);
            }
            return Type::Error;
        };

        let mut subst = match expected {
            Some(Type::Enum(expected_id, expected_args)) if *expected_id == enum_id => enum_data
                .generics
                .iter()
                .chain(enum_data.const_generics.iter())
                .zip(expected_args.iter())
                .map(|(name, ty)| (name.0.clone(), ty.clone()))
                .collect::<HashMap<_, _>>(),
            _ => HashMap::new(),
        };
        let params = generic_param_map_with_consts(
            enum_data.generics.iter().map(|name| name.0.as_str()),
            enum_data.const_generics.iter().map(|name| name.0.as_str()),
        );
        let fields = match &variant.kind {
            HirVariantKind::Tuple(fields) => fields.as_slice(),
            HirVariantKind::Unit => &[],
            HirVariantKind::Struct(_) => {
                for arg in args {
                    self.check_expr(ctx, *arg);
                    self.record_value_use(ctx, *arg, ValueUse::Move);
                }
                self.diagnostic(
                    "E0004",
                    format!(
                        "cannot call struct enum variant `{}`; use struct literal syntax",
                        variant.name.0
                    ),
                    span,
                );
                return Type::Error;
            }
        };

        if args.len() != fields.len() {
            self.diagnostic(
                "E0005",
                format!(
                    "enum variant `{}` expects {} argument(s), got {}",
                    variant.name.0,
                    fields.len(),
                    args.len()
                ),
                span,
            );
        }

        for (index, arg) in args.iter().enumerate() {
            if let Some(field) = fields.get(index) {
                let pattern = self.lower_type_ref_with_params_at(
                    field,
                    &params,
                    variant
                        .field_ranges
                        .get(index)
                        .copied()
                        .or(Some(variant.name_range)),
                );
                let expected = substitute_type(&pattern, &subst);
                let actual = if pattern_has_unresolved_param(&pattern, &subst) {
                    self.check_expr(ctx, *arg)
                } else {
                    self.check_expr_expected(ctx, *arg, &expected)
                };
                collect_subst(&pattern, &actual, &mut subst);
                let expected = substitute_type(&pattern, &subst);
                self.expect_assignable(
                    &expected,
                    &actual,
                    "enum variant argument",
                    ctx.expr_range(*arg),
                );
                self.record_value_use(ctx, *arg, ValueUse::Move);
            } else {
                self.check_expr(ctx, *arg);
                self.record_value_use(ctx, *arg, ValueUse::Move);
            }
        }

        let args = enum_data
            .generics
            .iter()
            .chain(enum_data.const_generics.iter())
            .map(|name| subst.get(&name.0).cloned().unwrap_or(Type::Unknown))
            .collect();
        let ty = Type::Enum(enum_id, args);
        self.check_type_bounds(ctx, &ty, span);
        ty
    }

    pub(super) fn enum_variant_type(
        &self,
        enum_id: hir::item_tree::EnumId,
        expected: Option<&Type>,
    ) -> Type {
        if let Some(Type::Enum(expected_id, args)) = expected
            && *expected_id == enum_id
        {
            return Type::Enum(enum_id, args.clone());
        }
        let args = self.hir.item_tree.enums[enum_id]
            .generics
            .iter()
            .chain(self.hir.item_tree.enums[enum_id].const_generics.iter())
            .map(|_| Type::Unknown)
            .collect();
        Type::Enum(enum_id, args)
    }

    pub(super) fn callable_bound_for_type(
        &mut self,
        ctx: &BodyCtx<'_>,
        ty: &Type,
    ) -> Option<CallableSignature> {
        let Type::Param(param) = self.resolve_type(ty) else {
            return None;
        };
        for bound in self.current_generic_bounds(ctx) {
            if bound_target_param(&bound) != Some(param.as_str()) {
                continue;
            }
            let Some(kind) = builtin_callable_kind(&bound.trait_ty) else {
                continue;
            };
            let signature = bound.callable.as_ref()?;
            return Some(self.lower_hir_callable_signature(
                signature,
                kind,
                &ctx.generic_params,
                Some(bound.trait_range),
            ));
        }
        let function = ctx.function?;
        for parameter in &function.params {
            let HirTypeRef::ImplTrait {
                trait_ty,
                trait_range,
                callable: Some(callable),
                hidden: Some(hidden),
            } = &parameter.ty
            else {
                continue;
            };
            if hidden.0 != param {
                continue;
            }
            let kind = builtin_callable_kind(trait_ty)?;
            return Some(self.lower_hir_callable_signature(
                callable,
                kind,
                &ctx.generic_params,
                Some(*trait_range),
            ));
        }
        None
    }

    pub(super) fn callable_bound_for_function_type(
        &mut self,
        function: &HirFunction,
        ty: &Type,
        params: &HashMap<String, Type>,
    ) -> Option<CallableSignature> {
        let Type::Param(param) = ty else {
            return None;
        };
        for bound in &function.generic_bounds {
            if bound_target_param(bound) != Some(param.as_str()) {
                continue;
            }
            let Some(kind) = builtin_callable_kind(&bound.trait_ty) else {
                continue;
            };
            let signature = bound.callable.as_ref()?;
            return Some(self.lower_hir_callable_signature(
                signature,
                kind,
                params,
                Some(bound.trait_range),
            ));
        }
        for parameter in &function.params {
            let HirTypeRef::ImplTrait {
                trait_ty,
                trait_range,
                callable: Some(callable),
                hidden: Some(hidden),
            } = &parameter.ty
            else {
                continue;
            };
            if hidden.0 != *param {
                continue;
            }
            let kind = builtin_callable_kind(trait_ty)?;
            return Some(self.lower_hir_callable_signature(
                callable,
                kind,
                params,
                Some(*trait_range),
            ));
        }
        None
    }

    pub(super) fn check_callable_requirement(
        &mut self,
        ctx: &BodyCtx<'_>,
        actual: &Type,
        required: &CallableSignature,
        target: &str,
        span: Option<rowan::TextRange>,
    ) {
        if actual.is_unknown_like() {
            return;
        }
        let actual_boundary = self
            .callable_signature_for_type(actual)
            .or_else(|| self.callable_bound_for_type(ctx, actual))
            .map(callable_signature_type)
            .unwrap_or_else(|| actual.clone());
        let required_boundary = callable_signature_type(required.clone());
        if self.unify_types(&required_boundary, &actual_boundary) {
            return;
        }
        if self.last_occurs_error.take().is_some() {
            self.diagnostic("E0046", "cannot construct an infinite type", span);
            return;
        }
        let unsafe_mismatch = self
            .callable_signature_for_type(&actual_boundary)
            .is_some_and(|signature| signature.is_unsafe && !required.is_unsafe);
        let (code, message) = if unsafe_mismatch {
            (
                "E0001",
                format!(
                    "unsafe function does not satisfy safe `{}` bound for `{target}`",
                    required.kind.as_str()
                ),
            )
        } else {
            (
                "E0035",
                format!(
                    "type `{}` does not satisfy callable bound `{}` for `{target}`",
                    actual.display(self.hir),
                    required.kind.as_str()
                ),
            )
        };
        self.diagnostic(code, message, span);
    }

    pub(crate) fn finish_pending_generic_calls(&mut self, ctx: &BodyCtx<'_>) {
        let pending = self
            .pending_generic_calls
            .iter()
            .filter(|call| call.body_id == ctx.body_id)
            .cloned()
            .collect::<Vec<_>>();
        self.pending_generic_calls
            .retain(|call| call.body_id != ctx.body_id);

        for call in pending {
            let mut subst = call
                .subst
                .iter()
                .map(|(name, ty)| (name.clone(), self.resolve_type(ty)))
                .collect::<HashMap<_, _>>();
            let function = self.hir.item_tree.functions[call.function].clone();
            self.check_generic_bounds(
                ctx,
                &function,
                &subst,
                &call.generic_arg_spans,
                call.callee_span,
                call.span,
            );
            subst = subst
                .iter()
                .map(|(name, ty)| (name.clone(), self.resolve_type(ty)))
                .collect();
            let unresolved = call
                .inferred_names
                .iter()
                .any(|name| subst.get(name).is_none_or(type_has_unresolved_inference));
            if unresolved {
                self.diagnostic(
                    "E0005",
                    format!(
                        "cannot infer type argument(s) for {} `{}`",
                        call.kind, function.name.0
                    ),
                    call.span,
                );
                continue;
            }

            let generic_args = self
                .result
                .generic_calls
                .get(&(ctx.body_id, call.callee))
                .map(|call| {
                    call.args
                        .iter()
                        .map(|arg| self.resolve_type(arg))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if call.check_sized {
                for arg in &generic_args {
                    self.expect_sized_value(arg, call.span);
                }
            }
            if let Some(caller) = call.caller {
                self.generic_edges.push(GenericEdge {
                    caller,
                    callee: call.function,
                    grows: generic_args
                        .iter()
                        .any(|arg| grows_generic_arg(arg, &ctx.generic_params)),
                    span: call.span,
                });
            }
        }
    }

    pub(super) fn check_generic_bounds(
        &mut self,
        ctx: &BodyCtx<'_>,
        function: &HirFunction,
        subst: &HashMap<String, Type>,
        generic_arg_spans: &HashMap<String, rowan::TextRange>,
        callee_span: Option<rowan::TextRange>,
        span: Option<rowan::TextRange>,
    ) {
        for bound in &function.generic_bounds {
            let bound_span = bound_target_param(bound)
                .and_then(|name| generic_arg_spans.get(name).copied())
                .or(span);
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
                self.check_callable_requirement(
                    ctx,
                    &actual,
                    &required,
                    &bound.target_ty.display(),
                    bound_span,
                );
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
                let format_placeholder = match (function.name.0.as_str(), trait_name.as_str()) {
                    ("print_debug", "Debug")
                        if self.hir.std_loaded
                            && self.hir.package_for_range(function.name_range).is_none() =>
                    {
                        Some("{:?}")
                    }
                    _ => None,
                };
                if let Some(placeholder) = format_placeholder {
                    let type_name = actual.display(self.hir);
                    let owner_package = self.hir.package_for_range(ctx.owner_range());
                    let derive_target = match &actual {
                        Type::Struct(id, _) => {
                            let item = &self.hir.item_tree.structs[*id];
                            Some((item.name.0.clone(), item.name_range))
                        }
                        Type::Enum(id, _) => {
                            let item = &self.hir.item_tree.enums[*id];
                            Some((item.name.0.clone(), item.name_range))
                        }
                        _ => None,
                    }
                    .filter(|(_, range)| {
                        owner_package.is_some()
                            && self.hir.package_for_range(*range) == owner_package
                    });
                    let diagnostics_before = self.result.diagnostics.len();
                    self.diagnostic(
                        "E0035",
                        format!("`{type_name}` doesn't implement `{trait_name}`"),
                        bound_span,
                    );
                    if let Some(diagnostic) = self.result.diagnostics.get_mut(diagnostics_before) {
                        diagnostic.labels[0].message = format!(
                            "`{type_name}` cannot be formatted using `{placeholder}` because it doesn't implement `{trait_name}`"
                        );
                        if let Some(callee_span) = callee_span
                            && Some(callee_span) != bound_span
                        {
                            diagnostic.labels.push(SourceLabel {
                                range: callee_span,
                                message: "required by this formatting parameter".into(),
                                style: LabelStyle::Secondary,
                            });
                        }
                        diagnostic.notes = vec![format!(
                            "the trait `{trait_name}` is not implemented for `{type_name}`"
                        )];
                        if let Some((name, range)) = derive_target {
                            diagnostic.labels.push(SourceLabel {
                                range,
                                message: format!(
                                    "consider annotating `{name}` with `#[derive(Debug)]`"
                                ),
                                style: LabelStyle::Secondary,
                            });
                            diagnostic.help = Some(format!(
                                "add `#[derive(Debug)]` to `{name}` or manually implement `Debug`"
                            ));
                        } else {
                            diagnostic.help =
                                Some(format!("implement `{trait_name}` for `{type_name}`"));
                        }
                    }
                    continue;
                }
                self.diagnostic(
                    "E0035",
                    format!(
                        "type `{}` does not satisfy bound `{}` for `{}`",
                        actual.display(self.hir),
                        trait_name,
                        bound.target_ty.display()
                    ),
                    bound_span,
                );
            }
        }

        for param in &function.params {
            let HirTypeRef::ImplTrait {
                trait_ty,
                trait_range,
                callable: Some(callable),
                hidden: Some(hidden),
            } = &param.ty
            else {
                continue;
            };
            let Some(kind) = builtin_callable_kind(trait_ty) else {
                continue;
            };
            let Some(actual) = subst.get(&hidden.0) else {
                continue;
            };
            let required =
                self.lower_hir_callable_signature(callable, kind, subst, Some(*trait_range));
            let bound_span = generic_arg_spans.get(&hidden.0).copied().or(span);
            self.check_callable_requirement(ctx, actual, &required, &hidden.0, bound_span);
        }
    }
}
