use std::collections::{HashMap, HashSet};

use hir::{
    HirFile,
    body::{
        BinaryOp, Body, BodyId, Expr, ExprId, PatId, Pattern, PatternBindingId, ResolvedName, Stmt,
        StmtId, UnaryOp,
    },
    item_tree::{FunctionId, HirTypeRef},
};
use type_checker::{CaptureMode, CaptureSource, OperatorCall, Type, TypeCheckResult};

/// Result of escape analysis: which locals and parameter places need stable storage.
#[derive(Debug, Default)]
pub struct EscapeResult {
    pub escaping_locals: HashSet<(BodyId, StmtId)>,
    escaping_params: HashSet<(BodyId, usize)>,
    escaping_lambda_params: HashSet<(BodyId, ExprId, usize)>,
    escaping_lambdas: HashSet<(BodyId, ExprId)>,
    address_taken_locals: HashSet<(BodyId, StmtId)>,
    address_taken_params: HashSet<(BodyId, usize)>,
    address_taken_lambda_params: HashSet<(BodyId, ExprId, usize)>,
}

impl EscapeResult {
    pub fn escapes(&self, body_id: BodyId, stmt: StmtId) -> bool {
        self.escaping_locals.contains(&(body_id, stmt))
    }

    pub fn param_escapes(&self, body_id: BodyId, index: usize) -> bool {
        self.escaping_params.contains(&(body_id, index))
    }

    pub fn lambda_param_escapes(&self, body_id: BodyId, lambda: ExprId, index: usize) -> bool {
        self.escaping_lambda_params
            .contains(&(body_id, lambda, index))
    }

    pub fn lambda_escapes(&self, body_id: BodyId, lambda: ExprId) -> bool {
        self.escaping_lambdas.contains(&(body_id, lambda))
    }

    pub fn local_needs_address(&self, body_id: BodyId, stmt: StmtId) -> bool {
        self.address_taken_locals.contains(&(body_id, stmt))
    }

    pub fn param_needs_address(&self, body_id: BodyId, index: usize) -> bool {
        self.address_taken_params.contains(&(body_id, index))
    }

    pub fn lambda_param_needs_address(
        &self,
        body_id: BodyId,
        lambda: ExprId,
        index: usize,
    ) -> bool {
        self.address_taken_lambda_params
            .contains(&(body_id, lambda, index))
    }
}

/// Run escape analysis on all function bodies with inter-procedural
/// refinement: a reference passed to a local function only forces heap
/// allocation when the callee's corresponding parameter actually escapes.
pub fn analyze_escapes(hir: &HirFile, type_result: &TypeCheckResult) -> EscapeResult {
    // Initialize: conservatively assume every param of every function escapes.
    let mut initial: HashMap<FunctionId, FnSummary> = HashMap::new();
    for (fid, func) in hir.item_tree.functions.iter() {
        let all_params: HashSet<usize> = (0..func.params.len()).collect();
        if !all_params.is_empty() {
            initial.insert(
                fid,
                FnSummary {
                    escaping: all_params,
                    returned: HashSet::new(),
                },
            );
        }
    }

    let mut analyzer = EscapeAnalyzer {
        hir,
        type_result,
        result: EscapeResult::default(),
        fn_summaries: initial,
    };

    // Fixpoint: re-analyze until per-function param summaries stabilize.
    // In practice this converges in 2–3 iterations.
    loop {
        analyzer.result.escaping_locals.clear();
        analyzer.result.escaping_params.clear();
        analyzer.result.escaping_lambda_params.clear();
        analyzer.result.escaping_lambdas.clear();
        analyzer.result.address_taken_locals.clear();
        analyzer.result.address_taken_params.clear();
        analyzer.result.address_taken_lambda_params.clear();
        let changed = analyzer.analyze_all_bodies();
        if !changed {
            break;
        }
    }

    analyzer.result
}

/// Per-function summary: parameters stored beyond the call and parameters
/// whose reference provenance flows into the return value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FnSummary {
    escaping: HashSet<usize>,
    returned: HashSet<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RefSource {
    Local(StmtId),
    LocalValue(StmtId),
    ParamPlace(usize),
    ParamValue(usize),
    LambdaParamPlace(ExprId, usize),
    LambdaParamValue(ExprId, usize),
    Lambda(ExprId),
}

type RefSources = HashSet<RefSource>;

struct EscapeAnalyzer<'a> {
    hir: &'a HirFile,
    type_result: &'a TypeCheckResult,
    result: EscapeResult,
    /// Summaries from the previous fixpoint iteration, initialized with all params.
    fn_summaries: HashMap<FunctionId, FnSummary>,
}

