use super::*;

impl TypeChecker<'_> {
    pub(super) fn bind_pattern(&mut self, ctx: &mut BodyCtx<'_>, pat: PatId, expected: &Type) {
        self.bind_pattern_with_mode(ctx, pat, expected, PatternBindingMode::Move, true);
    }

    pub(super) fn bind_pattern_with_mode(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        pat: PatId,
        expected: &Type,
        mut mode: PatternBindingMode,
        allow_reference_deref: bool,
    ) {
        let span = ctx.pat_range(pat);
        let pattern = ctx.body.pats[pat].clone();
        let mut effective = expected.clone();
        if self.pattern_uses_match_ergonomics(&pattern, expected) {
            let mut reported_missing_initializer = false;
            loop {
                if matches!(
                    (&pattern, &effective),
                    (
                        Pattern::Literal(LiteralPattern::String(_)),
                        Type::Ref(inner, false)
                    ) if inner.as_ref() == &Type::Str
                ) {
                    break;
                }
                let Type::Ref(inner, mutable) = &effective else {
                    break;
                };
                if !allow_reference_deref && !reported_missing_initializer {
                    self.diagnostic(
                        "E0010",
                        "reference destructuring requires an initializer",
                        span,
                    );
                    reported_missing_initializer = true;
                }
                mode = mode.through_reference(*mutable);
                effective = inner.as_ref().clone();
            }
        }
        self.result
            .pattern_types
            .insert((ctx.body_id, pat), effective.clone());
        match pattern {
            Pattern::Wildcard => {}
            Pattern::Reference { mutable, pattern } => {
                if mode != PatternBindingMode::Move {
                    self.diagnostic(
                        "E0010",
                        "reference patterns may only be written when the default binding mode is `move`",
                        span,
                    );
                }
                if !allow_reference_deref {
                    self.diagnostic("E0010", "reference pattern requires an initializer", span);
                }
                let Type::Ref(inner, actual_mutable) = &effective else {
                    if !effective.is_unknown_like() {
                        self.diagnostic(
                            "E0010",
                            format!(
                                "reference pattern cannot match value of type {}",
                                effective.display(self.hir)
                            ),
                            span,
                        );
                    }
                    self.bind_pattern_with_mode(
                        ctx,
                        pattern,
                        &Type::Unknown,
                        PatternBindingMode::Move,
                        allow_reference_deref,
                    );
                    return;
                };
                if mutable != *actual_mutable {
                    self.diagnostic(
                        "E0010",
                        format!(
                            "reference pattern mutability does not match value of type {}",
                            effective.display(self.hir)
                        ),
                        span,
                    );
                }
                self.bind_pattern_with_mode(
                    ctx,
                    pattern,
                    inner,
                    PatternBindingMode::Move,
                    allow_reference_deref,
                );
            }
            Pattern::Literal(literal) => {
                let literal_ty = self.literal_pattern_type(&literal, Some(&effective), span);
                self.expect_assignable(&effective, &literal_ty, "literal pattern", span);
                if let LiteralPattern::Int {
                    value, valid: true, ..
                } = literal
                    && let Type::Int(ty) = literal_ty
                    && !ty.contains_u64(value)
                {
                    self.diagnostic(
                        "E0011",
                        format!(
                            "integer literal `{value}` is out of range for `{}`",
                            ty.as_str()
                        ),
                        span,
                    );
                }
            }
            Pattern::Path { path } => {
                self.validate_unit_variant_pattern(&effective, &path, span);
            }
            Pattern::Binding { name, is_mut } => {
                if let Some(is_unit) = self.enum_variant_is_unit(&effective, &name.0) {
                    if !is_unit {
                        self.diagnostic(
                            "E0038",
                            format!("variant `{}` requires a payload pattern", name.0),
                            span,
                        );
                    }
                } else {
                    if is_mut && mode != PatternBindingMode::Move {
                        self.diagnostic(
                            "E0010",
                            "`mut` bindings may only be written when the default binding mode is `move`",
                            span,
                        );
                    }
                    self.record_pattern_binding(
                        ctx,
                        name.0,
                        mode.binding_type(effective.clone()),
                        PatternBindingId {
                            pattern: pat,
                            field: None,
                        },
                        is_mut,
                        mode,
                    );
                }
            }
            Pattern::Tuple { elements } => {
                if elements.is_empty() && effective == Type::Unit {
                    return;
                }
                let Type::Tuple(expected_elements) = &effective else {
                    if !effective.is_unknown_like() {
                        self.diagnostic(
                            "E0010",
                            format!(
                                "tuple pattern cannot match value of type {}",
                                effective.display(self.hir)
                            ),
                            span,
                        );
                    }
                    for element in elements {
                        self.bind_pattern_with_mode(
                            ctx,
                            element,
                            &Type::Unknown,
                            mode,
                            allow_reference_deref,
                        );
                    }
                    return;
                };

                if elements.len() != expected_elements.len() {
                    self.diagnostic(
                        "E0010",
                        format!(
                            "tuple pattern expects {} element(s), got {}",
                            expected_elements.len(),
                            elements.len()
                        ),
                        span,
                    );
                }
                for (index, element) in elements.into_iter().enumerate() {
                    let ty = expected_elements.get(index).unwrap_or(&Type::Unknown);
                    self.bind_pattern_with_mode(ctx, element, ty, mode, allow_reference_deref);
                }
            }
            Pattern::TupleStruct { path, elements } => {
                self.bind_tuple_variant_pattern(
                    ctx,
                    &effective,
                    &path,
                    &elements,
                    span,
                    mode,
                    allow_reference_deref,
                );
            }
            Pattern::Struct { path, fields } => {
                self.bind_struct_variant_pattern(
                    ctx,
                    pat,
                    &effective,
                    &path,
                    &fields,
                    span,
                    mode,
                    allow_reference_deref,
                );
            }
        }
    }

    pub(super) fn pattern_uses_match_ergonomics(&self, pattern: &Pattern, expected: &Type) -> bool {
        match pattern {
            Pattern::Literal(_)
            | Pattern::Path { .. }
            | Pattern::Tuple { .. }
            | Pattern::TupleStruct { .. }
            | Pattern::Struct { .. } => true,
            Pattern::Binding { name, .. } => self
                .enum_variant_is_unit_through_refs(expected, &name.0)
                .is_some(),
            Pattern::Wildcard | Pattern::Reference { .. } => false,
        }
    }

    pub(super) fn literal_pattern_type(
        &mut self,
        literal: &LiteralPattern,
        expected: Option<&Type>,
        span: Option<rowan::TextRange>,
    ) -> Type {
        match literal {
            LiteralPattern::Int { suffix, .. } => {
                self.int_literal_type(suffix.as_deref(), expected, span)
            }
            LiteralPattern::Float { suffix, .. } => {
                self.float_literal_type(suffix.as_deref(), expected, span)
            }
            LiteralPattern::String(_) => Type::Ref(Box::new(Type::Str), false),
            LiteralPattern::Char(_) => Type::Char,
            LiteralPattern::Bool(_) => Type::Bool,
        }
    }

    pub(super) fn enum_variant_is_unit(&self, expected: &Type, name: &str) -> Option<bool> {
        let Type::Enum(enum_id, _) = expected else {
            return None;
        };
        self.hir.item_tree.enums[*enum_id]
            .variants
            .iter()
            .find(|variant| variant.name.0 == name)
            .map(|variant| matches!(variant.kind, HirVariantKind::Unit))
    }

    pub(super) fn enum_variant_is_unit_through_refs(
        &self,
        expected: &Type,
        name: &str,
    ) -> Option<bool> {
        let mut expected = expected;
        while let Type::Ref(inner, _) = expected {
            expected = inner;
        }
        self.enum_variant_is_unit(expected, name)
    }

    pub(super) fn validate_unit_variant_pattern(
        &mut self,
        expected: &Type,
        path: &hir::item_tree::HirPath,
        span: Option<rowan::TextRange>,
    ) {
        let Type::Enum(enum_id, _) = expected else {
            self.diagnostic("E0038", "path pattern requires an enum value", span);
            return;
        };
        let Some(index) = self.enum_variant_index(*enum_id, path) else {
            self.diagnostic(
                "E0038",
                format!("unknown variant `{}` for this enum", path.display()),
                span,
            );
            return;
        };
        let variant = &self.hir.item_tree.enums[*enum_id].variants[index];
        if !matches!(variant.kind, HirVariantKind::Unit) {
            self.diagnostic(
                "E0038",
                format!("variant `{}` requires a payload pattern", variant.name.0),
                span,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn bind_tuple_variant_pattern(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        expected: &Type,
        path: &hir::item_tree::HirPath,
        elements: &[PatId],
        span: Option<rowan::TextRange>,
        mode: PatternBindingMode,
        allow_reference_deref: bool,
    ) {
        let Type::Enum(enum_id, args) = expected else {
            self.diagnostic(
                "E0038",
                "tuple variant pattern requires an enum value",
                span,
            );
            for element in elements {
                self.bind_pattern_with_mode(
                    ctx,
                    *element,
                    &Type::Unknown,
                    mode,
                    allow_reference_deref,
                );
            }
            return;
        };
        let enum_data = self.hir.item_tree.enums[*enum_id].clone();
        let Some(index) = self.enum_variant_index(*enum_id, path) else {
            self.diagnostic(
                "E0038",
                format!(
                    "unknown variant `{}` for `{}`",
                    path.display(),
                    enum_data.name.0
                ),
                span,
            );
            return;
        };
        let variant = &enum_data.variants[index];
        let HirVariantKind::Tuple(items) = &variant.kind else {
            self.diagnostic(
                "E0038",
                format!("variant `{}` is not tuple-style", variant.name.0),
                span,
            );
            return;
        };
        if elements.len() != items.len() {
            self.diagnostic(
                "E0038",
                format!(
                    "variant `{}` expects {} field(s), got {}",
                    variant.name.0,
                    items.len(),
                    elements.len()
                ),
                span,
            );
        }
        let subst = enum_data
            .generics
            .iter()
            .chain(enum_data.const_generics.iter())
            .zip(args.iter())
            .map(|(name, ty)| (name.0.clone(), ty.clone()))
            .collect::<HashMap<_, _>>();
        for (index, element) in elements.iter().enumerate() {
            let ty = items
                .get(index)
                .map(|ty| {
                    self.lower_type_ref_with_params_at(
                        ty,
                        &subst,
                        variant
                            .field_ranges
                            .get(index)
                            .copied()
                            .or(Some(variant.name_range)),
                    )
                })
                .unwrap_or(Type::Unknown);
            self.bind_pattern_with_mode(ctx, *element, &ty, mode, allow_reference_deref);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn bind_struct_variant_pattern(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        binding_pat: PatId,
        expected: &Type,
        path: &hir::item_tree::HirPath,
        fields: &[hir::body::FieldPat],
        span: Option<rowan::TextRange>,
        mode: PatternBindingMode,
        allow_reference_deref: bool,
    ) {
        if let Type::Struct(struct_id, args) = expected {
            let strukt = self.hir.item_tree.structs[*struct_id].clone();
            if path
                .segments
                .last()
                .is_none_or(|name| name.0 != strukt.name.0)
            {
                self.diagnostic(
                    "E0038",
                    format!("struct pattern must name `{}`", strukt.name.0),
                    span,
                );
                return;
            }
            let subst = strukt
                .generics
                .iter()
                .chain(strukt.const_generics.iter())
                .zip(args.iter())
                .map(|(name, ty)| (name.0.clone(), ty.clone()))
                .collect::<HashMap<_, _>>();
            let mut seen = HashSet::new();
            for (field_index, field) in fields.iter().enumerate() {
                if !seen.insert(field.name.0.clone()) {
                    self.diagnostic(
                        "E0038",
                        format!("field `{}` is bound more than once", field.name.0),
                        span,
                    );
                    continue;
                }
                let Some(item) = strukt
                    .fields
                    .iter()
                    .find(|item| item.name.0 == field.name.0)
                else {
                    self.diagnostic(
                        "E0038",
                        format!("struct `{}` has no field `{}`", strukt.name.0, field.name.0),
                        span,
                    );
                    continue;
                };
                self.check_struct_field_visibility(ctx, *struct_id, item, span);
                let ty = self.lower_type_ref_with_params_at(&item.ty, &subst, Some(item.ty_range));
                if let Some(pat) = field.pat {
                    self.bind_pattern_with_mode(ctx, pat, &ty, mode, allow_reference_deref);
                } else {
                    self.record_pattern_binding(
                        ctx,
                        field.name.0.clone(),
                        mode.binding_type(ty),
                        PatternBindingId {
                            pattern: binding_pat,
                            field: Some(field_index),
                        },
                        false,
                        mode,
                    );
                }
            }
            return;
        }

        let Type::Enum(enum_id, args) = expected else {
            self.diagnostic(
                "E0038",
                "struct variant pattern requires an enum value",
                span,
            );
            return;
        };
        let enum_data = self.hir.item_tree.enums[*enum_id].clone();
        let Some(index) = self.enum_variant_index(*enum_id, path) else {
            self.diagnostic(
                "E0038",
                format!(
                    "unknown variant `{}` for `{}`",
                    path.display(),
                    enum_data.name.0
                ),
                span,
            );
            return;
        };
        let variant = &enum_data.variants[index];
        let HirVariantKind::Struct(items) = &variant.kind else {
            self.diagnostic(
                "E0038",
                format!("variant `{}` is not struct-style", variant.name.0),
                span,
            );
            return;
        };
        let subst = enum_data
            .generics
            .iter()
            .chain(enum_data.const_generics.iter())
            .zip(args.iter())
            .map(|(name, ty)| (name.0.clone(), ty.clone()))
            .collect::<HashMap<_, _>>();
        let mut seen = HashSet::new();
        for (field_index, field) in fields.iter().enumerate() {
            if !seen.insert(field.name.0.clone()) {
                self.diagnostic(
                    "E0038",
                    format!("field `{}` is bound more than once", field.name.0),
                    span,
                );
                continue;
            }
            let Some(item) = items.iter().find(|item| item.name.0 == field.name.0) else {
                self.diagnostic(
                    "E0038",
                    format!(
                        "variant `{}` has no field `{}`",
                        variant.name.0, field.name.0
                    ),
                    span,
                );
                continue;
            };
            let ty = self.lower_type_ref_with_params_at(&item.ty, &subst, Some(item.ty_range));
            if let Some(pat) = field.pat {
                self.bind_pattern_with_mode(ctx, pat, &ty, mode, allow_reference_deref);
            } else {
                self.record_pattern_binding(
                    ctx,
                    field.name.0.clone(),
                    mode.binding_type(ty),
                    PatternBindingId {
                        pattern: binding_pat,
                        field: Some(field_index),
                    },
                    false,
                    mode,
                );
            }
        }
    }

    pub(super) fn record_pattern_binding(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        name: String,
        ty: Type,
        id: PatternBindingId,
        is_mut: bool,
        mode: PatternBindingMode,
    ) {
        self.result
            .pattern_binding_types
            .insert((ctx.body_id, id), ty.clone());
        self.result
            .pattern_binding_modes
            .insert((ctx.body_id, id), mode);
        ctx.bindings.insert(name, ty, id, is_mut);
    }

    pub(super) fn report_duplicate_pattern_bindings(&mut self, ctx: &BodyCtx<'_>, pat: PatId) {
        fn collect(body: &Body, pat: PatId, bindings: &mut Vec<(PatternBindingId, String)>) {
            match &body.pats[pat] {
                Pattern::Binding { name, .. } => bindings.push((
                    PatternBindingId {
                        pattern: pat,
                        field: None,
                    },
                    name.0.clone(),
                )),
                Pattern::Reference { pattern, .. } => collect(body, *pattern, bindings),
                Pattern::Tuple { elements } | Pattern::TupleStruct { elements, .. } => {
                    for element in elements {
                        collect(body, *element, bindings);
                    }
                }
                Pattern::Struct { fields, .. } => {
                    for (index, field) in fields.iter().enumerate() {
                        if let Some(field_pat) = field.pat {
                            collect(body, field_pat, bindings);
                        } else {
                            bindings.push((
                                PatternBindingId {
                                    pattern: pat,
                                    field: Some(index),
                                },
                                field.name.0.clone(),
                            ));
                        }
                    }
                }
                Pattern::Wildcard | Pattern::Literal(_) | Pattern::Path { .. } => {}
            }
        }

        let mut bindings = Vec::new();
        collect(ctx.body, pat, &mut bindings);
        let mut seen = HashSet::new();
        for (id, name) in bindings {
            if !self
                .result
                .pattern_binding_types
                .contains_key(&(ctx.body_id, id))
            {
                continue;
            }
            if !seen.insert(name.clone()) {
                self.diagnostic(
                    "E0058",
                    format!("identifier `{name}` is bound more than once in the same pattern"),
                    ctx.pat_range(id.pattern),
                );
            }
        }
    }

    /// Binds a `let` pattern. Unlike a match arm, a `let` must be irrefutable:
    /// there is no alternative branch to take when the pattern does not match,
    /// so a binding would otherwise read uninitialized storage.
    pub(super) fn bind_let_pattern(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        pat: PatId,
        expected: &Type,
        stmt_id: StmtId,
        has_initializer: bool,
    ) {
        let before = self.result.diagnostics.len();
        self.bind_pattern_with_mode(
            ctx,
            pat,
            expected,
            PatternBindingMode::Move,
            has_initializer,
        );
        self.report_duplicate_pattern_bindings(ctx, pat);
        // A pattern whose shape already mismatched the type reports a nonsense
        // witness — `let (a, b, c) = (1, 2)` is a wrong arity, not a refutable
        // pattern — so only well-formed patterns reach the coverage check.
        if self.result.diagnostics.len() == before
            && !expected.is_unknown_like()
            && let Some((witness, _)) = self.missing_let_pattern(ctx, pat, expected)
        {
            self.diagnostic(
                "E0057",
                format!("refutable pattern in `let` binding: `{witness}` is not covered"),
                ctx.pat_range(pat).or_else(|| ctx.stmt_range(stmt_id)),
            );
        }
    }

    pub(super) fn mark_delayed_pattern(
        &self,
        ctx: &mut BodyCtx<'_>,
        pat: PatId,
    ) -> Vec<PatternBindingId> {
        fn collect(body: &Body, pat: PatId, ids: &mut Vec<PatternBindingId>) {
            match &body.pats[pat] {
                Pattern::Binding { .. } => ids.push(PatternBindingId {
                    pattern: pat,
                    field: None,
                }),
                Pattern::Reference { pattern, .. } => collect(body, *pattern, ids),
                Pattern::Tuple { elements } | Pattern::TupleStruct { elements, .. } => {
                    for element in elements {
                        collect(body, *element, ids);
                    }
                }
                Pattern::Struct { fields, .. } => {
                    for (index, field) in fields.iter().enumerate() {
                        if let Some(field_pat) = field.pat {
                            collect(body, field_pat, ids);
                        } else {
                            ids.push(PatternBindingId {
                                pattern: pat,
                                field: Some(index),
                            });
                        }
                    }
                }
                Pattern::Wildcard | Pattern::Literal(_) | Pattern::Path { .. } => {}
            }
        }

        let mut ids = Vec::new();
        collect(ctx.body, pat, &mut ids);
        for id in &ids {
            ctx.mark_delayed_binding(*id);
        }
        ids
    }

    pub(super) fn type_of_resolved_name(
        &mut self,
        ctx: &BodyCtx<'_>,
        resolved: Option<&ResolvedName>,
    ) -> Type {
        match resolved {
            Some(ResolvedName::PatternBinding(id)) => self
                .result
                .pattern_binding_types
                .get(&(ctx.body_id, *id))
                .cloned()
                .unwrap_or(Type::Unknown),
            Some(ResolvedName::Param(index)) => ctx
                .function
                .and_then(|function| function.params.get(*index))
                .map(|param| {
                    self.lower_type_ref_with_params_at(
                        &param.ty,
                        &ctx.generic_params,
                        Some(param.ty_range),
                    )
                })
                .unwrap_or(Type::Unknown),
            Some(ResolvedName::LambdaParam { lambda, index }) => ctx
                .lambdas
                .iter()
                .rev()
                .find(|current| current.expr == *lambda)
                .and_then(|current| current.params.get(*index))
                .cloned()
                .unwrap_or(Type::Unknown),
            Some(ResolvedName::Function(fid)) => self.function_item_type(*fid),
            Some(ResolvedName::Struct(sid)) => Type::Struct(*sid, Vec::new()),
            Some(ResolvedName::Const(cid)) => {
                let konst = &self.hir.item_tree.consts[*cid];
                self.lower_type_ref_with_params_at(&konst.ty, &HashMap::new(), Some(konst.ty_range))
            }
            Some(ResolvedName::TypeAlias(tid)) => self.lower_type_alias(*tid),
            Some(ResolvedName::Unresolved) | None => Type::Unknown,
            Some(ResolvedName::Enum(eid)) => Type::Enum(*eid, Vec::new()),
            Some(ResolvedName::EnumVariant(eid, _)) => Type::Enum(*eid, Vec::new()),
            Some(ResolvedName::Trait(_)) | Some(ResolvedName::Module(_)) => Type::Unknown,
        }
    }

    pub(super) fn function_item_type(&mut self, fid: FunctionId) -> Type {
        let function = &self.hir.item_tree.functions[fid];
        let type_count = self.impl_generic_names(fid).len()
            + function.generics.len()
            + function.implicit_generics.len();
        let const_count = self.impl_const_generic_names(fid).len() + function.const_generics.len();
        let mut args = (0..type_count)
            .map(|_| self.fresh_infer())
            .collect::<Vec<_>>();
        args.extend((0..const_count).map(|_| Type::Const(ConstArg::Unknown)));
        Type::FunctionItem {
            function: fid,
            args,
        }
    }
}
