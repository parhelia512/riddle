use super::*;

impl TypeChecker<'_> {
    pub(super) fn check_struct_expr(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        resolved: Option<&ResolvedName>,
        fields: &[hir::body::StructExprField],
        explicit_type_args: &[HirTypeRef],
        expected: Option<&Type>,
        span: Option<rowan::TextRange>,
    ) -> Type {
        if let Some(ResolvedName::EnumVariant(enum_id, variant_index)) = resolved {
            return self.check_enum_struct_expr(
                ctx,
                *enum_id,
                *variant_index,
                fields,
                expected,
                span,
            );
        }
        let Some(ResolvedName::Struct(struct_id)) = resolved else {
            for field in fields {
                self.check_expr(ctx, field.value);
                self.record_value_use(ctx, field.value, ValueUse::Move);
            }
            self.diagnostic("E0009", "struct literal does not resolve to a struct", span);
            return Type::Error;
        };

        let strukt = self.hir.item_tree.structs[*struct_id].clone();
        let mut subst = match expected {
            Some(Type::Struct(expected_id, args)) if expected_id == struct_id => {
                self.struct_subst(*struct_id, args)
            }
            _ => HashMap::new(),
        };

        if !explicit_type_args.is_empty() {
            let type_param_names: Vec<String> =
                strukt.generics.iter().map(|n| n.0.clone()).collect();
            if explicit_type_args.len() != type_param_names.len() {
                self.diagnostic(
                    "E0009",
                    format!(
                        "struct `{}` expects {} type argument(s), got {}",
                        strukt.name.0,
                        type_param_names.len(),
                        explicit_type_args.len()
                    ),
                    span,
                );
            }
            for (name, ty) in type_param_names.iter().zip(explicit_type_args.iter()) {
                let lowered = self.lower_type_ref_with_params_at(ty, &ctx.generic_params, span);
                subst.insert(name.clone(), lowered);
            }
        }
        let expected_fields = strukt
            .fields
            .iter()
            .map(|field| (field.name.0.as_str(), field))
            .collect::<HashMap<_, _>>();
        let mut seen = Vec::new();

        for field in fields {
            let Some(expected_field) = expected_fields.get(field.name.0.as_str()) else {
                self.check_expr(ctx, field.value);
                self.diagnostic(
                    "E0006",
                    format!(
                        "unknown field `{}` on struct `{}`",
                        field.name.0, strukt.name.0
                    ),
                    span,
                );
                continue;
            };

            seen.push(field.name.0.as_str());
            self.check_struct_field_visibility(ctx, *struct_id, expected_field, span);
            let pattern = self.lower_type_ref_with_params_at(
                &expected_field.ty,
                &generic_param_map_with_consts(
                    strukt.generics.iter().map(|name| name.0.as_str()),
                    strukt.const_generics.iter().map(|name| name.0.as_str()),
                ),
                Some(expected_field.ty_range),
            );
            let expected = substitute_type(&pattern, &subst);
            let actual =
                if expected.is_unknown_like() || pattern_has_unresolved_param(&pattern, &subst) {
                    self.check_expr(ctx, field.value)
                } else {
                    self.check_expr_expected(ctx, field.value, &expected)
                };
            collect_subst(&pattern, &actual, &mut subst);
            let expected = substitute_type(&pattern, &subst);
            self.expect_assignable(&expected, &actual, "struct field", span);
        }
        for field in fields {
            self.record_value_use(ctx, field.value, ValueUse::Move);
        }

        for expected in &strukt.fields {
            if !seen.contains(&expected.name.0.as_str()) {
                self.diagnostic(
                    "E0007",
                    format!(
                        "missing field `{}` in struct literal `{}`",
                        expected.name.0, strukt.name.0
                    ),
                    span,
                );
            }
        }

        let args = strukt
            .generics
            .iter()
            .chain(strukt.const_generics.iter())
            .map(|name| subst.get(&name.0).cloned().unwrap_or(Type::Unknown))
            .collect();
        let ty = Type::Struct(*struct_id, args);
        self.check_type_bounds(ctx, &ty, span);
        ty
    }

    pub(super) fn check_enum_struct_expr(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        enum_id: hir::item_tree::EnumId,
        variant_index: usize,
        fields: &[hir::body::StructExprField],
        expected: Option<&Type>,
        span: Option<rowan::TextRange>,
    ) -> Type {
        let enum_data = self.hir.item_tree.enums[enum_id].clone();
        let Some(variant) = enum_data.variants.get(variant_index) else {
            self.diagnostic("E0009", "unknown enum variant", span);
            return Type::Error;
        };
        let HirVariantKind::Struct(expected_items) = &variant.kind else {
            for field in fields {
                self.check_expr(ctx, field.value);
                self.record_value_use(ctx, field.value, ValueUse::Move);
            }
            self.diagnostic(
                "E0009",
                format!("enum variant `{}` is not struct-style", variant.name.0),
                span,
            );
            return Type::Error;
        };

        let mut subst = match expected {
            Some(Type::Enum(expected_id, args)) if *expected_id == enum_id => enum_data
                .generics
                .iter()
                .chain(enum_data.const_generics.iter())
                .zip(args.iter())
                .map(|(name, ty)| (name.0.clone(), ty.clone()))
                .collect::<HashMap<_, _>>(),
            _ => HashMap::new(),
        };
        let generic_params = generic_param_map_with_consts(
            enum_data.generics.iter().map(|name| name.0.as_str()),
            enum_data.const_generics.iter().map(|name| name.0.as_str()),
        );
        let mut seen = HashSet::new();

        for field in fields {
            if !seen.insert(field.name.0.clone()) {
                self.check_expr(ctx, field.value);
                self.diagnostic(
                    "E0006",
                    format!("field `{}` is specified more than once", field.name.0),
                    span,
                );
                continue;
            }
            let Some(expected_field) = expected_items
                .iter()
                .find(|item| item.name.0 == field.name.0)
            else {
                self.check_expr(ctx, field.value);
                self.diagnostic(
                    "E0006",
                    format!(
                        "unknown field `{}` on variant `{}`",
                        field.name.0, variant.name.0
                    ),
                    span,
                );
                continue;
            };
            let pattern = self.lower_type_ref_with_params_at(
                &expected_field.ty,
                &generic_params,
                Some(expected_field.ty_range),
            );
            let expected = substitute_type(&pattern, &subst);
            let actual =
                if expected.is_unknown_like() || pattern_has_unresolved_param(&pattern, &subst) {
                    self.check_expr(ctx, field.value)
                } else {
                    self.check_expr_expected(ctx, field.value, &expected)
                };
            collect_subst(&pattern, &actual, &mut subst);
            let expected = substitute_type(&pattern, &subst);
            self.expect_assignable(&expected, &actual, "enum variant field", span);
        }
        for field in fields {
            self.record_value_use(ctx, field.value, ValueUse::Move);
        }

        for expected_field in expected_items {
            if !seen.contains(&expected_field.name.0) {
                self.diagnostic(
                    "E0007",
                    format!(
                        "missing field `{}` in variant `{}`",
                        expected_field.name.0, variant.name.0
                    ),
                    span,
                );
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

    pub(super) fn check_match(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        scrutinee: ExprId,
        arms: &[MatchArm],
        expected: Option<&Type>,
        span: Option<rowan::TextRange>,
    ) -> Type {
        let scrutinee_ty = self.check_expr(ctx, scrutinee);
        let mut result = None;
        let mut scrutinee_use = ValueUse::Shared;

        for arm in arms {
            ctx.push_scope();
            self.bind_pattern(ctx, arm.pat, &scrutinee_ty);
            scrutinee_use = scrutinee_use.merge(self.pattern_value_use(ctx, arm.pat));
            self.report_duplicate_pattern_bindings(ctx, arm.pat);
            if let Some(guard) = arm.guard {
                let guard_ty = self.check_expr(ctx, guard);
                self.expect_assignable(
                    &Type::Bool,
                    &guard_ty,
                    "match guard",
                    ctx.expr_range(guard),
                );
            }
            let arm_ty = match expected {
                Some(expected) => self.check_expr_expected(ctx, arm.body, expected),
                None => self.check_expr(ctx, arm.body),
            };
            ctx.pop_scope();

            result = Some(
                if let Some(expected @ Type::OpaqueCallable { .. }) = expected {
                    self.expect_assignable(expected, &arm_ty, "opaque callable return", span);
                    expected.clone()
                } else {
                    match result {
                        None => arm_ty,
                        Some(prev) => self.join_branch_types(prev, arm_ty, "match arms", span),
                    }
                },
            );
        }
        self.record_value_use(ctx, scrutinee, scrutinee_use);

        let missing = self.missing_match_pattern(ctx, arms, &scrutinee_ty);
        if let Some((pattern, range_notes)) = &missing {
            let message = if pattern == "_" {
                "non-exhaustive match; missing pattern `_`; add a wildcard arm".to_string()
            } else {
                format!("non-exhaustive match; missing pattern `{pattern}`")
            };
            self.diagnostic("E0039", message, span);
            if !range_notes.is_empty()
                && let Some(diagnostic) = self.result.diagnostics.last_mut()
            {
                diagnostic.notes.splice(0..0, range_notes.iter().cloned());
            }
        }

        let exhaustive = !scrutinee_ty.is_unknown_like() && missing.is_none();
        let all_arms_return = arms
            .iter()
            .all(|arm| self.expr_always_returns(ctx, arm.body));
        if scrutinee_ty.is_never() || (exhaustive && all_arms_return) {
            Type::Never
        } else {
            result.unwrap_or(Type::Unit)
        }
    }

    pub(super) fn check_for(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        expr_id: ExprId,
        pat: PatId,
        iterable: ExprId,
        body: ExprId,
        span: Option<rowan::TextRange>,
    ) -> Type {
        let iterable_ty = self.check_expr(ctx, iterable);
        self.record_value_use(ctx, iterable, ValueUse::Move);
        let Some(into_iter_trait) = self.find_trait_by_name("IntoIterator") else {
            if let Type::Array(item_ty, _) = &iterable_ty {
                ctx.push_scope();
                self.bind_pattern(ctx, pat, item_ty);
                ctx.loop_depth += 1;
                self.check_expr(ctx, body);
                ctx.loop_depth -= 1;
                self.record_value_use(ctx, body, ValueUse::Move);
                ctx.pop_scope();
                return Type::Unit;
            }
            self.diagnostic("E0035", "missing `IntoIterator` trait", span);
            return Type::Unit;
        };

        if !self.type_has_trait_id(ctx, &iterable_ty, into_iter_trait) {
            self.diagnostic(
                "E0035",
                format!(
                    "type `{}` cannot be used in a for loop because it does not implement `IntoIterator`",
                    iterable_ty.display(self.hir)
                ),
                ctx.expr_range(iterable),
            );
        }

        let item_ty = self
            .associated_type_for(ctx, &iterable_ty, into_iter_trait, "Item")
            .unwrap_or(Type::Unknown);
        let into_iter_ty = self
            .associated_type_for(ctx, &iterable_ty, into_iter_trait, "IntoIter")
            .unwrap_or(Type::Unknown);

        if item_ty.is_unknown_like() || into_iter_ty.is_unknown_like() {
            self.diagnostic(
                "E0035",
                "`IntoIterator` must define `Item` and `IntoIter` for use in a for loop",
                span,
            );
        }

        if let Some(iterator_trait) = self.find_trait_by_name("Iterator") {
            if !into_iter_ty.is_unknown_like()
                && !self.type_has_trait_id(ctx, &into_iter_ty, iterator_trait)
            {
                self.diagnostic(
                    "E0035",
                    format!(
                        "`IntoIterator::IntoIter` type `{}` does not implement `Iterator`",
                        into_iter_ty.display(self.hir)
                    ),
                    ctx.expr_range(iterable),
                );
            }
            if let Some(iter_item_ty) =
                self.associated_type_for(ctx, &into_iter_ty, iterator_trait, "Item")
            {
                self.expect_assignable(
                    &item_ty,
                    &iter_item_ty,
                    "iterator item",
                    ctx.expr_range(iterable),
                );
            }

            let has_into_iter = self.hir.item_tree.traits[into_iter_trait]
                .methods
                .iter()
                .any(|method| method.name.0 == "into_iter");
            if !has_into_iter {
                self.diagnostic("E0035", "`IntoIterator` must define `into_iter`", span);
            }
            if has_into_iter
                && let Some((next_ty, some_variant)) =
                    self.iterator_next_protocol(ctx, iterator_trait, &item_ty, span)
                && !item_ty.is_unknown_like()
                && !into_iter_ty.is_unknown_like()
            {
                if self.hir.item_tree.traits[into_iter_trait]
                    .methods
                    .iter()
                    .find(|method| method.name.0 == "into_iter")
                    .is_some_and(|method| method.is_unsafe)
                    || self.hir.item_tree.traits[iterator_trait]
                        .methods
                        .iter()
                        .find(|method| method.name.0 == "next")
                        .is_some_and(|method| method.is_unsafe)
                {
                    self.require_unsafe(ctx, "calling an unsafe function", span);
                }
                self.result.for_loops.insert(
                    (ctx.body_id, expr_id),
                    ForLoopInfo {
                        into_iter: TraitMethodCall {
                            trait_id: into_iter_trait,
                            method: "into_iter".into(),
                        },
                        next: TraitMethodCall {
                            trait_id: iterator_trait,
                            method: "next".into(),
                        },
                        item_ty: item_ty.clone(),
                        iter_ty: into_iter_ty.clone(),
                        next_ty,
                        some_variant,
                    },
                );
            }
        } else {
            self.diagnostic("E0035", "missing `Iterator` trait", span);
        }

        ctx.push_scope();
        self.bind_pattern(ctx, pat, &item_ty);
        ctx.loop_depth += 1;
        self.check_expr(ctx, body);
        ctx.loop_depth -= 1;
        self.record_value_use(ctx, body, ValueUse::Move);
        ctx.pop_scope();

        Type::Unit
    }

    pub(super) fn iterator_next_protocol(
        &mut self,
        ctx: &BodyCtx<'_>,
        iterator_trait: TraitId,
        item_ty: &Type,
        span: Option<rowan::TextRange>,
    ) -> Option<(Type, usize)> {
        let Some(next) = self.hir.item_tree.traits[iterator_trait]
            .methods
            .iter()
            .find(|method| method.name.0 == "next")
            .cloned()
        else {
            self.diagnostic("E0035", "`Iterator` must define `next`", span);
            return None;
        };
        let valid_return = next.ret_type.as_ref().is_some_and(|ret| {
            let HirTypeRef::Named(path) = ret else {
                return false;
            };
            let Some(name) = path.as_single_name() else {
                return false;
            };
            if name.0 != "Option" || path.type_args.len() != 1 {
                return false;
            }
            match &path.type_args[0] {
                HirTypeRef::Named(item)
                    if item.segments.len() == 2
                        && item.segments[0].0 == "Self"
                        && item.segments[1].0 == "Item" =>
                {
                    true
                }
                other => {
                    let actual = self.lower_type_ref_with_params_at(
                        other,
                        &ctx.generic_params,
                        next.ret_type_range.or(Some(next.name_range)),
                    );
                    actual.is_unknown_like() || self.bound_types_match(item_ty, &actual)
                }
            }
        });
        let option_id = self.find_enum_by_name("Option");
        let some_variant = option_id.and_then(|option_id| {
            self.hir.item_tree.enums[option_id]
                .variants
                .iter()
                .position(|variant| {
                    variant.name.0 == "Some"
                        && matches!(&variant.kind, HirVariantKind::Tuple(fields) if fields.len() == 1)
                })
        });
        let (Some(option_id), Some(some_variant)) = (option_id, some_variant) else {
            self.diagnostic(
                "E0035",
                "for loops require an `Option` enum with a single-field `Some` variant",
                span,
            );
            return None;
        };
        if !valid_return {
            self.diagnostic(
                "E0035",
                "`Iterator::next` must return `Option<Self::Item>`",
                span,
            );
            return None;
        }
        let option = self.hir.item_tree.enums[option_id].clone();
        let HirVariantKind::Tuple(fields) = &option.variants[some_variant].kind else {
            unreachable!();
        };
        let subst = option
            .generics
            .iter()
            .zip([item_ty.clone()])
            .map(|(name, ty)| (name.0.clone(), ty))
            .collect();
        let payload_ty = self.lower_type_ref_with_params_at(
            &fields[0],
            &subst,
            option.variants[some_variant]
                .field_ranges
                .first()
                .copied()
                .or(Some(option.variants[some_variant].name_range)),
        );
        if !self.bound_types_match(item_ty, &payload_ty) {
            self.diagnostic(
                "E0035",
                "`Option::Some` payload must match `Iterator::Item`",
                span,
            );
            return None;
        }
        Some((Type::Enum(option_id, vec![item_ty.clone()]), some_variant))
    }

    pub(super) fn check_array(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        elements: &[ExprId],
        expected: Option<&Type>,
        span: Option<rowan::TextRange>,
    ) -> Type {
        let (expected_element, expected_len) = match expected {
            Some(Type::Array(inner, len)) => (Some(inner.as_ref()), len.as_usize()),
            Some(Type::Slice(inner)) => (Some(inner.as_ref()), None),
            _ => (None, None),
        };
        let mut element_ty = None;
        for element in elements {
            let ty = match expected_element {
                Some(expected) => self.check_expr_expected(ctx, *element, expected),
                None => self.check_expr(ctx, *element),
            };
            let elem_span = ctx.expr_range(*element);
            element_ty = Some(match element_ty {
                None => ty,
                Some(prev) => {
                    self.expect_assignable(&prev, &ty, "array element", elem_span);
                    prev.or(ty)
                }
            });
        }
        for element in elements {
            self.record_value_use(ctx, *element, ValueUse::Move);
        }
        if let Some(expected_len) = expected_len
            && expected_len != elements.len()
        {
            self.diagnostic(
                "E0001",
                format!(
                    "array length mismatch: expected {}, got {}",
                    expected_len,
                    elements.len()
                ),
                span,
            );
        }

        let ty = Type::Array(
            Box::new(
                element_ty
                    .or_else(|| expected_element.cloned())
                    .unwrap_or(Type::Unknown),
            ),
            ConstArg::Value(elements.len()),
        );
        self.expect_sized_value(&ty, span);
        ty
    }

    pub(super) fn check_array_repeat(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        value: ExprId,
        len: ExprId,
        expected: Option<&Type>,
        span: Option<rowan::TextRange>,
    ) -> Type {
        let (expected_element, expected_len) = match expected {
            Some(Type::Array(inner, len)) => (Some(inner.as_ref()), len.as_usize()),
            _ => (None, None),
        };
        let value_ty = match expected_element {
            Some(expected) => self.check_expr_expected(ctx, value, expected),
            None => self.check_expr(ctx, value),
        };
        self.record_value_use(ctx, value, ValueUse::Copy);
        if !self.repeat_value_is_copy(&value_ty) {
            self.diagnostic(
                "E0031",
                format!(
                    "array repeat value must be Copy, got {}",
                    value_ty.display(self.hir)
                ),
                ctx.expr_range(value),
            );
        }
        let len_ty = self.check_expr(ctx, len);
        self.record_value_use(ctx, len, ValueUse::Move);
        if !matches!(len_ty, Type::Int(_)) {
            self.expect_assignable(
                &Type::Int(IntTy::I32),
                &len_ty,
                "array length",
                ctx.expr_range(len),
            );
        }
        let len_value = match &ctx.body.exprs[len] {
            Expr::IntLiteral { value, .. } => match usize::try_from(*value) {
                Ok(value) => value,
                Err(_) => {
                    self.diagnostic(
                        "E0002",
                        "array repeat length must be an integer literal that fits `usize`",
                        ctx.expr_range(len),
                    );
                    0
                }
            },
            _ => {
                self.diagnostic(
                    "E0002",
                    "array repeat length must be an integer literal that fits `usize`",
                    ctx.expr_range(len),
                );
                0
            }
        };
        if let Some(expected_len) = expected_len
            && expected_len != len_value
        {
            self.diagnostic(
                "E0001",
                format!(
                    "array length mismatch: expected {}, got {}",
                    expected_len, len_value
                ),
                span,
            );
        }

        let ty = Type::Array(Box::new(value_ty), ConstArg::Value(len_value));
        self.expect_sized_value(&ty, span);
        ty
    }

    pub(super) fn repeat_value_is_copy(&self, ty: &Type) -> bool {
        match ty {
            Type::Array(inner, _) => self.repeat_value_is_copy(inner),
            Type::Tuple(elements) => elements.iter().all(|elem| self.repeat_value_is_copy(elem)),
            _ => self.result.trait_env.type_is_copy(ty),
        }
    }

    pub(super) fn seed_type_inference<'b>(
        &mut self,
        names: impl Iterator<Item = &'b str>,
        subst: &mut HashMap<String, Type>,
    ) {
        for name in names {
            if !subst.contains_key(name) {
                subst.insert(name.to_string(), self.fresh_infer());
            }
        }
    }
}
