use std::collections::{HashMap, HashSet};

use rowan::TextRange;

use hir::{
    HirFile,
    body::{
        Body, BodyId, Expr, ExprId, PatId, Pattern, PatternBindingId, ResolvedName, SourceMap,
        Stmt, StmtId, UnaryOp,
    },
    item_tree::{FunctionId, HirTypeRef},
    place::Place,
};
use type_checker::{
    CaptureMode, CapturePlace, CaptureSource, ClosureKind, Diagnostic, LabelStyle, LambdaInfo,
    PatternBindingMode, Severity, SourceLabel, TraitEnv, Type, TypeCheckResult, ValueUse,
};

mod initialization;
mod reference_flow;

use reference_flow::{
    FlowKind, FunctionSummary, ReferenceFlow, SummaryOrigin, type_may_carry_reference,
};

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
    /// Any binding introduced by a pattern — a `let`, a `match` arm, or a `for`
    /// loop. `let` has no separate root because every `let` carries a pattern.
    Pattern(PatternBindingId),
    Param(usize),
    LambdaParam {
        lambda: ExprId,
        index: usize,
    },
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

#[derive(Debug, Clone, Default)]
struct OriginValue {
    origins: Origins,
    fields: Vec<OriginValue>,
}

impl OriginValue {
    fn from_origins(origins: Origins) -> Self {
        Self {
            origins,
            fields: Vec::new(),
        }
    }

    fn from_fields(fields: Vec<Self>) -> Self {
        let origins = fields
            .iter()
            .flat_map(|field| field.origins.iter().cloned())
            .collect();
        Self { origins, fields }
    }

    fn project(&self, index: usize) -> Self {
        self.fields
            .get(index)
            .cloned()
            .unwrap_or_else(|| self.flattened())
    }

    fn iterated(&self) -> Self {
        if self.fields.is_empty() {
            return self.flattened();
        }
        let mut value = Self::default();
        for field in self.fields.iter().cloned() {
            value.merge(field);
        }
        value
    }

    fn flattened(&self) -> Self {
        Self::from_origins(self.origins.clone())
    }

    fn merge(&mut self, other: Self) {
        if self.origins.is_empty() && self.fields.is_empty() {
            *self = other;
            return;
        }
        if other.origins.is_empty() && other.fields.is_empty() {
            return;
        }
        if self.fields.len() == other.fields.len() && !self.fields.is_empty() {
            for (field, other_field) in self.fields.iter_mut().zip(other.fields.iter().cloned()) {
                field.merge(other_field);
            }
        } else {
            self.fields.clear();
        }
        self.origins.extend(other.origins);
    }
}

fn projected_origin_value(
    value: &OriginValue,
    projection: &[AccessProjection],
    index: usize,
) -> (OriginValue, Vec<AccessProjection>) {
    if let Some(field) = value.fields.get(index) {
        return (field.clone(), Vec::new());
    }
    let mut projection = projection.to_vec();
    projection.push(AccessProjection::Field(index));
    (value.flattened(), projection)
}

#[derive(Debug, Clone)]
struct AccessTarget {
    place: AccessPlace,
    parents: HashSet<LoanId>,
}

#[derive(Debug, Default)]
pub struct AnalysisResult {
    pub diagnostics: Vec<Diagnostic>,
    pub moved_exprs: HashSet<(BodyId, ExprId)>,
}