impl<'a> EscapeAnalyzer<'a> {
    /// Run one pass over all bodies. Returns true if any function's param
    /// summary changed (meaning another Fixpoint iteration is needed).
    fn analyze_all_bodies(&mut self) -> bool {
        let mut changed = false;
        let mut new_summaries: HashMap<FunctionId, FnSummary> = HashMap::new();

        for (fid, _) in self.hir.item_tree.functions.iter() {
            if let Some(body_id) = self.hir.function_bodies.get(&fid).copied() {
                let escaped_params = self.analyze_one_body(body_id);
                let prev = self.fn_summaries.get(&fid).cloned().unwrap_or_default();
                if escaped_params != prev {
                    changed = true;
                }
                new_summaries.insert(fid, escaped_params);
            }
        }

        self.fn_summaries = new_summaries;
        changed
    }

    fn analyze_one_body(&mut self, body_id: BodyId) -> FnSummary {
        let body = &self.hir.bodies[body_id];
        let mut ctx = EscapeCtx::new(body_id, body);

        // Bottom-up mark escaping exprs
        self.mark_escaping_exprs(&mut ctx, body.root_block);

        if let Expr::Block {
            tail: Some(tail), ..
        } = &body.exprs[body.root_block]
        {
            self.mark_returning_sources(&mut ctx, *tail);
        }
        self.propagate_escaping_to_locals(&mut ctx);

        // Record results
        for stmt in &ctx.escaping_locals {
            self.result.escaping_locals.insert((body_id, *stmt));
        }
        for index in &ctx.escaping_param_places {
            self.result.escaping_params.insert((body_id, *index));
        }
        for (lambda, index) in &ctx.escaping_lambda_param_places {
            self.result
                .escaping_lambda_params
                .insert((body_id, *lambda, *index));
        }
        for lambda in &ctx.escaping_lambdas {
            self.result.escaping_lambdas.insert((body_id, *lambda));
        }
        for stmt in &ctx.address_taken_locals {
            self.result.address_taken_locals.insert((body_id, *stmt));
        }
        for index in &ctx.address_taken_params {
            self.result.address_taken_params.insert((body_id, *index));
        }
        for (lambda, index) in &ctx.address_taken_lambda_params {
            self.result
                .address_taken_lambda_params
                .insert((body_id, *lambda, *index));
        }

        FnSummary {
            escaping: ctx.escaping_params,
            returned: ctx.returned_params,
        }
    }

