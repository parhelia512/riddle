use std::collections::{HashMap, HashSet};

use rowan::TextRange;

use hir::{
    HirFile,
    body::{
        Body, BodyId, Expr, ExprId, PatternBindingId, ResolvedName, SourceMap, Stmt, StmtId,
        UnaryOp,
    },
    item_tree::{FunctionId, HirTypeRef},
    place::Place,
};
use type_checker::{
    CaptureMode, CaptureSource, ClosureKind, Diagnostic, LabelStyle, LambdaInfo, Severity,
    SourceLabel, TraitEnv, Type, TypeCheckResult,
};

mod reference_flow;

use reference_flow::{FlowKind, ReferenceFlow, type_may_carry_reference};

type LoanId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BorrowKind {
    Shared,
    Mutable,
}

impl BorrowKind {
    fn from_flow(kind: FlowKind, inherited: Self) -> Self {
        match kind {
            FlowKind::Inherit => inherited,
            FlowKind::Shared => Self::Shared,
            FlowKind::Mutable => Self::Mutable,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum AccessRoot {
    Local(StmtId),
    Pattern(PatternBindingId),
    Param(usize),
    LambdaParam { lambda: ExprId, index: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum AccessProjection {
    Field(usize),
    Index(Option<usize>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AccessPlace {
    root: AccessRoot,
    projections: Vec<AccessProjection>,
}

impl AccessPlace {
    fn new(root: AccessRoot) -> Self {
        Self {
            root,
            projections: Vec::new(),
        }
    }

    fn field(mut self, index: usize) -> Self {
        self.projections.push(AccessProjection::Field(index));
        self
    }

    fn index(mut self, index: Option<usize>) -> Self {
        self.projections.push(AccessProjection::Index(index));
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Origin {
    place: AccessPlace,
    kind: BorrowKind,
    loan: LoanId,
}

type Origins = HashSet<Origin>;

#[derive(Debug, Clone)]
struct AccessTarget {
    place: AccessPlace,
    parents: HashSet<LoanId>,
}

#[derive(Debug, Default)]
pub struct AnalysisResult {
    pub diagnostics: Vec<Diagnostic>,
}

/// Run move/borrow checking. Escape analysis identifies storage duration only;
/// heap allocation does not relax move or borrow rules.
pub fn analyze(hir: &HirFile, type_result: &TypeCheckResult) -> AnalysisResult {
    let reference_flow = ReferenceFlow::build(hir, type_result);
    let mut a = Analyzer {
        hir,
        type_result,
        trait_env: &type_result.trait_env,
        reference_flow: &reference_flow,
        result: AnalysisResult::default(),
    };
    a.analyze_all_bodies();
    a.result
}

struct Analyzer<'a> {
    hir: &'a HirFile,
    type_result: &'a TypeCheckResult,
    trait_env: &'a TraitEnv,
    reference_flow: &'a ReferenceFlow,
    result: AnalysisResult,
}

impl<'a> Analyzer<'a> {
    fn analyze_all_bodies(&mut self) {
        for (fid, _) in self.hir.item_tree.functions.iter() {
            if let Some(body_id) = self.hir.function_bodies.get(&fid).copied() {
                self.analyze_body(fid, body_id);
            }
        }
    }

    fn analyze_body(&mut self, function_id: FunctionId, body_id: BodyId) {
        let body = &self.hir.bodies[body_id];
        let mut ctx = BodyCtx::new(body_id, body);
        ctx.seed_params(
            self.hir.item_tree.functions[function_id]
                .params
                .iter()
                .map(|param| param.name.0.as_str()),
        );
        ctx.seed_reference_params(
            self.hir.item_tree.functions[function_id]
                .params
                .iter()
                .enumerate(),
        );
        self.move_check_body(&mut ctx);
    }

    // ═══════════════════════════════════════════════════════════
    // Move checking
    // ═══════════════════════════════════════════════════════════

    fn move_check_body(&mut self, ctx: &mut BodyCtx<'_>) {
        self.move_check_expr(ctx, ctx.body.root_block);
        if let Expr::Block {
            tail: Some(tail), ..
        } = &ctx.body.exprs[ctx.body.root_block]
        {
            self.consume_if_local(ctx, *tail);
        }
    }

    fn move_check_expr(&mut self, ctx: &mut BodyCtx<'_>, expr_id: ExprId) {
        let span = ctx.expr_range(expr_id);
        match &ctx.body.exprs[expr_id] {
            Expr::Missing
            | Expr::IntLiteral { .. }
            | Expr::FloatLiteral { .. }
            | Expr::StringLiteral { .. }
            | Expr::CharLiteral { .. }
            | Expr::BoolLiteral { .. } => {}

            Expr::Path { path, resolved } => {
                let origins = match resolved {
                    Some(ResolvedName::Local(stmt)) => {
                        ctx.local_origins.get(stmt).cloned().unwrap_or_default()
                    }
                    Some(ResolvedName::Param(index)) => {
                        ctx.param_origins.get(index).cloned().unwrap_or_default()
                    }
                    _ => Origins::new(),
                };
                ctx.expr_origins.insert(expr_id, origins);
                if let Some(name) = path.as_single_name()
                    && let Some(moved) = ctx.bindings.get(&name.0)
                {
                    if *moved {
                        let extra = resolved
                            .as_ref()
                            .and_then(|resolved| {
                                if let ResolvedName::Local(stmt) = resolved {
                                    let p = Place::root(*stmt);
                                    Some(self.move_site_labels(ctx, &p))
                                } else {
                                    None
                                }
                            })
                            .unwrap_or_default();
                        self.diag_with_labels(
                            format!("use of moved value: `{}`", name.0),
                            span,
                            "E0100",
                            &extra,
                        );
                    }
                    if let Some(ResolvedName::Local(stmt)) = resolved {
                        ctx.release_local_if_dead(*stmt);
                    }
                    return;
                }
                if let Some(ResolvedName::Local(stmt)) = resolved {
                    let place = Place::root(*stmt);
                    if ctx.moved_places.iter().any(|m| place_overlaps(m, &place)) {
                        let label = path.as_single_name().map(|n| n.0.as_str()).unwrap_or("_");
                        let extra = self.move_site_labels(ctx, &place);
                        self.diag_with_labels(
                            format!("use of moved value: `{}`", label),
                            span,
                            "E0100",
                            &extra,
                        );
                    }
                }
                if let Some(ResolvedName::Local(stmt)) = resolved {
                    ctx.release_local_if_dead(*stmt);
                }
            }

            Expr::Struct { fields, .. } => {
                let mut origins = Origins::new();
                for field in fields {
                    self.move_check_expr(ctx, field.value);
                    self.consume_if_local(ctx, field.value);
                    origins.extend(
                        ctx.expr_origins
                            .get(&field.value)
                            .cloned()
                            .unwrap_or_default(),
                    );
                }
                let origins = if self.expr_may_carry_reference(ctx, expr_id) {
                    origins
                } else {
                    for field in fields {
                        self.deactivate_unretained(ctx, field.value, &HashSet::new());
                    }
                    Origins::new()
                };
                ctx.expr_origins.insert(expr_id, origins);
            }

            Expr::Binary { lhs, rhs, op } => {
                self.move_check_expr(ctx, *lhs);
                self.move_check_expr(ctx, *rhs);
                if op.is_assignment() {
                    if let Some(lhs_place) = self.place_from_expr(ctx, *lhs)
                        && self.has_any_borrow(ctx, &lhs_place)
                    {
                        let name = self.expr_name(ctx, *lhs);
                        self.diag(
                            format!("cannot assign to `{}` while borrowed", name),
                            span,
                            "E0303",
                        );
                    }
                    if let Some(stmt) = self.direct_local_root(ctx, *lhs) {
                        ctx.bind_origins(
                            stmt,
                            ctx.expr_origins.get(rhs).cloned().unwrap_or_default(),
                        );
                    }
                    self.consume_if_local(ctx, *rhs);
                }
                let origins = if op.is_assignment() {
                    ctx.expr_origins.get(rhs).cloned().unwrap_or_default()
                } else {
                    self.deactivate_unretained(ctx, *lhs, &HashSet::new());
                    self.deactivate_unretained(ctx, *rhs, &HashSet::new());
                    Origins::new()
                };
                ctx.expr_origins.insert(expr_id, origins);
            }

            Expr::Unary { operand, op } => {
                self.move_check_expr(ctx, *operand);
                let origins = match op {
                    UnaryOp::Ref => self.create_borrow(ctx, *operand, BorrowKind::Shared, span),
                    UnaryOp::MutRef => self.create_borrow(ctx, *operand, BorrowKind::Mutable, span),
                    UnaryOp::Deref => ctx.expr_origins.get(operand).cloned().unwrap_or_default(),
                    _ => Origins::new(),
                };
                let origins = if self.expr_may_carry_reference(ctx, expr_id) {
                    origins
                } else {
                    self.deactivate_unretained(ctx, *operand, &HashSet::new());
                    Origins::new()
                };
                ctx.expr_origins.insert(expr_id, origins);
            }

            Expr::Block { stmts, tail } => {
                ctx.push_scope();
                for stmt in stmts {
                    self.move_check_stmt(ctx, *stmt);
                }
                if let Some(tail) = tail {
                    self.move_check_expr(ctx, *tail);
                    ctx.expr_origins.insert(
                        expr_id,
                        ctx.expr_origins.get(tail).cloned().unwrap_or_default(),
                    );
                    self.consume_if_local(ctx, *tail);
                }
                ctx.pop_scope();
            }

            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.move_check_expr(ctx, *cond);
                self.move_check_expr(ctx, *then_branch);
                if let Some(e) = else_branch {
                    self.move_check_expr(ctx, *e);
                }
                let mut origins = ctx
                    .expr_origins
                    .get(then_branch)
                    .cloned()
                    .unwrap_or_default();
                if let Some(e) = else_branch {
                    origins.extend(ctx.expr_origins.get(e).cloned().unwrap_or_default());
                }
                ctx.expr_origins.insert(expr_id, origins);
            }

            Expr::While { condition, body } => {
                self.move_check_expr(ctx, *condition);
                self.move_check_expr(ctx, *body);
            }

            Expr::For {
                pat,
                iterable,
                body,
            } => {
                self.move_check_expr(ctx, *iterable);
                self.consume_if_local(ctx, *iterable);
                ctx.push_scope();
                self.bind_pattern_names(ctx, *pat);
                self.move_check_expr(ctx, *body);
                ctx.pop_scope();
            }

            Expr::Match { scrutinee, arms } => {
                self.move_check_expr(ctx, *scrutinee);
                self.consume_if_local(ctx, *scrutinee);
                for arm in arms {
                    ctx.push_scope();
                    self.bind_pattern_names(ctx, arm.pat);
                    if let Some(g) = arm.guard {
                        self.move_check_expr(ctx, g);
                    }
                    self.move_check_expr(ctx, arm.body);
                    ctx.pop_scope();
                }
                let mut origins = Origins::new();
                for arm in arms {
                    origins.extend(ctx.expr_origins.get(&arm.body).cloned().unwrap_or_default());
                }
                ctx.expr_origins.insert(expr_id, origins);
            }

            Expr::Array { elements } | Expr::Tuple { elements } => {
                let mut origins = Origins::new();
                for el in elements {
                    self.move_check_expr(ctx, *el);
                    self.consume_if_local(ctx, *el);
                    origins.extend(ctx.expr_origins.get(el).cloned().unwrap_or_default());
                }
                ctx.expr_origins.insert(expr_id, origins);
            }

            Expr::ArrayRepeat { value, len } => {
                self.move_check_expr(ctx, *value);
                self.consume_if_local(ctx, *value);
                self.move_check_expr(ctx, *len);
                ctx.expr_origins.insert(
                    expr_id,
                    ctx.expr_origins.get(value).cloned().unwrap_or_default(),
                );
            }

            Expr::Call { callee, args } => {
                if let Expr::FieldAccess { base, .. } = &ctx.body.exprs[*callee]
                    && let Some(place) = self.place_from_expr(ctx, *base)
                    && ctx.moved_places.iter().any(|m| place_overlaps(m, &place))
                {
                    let extra = self.move_site_labels(ctx, &place);
                    let label = self.expr_name(ctx, *base);
                    self.diag_with_labels(
                        format!("use of moved value: `{}`", label),
                        span,
                        "E0100",
                        &extra,
                    );
                }
                self.move_check_expr(ctx, *callee);
                for arg in args {
                    self.move_check_expr(ctx, *arg);
                }
                let (inputs, modes, fid) = self.call_signature(ctx, *callee, args);
                let origins = self.check_call_borrows(ctx, expr_id, &inputs, &modes, fid, span);
                ctx.expr_origins.insert(expr_id, origins);
                for (index, input) in inputs.iter().enumerate() {
                    if modes.get(index).copied().flatten().is_none() {
                        self.consume_if_local(ctx, *input);
                    }
                }
                if self
                    .type_result
                    .expr_types
                    .get(&(ctx.body_id, *callee))
                    .and_then(Type::closure_kind)
                    == Some(ClosureKind::FnOnce)
                {
                    self.consume_if_local(ctx, *callee);
                }
            }

            Expr::Lambda { params, body, .. } => {
                if let Some(info) = self
                    .type_result
                    .lambda_infos
                    .get(&(ctx.body_id, expr_id))
                    .cloned()
                {
                    self.apply_capture_effects(ctx, expr_id, &info);
                    self.move_check_lambda_body(ctx, params, *body, &info);
                }
            }

            Expr::Unsafe { body } => {
                self.move_check_expr(ctx, *body);
                ctx.expr_origins.insert(
                    expr_id,
                    ctx.expr_origins.get(body).cloned().unwrap_or_default(),
                );
            }

            Expr::Cast { base, .. } => {
                self.move_check_expr(ctx, *base);
                ctx.expr_origins.insert(
                    expr_id,
                    ctx.expr_origins.get(base).cloned().unwrap_or_default(),
                );
            }

            Expr::FieldAccess { base, field } => {
                // Check if base is already moved before recursing — if so,
                // skip inner error and emit only this outer one.
                let base_moved = self
                    .place_from_expr(ctx, *base)
                    .map(|p| ctx.moved_places.iter().any(|m| place_overlaps(m, &p)))
                    .unwrap_or(false);
                if !base_moved {
                    self.move_check_expr(ctx, *base);
                }
                if let Some(place) = self.place_from_expr(ctx, expr_id)
                    && ctx.moved_places.iter().any(|m| place_overlaps(m, &place))
                {
                    let extra = self.move_site_labels(ctx, &place);
                    self.diag_with_labels(
                        format!("use of moved field: `{}`", field.0),
                        span,
                        "E0100",
                        &extra,
                    );
                }
                let origins = if self.expr_may_carry_reference(ctx, expr_id) {
                    ctx.expr_origins.get(base).cloned().unwrap_or_default()
                } else {
                    Origins::new()
                };
                ctx.expr_origins.insert(expr_id, origins);
            }

            Expr::IndexAccess { base, index } => {
                self.move_check_expr(ctx, *base);
                self.move_check_expr(ctx, *index);
                if let Some(place) = self.place_from_expr(ctx, expr_id)
                    && ctx.moved_places.iter().any(|m| place_overlaps(m, &place))
                {
                    let extra = self.move_site_labels(ctx, &place);
                    self.diag_with_labels(
                        "use of moved value from array".into(),
                        span,
                        "E0100",
                        &extra,
                    );
                }
                let origins = if self.expr_may_carry_reference(ctx, expr_id) {
                    ctx.expr_origins.get(base).cloned().unwrap_or_default()
                } else {
                    Origins::new()
                };
                ctx.expr_origins.insert(expr_id, origins);
            }
        }
    }

    fn move_check_stmt(&mut self, ctx: &mut BodyCtx<'_>, stmt_id: StmtId) {
        let s = &ctx.body.stmts[stmt_id];
        match s {
            Stmt::Let { init, .. } => {
                if let Some(init) = init {
                    self.move_check_expr(ctx, *init);
                    let origins = ctx.expr_origins.get(init).cloned().unwrap_or_default();
                    ctx.bind_origins(stmt_id, origins);
                    self.consume_if_local(ctx, *init);
                }
            }
            Stmt::Expr { expr } => self.move_check_expr(ctx, *expr),
            Stmt::Return { value } => {
                if let Some(v) = value {
                    self.move_check_expr(ctx, *v);
                    self.consume_if_local(ctx, *v);
                }
            }
            Stmt::Break | Stmt::Continue => {}
            Stmt::Item { .. } => {}
        }
    }

    fn consume_if_local(&mut self, ctx: &mut BodyCtx<'_>, expr_id: ExprId) {
        if let Expr::Path { path, resolved } = &ctx.body.exprs[expr_id]
            && let Some(name) = path.as_single_name()
            && ctx.bindings.contains(&name.0)
        {
            let ty = self
                .type_result
                .expr_types
                .get(&(ctx.body_id, expr_id))
                .cloned()
                .unwrap_or(Type::Unknown);
            let closure_kind = self
                .type_result
                .expr_types
                .get(&(ctx.body_id, expr_id))
                .and_then(Type::closure_kind);
            if !self.trait_env.type_is_copy(&ty)
                || matches!(closure_kind, Some(ClosureKind::FnMut | ClosureKind::FnOnce))
            {
                let access_place = resolved.as_ref().and_then(access_place_from_resolved_name);
                if access_place
                    .as_ref()
                    .is_some_and(|place| self.has_any_access_borrow(ctx, place))
                {
                    self.diag(
                        format!("cannot move `{}` while borrowed", name.0),
                        ctx.expr_range(expr_id),
                        "E0304",
                    );
                    return;
                }
                ctx.bindings.mark_moved(&name.0);
                // Record move site for secondary label.
                let span = ctx.expr_range(expr_id);
                if let Some(ResolvedName::Local(stmt)) = resolved {
                    let p = Place::root(*stmt);
                    ctx.moved_places.insert(p.clone());
                    ctx.moved_sites.insert(p, (span, "value moved here".into()));
                }
            }
            return;
        }

        let Some(place) = self.place_from_expr(ctx, expr_id) else {
            return;
        };
        let ty = self
            .type_result
            .expr_types
            .get(&(ctx.body_id, expr_id))
            .cloned()
            .unwrap_or(Type::Unknown);
        let closure_kind = self
            .type_result
            .expr_types
            .get(&(ctx.body_id, expr_id))
            .and_then(Type::closure_kind);
        if self.trait_env.type_is_copy(&ty)
            && !matches!(closure_kind, Some(ClosureKind::FnMut | ClosureKind::FnOnce))
        {
            return;
        }
        if self.has_any_borrow(ctx, &place) {
            let name = self.expr_name(ctx, expr_id);
            self.diag(
                format!("cannot move `{}` while borrowed", name),
                ctx.expr_range(expr_id),
                "E0304",
            );
            return;
        }
        ctx.moved_places.insert(place.clone());
        let span = ctx.expr_range(expr_id);
        let desc = "value moved here".to_string();
        ctx.moved_sites.insert(place, (span, desc));
    }

    fn apply_capture_effects(&mut self, ctx: &mut BodyCtx<'_>, lambda: ExprId, info: &LambdaInfo) {
        let span = ctx.expr_range(lambda);
        for capture in &info.captures {
            if ctx.bindings.get(&capture.name).copied() == Some(true) {
                self.diag(
                    format!("use of moved value: `{}`", capture.name),
                    span,
                    "E0100",
                );
                continue;
            }
            let move_place = match capture.source {
                CaptureSource::Local(stmt) => Some(Place::root(stmt)),
                CaptureSource::Pattern(_)
                | CaptureSource::Param(_)
                | CaptureSource::LambdaParam { .. } => None,
            };
            let access_place = access_place_from_capture_source(&capture.source);
            if let Some(place) = &move_place
                && ctx
                    .moved_places
                    .iter()
                    .any(|moved| place_overlaps(moved, place))
            {
                let extra = self.move_site_labels(ctx, place);
                self.diag_with_labels(
                    format!("use of moved value: `{}`", capture.name),
                    span,
                    "E0100",
                    &extra,
                );
                continue;
            }

            match capture.mode {
                CaptureMode::Shared => {
                    if self.has_mut_access_borrow(ctx, &access_place) {
                        self.diag(
                            format!(
                                "cannot capture `{}` by shared reference while mutably borrowed",
                                capture.name
                            ),
                            span,
                            "E0301",
                        );
                    } else {
                        ctx.new_loan(access_place.clone(), BorrowKind::Shared, span, false);
                    }
                }
                CaptureMode::Mutable => {
                    if self.has_shared_access_borrow(ctx, &access_place) {
                        self.diag(
                            format!(
                                "cannot capture `{}` mutably while shared-borrowed",
                                capture.name
                            ),
                            span,
                            "E0300",
                        );
                    } else if self.has_mut_access_borrow(ctx, &access_place) {
                        self.diag(
                            format!("cannot capture `{}` mutably more than once", capture.name),
                            span,
                            "E0302",
                        );
                    } else {
                        ctx.new_loan(access_place.clone(), BorrowKind::Mutable, span, false);
                    }
                }
                CaptureMode::Value => {
                    if self.trait_env.type_is_copy(&capture.ty) {
                        continue;
                    }
                    if self.has_any_access_borrow(ctx, &access_place) {
                        self.diag(
                            format!("cannot move `{}` into closure while borrowed", capture.name),
                            span,
                            "E0304",
                        );
                        continue;
                    }
                    if let Some(place) = move_place {
                        ctx.moved_places.insert(place.clone());
                        ctx.moved_sites
                            .insert(place, (span, "value moved into closure here".into()));
                    }
                    ctx.bindings.mark_moved(&capture.name);
                }
            }
        }
    }

    fn move_check_lambda_body(
        &mut self,
        outer: &BodyCtx<'_>,
        params: &[hir::body::LambdaParam],
        body: ExprId,
        info: &LambdaInfo,
    ) {
        let mut ctx = BodyCtx::new(outer.body_id, outer.body);
        ctx.seed_params(
            params
                .iter()
                .map(|param| param.name.0.as_str())
                .chain(info.captures.iter().map(|capture| capture.name.as_str())),
        );
        self.move_check_expr(&mut ctx, body);
    }

    fn place_from_expr(&self, ctx: &BodyCtx<'_>, expr_id: ExprId) -> Option<Place> {
        match &ctx.body.exprs[expr_id] {
            Expr::Path {
                resolved: Some(ResolvedName::Local(stmt)),
                ..
            } => Some(Place::root(*stmt)),
            Expr::FieldAccess { base, field } => {
                let base_place = self.place_from_expr(ctx, *base)?;
                let idx = self.resolve_field_index(ctx.body_id, *base, field)?;
                Some(base_place.field(idx))
            }
            Expr::IndexAccess { base, index } => {
                let base_place = self.place_from_expr(ctx, *base)?;
                let idx = match &ctx.body.exprs[*index] {
                    Expr::IntLiteral { value, .. } => *value as usize,
                    _ => return None,
                };
                Some(base_place.index(idx))
            }
            _ => None,
        }
    }

    fn resolve_field_index(
        &self,
        body_id: BodyId,
        base: ExprId,
        field: &hir::Name,
    ) -> Option<usize> {
        let ty = self.type_result.expr_types.get(&(body_id, base))?;
        let struct_id = match ty {
            Type::Ref(inner, _) => match inner.as_ref() {
                Type::Struct(sid, _) => Some(*sid),
                _ => None,
            },
            Type::Struct(sid, _) => Some(*sid),
            _ => None,
        }?;
        let strukt = &self.hir.item_tree.structs[struct_id];
        strukt.fields.iter().position(|f| f.name == *field)
    }

    fn has_any_borrow(&self, ctx: &BodyCtx<'_>, place: &Place) -> bool {
        let place = access_place_from_move_place(place);
        self.has_any_access_borrow(ctx, &place)
    }

    fn has_any_access_borrow(&self, ctx: &BodyCtx<'_>, place: &AccessPlace) -> bool {
        ctx.loans
            .values()
            .any(|loan| loan.active && access_places_overlap(&loan.place, place))
    }

    fn access_targets(&self, ctx: &BodyCtx<'_>, expr_id: ExprId) -> Vec<AccessTarget> {
        match &ctx.body.exprs[expr_id] {
            Expr::Path {
                resolved: Some(ResolvedName::Local(stmt)),
                ..
            } => vec![AccessTarget {
                place: AccessPlace::new(AccessRoot::Local(*stmt)),
                parents: HashSet::new(),
            }],
            Expr::Path {
                resolved: Some(ResolvedName::PatternBinding(id)),
                ..
            } => vec![AccessTarget {
                place: AccessPlace::new(AccessRoot::Pattern(*id)),
                parents: HashSet::new(),
            }],
            Expr::Path {
                resolved: Some(ResolvedName::Param(index)),
                ..
            } => vec![AccessTarget {
                place: AccessPlace::new(AccessRoot::Param(*index)),
                parents: HashSet::new(),
            }],
            Expr::Path {
                resolved: Some(ResolvedName::LambdaParam { lambda, index }),
                ..
            } => vec![AccessTarget {
                place: AccessPlace::new(AccessRoot::LambdaParam {
                    lambda: *lambda,
                    index: *index,
                }),
                parents: HashSet::new(),
            }],
            Expr::FieldAccess { base, field } => {
                let index = self.resolve_field_index(ctx.body_id, *base, field);
                let mut targets = if self.expr_is_reference(ctx, *base) {
                    self.origin_targets(ctx, *base)
                } else {
                    self.access_targets(ctx, *base)
                };
                let Some(index) = index else {
                    return targets;
                };
                for target in &mut targets {
                    target.place = target.place.clone().field(index);
                }
                targets
            }
            Expr::IndexAccess { base, index } => {
                let mut targets = if self.expr_is_reference(ctx, *base) {
                    self.origin_targets(ctx, *base)
                } else {
                    self.access_targets(ctx, *base)
                };
                let index = match &ctx.body.exprs[*index] {
                    Expr::IntLiteral { value, .. } => Some(*value as usize),
                    _ => None,
                };
                for target in &mut targets {
                    target.place = target.place.clone().index(index);
                }
                targets
            }
            Expr::Unary {
                operand,
                op: UnaryOp::Deref,
            } => self.origin_targets(ctx, *operand),
            _ => Vec::new(),
        }
    }

    fn direct_local_root(&self, ctx: &BodyCtx<'_>, expr_id: ExprId) -> Option<StmtId> {
        match &ctx.body.exprs[expr_id] {
            Expr::Path {
                resolved: Some(ResolvedName::Local(stmt)),
                ..
            } => Some(*stmt),
            Expr::FieldAccess { base, .. } | Expr::IndexAccess { base, .. } => {
                self.direct_local_root(ctx, *base)
            }
            _ => None,
        }
    }

    fn origin_targets(&self, ctx: &BodyCtx<'_>, expr_id: ExprId) -> Vec<AccessTarget> {
        let mut targets: HashMap<AccessPlace, HashSet<LoanId>> = HashMap::new();
        for origin in ctx.expr_origins.get(&expr_id).into_iter().flatten() {
            targets
                .entry(origin.place.clone())
                .or_default()
                .extend(ctx.loan_family(origin.loan));
        }
        targets
            .into_iter()
            .map(|(place, parents)| AccessTarget { place, parents })
            .collect()
    }

    fn expr_is_reference(&self, ctx: &BodyCtx<'_>, expr_id: ExprId) -> bool {
        matches!(
            self.type_result.expr_types.get(&(ctx.body_id, expr_id)),
            Some(Type::Ref(..))
        )
    }

    fn call_signature(
        &self,
        ctx: &BodyCtx<'_>,
        callee: ExprId,
        args: &[ExprId],
    ) -> (Vec<ExprId>, Vec<Option<BorrowKind>>, Option<FunctionId>) {
        if let Some(call) = self
            .type_result
            .trait_method_calls
            .get(&(ctx.body_id, callee))
            && let Expr::FieldAccess { base, .. } = &ctx.body.exprs[callee]
            && let Some(function) = self.hir.item_tree.traits[call.trait_id]
                .methods
                .iter()
                .find(|method| method.name.0 == call.method)
        {
            let inputs = std::iter::once(*base)
                .chain(args.iter().copied())
                .collect::<Vec<_>>();
            let modes = function
                .params
                .iter()
                .take(inputs.len())
                .map(|param| hir_ref_kind(&param.ty))
                .collect();
            return (inputs, modes, None);
        }

        let fid = match self.type_result.expr_types.get(&(ctx.body_id, callee)) {
            Some(Type::Function(fid)) => Some(*fid),
            _ => None,
        };
        if let Some(fid) = fid {
            let function = &self.hir.item_tree.functions[fid];
            let is_method = matches!(ctx.body.exprs[callee], Expr::FieldAccess { .. })
                && !function.params.is_empty();
            let mut inputs = Vec::new();
            if is_method && let Expr::FieldAccess { base, .. } = &ctx.body.exprs[callee] {
                inputs.push(*base);
            }
            inputs.extend(args.iter().copied());
            let modes = function
                .params
                .iter()
                .take(inputs.len())
                .map(|param| hir_ref_kind(&param.ty))
                .collect();
            return (inputs, modes, Some(fid));
        }

        let inputs = args.to_vec();
        let modes = match self.type_result.expr_types.get(&(ctx.body_id, callee)) {
            Some(Type::Fn { params, .. }) => params.iter().map(type_ref_kind).collect(),
            _ => vec![None; inputs.len()],
        };
        (inputs, modes, None)
    }

    fn check_call_borrows(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        call: ExprId,
        inputs: &[ExprId],
        modes: &[Option<BorrowKind>],
        fid: Option<FunctionId>,
        span: Option<TextRange>,
    ) -> Origins {
        let mut prepared = Vec::with_capacity(inputs.len());
        for (index, input) in inputs.iter().enumerate() {
            let Some(kind) = modes.get(index).copied().flatten() else {
                prepared.push(ctx.expr_origins.get(input).cloned().unwrap_or_default());
                continue;
            };

            let targets = if self.expr_is_reference(ctx, *input) {
                self.origin_targets(ctx, *input)
            } else {
                self.access_targets(ctx, *input)
            };
            let mut origins = Origins::new();
            for target in targets {
                if self.borrow_conflicts(ctx, &target.place, kind, &target.parents, span, *input) {
                    continue;
                }
                let loan = ctx.new_loan_with_parents(
                    target.place.clone(),
                    kind,
                    span,
                    false,
                    target.parents,
                );
                origins.insert(Origin {
                    place: target.place,
                    kind,
                    loan,
                });
            }
            prepared.push(origins);
        }

        let may_carry_reference = self.expr_may_carry_reference(ctx, call);
        let summary = may_carry_reference
            .then(|| fid.and_then(|fid| self.reference_flow.summary(fid)))
            .flatten();
        let mut result = Origins::new();
        if let Some(summary) = summary {
            for source in &summary.origins {
                let Some(input_origins) = prepared.get(source.param) else {
                    continue;
                };
                for origin in input_origins {
                    let kind = BorrowKind::from_flow(source.kind, origin.kind);
                    if kind == origin.kind {
                        result.insert(origin.clone());
                        continue;
                    }
                    let parents = ctx.loan_family(origin.loan);
                    if self.borrow_conflicts(
                        ctx,
                        &origin.place,
                        kind,
                        &parents,
                        span,
                        inputs[source.param],
                    ) {
                        continue;
                    }
                    let loan =
                        ctx.new_loan_with_parents(origin.place.clone(), kind, span, false, parents);
                    result.insert(Origin {
                        place: origin.place.clone(),
                        kind,
                        loan,
                    });
                }
            }
        } else if may_carry_reference {
            for origins in &prepared {
                result.extend(origins.iter().cloned());
            }
        }

        let retained = result
            .iter()
            .map(|origin| origin.loan)
            .collect::<HashSet<_>>();
        for (index, origins) in prepared.iter().enumerate() {
            if modes.get(index).copied().flatten().is_none() {
                continue;
            }
            for origin in origins {
                if !retained.contains(&origin.loan)
                    && let Some(loan) = ctx.loans.get_mut(&origin.loan)
                {
                    loan.active = false;
                }
            }
        }

        for input in inputs {
            self.deactivate_unretained(ctx, *input, &retained);
        }

        result
    }

    fn expr_may_carry_reference(&self, ctx: &BodyCtx<'_>, expr_id: ExprId) -> bool {
        self.type_result
            .expr_types
            .get(&(ctx.body_id, expr_id))
            .is_none_or(|ty| type_may_carry_reference(self.hir, ty))
    }

    fn deactivate_unretained(
        &self,
        ctx: &mut BodyCtx<'_>,
        expr_id: ExprId,
        retained: &HashSet<LoanId>,
    ) {
        let origins = ctx.expr_origins.get(&expr_id).cloned().unwrap_or_default();
        for origin in origins {
            if retained.contains(&origin.loan) {
                continue;
            }
            if let Some(loan) = ctx.loans.get_mut(&origin.loan)
                && loan.holders.is_empty()
                && !loan.permanent
            {
                loan.active = false;
            }
        }
    }

    fn create_borrow(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        operand: ExprId,
        kind: BorrowKind,
        span: Option<TextRange>,
    ) -> Origins {
        let mut origins = Origins::new();
        for target in self.access_targets(ctx, operand) {
            if self.borrow_conflicts(ctx, &target.place, kind, &target.parents, span, operand) {
                continue;
            }
            let loan = ctx.new_loan_with_parents(
                target.place.clone(),
                kind,
                span,
                false,
                target.parents.clone(),
            );
            origins.insert(Origin {
                place: target.place,
                kind,
                loan,
            });
        }
        origins
    }

    fn borrow_conflicts(
        &mut self,
        ctx: &BodyCtx<'_>,
        place: &AccessPlace,
        kind: BorrowKind,
        parents: &HashSet<LoanId>,
        span: Option<TextRange>,
        expr_id: ExprId,
    ) -> bool {
        let conflict = ctx.loans.iter().find_map(|(id, loan)| {
            (loan.active
                && !parents.contains(id)
                && access_places_overlap(&loan.place, place)
                && !(loan.kind == BorrowKind::Shared && kind == BorrowKind::Shared))
                .then(|| loan.clone())
        });
        let Some(conflict) = conflict else {
            return false;
        };

        let name = self.expr_name(ctx, expr_id);
        let (code, message) = match (kind, conflict.kind) {
            (BorrowKind::Mutable, BorrowKind::Shared) => (
                "E0300",
                format!(
                    "cannot borrow `{}` as mutable because it is also borrowed as immutable",
                    name
                ),
            ),
            (BorrowKind::Shared, BorrowKind::Mutable) => (
                "E0301",
                format!(
                    "cannot borrow `{}` as immutable because it is also borrowed as mutable",
                    name
                ),
            ),
            (BorrowKind::Mutable, BorrowKind::Mutable) => (
                "E0302",
                format!(
                    "cannot borrow `{}` as mutable more than once at a time",
                    name
                ),
            ),
            (BorrowKind::Shared, BorrowKind::Shared) => unreachable!(),
        };
        let labels = conflict
            .issued_at
            .map(|range| {
                vec![(
                    range,
                    "first borrow occurs here".into(),
                    LabelStyle::Secondary,
                )]
            })
            .unwrap_or_default();
        self.diag_with_labels(message, span, code, &labels);
        true
    }

    fn has_shared_access_borrow(&self, ctx: &BodyCtx<'_>, place: &AccessPlace) -> bool {
        ctx.loans.values().any(|loan| {
            loan.active
                && loan.kind == BorrowKind::Shared
                && access_places_overlap(&loan.place, place)
        })
    }

    fn has_mut_access_borrow(&self, ctx: &BodyCtx<'_>, place: &AccessPlace) -> bool {
        ctx.loans.values().any(|loan| {
            loan.active
                && loan.kind == BorrowKind::Mutable
                && access_places_overlap(&loan.place, place)
        })
    }

    fn expr_name(&self, ctx: &BodyCtx<'_>, expr_id: ExprId) -> String {
        match &ctx.body.exprs[expr_id] {
            Expr::Path { path, .. } => path
                .as_single_name()
                .map(|n| n.0.as_str().to_string())
                .unwrap_or_else(|| "_".into()),
            Expr::FieldAccess { field, .. } => field.0.clone(),
            _ => String::from("_"),
        }
    }

    fn bind_pattern_names(&self, ctx: &mut BodyCtx<'_>, pat: hir::body::PatId) {
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

    fn diag(&mut self, message: String, span: Option<TextRange>, code: &'static str) {
        self.diag_with_labels(message, span, code, &[])
    }

    /// Build secondary labels for the move site that caused this E0100 error.
    fn move_site_labels(
        &self,
        ctx: &BodyCtx<'_>,
        place: &Place,
    ) -> Vec<(TextRange, String, LabelStyle)> {
        // Find the most specific moved site — scan for a prefix match.
        let mut best: Option<(&Place, &(Option<TextRange>, String))> = None;
        for (moved_place, site) in &ctx.moved_sites {
            if place_overlaps(moved_place, place) {
                match best {
                    None => best = Some((moved_place, site)),
                    Some((existing, _))
                        if moved_place.projections.len() > existing.projections.len() =>
                    {
                        best = Some((moved_place, site));
                    }
                    _ => {}
                }
            }
        }
        match best {
            Some((_, (Some(range), desc))) => {
                vec![(*range, desc.clone(), LabelStyle::Secondary)]
            }
            _ => vec![],
        }
    }

    fn diag_with_labels(
        &mut self,
        message: String,
        span: Option<TextRange>,
        code: &'static str,
        extra_labels: &[(TextRange, String, LabelStyle)],
    ) {
        let span = span.expect("move-checker diagnostics require a source range");
        let notes = match code {
            "E0100" => vec!["borrow with `&` if the original value must remain usable".into()],
            "E0300" => vec!["a mutable borrow cannot overlap an existing shared borrow".into()],
            "E0301" => vec!["a shared borrow cannot overlap an existing mutable borrow".into()],
            "E0302" => vec!["only one mutable borrow of a place may be active at a time".into()],
            "E0303" => vec!["the borrow must end before assigning to the value".into()],
            "E0304" => vec!["the borrow must end before moving the value".into()],
            _ => Vec::new(),
        };
        let mut labels = vec![SourceLabel {
            range: span,
            message: String::new(),
            style: LabelStyle::Primary,
        }];
        for (range, msg, style) in extra_labels {
            labels.push(SourceLabel {
                range: *range,
                message: msg.clone(),
                style: *style,
            });
        }
        self.result.diagnostics.push(Diagnostic {
            code,
            severity: Severity::Error,
            message,
            labels,
            help: None,
            notes,
        });
    }
}

// ═══════════════════════════════════════════════════════════
// Context types
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Clone)]
struct BorrowRecord {
    place: AccessPlace,
    kind: BorrowKind,
    scope_depth: usize,
    issued_at: Option<TextRange>,
    active: bool,
    permanent: bool,
    holders: HashSet<StmtId>,
    parents: HashSet<LoanId>,
}

struct BodyCtx<'a> {
    body_id: BodyId,
    body: &'a Body,
    source_map: &'a SourceMap,

    // Move tracking
    bindings: MoveBindings,
    moved_places: HashSet<Place>,
    /// Where each place was moved — (span, description) for secondary labels.
    moved_sites: HashMap<Place, (Option<TextRange>, String)>,

    // Borrow and reference provenance tracking
    loans: HashMap<LoanId, BorrowRecord>,
    next_loan: LoanId,
    expr_origins: HashMap<ExprId, Origins>,
    local_origins: HashMap<StmtId, Origins>,
    param_origins: HashMap<usize, Origins>,
    remaining_uses: HashMap<StmtId, usize>,
    scope_depth: usize,
}

impl<'a> BodyCtx<'a> {
    fn new(body_id: BodyId, body: &'a Body) -> Self {
        Self {
            body_id,
            body,
            source_map: &body.source_map,
            bindings: MoveBindings::default(),
            moved_places: HashSet::new(),
            moved_sites: HashMap::new(),
            loans: HashMap::new(),
            next_loan: 0,
            expr_origins: HashMap::new(),
            local_origins: HashMap::new(),
            param_origins: HashMap::new(),
            remaining_uses: collect_local_uses(body),
            scope_depth: 0,
        }
    }

    fn seed_params<'b>(&mut self, params: impl IntoIterator<Item = &'b str>) {
        for name in params {
            self.bindings.insert_available(name.to_string());
        }
    }

    fn push_scope(&mut self) {
        self.bindings.push_scope();
        self.scope_depth += 1;
    }
    fn pop_scope(&mut self) {
        self.bindings.pop_scope();
        if self.scope_depth > 0 {
            self.scope_depth -= 1;
        }
        let current = self.scope_depth;
        for loan in self.loans.values_mut() {
            if loan.scope_depth > current && !loan.permanent {
                loan.active = false;
            }
        }
    }

    fn seed_reference_params<'b>(
        &mut self,
        params: impl IntoIterator<Item = (usize, &'b hir::item_tree::HirParam)>,
    ) {
        for (index, param) in params {
            let kind = match &param.ty {
                HirTypeRef::Ref(_, true) => BorrowKind::Mutable,
                HirTypeRef::Ref(_, false) => BorrowKind::Shared,
                _ => continue,
            };
            let place = AccessPlace::new(AccessRoot::Param(index));
            let loan = self.new_loan(place.clone(), kind, Some(param.name_range), true);
            self.param_origins
                .insert(index, [Origin { place, kind, loan }].into_iter().collect());
        }
    }
    fn expr_range(&self, id: ExprId) -> Option<TextRange> {
        self.source_map.expr_ranges.get(&id).copied()
    }

    fn new_loan(
        &mut self,
        place: AccessPlace,
        kind: BorrowKind,
        issued_at: Option<TextRange>,
        permanent: bool,
    ) -> LoanId {
        self.new_loan_with_parents(place, kind, issued_at, permanent, HashSet::new())
    }

    fn new_loan_with_parents(
        &mut self,
        place: AccessPlace,
        kind: BorrowKind,
        issued_at: Option<TextRange>,
        permanent: bool,
        parents: HashSet<LoanId>,
    ) -> LoanId {
        let id = self.next_loan;
        self.next_loan += 1;
        self.loans.insert(
            id,
            BorrowRecord {
                place,
                kind,
                scope_depth: self.scope_depth,
                issued_at,
                active: true,
                permanent,
                holders: HashSet::new(),
                parents,
            },
        );
        id
    }

    fn loan_family(&self, id: LoanId) -> HashSet<LoanId> {
        let mut family = HashSet::from([id]);
        let mut pending = vec![id];
        while let Some(current) = pending.pop() {
            let Some(loan) = self.loans.get(&current) else {
                continue;
            };
            for parent in &loan.parents {
                if family.insert(*parent) {
                    pending.push(*parent);
                }
            }
        }
        family
    }

    fn bind_origins(&mut self, stmt: StmtId, origins: Origins) {
        if let Some(previous) = self.local_origins.insert(stmt, origins.clone()) {
            for origin in previous {
                if let Some(loan) = self.loans.get_mut(&origin.loan) {
                    loan.holders.remove(&stmt);
                    if loan.holders.is_empty() && !loan.permanent {
                        loan.active = false;
                    }
                }
            }
        }
        for origin in origins {
            if let Some(loan) = self.loans.get_mut(&origin.loan) {
                loan.holders.insert(stmt);
                loan.scope_depth = loan.scope_depth.min(self.scope_depth);
                loan.active = true;
            }
        }
    }

    fn release_local_if_dead(&mut self, stmt: StmtId) {
        let Some(remaining) = self.remaining_uses.get_mut(&stmt) else {
            return;
        };
        *remaining = remaining.saturating_sub(1);
        if *remaining != 0 {
            return;
        }
        let Some(origins) = self.local_origins.get(&stmt) else {
            return;
        };
        for origin in origins {
            if let Some(loan) = self.loans.get_mut(&origin.loan) {
                loan.holders.remove(&stmt);
                if loan.holders.is_empty() && !loan.permanent {
                    loan.active = false;
                }
            }
        }
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

fn place_overlaps(a: &Place, b: &Place) -> bool {
    a.is_prefix_of(b) || b.is_prefix_of(a)
}

fn access_places_overlap(a: &AccessPlace, b: &AccessPlace) -> bool {
    if a.root != b.root {
        return false;
    }
    for (left, right) in a.projections.iter().zip(&b.projections) {
        match (left, right) {
            (AccessProjection::Field(left), AccessProjection::Field(right)) if left != right => {
                return false;
            }
            (AccessProjection::Index(Some(left)), AccessProjection::Index(Some(right)))
                if left != right =>
            {
                return false;
            }
            (AccessProjection::Field(_), AccessProjection::Index(_))
            | (AccessProjection::Index(_), AccessProjection::Field(_)) => return false,
            _ => {}
        }
    }
    true
}

fn access_place_from_move_place(place: &Place) -> AccessPlace {
    let mut result = AccessPlace::new(AccessRoot::Local(place.local));
    for projection in &place.projections {
        result = match projection {
            hir::place::Projection::Field(index) => result.field(*index),
            hir::place::Projection::Index(index) => result.index(Some(*index)),
        };
    }
    result
}

fn access_place_from_resolved_name(name: &ResolvedName) -> Option<AccessPlace> {
    let root = match name {
        ResolvedName::Local(stmt) => AccessRoot::Local(*stmt),
        ResolvedName::PatternBinding(id) => AccessRoot::Pattern(*id),
        ResolvedName::Param(index) => AccessRoot::Param(*index),
        ResolvedName::LambdaParam { lambda, index } => AccessRoot::LambdaParam {
            lambda: *lambda,
            index: *index,
        },
        _ => return None,
    };
    Some(AccessPlace::new(root))
}

fn access_place_from_capture_source(source: &CaptureSource) -> AccessPlace {
    let root = match source {
        CaptureSource::Local(stmt) => AccessRoot::Local(*stmt),
        CaptureSource::Pattern(id) => AccessRoot::Pattern(*id),
        CaptureSource::Param(index) => AccessRoot::Param(*index),
        CaptureSource::LambdaParam { lambda, index } => AccessRoot::LambdaParam {
            lambda: *lambda,
            index: *index,
        },
    };
    AccessPlace::new(root)
}

fn hir_ref_kind(ty: &HirTypeRef) -> Option<BorrowKind> {
    match ty {
        HirTypeRef::Ref(_, true) => Some(BorrowKind::Mutable),
        HirTypeRef::Ref(_, false) => Some(BorrowKind::Shared),
        _ => None,
    }
}

fn type_ref_kind(ty: &Type) -> Option<BorrowKind> {
    match ty {
        Type::Ref(_, true) => Some(BorrowKind::Mutable),
        Type::Ref(_, false) => Some(BorrowKind::Shared),
        _ => None,
    }
}

fn collect_local_uses(body: &Body) -> HashMap<StmtId, usize> {
    fn expr(body: &Body, id: ExprId, uses: &mut HashMap<StmtId, usize>) {
        match &body.exprs[id] {
            Expr::Path {
                resolved: Some(ResolvedName::Local(stmt)),
                ..
            } => *uses.entry(*stmt).or_default() += 1,
            Expr::Binary { lhs, rhs, .. } => {
                expr(body, *lhs, uses);
                expr(body, *rhs, uses);
            }
            Expr::Unary { operand, .. }
            | Expr::FieldAccess { base: operand, .. }
            | Expr::Unsafe { body: operand }
            | Expr::Cast { base: operand, .. } => expr(body, *operand, uses),
            Expr::Block { stmts, tail } => {
                for stmt_id in stmts {
                    match &body.stmts[*stmt_id] {
                        Stmt::Let { init, .. } => {
                            if let Some(init) = init {
                                expr(body, *init, uses);
                            }
                        }
                        Stmt::Expr { expr: value } => expr(body, *value, uses),
                        Stmt::Return { value } => {
                            if let Some(value) = value {
                                expr(body, *value, uses);
                            }
                        }
                        Stmt::Break | Stmt::Continue | Stmt::Item { .. } => {}
                    }
                }
                if let Some(tail) = tail {
                    expr(body, *tail, uses);
                }
            }
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                expr(body, *cond, uses);
                expr(body, *then_branch, uses);
                if let Some(branch) = else_branch {
                    expr(body, *branch, uses);
                }
            }
            Expr::While {
                condition,
                body: loop_body,
            } => {
                expr(body, *condition, uses);
                expr(body, *loop_body, uses);
            }
            Expr::For {
                iterable,
                body: loop_body,
                ..
            } => {
                expr(body, *iterable, uses);
                expr(body, *loop_body, uses);
            }
            Expr::Match { scrutinee, arms } => {
                expr(body, *scrutinee, uses);
                for arm in arms {
                    if let Some(guard) = arm.guard {
                        expr(body, guard, uses);
                    }
                    expr(body, arm.body, uses);
                }
            }
            Expr::Array { elements } | Expr::Tuple { elements } => {
                for element in elements {
                    expr(body, *element, uses);
                }
            }
            Expr::ArrayRepeat { value, len } => {
                expr(body, *value, uses);
                expr(body, *len, uses);
            }
            Expr::Struct { fields, .. } => {
                for field in fields {
                    expr(body, field.value, uses);
                }
            }
            Expr::Call { callee, args } => {
                expr(body, *callee, uses);
                for arg in args {
                    expr(body, *arg, uses);
                }
            }
            Expr::Lambda {
                body: lambda_body, ..
            } => expr(body, *lambda_body, uses),
            Expr::IndexAccess { base, index } => {
                expr(body, *base, uses);
                expr(body, *index, uses);
            }
            Expr::Missing
            | Expr::IntLiteral { .. }
            | Expr::FloatLiteral { .. }
            | Expr::StringLiteral { .. }
            | Expr::CharLiteral { .. }
            | Expr::BoolLiteral { .. }
            | Expr::Path { .. } => {}
        }
    }

    let mut uses = HashMap::new();
    expr(body, body.root_block, &mut uses);
    uses
}
