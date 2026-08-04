use super::*;

impl<'a> LowerCtx<'a> {
    // 表达式降级
    pub(super) fn lower_expr(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        expr_id: ExprId,
    ) -> Value {
        // 命中缓存直接返回
        if let Some(&v) = self.expr_cache.get(&expr_id) {
            return v;
        }

        let expr = &body.exprs[expr_id];

        // 从类型检查结果中查表达式类型
        let tc_type = self
            .current_body
            .and_then(|bid| self.type_result.expr_types.get(&(bid, expr_id)));
        let mir_type = tc_type.map(|t| self.convert_type(t)).unwrap_or(Type::Unit);
        let diverges = matches!(mir_type, Type::Never);

        let value = match expr {
            Expr::Missing => builder.unit_const(),

            Expr::IntLiteral { value, suffix } => {
                let ty = parse_int_suffix(suffix.as_deref());
                builder.iconst(*value, ty)
            }

            Expr::FloatLiteral { value, suffix } => {
                // HIR 中 value 已经是 f64，直接使用
                let ty = parse_float_suffix(suffix.as_deref());
                builder.fconst(*value, ty)
            }

            Expr::StringLiteral { value } => builder.sconst(value.clone()),

            Expr::CharLiteral { value } => builder.char_const(value.chars().next().unwrap_or('\0')),

            Expr::BoolLiteral { value } => builder.bconst(*value),

            Expr::Path { path, resolved } => match resolved {
                Some(ResolvedName::Param(idx)) => {
                    let capture = CapturePlace::root(CaptureSource::Param(*idx));
                    let value =
                        if let Some(access) = self.capture_access_for_place(builder, &capture) {
                            builder.load(access.place, access.ty)
                        } else if let Some(storage) =
                            self.parameter_place(builder, &CaptureSource::Param(*idx))
                        {
                            builder.load(storage, mir_type.clone())
                        } else {
                            param_values
                                .get(*idx)
                                .copied()
                                .unwrap_or_else(|| builder.unit_const())
                        };
                    self.clear_drop_flags_if_moved(builder, body, expr_id);
                    value
                }
                Some(ResolvedName::LambdaParam { lambda, index }) => {
                    let source = CaptureSource::LambdaParam {
                        lambda: *lambda,
                        index: *index,
                    };
                    let value = if self.current_lambda == Some(*lambda) {
                        self.parameter_place(builder, &source)
                            .map(|place| builder.load(place, mir_type.clone()))
                            .or_else(|| param_values.get(*index).copied())
                            .unwrap_or_else(|| builder.unit_const())
                    } else if let Some(access) =
                        self.capture_access_for_place(builder, &CapturePlace::root(source.clone()))
                    {
                        builder.load(access.place, access.ty)
                    } else {
                        builder.unit_const()
                    };
                    self.clear_drop_flags_if_moved(builder, body, expr_id);
                    value
                }
                Some(ResolvedName::PatternBinding(id)) => {
                    let source = CaptureSource::Pattern(*id);
                    let value = if let Some(access) =
                        self.capture_access_for_place(builder, &CapturePlace::root(source))
                    {
                        builder.load(access.place, access.ty)
                    } else {
                        self.binding_value(builder, *id, &mir_type)
                            .unwrap_or_else(|| builder.unit_const())
                    };
                    self.clear_drop_flags_if_moved(builder, body, expr_id);
                    value
                }
                Some(ResolvedName::Function(fid)) => {
                    let args = match tc_type {
                        Some(type_checker::Type::FunctionItem { args, .. }) => args.clone(),
                        _ => Vec::new(),
                    };
                    self.lower_function_value(builder, *fid, &args, &mir_type)
                }
                Some(ResolvedName::Const(const_id)) => self.lower_const_value(builder, *const_id),
                Some(ResolvedName::EnumVariant(enum_id, idx)) => {
                    self.lower_enum_variant_value(builder, *enum_id, *idx, Vec::new(), mir_type)
                }
                _ => path
                    .as_single_name()
                    .and_then(|name| {
                        self.generic_const_subst
                            .get(&name.0)
                            .map(|value| builder.iconst(*value as u64, IntTy::Usize))
                    })
                    .unwrap_or_else(|| builder.unit_const()),
            },

            Expr::Binary { lhs, rhs, op } => {
                if matches!(op, HirBinOp::And | HirBinOp::Or) {
                    let value =
                        self.lower_short_circuit_expr(builder, param_values, body, *lhs, *rhs, op);
                    let value = self.apply_expr_coercion(builder, expr_id, value);
                    self.expr_cache.insert(expr_id, value);
                    return value;
                }
                if let Some(call) = self
                    .current_body
                    .and_then(|bid| self.type_result.operator_calls.get(&(bid, expr_id)))
                    .cloned()
                {
                    let function = match call {
                        OperatorCall::Function(fid) => Some(fid),
                        OperatorCall::Trait(call) => {
                            let lhs_ty = self
                                .current_body
                                .and_then(|bid| self.type_result.expr_types.get(&(bid, *lhs)));
                            let rhs_ty = self
                                .current_body
                                .and_then(|bid| self.type_result.expr_types.get(&(bid, *rhs)));
                            lhs_ty.and_then(|lhs_ty| {
                                self.find_trait_impl_method(
                                    call.trait_id,
                                    &call.method,
                                    lhs_ty,
                                    rhs_ty,
                                )
                            })
                        }
                    };
                    let Some(function) = function else {
                        return builder.unit_const();
                    };
                    if let Some(op) = self.builtin_operator_for_method(function) {
                        return self.lower_builtin_operator_method_call(
                            builder,
                            param_values,
                            body,
                            expr_id,
                            *lhs,
                            &[*rhs],
                            op,
                        );
                    }
                    let receiver_ty = self.hir.item_tree.functions[function]
                        .params
                        .first()
                        .map(|param| param.ty.clone());
                    let rhs_ty = self.hir.item_tree.functions[function]
                        .params
                        .get(1)
                        .map(|param| param.ty.clone());
                    let lv = if let Some(receiver_ty) = &receiver_ty {
                        self.lower_receiver_arg(builder, param_values, body, *lhs, receiver_ty)
                    } else {
                        self.lower_expr(builder, param_values, body, *lhs)
                    };
                    let rv = if let Some(rhs_ty) = rhs_ty {
                        self.lower_receiver_arg(builder, param_values, body, *rhs, &rhs_ty)
                    } else {
                        self.lower_expr(builder, param_values, body, *rhs)
                    };
                    return self.lower_operator_call(
                        builder,
                        *lhs,
                        Some(*rhs),
                        function,
                        vec![lv, rv],
                        mir_type,
                    );
                }

                if op.is_assignment() {
                    let rv = self.lower_expr(builder, param_values, body, *rhs);
                    let lv = self.lower_lvalue(builder, param_values, body, *lhs);
                    return match op {
                        HirBinOp::Assign => {
                            let lhs_ty = self
                                .current_body
                                .and_then(|body_id| {
                                    self.type_result.expr_types.get(&(body_id, *lhs))
                                })
                                .cloned();
                            let assignment_slots = self
                                .drop_place_from_expr(body, *lhs)
                                .and_then(|(source, projection)| {
                                    self.drop_slots.get(&source).map(|slots| {
                                        slots
                                            .iter()
                                            .filter(|slot| {
                                                projection.is_empty()
                                                    || slot
                                                        .projection
                                                        .starts_with(projection.as_slice())
                                            })
                                            .cloned()
                                            .collect::<Vec<_>>()
                                    })
                                })
                                .unwrap_or_default();
                            // Raw-pointer indexing addresses uninitialized storage in
                            // containers such as `Vector<T>`; there is no old value to drop.
                            let raw_pointer_index = matches!(
                                &body.exprs[*lhs],
                                Expr::IndexAccess { base, .. }
                                    if self
                                        .current_body
                                        .and_then(|body_id| {
                                            self.type_result.expr_types.get(&(body_id, *base))
                                        })
                                        .is_some_and(|ty| {
                                            matches!(ty, type_checker::Type::Ptr { .. })
                                        })
                            );
                            if !raw_pointer_index
                                && let Some(ty) = lhs_ty.as_ref()
                                && self.type_needs_drop(ty, 0)
                            {
                                if assignment_slots.is_empty() {
                                    self.emit_drop_glue(builder, lv, ty);
                                } else {
                                    for slot in &assignment_slots {
                                        self.emit_drop_slot(builder, slot);
                                    }
                                }
                            }
                            builder.store(rv, lv);
                            for slot in &assignment_slots {
                                let active = builder.bconst(true);
                                let flag = self.drop_slot_flag_place(builder, slot);
                                builder.store(active, flag);
                            }
                            builder.unit_const()
                        }
                        _ => {
                            let base_op = op.compound_base().unwrap();
                            let value_ty = self
                                .current_body
                                .and_then(|bid| self.type_result.expr_types.get(&(bid, *lhs)))
                                .map(|t| self.convert_type(t))
                                .unwrap_or(mir_type);
                            let current = builder.load(lv, value_ty.clone());
                            let updated =
                                builder.binop(convert_binop(&base_op), current, rv, value_ty);
                            builder.store(updated, lv);
                            builder.unit_const()
                        }
                    };
                }

                let values = self.lower_expr_sequence(
                    builder,
                    param_values,
                    body,
                    expr_id,
                    0,
                    &[*lhs, *rhs],
                );
                let [lv, rv] = values.as_slice() else {
                    unreachable!();
                };
                let (lv, rv) = (*lv, *rv);

                match op {
                    HirBinOp::Eq
                    | HirBinOp::Neq
                    | HirBinOp::Lt
                    | HirBinOp::Gt
                    | HirBinOp::LtEq
                    | HirBinOp::GtEq => {
                        let lhs_ty = self
                            .current_body
                            .and_then(|bid| self.type_result.expr_types.get(&(bid, *lhs)))
                            .cloned()
                            .unwrap_or(type_checker::Type::Error);
                        let rhs_ty = self
                            .current_body
                            .and_then(|bid| self.type_result.expr_types.get(&(bid, *rhs)))
                            .cloned()
                            .unwrap_or(type_checker::Type::Error);
                        self.lower_comparison(builder, op, lv, rv, &lhs_ty, &rhs_ty)
                    }
                    _ => {
                        let binop = convert_binop(op);
                        builder.binop(binop, lv, rv, mir_type)
                    }
                }
            }

            Expr::Unary { operand, op } => {
                if let Some(call) = self
                    .current_body
                    .and_then(|bid| self.type_result.operator_calls.get(&(bid, expr_id)))
                    .cloned()
                {
                    let function = match call {
                        OperatorCall::Function(fid) => Some(fid),
                        OperatorCall::Trait(call) => self
                            .current_body
                            .and_then(|bid| self.type_result.expr_types.get(&(bid, *operand)))
                            .and_then(|operand_ty| {
                                self.find_trait_impl_method(
                                    call.trait_id,
                                    &call.method,
                                    operand_ty,
                                    None,
                                )
                            }),
                    };
                    let Some(function) = function else {
                        return builder.unit_const();
                    };
                    if let Some(op) = self.builtin_operator_for_method(function) {
                        return self.lower_builtin_operator_method_call(
                            builder,
                            param_values,
                            body,
                            expr_id,
                            *operand,
                            &[],
                            op,
                        );
                    }
                    let receiver_ty = self.hir.item_tree.functions[function]
                        .params
                        .first()
                        .map(|param| param.ty.clone());
                    let value = if let Some(receiver_ty) = receiver_ty {
                        self.lower_receiver_arg(builder, param_values, body, *operand, &receiver_ty)
                    } else {
                        self.lower_expr(builder, param_values, body, *operand)
                    };
                    return self.lower_operator_call(
                        builder,
                        *operand,
                        None,
                        function,
                        vec![value],
                        mir_type,
                    );
                }
                if matches!(op, HirUnOp::Neg)
                    && let Expr::IntLiteral { value, .. } = &body.exprs[*operand]
                    && let Type::Int(ty) = mir_type
                {
                    return builder.negative_iconst(*value, ty);
                }
                let ov = if matches!(op, HirUnOp::Ref | HirUnOp::MutRef) {
                    self.lower_lvalue(builder, param_values, body, *operand)
                } else {
                    self.lower_expr(builder, param_values, body, *operand)
                };
                // +x is a no-op, return operand directly
                if matches!(op, HirUnOp::Pos) {
                    return ov;
                }
                let unop = convert_unop(op);
                builder.unop(unop, ov, mir_type)
            }

            Expr::Block { stmts, tail } => {
                self.drop_scopes.push(Vec::new());
                // 块：顺序执行语句，尾表达式返回值
                for &stmt in stmts {
                    self.lower_stmt(builder, param_values, body, stmt);
                    if !builder.needs_return() {
                        break;
                    }
                }
                let result = if !builder.needs_return() {
                    builder.unit_const()
                } else {
                    match tail {
                        Some(tail_expr) => self.lower_expr(builder, param_values, body, *tail_expr),
                        None => builder.unit_const(),
                    }
                };
                if builder.needs_return() {
                    self.emit_current_drop_scope(builder);
                }
                self.drop_scopes.pop();
                result
            }

            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.temporary_drop_scopes.push(Vec::new());
                let cv = self.lower_expr(builder, param_values, body, *cond);
                if builder.needs_return() {
                    self.emit_current_temporary_drop_scope(builder);
                }
                self.temporary_drop_scopes.pop();
                let then_block = builder.func.new_block_labeled("then");
                let else_block = builder.func.new_block_labeled("else");
                let merge_block = builder.func.new_block_labeled("merge");

                builder.set_cond_branch(cv, then_block, else_block);

                // then 分支
                builder.switch_to_block(then_block);
                self.temporary_drop_scopes.push(Vec::new());
                let tv = self.lower_expr(builder, param_values, body, *then_branch);
                if builder.needs_return() {
                    self.emit_current_temporary_drop_scope(builder);
                }
                self.temporary_drop_scopes.pop();
                let then_exit = builder.current_block;
                let mut phi_args = Vec::new();
                if builder.needs_return() {
                    builder.set_branch(merge_block);
                    phi_args.push((tv, then_exit));
                }

                // else 分支
                builder.switch_to_block(else_block);
                self.temporary_drop_scopes.push(Vec::new());
                let ev = match else_branch {
                    Some(eb) => self.lower_expr(builder, param_values, body, *eb),
                    None => builder.unit_const(),
                };
                if builder.needs_return() {
                    self.emit_current_temporary_drop_scope(builder);
                }
                self.temporary_drop_scopes.pop();
                let else_exit = builder.current_block;
                if builder.needs_return() {
                    builder.set_branch(merge_block);
                    phi_args.push((ev, else_exit));
                }

                // merge 块：用 phi 节点合并两条路径的值
                builder.switch_to_block(merge_block);
                match phi_args.len() {
                    0 => builder.unit_const(),
                    _ => {
                        let phi = Inst::new(InstKind::Phi(phi_args), mir_type.clone());
                        builder.func.push_inst(merge_block, phi)
                    }
                }
            }

            Expr::While {
                condition,
                body: while_body,
            } => {
                let cond_block = builder.func.new_block_labeled("while_cond");
                let body_block = builder.func.new_block_labeled("while_body");
                let exit_block = builder.func.new_block_labeled("while_exit");

                // 跳转到条件块
                builder.set_branch(cond_block);

                // 条件块：计算条件，条件分支
                builder.switch_to_block(cond_block);
                self.temporary_drop_scopes.push(Vec::new());
                let cv = self.lower_expr(builder, param_values, body, *condition);
                if builder.needs_return() {
                    self.emit_current_temporary_drop_scope(builder);
                }
                self.temporary_drop_scopes.pop();
                builder.set_cond_branch(cv, body_block, exit_block);

                // 循环体：执行后跳回条件块
                builder.switch_to_block(body_block);
                self.loop_targets.push(LoopTargets {
                    break_block: exit_block,
                    continue_block: cond_block,
                    drop_depth: self.drop_scopes.len(),
                    temporary_drop_depth: self.temporary_drop_scopes.len(),
                });
                self.lower_expr(builder, param_values, body, *while_body);
                self.loop_targets.pop();
                if builder.needs_return() {
                    builder.set_branch(cond_block);
                }

                // 出口块
                builder.switch_to_block(exit_block);
                builder.unit_const()
            }

            Expr::For {
                pat,
                iterable,
                body: for_body,
            } => self.lower_for_expr(
                builder,
                param_values,
                body,
                expr_id,
                *pat,
                *iterable,
                *for_body,
            ),

            Expr::Match { scrutinee, arms } => {
                self.lower_match_expr(builder, param_values, body, *scrutinee, arms, mir_type)
            }

            Expr::Array { elements } => {
                let vals =
                    self.lower_expr_sequence(builder, param_values, body, expr_id, 0, elements);
                builder.array_value(vals, mir_type)
            }

            Expr::Tuple { elements } => {
                let values =
                    self.lower_expr_sequence(builder, param_values, body, expr_id, 0, elements);
                builder.tuple_value(values, mir_type)
            }

            Expr::ArrayRepeat { value, .. } => {
                let len = match tc_type {
                    Some(type_checker::Type::Array(_, len)) => len.as_usize().unwrap_or(0),
                    _ => 0,
                };
                let val = self.lower_expr(builder, param_values, body, *value);
                builder.array_value(vec![val; len], mir_type)
            }

            Expr::Struct {
                fields, resolved, ..
            } => {
                if let Some(ResolvedName::EnumVariant(enum_id, variant_index)) = resolved {
                    let expressions = match &self.hir.item_tree.enums[*enum_id].variants
                        [*variant_index]
                        .kind
                    {
                        hir::item_tree::HirVariantKind::Struct(expected_fields) => expected_fields
                            .iter()
                            .filter_map(|expected| {
                                fields
                                    .iter()
                                    .find(|field| field.name == expected.name)
                                    .map(|field| field.value)
                            })
                            .collect(),
                        _ => Vec::new(),
                    };
                    let values = self.lower_expr_sequence(
                        builder,
                        param_values,
                        body,
                        expr_id,
                        0,
                        &expressions,
                    );
                    self.lower_enum_variant_value(
                        builder,
                        *enum_id,
                        *variant_index,
                        values,
                        mir_type,
                    )
                } else {
                    let expressions = fields.iter().map(|field| field.value).collect::<Vec<_>>();
                    let vals = self.lower_expr_sequence(
                        builder,
                        param_values,
                        body,
                        expr_id,
                        0,
                        &expressions,
                    );
                    builder.struct_value(vals, mir_type)
                }
            }

            Expr::Call { callee, args, .. } => {
                if let Some(value) = self.lower_static_trait_call(
                    builder,
                    body,
                    expr_id,
                    *callee,
                    args,
                    mir_type.clone(),
                ) {
                    value
                } else if let Expr::Path {
                    resolved: Some(ResolvedName::EnumVariant(enum_id, variant_index)),
                    ..
                } = &body.exprs[*callee]
                {
                    let arg_vals =
                        self.lower_expr_sequence(builder, param_values, body, expr_id, 1, args);
                    self.lower_enum_variant_value(
                        builder,
                        *enum_id,
                        *variant_index,
                        arg_vals,
                        mir_type,
                    )
                } else if let Some(value) = self.lower_builtin_call(
                    builder,
                    param_values,
                    body,
                    *callee,
                    args,
                    mir_type.clone(),
                ) {
                    value
                } else if self.callee_function_id(*callee).is_none() {
                    let expressions = std::iter::once(*callee)
                        .chain(args.iter().copied())
                        .collect::<Vec<_>>();
                    let mut values = self.lower_expr_sequence(
                        builder,
                        param_values,
                        body,
                        expr_id,
                        0,
                        &expressions,
                    );
                    let callee_value = values.remove(0);
                    let mut arg_values = values;
                    let callee_ty = self
                        .current_body
                        .and_then(|body_id| self.type_result.expr_types.get(&(body_id, *callee)))
                        .map(|ty| self.convert_type(ty))
                        .unwrap_or(Type::Unit);
                    if let Some(signature) = closure_call_signature(&callee_ty) {
                        let call =
                            builder.extract_value(callee_value, 0, Type::FnPtr(signature.clone()));
                        let env = builder.extract_value(callee_value, 1, closure_env_type());
                        arg_values.insert(0, env);
                        builder.call_indirect(call, arg_values, mir_type)
                    } else {
                        builder.call_indirect(callee_value, arg_values, mir_type)
                    }
                } else {
                    let target_fid = self.callee_function_id(*callee);
                    let method_target = match (target_fid, &body.exprs[*callee]) {
                        (Some(fid), Expr::FieldAccess { base, .. }) => {
                            Some((self.actual_method_fid(*callee, fid, *base), *base))
                        }
                        _ => None,
                    };
                    if let Some((fid, base)) = method_target
                        && let Some(op) = self.builtin_operator_for_method(fid)
                    {
                        return self.lower_builtin_operator_method_call(
                            builder,
                            param_values,
                            body,
                            expr_id,
                            base,
                            args,
                            op,
                        );
                    }

                    let name = if let Some((fid, base)) = method_target {
                        self.mono_function_name(fid, *callee)
                            .or_else(|| self.mono_method_name(fid, base, args.first().copied()))
                            .unwrap_or_else(|| self.function_name(fid))
                    } else {
                        target_fid
                            .map(|fid| {
                                self.mono_function_name(fid, *callee)
                                    .unwrap_or_else(|| self.function_name(fid))
                            })
                            .unwrap_or_else(|| callee_name(body, *callee))
                    };
                    let receiver = if let Some((receiver_fid, base)) = method_target
                        && let Some(receiver) =
                            self.hir.item_tree.functions[receiver_fid].params.first()
                    {
                        Some(self.lower_receiver_arg(
                            builder,
                            param_values,
                            body,
                            base,
                            &receiver.ty,
                        ))
                    } else {
                        None
                    };
                    let args =
                        self.lower_expr_sequence(builder, param_values, body, expr_id, 1, args);
                    let mut arg_vals =
                        Vec::with_capacity(args.len() + usize::from(receiver.is_some()));
                    if let Some(receiver) = receiver {
                        arg_vals.push(receiver);
                    }
                    arg_vals.extend(args);
                    // 检查是否是 extern 函数调用
                    let is_extern = target_fid
                        .map(|fid| {
                            self.hir.item_tree.extern_function_ids.contains(&fid)
                                && !self.hir.function_bodies.contains_key(&fid)
                        })
                        .unwrap_or(false);
                    let func_ref = if is_extern {
                        FuncRef::Extern(name)
                    } else {
                        FuncRef::Local(name)
                    };
                    builder.call(func_ref, arg_vals, mir_type)
                }
            }

            Expr::Lambda {
                params,
                body: lambda_body,
                ..
            } => {
                let body_id = self.current_body.expect("lambda outside of a body");
                self.lower_lambda(
                    builder,
                    param_values,
                    body_id,
                    expr_id,
                    params,
                    *lambda_body,
                    &mir_type,
                )
            }

            Expr::FieldAccess { base, field } => {
                let captured = self
                    .capture_place_from_expr(body, expr_id)
                    .and_then(|place| self.capture_access_for_place(builder, &place))
                    .filter(|access| access.ty == mir_type);
                let value = if self.expression_requires_temporary_place(body, *base) {
                    let base_place =
                        self.materialize_temporary_place(builder, param_values, body, *base);
                    let field = builder.field_ptr(
                        base_place,
                        self.resolve_field_index(*base, field),
                        mir_type.clone(),
                    );
                    builder.load(field, mir_type)
                } else if let Some(access) = captured {
                    builder.load(access.place, access.ty)
                } else {
                    let bv = self.lower_expr(builder, param_values, body, *base);
                    let field_idx = self.resolve_field_index(*base, field);
                    builder.extract_value(bv, field_idx, mir_type)
                };
                self.clear_drop_flags_if_moved(builder, body, expr_id);
                value
            }

            Expr::IndexAccess { base, index } => {
                if let Some(place) = self.lower_trait_index_place(
                    builder,
                    param_values,
                    body,
                    expr_id,
                    *base,
                    *index,
                ) {
                    let value = builder.load(place, mir_type);
                    self.clear_drop_flags_if_moved(builder, body, expr_id);
                    let value = self.apply_expr_coercion(builder, expr_id, value);
                    if !diverges {
                        self.expr_cache.insert(expr_id, value);
                    }
                    return value;
                }
                let captured = self
                    .capture_place_from_expr(body, expr_id)
                    .and_then(|place| self.capture_access_for_place(builder, &place))
                    .filter(|access| access.ty == mir_type);
                if let Some(access) = captured {
                    let value = builder.load(access.place, access.ty);
                    self.clear_drop_flags_if_moved(builder, body, expr_id);
                    value
                } else {
                    let (base_val, index_val) = if self
                        .expression_requires_temporary_place(body, *base)
                    {
                        (
                            self.materialize_temporary_place(builder, param_values, body, *base),
                            self.lower_expr(builder, param_values, body, *index),
                        )
                    } else {
                        let values = self.lower_expr_sequence(
                            builder,
                            param_values,
                            body,
                            expr_id,
                            0,
                            &[*base, *index],
                        );
                        let [base, index] = values.as_slice() else {
                            unreachable!();
                        };
                        (*base, *index)
                    };
                    let ptr = if let Some(len) = self.index_len(builder, base_val, *base) {
                        builder.checked_index_ptr(base_val, index_val, len, mir_type.clone())
                    } else {
                        builder.index_ptr(base_val, index_val, mir_type.clone())
                    };
                    let value = builder.load(ptr, mir_type);
                    self.clear_drop_flags_if_moved(builder, body, expr_id);
                    self.clear_dynamic_index_drop_flags_if_moved(
                        builder, body, expr_id, *index, index_val,
                    );
                    value
                }
            }

            Expr::Unsafe { body: body_expr } => {
                self.lower_expr(builder, param_values, body, *body_expr)
            }

            Expr::Cast { base, target: _ } => {
                let base_val = self.lower_expr(builder, param_values, body, *base);
                let base_tc_ty = self
                    .current_body
                    .and_then(|bid| self.type_result.expr_types.get(&(bid, *base)))
                    .cloned();
                let base_mir_ty = base_tc_ty
                    .as_ref()
                    .map(|t| self.convert_type(t))
                    .unwrap_or(Type::Unit);

                if is_raw_parts_to_slice_cast(&base_mir_ty, &mir_type) {
                    let Type::Tuple(parts) = &base_mir_ty else {
                        unreachable!();
                    };
                    let data = builder.extract_value(base_val, 0, parts[0].clone());
                    let len = builder.extract_value(base_val, 1, Type::Int(IntTy::Usize));
                    builder.struct_value(vec![data, len], mir_type)
                } else if is_slice_to_raw_parts_cast(&base_mir_ty, &mir_type) {
                    let Type::Tuple(parts) = &mir_type else {
                        unreachable!();
                    };
                    let data = builder.extract_value(base_val, 0, parts[0].clone());
                    let len = builder.extract_value(base_val, 1, Type::Int(IntTy::Usize));
                    builder.struct_value(vec![data, len], mir_type)
                } else if is_byte_str_layout_cast(&base_mir_ty, &mir_type) {
                    let data = builder.extract_value(
                        base_val,
                        0,
                        Type::Ptr(Box::new(Type::Int(IntTy::U8))),
                    );
                    let len = builder.extract_value(base_val, 1, Type::Int(IntTy::Usize));
                    builder.struct_value(vec![data, len], mir_type)
                } else {
                    let cast_op = determine_cast_op(&base_mir_ty, &mir_type);
                    builder.cast(cast_op, base_val, mir_type)
                }
            }

            Expr::Try { operand } => {
                self.lower_try_expr(builder, param_values, body, expr_id, *operand, mir_type)
            }
        };