    fn mark_escaping_exprs(&mut self, ctx: &mut EscapeCtx<'_>, expr_id: ExprId) -> bool {
        if ctx.escaping_exprs.contains(&expr_id) {
            return true;
        }

        let expr = &ctx.body.exprs[expr_id];
        let escapes = match expr {
            Expr::Block { stmts, tail } => {
                for stmt in stmts {
                    self.escape_check_stmt(ctx, *stmt);
                }
                if let Some(tail) = tail {
                    self.mark_escaping_exprs(ctx, *tail);
                    self.record_ref_chain(ctx, expr_id, *tail);
                    ctx.escaping_exprs.contains(tail)
                } else {
                    false
                }
            }

            Expr::Unary {
                operand,
                op: UnaryOp::Ref | UnaryOp::MutRef,
            } => {
                self.mark_escaping_exprs(ctx, *operand);
                self.record_ref(ctx, expr_id, *operand);
                false
            }

            Expr::Unary { operand, op } => {
                if let Some(fid) = self
                    .type_result
                    .operator_calls
                    .get(&(ctx.body_id, expr_id))
                    .and_then(|call| match call {
                        OperatorCall::Function(fid) => Some(*fid),
                        OperatorCall::Trait(_) => None,
                    })
                {
                    let by_ref = self.hir.item_tree.functions[fid]
                        .params
                        .first()
                        .is_some_and(|param| matches!(param.ty, HirTypeRef::Ref(..)));
                    let returned = self.handle_call_operand(ctx, Some(fid), 0, *operand, by_ref);
                    ctx.expr_sources
                        .entry(expr_id)
                        .or_default()
                        .extend(returned);
                } else {
                    self.mark_escaping_exprs(ctx, *operand);
                }
                if *op == UnaryOp::Deref && self.expr_may_carry_reference(ctx, expr_id) {
                    self.record_ref_chain(ctx, expr_id, *operand);
                }
                ctx.escaping_exprs.contains(operand)
            }

            Expr::Struct { fields, .. } => {
                for field in fields {
                    self.mark_escaping_exprs(ctx, field.value);
                }
                for field in fields {
                    self.record_ref_chain(ctx, expr_id, field.value);
                }
                fields.iter().any(|f| ctx.escaping_exprs.contains(&f.value))
            }

            Expr::Array { elements } | Expr::Tuple { elements } => {
                for el in elements {
                    self.mark_escaping_exprs(ctx, *el);
                }
                for el in elements {
                    self.record_ref_chain(ctx, expr_id, *el);
                }
                elements.iter().any(|e| ctx.escaping_exprs.contains(e))
            }

            Expr::ArrayRepeat { value, len } => {
                self.mark_escaping_exprs(ctx, *value);
                self.mark_escaping_exprs(ctx, *len);
                self.record_ref_chain(ctx, expr_id, *value);
                ctx.escaping_exprs.contains(value)
            }

            Expr::Path { resolved, .. } => match resolved {
                Some(ResolvedName::Local(stmt)) => {
                    ctx.expr_sources
                        .entry(expr_id)
                        .or_default()
                        .insert(RefSource::LocalValue(*stmt));
                    ctx.escaping_locals.contains(stmt)
                }
                Some(ResolvedName::Param(idx)) => {
                    ctx.expr_sources
                        .entry(expr_id)
                        .or_default()
                        .insert(RefSource::ParamValue(*idx));
                    ctx.escaping_params.contains(idx)
                }
                Some(ResolvedName::LambdaParam { lambda, index }) => {
                    ctx.expr_sources
                        .entry(expr_id)
                        .or_default()
                        .insert(RefSource::LambdaParamValue(*lambda, *index));
                    ctx.escaping_lambda_param_places
                        .contains(&(*lambda, *index))
                }
                Some(ResolvedName::PatternBinding(id)) => {
                    if let Some(sources) = self.pattern_sources(ctx, *id) {
                        ctx.expr_sources.entry(expr_id).or_default().extend(sources);
                    }
                    false
                }
                _ => false,
            },

            Expr::Binary {
                lhs,
                rhs,
                op: BinaryOp::Assign,
            } => {
                self.mark_escaping_exprs(ctx, *lhs);
                self.mark_escaping_exprs(ctx, *rhs);
                if let Some(sources) = ctx.expr_sources.get(rhs).cloned() {
                    if let Some(stmt) = self.direct_local_root(ctx, *lhs) {
                        ctx.stmt_sources.entry(stmt).or_default().extend(sources);
                    } else {
                        Self::mark_source_sink(ctx, &sources);
                    }
                }
                ctx.escaping_exprs.contains(rhs)
            }

            Expr::Binary { lhs, rhs, .. } => {
                if let Some(fid) = self
                    .type_result
                    .operator_calls
                    .get(&(ctx.body_id, expr_id))
                    .and_then(|call| match call {
                        OperatorCall::Function(fid) => Some(*fid),
                        OperatorCall::Trait(_) => None,
                    })
                {
                    let returned = self.handle_operator_args(ctx, fid, *lhs, *rhs);
                    ctx.expr_sources
                        .entry(expr_id)
                        .or_default()
                        .extend(returned);
                } else {
                    self.mark_escaping_exprs(ctx, *lhs);
                    self.mark_escaping_exprs(ctx, *rhs);
                }
                ctx.escaping_exprs.contains(lhs) || ctx.escaping_exprs.contains(rhs)
            }

            Expr::Call { callee, args, .. } => {
                self.mark_escaping_exprs(ctx, *callee);
                let returned = self.handle_call_args(ctx, *callee, args);
                ctx.expr_sources
                    .entry(expr_id)
                    .or_default()
                    .extend(returned);
                false
            }

            Expr::Lambda { body, .. } => {
                let mut sources: RefSources = std::iter::once(RefSource::Lambda(expr_id)).collect();
                if let Some(info) = self.type_result.lambda_infos.get(&(ctx.body_id, expr_id)) {
                    for capture in &info.captures {
                        let captured = self.capture_sources(ctx, &capture.source, capture.mode);
                        if matches!(capture.mode, CaptureMode::Shared | CaptureMode::Mutable) {
                            Self::mark_address_taken(ctx, &captured);
                        }
                        sources.extend(captured);
                    }
                }
                ctx.expr_sources.entry(expr_id).or_default().extend(sources);
                ctx.lambda_stack.push(expr_id);
                self.mark_escaping_exprs(ctx, *body);
                if let Expr::Block {
                    tail: Some(tail), ..
                } = &ctx.body.exprs[*body]
                {
                    self.record_lambda_return_sources(ctx, expr_id, *tail);
                }
                ctx.lambda_stack.pop();
                false
            }

            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.mark_escaping_exprs(ctx, *cond);
                self.mark_escaping_exprs(ctx, *then_branch);
                if let Some(eb) = else_branch {
                    self.mark_escaping_exprs(ctx, *eb);
                }
                self.record_ref_chain(ctx, expr_id, *then_branch);
                if let Some(else_branch) = else_branch {
                    self.record_ref_chain(ctx, expr_id, *else_branch);
                }
                let t = ctx.escaping_exprs.contains(then_branch);
                let e = else_branch.is_some_and(|eb| ctx.escaping_exprs.contains(&eb));
                t || e
            }

            Expr::While { condition, body } => {
                self.mark_escaping_exprs(ctx, *condition);
                self.mark_escaping_exprs(ctx, *body);
                ctx.escaping_exprs.contains(body)
            }

            Expr::For {
                pat,
                iterable,
                body,
            } => {
                self.mark_escaping_exprs(ctx, *iterable);
                let sources = ctx.expr_sources.get(iterable).cloned().unwrap_or_default();
                self.push_pattern_sources(ctx, *pat, &sources);
                self.mark_escaping_exprs(ctx, *body);
                ctx.pattern_sources.pop();
                ctx.escaping_exprs.contains(body)
            }

            Expr::Match { scrutinee, arms } => {
                self.mark_escaping_exprs(ctx, *scrutinee);
                let sources = ctx.expr_sources.get(scrutinee).cloned().unwrap_or_default();
                for arm in arms {
                    self.push_pattern_sources(ctx, arm.pat, &sources);
                    if let Some(guard) = arm.guard {
                        self.mark_escaping_exprs(ctx, guard);
                    }
                    self.mark_escaping_exprs(ctx, arm.body);
                    self.record_ref_chain(ctx, expr_id, arm.body);
                    ctx.pattern_sources.pop();
                }
                arms.iter()
                    .any(|arm| ctx.escaping_exprs.contains(&arm.body))
            }

            Expr::FieldAccess { base, .. } => {
                self.mark_escaping_exprs(ctx, *base);
                if self.expr_may_carry_reference(ctx, expr_id) {
                    self.record_ref_chain(ctx, expr_id, *base);
                }
                ctx.escaping_exprs.contains(base)
            }

            Expr::IndexAccess { base, index } => {
                self.mark_escaping_exprs(ctx, *base);
                self.mark_escaping_exprs(ctx, *index);
                if self.expr_may_carry_reference(ctx, expr_id) {
                    self.record_ref_chain(ctx, expr_id, *base);
                }
                ctx.escaping_exprs.contains(base) || ctx.escaping_exprs.contains(index)
            }

            Expr::Missing
            | Expr::IntLiteral { .. }
            | Expr::FloatLiteral { .. }
            | Expr::StringLiteral { .. }
            | Expr::CharLiteral { .. }
            | Expr::BoolLiteral { .. } => false,

            Expr::Unsafe { body } => {
                self.mark_escaping_exprs(ctx, *body);
                self.record_ref_chain(ctx, expr_id, *body);
                ctx.escaping_exprs.contains(body)
            }

            Expr::Cast { base, .. } => {
                self.mark_escaping_exprs(ctx, *base);
                if self.expr_may_carry_reference(ctx, expr_id) {
                    self.record_ref_chain(ctx, expr_id, *base);
                }
                ctx.escaping_exprs.contains(base)
            }
        };

