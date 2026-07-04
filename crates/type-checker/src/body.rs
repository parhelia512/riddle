use std::collections::HashMap;

use hir::{
    Name,
    body::{BinaryOp, Expr, ExprId, MatchArm, PatId, Pattern, ResolvedName, Stmt, StmtId, UnaryOp},
    item_tree::{
        FunctionId, HirAssocTypeConstraint, HirFunction, HirGenericBound, HirTypeRef,
        HirVariantKind, TraitId,
    },
};

use crate::{
    checker::{GenericEdge, TypeChecker},
    context::BodyCtx,
    lowering::{collect_subst, generic_param_map_with_consts, substitute_type},
    result::{ForLoopInfo, OperatorCall, TraitMethodCall},
    types::{ConstArg, IntTy, Type},
};

impl TypeChecker<'_> {
    pub(crate) fn check_stmt(&mut self, ctx: &mut BodyCtx<'_>, stmt_id: StmtId) {
        match &ctx.body.stmts[stmt_id] {
            Stmt::Let {
                ty,
                ty_range,
                init,
                is_mut,
                ..
            } => {
                let declared =
                    self.lower_type_ref_with_params_at(ty, &ctx.generic_params, *ty_range);
                let explicit_error = type_ref_contains_error(ty)
                    || type_contains_unresolved_const_param(&declared, &ctx.generic_params);
                if explicit_error {
                    self.diagnostic("E0034", "invalid type annotation", ctx.stmt_range(stmt_id));
                } else {
                    self.check_type_bounds(ctx, &declared, ctx.stmt_range(stmt_id));
                }
                let init_ty = init.map(|expr| {
                    if explicit_error || declared.is_unknown_like() {
                        self.check_expr(ctx, expr)
                    } else {
                        self.check_expr_expected(ctx, expr, &declared)
                    }
                });

                if let Some(init_ty) = init_ty {
                    if !explicit_error && !declared.is_unknown_like() {
                        self.expect_assignable(
                            &declared,
                            &init_ty,
                            "let initializer",
                            ctx.stmt_range(stmt_id),
                        );
                    }
                    let local_ty = if explicit_error {
                        declared
                    } else {
                        declared.or(init_ty)
                    };
                    ctx.locals.insert(stmt_id, (local_ty, *is_mut));
                } else {
                    ctx.locals.insert(stmt_id, (declared, *is_mut));
                }
            }
            Stmt::Expr { expr } => {
                self.check_expr(ctx, *expr);
            }
            Stmt::Return { value } => {
                let expected = ctx.return_ty.clone();
                let actual = value
                    .map(|expr| self.check_expr_expected(ctx, expr, &expected))
                    .unwrap_or(Type::Unit);
                self.expect_assignable(&expected, &actual, "return value", ctx.stmt_range(stmt_id));
            }
            Stmt::Item { .. } => {}
        }
    }

    pub(crate) fn check_expr(&mut self, ctx: &mut BodyCtx<'_>, expr_id: ExprId) -> Type {
        self.check_expr_inner(ctx, expr_id, None)
    }

    pub(crate) fn check_expr_expected(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        expr_id: ExprId,
        expected: &Type,
    ) -> Type {
        self.check_expr_inner(ctx, expr_id, Some(expected))
    }

    fn check_expr_inner(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        expr_id: ExprId,
        expected: Option<&Type>,
    ) -> Type {
        let span = ctx.expr_range(expr_id);
        let ty = match &ctx.body.exprs[expr_id] {
            Expr::Missing => Type::Error,
            Expr::IntLiteral { suffix, .. } => self.int_literal_type(suffix.as_deref(), expected),
            Expr::FloatLiteral { suffix, .. } => {
                self.float_literal_type(suffix.as_deref(), expected)
            }
            Expr::StringLiteral { .. } => Type::Str,
            Expr::CharLiteral { .. } => Type::Char,
            Expr::BoolLiteral { .. } => Type::Bool,
            Expr::Path { path, resolved } => {
                if let Some(binding_ty) = path
                    .as_single_name()
                    .and_then(|name| ctx.bindings.get(&name.0))
                    .cloned()
                {
                    binding_ty
                } else if path
                    .as_single_name()
                    .and_then(|name| ctx.generic_params.get(&name.0))
                    .is_some_and(|ty| matches!(ty, Type::Const(_)))
                {
                    Type::Int(IntTy::Usize)
                } else if let Some(ResolvedName::EnumVariant(enum_id, _)) = resolved {
                    self.enum_variant_type(*enum_id, expected)
                } else {
                    self.type_of_resolved_name(ctx, resolved.as_ref())
                }
            }
            Expr::Struct {
                resolved, fields, ..
            } => self.check_struct_expr(ctx, resolved.as_ref(), fields, expected, span),
            Expr::Binary { lhs, rhs, op } => {
                self.check_binary(ctx, expr_id, *lhs, *rhs, *op, expected, span)
            }
            Expr::Unary { operand, op } => self.check_unary(ctx, *operand, *op, expected, span),
            Expr::Block { stmts, tail } => {
                ctx.push_scope();
                for stmt in stmts {
                    self.check_stmt(ctx, *stmt);
                }
                let ty = tail
                    .map(|expr| match expected {
                        Some(expected) => self.check_expr_expected(ctx, expr, expected),
                        None => self.check_expr(ctx, expr),
                    })
                    .unwrap_or(Type::Unit);
                ctx.pop_scope();
                ty
            }
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let cond_ty = self.check_expr(ctx, *cond);
                self.expect_assignable(
                    &Type::Bool,
                    &cond_ty,
                    "if condition",
                    ctx.expr_range(*cond),
                );

                let then_ty = match expected {
                    Some(expected) => self.check_expr_expected(ctx, *then_branch, expected),
                    None => self.check_expr(ctx, *then_branch),
                };
                let else_ty = else_branch
                    .map(|expr| match expected {
                        Some(expected) => self.check_expr_expected(ctx, expr, expected),
                        None => self.check_expr(ctx, expr),
                    })
                    .unwrap_or(Type::Unit);
                self.join_branch_types(then_ty, else_ty, "if branches", span)
            }
            Expr::While { condition, body } => {
                let condition_ty = self.check_expr(ctx, *condition);
                self.expect_assignable(
                    &Type::Bool,
                    &condition_ty,
                    "while condition",
                    ctx.expr_range(*condition),
                );
                self.check_expr(ctx, *body);
                Type::Unit
            }
            Expr::For {
                pat,
                iterable,
                body,
            } => self.check_for(ctx, expr_id, *pat, *iterable, *body, span),
            Expr::Match { scrutinee, arms } => {
                self.check_match(ctx, *scrutinee, arms, expected, span)
            }
            Expr::Array { elements } => self.check_array(ctx, elements, expected, span),
            Expr::ArrayRepeat { value, len } => {
                self.check_array_repeat(ctx, *value, *len, expected, span)
            }
            Expr::Call { callee, args } => self.check_call(ctx, *callee, args, expected, span),
            Expr::FieldAccess { base, field } => self.check_field_access(ctx, *base, field, span),
            Expr::Unsafe { body } => {
                let ty = match expected {
                    Some(expected) => self.check_expr_expected(ctx, *body, expected),
                    None => self.check_expr(ctx, *body),
                };
                ty
            }
            Expr::IndexAccess { base, index } => {
                let base_ty = self.check_expr(ctx, *base);
                let index_ty = self.check_expr(ctx, *index);
                if !index_ty.is_unknown_like() && !index_ty.is_integer() {
                    self.expect_assignable(
                        &Type::Int(IntTy::I32),
                        &index_ty,
                        "index",
                        ctx.expr_range(*index),
                    );
                }
                // Extract element type from array / pointer base.
                match &base_ty {
                    Type::Array(inner, _) | Type::Ptr { inner, .. } => *inner.clone(),
                    _ => Type::Unknown,
                }
            }
            Expr::Cast { base, target } => {
                self.check_expr(ctx, *base);
                self.lower_type_ref(target)
            }
        };

        self.result
            .expr_types
            .insert((ctx.body_id, expr_id), ty.clone());
        ty
    }

    fn check_binary(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        expr_id: ExprId,
        lhs: ExprId,
        rhs: ExprId,
        op: BinaryOp,
        expected: Option<&Type>,
        span: Option<rowan::TextRange>,
    ) -> Type {
        if op == BinaryOp::Assign {
            let lhs_ty = self.check_expr(ctx, lhs);
            let rhs_ty = self.check_expr_expected(ctx, rhs, &lhs_ty);
            self.expect_assignable(&lhs_ty, &rhs_ty, "assignment", span);
            self.check_assign_mut(ctx, lhs, span);
            return Type::Unit;
        }

        if let Some(base_op) = op.compound_base() {
            let lhs_ty = self.check_expr(ctx, lhs);
            let rhs_ty = match base_op {
                BinaryOp::Add
                | BinaryOp::Sub
                | BinaryOp::Mul
                | BinaryOp::Div
                | BinaryOp::Mod
                | BinaryOp::BitAnd
                | BinaryOp::BitOr
                | BinaryOp::BitXor
                    if lhs_ty.is_numeric() || lhs_ty.is_bitwise_scalar() =>
                {
                    self.check_expr_expected(ctx, rhs, &lhs_ty)
                }
                _ => self.check_expr(ctx, rhs),
            };
            let result_ty = self.check_binary_types(ctx, lhs, rhs, base_op, &lhs_ty, &rhs_ty, span);
            self.expect_assignable(&lhs_ty, &result_ty, "assignment", span);
            self.check_assign_mut(ctx, lhs, span);
            return Type::Unit;
        }

        let lhs_ty = match (op, expected) {
            (
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod,
                Some(expected),
            ) if expected.is_numeric() => self.check_expr_expected(ctx, lhs, expected),
            _ => self.check_expr(ctx, lhs),
        };
        if op == BinaryOp::Add && !lhs_ty.is_numeric() && !lhs_ty.is_unknown_like() {
            if let Some(ty) = self.check_overloaded_add(ctx, expr_id, lhs, rhs, &lhs_ty, span) {
                return ty;
            }
        }
        let rhs_ty = match op {
            BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Mod
            | BinaryOp::Eq
            | BinaryOp::Neq
            | BinaryOp::Lt
            | BinaryOp::Gt
            | BinaryOp::LtEq
            | BinaryOp::GtEq
                if lhs_ty.is_numeric() =>
            {
                self.check_expr_expected(ctx, rhs, &lhs_ty)
            }
            _ => self.check_expr(ctx, rhs),
        };

        self.check_binary_types(ctx, lhs, rhs, op, &lhs_ty, &rhs_ty, span)
    }

    fn check_overloaded_add(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        expr_id: ExprId,
        lhs: ExprId,
        rhs: ExprId,
        lhs_ty: &Type,
        span: Option<rowan::TextRange>,
    ) -> Option<Type> {
        let trait_id = self.find_lang_trait("add")?;
        if let Some(ty) = self.check_trait_bound_add(ctx, lhs, rhs, lhs_ty, trait_id) {
            return Some(ty);
        }
        let method = self.find_trait_impl_method(lhs_ty, trait_id, "add")?;

        let receiver = method.function.params.first()?;
        let expected_receiver = self.lower_type_ref_with_params(&receiver.ty, &method.subst);
        let actual_receiver = self.receiver_argument_type(lhs_ty, &expected_receiver);
        self.expect_assignable(
            &expected_receiver,
            &actual_receiver,
            "operator receiver",
            ctx.expr_range(lhs),
        );

        let Some(rhs_param) = method.function.params.get(1) else {
            self.check_expr(ctx, rhs);
            self.diagnostic(
                "E0005",
                format!(
                    "operator `+` method `{}` needs a rhs parameter",
                    method.function.name.0
                ),
                span,
            );
            return Some(Type::Error);
        };
        let expected_rhs = self.lower_type_ref_with_params(&rhs_param.ty, &method.subst);
        let actual_rhs = self.check_expr_expected(ctx, rhs, &expected_rhs);
        self.expect_assignable(
            &expected_rhs,
            &actual_rhs,
            "right operand",
            ctx.expr_range(rhs),
        );

        self.result.operator_calls.insert(
            (ctx.body_id, expr_id),
            OperatorCall {
                function: method.fid,
            },
        );

        Some(
            method
                .function
                .ret_type
                .as_ref()
                .map(|ty| self.lower_type_ref_with_params(ty, &method.subst))
                .unwrap_or(Type::Unit),
        )
    }

    fn check_trait_bound_add(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        _lhs: ExprId,
        rhs: ExprId,
        lhs_ty: &Type,
        trait_id: hir::item_tree::TraitId,
    ) -> Option<Type> {
        let Type::Param(param) = lhs_ty else {
            return None;
        };
        let bounds = self.current_generic_bounds(ctx);
        let bound = bounds
            .iter()
            .find(|bound| {
                bound_target_param(bound).is_some_and(|name| name == *param)
                    && self.resolve_trait_ref(&bound.trait_ty) == Some(trait_id)
            })?
            .clone();

        let actual_rhs = self.check_expr_expected(ctx, rhs, lhs_ty);
        self.expect_assignable(lhs_ty, &actual_rhs, "right operand", ctx.expr_range(rhs));
        let output = self
            .bound_assoc_type(ctx, &bound, "Output")
            .unwrap_or(Type::Unknown);
        Some(output)
    }

    fn check_binary_types(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        lhs: ExprId,
        rhs: ExprId,
        op: BinaryOp,
        lhs_ty: &Type,
        rhs_ty: &Type,
        span: Option<rowan::TextRange>,
    ) -> Type {
        match op {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
                self.expect_numeric(lhs_ty, "left operand", ctx.expr_range(lhs));
                self.expect_numeric(rhs_ty, "right operand", ctx.expr_range(rhs));
                if !lhs_ty.is_numeric() || !rhs_ty.is_numeric() {
                    return Type::Error;
                }
                if lhs_ty.is_unknown_like() || rhs_ty.is_unknown_like() {
                    Type::Unknown
                } else if let Some(result) = self.join_numeric_types(lhs_ty, rhs_ty) {
                    result
                } else {
                    self.diagnostic(
                        "E0001",
                        format!(
                            "binary operands have different types: {} and {}",
                            lhs_ty.display(self.hir),
                            rhs_ty.display(self.hir)
                        ),
                        span,
                    );
                    Type::Error
                }
            }
            BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor => {
                if !lhs_ty.is_unknown_like()
                    && !rhs_ty.is_unknown_like()
                    && (!lhs_ty.is_bitwise_scalar() || !rhs_ty.is_bitwise_scalar())
                {
                    self.diagnostic(
                        "E0003",
                        format!(
                            "bitwise operation requires integer or bool operands, got {} and {}",
                            lhs_ty.display(self.hir),
                            rhs_ty.display(self.hir)
                        ),
                        span,
                    );
                    return Type::Error;
                }
                if lhs_ty == &Type::Bool && rhs_ty == &Type::Bool {
                    Type::Bool
                } else if let Some(result) = self.join_numeric_types(lhs_ty, rhs_ty) {
                    result
                } else {
                    self.diagnostic(
                        "E0001",
                        format!(
                            "bitwise operands have different types: {} and {}",
                            lhs_ty.display(self.hir),
                            rhs_ty.display(self.hir)
                        ),
                        span,
                    );
                    Type::Error
                }
            }
            BinaryOp::Shl | BinaryOp::Shr => {
                if !lhs_ty.is_unknown_like()
                    && !rhs_ty.is_unknown_like()
                    && (!lhs_ty.is_integer() || !rhs_ty.is_integer())
                {
                    self.diagnostic(
                        "E0003",
                        format!(
                            "shift operation requires integer operands, got {} and {}",
                            lhs_ty.display(self.hir),
                            rhs_ty.display(self.hir)
                        ),
                        span,
                    );
                    return Type::Error;
                }
                lhs_ty.clone()
            }
            BinaryOp::Eq | BinaryOp::Neq => {
                if self.join_numeric_types(lhs_ty, rhs_ty).is_none() {
                    self.expect_assignable(lhs_ty, rhs_ty, "comparison", span);
                }
                if !self.is_builtin_equality(lhs_ty, rhs_ty)
                    && !lhs_ty.is_unknown_like()
                    && !rhs_ty.is_unknown_like()
                    && !self.type_has_lang_trait(ctx, lhs_ty, "partial_eq")
                {
                    self.diagnostic(
                        "E0036",
                        format!(
                            "type `{}` must implement `PartialEq` for equality comparison",
                            lhs_ty.display(self.hir)
                        ),
                        span,
                    );
                }
                Type::Bool
            }
            BinaryOp::Lt | BinaryOp::Gt | BinaryOp::LtEq | BinaryOp::GtEq => {
                if self.is_builtin_ordering(lhs_ty, rhs_ty) {
                    if *lhs_ty != Type::Char && self.join_numeric_types(lhs_ty, rhs_ty).is_none() {
                        self.expect_assignable(lhs_ty, rhs_ty, "comparison", span);
                    }
                } else if !lhs_ty.is_unknown_like()
                    && !rhs_ty.is_unknown_like()
                    && self.type_has_lang_trait(ctx, lhs_ty, "partial_ord")
                {
                    self.expect_assignable(lhs_ty, rhs_ty, "comparison", span);
                } else if !lhs_ty.is_unknown_like() && !rhs_ty.is_unknown_like() {
                    self.diagnostic(
                        "E0003",
                        format!(
                            "ordered comparison requires compatible numeric or char operands or `PartialOrd`, got {} and {}",
                            lhs_ty.display(self.hir),
                            rhs_ty.display(self.hir)
                        ),
                        span,
                    );
                    return Type::Error;
                }
                Type::Bool
            }
            BinaryOp::And | BinaryOp::Or => {
                self.expect_assignable(&Type::Bool, lhs_ty, "left operand", ctx.expr_range(lhs));
                self.expect_assignable(&Type::Bool, rhs_ty, "right operand", ctx.expr_range(rhs));
                Type::Bool
            }
            BinaryOp::Assign
            | BinaryOp::AddAssign
            | BinaryOp::SubAssign
            | BinaryOp::MulAssign
            | BinaryOp::DivAssign
            | BinaryOp::ModAssign
            | BinaryOp::BitAndAssign
            | BinaryOp::BitOrAssign
            | BinaryOp::BitXorAssign
            | BinaryOp::ShlAssign
            | BinaryOp::ShrAssign => unreachable!(),
        }
    }

    fn check_unary(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        operand: ExprId,
        op: UnaryOp,
        expected: Option<&Type>,
        _span: Option<rowan::TextRange>,
    ) -> Type {
        let operand_ty = match (op, expected) {
            (UnaryOp::Neg | UnaryOp::Pos, Some(expected)) if expected.is_numeric() => {
                self.check_expr_expected(ctx, operand, expected)
            }
            _ => self.check_expr(ctx, operand),
        };
        match op {
            UnaryOp::Neg | UnaryOp::Pos => {
                self.expect_numeric(&operand_ty, "unary operand", ctx.expr_range(operand));
                operand_ty
            }
            UnaryOp::Not => {
                self.expect_assignable(
                    &Type::Bool,
                    &operand_ty,
                    "unary operand",
                    ctx.expr_range(operand),
                );
                Type::Bool
            }
            UnaryOp::Ref => Type::Ref(Box::new(operand_ty), false),
            UnaryOp::MutRef => {
                self.check_assign_mut(ctx, operand, ctx.expr_range(operand));
                Type::Ref(Box::new(operand_ty), true)
            }
            UnaryOp::Deref => match &operand_ty {
                Type::Ref(inner, _) => *inner.clone(),
                Type::Ptr { inner, .. } => *inner.clone(),
                Type::Unknown | Type::Error => operand_ty,
                other => {
                    self.diagnostic(
                        "E0008",
                        format!(
                            "cannot dereference value of type {}",
                            other.display(self.hir)
                        ),
                        ctx.expr_range(operand),
                    );
                    Type::Error
                }
            },
        }
    }

    fn check_struct_expr(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        resolved: Option<&ResolvedName>,
        fields: &[hir::body::StructExprField],
        expected: Option<&Type>,
        span: Option<rowan::TextRange>,
    ) -> Type {
        let Some(ResolvedName::Struct(struct_id)) = resolved else {
            for field in fields {
                self.check_expr(ctx, field.value);
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
            let pattern = self.lower_type_ref_with_params_at(
                &expected_field.ty,
                &generic_param_map_with_consts(
                    strukt.generics.iter().map(|name| name.0.as_str()),
                    strukt.const_generics.iter().map(|name| name.0.as_str()),
                ),
                Some(expected_field.ty_range),
            );
            let expected = substitute_type(&pattern, &subst);
            let actual = if expected.is_unknown_like() || expected_has_param(&expected) {
                self.check_expr(ctx, field.value)
            } else {
                self.check_expr_expected(ctx, field.value, &expected)
            };
            collect_subst(&pattern, &actual, &mut subst);
            let expected = substitute_type(&pattern, &subst);
            self.expect_assignable(&expected, &actual, "struct field", span);
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

    fn check_match(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        scrutinee: ExprId,
        arms: &[MatchArm],
        expected: Option<&Type>,
        span: Option<rowan::TextRange>,
    ) -> Type {
        let scrutinee_ty = self.check_expr(ctx, scrutinee);
        let mut result = None;

        for arm in arms {
            ctx.push_scope();
            self.bind_pattern(ctx, arm.pat, &scrutinee_ty);
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

            result = Some(match result {
                None => arm_ty,
                Some(prev) => self.join_branch_types(prev, arm_ty, "match arms", span),
            });
        }

        result.unwrap_or(Type::Unit)
    }

    fn check_for(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        expr_id: ExprId,
        pat: PatId,
        iterable: ExprId,
        body: ExprId,
        span: Option<rowan::TextRange>,
    ) -> Type {
        let iterable_ty = self.check_expr(ctx, iterable);
        let Some(into_iter_trait) = self.find_trait_by_name("IntoIterator") else {
            if let Type::Array(item_ty, _) = &iterable_ty {
                ctx.push_scope();
                self.bind_pattern(ctx, pat, item_ty);
                self.check_expr(ctx, body);
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

            if let (Some(into_iter_method), Some(next_method)) = (
                self.find_trait_impl_method(&iterable_ty, into_iter_trait, "into_iter"),
                self.find_trait_impl_method(&into_iter_ty, iterator_trait, "next"),
            ) {
                self.result.for_loops.insert(
                    (ctx.body_id, expr_id),
                    ForLoopInfo {
                        into_iter: into_iter_method.fid,
                        next: next_method.fid,
                        item_ty: item_ty.clone(),
                        iter_ty: into_iter_ty.clone(),
                    },
                );
            }
        }

        ctx.push_scope();
        self.bind_pattern(ctx, pat, &item_ty);
        self.check_expr(ctx, body);
        ctx.pop_scope();

        Type::Unit
    }

    fn check_array(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        elements: &[ExprId],
        expected: Option<&Type>,
        span: Option<rowan::TextRange>,
    ) -> Type {
        let (expected_element, expected_len) = match expected {
            Some(Type::Array(inner, len)) => (Some(inner.as_ref()), len.as_usize()),
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
        if let Some(expected_len) = expected_len {
            if expected_len != elements.len() {
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
        }

        Type::Array(
            Box::new(
                element_ty
                    .or_else(|| expected_element.cloned())
                    .unwrap_or(Type::Unknown),
            ),
            ConstArg::Value(elements.len()),
        )
    }

    fn check_array_repeat(
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
        if !matches!(len_ty, Type::Int(_)) {
            self.expect_assignable(
                &Type::Int(IntTy::I32),
                &len_ty,
                "array length",
                ctx.expr_range(len),
            );
        }
        let len_value = match &ctx.body.exprs[len] {
            Expr::IntLiteral { value, .. } if *value >= 0 => *value as usize,
            _ => {
                self.diagnostic(
                    "E0002",
                    "array repeat length must be a non-negative integer literal",
                    ctx.expr_range(len),
                );
                0
            }
        };
        if let Some(expected_len) = expected_len {
            if expected_len != len_value {
                self.diagnostic(
                    "E0001",
                    format!(
                        "array length mismatch: expected {}, got {}",
                        expected_len, len_value
                    ),
                    span,
                );
            }
        }

        Type::Array(Box::new(value_ty), ConstArg::Value(len_value))
    }

    fn repeat_value_is_copy(&self, ty: &Type) -> bool {
        match ty {
            Type::Array(inner, _) => self.repeat_value_is_copy(inner),
            Type::Tuple(elements) => elements.iter().all(|elem| self.repeat_value_is_copy(elem)),
            _ => self.result.trait_env.type_is_copy(ty),
        }
    }

    fn check_call(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        callee: ExprId,
        args: &[ExprId],
        expected: Option<&Type>,
        span: Option<rowan::TextRange>,
    ) -> Type {
        if let Expr::FieldAccess { base, field } = &ctx.body.exprs[callee] {
            return self.check_method_call(ctx, callee, *base, field.clone(), args, span);
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

        let callee_ty = self.check_expr(ctx, callee);
        let Type::Function(fid) = callee_ty else {
            for arg in args {
                self.check_expr(ctx, *arg);
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

        let function = &self.hir.item_tree.functions[fid];
        let params = generic_param_map_with_consts(
            function.generics.iter().map(|name| name.0.as_str()),
            function.const_generics.iter().map(|name| name.0.as_str()),
        );
        let mut subst = HashMap::new();
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
                let pattern = self.lower_type_ref_with_params(&param.ty, &params);
                let expected = substitute_type(&pattern, &subst);
                let actual = if expected_has_param(&expected) {
                    self.check_expr(ctx, *arg)
                } else {
                    self.check_expr_expected(ctx, *arg, &expected)
                };
                collect_subst(&pattern, &actual, &mut subst);
                let expected = substitute_type(&pattern, &subst);
                self.expect_assignable(
                    &expected,
                    &actual,
                    "function argument",
                    ctx.expr_range(*arg),
                );
            } else {
                self.check_expr(ctx, *arg);
            }
        }

        if !function.generics.is_empty() || !function.const_generics.is_empty() {
            if function
                .generics
                .iter()
                .chain(function.const_generics.iter())
                .any(|name| subst.get(&name.0).is_none_or(generic_arg_unknown))
            {
                self.diagnostic(
                    "E0005",
                    format!(
                        "cannot infer type argument(s) for function `{}`",
                        function.name.0
                    ),
                    span,
                );
            }
            self.check_generic_bounds(ctx, function, &subst, span);
            let args = function
                .generics
                .iter()
                .chain(function.const_generics.iter())
                .map(|name| subst.get(&name.0).cloned().unwrap_or(Type::Unknown))
                .collect::<Vec<_>>();
            self.generic_edges.push(GenericEdge {
                caller: ctx.function_id,
                callee: fid,
                grows: args
                    .iter()
                    .any(|arg| grows_generic_arg(arg, &ctx.generic_params)),
                span,
            });
            self.result
                .generic_calls
                .insert((ctx.body_id, callee), crate::result::GenericCall { args });
        }

        function
            .ret_type
            .as_ref()
            .map(|ty| substitute_type(&self.lower_type_ref_with_params(ty, &params), &subst))
            .unwrap_or(Type::Unit)
    }

    fn check_enum_variant_call(
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
                let pattern = self.lower_type_ref_with_params(field, &params);
                let expected = substitute_type(&pattern, &subst);
                let actual = if expected_has_param(&expected) {
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
            } else {
                self.check_expr(ctx, *arg);
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

    fn enum_variant_type(&self, enum_id: hir::item_tree::EnumId, expected: Option<&Type>) -> Type {
        if let Some(Type::Enum(expected_id, args)) = expected {
            if *expected_id == enum_id {
                return Type::Enum(enum_id, args.clone());
            }
        }
        let args = self.hir.item_tree.enums[enum_id]
            .generics
            .iter()
            .chain(self.hir.item_tree.enums[enum_id].const_generics.iter())
            .map(|_| Type::Unknown)
            .collect();
        Type::Enum(enum_id, args)
    }

    fn check_generic_bounds(
        &mut self,
        ctx: &BodyCtx<'_>,
        function: &hir::item_tree::HirFunction,
        subst: &HashMap<String, Type>,
        span: Option<rowan::TextRange>,
    ) {
        for bound in &function.generic_bounds {
            let actual = self.lower_type_ref_with_params(&bound.target_ty, subst);
            if actual.is_unknown_like() {
                continue;
            }
            let Some(trait_id) = self.resolve_trait_ref(&bound.trait_ty) else {
                self.diagnostic(
                    "E0023",
                    format!(
                        "generic bound references unknown trait `{}`",
                        bound.trait_ty.display()
                    ),
                    span,
                );
                continue;
            };
            if !self.type_satisfies_bound(ctx, &actual, trait_id, &bound.assoc_constraints, subst) {
                let trait_name = self.hir.item_tree.traits[trait_id].name.0.clone();
                self.diagnostic(
                    "E0035",
                    format!(
                        "type `{}` does not satisfy bound `{}` for `{}`",
                        actual.display(self.hir),
                        trait_name,
                        bound.target_ty.display()
                    ),
                    span,
                );
            }
        }
    }

    pub(crate) fn check_type_bounds(
        &mut self,
        ctx: &BodyCtx<'_>,
        ty: &Type,
        span: Option<rowan::TextRange>,
    ) {
        match ty {
            Type::Ref(inner, _) | Type::Ptr { inner, .. } | Type::Array(inner, _) => {
                self.check_type_bounds(ctx, inner, span)
            }
            Type::Tuple(elements) => {
                for element in elements {
                    self.check_type_bounds(ctx, element, span);
                }
            }
            Type::Struct(struct_id, args) => {
                for arg in args {
                    self.check_type_bounds(ctx, arg, span);
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
                    self.check_type_bounds(ctx, arg, span);
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
            Type::Function(_)
            | Type::Param(_)
            | Type::Const(_)
            | Type::Unknown
            | Type::Error
            | Type::InferInt
            | Type::InferFloat
            | Type::Int(_)
            | Type::Float(_)
            | Type::Bool
            | Type::Str
            | Type::Char
            | Type::Unit
            | Type::Never => {}
        }
    }

    fn check_item_bounds(
        &mut self,
        ctx: &BodyCtx<'_>,
        item_name: &str,
        bounds: &[HirGenericBound],
        subst: &HashMap<String, Type>,
        span: Option<rowan::TextRange>,
    ) {
        for bound in bounds {
            let actual = self.lower_type_ref_with_params(&bound.target_ty, subst);
            if actual.is_unknown_like() {
                continue;
            }
            let Some(trait_id) = self.resolve_trait_ref(&bound.trait_ty) else {
                self.diagnostic(
                    "E0023",
                    format!(
                        "generic bound references unknown trait `{}`",
                        bound.trait_ty.display()
                    ),
                    span,
                );
                continue;
            };
            if !self.type_satisfies_bound(ctx, &actual, trait_id, &bound.assoc_constraints, subst) {
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

    fn current_generic_bounds(&self, ctx: &BodyCtx<'_>) -> Vec<HirGenericBound> {
        let mut bounds = self
            .hir
            .item_tree
            .impls
            .iter()
            .find_map(|(_, imp)| {
                imp.methods
                    .contains(&ctx.function_id)
                    .then(|| imp.generic_bounds.clone())
            })
            .unwrap_or_default();
        bounds.extend(ctx.function.generic_bounds.clone());
        bounds
    }

    fn type_satisfies_bound(
        &mut self,
        ctx: &BodyCtx<'_>,
        actual: &Type,
        trait_id: hir::item_tree::TraitId,
        assoc_constraints: &[HirAssocTypeConstraint],
        subst: &HashMap<String, Type>,
    ) -> bool {
        if actual.is_unknown_like() {
            return true;
        }
        if let Type::Param(param) = actual {
            return self.param_has_trait_bound(ctx, param, trait_id, assoc_constraints, subst);
        }
        self.result.trait_env.type_implements(actual, trait_id)
            && self.assoc_constraints_match(actual, trait_id, assoc_constraints, subst)
    }

    fn type_has_lang_trait(&mut self, ctx: &BodyCtx<'_>, ty: &Type, lang: &str) -> bool {
        let Some(trait_id) = self.find_lang_trait(lang) else {
            return false;
        };
        self.type_has_trait_id(ctx, ty, trait_id)
    }

    fn type_has_trait_id(&mut self, ctx: &BodyCtx<'_>, ty: &Type, trait_id: TraitId) -> bool {
        if ty.is_unknown_like() {
            return true;
        }
        if let Type::Param(param) = ty {
            return self.param_has_trait_bound(ctx, param, trait_id, &[], &ctx.generic_params);
        }
        self.result.trait_env.type_implements(ty, trait_id)
    }

    fn associated_type_for(
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
                    if !bound_target_param(&bound).is_some_and(|name| name == *param) {
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

    fn param_has_trait_bound(
        &mut self,
        ctx: &BodyCtx<'_>,
        param: &str,
        required_trait: hir::item_tree::TraitId,
        required_assoc: &[HirAssocTypeConstraint],
        subst: &HashMap<String, Type>,
    ) -> bool {
        self.current_generic_bounds(ctx).into_iter().any(|bound| {
            if !bound_target_param(&bound).is_some_and(|name| name == param) {
                return false;
            }
            let Some(bound_trait) = self.resolve_trait_ref(&bound.trait_ty) else {
                return false;
            };
            if !self.trait_implies(bound_trait, required_trait) {
                return false;
            }
            required_assoc.iter().all(|required| {
                let expected = self.lower_type_ref_with_params(&required.ty, subst);
                self.bound_assoc_type(ctx, &bound, &required.name.0)
                    .map(|actual| self.bound_types_match(&expected, &actual))
                    .unwrap_or(false)
            })
        })
    }

    fn assoc_constraints_match(
        &mut self,
        actual: &Type,
        trait_id: hir::item_tree::TraitId,
        assoc_constraints: &[HirAssocTypeConstraint],
        subst: &HashMap<String, Type>,
    ) -> bool {
        assoc_constraints.iter().all(|constraint| {
            let expected = self.lower_type_ref_with_params(&constraint.ty, subst);
            self.result
                .trait_env
                .associated_type(actual, trait_id, &constraint.name.0)
                .map(|actual| self.bound_types_match(&expected, &actual))
                .unwrap_or(false)
        })
    }

    fn impl_bounds_satisfied(
        &mut self,
        imp: &hir::item_tree::HirImpl,
        subst: &HashMap<String, Type>,
    ) -> bool {
        imp.generic_bounds.iter().all(|bound| {
            let actual = self.lower_type_ref_with_params(&bound.target_ty, subst);
            let Some(trait_id) = self.resolve_trait_ref(&bound.trait_ty) else {
                return false;
            };
            self.result.trait_env.type_implements(&actual, trait_id)
                && self.assoc_constraints_match(&actual, trait_id, &bound.assoc_constraints, subst)
        })
    }

    fn bound_assoc_type(
        &mut self,
        ctx: &BodyCtx<'_>,
        bound: &HirGenericBound,
        name: &str,
    ) -> Option<Type> {
        bound
            .assoc_constraints
            .iter()
            .find(|constraint| constraint.name.0 == name)
            .map(|constraint| self.lower_type_ref_with_params(&constraint.ty, &ctx.generic_params))
    }

    fn trait_implies(
        &self,
        actual: hir::item_tree::TraitId,
        required: hir::item_tree::TraitId,
    ) -> bool {
        if actual == required {
            return true;
        }
        matches!(
            (self.trait_lang(actual), self.trait_lang(required)),
            (Some("eq"), Some("partial_eq"))
                | (Some("partial_ord"), Some("partial_eq"))
                | (Some("ord"), Some("partial_eq" | "eq" | "partial_ord"))
        )
    }

    fn bound_types_match(&self, expected: &Type, actual: &Type) -> bool {
        expected.is_unknown_like()
            || actual.is_unknown_like()
            || expected == actual
            || self.numeric_assignable(expected, actual)
    }

    fn is_builtin_equality(&self, lhs_ty: &Type, rhs_ty: &Type) -> bool {
        self.join_numeric_types(lhs_ty, rhs_ty).is_some()
            || matches!(
                (lhs_ty, rhs_ty),
                (Type::Bool, Type::Bool)
                    | (Type::Char, Type::Char)
                    | (Type::Str, Type::Str)
                    | (Type::Unit, Type::Unit)
            )
    }

    fn is_builtin_ordering(&self, lhs_ty: &Type, rhs_ty: &Type) -> bool {
        lhs_ty.is_ordered_scalar()
            && rhs_ty.is_ordered_scalar()
            && (*lhs_ty == Type::Char) == (*rhs_ty == Type::Char)
    }

    fn check_method_call(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        callee: ExprId,
        base: ExprId,
        method_name: Name,
        args: &[ExprId],
        span: Option<rowan::TextRange>,
    ) -> Type {
        let base_ty = self.check_expr(ctx, base);
        let Some(method) = self.find_method(ctx, &base_ty, &method_name) else {
            for arg in args {
                self.check_expr(ctx, *arg);
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
        };

        self.result
            .expr_types
            .insert((ctx.body_id, callee), Type::Function(method.fid));
        if let Some(trait_id) = method.trait_id {
            self.result.trait_method_calls.insert(
                (ctx.body_id, callee),
                TraitMethodCall {
                    trait_id,
                    method: method.function.name.0.clone(),
                },
            );
        }

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
            let expected = self.lower_type_ref_with_params(&receiver.ty, &method.subst);
            let actual = self.receiver_argument_type(&base_ty, &expected);
            if matches!(expected, Type::Ref(_, true)) {
                self.check_assign_mut(ctx, base, ctx.expr_range(base));
            }
            self.expect_assignable(&expected, &actual, "method receiver", ctx.expr_range(base));
        }

        for (index, arg) in args.iter().enumerate() {
            if let Some(param) = method.function.params.get(index + receiver_count) {
                let expected = self.lower_type_ref_with_params(&param.ty, &method.subst);
                let actual = self.check_expr_expected(ctx, *arg, &expected);
                self.expect_assignable(&expected, &actual, "method argument", ctx.expr_range(*arg));
            } else {
                self.check_expr(ctx, *arg);
            }
        }

        method
            .function
            .ret_type
            .as_ref()
            .map(|ty| self.lower_type_ref_with_params(ty, &method.subst))
            .unwrap_or(Type::Unit)
    }

    fn find_method(
        &mut self,
        ctx: &BodyCtx<'_>,
        receiver_ty: &Type,
        method_name: &Name,
    ) -> Option<ResolvedMethod> {
        self.find_inherent_method(receiver_ty, method_name)
            .or_else(|| self.find_trait_impl_method_by_name(receiver_ty, method_name))
            .or_else(|| self.find_trait_bound_method(ctx, receiver_ty, method_name))
    }

    fn find_inherent_method(
        &mut self,
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
            let Some(mut subst) = self.impl_subst_from_self_ty(&imp, receiver_self_ty) else {
                continue;
            };
            if !self.impl_bounds_satisfied(&imp, &subst) {
                continue;
            }
            subst.insert("Self".into(), receiver_self_ty.clone());
            for fid in imp.methods {
                if self.hir.item_tree.functions[fid].name == *method_name {
                    return Some(ResolvedMethod {
                        fid,
                        function: self.hir.item_tree.functions[fid].clone(),
                        subst,
                        trait_id: None,
                    });
                }
            }
        }

        None
    }

    fn find_trait_impl_method_by_name(
        &mut self,
        receiver_ty: &Type,
        method_name: &Name,
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
            let Some(trait_id) = self.resolve_trait_ref(trait_ty) else {
                continue;
            };
            let Some(mut subst) = self.impl_subst_from_self_ty(&imp, receiver_ty) else {
                continue;
            };
            if !self.impl_bounds_satisfied(&imp, &subst) {
                continue;
            }
            subst.insert("Self".into(), receiver_ty.clone());
            let Some(fid) = imp
                .methods
                .iter()
                .copied()
                .find(|fid| self.hir.item_tree.functions[*fid].name == *method_name)
            else {
                continue;
            };
            return Some(ResolvedMethod {
                fid,
                function: self.hir.item_tree.functions[fid].clone(),
                subst,
                trait_id: Some(trait_id),
            });
        }

        None
    }

    fn find_trait_impl_method(
        &mut self,
        receiver_ty: &Type,
        trait_id: hir::item_tree::TraitId,
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
            if !self.impl_bounds_satisfied(&imp, &subst) {
                continue;
            }
            subst.insert("Self".into(), receiver_ty.clone());
            let Some(fid) = imp
                .methods
                .iter()
                .copied()
                .find(|fid| self.hir.item_tree.functions[*fid].name.0 == method_name)
            else {
                continue;
            };
            return Some(ResolvedMethod {
                fid,
                function: self.hir.item_tree.functions[fid].clone(),
                subst,
                trait_id: Some(trait_id),
            });
        }

        None
    }

    fn find_trait_bound_method(
        &mut self,
        ctx: &BodyCtx<'_>,
        receiver_ty: &Type,
        method_name: &Name,
    ) -> Option<ResolvedMethod> {
        let Type::Param(param) = receiver_ty else {
            return None;
        };
        let bounds = self
            .current_generic_bounds(ctx)
            .into_iter()
            .filter(|bound| bound_target_param(bound).is_some_and(|name| name == *param))
            .collect::<Vec<_>>();

        for bound in bounds {
            let Some(trait_id) = self.resolve_trait_ref(&bound.trait_ty) else {
                continue;
            };
            let Some(function) = self.hir.item_tree.traits[trait_id]
                .methods
                .iter()
                .find(|method| method.name == *method_name)
                .cloned()
            else {
                continue;
            };
            let mut subst = generic_param_map_with_consts(
                function.generics.iter().map(|name| name.0.as_str()),
                function.const_generics.iter().map(|name| name.0.as_str()),
            );
            subst.insert("Self".into(), receiver_ty.clone());
            return Some(ResolvedMethod {
                fid: ctx.function_id,
                function,
                subst,
                trait_id: Some(trait_id),
            });
        }
        None
    }

    fn receiver_argument_type(&self, base_ty: &Type, expected: &Type) -> Type {
        match expected {
            Type::Ref(inner, mutable) if inner.as_ref() == base_ty => {
                Type::Ref(Box::new(base_ty.clone()), *mutable)
            }
            _ => base_ty.clone(),
        }
    }

    fn check_field_access(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        base: ExprId,
        field: &Name,
        span: Option<rowan::TextRange>,
    ) -> Type {
        let base_ty = self.check_expr(ctx, base);
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
        let Some(field_ty) = strukt
            .fields
            .iter()
            .find(|candidate| candidate.name == *field)
            .map(|candidate| {
                self.lower_type_ref_with_params_at(&candidate.ty, &subst, Some(candidate.ty_range))
            })
        else {
            self.diagnostic(
                "E0006",
                format!("unknown field `{}` on struct `{}`", field.0, strukt.name.0),
                span,
            );
            return Type::Error;
        };

        field_ty
    }

    /// Check that the LHS of an assignment targets a mutable binding.
    fn check_assign_mut(&mut self, ctx: &BodyCtx<'_>, lhs: ExprId, span: Option<rowan::TextRange>) {
        if let Expr::Path { path, .. } = &ctx.body.exprs[lhs] {
            if let Some(name) = path.as_single_name() {
                if ctx.bindings.get(&name.0).is_some() {
                    self.diagnostic(
                        "E0031",
                        format!(
                            "cannot assign to `{}`, as it is not declared as mutable",
                            name.0
                        ),
                        span,
                    );
                    return;
                }
            }
        }
        if let Some(stmt_id) = self.root_local_of_expr(ctx, lhs) {
            if let Some((_, false)) = ctx.locals.get(&stmt_id) {
                let name = self.local_name(ctx, stmt_id);
                self.diagnostic(
                    "E0031",
                    format!(
                        "cannot assign to `{}`, as it is not declared as mutable",
                        name
                    ),
                    span,
                );
            }
        }
    }

    /// Walk the expression to find the root local StmtId (ignoring dereferences).
    fn root_local_of_expr(&self, ctx: &BodyCtx<'_>, expr_id: ExprId) -> Option<StmtId> {
        match &ctx.body.exprs[expr_id] {
            Expr::Path {
                resolved: Some(ResolvedName::Local(stmt)),
                ..
            } => Some(*stmt),
            Expr::FieldAccess { base, .. } => self.root_local_of_expr(ctx, *base),
            Expr::IndexAccess { base, .. } => self.root_local_of_expr(ctx, *base),
            _ => None,
        }
    }

    /// Get the name of a local binding for error messages.
    fn local_name(&self, ctx: &BodyCtx<'_>, stmt_id: StmtId) -> String {
        match &ctx.body.stmts[stmt_id] {
            Stmt::Let { name, .. } => name.0.clone(),
            _ => String::from("_"),
        }
    }

    fn bind_pattern(&mut self, ctx: &mut BodyCtx<'_>, pat: PatId, expected: &Type) {
        let span = ctx.pat_range(pat);
        match &ctx.body.pats[pat] {
            Pattern::Wildcard | Pattern::Literal | Pattern::Path { .. } => {}
            Pattern::Binding { name } => {
                ctx.bindings.insert(name.0.clone(), expected.clone());
            }
            Pattern::Tuple { elements } => {
                let Type::Tuple(expected_elements) = expected else {
                    if !expected.is_unknown_like() {
                        self.diagnostic(
                            "E0010",
                            format!(
                                "tuple pattern cannot match value of type {}",
                                expected.display(self.hir)
                            ),
                            span,
                        );
                    }
                    for element in elements {
                        self.bind_pattern(ctx, *element, &Type::Unknown);
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
                for (index, element) in elements.iter().enumerate() {
                    let ty = expected_elements.get(index).unwrap_or(&Type::Unknown);
                    self.bind_pattern(ctx, *element, ty);
                }
            }
            Pattern::TupleStruct { elements, .. } => {
                for element in elements {
                    self.bind_pattern(ctx, *element, &Type::Unknown);
                }
            }
            Pattern::Struct { fields, .. } => {
                for field in fields {
                    if let Some(pat) = field.pat {
                        self.bind_pattern(ctx, pat, &Type::Unknown);
                    } else {
                        ctx.bindings.insert(field.name.0.clone(), Type::Unknown);
                    }
                }
            }
        }
    }

    fn type_of_resolved_name(
        &mut self,
        ctx: &BodyCtx<'_>,
        resolved: Option<&ResolvedName>,
    ) -> Type {
        match resolved {
            Some(ResolvedName::Local(stmt)) => ctx
                .locals
                .get(stmt)
                .map(|(ty, _)| ty.clone())
                .unwrap_or(Type::Unknown),
            Some(ResolvedName::Param(index)) => ctx
                .function
                .params
                .get(*index)
                .map(|param| self.lower_type_ref_with_params(&param.ty, &ctx.generic_params))
                .unwrap_or(Type::Unknown),
            Some(ResolvedName::Function(fid)) => Type::Function(*fid),
            Some(ResolvedName::Struct(sid)) => Type::Struct(*sid, Vec::new()),
            Some(ResolvedName::Const(cid)) => {
                let konst = &self.hir.item_tree.consts[*cid];
                self.lower_type_ref(&konst.ty)
            }
            Some(ResolvedName::TypeAlias(tid)) => self.lower_type_alias(*tid),
            Some(ResolvedName::Unresolved) | None => Type::Unknown,
            Some(ResolvedName::Enum(eid)) => Type::Enum(*eid, Vec::new()),
            Some(ResolvedName::EnumVariant(eid, _)) => Type::Enum(*eid, Vec::new()),
            Some(ResolvedName::Trait(_)) | Some(ResolvedName::Module(_)) => Type::Unknown,
        }
    }
}

struct ResolvedMethod {
    fid: FunctionId,
    function: HirFunction,
    subst: HashMap<String, Type>,
    trait_id: Option<hir::item_tree::TraitId>,
}

fn expected_has_param(ty: &Type) -> bool {
    match ty {
        Type::Param(_) | Type::Const(ConstArg::Param(_)) => true,
        Type::Ref(inner, _) => expected_has_param(inner),
        Type::Ptr { inner, .. } => expected_has_param(inner),
        Type::Tuple(elements) => elements.iter().any(expected_has_param),
        Type::Array(inner, len) => expected_has_param(inner) || const_has_param(len),
        Type::Struct(_, args) | Type::Enum(_, args) => args.iter().any(expected_has_param),
        _ => false,
    }
}

fn bound_target_param(bound: &HirGenericBound) -> Option<&str> {
    match &bound.target_ty {
        HirTypeRef::Named(path)
            if matches!(path.anchor, hir::item_tree::PathAnchor::Plain)
                && path.segments.len() == 1
                && path.type_args.is_empty() =>
        {
            Some(path.segments[0].0.as_str())
        }
        _ => None,
    }
}

fn const_has_param(value: &ConstArg) -> bool {
    matches!(value, ConstArg::Param(_))
}

fn generic_arg_unknown(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Unknown | Type::Error | Type::Const(ConstArg::Unknown | ConstArg::Error)
    )
}

fn type_contains_unresolved_const_param(ty: &Type, params: &HashMap<String, Type>) -> bool {
    match ty {
        Type::Const(ConstArg::Param(name)) => !matches!(params.get(name), Some(Type::Const(_))),
        Type::Ref(inner, _) | Type::Ptr { inner, .. } => {
            type_contains_unresolved_const_param(inner, params)
        }
        Type::Tuple(elements) => elements
            .iter()
            .any(|element| type_contains_unresolved_const_param(element, params)),
        Type::Array(inner, len) => {
            type_contains_unresolved_const_param(inner, params)
                || matches!(len, ConstArg::Param(name) if !matches!(params.get(name), Some(Type::Const(_))))
        }
        Type::Struct(_, args) | Type::Enum(_, args) => args
            .iter()
            .any(|arg| type_contains_unresolved_const_param(arg, params)),
        _ => false,
    }
}

fn type_ref_contains_error(ty: &HirTypeRef) -> bool {
    match ty {
        HirTypeRef::Error => true,
        HirTypeRef::Ref(inner, _) => type_ref_contains_error(inner),
        HirTypeRef::Array(inner, len) => {
            type_ref_contains_error(inner) || matches!(len, hir::item_tree::HirConstArg::Error)
        }
        HirTypeRef::Ptr { inner, .. } => type_ref_contains_error(inner),
        HirTypeRef::Tuple(elements) => elements.iter().any(type_ref_contains_error),
        HirTypeRef::Named(path) => path.type_args.iter().any(type_ref_contains_error),
        HirTypeRef::Const(value) => matches!(value, hir::item_tree::HirConstArg::Error),
        HirTypeRef::Unknown => false,
    }
}

fn grows_generic_arg(ty: &Type, params: &HashMap<String, Type>) -> bool {
    match ty {
        Type::Param(name) => !params.contains_key(name),
        Type::Const(ConstArg::Param(name)) => !params.contains_key(name),
        Type::Ref(inner, _) | Type::Ptr { inner, .. } => contains_current_param(inner, params),
        Type::Array(inner, len) => {
            contains_current_param(inner, params) || const_contains_current_param(len, params)
        }
        Type::Tuple(elements) => elements
            .iter()
            .any(|element| contains_current_param(element, params)),
        Type::Struct(_, args) | Type::Enum(_, args) => {
            args.iter().any(|arg| contains_current_param(arg, params))
        }
        _ => false,
    }
}

fn contains_current_param(ty: &Type, params: &HashMap<String, Type>) -> bool {
    match ty {
        Type::Param(name) => params.contains_key(name),
        Type::Const(ConstArg::Param(name)) => params.contains_key(name),
        Type::Ref(inner, _) | Type::Ptr { inner, .. } => contains_current_param(inner, params),
        Type::Array(inner, len) => {
            contains_current_param(inner, params) || const_contains_current_param(len, params)
        }
        Type::Tuple(elements) => elements
            .iter()
            .any(|element| contains_current_param(element, params)),
        Type::Struct(_, args) | Type::Enum(_, args) => {
            args.iter().any(|arg| contains_current_param(arg, params))
        }
        _ => false,
    }
}

fn const_contains_current_param(value: &ConstArg, params: &HashMap<String, Type>) -> bool {
    matches!(value, ConstArg::Param(name) if params.contains_key(name))
}
