use std::collections::{HashMap, HashSet};

use hir::{
    HirFile,
    body::{BinaryOp, Body, BodyId, Expr, ExprId, ResolvedName, Stmt, StmtId, UnaryOp},
    item_tree::FunctionId,
    place::Place,
};

/// Result of escape analysis: which locals must be heap-allocated.
#[derive(Debug, Default)]
pub struct EscapeResult {
    pub escaping_locals: HashSet<(BodyId, StmtId)>,
}

impl EscapeResult {
    pub fn escapes(&self, body_id: BodyId, stmt: StmtId) -> bool {
        self.escaping_locals.contains(&(body_id, stmt))
    }
}

/// Run escape analysis on all function bodies with inter-procedural
/// refinement: a reference passed to a local function only forces heap
/// allocation when the callee's corresponding parameter actually escapes.
pub fn analyze_escapes(hir: &HirFile) -> EscapeResult {
    // Initialize: conservatively assume every param of every function escapes.
    let mut initial: HashMap<FunctionId, FnSummary> = HashMap::new();
    for (fid, func) in hir.item_tree.functions.iter() {
        let all_params: HashSet<usize> = (0..func.params.len()).collect();
        if !all_params.is_empty() {
            initial.insert(fid, all_params);
        }
    }

    let mut analyzer = EscapeAnalyzer {
        hir,
        result: EscapeResult::default(),
        fn_param_escapes: initial,
        last_ctx: None,
    };

    // Fixpoint: re-analyze until per-function param summaries stabilize.
    // In practice this converges in 2–3 iterations.
    loop {
        analyzer.result.escaping_locals.clear();
        let changed = analyzer.analyze_all_bodies();
        if !changed {
            break;
        }
    }

    analyzer.result
}

/// Per-function summary: which parameter indices escape the function body.
type FnSummary = HashSet<usize>;

struct EscapeAnalyzer<'a> {
    hir: &'a HirFile,
    result: EscapeResult,
    /// Summaries from the previous Fixpoint iteration.
    /// Initially empty (= conservative: assume all params escape).
    fn_param_escapes: HashMap<FunctionId, FnSummary>,
    /// Cache the EscapeCtx after analysis so we can read escaping_params.
    last_ctx: Option<EscapeCtx<'a>>,
}

impl<'a> EscapeAnalyzer<'a> {
    /// Run one pass over all bodies. Returns true if any function's param
    /// summary changed (meaning another Fixpoint iteration is needed).
    fn analyze_all_bodies(&mut self) -> bool {
        let mut changed = false;
        let mut new_summaries: HashMap<FunctionId, FnSummary> = HashMap::new();

        for (fid, _) in self.hir.item_tree.functions.iter() {
            if let Some(body_id) = self.hir.function_bodies.get(&fid).copied() {
                self.analyze_one_body(fid, body_id);

                // Collect which params of this function escaped during analysis.
                if let Some(ctx) = self.last_ctx.as_ref() {
                    let escaped_params: HashSet<usize> =
                        ctx.escaping_params.iter().copied().collect();

                    let prev = self.fn_param_escapes.get(&fid).cloned().unwrap_or_default();
                    if escaped_params != prev {
                        changed = true;
                    }
                    new_summaries.insert(fid, escaped_params);
                }
            }
        }

        self.fn_param_escapes = new_summaries;
        changed
    }

    fn analyze_one_body(&mut self, _fid: FunctionId, body_id: BodyId) {
        let body = &self.hir.bodies[body_id];
        let mut ctx = EscapeCtx::new(body_id, body);

        // Bottom-up mark escaping exprs
        self.mark_escaping_exprs(&mut ctx, body.root_block);

        if let Expr::Block {
            tail: Some(tail), ..
        } = &body.exprs[body.root_block]
        {
            ctx.root_tail = Some(*tail);
        }
        if let Some(tail) = ctx.root_tail {
            self.mark_escaping_exprs(&mut ctx, tail);
            if ctx.ref_to_place.contains_key(&tail)
                || ctx.expr_ref_source.contains_key(&tail)
                || ctx.ref_to_param.contains_key(&tail)
                || ctx.expr_param_source.contains_key(&tail)
            {
                ctx.escaping_exprs.insert(tail);
            }
        }
        self.propagate_escaping_to_locals(&mut ctx);

        // Record results
        for stmt in &ctx.escaping_locals {
            self.result.escaping_locals.insert((body_id, *stmt));
        }

        self.last_ctx = Some(ctx);
    }