        if escapes {
            ctx.escaping_exprs.insert(expr_id);
        }
        escapes
    }

    /// Handle call arguments for escape: a ref passed to a local function
    /// only forces heap allocation when the callee's param actually escapes.
    fn handle_call_args(
        &mut self,
        ctx: &mut EscapeCtx<'_>,
        callee: ExprId,
        args: &[ExprId],
    ) -> RefSources {
        let callee_fid = self.resolve_callee(ctx, callee);
        let mut returned = RefSources::new();
        let mut pending = ctx
            .expr_sources
            .get(&callee)
            .into_iter()
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        let mut seen = RefSources::new();
        while let Some(source) = pending.pop() {
            if !seen.insert(source) {
                continue;
            }
            match source {
                RefSource::Lambda(lambda) => returned.extend(
                    ctx.lambda_return_sources
                        .get(&lambda)
                        .into_iter()
                        .flatten()
                        .copied(),
                ),
                RefSource::LocalValue(stmt) => {
                    pending.extend(ctx.stmt_sources.get(&stmt).into_iter().flatten().copied())
                }
                _ => {}
            }
        }
        let receiver = callee_fid.and_then(|fid| {
            let Expr::FieldAccess { base, .. } = &ctx.body.exprs[callee] else {
                return None;
            };
            let param = self.hir.item_tree.functions[fid].params.first()?;
            Some((*base, matches!(param.ty, HirTypeRef::Ref(..))))
        });

        if let Some((receiver, by_ref)) = receiver {
            returned.extend(self.handle_call_operand(ctx, callee_fid, 0, receiver, by_ref));
        }

        let param_offset = usize::from(receiver.is_some());

        for (i, arg) in args.iter().enumerate() {
            returned.extend(self.handle_call_operand(
                ctx,
                callee_fid,
                i + param_offset,
                *arg,
                false,
            ));
        }
        returned
    }

    fn handle_operator_args(
        &mut self,
        ctx: &mut EscapeCtx<'_>,
        fid: FunctionId,
        lhs: ExprId,
        rhs: ExprId,
    ) -> RefSources {
        let receiver_by_ref = self.hir.item_tree.functions[fid]
            .params
            .first()
            .is_some_and(|param| matches!(param.ty, HirTypeRef::Ref(..)));
        let mut returned = self.handle_call_operand(ctx, Some(fid), 0, lhs, receiver_by_ref);
        returned.extend(self.handle_call_operand(ctx, Some(fid), 1, rhs, false));
        returned
    }

    fn handle_call_operand(
        &mut self,
        ctx: &mut EscapeCtx<'_>,
        callee_fid: Option<FunctionId>,
        param_index: usize,
        operand: ExprId,
        auto_borrow: bool,
    ) -> RefSources {
        self.mark_escaping_exprs(ctx, operand);
        let sources = if auto_borrow
            && !self
                .type_result
                .expr_types
                .get(&(ctx.body_id, operand))
                .is_some_and(|ty| matches!(ty, Type::Ref(..)))
        {
            self.place_sources(ctx, operand)
        } else {
            ctx.expr_sources.get(&operand).cloned().unwrap_or_default()
        };
        let Some(summary) = callee_fid.and_then(|fid| self.fn_summaries.get(&fid)) else {
            Self::mark_source_sink(ctx, &sources);
            return RefSources::new();
        };
        if summary.escaping.contains(&param_index) {
            Self::mark_source_sink(ctx, &sources);
        }
        if summary.returned.contains(&param_index) {
            sources
        } else {
            RefSources::new()
        }
    }

    fn resolve_callee(&self, ctx: &EscapeCtx<'_>, callee: ExprId) -> Option<FunctionId> {
        if let Some(Type::Function(fid)) = self.type_result.expr_types.get(&(ctx.body_id, callee)) {
            return Some(*fid);
        }
        match &ctx.body.exprs[callee] {
            Expr::Path {
                resolved: Some(ResolvedName::Function(fid)),
                ..
            } => Some(*fid),
            _ => None,
        }
    }

    fn escape_check_stmt(&mut self, ctx: &mut EscapeCtx<'_>, stmt_id: StmtId) {
        let s = &ctx.body.stmts[stmt_id];
        match s {
            Stmt::Let { init, .. } => {
                if let Some(lambda) = ctx.lambda_stack.last().copied() {
                    ctx.lambda_locals.entry(lambda).or_default().insert(stmt_id);
                }
                if let Some(init) = init {
                    self.mark_escaping_exprs(ctx, *init);
                    self.record_stmt_ref_chain(ctx, stmt_id, *init);
                }
            }
            Stmt::Expr { expr } => {
                self.mark_escaping_exprs(ctx, *expr);
            }
            Stmt::Return { value } => {
                if let Some(v) = value {
                    self.mark_escaping_exprs(ctx, *v);
                    if let Some(lambda) = ctx.lambda_stack.last().copied() {
                        self.record_lambda_return_sources(ctx, lambda, *v);
                    } else {
                        self.mark_returning_sources(ctx, *v);
                    }
                }
            }
            Stmt::Break | Stmt::Continue => {}
            Stmt::Item { .. } => {}
        }
    }

    fn expr_may_carry_reference(&self, ctx: &EscapeCtx<'_>, expr_id: ExprId) -> bool {
        self.type_result
            .expr_types
            .get(&(ctx.body_id, expr_id))
            .is_none_or(type_may_carry_reference)
    }

    fn mark_returning_sources(&self, ctx: &mut EscapeCtx<'_>, expr_id: ExprId) {
        let Some(sources) = ctx.expr_sources.get(&expr_id).cloned() else {
            return;
        };
        let mut pending: Vec<RefSource> = sources.into_iter().collect();
        let mut seen = HashSet::new();
        while let Some(source) = pending.pop() {
            if !seen.insert(source) {
                continue;
            }
            match source {
                RefSource::Local(stmt) => {
                    ctx.escaping_locals.insert(stmt);
                }
                RefSource::LocalValue(stmt) => {
                    if let Some(nested) = ctx.stmt_sources.get(&stmt) {
                        pending.extend(nested.iter().copied());
                    }
                }
                RefSource::ParamPlace(index) => {
                    ctx.escaping_param_places.insert(index);
                    ctx.returned_params.insert(index);
                }
                RefSource::ParamValue(index) => {
                    ctx.returned_params.insert(index);
                }
                RefSource::LambdaParamPlace(lambda, index) => {
                    ctx.escaping_lambda_param_places.insert((lambda, index));
                }
                RefSource::LambdaParamValue(..) => {}
                RefSource::Lambda(lambda) => {
                    ctx.escaping_lambdas.insert(lambda);
                }
            }
        }
    }

    fn record_lambda_return_sources(
        &self,
        ctx: &mut EscapeCtx<'_>,
        lambda: ExprId,
        expr_id: ExprId,
    ) {
        let Some(sources) = ctx.expr_sources.get(&expr_id).cloned() else {
            return;
        };
        let locals = ctx.lambda_locals.get(&lambda).cloned().unwrap_or_default();
        let mut returned = RefSources::new();
        let mut pending: Vec<RefSource> = sources.into_iter().collect();
        let mut seen = HashSet::new();
        while let Some(source) = pending.pop() {
            if !seen.insert(source) {
                continue;
            }
            match source {
                RefSource::Local(stmt) if locals.contains(&stmt) => {
                    ctx.escaping_locals.insert(stmt);
                }
                RefSource::LocalValue(stmt) if locals.contains(&stmt) => {
                    if let Some(nested) = ctx.stmt_sources.get(&stmt) {
                        pending.extend(nested.iter().copied());
                    }
                }
                RefSource::LambdaParamPlace(owner, index) if owner == lambda => {
                    ctx.escaping_lambda_param_places.insert((owner, index));
                }
                RefSource::LambdaParamValue(owner, _) if owner == lambda => {}
                RefSource::Lambda(inner) => {
                    ctx.escaping_lambdas.insert(inner);
                    returned.insert(source);
                }
                source => {
                    returned.insert(source);
                }
            }
        }
        ctx.lambda_return_sources
            .entry(lambda)
            .or_default()
            .extend(returned);
    }

    fn mark_source_sink(ctx: &mut EscapeCtx<'_>, sources: &RefSources) -> bool {
        ctx.escaping_sources.extend(sources.iter().copied());
        Self::mark_sources(ctx, sources)
    }

    fn mark_sources(ctx: &mut EscapeCtx<'_>, sources: &RefSources) -> bool {
        let mut changed = false;
        let mut pending: Vec<RefSource> = sources.iter().copied().collect();
        let mut seen = HashSet::new();

        while let Some(source) = pending.pop() {
            if !seen.insert(source) {
                continue;
            }
            match source {
                RefSource::Local(stmt) => {
                    changed |= ctx.escaping_locals.insert(stmt);
                }
                RefSource::LocalValue(stmt) => {
                    if let Some(nested) = ctx.stmt_sources.get(&stmt) {
                        pending.extend(nested.iter().copied());
                    }
                }
                RefSource::ParamPlace(index) => {
                    changed |= ctx.escaping_param_places.insert(index);
                    changed |= ctx.escaping_params.insert(index);
                }
                RefSource::ParamValue(index) => {
                    changed |= ctx.escaping_params.insert(index);
                }
                RefSource::LambdaParamPlace(lambda, index) => {
                    changed |= ctx.escaping_lambda_param_places.insert((lambda, index));
                }
                RefSource::LambdaParamValue(..) => {}
                RefSource::Lambda(lambda) => {
                    changed |= ctx.escaping_lambdas.insert(lambda);
                }
            }
        }
        changed
    }

    fn capture_sources(
        &self,
        ctx: &EscapeCtx<'_>,
        source: &CaptureSource,
        mode: CaptureMode,
    ) -> RefSources {
        let by_ref = matches!(mode, CaptureMode::Shared | CaptureMode::Mutable);
        match source {
            CaptureSource::Local(stmt) if by_ref => {
                [RefSource::Local(*stmt), RefSource::LocalValue(*stmt)]
                    .into_iter()
                    .collect()
            }
            CaptureSource::Local(stmt) => std::iter::once(RefSource::LocalValue(*stmt)).collect(),
            CaptureSource::Pattern(id) => self.pattern_sources(ctx, *id).unwrap_or_default(),
            CaptureSource::Param(index) if by_ref => {
                [RefSource::ParamPlace(*index), RefSource::ParamValue(*index)]
                    .into_iter()
                    .collect()
            }
            CaptureSource::Param(index) => std::iter::once(RefSource::ParamValue(*index)).collect(),
            CaptureSource::LambdaParam { lambda, index } if by_ref => [
                RefSource::LambdaParamPlace(*lambda, *index),
                RefSource::LambdaParamValue(*lambda, *index),
            ]
            .into_iter()
            .collect(),
            CaptureSource::LambdaParam { lambda, index } => {
                std::iter::once(RefSource::LambdaParamValue(*lambda, *index)).collect()
            }
        }
    }

    fn mark_address_taken(ctx: &mut EscapeCtx<'_>, sources: &RefSources) {
        for source in sources {
            match source {
                RefSource::Local(stmt) => {
                    ctx.address_taken_locals.insert(*stmt);
                }
                RefSource::ParamPlace(index) => {
                    ctx.address_taken_params.insert(*index);
                }
                RefSource::LambdaParamPlace(lambda, index) => {
                    ctx.address_taken_lambda_params.insert((*lambda, *index));
                }
                RefSource::LocalValue(_)
                | RefSource::ParamValue(_)
                | RefSource::LambdaParamValue(..)
                | RefSource::Lambda(_) => {}
            }
        }
    }

    /// Record that `ref_expr` (a `&...` expression) refers to the place/param
    /// of `operand`.
    fn record_ref(&self, ctx: &mut EscapeCtx<'_>, ref_expr: ExprId, operand: ExprId) {
        let sources = self.place_sources(ctx, operand);
        if !sources.is_empty() {
            ctx.expr_sources
                .entry(ref_expr)
                .or_default()
                .extend(sources);
        }
    }

    fn push_pattern_sources(&self, ctx: &mut EscapeCtx<'_>, pat: PatId, sources: &RefSources) {
        let mut bindings = HashSet::new();
        Self::collect_pattern_bindings(ctx.body, pat, &mut bindings);
        ctx.pattern_sources.push(
            bindings
                .into_iter()
                .map(|id| (id, sources.clone()))
                .collect(),
        );
    }

    fn collect_pattern_bindings(body: &Body, pat: PatId, bindings: &mut HashSet<PatternBindingId>) {
        match &body.pats[pat] {
            Pattern::Binding { .. } => {
                bindings.insert(PatternBindingId {
                    pattern: pat,
                    field: None,
                });
            }
            Pattern::Tuple { elements } | Pattern::TupleStruct { elements, .. } => {
                for element in elements {
                    Self::collect_pattern_bindings(body, *element, bindings);
                }
            }
            Pattern::Struct { fields, .. } => {
                for (index, field) in fields.iter().enumerate() {
                    if let Some(field_pat) = field.pat {
                        Self::collect_pattern_bindings(body, field_pat, bindings);
                    } else {
                        bindings.insert(PatternBindingId {
                            pattern: pat,
                            field: Some(index),
                        });
                    }
                }
            }
            Pattern::Wildcard | Pattern::Literal(_) | Pattern::Path { .. } => {}
        }
    }

    fn pattern_sources(&self, ctx: &EscapeCtx<'_>, id: PatternBindingId) -> Option<RefSources> {
        ctx.pattern_sources
            .iter()
            .rev()
            .find_map(|scope| scope.get(&id).cloned())
    }

    fn place_sources(&self, ctx: &EscapeCtx<'_>, expr_id: ExprId) -> RefSources {
        match &ctx.body.exprs[expr_id] {
            Expr::Path {
                resolved: Some(ResolvedName::Local(stmt)),
                ..
            } => [RefSource::Local(*stmt), RefSource::LocalValue(*stmt)]
                .into_iter()
                .collect(),
            Expr::Path {
                resolved: Some(ResolvedName::Param(index)),
                ..
            } => [RefSource::ParamPlace(*index), RefSource::ParamValue(*index)]
                .into_iter()
                .collect(),
            Expr::Path {
                resolved: Some(ResolvedName::LambdaParam { lambda, index }),
                ..
            } => [
                RefSource::LambdaParamPlace(*lambda, *index),
                RefSource::LambdaParamValue(*lambda, *index),
            ]
            .into_iter()
            .collect(),
            Expr::Path {
                resolved: Some(ResolvedName::PatternBinding(id)),
                ..
            } => self.pattern_sources(ctx, *id).unwrap_or_default(),
            Expr::FieldAccess { base, .. } | Expr::IndexAccess { base, .. } => {
                let indirect = self
                    .type_result
                    .expr_types
                    .get(&(ctx.body_id, *base))
                    .is_some_and(|ty| matches!(ty, Type::Ref(..) | Type::Ptr { .. }));
                if indirect {
                    ctx.expr_sources.get(base).cloned().unwrap_or_default()
                } else {
                    self.place_sources(ctx, *base)
                }
            }
            Expr::Unary {
                operand,
                op: UnaryOp::Deref,
            } => ctx
                .expr_sources
                .get(operand)
                .cloned()
                .unwrap_or_else(|| self.place_sources(ctx, *operand)),
            Expr::Block {
                tail: Some(tail), ..
            } => ctx.expr_sources.get(tail).cloned().unwrap_or_default(),
            _ => ctx.expr_sources.get(&expr_id).cloned().unwrap_or_default(),
        }
    }

    fn direct_local_root(&self, ctx: &EscapeCtx<'_>, expr_id: ExprId) -> Option<StmtId> {
        match &ctx.body.exprs[expr_id] {
            Expr::Path {
                resolved: Some(ResolvedName::Local(stmt)),
                ..
            } => Some(*stmt),
            Expr::FieldAccess { base, .. } | Expr::IndexAccess { base, .. } => {
                let indirect = self
                    .type_result
                    .expr_types
                    .get(&(ctx.body_id, *base))
                    .is_some_and(|ty| matches!(ty, Type::Ref(..) | Type::Ptr { .. }));
                (!indirect)
                    .then(|| self.direct_local_root(ctx, *base))
                    .flatten()
            }
            _ => None,
        }
    }

    /// Propagate ref-source chains through struct/array/field expressions.
    fn record_ref_chain(&self, ctx: &mut EscapeCtx<'_>, parent: ExprId, child: ExprId) {
        if let Some(sources) = ctx.expr_sources.get(&child).cloned() {
            ctx.expr_sources.entry(parent).or_default().extend(sources);
        }
    }

    fn record_stmt_ref_chain(&self, ctx: &mut EscapeCtx<'_>, stmt_id: StmtId, init: ExprId) {
        if let Some(sources) = ctx.expr_sources.get(&init).cloned() {
            ctx.stmt_sources.entry(stmt_id).or_default().extend(sources);
        }
    }

    fn propagate_escaping_to_locals(&self, ctx: &mut EscapeCtx<'_>) {
        let mut changed = true;
        while changed {
            changed = false;

            let sources = ctx.escaping_sources.clone();
            changed |= Self::mark_sources(ctx, &sources);

            let escaping: Vec<ExprId> = ctx.escaping_exprs.iter().copied().collect();
            for escaping_expr in escaping {
                if let Some(sources) = ctx.expr_sources.get(&escaping_expr).cloned() {
                    changed |= Self::mark_sources(ctx, &sources);
                }
            }

            let locals: Vec<StmtId> = ctx.escaping_locals.iter().copied().collect();
            for stmt in &locals {
                if let Some(sources) = ctx.stmt_sources.get(stmt).cloned() {
                    changed |= Self::mark_sources(ctx, &sources);
                }
            }
        }
    }
}

