use std::collections::{HashMap, HashSet};

use hir::{
    HirFile,
    body::{Body, BodyId, Expr, ExprId, ResolvedName, Stmt, StmtId},
};
use type_checker::{Diagnostic, TraitEnv, Type, TypeCheckResult};

#[derive(Debug, Default)]
pub struct MoveResult {
    pub diagnostics: Vec<Diagnostic>,
}

pub fn check_moves(hir: &HirFile, type_result: &TypeCheckResult) -> MoveResult {
    let mut checker = MoveChecker {
        hir,
        type_result,
        trait_env: &type_result.trait_env,
        result: MoveResult::default(),
    };
    checker.check_all_bodies();
    checker.result
}

struct MoveChecker<'a> {
    hir: &'a HirFile,
    type_result: &'a TypeCheckResult,
    trait_env: &'a TraitEnv,
    result: MoveResult,
}

impl<'a> MoveChecker<'a> {
    fn check_all_bodies(&mut self) {
        for (fid, _) in self.hir.item_tree.functions.iter() {
            if let Some(body_id) = self.hir.function_bodies.get(&fid).copied() {
                self.check_body(body_id);
            }
        }
    }

    fn check_body(&mut self, body_id: BodyId) {
        let body = &self.hir.bodies[body_id];
        let mut ctx = BodyMoveCtx::new(body_id, body);
        self.check_expr(&mut ctx, body.root_block);
        if let Expr::Block {
            tail: Some(tail), ..
        } = &body.exprs[body.root_block]
        {
            self.consume_if_local(&mut ctx, *tail);
        }
    }

