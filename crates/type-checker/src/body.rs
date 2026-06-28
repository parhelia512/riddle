use std::collections::HashMap;

use hir::{
    Name,
    body::{BinaryOp, Expr, ExprId, MatchArm, PatId, Pattern, ResolvedName, Stmt, StmtId, UnaryOp},
};

use crate::{checker::TypeChecker, context::BodyCtx, types::{IntTy, Type}};

impl TypeChecker<'_> {
    pub(crate) fn check_stmt(&mut self, ctx: &mut BodyCtx<'_>, stmt_id: StmtId) {
        match &ctx.body.stmts[stmt_id] {
            Stmt::Let { ty, init, .. } => {
                let declared = self.lower_type_ref(ty);
                let init_ty = init.map(|expr| {
                    if declared.is_unknown_like() {
                        self.check_expr(ctx, expr)
                    } else {
                        self.check_expr_expected(ctx, expr, &declared)
                    }
                });

                if let Some(init_ty) = init_ty {
                    if !declared.is_unknown_like() {
                        self.expect_assignable(&declared, &init_ty, "let initializer", ctx.stmt_range(stmt_id));
                    }
                    ctx.locals.insert(stmt_id, declared.or(init_ty));
                } else {
                    ctx.locals.insert(stmt_id, declared);
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
                } else {
                    self.type_of_resolved_name(ctx, resolved.as_ref())
                }
            }
            Expr::Struct {
                resolved, fields, ..
            } => self.check_struct_expr(ctx, resolved.as_ref(), fields, span),
            Expr::Binary { lhs, rhs, op } => self.check_binary(ctx, *lhs, *rhs, *op, expected, span),
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
                self.expect_assignable(&Type::Bool, &cond_ty, "if condition", ctx.expr_range(*cond));

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
                self.expect_assignable(&Type::Bool, &condition_ty, "while condition", ctx.expr_range(*condition));
                self.check_expr(ctx, *body);
                Type::Unit
            }
            Expr::Match { scrutinee, arms } => self.check_match(ctx, *scrutinee, arms, expected, span),
            Expr::Array { elements } => self.check_array(ctx, elements, expected, span),
            Expr::Call { callee, args } => self.check_call(ctx, *callee, args, span),
            Expr::FieldAccess { base, field } => self.check_field_access(ctx, *base, field, span),
            Expr::Unsafe { body } => {
                let ty = match expected {
                    Some(expected) => self.check_expr_expected(ctx, *body, expected),
                    None => self.check_expr(ctx, *body),
                };
                ty
            }
            Expr::IndexAccess { base, index } => {
                let _base_ty = self.check_expr(ctx, *base);
                let _index_ty = self.check_expr(ctx, *index);
                self.expect_assignable(&Type::Int(IntTy::I32), &_index_ty, "index", ctx.expr_range(*index));
                // ponytail: return array element type when array type info is available
                Type::Unknown
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
            // ponytail: mutability check deferred — needs MIR/borrowck integration
            return Type::Unit;
        }

        let lhs_ty = match (op, expected) {
            (
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod,
                Some(expected),
            ) if expected.is_numeric() => self.check_expr_expected(ctx, lhs, expected),
            _ => self.check_expr(ctx, lhs),
        };
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

        match op {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
                self.expect_numeric(&lhs_ty, "left operand", ctx.expr_range(lhs));
                self.expect_numeric(&rhs_ty, "right operand", ctx.expr_range(rhs));
                if !lhs_ty.is_numeric() || !rhs_ty.is_numeric() {
                    return Type::Error;
                }
                if lhs_ty.is_unknown_like() || rhs_ty.is_unknown_like() {
                    Type::Unknown
                } else if let Some(result) = self.join_numeric_types(&lhs_ty, &rhs_ty) {
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
            BinaryOp::Eq | BinaryOp::Neq => {
                if self.join_numeric_types(&lhs_ty, &rhs_ty).is_none() {
                    self.expect_assignable(&lhs_ty, &rhs_ty, "comparison", span);
                }
                Type::Bool
            }
            BinaryOp::Lt | BinaryOp::Gt | BinaryOp::LtEq | BinaryOp::GtEq => {
                self.expect_numeric(&lhs_ty, "left operand", ctx.expr_range(lhs));
                self.expect_numeric(&rhs_ty, "right operand", ctx.expr_range(rhs));
                if self.join_numeric_types(&lhs_ty, &rhs_ty).is_none() {
                    self.expect_assignable(&lhs_ty, &rhs_ty, "comparison", span);
                }
                Type::Bool
            }
            BinaryOp::And | BinaryOp::Or => {
                self.expect_assignable(&Type::Bool, &lhs_ty, "left operand", ctx.expr_range(lhs));
                self.expect_assignable(&Type::Bool, &rhs_ty, "right operand", ctx.expr_range(rhs));
                Type::Bool
            }
            BinaryOp::Assign => unreachable!(),
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
                self.expect_assignable(&Type::Bool, &operand_ty, "unary operand", ctx.expr_range(operand));
                Type::Bool
            }
            UnaryOp::Ref => Type::Ref(Box::new(operand_ty), false),
            UnaryOp::MutRef => Type::Ref(Box::new(operand_ty), true),
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
        span: Option<rowan::TextRange>,
    ) -> Type {
        let Some(ResolvedName::Struct(struct_id)) = resolved else {
            for field in fields {
                self.check_expr(ctx, field.value);
            }
            self.diagnostic("E0009", "struct literal does not resolve to a struct", span);
            return Type::Error;
        };

        let strukt = &self.hir.item_tree.structs[*struct_id];
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
            let expected = self.lower_type_ref(&expected_field.ty);
            let actual = self.check_expr_expected(ctx, field.value, &expected);
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

        Type::Struct(*struct_id)
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
                self.expect_assignable(&Type::Bool, &guard_ty, "match guard", ctx.expr_range(guard));
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

    fn check_array(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        elements: &[ExprId],
        expected: Option<&Type>,
        _span: Option<rowan::TextRange>,
    ) -> Type {
        let expected_element = match expected {
            Some(Type::Array(inner)) => Some(inner.as_ref()),
            _ => None,
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

        Type::Array(Box::new(element_ty.unwrap_or(Type::Unknown)))
    }

    fn check_call(&mut self, ctx: &mut BodyCtx<'_>, callee: ExprId, args: &[ExprId], span: Option<rowan::TextRange>) -> Type {
        let callee_ty = self.check_expr(ctx, callee);
        let Type::Function(fid) = callee_ty else {
            for arg in args {
                self.check_expr(ctx, *arg);
            }
            if !callee_ty.is_unknown_like() {
                self.diagnostic(
                    "E0004",
                    format!(
                        "cannot call value of type {}",
                        callee_ty.display(self.hir)
                    ),
                    ctx.expr_range(callee),
                );
            }
            return Type::Error;
        };

        let function = &self.hir.item_tree.functions[fid];
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
                let expected = self.lower_type_ref(&param.ty);
                let actual = self.check_expr_expected(ctx, *arg, &expected);
                self.expect_assignable(&expected, &actual, "function argument", ctx.expr_range(*arg));
            } else {
                self.check_expr(ctx, *arg);
            }
        }

        function
            .ret_type
            .as_ref()
            .map(|ty| self.lower_type_ref(ty))
            .unwrap_or(Type::Unit)
    }

    fn check_field_access(&mut self, ctx: &mut BodyCtx<'_>, base: ExprId, field: &Name, span: Option<rowan::TextRange>) -> Type {
        let base_ty = self.check_expr(ctx, base);
        let struct_id = match &base_ty {
            Type::Struct(id) => Some(*id),
            Type::Ref(inner, _) => match inner.as_ref() {
                Type::Struct(id) => Some(*id),
                _ => None,
            },
            _ => None,
        };

        let Some(struct_id) = struct_id else {
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

        let strukt = &self.hir.item_tree.structs[struct_id];
        let Some(field_ty) = strukt
            .fields
            .iter()
            .find(|candidate| candidate.name == *field)
            .map(|candidate| self.lower_type_ref(&candidate.ty))
        else {
            self.diagnostic(
                "E0006",
                format!(
                    "unknown field `{}` on struct `{}`",
                    field.0, strukt.name.0
                ),
                span,
            );
            return Type::Error;
        };

        field_ty
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
            Some(ResolvedName::Local(stmt)) => {
                ctx.locals.get(stmt).cloned().unwrap_or(Type::Unknown)
            }
            Some(ResolvedName::Param(index)) => ctx
                .function
                .params
                .get(*index)
                .map(|param| self.lower_type_ref(&param.ty))
                .unwrap_or(Type::Unknown),
            Some(ResolvedName::Function(fid)) => Type::Function(*fid),
            Some(ResolvedName::Struct(sid)) => Type::Struct(*sid),
            Some(ResolvedName::Const(cid)) => {
                let konst = &self.hir.item_tree.consts[*cid];
                self.lower_type_ref(&konst.ty)
            }
            Some(ResolvedName::TypeAlias(tid)) => self.lower_type_alias(*tid),
            Some(ResolvedName::Unresolved) | None => Type::Unknown,
            Some(ResolvedName::Enum(eid)) => Type::Enum(*eid),
            Some(ResolvedName::EnumVariant(eid, _)) => Type::Enum(*eid),
            Some(ResolvedName::Trait(_))
            | Some(ResolvedName::Module(_)) => Type::Unknown,
        }
    }
}