fn type_may_carry_reference(ty: &Type) -> bool {
    match ty {
        Type::Ref(..) | Type::Ptr { .. } => true,
        Type::Tuple(elements) => elements.iter().any(type_may_carry_reference),
        Type::Array(inner, _) => type_may_carry_reference(inner),
        Type::Struct(..)
        | Type::Enum(..)
        | Type::Param(..)
        | Type::InferVar(..)
        | Type::Unknown
        | Type::Error => true,
        Type::Fn { params, ret, .. } => {
            params.iter().any(type_may_carry_reference) || type_may_carry_reference(ret)
        }
        Type::Function(..) => true,
        Type::Int(..)
        | Type::Float(..)
        | Type::InferInt
        | Type::InferFloat
        | Type::Bool
        | Type::Str
        | Type::Char
        | Type::Unit
        | Type::Never
        | Type::Const(..) => false,
    }
}

/// Escape analysis context for a single body.
struct EscapeCtx<'a> {
    body_id: BodyId,
    body: &'a Body,

    escaping_exprs: HashSet<ExprId>,
    escaping_locals: HashSet<StmtId>,
    escaping_params: HashSet<usize>,
    returned_params: HashSet<usize>,
    escaping_param_places: HashSet<usize>,
    escaping_lambda_param_places: HashSet<(ExprId, usize)>,
    escaping_lambdas: HashSet<ExprId>,
    address_taken_locals: HashSet<StmtId>,
    address_taken_params: HashSet<usize>,
    address_taken_lambda_params: HashSet<(ExprId, usize)>,
    lambda_stack: Vec<ExprId>,
    lambda_locals: HashMap<ExprId, HashSet<StmtId>>,
    lambda_return_sources: HashMap<ExprId, RefSources>,
    escaping_sources: RefSources,
    expr_sources: HashMap<ExprId, RefSources>,
    stmt_sources: HashMap<StmtId, RefSources>,
    pattern_sources: Vec<HashMap<PatternBindingId, RefSources>>,
}

impl<'a> EscapeCtx<'a> {
    fn new(body_id: BodyId, body: &'a Body) -> Self {
        Self {
            body_id,
            body,
            escaping_exprs: HashSet::new(),
            escaping_locals: HashSet::new(),
            escaping_params: HashSet::new(),
            returned_params: HashSet::new(),
            escaping_param_places: HashSet::new(),
            escaping_lambda_param_places: HashSet::new(),
            escaping_lambdas: HashSet::new(),
            address_taken_locals: HashSet::new(),
            address_taken_params: HashSet::new(),
            address_taken_lambda_params: HashSet::new(),
            lambda_stack: Vec::new(),
            lambda_locals: HashMap::new(),
            lambda_return_sources: HashMap::new(),
            escaping_sources: RefSources::new(),
            expr_sources: HashMap::new(),
            stmt_sources: HashMap::new(),
            pattern_sources: Vec::new(),
        }
    }
}