    fn check_expr(&mut self, ctx: &mut BodyMoveCtx<'_>, expr_id: ExprId) {
        match &ctx.body.exprs[expr_id] {
            Expr::Missing
            | Expr::IntLiteral { .. }
            | Expr::FloatLiteral { .. }
            | Expr::StringLiteral { .. }
            | Expr::CharLiteral { .. }
            | Expr::BoolLiteral { .. } => {}

            Expr::Path { path, resolved } => {
                if let Some(name) = path.as_single_name() {
                    if let Some(moved) = ctx.bindings.get(&name.0) {
                        if *moved {
                            self.diagnostic(format!("use of moved value: `{}`", name.0));
                        }
                        return;
                    }
                }
                if let Some(ResolvedName::Local(stmt)) = resolved {
                    if ctx.moved_locals.contains(stmt) {
                        let label = path.as_single_name().map(|n| n.0.as_str()).unwrap_or("_");
                        self.diagnostic(format!("use of moved value: `{}`", label));
                    }
                }
            }

            Expr::Struct { fields, .. } => {
                for field in fields {
                    self.check_expr(ctx, field.value);
                    self.consume_if_local(ctx, field.value);
                }
            }

            Expr::Binary { lhs, rhs, op } => {
                use hir::body::BinaryOp;
                self.check_expr(ctx, *lhs);
                self.check_expr(ctx, *rhs);
                if *op == BinaryOp::Assign {
                    self.consume_if_local(ctx, *rhs);
                }
            }

            Expr::Unary { operand, .. } => {
                self.check_expr(ctx, *operand);
            }

            Expr::Block { stmts, tail } => {
                ctx.push_scope();
                for stmt in stmts {
                    self.check_stmt(ctx, *stmt);
                }
                if let Some(tail) = tail {
                    self.check_expr(ctx, *tail);
                    self.consume_if_local(ctx, *tail);
                }
                ctx.pop_scope();
            }

            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.check_expr(ctx, *cond);
                self.check_expr(ctx, *then_branch);
                if let Some(e) = else_branch {
                    self.check_expr(ctx, *e);
                }
            }

            Expr::While { condition, body } => {
                self.check_expr(ctx, *condition);
                self.check_expr(ctx, *body);
            }

            Expr::Match { scrutinee, arms } => {
                self.check_expr(ctx, *scrutinee);
                self.consume_if_local(ctx, *scrutinee);
                for arm in arms {
                    ctx.push_scope();
                    self.bind_pattern_names(ctx, arm.pat);
                    if let Some(g) = arm.guard {
                        self.check_expr(ctx, g);
                    }
                    self.check_expr(ctx, arm.body);
                    ctx.pop_scope();
                }
            }

            Expr::Array { elements } => {
                for el in elements {
                    self.check_expr(ctx, *el);
                    self.consume_if_local(ctx, *el);
                }
            }

            Expr::Call { callee, args } => {
                self.check_expr(ctx, *callee);
                for arg in args {
                    self.check_expr(ctx, *arg);
                    self.consume_if_local(ctx, *arg);
                }
            }

            Expr::FieldAccess { base, .. } => {
                self.check_expr(ctx, *base);
            }
        }
    }

    fn check_stmt(&mut self, ctx: &mut BodyMoveCtx<'_>, stmt_id: StmtId) {
        let s = &ctx.body.stmts[stmt_id];
        match s {
            Stmt::Let { init, .. } => {
                if let Some(init) = init {
                    self.check_expr(ctx, *init);
                    self.consume_if_local(ctx, *init);
                }
            }
            Stmt::Expr { expr } => self.check_expr(ctx, *expr),
            Stmt::Return { value } => {
                if let Some(v) = value {
                    self.check_expr(ctx, *v);
                    self.consume_if_local(ctx, *v);
                }
            }
            Stmt::Item { .. } => {}
        }
    }

    fn consume_if_local(&mut self, ctx: &mut BodyMoveCtx<'_>, expr_id: ExprId) {
        let Expr::Path { path, resolved, .. } = &ctx.body.exprs[expr_id] else {
            return;
        };
        let ty = self
            .type_result
            .expr_types
            .get(&(ctx.body_id, expr_id))
            .cloned()
            .unwrap_or(Type::Unknown);
        if self.trait_env.type_is_copy(&ty) {
            return;
        }
        if let Some(name) = path.as_single_name() {
            if ctx.bindings.contains(&name.0) {
                ctx.bindings.mark_moved(&name.0);
                return;
            }
        }
        if let Some(ResolvedName::Local(stmt)) = resolved {
            ctx.moved_locals.insert(*stmt);
        }
    }

    fn bind_pattern_names(&self, ctx: &mut BodyMoveCtx<'_>, pat: hir::body::PatId) {
        match &ctx.body.pats[pat] {
            hir::body::Pattern::Binding { name } => {
                ctx.bindings.insert_available(name.0.clone());
            }
            hir::body::Pattern::Tuple { elements } => {
                for el in elements {
                    self.bind_pattern_names(ctx, *el);
                }
            }
            hir::body::Pattern::TupleStruct { elements, .. } => {
                for el in elements {
                    self.bind_pattern_names(ctx, *el);
                }
            }
            hir::body::Pattern::Struct { fields, .. } => {
                for f in fields {
                    if let Some(p) = f.pat {
                        self.bind_pattern_names(ctx, p);
                    } else {
                        ctx.bindings.insert_available(f.name.0.clone());
                    }
                }
            }
            _ => {}
        }
    }

    fn diagnostic(&mut self, message: String) {
        self.result.diagnostics.push(Diagnostic { message });
    }
}

struct BodyMoveCtx<'a> {
    body_id: BodyId,
    body: &'a Body,
    moved_locals: HashSet<StmtId>,
    bindings: MoveBindings,
}

impl<'a> BodyMoveCtx<'a> {
    fn new(body_id: BodyId, body: &'a Body) -> Self {
        Self {
            body_id,
            body,
            moved_locals: HashSet::new(),
            bindings: MoveBindings::default(),
        }
    }
    fn push_scope(&mut self) {
        self.bindings.push_scope();
    }
    fn pop_scope(&mut self) {
        self.bindings.pop_scope();
    }
}

#[derive(Debug, Default)]
struct MoveBindings {
    scopes: Vec<HashMap<String, bool>>,
}

impl MoveBindings {
    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }
    fn pop_scope(&mut self) {
        self.scopes.pop();
    }
    fn insert_available(&mut self, name: String) {
        if self.scopes.is_empty() {
            self.push_scope();
        }
        self.scopes.last_mut().unwrap().insert(name, false);
    }
    fn get(&self, name: &str) -> Option<&bool> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }
    fn contains(&self, name: &str) -> bool {
        self.scopes.iter().rev().any(|s| s.contains_key(name))
    }
    fn mark_moved(&mut self, name: &str) {
        for s in self.scopes.iter_mut().rev() {
            if let Some(m) = s.get_mut(name) {
                *m = true;
                return;
            }
        }
    }
}