        let value = self.apply_expr_coercion(builder, expr_id, value);
        if diverges && builder.needs_return() {
            builder.set_unreachable();
        }
        self.expr_cache.insert(expr_id, value);
        value
    }

    pub(super) fn lower_short_circuit_expr(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        lhs: ExprId,
        rhs: ExprId,
        op: &HirBinOp,
    ) -> Value {
        let lhs = self.lower_expr(builder, param_values, body, lhs);
        if !builder.needs_return() {
            return lhs;
        }

        let rhs_block = builder.func.new_block_labeled("logical_rhs");
        let short_block = builder.func.new_block_labeled("logical_short");
        let merge_block = builder.func.new_block_labeled("logical_merge");
        if matches!(op, HirBinOp::And) {
            builder.set_cond_branch(lhs, rhs_block, short_block);
        } else {
            builder.set_cond_branch(lhs, short_block, rhs_block);
        }

        let mut phi_args = Vec::with_capacity(2);
        builder.switch_to_block(rhs_block);
        self.temporary_drop_scopes.push(Vec::new());
        let rhs = self.lower_expr(builder, param_values, body, rhs);
        if builder.needs_return() {
            self.emit_current_temporary_drop_scope(builder);
            let rhs_exit = builder.current_block;
            builder.set_branch(merge_block);
            phi_args.push((rhs, rhs_exit));
        }
        self.temporary_drop_scopes.pop();

        builder.switch_to_block(short_block);
        let short = builder.bconst(matches!(op, HirBinOp::Or));
        let short_exit = builder.current_block;
        builder.set_branch(merge_block);
        phi_args.push((short, short_exit));

        builder.switch_to_block(merge_block);
        let phi = Inst::new(InstKind::Phi(phi_args), Type::Bool);
        builder.func.push_inst(merge_block, phi)
    }

    pub(super) fn lower_try_expr(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        expr_id: ExprId,
        operand: ExprId,
        result_ty: Type,
    ) -> Value {
        let Some(body_id) = self.current_body else {
            return builder.unit_const();
        };
        let operand_tc_ty = self
            .type_result
            .expr_types
            .get(&(body_id, operand))
            .cloned()
            .unwrap_or(type_checker::Type::Unknown);
        let type_checker::Type::Enum(result_id, result_args) = &operand_tc_ty else {
            return self.lower_expr(builder, param_values, body, operand);
        };
        let Some(return_ty) = self.current_function_return_type() else {
            return self.lower_expr(builder, param_values, body, operand);
        };
        let type_checker::Type::Enum(return_id, _) = &return_ty else {
            return self.lower_expr(builder, param_values, body, operand);
        };
        if result_id != return_id || result_args.len() != 2 {
            return self.lower_expr(builder, param_values, body, operand);
        }

        let operand_value = self.lower_expr(builder, param_values, body, operand);
        let operand_mir_ty = self.convert_type(&operand_tc_ty);
        let tag = builder.extract_value(operand_value, 0, Type::Int(IntTy::U32));
        let enum_data = &self.hir.item_tree.enums[*result_id];
        let Some(ok_variant) = enum_data
            .variants
            .iter()
            .position(|variant| variant.name.0 == "Ok")
        else {
            return operand_value;
        };
        let Some(err_variant) = enum_data
            .variants
            .iter()
            .position(|variant| variant.name.0 == "Err")
        else {
            return operand_value;
        };
        let ok_block = builder.func.new_block_labeled("try_ok");
        let err_block = builder.func.new_block_labeled("try_err");
        let merge_block = builder.func.new_block_labeled("try_merge");
        let expected_tag = builder.iconst(ok_variant as u64, IntTy::U32);
        let is_ok = builder.cmp(CmpOp::Eq, tag, expected_tag);
        builder.set_cond_branch(is_ok, ok_block, err_block);

        builder.switch_to_block(err_block);
        let err_offset = 1 + self.enum_payload_offset(enum_data, err_variant);
        let error_mir_ty = match &operand_mir_ty {
            Type::Enum(enum_ty) => enum_ty
                .variants
                .get(err_variant)
                .and_then(|variant| match &variant.kind {
                    EnumVariantKind::Tuple(fields) if fields.len() == 1 => fields.first().cloned(),
                    EnumVariantKind::Struct(fields) if fields.len() == 1 => {
                        fields.first().map(|(_, ty)| ty.clone())
                    }
                    _ => None,
                })
                .unwrap_or(Type::Unit),
            _ => Type::Unit,
        };
        let error_value = builder.extract_value(operand_value, err_offset, error_mir_ty.clone());
        let error_tc_ty = result_args[1].clone();
        let converted_error = if let Some(call) = self
            .type_result
            .trait_method_calls
            .get(&(body_id, expr_id))
            .cloned()
        {
            let target_error = match &return_ty {
                type_checker::Type::Enum(_, return_args) => &return_args[1],
                _ => &error_tc_ty,
            };
            match self.find_trait_impl_method(
                call.trait_id,
                &call.method,
                &error_tc_ty,
                Some(target_error),
            ) {
                Some(fid) => {
                    let name = self
                        .mono_method_name_for_receiver(fid, &error_tc_ty, Some(target_error))
                        .unwrap_or_else(|| self.function_name(fid));
                    let return_mir_ty = self.hir.item_tree.functions[fid]
                        .ret_type
                        .as_ref()
                        .map(|ty| self.convert_hir_type(ty))
                        .unwrap_or(Type::Unit);
                    builder.call(FuncRef::Local(name), vec![error_value], return_mir_ty)
                }
                None => error_value,
            }
        } else {
            error_value
        };
        let return_mir_ty = self.current_function_return_mir_type();
        let error_result = self.lower_enum_variant_value(
            builder,
            *return_id,
            err_variant,
            vec![converted_error],
            return_mir_ty,
        );
        self.emit_current_drop_scope(builder);
        builder.set_return(Some(error_result));

        builder.switch_to_block(ok_block);
        let ok_offset = 1 + self.enum_payload_offset(enum_data, ok_variant);
        let ok_mir_ty = match &operand_mir_ty {
            Type::Enum(enum_ty) => enum_ty
                .variants
                .get(ok_variant)
                .and_then(|variant| match &variant.kind {
                    EnumVariantKind::Tuple(fields) if fields.len() == 1 => fields.first().cloned(),
                    EnumVariantKind::Struct(fields) if fields.len() == 1 => {
                        fields.first().map(|(_, ty)| ty.clone())
                    }
                    _ => None,
                })
                .unwrap_or(result_ty.clone()),
            _ => result_ty.clone(),
        };
        let ok_value = builder.extract_value(operand_value, ok_offset, ok_mir_ty);
        builder.set_branch(merge_block);

        builder.switch_to_block(merge_block);
        let phi = Inst::new(InstKind::Phi(vec![(ok_value, ok_block)]), result_ty);
        builder.func.push_inst(merge_block, phi)
    }

    pub(super) fn current_function_return_type(&self) -> Option<type_checker::Type> {
        let fid = self.current_function?;
        let function = &self.hir.item_tree.functions[fid];
        Some(
            function
                .ret_type
                .as_ref()
                .map(|ty| self.lower_hir_type_for_pattern(ty, &self.generic_tc_subst))
                .unwrap_or(type_checker::Type::Unit),
        )
    }

    pub(super) fn current_function_return_mir_type(&self) -> Type {
        let Some(fid) = self.current_function else {
            return Type::Unit;
        };
        self.hir.item_tree.functions[fid]
            .ret_type
            .as_ref()
            .map(|ty| self.convert_hir_type(ty))
            .unwrap_or(Type::Unit)
    }

    pub(super) fn apply_expr_coercion(
        &self,
        builder: &mut Builder,
        expr_id: ExprId,
        value: Value,
    ) -> Value {
        let Some(body_id) = self.current_body else {
            return value;
        };
        let Some(target) = self.type_result.expr_coercions.get(&(body_id, expr_id)) else {
            return value;
        };
        let Some(actual) = self.type_result.expr_types.get(&(body_id, expr_id)) else {
            return value;
        };
        let (type_checker::Type::Ref(actual, _), type_checker::Type::Ref(target, target_mut)) =
            (actual, target)
        else {
            return value;
        };
        let (
            type_checker::Type::Array(_, type_checker::ConstArg::Value(len)),
            type_checker::Type::Slice(_),
        ) = (actual.as_ref(), target.as_ref())
        else {
            return value;
        };
        let len = builder.iconst(*len as u64, IntTy::Usize);
        builder.struct_value(
            vec![value, len],
            Type::Ref(Box::new(self.convert_type(target.as_ref())), *target_mut),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn lower_for_expr(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        for_expr: ExprId,
        pat: PatId,
        iterable: ExprId,
        for_body: ExprId,
    ) -> Value {
        if let Some(info) = self
            .current_body
            .and_then(|bid| self.type_result.for_loops.get(&(bid, for_expr)))
            .cloned()
        {
            return self.lower_iterator_for_expr(
                builder,
                param_values,
                body,
                pat,
                iterable,
                for_body,
                &info,
            );
        }

        if let Some((item_ty, len)) = self.array_iter_info(iterable) {
            return self.lower_array_for_expr(
                builder,
                param_values,
                body,
                pat,
                iterable,
                for_body,
                item_ty,
                len,
            );
        }

        let iterable_value = self.lower_expr(builder, param_values, body, iterable);
        if !self.is_std_range_expr(iterable) {
            return builder.unit_const();
        }

        let i32_ty = Type::Int(IntTy::I32);
        let start = builder.extract_value(iterable_value, 0, i32_ty.clone());
        let end = builder.extract_value(iterable_value, 1, i32_ty.clone());
        let cursor = builder.alloca(i32_ty.clone());
        builder.store(start, cursor);

        let cond_block = builder.func.new_block_labeled("for_cond");
        let body_block = builder.func.new_block_labeled("for_body");
        let step_block = builder.func.new_block_labeled("for_step");
        let exit_block = builder.func.new_block_labeled("for_exit");

        builder.set_branch(cond_block);

        builder.switch_to_block(cond_block);
        let current = builder.load(cursor, i32_ty.clone());
        let current_end = end;
        let keep_going = builder.cmp(CmpOp::Lt, current, current_end);
        builder.set_cond_branch(keep_going, body_block, exit_block);

        builder.switch_to_block(body_block);
        self.push_pattern_binding(body, pat, current, i32_ty.clone());
        self.loop_targets.push(LoopTargets {
            break_block: exit_block,
            continue_block: step_block,
            drop_depth: self.drop_scopes.len(),
            temporary_drop_depth: self.temporary_drop_scopes.len(),
        });
        self.lower_expr(builder, param_values, body, for_body);
        self.loop_targets.pop();
        self.pattern_bindings.pop();
        if builder.needs_return() {
            builder.set_branch(step_block);
        }

        builder.switch_to_block(step_block);
        let current = builder.load(cursor, i32_ty.clone());
        let one = builder.iconst(1, IntTy::I32);
        let next = builder.binop(BinOp::Add, current, one, i32_ty);
        builder.store(next, cursor);
        builder.set_branch(cond_block);

        builder.switch_to_block(exit_block);
        builder.unit_const()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn lower_array_for_expr(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        pat: PatId,
        iterable: ExprId,
        for_body: ExprId,
        item_ty: Type,
        len: usize,
    ) -> Value {
        let body_id = self.current_body.expect("for loop outside a function body");
        let item_tc_ty = match self
            .type_result
            .expr_types
            .get(&(body_id, iterable))
            .map(|ty| self.substitute_tc_type(ty))
        {
            Some(type_checker::Type::Array(item, _)) => *item,
            _ => type_checker::Type::Unknown,
        };
        let array_tc_ty = type_checker::Type::Array(
            Box::new(item_tc_ty.clone()),
            type_checker::ConstArg::Value(len),
        );
        let array_ty = Type::Array(Box::new(item_ty.clone()), len);
        let iterable_value = self.lower_expr(builder, param_values, body, iterable);
        let iterable_place = builder.alloca(array_ty);
        builder.store(iterable_value, iterable_place);
        let owner_slots = self.create_drop_slots(builder, iterable_place, &array_tc_ty, Vec::new());
        self.drop_scopes
            .push(owner_slots.iter().cloned().rev().collect());

        let index_ty = Type::Int(IntTy::I32);
        let zero = builder.iconst(0, IntTy::I32);
        let end = builder.iconst(len as u64, IntTy::I32);
        let cursor = builder.alloca(index_ty.clone());
        builder.store(zero, cursor);

        let cond_block = builder.func.new_block_labeled("for_array_cond");
        let body_block = builder.func.new_block_labeled("for_array_body");
        let step_block = builder.func.new_block_labeled("for_array_step");
        let exit_block = builder.func.new_block_labeled("for_array_exit");

        builder.set_branch(cond_block);

        builder.switch_to_block(cond_block);
        let current = builder.load(cursor, index_ty.clone());
        let keep_going = builder.cmp(CmpOp::Lt, current, end);
        builder.set_cond_branch(keep_going, body_block, exit_block);

        builder.switch_to_block(body_block);
        let item_ptr = builder.index_ptr(iterable_place, current, item_ty.clone());
        let item = builder.load(item_ptr, item_ty.clone());
        self.clear_indexed_drop_slots(builder, &owner_slots, current, IntTy::I32);
        let item_place = self.type_needs_drop(&item_tc_ty, 0).then(|| {
            let place = builder.alloca(item_ty.clone());
            builder.store(item, place);
            place
        });
        self.push_match_pattern_bindings(builder, body, pat, item, item_place, &item_tc_ty);
        let pattern_sources =
            self.push_pattern_drop_scope(builder, body, pat, item_place, &item_tc_ty, true);
        let item_drop_depth = self.drop_scopes.len() - 1;
        self.loop_targets.push(LoopTargets {
            break_block: exit_block,
            continue_block: step_block,
            drop_depth: item_drop_depth,
            temporary_drop_depth: self.temporary_drop_scopes.len(),
        });
        self.lower_expr(builder, param_values, body, for_body);
        self.loop_targets.pop();
        if builder.needs_return() {
            self.emit_current_drop_scope(builder);
            builder.set_branch(step_block);
        }
        self.pop_pattern_drop_scope(pattern_sources);
        self.pattern_bindings.pop();

        builder.switch_to_block(step_block);
        let current = builder.load(cursor, index_ty.clone());
        let one = builder.iconst(1, IntTy::I32);
        let next = builder.binop(BinOp::Add, current, one, index_ty);
        builder.store(next, cursor);
        builder.set_branch(cond_block);

        builder.switch_to_block(exit_block);
        self.emit_current_drop_scope(builder);
        self.drop_scopes.pop();
        builder.unit_const()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn lower_iterator_for_expr(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        pat: PatId,
        iterable: ExprId,
        for_body: ExprId,
        info: &type_checker::ForLoopInfo,
    ) -> Value {
        let body_id = self.current_body.expect("for loop outside a function body");
        let iterable_ty = self
            .type_result
            .expr_types
            .get(&(body_id, iterable))
            .cloned()
            .map(|ty| self.substitute_tc_type(&ty))
            .expect("missing iterable type for checked for loop");
        let iter_tc_ty = self.substitute_tc_type(&info.iter_ty);
        let next_tc_ty = self.substitute_tc_type(&info.next_ty);

        let iterable_value = self.lower_expr(builder, param_values, body, iterable);
        let iter_ty = self.convert_type(&iter_tc_ty);
        let item_ty = self.convert_type(&info.item_ty);
        let option_ty = self.convert_type(&next_tc_ty);
        let into_iter_fid = self
            .find_trait_impl_method(
                info.into_iter.trait_id,
                &info.into_iter.method,
                &iterable_ty,
                None,
            )
            .expect("missing IntoIterator impl method for checked for loop");
        let next_fid = self
            .find_trait_impl_method(info.next.trait_id, &info.next.method, &iter_tc_ty, None)
            .expect("missing Iterator impl method for checked for loop");
        let into_iter_name = self
            .mono_method_name_for_receiver(into_iter_fid, &iterable_ty, None)
            .unwrap_or_else(|| self.function_name(into_iter_fid));
        let next_name = self
            .mono_method_name_for_receiver(next_fid, &iter_tc_ty, None)
            .unwrap_or_else(|| self.function_name(next_fid));

        let iter_value = builder.call(
            FuncRef::Local(into_iter_name),
            vec![iterable_value],
            iter_ty.clone(),
        );
        // `IntoIterator::into_iter(self)` consumes the owner. Its destructor
        // must not run again after the iterator takes over the allocation.
        if !matches!(iterable_ty, type_checker::Type::Ref(..))
            && let Some((source, _)) = self.drop_place_from_expr(body, iterable)
        {
            self.clear_drop_slots_for_source(builder, &source);
        }
        let iter_slot = builder.alloca(iter_ty.clone());
        builder.store(iter_value, iter_slot);
        let iter_owner_slots = self.create_drop_slots(builder, iter_slot, &iter_tc_ty, Vec::new());
        self.drop_scopes
            .push(iter_owner_slots.iter().cloned().rev().collect());
        // ponytail: array IntoIterator is sequential; add ManuallyDrop-like storage before
        // permitting custom array iterators that yield elements out of order.
        let tracks_array = matches!(iterable_ty, type_checker::Type::Array(..));
        let zero = tracks_array.then(|| builder.iconst(0, IntTy::Usize));
        let array_cursor = if tracks_array {
            let cursor = builder.alloca(Type::Int(IntTy::Usize));
            builder.store(zero.unwrap(), cursor);
            Some(cursor)
        } else {
            None
        };

        let cond_block = builder.func.new_block_labeled("for_iter_cond");
        let body_block = builder.func.new_block_labeled("for_iter_body");
        let exit_block = builder.func.new_block_labeled("for_iter_exit");

        builder.set_branch(cond_block);

        builder.switch_to_block(cond_block);
        let next_receiver = match self.hir.item_tree.functions[next_fid]
            .params
            .first()
            .map(|param| &param.ty)
        {
            Some(hir::item_tree::HirTypeRef::Ref(_, mutable)) => {
                let op = if *mutable {
                    HirUnOp::MutRef
                } else {
                    HirUnOp::Ref
                };
                builder.unop(
                    convert_unop(&op),
                    iter_slot,
                    Type::Ref(Box::new(iter_ty), *mutable),
                )
            }
            _ => iter_slot,
        };
        let next_value = builder.call(
            FuncRef::Local(next_name),
            vec![next_receiver],
            option_ty.clone(),
        );
        let tag = builder.extract_value(next_value, 0, Type::Int(IntTy::U32));
        let some_tag = builder.iconst(info.some_variant as u64, IntTy::U32);
        let has_item = builder.cmp(CmpOp::Eq, tag, some_tag);
        builder.set_cond_branch(has_item, body_block, exit_block);

        builder.switch_to_block(body_block);
        let option_id = match next_tc_ty {
            type_checker::Type::Enum(enum_id, _) => enum_id,
            _ => unreachable!("checked Iterator::next result is not an enum"),
        };
        let payload_index =
            1 + self.enum_payload_offset(&self.hir.item_tree.enums[option_id], info.some_variant);
        let item = builder.extract_value(next_value, payload_index, item_ty.clone());
        if tracks_array {
            let cursor = array_cursor.unwrap();
            let current = builder.load(cursor, Type::Int(IntTy::Usize));
            self.clear_indexed_drop_slots(builder, &iter_owner_slots, current, IntTy::Usize);
            let one = builder.iconst(1, IntTy::Usize);
            let next = builder.binop(BinOp::Add, current, one, Type::Int(IntTy::Usize));
            builder.store(next, cursor);
        }
        let item_place = self.type_needs_drop(&info.item_ty, 0).then(|| {
            let place = builder.alloca(item_ty.clone());
            builder.store(item, place);
            place
        });
        self.push_match_pattern_bindings(builder, body, pat, item, item_place, &info.item_ty);
        let pattern_sources =
            self.push_pattern_drop_scope(builder, body, pat, item_place, &info.item_ty, true);
        let item_drop_depth = self.drop_scopes.len() - 1;
        self.loop_targets.push(LoopTargets {
            break_block: exit_block,
            continue_block: cond_block,
            drop_depth: item_drop_depth,
            temporary_drop_depth: self.temporary_drop_scopes.len(),
        });
        self.lower_expr(builder, param_values, body, for_body);
        self.loop_targets.pop();
        if builder.needs_return() {
            self.emit_current_drop_scope(builder);
            builder.set_branch(cond_block);
        }
        self.pop_pattern_drop_scope(pattern_sources);
        self.pattern_bindings.pop();

        builder.switch_to_block(exit_block);
        self.emit_current_drop_scope(builder);
        self.drop_scopes.pop();
        builder.unit_const()
    }
}