/// Run move/borrow checking. Escape analysis identifies storage duration only;
/// heap allocation does not relax move or borrow rules.
pub fn analyze(hir: &HirFile, type_result: &TypeCheckResult) -> AnalysisResult {
    let reference_flow = ReferenceFlow::build(hir, type_result);
    let mut result = AnalysisResult::default();
    initialization::check(hir, type_result, &mut result);
    let mut a = Analyzer {
        hir,
        type_result,
        trait_env: &type_result.trait_env,
        reference_flow: &reference_flow,
        result,
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
        let mut ctx = BodyCtx::new(function_id, body_id, body);
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
            self.check_returned_drop_borrow(ctx, *tail);
            self.apply_recorded_value_use(ctx, *tail);
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
                let value = match resolved {
                    Some(ResolvedName::PatternBinding(id)) => ctx.local_origin_value(*id),
                    Some(ResolvedName::Param(index)) => OriginValue::from_origins(
                        ctx.param_origins.get(index).cloned().unwrap_or_default(),
                    ),
                    _ => OriginValue::default(),
                };
                ctx.set_expr_origin_value(expr_id, value);
                if let Some(name) = path.as_single_name()
                    && let Some(moved) = ctx.bindings.get(&name.0)
                {
                    if *moved {
                        let extra = resolved
                            .as_ref()
                            .and_then(|resolved| {
                                if let ResolvedName::PatternBinding(id) = resolved {
                                    let p = Place::root(*id);
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
                    if let Some(ResolvedName::PatternBinding(id)) = resolved {
                        ctx.release_local_if_dead(*id);
                    }
                    return;
                }
                if let Some(ResolvedName::PatternBinding(id)) = resolved {
                    let place = Place::root(*id);
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
                    ctx.release_local_if_dead(*id);
                }
            }

            Expr::Struct { fields, .. } => {
                let mut origins = Origins::new();
                for field in fields {
                    self.move_check_expr(ctx, field.value);
                    self.apply_recorded_value_use(ctx, field.value);
                    origins.extend(ctx.expr_origin_value(field.value).origins);
                }
                let value = if self.expr_may_carry_reference(ctx, expr_id) {
                    OriginValue::from_origins(origins)
                } else {
                    for field in fields {
                        self.deactivate_unretained(ctx, field.value, &HashSet::new());
                    }
                    OriginValue::default()
                };
                ctx.set_expr_origin_value(expr_id, value);
            }

            Expr::Binary { lhs, rhs, op } => {
                let direct_assignment = (*op == hir::body::BinaryOp::Assign)
                    .then(|| self.local_assignment(ctx, *lhs))
                    .flatten()
                    .filter(|(_, direct)| *direct)
                    .map(|(binding, _)| binding);
                if let Some(binding) = direct_assignment {
                    ctx.release_local_if_dead(binding);
                } else {
                    self.move_check_expr(ctx, *lhs);
                }
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
                    if let Some((binding, direct)) = self.local_assignment(ctx, *lhs) {
                        let mut value = ctx.expr_origin_value(*rhs);
                        if !direct {
                            value.origins.extend(
                                ctx.local_origins
                                    .get(&binding)
                                    .into_iter()
                                    .flatten()
                                    .cloned(),
                            );
                            value.fields.clear();
                        }
                        ctx.bind_origin_value(binding, value);
                    }
                    self.apply_recorded_value_use(ctx, *rhs);
                    if let Some(binding) = direct_assignment {
                        let place = Place::root(binding);
                        ctx.bindings.mark_available(&self.expr_name(ctx, *lhs));
                        ctx.moved_places
                            .retain(|moved| !place_overlaps(moved, &place));
                        ctx.moved_sites
                            .retain(|moved, _| !place_overlaps(moved, &place));
                    }
                }
                let origins = if op.is_assignment() {
                    ctx.expr_origins.get(rhs).cloned().unwrap_or_default()
                } else {
                    self.apply_recorded_value_use(ctx, *lhs);
                    self.apply_recorded_value_use(ctx, *rhs);
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
                self.apply_recorded_value_use(ctx, *operand);
            }

            Expr::Block { stmts, tail } => {
                ctx.push_scope();
                for stmt in stmts {
                    self.move_check_stmt(ctx, *stmt);
                }
                if let Some(tail) = tail {
                    self.move_check_expr(ctx, *tail);
                    ctx.set_expr_origin_value(expr_id, ctx.expr_origin_value(*tail));
                    self.apply_recorded_value_use(ctx, *tail);
                }
                ctx.pop_scope();
            }

            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.move_check_expr(ctx, *cond);
                self.apply_recorded_value_use(ctx, *cond);
                self.move_check_expr(ctx, *then_branch);
                self.apply_recorded_value_use(ctx, *then_branch);
                if let Some(e) = else_branch {
                    self.move_check_expr(ctx, *e);
                    self.apply_recorded_value_use(ctx, *e);
                }
                let mut value = ctx.expr_origin_value(*then_branch);
                if let Some(e) = else_branch {
                    value.merge(ctx.expr_origin_value(*e));
                }
                ctx.set_expr_origin_value(expr_id, value);
            }

            Expr::While { condition, body } => {
                let loop_entry = ctx.clone();
                self.move_check_expr(ctx, *condition);
                self.apply_recorded_value_use(ctx, *condition);
                let mut condition_exit = ctx.clone();
                self.move_check_expr(ctx, *body);
                self.apply_recorded_value_use(ctx, *body);

                let mut loop_head = loop_entry.clone();
                let mut loop_exit = ctx.clone();
                loop {
                    let mut next_head = loop_entry.clone();
                    next_head.merge_move_state_from(&loop_exit);
                    if next_head.same_move_state(&loop_head) {
                        break;
                    }
                    loop_head = next_head;

                    let mut iteration = loop_entry.clone();
                    iteration.copy_move_state_from(&loop_head);
                    let diagnostic_count = self.result.diagnostics.len();
                    self.move_check_expr(&mut iteration, *condition);
                    self.apply_recorded_value_use(&mut iteration, *condition);
                    condition_exit = iteration.clone();
                    self.move_check_expr(&mut iteration, *body);
                    self.apply_recorded_value_use(&mut iteration, *body);
                    self.retain_new_loop_move_diagnostics(diagnostic_count);
                    loop_exit = iteration;
                }
                ctx.copy_move_state_from(&condition_exit);
            }

            Expr::For {
                pat,
                iterable,
                body,
            } => {
                ctx.push_scope();
                self.move_check_expr(ctx, *iterable);
                let item_value = ctx.expr_origin_value(*iterable).iterated();
                self.apply_recorded_value_use(ctx, *iterable);
                let item_ty = self
                    .type_result
                    .for_loops
                    .get(&(ctx.body_id, expr_id))
                    .map(|info| info.item_ty.clone())
                    .or_else(|| {
                        self.type_result
                            .expr_types
                            .get(&(ctx.body_id, *iterable))
                            .and_then(|ty| match ty {
                                Type::Array(item, _) => Some((**item).clone()),
                                _ => None,
                            })
                    });
                if let Some(item_ty) = item_ty {
                    self.check_pattern_move_from_drop(ctx, *pat, &item_ty);
                }
                self.check_explicit_reference_pattern_move(ctx, *pat);
                let loop_entry = ctx.clone();
                ctx.push_scope();
                self.bind_pattern_names(ctx, *pat);
                self.bind_pattern_origins(ctx, *pat, &item_value);
                self.move_check_expr(ctx, *body);
                ctx.pop_scope();

                let mut loop_head = loop_entry.clone();
                let mut loop_exit = ctx.clone();
                loop {
                    let mut next_head = loop_entry.clone();
                    next_head.merge_move_state_from(&loop_exit);
                    if next_head.same_move_state(&loop_head) {
                        break;
                    }
                    loop_head = next_head;

                    let mut iteration = loop_entry.clone();
                    iteration.copy_move_state_from(&loop_head);
                    let diagnostic_count = self.result.diagnostics.len();
                    iteration.push_scope();
                    self.bind_pattern_names(&mut iteration, *pat);
                    self.bind_pattern_origins(&mut iteration, *pat, &item_value);
                    self.move_check_expr(&mut iteration, *body);
                    self.retain_new_loop_move_diagnostics(diagnostic_count);
                    iteration.pop_scope();
                    loop_exit = iteration;
                }
                ctx.copy_move_state_from(&loop_head);
                ctx.pop_scope();
            }

            Expr::Match { scrutinee, arms } => {
                self.move_check_expr(ctx, *scrutinee);
                let scrutinee_value = ctx.expr_origin_value(*scrutinee);
                let scrutinee_ty = self
                    .type_result
                    .expr_types
                    .get(&(ctx.body_id, *scrutinee))
                    .cloned()
                    .unwrap_or(Type::Unknown);
                let scrutinee_place = self.place_from_expr(ctx, *scrutinee);
                let base_bindings = ctx.bindings.clone();
                let base_moved_places = ctx.moved_places.clone();
                let base_moved_sites = ctx.moved_sites.clone();
                let mut merged_bindings = base_bindings.clone();
                let mut merged_moved_places = base_moved_places.clone();
                let mut merged_moved_sites = base_moved_sites.clone();
                for arm in arms {
                    ctx.bindings = base_bindings.clone();
                    ctx.moved_places = base_moved_places.clone();
                    ctx.moved_sites = base_moved_sites.clone();
                    self.check_pattern_move_from_drop(ctx, arm.pat, &scrutinee_ty);
                    self.check_explicit_reference_pattern_move(ctx, arm.pat);
                    ctx.push_scope();
                    self.bind_pattern_names(ctx, arm.pat);
                    self.bind_pattern_origins(ctx, arm.pat, &scrutinee_value);
                    if let Some(g) = arm.guard {
                        let old_guard = std::mem::replace(&mut ctx.in_match_guard, true);
                        self.move_check_expr(ctx, g);
                        ctx.in_match_guard = old_guard;
                    }
                    if let Some(root) = &scrutinee_place {
                        for place in self.pattern_move_places(ctx, arm.pat, root) {
                            if self.has_any_borrow(ctx, &place) {
                                self.diag(
                                    "cannot move a pattern field while borrowed".into(),
                                    ctx.source_map.pat_ranges.get(&arm.pat).copied(),
                                    "E0304",
                                );
                                continue;
                            }
                            ctx.moved_places.insert(place.clone());
                            ctx.moved_sites.insert(
                                place,
                                (
                                    ctx.source_map.pat_ranges.get(&arm.pat).copied(),
                                    "field moved by pattern here".into(),
                                ),
                            );
                        }
                    }
                    self.move_check_expr(ctx, arm.body);
                    self.apply_recorded_value_use(ctx, arm.body);
                    ctx.pop_scope();
                    merged_bindings.merge_moved_from(&ctx.bindings);
                    merged_moved_places.extend(ctx.moved_places.iter().cloned());
                    merged_moved_sites.extend(ctx.moved_sites.clone());
                }
                ctx.bindings = merged_bindings;
                ctx.moved_places = merged_moved_places;
                ctx.moved_sites = merged_moved_sites;
                let mut value = OriginValue::default();
                for arm in arms {
                    value.merge(ctx.expr_origin_value(arm.body));
                }
                ctx.set_expr_origin_value(expr_id, value);
            }

            Expr::Array { elements } | Expr::Tuple { elements } => {
                let mut fields = Vec::with_capacity(elements.len());
                for el in elements {
                    self.move_check_expr(ctx, *el);
                    self.apply_recorded_value_use(ctx, *el);
                    fields.push(ctx.expr_origin_value(*el));
                }
                ctx.set_expr_origin_value(expr_id, OriginValue::from_fields(fields));
            }

            Expr::ArrayRepeat { value, len } => {
                self.move_check_expr(ctx, *value);
                self.apply_recorded_value_use(ctx, *value);
                self.move_check_expr(ctx, *len);
                self.apply_recorded_value_use(ctx, *len);
                ctx.set_expr_origin_value(
                    expr_id,
                    OriginValue::from_fields(vec![ctx.expr_origin_value(*value)]),
                );
            }

            Expr::Call { callee, args, .. } => {
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
                let value = self.check_call_borrows(ctx, expr_id, &inputs, &modes, fid, span);
                ctx.set_expr_origin_value(expr_id, value);
                for input in &inputs {
                    self.apply_recorded_value_use(ctx, *input);
                }
                self.apply_recorded_value_use(ctx, *callee);
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
                ctx.set_expr_origin_value(expr_id, ctx.expr_origin_value(*body));
                self.apply_recorded_value_use(ctx, *body);
            }

            Expr::Cast { base, .. } => {
                self.move_check_expr(ctx, *base);
                ctx.set_expr_origin_value(expr_id, ctx.expr_origin_value(*base));
                self.apply_recorded_value_use(ctx, *base);
            }

            Expr::Try { operand } => {
                self.move_check_expr(ctx, *operand);
                ctx.set_expr_origin_value(expr_id, ctx.expr_origin_value(*operand));
                self.apply_recorded_value_use(ctx, *operand);
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
                let value = if self.expr_may_carry_reference(ctx, expr_id) {
                    let base_value = ctx.expr_origin_value(*base);
                    self.resolve_field_index(ctx.body_id, *base, field)
                        .map(|index| base_value.project(index))
                        .unwrap_or_else(|| base_value.flattened())
                } else {
                    OriginValue::default()
                };
                ctx.set_expr_origin_value(expr_id, value);
            }

            Expr::IndexAccess { base, index } => {
                self.move_check_expr(ctx, *base);
                self.move_check_expr(ctx, *index);
                self.check_trait_index_receiver_borrow(ctx, expr_id, *base, span);
                self.apply_recorded_value_use(ctx, *index);
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
                let value = if self.expr_may_carry_reference(ctx, expr_id) {
                    let base_value = ctx.expr_origin_value(*base);
                    match &ctx.body.exprs[*index] {
                        Expr::IntLiteral { value: index, .. } => usize::try_from(*index)
                            .ok()
                            .map(|index| base_value.project(index))
                            .unwrap_or_else(|| base_value.iterated()),
                        _ => base_value.iterated(),
                    }
                } else {
                    OriginValue::default()
                };
                ctx.set_expr_origin_value(expr_id, value);
            }
        }
    }

    fn move_check_stmt(&mut self, ctx: &mut BodyCtx<'_>, stmt_id: StmtId) {
        let s = &ctx.body.stmts[stmt_id];
        match s {
            Stmt::Let { pat, init, .. } => {
                let pat = *pat;
                if let Some(init) = *init {
                    self.move_check_expr(ctx, init);
                    let value = ctx.expr_origin_value(init);
                    self.bind_pattern_origins(ctx, pat, &value);
                    self.deactivate_unretained(ctx, init, &HashSet::new());
                    self.check_explicit_reference_pattern_move(ctx, pat);
                    self.apply_recorded_value_use(ctx, init);
                }
                self.reset_pattern_moves(ctx, pat);
            }
            Stmt::Expr { expr } => {
                self.move_check_expr(ctx, *expr);
                self.apply_recorded_value_use(ctx, *expr);
            }
            Stmt::Return { value } => {
                if let Some(v) = value {
                    self.move_check_expr(ctx, *v);
                    self.check_returned_drop_borrow(ctx, *v);
                    self.apply_recorded_value_use(ctx, *v);
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
                if ctx.in_match_guard && matches!(resolved, Some(ResolvedName::PatternBinding(_))) {
                    self.diag(
                        format!("cannot move pattern binding `{}` in a match guard", name.0),
                        ctx.expr_range(expr_id),
                        "E0307",
                    );
                    return;
                }
                let access_place = resolved.as_ref().and_then(access_place_from_resolved_name);
                if access_place
                    .as_ref()
                    .is_some_and(|place| self.has_access_borrow_except_origins(ctx, place, expr_id))
                {
                    self.diag(
                        format!("cannot move `{}` while borrowed", name.0),
                        ctx.expr_range(expr_id),
                        "E0304",
                    );
                    return;
                }
                ctx.bindings.mark_moved(&name.0);
                self.result.moved_exprs.insert((ctx.body_id, expr_id));
                // Record move site for secondary label.
                let span = ctx.expr_range(expr_id);
                if let Some(ResolvedName::PatternBinding(id)) = resolved {
                    let p = Place::root(*id);
                    ctx.moved_places.insert(p.clone());
                    ctx.moved_sites.insert(p, (span, "value moved here".into()));
                }
            }
            return;
        }

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
        if self.place_has_explicit_reference_deref(ctx, expr_id) {
            self.diag(
                "cannot move out of dereference of a non-Copy value".into(),
                ctx.expr_range(expr_id),
                "E0308",
            );
            return;
        }
        let Some(place) = self.place_from_expr(ctx, expr_id) else {
            return;
        };
        if !place.projections.is_empty()
            && self
                .root_type_from_expr(ctx, expr_id)
                .is_some_and(|ty| self.trait_env.type_has_explicit_drop(ty))
        {
            self.diag(
                "cannot move out of a field of a type that implements `Drop`".into(),
                ctx.expr_range(expr_id),
                "E0305",
            );
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
        self.result.moved_exprs.insert((ctx.body_id, expr_id));
        let span = ctx.expr_range(expr_id);
        let desc = "value moved here".to_string();
        ctx.moved_sites.insert(place, (span, desc));
    }

    fn apply_recorded_value_use(&mut self, ctx: &mut BodyCtx<'_>, expr_id: ExprId) {
        if self
            .type_result
            .value_uses
            .get(&(ctx.body_id, expr_id))
            .copied()
            == Some(ValueUse::Move)
        {
            self.consume_if_local(ctx, expr_id);
        }
    }

    fn root_type_from_expr<'b>(&'b self, ctx: &BodyCtx<'_>, expr_id: ExprId) -> Option<&'b Type> {
        let mut root = expr_id;
        loop {
            root = match &ctx.body.exprs[root] {
                Expr::FieldAccess { base, .. } | Expr::IndexAccess { base, .. } => *base,
                _ => break,
            };
        }
        self.type_result.expr_types.get(&(ctx.body_id, root))
    }

    fn check_returned_drop_borrow(&mut self, ctx: &BodyCtx<'_>, expr_id: ExprId) {
        let captured_owner = self
            .type_result
            .lambda_infos
            .get(&(ctx.body_id, expr_id))
            .into_iter()
            .flat_map(|info| &info.captures)
            .find_map(|capture| {
                let root = match &capture.place.source {
                    CaptureSource::Pattern(id) => AccessRoot::Pattern(*id),
                    CaptureSource::Param(index) => AccessRoot::Param(*index),
                    CaptureSource::LambdaParam { lambda, index } => AccessRoot::LambdaParam {
                        lambda: *lambda,
                        index: *index,
                    },
                };
                (capture.mode != CaptureMode::Value && self.root_has_explicit_drop(ctx, root))
                    .then_some(root)
            });
        let owner = captured_owner.or_else(|| {
            ctx.expr_origins
                .get(&expr_id)
                .into_iter()
                .flatten()
                .find_map(|origin| {
                    self.root_has_explicit_drop(ctx, origin.place.root)
                        .then_some(origin.place.root)
                })
        });
        let Some(owner) = owner else { return };
        let owner_range = match owner {
            AccessRoot::Pattern(id) => ctx.source_map.pat_ranges.get(&id.pattern).copied(),
            AccessRoot::Param(index) => self.hir.item_tree.functions[ctx.function_id]
                .params
                .get(index)
                .map(|param| param.name_range),
            AccessRoot::LambdaParam { .. } => None,
        };
        let labels = owner_range
            .map(|range| {
                vec![(
                    range,
                    "value that owns the destructor is declared here".into(),
                    LabelStyle::Secondary,
                )]
            })
            .unwrap_or_default();
        self.diag_with_labels(
            "borrow of a value that implements `Drop` cannot outlive its owner".into(),
            ctx.expr_range(expr_id),
            "E0306",
            &labels,
        );
    }

    fn root_has_explicit_drop(&self, ctx: &BodyCtx<'_>, root: AccessRoot) -> bool {
        let ty = match root {
            AccessRoot::Pattern(binding) => self
                .type_result
                .pattern_binding_types
                .get(&(ctx.body_id, binding)),
            AccessRoot::Param(index) => {
                ctx.body
                    .exprs
                    .iter()
                    .find_map(|(expr_id, expr)| match expr {
                        Expr::Path {
                            resolved: Some(ResolvedName::Param(param)),
                            ..
                        } if *param == index => {
                            self.type_result.expr_types.get(&(ctx.body_id, expr_id))
                        }
                        _ => None,
                    })
            }
            AccessRoot::LambdaParam { .. } => None,
        };
        ty.is_some_and(|ty| self.trait_env.type_has_explicit_drop(ty))
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
            let move_place = move_place_from_capture(&capture.place);
            let access_place = access_place_from_capture(&capture.place);
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
                    if ctx.in_match_guard
                        && matches!(&capture.place.source, CaptureSource::Pattern(_))
                    {
                        self.diag(
                            format!(
                                "cannot move pattern binding `{}` in a match guard",
                                capture.name
                            ),
                            span,
                            "E0307",
                        );
                        continue;
                    }
                    if !capture.place.projections.is_empty()
                        && match &capture.place.source {
                            CaptureSource::Pattern(id) => self
                                .type_result
                                .pattern_binding_types
                                .get(&(ctx.body_id, *id))
                                .is_some_and(|ty| self.trait_env.type_has_explicit_drop(ty)),
                            CaptureSource::Param(_) | CaptureSource::LambdaParam { .. } => false,
                        }
                    {
                        self.diag(
                            "cannot move out of a field of a type that implements `Drop`".into(),
                            span,
                            "E0305",
                        );
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
                    if capture.place.projections.is_empty() {
                        ctx.bindings.mark_moved(&capture.name);
                    }
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
        let mut ctx = BodyCtx::new(outer.function_id, outer.body_id, outer.body);
        ctx.seed_params(
            params
                .iter()
                .map(|param| param.name.0.as_str())
                .chain(info.captures.iter().map(|capture| capture.name.as_str())),
        );
        self.move_check_expr(&mut ctx, body);
        if let Expr::Block {
            tail: Some(tail), ..
        } = &ctx.body.exprs[body]
        {
            self.check_returned_drop_borrow(&ctx, *tail);
        }
    }

    fn place_from_expr(&self, ctx: &BodyCtx<'_>, expr_id: ExprId) -> Option<Place> {
        match &ctx.body.exprs[expr_id] {
            Expr::Path {
                resolved: Some(ResolvedName::PatternBinding(id)),
                ..
            } => Some(Place::root(*id)),
            Expr::FieldAccess { base, field } => {
                let base_place = self.place_from_expr(ctx, *base)?;
                let idx = self.resolve_field_index(ctx.body_id, *base, field)?;
                Some(base_place.field(idx))
            }
            Expr::IndexAccess { base, index } => {
                let base_place = self.place_from_expr(ctx, *base)?;
                let idx = match &ctx.body.exprs[*index] {
                    Expr::IntLiteral { value, .. } => usize::try_from(*value).ok(),
                    _ => None,
                };
                Some(base_place.index(idx))
            }
            _ => None,
        }
    }

    fn place_has_explicit_reference_deref(&self, ctx: &BodyCtx<'_>, expr_id: ExprId) -> bool {
        match &ctx.body.exprs[expr_id] {
            Expr::Unary {
                operand,
                op: UnaryOp::Deref,
            } => matches!(
                self.type_result.expr_types.get(&(ctx.body_id, *operand)),
                Some(Type::Ref(..))
            ),
            Expr::FieldAccess { base, .. } => {
                let base_ty = self.type_result.expr_types.get(&(ctx.body_id, *base));
                let value_ty = self.type_result.expr_types.get(&(ctx.body_id, expr_id));
                matches!(base_ty, Some(Type::Ref(..)))
                    && !matches!(value_ty, Some(Type::Ptr { .. }))
                    || self.place_has_explicit_reference_deref(ctx, *base)
            }
            Expr::IndexAccess { base, .. } => {
                let base_ty = self.type_result.expr_types.get(&(ctx.body_id, *base));
                self.type_result
                    .trait_method_calls
                    .get(&(ctx.body_id, expr_id))
                    .is_some_and(|call| call.method == "index" || call.method == "index_mut")
                    || !matches!(base_ty, Some(Type::Ptr { .. }))
                        && (matches!(base_ty, Some(Type::Ref(..)))
                            || self.place_has_explicit_reference_deref(ctx, *base))
            }
            _ => false,
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

    fn check_trait_index_receiver_borrow(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        expr_id: ExprId,
        base: ExprId,
        span: Option<TextRange>,
    ) {
        let Some(call) = self
            .type_result
            .trait_method_calls
            .get(&(ctx.body_id, expr_id))
        else {
            return;
        };
        let kind = if call.method == "index_mut" {
            BorrowKind::Mutable
        } else if call.method == "index" {
            BorrowKind::Shared
        } else {
            return;
        };
        let targets = if self.expr_is_reference(ctx, base) {
            self.origin_targets(ctx, base)
        } else {
            self.access_targets(ctx, base)
        };
        for target in targets {
            self.borrow_conflicts(ctx, &target.place, kind, &target.parents, span, base);
        }
    }

    fn has_any_access_borrow(&self, ctx: &BodyCtx<'_>, place: &AccessPlace) -> bool {
        ctx.loans
            .values()
            .any(|loan| loan.active && access_places_overlap(&loan.place, place))
    }

    fn has_access_borrow_except_origins(
        &self,
        ctx: &BodyCtx<'_>,
        place: &AccessPlace,
        expr_id: ExprId,
    ) -> bool {
        let origins = ctx.expr_origins.get(&expr_id);
        ctx.loans.iter().any(|(id, loan)| {
            loan.active
                && access_places_overlap(&loan.place, place)
                && !origins.is_some_and(|origins| origins.iter().any(|origin| origin.loan == *id))
        })
    }

    fn access_targets(&self, ctx: &BodyCtx<'_>, expr_id: ExprId) -> Vec<AccessTarget> {
        match &ctx.body.exprs[expr_id] {
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
                    Expr::IntLiteral { value, .. } => usize::try_from(*value).ok(),
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

    fn local_assignment(
        &self,
        ctx: &BodyCtx<'_>,
        expr_id: ExprId,
    ) -> Option<(PatternBindingId, bool)> {
        match &ctx.body.exprs[expr_id] {
            Expr::Path {
                resolved: Some(ResolvedName::PatternBinding(id)),
                ..
            } => Some((*id, true)),
            Expr::FieldAccess { base, .. } | Expr::IndexAccess { base, .. } => {
                self.local_assignment(ctx, *base).map(|(id, _)| (id, false))
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
            Some(Type::FunctionItem { function: fid, .. }) => Some(*fid),
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
            Some(
                Type::CallableConstraint(signature)
                | Type::Closure { signature, .. }
                | Type::OpaqueCallable { signature, .. },
            ) => signature.params.iter().map(type_ref_kind).collect(),
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
    ) -> OriginValue {
        let mut prepared = Vec::with_capacity(inputs.len());
        for (index, input) in inputs.iter().enumerate() {
            let Some(kind) = modes.get(index).copied().flatten() else {
                prepared.push(ctx.expr_origin_value(*input));
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
            prepared.push(OriginValue::from_origins(origins));
        }

        let may_carry_reference = self.expr_may_carry_reference(ctx, call);
        let summary = may_carry_reference
            .then(|| fid.and_then(|fid| self.reference_flow.summary(fid)))
            .flatten();
        let result = if let Some(summary) = summary {
            self.instantiate_call_summary(ctx, summary, &prepared, inputs, span)
        } else if may_carry_reference {
            let mut result = OriginValue::default();
            for value in &prepared {
                result.merge(value.flattened());
            }
            result
        } else {
            OriginValue::default()
        };

        let retained = result
            .origins
            .iter()
            .map(|origin| origin.loan)
            .collect::<HashSet<_>>();
        for (index, value) in prepared.iter().enumerate() {
            if modes.get(index).copied().flatten().is_none() {
                continue;
            }
            for origin in &value.origins {
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

    fn instantiate_call_summary(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        summary: &FunctionSummary,
        inputs: &[OriginValue],
        input_exprs: &[ExprId],
        span: Option<TextRange>,
    ) -> OriginValue {
        let mut mapped = HashMap::new();
        self.instantiate_call_summary_inner(ctx, summary, inputs, input_exprs, span, &mut mapped)
    }

    fn instantiate_call_summary_inner(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        summary: &FunctionSummary,
        inputs: &[OriginValue],
        input_exprs: &[ExprId],
        span: Option<TextRange>,
        mapped: &mut HashMap<(SummaryOrigin, LoanId), Option<Origin>>,
    ) -> OriginValue {
        let mut result = OriginValue::default();
        for source in &summary.origins {
            let Some(input) = inputs.get(source.param) else {
                continue;
            };
            result.merge(self.map_summary_input(ctx, input, *source, input_exprs, span, mapped));
        }
        if !summary.fields.is_empty() {
            result.fields = summary
                .fields
                .iter()
                .map(|field| {
                    self.instantiate_call_summary_inner(
                        ctx,
                        field,
                        inputs,
                        input_exprs,
                        span,
                        mapped,
                    )
                })
                .collect();
        }
        if summary.opaque {
            for input in inputs {
                result.merge(input.flattened());
            }
        }
        result
    }

    fn map_summary_input(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        input: &OriginValue,
        source: SummaryOrigin,
        input_exprs: &[ExprId],
        span: Option<TextRange>,
        mapped: &mut HashMap<(SummaryOrigin, LoanId), Option<Origin>>,
    ) -> OriginValue {
        let origins = input
            .origins
            .iter()
            .filter_map(|origin| {
                if source.kind == FlowKind::Inherit {
                    return Some(origin.clone());
                }
                let key = (source, origin.loan);
                if let Some(mapped) = mapped.get(&key) {
                    return mapped.clone();
                }
                let kind = BorrowKind::from_flow(source.kind, origin.kind);
                if kind == origin.kind {
                    mapped.insert(key, Some(origin.clone()));
                    return Some(origin.clone());
                }
                let parents = ctx.loan_family(origin.loan);
                let mapped_origin = if self.borrow_conflicts(
                    ctx,
                    &origin.place,
                    kind,
                    &parents,
                    span,
                    input_exprs[source.param],
                ) {
                    None
                } else {
                    let loan =
                        ctx.new_loan_with_parents(origin.place.clone(), kind, span, false, parents);
                    Some(Origin {
                        place: origin.place.clone(),
                        kind,
                        loan,
                    })
                };
                mapped.insert(key, mapped_origin.clone());
                mapped_origin
            })
            .collect();
        let fields = input
            .fields
            .iter()
            .map(|field| self.map_summary_input(ctx, field, source, input_exprs, span, mapped))
            .collect();
        OriginValue { origins, fields }
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
            ctx.deactivate_loan_if_unheld(origin.loan);
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
        let name = self.expr_name(ctx, expr_id);
        self.borrow_conflicts_named(ctx, place, kind, parents, span, &name)
    }

    fn borrow_conflicts_named(
        &mut self,
        ctx: &BodyCtx<'_>,
        place: &AccessPlace,
        kind: BorrowKind,
        parents: &HashSet<LoanId>,
        span: Option<TextRange>,
        name: &str,
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
            hir::body::Pattern::Binding { name, .. } => {
                self.bind_pattern_name(
                    ctx,
                    PatternBindingId {
                        pattern: pat,
                        field: None,
                    },
                    &name.0,
                );
            }
            hir::body::Pattern::Reference { pattern, .. } => {
                self.bind_pattern_names(ctx, *pattern);
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
                for (index, f) in fields.iter().enumerate() {
                    if let Some(p) = f.pat {
                        self.bind_pattern_names(ctx, p);
                    } else {
                        self.bind_pattern_name(
                            ctx,
                            PatternBindingId {
                                pattern: pat,
                                field: Some(index),
                            },
                            &f.name.0,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn bind_pattern_name(&self, ctx: &mut BodyCtx<'_>, id: PatternBindingId, name: &str) {
        ctx.bindings.insert_available(name.to_string());
        self.reset_binding_move(ctx, id);
    }

    fn reset_pattern_moves(&self, ctx: &mut BodyCtx<'_>, pat: PatId) {
        let mut bindings = Vec::new();
        initialization::collect_pattern_bindings(ctx.body, pat, &mut bindings);
        for (id, _) in bindings {
            self.reset_binding_move(ctx, id);
        }
    }

    fn reset_binding_move(&self, ctx: &mut BodyCtx<'_>, id: PatternBindingId) {
        let place = Place::root(id);
        ctx.moved_places
            .retain(|moved| !place_overlaps(moved, &place));
        ctx.moved_sites
            .retain(|moved, _| !place_overlaps(moved, &place));
    }

    fn bind_pattern_origins(&mut self, ctx: &mut BodyCtx<'_>, pat: PatId, value: &OriginValue) {
        let mut reborrows = HashMap::new();
        self.bind_pattern_origins_inner(ctx, pat, value, &[], &mut reborrows);
    }

    fn bind_pattern_origins_inner(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        pat: PatId,
        value: &OriginValue,
        projection: &[AccessProjection],
        reborrows: &mut HashMap<(LoanId, BorrowKind, Vec<AccessProjection>), Origin>,
    ) {
        match &ctx.body.pats[pat] {
            Pattern::Binding { .. } => {
                let id = PatternBindingId {
                    pattern: pat,
                    field: None,
                };
                let value =
                    self.pattern_binding_origin_value(ctx, id, value, projection, reborrows);
                ctx.bind_origin_value(id, value);
            }
            Pattern::Reference { pattern, .. } => {
                self.bind_pattern_origins_inner(ctx, *pattern, value, projection, reborrows);
            }
            Pattern::Tuple { elements } | Pattern::TupleStruct { elements, .. } => {
                let elements = elements.clone();
                for (index, element) in elements.into_iter().enumerate() {
                    let (field_value, field_projection) =
                        projected_origin_value(value, projection, index);
                    self.bind_pattern_origins_inner(
                        ctx,
                        element,
                        &field_value,
                        &field_projection,
                        reborrows,
                    );
                }
            }
            Pattern::Struct { fields, .. } => {
                let fields = fields.clone();
                for (binding_index, field) in fields.into_iter().enumerate() {
                    let Some(index) = self.pattern_field_index(ctx, pat, &field.name) else {
                        continue;
                    };
                    let (field_value, field_projection) =
                        projected_origin_value(value, projection, index);
                    if let Some(field_pat) = field.pat {
                        self.bind_pattern_origins_inner(
                            ctx,
                            field_pat,
                            &field_value,
                            &field_projection,
                            reborrows,
                        );
                    } else {
                        let id = PatternBindingId {
                            pattern: pat,
                            field: Some(binding_index),
                        };
                        let field_value = self.pattern_binding_origin_value(
                            ctx,
                            id,
                            &field_value,
                            &field_projection,
                            reborrows,
                        );
                        ctx.bind_origin_value(id, field_value);
                    }
                }
            }
            Pattern::Wildcard | Pattern::Literal(_) | Pattern::Path { .. } => {}
        }
    }

    fn pattern_binding_origin_value(
        &mut self,
        ctx: &mut BodyCtx<'_>,
        id: PatternBindingId,
        value: &OriginValue,
        projection: &[AccessProjection],
        reborrows: &mut HashMap<(LoanId, BorrowKind, Vec<AccessProjection>), Origin>,
    ) -> OriginValue {
        let kind = match self
            .type_result
            .pattern_binding_modes
            .get(&(ctx.body_id, id))
        {
            Some(PatternBindingMode::Ref) => BorrowKind::Shared,
            Some(PatternBindingMode::RefMut) => BorrowKind::Mutable,
            _ => {
                return self
                    .type_result
                    .pattern_binding_types
                    .get(&(ctx.body_id, id))
                    .filter(|ty| type_may_carry_reference(self.hir, ty))
                    .map(|_| value.clone())
                    .unwrap_or_default();
            }
        };
        let mut origins = Origins::new();
        for origin in &value.origins {
            let key = (origin.loan, kind, projection.to_vec());
            if let Some(reborrow) = reborrows.get(&key) {
                origins.insert(reborrow.clone());
                continue;
            }
            let mut place = origin.place.clone();
            for projection in projection {
                place = match projection {
                    AccessProjection::Field(index) => place.field(*index),
                    AccessProjection::Index(index) => place.index(*index),
                };
            }
            let parents = ctx.loan_family(origin.loan);
            let span = ctx.source_map.pat_ranges.get(&id.pattern).copied();
            if self.borrow_conflicts_named(ctx, &place, kind, &parents, span, "pattern binding") {
                continue;
            }
            let loan = ctx.new_loan_with_parents(place.clone(), kind, span, false, parents);
            let reborrow = Origin { place, kind, loan };
            reborrows.insert(key, reborrow.clone());
            origins.insert(reborrow);
        }
        OriginValue::from_origins(origins)
    }

    fn pattern_move_places(&self, ctx: &BodyCtx<'_>, pat: PatId, root: &Place) -> Vec<Place> {
        let mut places = Vec::new();
        self.collect_pattern_move_places(ctx, pat, root, &mut places);
        places
    }

    fn collect_pattern_move_places(
        &self,
        ctx: &BodyCtx<'_>,
        pat: PatId,
        root: &Place,
        places: &mut Vec<Place>,
    ) {
        let binding_moves = |id| {
            self.type_result
                .pattern_binding_modes
                .get(&(ctx.body_id, id))
                == Some(&PatternBindingMode::Move)
                && self
                    .type_result
                    .pattern_binding_types
                    .get(&(ctx.body_id, id))
                    .is_some_and(|ty| !self.trait_env.type_is_copy(ty))
        };
        match &ctx.body.pats[pat] {
            Pattern::Binding { .. } => {
                if binding_moves(PatternBindingId {
                    pattern: pat,
                    field: None,
                }) {
                    places.push(root.clone());
                }
            }
            Pattern::Reference { .. } => {}
            Pattern::Tuple { elements } | Pattern::TupleStruct { elements, .. } => {
                for (index, element) in elements.iter().enumerate() {
                    self.collect_pattern_move_places(
                        ctx,
                        *element,
                        &root.clone().field(index),
                        places,
                    );
                }
            }
            Pattern::Struct { fields, .. } => {
                for (binding_index, field) in fields.iter().enumerate() {
                    let Some(index) = self.pattern_field_index(ctx, pat, &field.name) else {
                        continue;
                    };
                    let field_place = root.clone().field(index);
                    if let Some(field_pat) = field.pat {
                        self.collect_pattern_move_places(ctx, field_pat, &field_place, places);
                    } else if binding_moves(PatternBindingId {
                        pattern: pat,
                        field: Some(binding_index),
                    }) {
                        places.push(field_place);
                    }
                }
            }
            Pattern::Wildcard | Pattern::Literal(_) | Pattern::Path { .. } => {}
        }
    }

    fn pattern_field_index(
        &self,
        ctx: &BodyCtx<'_>,
        pat: PatId,
        field: &hir::Name,
    ) -> Option<usize> {
        let ty = self.type_result.pattern_types.get(&(ctx.body_id, pat))?;
        match ty {
            Type::Struct(id, _) => self.hir.item_tree.structs[*id]
                .fields
                .iter()
                .position(|item| item.name == *field),
            Type::Enum(id, _) => {
                let Pattern::Struct { path, .. } = &ctx.body.pats[pat] else {
                    return None;
                };
                let name = path.segments.last()?;
                let variant = self.hir.item_tree.enums[*id]
                    .variants
                    .iter()
                    .find(|variant| variant.name == *name)?;
                let hir::item_tree::HirVariantKind::Struct(fields) = &variant.kind else {
                    return None;
                };
                fields.iter().position(|item| item.name == *field)
            }
            _ => None,
        }
    }

    fn check_pattern_move_from_drop(&mut self, ctx: &BodyCtx<'_>, pat: PatId, ty: &Type) {
        let Some(pat) = self.pattern_move_from_drop(ctx, pat, ty) else {
            return;
        };
        self.diag(
            "cannot move out of a field of a type that implements `Drop`".into(),
            ctx.source_map.pat_ranges.get(&pat).copied(),
            "E0305",
        );
    }

    fn pattern_move_from_drop(&self, ctx: &BodyCtx<'_>, pat: PatId, ty: &Type) -> Option<PatId> {
        if self.trait_env.type_has_explicit_drop(ty)
            && matches!(
                ctx.body.pats[pat],
                Pattern::Tuple { .. } | Pattern::TupleStruct { .. } | Pattern::Struct { .. }
            )
            && self.pattern_moves_non_copy(ctx, pat)
        {
            return Some(pat);
        }
        let children = match &ctx.body.pats[pat] {
            Pattern::Tuple { elements } | Pattern::TupleStruct { elements, .. } => elements.clone(),
            Pattern::Struct { fields, .. } => fields.iter().filter_map(|field| field.pat).collect(),
            Pattern::Reference { .. } => Vec::new(),
            _ => Vec::new(),
        };
        children.into_iter().find_map(|child| {
            let child_ty = self.type_result.pattern_types.get(&(ctx.body_id, child))?;
            self.pattern_move_from_drop(ctx, child, child_ty)
        })
    }

    fn pattern_moves_non_copy(&self, ctx: &BodyCtx<'_>, pat: PatId) -> bool {
        let binding_moves = |id| {
            self.type_result
                .pattern_binding_modes
                .get(&(ctx.body_id, id))
                == Some(&PatternBindingMode::Move)
                && self
                    .type_result
                    .pattern_binding_types
                    .get(&(ctx.body_id, id))
                    .is_some_and(|ty| !self.trait_env.type_is_copy(ty))
        };
        match &ctx.body.pats[pat] {
            Pattern::Binding { .. } => binding_moves(PatternBindingId {
                pattern: pat,
                field: None,
            }),
            Pattern::Tuple { elements } | Pattern::TupleStruct { elements, .. } => elements
                .iter()
                .any(|element| self.pattern_moves_non_copy(ctx, *element)),
            Pattern::Struct { fields, .. } => fields.iter().enumerate().any(|(index, field)| {
                field
                    .pat
                    .is_some_and(|pat| self.pattern_moves_non_copy(ctx, pat))
                    || (field.pat.is_none()
                        && binding_moves(PatternBindingId {
                            pattern: pat,
                            field: Some(index),
                        }))
            }),
            Pattern::Reference { .. }
            | Pattern::Wildcard
            | Pattern::Literal(_)
            | Pattern::Path { .. } => false,
        }
    }

    fn check_explicit_reference_pattern_move(&mut self, ctx: &BodyCtx<'_>, pat: PatId) {
        let Some(binding) = self.explicit_reference_pattern_move(ctx, pat, false) else {
            return;
        };
        self.diag(
            "cannot move out of dereference of a non-Copy value".into(),
            ctx.source_map.pat_ranges.get(&binding).copied(),
            "E0308",
        );
    }

    fn explicit_reference_pattern_move(
        &self,
        ctx: &BodyCtx<'_>,
        pat: PatId,
        behind_reference: bool,
    ) -> Option<PatId> {
        let binding_moves = |id| {
            self.type_result
                .pattern_binding_modes
                .get(&(ctx.body_id, id))
                == Some(&PatternBindingMode::Move)
                && self
                    .type_result
                    .pattern_binding_types
                    .get(&(ctx.body_id, id))
                    .is_some_and(|ty| !self.trait_env.type_is_copy(ty))
        };
        match &ctx.body.pats[pat] {
            Pattern::Binding { .. }
                if behind_reference
                    && binding_moves(PatternBindingId {
                        pattern: pat,
                        field: None,
                    }) =>
            {
                Some(pat)
            }
            Pattern::Reference { pattern, .. } => {
                self.explicit_reference_pattern_move(ctx, *pattern, true)
            }
            Pattern::Tuple { elements } | Pattern::TupleStruct { elements, .. } => {
                elements.iter().find_map(|element| {
                    self.explicit_reference_pattern_move(ctx, *element, behind_reference)
                })
            }
            Pattern::Struct { fields, .. } => {
                fields.iter().enumerate().find_map(|(index, field)| {
                    if let Some(field_pattern) = field.pat {
                        self.explicit_reference_pattern_move(ctx, field_pattern, behind_reference)
                    } else if behind_reference
                        && binding_moves(PatternBindingId {
                            pattern: pat,
                            field: Some(index),
                        })
                    {
                        Some(pat)
                    } else {
                        None
                    }
                })
            }
            Pattern::Binding { .. }
            | Pattern::Wildcard
            | Pattern::Literal(_)
            | Pattern::Path { .. } => None,
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
            "E0059" => vec!["assign the binding on every path before reading it".into()],
            "E0100" => vec!["borrow with `&` if the original value must remain usable".into()],
            "E0300" => vec!["a mutable borrow cannot overlap an existing shared borrow".into()],
            "E0301" => vec!["a shared borrow cannot overlap an existing mutable borrow".into()],
            "E0302" => vec!["only one mutable borrow of a place may be active at a time".into()],
            "E0303" => vec!["the borrow must end before assigning to the value".into()],
            "E0304" => vec!["the borrow must end before moving the value".into()],
            "E0307" => vec![
                "borrow the pattern binding in the guard or move it from the selected arm body"
                    .into(),
            ],
            "E0308" => vec![
                "borrow through the reference, or implement `Copy` when duplication is intended"
                    .into(),
            ],
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

    fn retain_new_loop_move_diagnostics(&mut self, start: usize) {
        let replayed = self.result.diagnostics.split_off(start);
        for diagnostic in replayed {
            let primary = diagnostic.labels.first().map(|label| label.range);
            let duplicate = self.result.diagnostics.iter().any(|existing| {
                existing.code == diagnostic.code
                    && existing.message == diagnostic.message
                    && existing.labels.first().map(|label| label.range) == primary
            });
            if diagnostic.code == "E0100" && !duplicate {
                self.result.diagnostics.push(diagnostic);
            }
        }
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
    holders: HashSet<PatternBindingId>,
    parents: HashSet<LoanId>,
}

#[derive(Clone)]
struct BodyCtx<'a> {
    function_id: FunctionId,
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
    local_origins: HashMap<PatternBindingId, Origins>,
    expr_origin_fields: HashMap<ExprId, Vec<OriginValue>>,
    local_origin_fields: HashMap<PatternBindingId, Vec<OriginValue>>,
    param_origins: HashMap<usize, Origins>,
    remaining_uses: HashMap<PatternBindingId, usize>,
    scope_depth: usize,
    in_match_guard: bool,
}

impl<'a> BodyCtx<'a> {
    fn new(function_id: FunctionId, body_id: BodyId, body: &'a Body) -> Self {
        Self {
            function_id,
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
            expr_origin_fields: HashMap::new(),
            local_origin_fields: HashMap::new(),
            param_origins: HashMap::new(),
            remaining_uses: collect_local_uses(body),
            scope_depth: 0,
            in_match_guard: false,
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

    fn copy_move_state_from(&mut self, other: &Self) {
        self.bindings = other.bindings.clone();
        self.moved_places = other.moved_places.clone();
        self.moved_sites = other.moved_sites.clone();
    }

    fn merge_move_state_from(&mut self, other: &Self) {
        self.bindings.merge_moved_from(&other.bindings);
        self.moved_places.extend(other.moved_places.iter().cloned());
        self.moved_sites.extend(other.moved_sites.clone());
    }

    fn same_move_state(&self, other: &Self) -> bool {
        self.bindings == other.bindings && self.moved_places == other.moved_places
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

    fn bind_origins(&mut self, binding: PatternBindingId, origins: Origins) {
        if let Some(previous) = self.local_origins.insert(binding, origins.clone()) {
            let mut released = Vec::new();
            for origin in previous {
                if let Some(loan) = self.loans.get_mut(&origin.loan) {
                    loan.holders.remove(&binding);
                    released.push(origin.loan);
                }
            }
            for loan in released {
                self.deactivate_loan_if_unheld(loan);
            }
        }
        for origin in origins {
            if let Some(loan) = self.loans.get_mut(&origin.loan) {
                loan.holders.insert(binding);
                loan.scope_depth = loan.scope_depth.min(self.scope_depth);
                loan.active = true;
            }
        }
    }

    fn expr_origin_value(&self, expr: ExprId) -> OriginValue {
        OriginValue {
            origins: self.expr_origins.get(&expr).cloned().unwrap_or_default(),
            fields: self
                .expr_origin_fields
                .get(&expr)
                .cloned()
                .unwrap_or_default(),
        }
    }

    fn local_origin_value(&self, binding: PatternBindingId) -> OriginValue {
        OriginValue {
            origins: self
                .local_origins
                .get(&binding)
                .cloned()
                .unwrap_or_default(),
            fields: self
                .local_origin_fields
                .get(&binding)
                .cloned()
                .unwrap_or_default(),
        }
    }

    fn set_expr_origin_value(&mut self, expr: ExprId, value: OriginValue) {
        self.expr_origins.insert(expr, value.origins);
        if value.fields.is_empty() {
            self.expr_origin_fields.remove(&expr);
        } else {
            self.expr_origin_fields.insert(expr, value.fields);
        }
    }

    fn bind_origin_value(&mut self, binding: PatternBindingId, value: OriginValue) {
        self.bind_origins(binding, value.origins);
        if value.fields.is_empty() {
            self.local_origin_fields.remove(&binding);
        } else {
            self.local_origin_fields.insert(binding, value.fields);
        }
    }

    fn release_local_if_dead(&mut self, binding: PatternBindingId) {
        let Some(remaining) = self.remaining_uses.get_mut(&binding) else {
            return;
        };
        *remaining = remaining.saturating_sub(1);
        if *remaining != 0 {
            return;
        }
        let Some(origins) = self.local_origins.get(&binding) else {
            return;
        };
        let origins = origins.iter().map(|origin| origin.loan).collect::<Vec<_>>();
        for loan in &origins {
            if let Some(record) = self.loans.get_mut(loan) {
                record.holders.remove(&binding);
            }
        }
        for loan in origins {
            self.deactivate_loan_if_unheld(loan);
        }
    }

    fn deactivate_loan_if_unheld(&mut self, loan: LoanId) {
        let can_deactivate = self.loans.get(&loan).is_some_and(|record| {
            record.active
                && !record.permanent
                && record.holders.is_empty()
                && !self.loans.iter().any(|(child, child_record)| {
                    *child != loan && child_record.active && child_record.parents.contains(&loan)
                })
        });
        if !can_deactivate {
            return;
        }
        let parents = self
            .loans
            .get(&loan)
            .map(|record| record.parents.iter().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        if let Some(record) = self.loans.get_mut(&loan) {
            record.active = false;
        }
        for parent in parents {
            self.deactivate_loan_if_unheld(parent);
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

    fn mark_available(&mut self, name: &str) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(moved) = scope.get_mut(name) {
                *moved = false;
                return;
            }
        }
    }

    fn merge_moved_from(&mut self, other: &Self) {
        for (scope, other_scope) in self.scopes.iter_mut().zip(&other.scopes) {
            for (name, moved) in other_scope {
                if *moved && let Some(current) = scope.get_mut(name) {
                    *current = true;
                }
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
    let mut result = AccessPlace::new(AccessRoot::Pattern(place.local));
    for projection in &place.projections {
        result = match projection {
            hir::place::Projection::Field(index) => result.field(*index),
            hir::place::Projection::Index(index) => result.index(*index),
        };
    }
    result
}

fn access_place_from_resolved_name(name: &ResolvedName) -> Option<AccessPlace> {
    let root = match name {
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

fn access_place_from_capture(place: &CapturePlace) -> AccessPlace {
    let root = match &place.source {
        CaptureSource::Pattern(id) => AccessRoot::Pattern(*id),
        CaptureSource::Param(index) => AccessRoot::Param(*index),
        CaptureSource::LambdaParam { lambda, index } => AccessRoot::LambdaParam {
            lambda: *lambda,
            index: *index,
        },
    };
    let mut result = AccessPlace::new(root);
    for projection in &place.projections {
        result = match projection {
            hir::place::Projection::Field(index) => result.field(*index),
            hir::place::Projection::Index(index) => result.index(*index),
        };
    }
    result
}

fn move_place_from_capture(capture: &CapturePlace) -> Option<Place> {
    let CaptureSource::Pattern(id) = &capture.source else {
        return None;
    };
    let mut place = Place::root(*id);
    for projection in &capture.projections {
        place = match projection {
            hir::place::Projection::Field(index) => place.field(*index),
            hir::place::Projection::Index(index) => place.index(*index),
        };
    }
    Some(place)
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

fn collect_local_uses(body: &Body) -> HashMap<PatternBindingId, usize> {
    fn expr(body: &Body, id: ExprId, uses: &mut HashMap<PatternBindingId, usize>) {
        match &body.exprs[id] {
            Expr::Path {
                resolved: Some(ResolvedName::PatternBinding(binding)),
                ..
            } => *uses.entry(*binding).or_default() += 1,
            Expr::Binary { lhs, rhs, .. } => {
                expr(body, *lhs, uses);
                expr(body, *rhs, uses);
            }
            Expr::Unary { operand, .. }
            | Expr::FieldAccess { base: operand, .. }
            | Expr::Unsafe { body: operand }
            | Expr::Cast { base: operand, .. }
            | Expr::Try { operand } => expr(body, *operand, uses),
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
            Expr::Call { callee, args, .. } => {
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