    // ═══════════════════════════════════════════════════════════
    // Escape analysis logic
    // ═══════════════════════════════════════════════════════════

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
                tail.map_or(false, |t| ctx.escaping_exprs.contains(&t))
            }

            Expr::Unary {
                operand,
                op: UnaryOp::Ref,
            } => {
                self.record_ref(ctx, expr_id, *operand);
                false
            }

            Expr::Unary { operand, .. } => {
                self.mark_escaping_exprs(ctx, *operand);
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

            Expr::Array { elements } => {
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
                Some(ResolvedName::Local(stmt)) => ctx.escaping_locals.contains(stmt),
                Some(ResolvedName::Param(idx)) => ctx.escaping_params.contains(idx),
                _ => false,
            },

            Expr::Binary {
                lhs,
                rhs,
                op: BinaryOp::Assign,
            } => {
                self.mark_escaping_exprs(ctx, *lhs);
                self.mark_escaping_exprs(ctx, *rhs);
                ctx.escaping_exprs.contains(rhs)
            }

            Expr::Binary { lhs, rhs, .. } => {
                self.mark_escaping_exprs(ctx, *lhs);
                self.mark_escaping_exprs(ctx, *rhs);
                ctx.escaping_exprs.contains(lhs) || ctx.escaping_exprs.contains(rhs)
            }

            Expr::Call { callee, args } => {
                self.mark_escaping_exprs(ctx, *callee);
                self.handle_call_args(ctx, *callee, args);
                args.iter().any(|arg| ctx.escaping_exprs.contains(arg))
                    || args.iter().any(|arg| ctx.ref_to_place.contains_key(arg))
                    || args.iter().any(|arg| ctx.expr_ref_source.contains_key(arg))
                    || args.iter().any(|arg| ctx.ref_to_param.contains_key(arg))
                    || args
                        .iter()
                        .any(|arg| ctx.expr_param_source.contains_key(arg))
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
                let t = ctx.escaping_exprs.contains(then_branch);
                let e = else_branch.map_or(false, |eb| ctx.escaping_exprs.contains(&eb));
                t || e
            }

            Expr::While { condition, body } => {
                self.mark_escaping_exprs(ctx, *condition);
                self.mark_escaping_exprs(ctx, *body);
                ctx.escaping_exprs.contains(body)
            }

            Expr::Match { scrutinee, arms } => {
                self.mark_escaping_exprs(ctx, *scrutinee);
                for arm in arms {
                    self.mark_escaping_exprs(ctx, arm.body);
                }
                arms.iter()
                    .any(|arm| ctx.escaping_exprs.contains(&arm.body))
            }

            Expr::FieldAccess { base, .. } => {
                self.mark_escaping_exprs(ctx, *base);
                self.record_ref_chain(ctx, expr_id, *base);
                // Also trace through local binding chains.
                if let Expr::Path {
                    resolved: Some(ResolvedName::Local(stmt)),
                    ..
                } = &ctx.body.exprs[*base]
                {
                    if let Some(target) = ctx.stmt_ref_source.get(stmt).cloned() {
                        ctx.expr_ref_source.insert(expr_id, target);
                    }
                }
                ctx.escaping_exprs.contains(base)
            }

            Expr::IndexAccess { base, .. } => {
                self.mark_escaping_exprs(ctx, *base);
                self.record_ref_chain(ctx, expr_id, *base);
                if let Expr::Path {
                    resolved: Some(ResolvedName::Local(stmt)),
                    ..
                } = &ctx.body.exprs[*base]
                {
                    if let Some(target) = ctx.stmt_ref_source.get(stmt).cloned() {
                        ctx.expr_ref_source.insert(expr_id, target);
                    }
                }
                ctx.escaping_exprs.contains(base)
            }

            Expr::Missing
            | Expr::IntLiteral { .. }
            | Expr::FloatLiteral { .. }
            | Expr::StringLiteral { .. }
            | Expr::CharLiteral { .. }
            | Expr::BoolLiteral { .. } => false,

            Expr::Unsafe { body } => {
                self.mark_escaping_exprs(ctx, *body);
                ctx.escaping_exprs.contains(body)
            }

            Expr::Cast { base, .. } => {
                self.mark_escaping_exprs(ctx, *base);
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
    fn handle_call_args(&mut self, ctx: &mut EscapeCtx<'_>, callee: ExprId, args: &[ExprId]) {
        // Resolve the callee to a FunctionId.
        let callee_fid = self.resolve_callee(ctx, callee);

        for (i, arg) in args.iter().enumerate() {
            self.mark_escaping_exprs(ctx, *arg);

            // — local ref source → if callee is known and param i doesn't
            //   escape, skip marking the source local/param as escaping.
            let callee_param_escapes = callee_fid
                .and_then(|fid| self.fn_param_escapes.get(&fid))
                .map(|s| s.contains(&i))
                .unwrap_or(true); // extern / unknown → conservative

            if let Some(target) = ctx.ref_to_place.get(arg) {
                if callee_param_escapes {
                    ctx.escaping_locals.insert(target.local);
                }
            }
            if let Some(target) = ctx.expr_ref_source.get(arg) {
                if callee_param_escapes {
                    ctx.escaping_locals.insert(target.local);
                }
            }
            // — param-to-param chain: &param_i passed to callee.
            if let Some(&p_idx) = ctx.ref_to_param.get(arg) {
                if callee_param_escapes {
                    ctx.escaping_params.insert(p_idx);
                }
            }
            if let Some(&p_idx) = ctx.expr_param_source.get(arg) {
                if callee_param_escapes {
                    ctx.escaping_params.insert(p_idx);
                }
            }
        }
    }

    /// Resolve a callee expression to a `FunctionId` when it is a direct
    /// name reference to a function defined in this module.
    fn resolve_callee(&self, ctx: &EscapeCtx<'_>, callee: ExprId) -> Option<FunctionId> {
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
                    ctx.escaping_exprs.insert(*v);
                    // If the returned expression is a direct ref to a param/place,
                    // mark the source as escaping.
                    self.mark_return_sources(ctx, *v);
                }
            }
            Stmt::Item { .. } => {}
        }
    }

    /// When a ref is returned, the source local/param escapes.
    fn mark_return_sources(&self, ctx: &mut EscapeCtx<'_>, expr_id: ExprId) {
        if let Some(target) = ctx.ref_to_place.get(&expr_id) {
            ctx.escaping_locals.insert(target.local);
        }
        if let Some(target) = ctx.expr_ref_source.get(&expr_id) {
            ctx.escaping_locals.insert(target.local);
        }
        if let Some(&p_idx) = ctx.ref_to_param.get(&expr_id) {
            ctx.escaping_params.insert(p_idx);
        }
        if let Some(&p_idx) = ctx.expr_param_source.get(&expr_id) {
            ctx.escaping_params.insert(p_idx);
        }
    }

    /// Record that `ref_expr` (a `&...` expression) refers to the place/param
    /// of `operand`.
    fn record_ref(&mut self, ctx: &mut EscapeCtx<'_>, ref_expr: ExprId, operand: ExprId) {
        match &ctx.body.exprs[operand] {
            Expr::Path {
                resolved: Some(ResolvedName::Local(stmt)),
                ..
            } => {
                ctx.ref_to_place.insert(ref_expr, Place::root(*stmt));
            }
            Expr::Path {
                resolved: Some(ResolvedName::Param(idx)),
                ..
            } => {
                ctx.ref_to_param.insert(ref_expr, *idx);
            }
            Expr::FieldAccess { base, .. } => {
                if let Some(target) = ctx.ref_to_place.get(base).cloned() {
                    ctx.ref_to_place.insert(ref_expr, target);
                } else if let Some(target) = ctx.expr_ref_source.get(base).cloned() {
                    ctx.expr_ref_source.insert(ref_expr, target);
                } else if let Some(&p_idx) = ctx.ref_to_param.get(base) {
                    ctx.expr_param_source.insert(ref_expr, p_idx);
                } else if let Some(&p_idx) = ctx.expr_param_source.get(base) {
                    ctx.expr_param_source.insert(ref_expr, p_idx);
                }
            }
            Expr::IndexAccess { base, .. } => {
                if let Some(target) = ctx.ref_to_place.get(base).cloned() {
                    ctx.ref_to_place.insert(ref_expr, target);
                } else if let Some(target) = ctx.expr_ref_source.get(base).cloned() {
                    ctx.expr_ref_source.insert(ref_expr, target);
                } else if let Some(&p_idx) = ctx.ref_to_param.get(base) {
                    ctx.expr_param_source.insert(ref_expr, p_idx);
                } else if let Some(&p_idx) = ctx.expr_param_source.get(base) {
                    ctx.expr_param_source.insert(ref_expr, p_idx);
                }
            }
            _ => {}
        }
    }

    /// Propagate ref-source chains through struct/array/field expressions.
    fn record_ref_chain(&self, ctx: &mut EscapeCtx<'_>, parent: ExprId, child: ExprId) {
        // Prefer ref_to_place, then expr_ref_source, then param variants.
        if let Some(target) = ctx.ref_to_place.get(&child).cloned() {
            ctx.expr_ref_source.insert(parent, target);
        } else if let Some(target) = ctx.expr_ref_source.get(&child).cloned() {
            ctx.expr_ref_source.insert(parent, target);
        } else if let Some(&p_idx) = ctx.ref_to_param.get(&child) {
            ctx.expr_param_source.insert(parent, p_idx);
        } else if let Some(&p_idx) = ctx.expr_param_source.get(&child) {
            ctx.expr_param_source.insert(parent, p_idx);
        }
    }

    fn record_stmt_ref_chain(&mut self, ctx: &mut EscapeCtx<'_>, stmt_id: StmtId, init: ExprId) {
        if let Some(target) = ctx.ref_to_place.get(&init).cloned() {
            ctx.stmt_ref_source.insert(stmt_id, target);
            return;
        }
        if let Expr::Path {
            resolved: Some(ResolvedName::Local(source_stmt)),
            ..
        } = &ctx.body.exprs[init]
        {
            if let Some(target) = ctx.stmt_ref_source.get(source_stmt).cloned() {
                ctx.stmt_ref_source.insert(stmt_id, target);
            }
            return;
        }
        if let Some(target) = ctx.expr_ref_source.get(&init).cloned() {
            ctx.stmt_ref_source.insert(stmt_id, target);
        }
    }

    fn propagate_escaping_to_locals(&mut self, ctx: &mut EscapeCtx<'_>) {
        let mut changed = true;
        while changed {
            changed = false;

            let escaping: Vec<ExprId> = ctx.escaping_exprs.iter().copied().collect();
            for escaping_expr in escaping {
                // Local targets
                if let Some(target) = ctx.ref_to_place.get(&escaping_expr).cloned() {
                    if ctx.escaping_locals.insert(target.local) {
                        changed = true;
                    }
                }
                if let Some(target) = ctx.expr_ref_source.get(&escaping_expr).cloned() {
                    if ctx.escaping_locals.insert(target.local) {
                        changed = true;
                    }
                }
                // Param targets
                if let Some(&p_idx) = ctx.ref_to_param.get(&escaping_expr) {
                    if ctx.escaping_params.insert(p_idx) {
                        changed = true;
                    }
                }
                if let Some(&p_idx) = ctx.expr_param_source.get(&escaping_expr) {
                    if ctx.escaping_params.insert(p_idx) {
                        changed = true;
                    }
                }
                // stmt_ref_source indirection via Path(Local)
                if let Expr::Path {
                    resolved: Some(ResolvedName::Local(stmt)),
                    ..
                } = &ctx.body.exprs[escaping_expr]
                {
                    if let Some(target) = ctx.stmt_ref_source.get(stmt).cloned() {
                        if ctx.escaping_locals.insert(target.local) {
                            changed = true;
                        }
                    }
                }
            }

            // Propagate escaping locals → exprs that reference them
            let locals: Vec<StmtId> = ctx.escaping_locals.iter().copied().collect();
            for stmt in &locals {
                for (&ref_expr, target) in &ctx.ref_to_place {
                    if target.local == *stmt && ctx.escaping_exprs.insert(ref_expr) {
                        changed = true;
                    }
                }
                for (&expr, target) in &ctx.expr_ref_source {
                    if target.local == *stmt && ctx.escaping_exprs.insert(expr) {
                        changed = true;
                    }
                }
            }

            // Propagate escaping params → exprs that reference them
            let params: Vec<usize> = ctx.escaping_params.iter().copied().collect();
            for p_idx in &params {
                for (&ref_expr, &rp_idx) in &ctx.ref_to_param {
                    if rp_idx == *p_idx && ctx.escaping_exprs.insert(ref_expr) {
                        changed = true;
                    }
                }
                for (&expr, &ep_idx) in &ctx.expr_param_source {
                    if ep_idx == *p_idx && ctx.escaping_exprs.insert(expr) {
                        changed = true;
                    }
                }
            }

            // Chain through stmt_ref_source
            for stmt in &locals {
                if let Some(source) = ctx.stmt_ref_source.get(stmt).cloned() {
                    if ctx.escaping_locals.insert(source.local) {
                        changed = true;
                    }
                }
            }
            let locals2: Vec<StmtId> = ctx.escaping_locals.iter().copied().collect();
            for stmt in &locals2 {
                for (&r_stmt, source) in &ctx.stmt_ref_source {
                    if source.local == *stmt && ctx.escaping_locals.insert(r_stmt) {
                        changed = true;
                    }
                }
            }
        }
    }
}

