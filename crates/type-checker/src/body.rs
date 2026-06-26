use std::collections::HashMap;

use hir::{
    Name,
    body::{BinaryOp, Expr, ExprId, MatchArm, PatId, Pattern, ResolvedName, Stmt, StmtId, UnaryOp},
};

use crate::{checker::TypeChecker, context::BodyCtx, types::Type};

impl TypeChecker<'_> {
    // ── Statements ──────────────────────────────────────────────────

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
                        self.expect_assignable(&declared, &init_ty, "let initializer");
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
                self.expect_assignable(&expected, &actual, "return value");
            }
            Stmt::Item { .. } => {}
        }
    }

    // ── Expressions ─────────────────────────────────────────────────

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
            } => self.check_struct_expr(ctx, resolved.as_ref(), fields),
            Expr::Binary { lhs, rhs, op } => self.check_binary(ctx, *lhs, *rhs, *op, expected),
            Expr::Unary { operand, op } => self.check_unary(ctx, *operand, *op, expected),
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
                self.expect_assignable(&Type::Bool, &cond_ty, "if condition");

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
                self.join_branch_types(then_ty, else_ty, "if branches")
            }
            Expr::While { condition, body } => {
                let condition_ty = self.check_expr(ctx, *condition);
                self.expect_assignable(&Type::Bool, &condition_ty, "while condition");
                self.check_expr(ctx, *body);
                Type::Unit
            }
            Expr::Match { scrutinee, arms } => self.check_match(ctx, *scrutinee, arms, expected),
            Expr::Array { elements } => self.check_array(ctx, elements, expected),
            Expr::Call { callee, args } => self.check_call(ctx, *callee, args),
            Expr::FieldAccess { base, field } => self.check_field_access(ctx, *base, field),
        };

        self.result
            .expr_types
            .insert((ctx.body_id, expr_id), ty.clone());
        ty
    }

    // ── Binary / Unary ──────────────────────────────────────────────

    fn check_binary(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        lhs: ExprId,
        rhs: ExprId,
        op: BinaryOp,
        expected: Option<&Type>,
    ) -> Type {
        if op == BinaryOp::Assign {
            let lhs_ty = self.check_expr(ctx, lhs);
            let rhs_ty = self.check_expr_expected(ctx, rhs, &lhs_ty);
            self.expect_assignable(&lhs_ty, &rhs_ty, "assignment");
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
                self.expect_numeric(&lhs_ty, "left operand");
                self.expect_numeric(&rhs_ty, "right operand");
                if lhs_ty.is_unknown_like() || rhs_ty.is_unknown_like() {
                    Type::Unknown
                } else if let Some(result) = self.join_numeric_types(&lhs_ty, &rhs_ty) {
                    result
                } else {
                    self.diagnostic(format!(
                        "binary operands have different types: {} and {}",
                        lhs_ty.display(self.hir),
                        rhs_ty.display(self.hir)
                    ));
                    Type::Error
                }
            }
            BinaryOp::Eq | BinaryOp::Neq => {
                if self.join_numeric_types(&lhs_ty, &rhs_ty).is_none() {
                    self.expect_assignable(&lhs_ty, &rhs_ty, "comparison");
                }
                Type::Bool
            }
            BinaryOp::Lt | BinaryOp::Gt | BinaryOp::LtEq | BinaryOp::GtEq => {
                self.expect_numeric(&lhs_ty, "left operand");
                self.expect_numeric(&rhs_ty, "right operand");
                if self.join_numeric_types(&lhs_ty, &rhs_ty).is_none() {
                    self.expect_assignable(&lhs_ty, &rhs_ty, "comparison");
                }
                Type::Bool
            }
            BinaryOp::And | BinaryOp::Or => {
                self.expect_assignable(&Type::Bool, &lhs_ty, "left operand");
                self.expect_assignable(&Type::Bool, &rhs_ty, "right operand");
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
    ) -> Type {
        let operand_ty = match (op, expected) {
            (UnaryOp::Neg | UnaryOp::Pos, Some(expected)) if expected.is_numeric() => {
                self.check_expr_expected(ctx, operand, expected)
            }
            _ => self.check_expr(ctx, operand),
        };
        match op {
            UnaryOp::Neg | UnaryOp::Pos => {
                self.expect_numeric(&operand_ty, "unary operand");
                operand_ty
            }
            UnaryOp::Not => {
                self.expect_assignable(&Type::Bool, &operand_ty, "unary operand");
                Type::Bool
            }
            UnaryOp::Ref => Type::Ref(Box::new(operand_ty)),
            UnaryOp::Deref => match operand_ty {
                Type::Ref(inner) => *inner,
                Type::Unknown | Type::Error => operand_ty,
                other => {
                    self.diagnostic(format!(
                        "cannot dereference value of type {}",
                        other.display(self.hir)
                    ));
                    Type::Error
                }
            },
        }
    }

    // ── Struct / Match / Array ──────────────────────────────────────

    fn check_struct_expr(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        resolved: Option<&ResolvedName>,
        fields: &[hir::body::StructExprField],
    ) -> Type {
        let Some(ResolvedName::Struct(struct_id)) = resolved else {
            for field in fields {
                self.check_expr(ctx, field.value);
            }
            self.diagnostic("struct literal does not resolve to a struct");
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
                self.diagnostic(format!(
                    "unknown field `{}` on struct `{}`",
                    field.name.0, strukt.name.0
                ));
                continue;
            };

            seen.push(field.name.0.as_str());
            let expected = self.lower_type_ref(&expected_field.ty);
            let actual = self.check_expr_expected(ctx, field.value, &expected);
            self.expect_assignable(&expected, &actual, "struct field");
        }

        for expected in &strukt.fields {
            if !seen.contains(&expected.name.0.as_str()) {
                self.diagnostic(format!(
                    "missing field `{}` in struct literal `{}`",
                    expected.name.0, strukt.name.0
                ));
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
    ) -> Type {
        let scrutinee_ty = self.check_expr(ctx, scrutinee);
        let mut result = None;

        for arm in arms {
            ctx.push_scope();
            self.bind_pattern(ctx, arm.pat, &scrutinee_ty);
            if let Some(guard) = arm.guard {
                let guard_ty = self.check_expr(ctx, guard);
                self.expect_assignable(&Type::Bool, &guard_ty, "match guard");
            }
            let arm_ty = match expected {
                Some(expected) => self.check_expr_expected(ctx, arm.body, expected),
                None => self.check_expr(ctx, arm.body),
            };
            ctx.pop_scope();

            result = Some(match result {
                None => arm_ty,
                Some(prev) => self.join_branch_types(prev, arm_ty, "match arms"),
            });
        }

        result.unwrap_or(Type::Unit)
    }

    fn check_array(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        elements: &[ExprId],
        expected: Option<&Type>,
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
            element_ty = Some(match element_ty {
                None => ty,
                Some(prev) => {
                    self.expect_assignable(&prev, &ty, "array element");
                    prev.or(ty)
                }
            });
        }

        Type::Array(Box::new(element_ty.unwrap_or(Type::Unknown)))
    }

    // ── Call / Field ────────────────────────────────────────────────

    fn check_call(&mut self, ctx: &mut BodyCtx<'_>, callee: ExprId, args: &[ExprId]) -> Type {
        let callee_ty = self.check_expr(ctx, callee);
        let Type::Function(fid) = callee_ty else {
            for arg in args {
                self.check_expr(ctx, *arg);
            }
            if !callee_ty.is_unknown_like() {
                self.diagnostic(format!(
                    "cannot call value of type {}",
                    callee_ty.display(self.hir)
                ));
            }
            return Type::Error;
        };

        let function = &self.hir.item_tree.functions[fid];
        if args.len() != function.params.len() {
            self.diagnostic(format!(
                "function `{}` expects {} argument(s), got {}",
                function.name.0,
                function.params.len(),
                args.len()
            ));
        }

        for (index, arg) in args.iter().enumerate() {
            if let Some(param) = function.params.get(index) {
                let expected = self.lower_type_ref(&param.ty);
                let actual = self.check_expr_expected(ctx, *arg, &expected);
                self.expect_assignable(&expected, &actual, "function argument");
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

    fn check_field_access(&mut self, ctx: &mut BodyCtx<'_>, base: ExprId, field: &Name) -> Type {
        let base_ty = self.check_expr(ctx, base);
        let struct_id = match &base_ty {
            Type::Struct(id) => Some(*id),
            Type::Ref(inner) => match inner.as_ref() {
                Type::Struct(id) => Some(*id),
                _ => None,
            },
            _ => None,
        };

        let Some(struct_id) = struct_id else {
            if !base_ty.is_unknown_like() {
                self.diagnostic(format!(
                    "cannot access field `{}` on type {}",
                    field.0,
                    base_ty.display(self.hir)
                ));
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
            self.diagnostic(format!(
                "unknown field `{}` on struct `{}`",
                field.0, strukt.name.0
            ));
            return Type::Error;
        };

        field_ty
    }

    // ── Patterns ────────────────────────────────────────────────────

    fn bind_pattern(&mut self, ctx: &mut BodyCtx<'_>, pat: PatId, expected: &Type) {
        match &ctx.body.pats[pat] {
            Pattern::Wildcard | Pattern::Literal | Pattern::Path { .. } => {}
            Pattern::Binding { name } => {
                ctx.bindings.insert(name.0.clone(), expected.clone());
            }
            Pattern::Tuple { elements } => {
                let Type::Tuple(expected_elements) = expected else {
                    if !expected.is_unknown_like() {
                        self.diagnostic(format!(
                            "tuple pattern cannot match value of type {}",
                            expected.display(self.hir)
                        ));
                    }
                    for element in elements {
                        self.bind_pattern(ctx, *element, &Type::Unknown);
                    }
                    return;
                };

                if elements.len() != expected_elements.len() {
                    self.diagnostic(format!(
                        "tuple pattern expects {} element(s), got {}",
                        expected_elements.len(),
                        elements.len()
                    ));
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

    // ── Name resolution ─────────────────────────────────────────────

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
            Some(ResolvedName::Enum(_))
            | Some(ResolvedName::Trait(_))
            | Some(ResolvedName::Module(_)) => Type::Unknown,
        }
    }
}
