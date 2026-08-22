use super::{
    Body, Builder, BuiltinOperator, Expr, ExprId, FuncRef, HirBinOp, HirTypeRef, HirUnOp, Inst,
    InstKind, IntTy, LowerCtx, PathAnchor, ResolvedName, Type, UnOp, Value,
    builtin_comparison_types, comparison_trait, convert_cmp_op, convert_unop,
};

impl LowerCtx<'_> {
    pub(super) fn is_std_range_expr(&self, expr: ExprId) -> bool {
        self.current_body
            .and_then(|bid| self.type_result.expr_types.get(&(bid, expr)))
            .and_then(|ty| match ty {
                type_checker::Type::Struct(sid, _) => Some(*sid),
                _ => None,
            })
            .is_some_and(|sid| {
                let s = &self.hir.item_tree.structs[sid];
                if s.name.0 != "Range" || s.fields.len() != 2 {
                    return false;
                }
                let is_i32 = |ty: &HirTypeRef| {
                    matches!(ty, HirTypeRef::Named(p)
                        if p.anchor == PathAnchor::Plain
                            && p.segments.len() == 1
                            && p.segments[0].0 == "i32"
                            && p.type_args.is_empty())
                };
                s.fields[0].name.0 == "start"
                    && s.fields[1].name.0 == "end"
                    && is_i32(&s.fields[0].ty)
                    && is_i32(&s.fields[1].ty)
            })
    }

    pub(super) fn array_iter_info(&self, expr: ExprId) -> Option<(Type, usize)> {
        self.current_body
            .and_then(|bid| self.type_result.expr_types.get(&(bid, expr)))
            .and_then(|ty| match ty {
                type_checker::Type::Array(inner, len) => {
                    Some((self.convert_type(inner), len.as_usize()?))
                }
                _ => None,
            })
    }

    pub(super) fn callee_function_id(&self, callee: ExprId) -> Option<hir::item_tree::FunctionId> {
        self.current_body
            .and_then(|bid| self.type_result.expr_types.get(&(bid, callee)))
            .and_then(|ty| match ty {
                type_checker::Type::FunctionItem { function: fid, .. } => Some(*fid),
                _ => None,
            })
    }

    pub(super) fn lower_builtin_call(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        callee: ExprId,
        args: &[ExprId],
        result_ty: Type,
    ) -> Option<Value> {
        let fid = self.callee_function_id(callee)?;
        let function = &self.hir.item_tree.functions[fid];
        if !self.hir.std_loaded || self.hir.package_for_range(function.name_range).is_some() {
            return None;
        }
        let builtin = function
            .attrs
            .iter()
            .find(|attr| attr.name.0 == "builtin")
            .and_then(|attr| attr.value.clone())?;
        let body_id = self.current_body?;
        let generic_call = self
            .type_result
            .generic_calls
            .get(&(body_id, callee))
            .cloned();
        match builtin.as_str() {
            "panic" => {
                let message = self.lower_expr(builder, param_values, body, *args.first()?);
                let range = body.source_map.expr_ranges.get(&callee)?;
                let offset: usize = range.start().into();
                // ponytail: locations use the combined source; pass file mappings into MIR when module-accurate paths are needed.
                let (line, column) = line_column(self.source, offset);
                Some(builder.panic(message, line, column))
            }
            "panic_at" => {
                let message = self.lower_expr(builder, param_values, body, *args.first()?);
                let Expr::IntLiteral { value: line, .. } = body.exprs[*args.get(1)?] else {
                    return None;
                };
                let Expr::IntLiteral { value: column, .. } = body.exprs[*args.get(2)?] else {
                    return None;
                };
                Some(builder.panic(
                    message,
                    u32::try_from(line).ok()?,
                    u32::try_from(column).ok()?,
                ))
            }
            "size_of" => {
                let ty = self.convert_type(generic_call.as_ref()?.args.first()?);
                Some(builder.size_of(ty))
            }
            "replace" => {
                let destination = self.lower_expr(builder, param_values, body, *args.first()?);
                let source = self.lower_expr(builder, param_values, body, *args.get(1)?);
                let previous = builder.load(destination, result_ty);
                builder.store(source, destination);
                Some(previous)
            }
            _ => None,
        }
    }

    pub(super) fn lower_static_trait_call(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        call: (ExprId, ExprId, &[ExprId], Type),
    ) -> Option<Value> {
        let (expr_id, callee, args, result_ty) = call;
        let body_id = self.current_body?;
        let trait_call = self
            .type_result
            .trait_method_calls
            .get(&(body_id, expr_id))
            .cloned()?;

        if let hir::body::Expr::FieldAccess { base, .. } = &body.exprs[callee] {
            if trait_call.dynamic {
                let object = self.lower_expr(builder, param_values, body, *base);
                let object_ty =
                    self.convert_type(self.type_result.expr_types.get(&(body_id, *base))?);
                let Type::Struct(struct_ty) = object_ty else {
                    return None;
                };
                let field_name = format!("method_{}", trait_call.method);
                let (slot, method_ty) =
                    struct_ty
                        .fields
                        .iter()
                        .enumerate()
                        .find_map(|(index, (name, ty))| {
                            (name == &field_name).then(|| match ty {
                                Type::FnPtr(signature) => (index, Type::FnPtr(signature.clone())),
                                _ => (index, ty.clone()),
                            })
                        })?;
                let Type::FnPtr(_) = method_ty else {
                    return None;
                };
                let data = builder.extract_value(object, 0, Type::Ptr(Box::new(Type::Unit)));
                let mut values =
                    self.lower_expr_sequence(builder, param_values, body, expr_id, 1, args);
                values.insert(0, data);
                let method = builder.extract_value(object, slot, method_ty);
                return Some(builder.call_indirect(method, values, result_ty));
            }
            let receiver_ty = self
                .type_result
                .expr_types
                .get(&(body_id, *base))
                .cloned()?;
            let rhs_ty = args
                .first()
                .and_then(|arg| self.type_result.expr_types.get(&(body_id, *arg)))
                .cloned();
            let callee_ty = self
                .type_result
                .expr_types
                .get(&(body_id, callee))
                .cloned()
                .unwrap_or_else(|| receiver_ty.clone());
            let candidates = [
                (&receiver_ty, rhs_ty.as_ref()),
                (&receiver_ty, None),
                (&callee_ty, rhs_ty.as_ref()),
                (&callee_ty, None),
            ];
            let (fid, dispatch_ty, dispatch_rhs) =
                candidates.into_iter().find_map(|(ty, rhs)| {
                    self.find_trait_impl_method(trait_call.trait_id, &trait_call.method, ty, rhs)
                        .map(|fid| (fid, ty, rhs))
                })?;
            if let Some(op) = self.builtin_operator_for_method(fid) {
                return Some(self.lower_builtin_operator_method_call(
                    builder,
                    param_values,
                    body,
                    expr_id,
                    *base,
                    args,
                    op,
                ));
            }
            let receiver_param = self.hir.item_tree.functions[fid].params.first()?.ty.clone();
            let receiver =
                self.lower_receiver_arg(builder, param_values, body, *base, &receiver_param);
            let mut values =
                self.lower_expr_sequence(builder, param_values, body, expr_id, 1, args);
            values.insert(0, receiver);
            let name = self
                .mono_method_name_for_receiver(fid, dispatch_ty, dispatch_rhs)
                .unwrap_or_else(|| self.function_name(fid));
            return Some(builder.call(FuncRef::Local(name), values, result_ty));
        }
        if !matches!(
            body.exprs[callee],
            hir::body::Expr::Path {
                resolved: Some(ResolvedName::Trait(_)),
                ..
            }
        ) {
            return None;
        }
        let receiver_ty = self.type_result.expr_types.get(&(body_id, callee))?;
        let fid = self.find_trait_impl_method(
            trait_call.trait_id,
            &trait_call.method,
            receiver_ty,
            None,
        )?;
        let name = self
            .mono_method_name_for_receiver(fid, receiver_ty, None)
            .unwrap_or_else(|| self.function_name(fid));
        let values = self.lower_expr_sequence(builder, param_values, body, expr_id, 0, args);
        Some(builder.call(FuncRef::Local(name), values, result_ty))
    }

    pub(super) fn lower_operator_call(
        &mut self,
        builder: &mut Builder,
        lhs: ExprId,
        rhs: Option<ExprId>,
        fid: hir::item_tree::FunctionId,
        args: Vec<Value>,
        ret_ty: Type,
    ) -> Value {
        let name = self
            .mono_method_name(fid, lhs, rhs)
            .unwrap_or_else(|| self.function_name(fid));
        builder.call(FuncRef::Local(name), args, ret_ty)
    }

    pub(super) fn lower_comparison(
        &mut self,
        builder: &mut Builder,
        op: HirBinOp,
        lhs: Value,
        rhs: Value,
        lhs_ty: &type_checker::Type,
        rhs_ty: &type_checker::Type,
    ) -> Value {
        let lhs_ty = self.substitute_tc_type(lhs_ty);
        let rhs_ty = self.substitute_tc_type(rhs_ty);

        match (&lhs_ty, &rhs_ty) {
            (type_checker::Type::Tuple(lhs_elements), type_checker::Type::Tuple(rhs_elements))
                if lhs_elements.len() == rhs_elements.len() =>
            {
                let elements = lhs_elements
                    .iter()
                    .zip(rhs_elements)
                    .enumerate()
                    .map(|(index, (lhs_ty, rhs_ty))| {
                        (
                            builder.extract_value(lhs, index, self.convert_type(lhs_ty)),
                            builder.extract_value(rhs, index, self.convert_type(rhs_ty)),
                            lhs_ty.clone(),
                            rhs_ty.clone(),
                        )
                    })
                    .collect();
                return self.lower_aggregate_comparison(builder, op, elements);
            }
            (
                type_checker::Type::Array(lhs_inner, lhs_len),
                type_checker::Type::Array(rhs_inner, rhs_len),
            ) if lhs_len == rhs_len => {
                let Some(len) = lhs_len.as_usize() else {
                    return self.lower_comparison_leaf(builder, op, lhs, rhs, &lhs_ty, &rhs_ty);
                };
                let lhs_mir_ty = self.convert_type(lhs_inner);
                let rhs_mir_ty = self.convert_type(rhs_inner);
                let elements = (0..len)
                    .map(|index| {
                        let index_value = builder.iconst(index as u64, IntTy::Usize);
                        let lhs_ptr = builder.index_ptr(lhs, index_value, lhs_mir_ty.clone());
                        let rhs_ptr = builder.index_ptr(rhs, index_value, rhs_mir_ty.clone());
                        (
                            builder.load(lhs_ptr, lhs_mir_ty.clone()),
                            builder.load(rhs_ptr, rhs_mir_ty.clone()),
                            lhs_inner.as_ref().clone(),
                            rhs_inner.as_ref().clone(),
                        )
                    })
                    .collect();
                return self.lower_aggregate_comparison(builder, op, elements);
            }
            _ => {}
        }

        self.lower_comparison_leaf(builder, op, lhs, rhs, &lhs_ty, &rhs_ty)
    }

    pub(super) fn lower_aggregate_comparison(
        &mut self,
        builder: &mut Builder,
        op: HirBinOp,
        elements: Vec<(Value, Value, type_checker::Type, type_checker::Type)>,
    ) -> Value {
        match op {
            HirBinOp::Eq => self.lower_aggregate_equality(builder, elements, false),
            HirBinOp::Neq => self.lower_aggregate_equality(builder, elements, true),
            HirBinOp::Lt | HirBinOp::Gt | HirBinOp::LtEq | HirBinOp::GtEq => {
                self.lower_aggregate_ordering(builder, op, elements)
            }
            _ => unreachable!("aggregate comparison called with non-comparison op"),
        }
    }

    pub(super) fn lower_aggregate_equality(
        &mut self,
        builder: &mut Builder,
        elements: Vec<(Value, Value, type_checker::Type, type_checker::Type)>,
        negate: bool,
    ) -> Value {
        if elements.is_empty() {
            return builder.bconst(!negate);
        }

        let merge_block = builder.func.new_block_labeled("cmp_merge");
        let mut phi_args = Vec::with_capacity(elements.len() + 1);
        for (lhs, rhs, lhs_ty, rhs_ty) in elements {
            let equal = self.lower_comparison(builder, HirBinOp::Eq, lhs, rhs, &lhs_ty, &rhs_ty);
            let next_block = builder.func.new_block_labeled("cmp_next");
            let result_block = builder.func.new_block_labeled("cmp_result");
            builder.set_cond_branch(equal, next_block, result_block);

            builder.switch_to_block(result_block);
            let result = builder.bconst(negate);
            let result_exit = builder.current_block;
            builder.set_branch(merge_block);
            phi_args.push((result, result_exit));
            builder.switch_to_block(next_block);
        }

        let result = builder.bconst(!negate);
        let result_exit = builder.current_block;
        builder.set_branch(merge_block);
        phi_args.push((result, result_exit));

        builder.switch_to_block(merge_block);
        builder
            .func
            .push_inst(merge_block, Inst::new(InstKind::Phi(phi_args), Type::Bool))
    }

    pub(super) fn lower_aggregate_ordering(
        &mut self,
        builder: &mut Builder,
        op: HirBinOp,
        elements: Vec<(Value, Value, type_checker::Type, type_checker::Type)>,
    ) -> Value {
        if elements.is_empty() {
            return builder.bconst(matches!(op, HirBinOp::LtEq | HirBinOp::GtEq));
        }

        let merge_block = builder.func.new_block_labeled("cmp_merge");
        let mut phi_args = Vec::with_capacity(elements.len() + 1);
        for (lhs, rhs, lhs_ty, rhs_ty) in elements {
            let equal = self.lower_comparison(builder, HirBinOp::Eq, lhs, rhs, &lhs_ty, &rhs_ty);
            let next_block = builder.func.new_block_labeled("cmp_next");
            let result_block = builder.func.new_block_labeled("cmp_result");
            builder.set_cond_branch(equal, next_block, result_block);

            builder.switch_to_block(result_block);
            let decision_op = match op {
                HirBinOp::Lt | HirBinOp::LtEq => HirBinOp::Lt,
                HirBinOp::Gt | HirBinOp::GtEq => HirBinOp::Gt,
                _ => unreachable!("non-ordering op in aggregate ordering"),
            };
            let result = self.lower_comparison(builder, decision_op, lhs, rhs, &lhs_ty, &rhs_ty);
            let result_exit = builder.current_block;
            if builder.needs_return() {
                builder.set_branch(merge_block);
                phi_args.push((result, result_exit));
            }
            builder.switch_to_block(next_block);
        }

        let result = builder.bconst(matches!(op, HirBinOp::LtEq | HirBinOp::GtEq));
        let result_exit = builder.current_block;
        builder.set_branch(merge_block);
        phi_args.push((result, result_exit));

        builder.switch_to_block(merge_block);
        builder
            .func
            .push_inst(merge_block, Inst::new(InstKind::Phi(phi_args), Type::Bool))
    }

    pub(super) fn lower_comparison_leaf(
        &mut self,
        builder: &mut Builder,
        op: HirBinOp,
        lhs: Value,
        rhs: Value,
        lhs_ty: &type_checker::Type,
        rhs_ty: &type_checker::Type,
    ) -> Value {
        if matches!(
            (lhs_ty, rhs_ty),
            (type_checker::Type::Unit, type_checker::Type::Unit)
        ) {
            return builder.bconst(matches!(op, HirBinOp::Eq | HirBinOp::LtEq | HirBinOp::GtEq));
        }
        if builtin_comparison_types(op, lhs_ty, rhs_ty) {
            return builder.cmp(convert_cmp_op(op), lhs, rhs);
        }

        if let Some((lang, method)) = comparison_trait(op)
            && let Some(lang_item) = type_checker::lang_items::LangItem::from_name(lang)
            && let Some(trait_id) = self.type_result.trait_env.lang_items.get(lang_item)
            && let Some(fid) = self.find_trait_impl_method(trait_id, method, lhs_ty, Some(rhs_ty))
        {
            let Some(receiver_ty) = self.hir.item_tree.functions[fid]
                .params
                .first()
                .map(|param| param.ty.clone())
            else {
                return builder.cmp(convert_cmp_op(op), lhs, rhs);
            };
            let Some(rhs_param_ty) = self.hir.item_tree.functions[fid]
                .params
                .get(1)
                .map(|param| param.ty.clone())
            else {
                return builder.cmp(convert_cmp_op(op), lhs, rhs);
            };
            let lhs_arg = self.lower_comparison_arg(builder, lhs, lhs_ty, &receiver_ty);
            let rhs_arg = self.lower_comparison_arg(builder, rhs, rhs_ty, &rhs_param_ty);
            let name = self
                .mono_method_name_for_receiver(fid, lhs_ty, Some(rhs_ty))
                .unwrap_or_else(|| self.function_name(fid));
            return builder.call(FuncRef::Local(name), vec![lhs_arg, rhs_arg], Type::Bool);
        }

        builder.cmp(convert_cmp_op(op), lhs, rhs)
    }

    pub(super) fn lower_comparison_arg(
        &self,
        builder: &mut Builder,
        value: Value,
        actual_ty: &type_checker::Type,
        expected: &hir::item_tree::HirTypeRef,
    ) -> Value {
        let actual_mir_ty = self.convert_type(actual_ty);
        match expected {
            hir::item_tree::HirTypeRef::Ref(_, _) if matches!(actual_mir_ty, Type::Ref(_, _)) => {
                value
            }
            hir::item_tree::HirTypeRef::Ref(_, mutable) => {
                let place = builder.alloca(actual_mir_ty.clone());
                builder.store(value, place);
                builder.unop(
                    if *mutable { UnOp::MutRef } else { UnOp::Ref },
                    place,
                    Type::Ref(Box::new(actual_mir_ty), *mutable),
                )
            }
            _ => value,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn lower_builtin_operator_method_call(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        owner: ExprId,
        base: ExprId,
        args: &[ExprId],
        op: BuiltinOperator,
    ) -> Value {
        let value_ty = self
            .current_body
            .and_then(|bid| self.type_result.expr_types.get(&(bid, base)))
            .map_or(Type::Unit, |ty| self.convert_type(ty));
        match op {
            BuiltinOperator::Binary(op) => {
                let expressions = [
                    base,
                    *args.first().expect("checked binary operator missing rhs"),
                ];
                let values =
                    self.lower_expr_sequence(builder, param_values, body, owner, 0, &expressions);
                let [lhs, rhs] = values.as_slice() else {
                    unreachable!();
                };
                builder.binop(op, *lhs, *rhs, value_ty)
            }
            BuiltinOperator::Unary(op) => {
                let operand = self.lower_expr(builder, param_values, body, base);
                builder.unop(op, operand, value_ty)
            }
            BuiltinOperator::Assign(op) => {
                let rhs = self.lower_expr(
                    builder,
                    param_values,
                    body,
                    *args
                        .first()
                        .expect("checked assignment operator missing rhs"),
                );
                let place = self.lower_lvalue(builder, param_values, body, base);
                let lhs = builder.load(place, value_ty.clone());
                let value = builder.binop(op, lhs, rhs, value_ty);
                builder.store(value, place);
                builder.unit_const()
            }
        }
    }

    pub(super) fn actual_method_fid(
        &self,
        callee: ExprId,
        fid: hir::item_tree::FunctionId,
        base: ExprId,
    ) -> hir::item_tree::FunctionId {
        let Some(body_id) = self.current_body else {
            return fid;
        };
        let Some(receiver_ty) = self.type_result.expr_types.get(&(body_id, base)) else {
            return fid;
        };
        if let Some(call) = self.type_result.trait_method_calls.get(&(body_id, callee)) {
            return self
                .find_trait_impl_method(call.trait_id, &call.method, receiver_ty, None)
                .unwrap_or(fid);
        }
        let Some(imp) = self.impl_for_method(fid) else {
            return fid;
        };
        if self.impl_type_matches(imp, receiver_ty) {
            return fid;
        }
        let Some(trait_ty) = &imp.trait_ty else {
            return fid;
        };
        let Some(trait_id) = self.resolve_trait_ref(trait_ty) else {
            return fid;
        };
        let method_name = &self.hir.item_tree.functions[fid].name;
        self.find_trait_impl_method(trait_id, &method_name.0, receiver_ty, None)
            .unwrap_or(fid)
    }

    pub(super) fn find_trait_impl_method(
        &self,
        trait_id: hir::item_tree::TraitId,
        method_name: &str,
        receiver_ty: &type_checker::Type,
        rhs_ty: Option<&type_checker::Type>,
    ) -> Option<hir::item_tree::FunctionId> {
        let receiver_ty = self.substitute_tc_type(receiver_ty);
        let dereferenced = match &receiver_ty {
            type_checker::Type::Ref(inner, _) => Some(inner.as_ref()),
            _ => None,
        };
        for receiver_ty in std::iter::once(&receiver_ty).chain(dereferenced) {
            for (_, candidate) in self.hir.item_tree.impls.iter() {
                let Some(candidate_trait) = candidate.trait_ty.as_ref() else {
                    continue;
                };
                if self.resolve_trait_ref(candidate_trait) != Some(trait_id)
                    || !self.impl_type_matches(candidate, receiver_ty)
                    || !self.impl_trait_args_match(candidate, receiver_ty, rhs_ty)
                {
                    continue;
                }
                return candidate
                    .methods
                    .iter()
                    .copied()
                    .find(|candidate_fid| {
                        self.hir.item_tree.functions[*candidate_fid].name.0 == method_name
                    })
                    .or_else(|| self.default_method(trait_id, method_name));
            }
        }
        None
    }

    pub(super) fn default_method(
        &self,
        trait_id: hir::item_tree::TraitId,
        method_name: &str,
    ) -> Option<hir::item_tree::FunctionId> {
        self.hir.item_tree.traits[trait_id]
            .default_methods
            .iter()
            .copied()
            .find(|fid| self.hir.item_tree.functions[*fid].name.0 == method_name)
    }

    pub(super) fn lower_receiver_arg(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        base: ExprId,
        expected: &hir::item_tree::HirTypeRef,
    ) -> Value {
        let base_ty = self
            .current_body
            .and_then(|bid| self.type_result.expr_types.get(&(bid, base)))
            .map_or(Type::Unit, |t| self.convert_type(t));

        match expected {
            hir::item_tree::HirTypeRef::Ref(_, _) if matches!(base_ty, Type::Ref(_, _)) => {
                self.lower_expr(builder, param_values, body, base)
            }
            hir::item_tree::HirTypeRef::Ref(_, true) => {
                let place = self.lower_lvalue(builder, param_values, body, base);
                builder.unop(
                    convert_unop(HirUnOp::MutRef),
                    place,
                    Type::Ref(Box::new(base_ty), true),
                )
            }
            hir::item_tree::HirTypeRef::Ref(_, mutable) => {
                let base_val = self.lower_lvalue(builder, param_values, body, base);
                let expected_ty = Type::Ref(Box::new(base_ty), *mutable);
                builder.unop(convert_unop(HirUnOp::Ref), base_val, expected_ty)
            }
            _ => self.lower_expr(builder, param_values, body, base),
        }
    }

    pub(super) fn lower_trait_index_place(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        expr_id: ExprId,
        base: ExprId,
        index: ExprId,
    ) -> Option<Value> {
        let body_id = self.current_body?;
        let call = self
            .type_result
            .trait_method_calls
            .get(&(body_id, expr_id))?
            .clone();
        if call.method != "index" && call.method != "index_mut" {
            return None;
        }
        let base_ty = self
            .type_result
            .expr_types
            .get(&(body_id, base))
            .cloned()
            .map(|ty| self.substitute_tc_type(&ty))?;
        let receiver_ty = match &base_ty {
            type_checker::Type::Ref(inner, _) => inner.as_ref().clone(),
            _ => base_ty.clone(),
        };
        let index_ty = self
            .type_result
            .expr_types
            .get(&(body_id, index))
            .cloned()
            .map(|ty| self.substitute_tc_type(&ty))?;
        let fid = self.find_trait_impl_method(
            call.trait_id,
            &call.method,
            &receiver_ty,
            Some(&index_ty),
        )?;
        let function = &self.hir.item_tree.functions[fid];
        let receiver_param = function.params.first()?.ty.clone();
        let index_param = function.params.get(1)?.ty.clone();
        let name = self
            .mono_method_name_for_receiver(fid, &receiver_ty, Some(&index_ty))
            .unwrap_or_else(|| self.function_name(fid));
        let receiver = self.lower_receiver_arg(builder, param_values, body, base, &receiver_param);
        let index = self.lower_receiver_arg(builder, param_values, body, index, &index_param);
        let output = self
            .type_result
            .expr_types
            .get(&(body_id, expr_id))
            .cloned()
            .map(|ty| self.convert_type(&ty))?;
        let result = Type::Ref(Box::new(output), call.method == "index_mut");
        Some(builder.call(FuncRef::Local(name), vec![receiver, index], result))
    }
}

fn line_column(source: &str, offset: usize) -> (u32, u32) {
    let mut line = 1;
    let mut column = 1;
    for (index, ch) in source.char_indices() {
        if index >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}