/// Escape analysis context for a single body.
struct EscapeCtx<'a> {
    body: &'a Body,

    root_tail: Option<ExprId>,
    escaping_exprs: HashSet<ExprId>,
    escaping_locals: HashSet<StmtId>,
    escaping_params: HashSet<usize>,

    /// `&local` → Place of the local
    ref_to_place: HashMap<ExprId, Place>,
    /// Expression whose value derives from a ref to a place (chain tail)
    expr_ref_source: HashMap<ExprId, Place>,
    /// Let binding that stores a ref to a place
    stmt_ref_source: HashMap<StmtId, Place>,

    /// `&param` → param index
    ref_to_param: HashMap<ExprId, usize>,
    /// Expression whose value derives from a ref to a param (chain tail)
    expr_param_source: HashMap<ExprId, usize>,
}

impl<'a> EscapeCtx<'a> {
    fn new(body_id: BodyId, body: &'a Body) -> Self {
        let _ = body_id;
        Self {
            body,
            root_tail: None,
            escaping_exprs: HashSet::new(),
            escaping_locals: HashSet::new(),
            escaping_params: HashSet::new(),
            ref_to_place: HashMap::new(),
            expr_ref_source: HashMap::new(),
            stmt_ref_source: HashMap::new(),
            ref_to_param: HashMap::new(),
            expr_param_source: HashMap::new(),
        }
    }
}
