use std::collections::{HashMap, HashSet};

use hir::{
    HirFile,
    body::{
        BinaryOp, Body, BodyId, Expr, ExprId, PatId, Pattern, PatternBindingId, ResolvedName, Stmt,
        StmtId, UnaryOp,
    },
    item_tree::{FunctionId, HirTypeRef},
};
use ty::{
    CaptureMode, CaptureSource, Diagnostic, LabelStyle, OperatorCall, PatternBindingMode, Severity,
    SourceLabel, Type, TypeCheckResult,
};

/// Result of escape analysis: which locals, parameters, and temporaries need stable storage.
#[derive(Debug, Default)]
pub struct EscapeResult {
    pub escaping_locals: HashSet<(BodyId, PatternBindingId)>,
    escaping_temporaries: HashSet<(BodyId, ExprId)>,
    escaping_params: HashSet<(BodyId, usize)>,
    escaping_lambda_params: HashSet<(BodyId, ExprId, usize)>,
    escaping_lambdas: HashSet<(BodyId, ExprId)>,
    lifetime_escaping_locals: HashSet<(BodyId, PatternBindingId)>,
    lifetime_escaping_temporaries: HashSet<(BodyId, ExprId)>,
    lifetime_escaping_params: HashSet<(BodyId, usize)>,
    lifetime_escaping_lambda_params: HashSet<(BodyId, ExprId, usize)>,
    address_taken_locals: HashSet<(BodyId, PatternBindingId)>,
    address_taken_params: HashSet<(BodyId, usize)>,
    address_taken_lambda_params: HashSet<(BodyId, ExprId, usize)>,
}

impl EscapeResult {
    #[must_use]
    pub fn escapes(&self, body_id: BodyId, binding: PatternBindingId) -> bool {
        self.escaping_locals.contains(&(body_id, binding))
    }

    #[must_use]
    pub fn temporary_escapes(&self, body_id: BodyId, expr: ExprId) -> bool {
        self.escaping_temporaries.contains(&(body_id, expr))
    }

    #[must_use]
    pub fn param_escapes(&self, body_id: BodyId, index: usize) -> bool {
        self.escaping_params.contains(&(body_id, index))
    }

    #[must_use]
    pub fn lambda_param_escapes(&self, body_id: BodyId, lambda: ExprId, index: usize) -> bool {
        self.escaping_lambda_params
            .contains(&(body_id, lambda, index))
    }

    #[must_use]
    pub fn lambda_escapes(&self, body_id: BodyId, lambda: ExprId) -> bool {
        self.escaping_lambdas.contains(&(body_id, lambda))
    }

    #[must_use]
    pub fn local_needs_address(&self, body_id: BodyId, binding: PatternBindingId) -> bool {
        self.address_taken_locals.contains(&(body_id, binding))
    }

    #[must_use]
    pub fn param_needs_address(&self, body_id: BodyId, index: usize) -> bool {
        self.address_taken_params.contains(&(body_id, index))
    }

    #[must_use]
    pub fn lambda_param_needs_address(
        &self,
        body_id: BodyId,
        lambda: ExprId,
        index: usize,
    ) -> bool {
        self.address_taken_lambda_params
            .contains(&(body_id, lambda, index))
    }

    #[must_use]
    pub fn reference_escape_diagnostics(&self, hir: &HirFile) -> Vec<Diagnostic> {
        let mut ranges = Vec::new();
        for (body_id, binding) in &self.lifetime_escaping_locals {
            if let Some(range) = hir.bodies[*body_id]
                .source_map
                .pat_ranges
                .get(&binding.pattern)
                .copied()
            {
                ranges.push(range);
            }
        }
        for (body_id, expr) in &self.lifetime_escaping_temporaries {
            if let Some(range) = hir.bodies[*body_id]
                .source_map
                .expr_ranges
                .get(expr)
                .copied()
            {
                ranges.push(range);
            }
        }
        for (body_id, index) in &self.lifetime_escaping_params {
            if let Some(range) = hir
                .function_bodies
                .iter()
                .find_map(|(function, body)| (*body == *body_id).then_some(*function))
                .and_then(|function| {
                    hir.item_tree.functions[function]
                        .params
                        .get(*index)
                        .map(|param| param.name_range)
                })
            {
                ranges.push(range);
            }
        }
        for (body_id, lambda, index) in &self.lifetime_escaping_lambda_params {
            let body = &hir.bodies[*body_id];
            let range = match &body.exprs[*lambda] {
                Expr::Lambda { params, .. } => params
                    .get(*index)
                    .and_then(|param| param.name_range)
                    .or_else(|| body.source_map.expr_ranges.get(lambda).copied()),
                _ => body.source_map.expr_ranges.get(lambda).copied(),
            };
            if let Some(range) = range {
                ranges.push(range);
            }
        }
        ranges.retain(|range| {
            hir.package_ranges
                .iter()
                .any(|package| package.start() <= range.start() && range.end() <= package.end())
        });
        ranges.sort_by_key(|range| (range.start(), range.end()));
        ranges.dedup();

        ranges
            .into_iter()
            .map(|range| Diagnostic {
                code: "E0310",
                severity: Severity::Error,
                message: "reference to stack-owned value cannot escape when GC is disabled".into(),
                labels: vec![SourceLabel {
                    range,
                    message: "this value would need to outlive its stack storage".into(),
                    style: LabelStyle::Primary,
                }],
                help: Some(
                    "return or capture owned data, or keep the reference within its owner's scope"
                        .into(),
                ),
                notes: vec![
                    "references received from a caller may still be forwarded without extending their lifetime"
                        .into(),
                ],
            })
            .collect()
    }
}

/// Run escape analysis on all function bodies.
///
/// Inter-procedural refinement only forces heap allocation when the local
/// callee's corresponding parameter actually escapes.
#[must_use]
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
                    lifetime_escaping: HashSet::new(),
                    returned: HashSet::new(),
                    returned_fields: Vec::new(),
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
        analyzer.result.escaping_temporaries.clear();
        analyzer.result.escaping_params.clear();
        analyzer.result.escaping_lambda_params.clear();
        analyzer.result.escaping_lambdas.clear();
        analyzer.result.lifetime_escaping_locals.clear();
        analyzer.result.lifetime_escaping_temporaries.clear();
        analyzer.result.lifetime_escaping_params.clear();
        analyzer.result.lifetime_escaping_lambda_params.clear();
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
    lifetime_escaping: HashSet<usize>,
    returned: HashSet<usize>,
    returned_fields: Vec<ReturnSummary>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ReturnSummary {
    params: HashSet<usize>,
    fields: Vec<Self>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RefSource {
    Local(PatternBindingId),
    LocalValue(PatternBindingId),
    Temporary(ExprId),
    ParamPlace(usize),
    ParamValue(usize),
    LambdaParamPlace(ExprId, usize),
    LambdaParamValue(ExprId, usize),
    Lambda(ExprId),
}

type RefSources = HashSet<RefSource>;

#[derive(Debug, Clone, Default)]
struct SourceValue {
    sources: RefSources,
    fields: Vec<Self>,
}

impl SourceValue {
    const fn from_sources(sources: RefSources) -> Self {
        Self {
            sources,
            fields: Vec::new(),
        }
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
        Self::from_sources(self.sources.clone())
    }

    fn merge(&mut self, other: Self) {
        if self.sources.is_empty() && self.fields.is_empty() {
            *self = other;
            return;
        }
        if other.sources.is_empty() && other.fields.is_empty() {
            return;
        }
        if self.fields.len() == other.fields.len() && !self.fields.is_empty() {
            for (field, other_field) in self.fields.iter_mut().zip(other.fields.iter().cloned()) {
                field.merge(other_field);
            }
        } else {
            self.fields.clear();
        }
        self.sources.extend(other.sources);
    }
}

struct EscapeAnalyzer<'a> {
    hir: &'a HirFile,
    type_result: &'a TypeCheckResult,
    result: EscapeResult,
    /// Summaries from the previous fixpoint iteration, initialized with all params.
    fn_summaries: HashMap<FunctionId, FnSummary>,
}

impl EscapeAnalyzer<'_> {
    /// Run one pass over all bodies. Returns true if any function's param
    /// summary changed (meaning another Fixpoint iteration is needed).
    fn analyze_all_bodies(&mut self) -> bool {
        let mut changed = false;
        let mut new_summaries: HashMap<FunctionId, FnSummary> = HashMap::new();

        for (fid, _) in self.hir.item_tree.functions.iter() {
            if let Some(body_id) = self.hir.function_bodies.get(&fid).copied() {
                let escaped_params = self.analyze_one_body(fid, body_id);
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

    fn analyze_one_body(&mut self, _fid: FunctionId, body_id: BodyId) -> FnSummary {
        let body = &self.hir.bodies[body_id];
        let mut ctx = EscapeCtx::new(body_id, body);

        // Bottom-up mark escaping exprs
        self.mark_escaping_exprs(&mut ctx, body.root_block);

        if let Expr::Block {
            tail: Some(tail), ..
        } = &body.exprs[body.root_block]
        {
            Self::mark_returning_sources(&mut ctx, *tail);
        }
        Self::propagate_escaping_to_locals(&mut ctx);

        // Record results
        for binding in &ctx.escaping_locals {
            self.result.escaping_locals.insert((body_id, *binding));
        }
        for expr in &ctx.escaping_temporaries {
            self.result.escaping_temporaries.insert((body_id, *expr));
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
        for binding in &ctx.lifetime_escaping_locals {
            self.result
                .lifetime_escaping_locals
                .insert((body_id, *binding));
        }
        for expr in &ctx.lifetime_escaping_temporaries {
            self.result
                .lifetime_escaping_temporaries
                .insert((body_id, *expr));
        }
        for index in &ctx.lifetime_escaping_params {
            self.result
                .lifetime_escaping_params
                .insert((body_id, *index));
        }
        for (lambda, index) in &ctx.lifetime_escaping_lambda_params {
            self.result
                .lifetime_escaping_lambda_params
                .insert((body_id, *lambda, *index));
        }
        for binding in &ctx.address_taken_locals {
            self.result.address_taken_locals.insert((body_id, *binding));
        }
        for index in &ctx.address_taken_params {
            self.result.address_taken_params.insert((body_id, *index));
        }
        for (lambda, index) in &ctx.address_taken_lambda_params {
            self.result
                .address_taken_lambda_params
                .insert((body_id, *lambda, *index));
        }

        let returned_fields = ctx
            .returned_value
            .fields
            .iter()
            .map(|field| Self::summarize_return_value(&ctx, field))
            .collect();
        FnSummary {
            escaping: ctx.escaping_params,
            lifetime_escaping: ctx.lifetime_escaping_param_values,
            returned: ctx.returned_params,
            returned_fields,
        }
    }

    fn mark_escaping_exprs(&mut self, ctx: &mut EscapeCtx<'_>, expr_id: ExprId) -> bool {
        if ctx.escaping_exprs.contains(&expr_id) {
            return true;
        }

        let expr = &ctx.body.exprs[expr_id];
        let escapes = match expr {
            Expr::Block { .. } => self.mark_block_expr(ctx, expr_id),

            Expr::Unary { .. } => self.mark_unary_expr(ctx, expr_id),

            Expr::Struct { .. }
            | Expr::Array { .. }
            | Expr::Tuple { .. }
            | Expr::ArrayRepeat { .. } => self.mark_aggregate_expr(ctx, expr_id),

            Expr::Path { .. } => Self::mark_path_expr(ctx, expr_id),

            Expr::Binary { .. } => self.mark_binary_expr(ctx, expr_id),

            Expr::Call { callee, args, .. } => {
                self.mark_escaping_exprs(ctx, *callee);
                let returned = self.handle_call_args(ctx, *callee, args);
                ctx.set_expr_source_value(expr_id, returned);
                false
            }

            Expr::Lambda { .. } => self.mark_lambda_expr(ctx, expr_id),

            Expr::If { .. }
            | Expr::While { .. }
            | Expr::Loop { .. }
            | Expr::For { .. }
            | Expr::Match { .. } => self.mark_control_expr(ctx, expr_id),

            Expr::FieldAccess { .. } | Expr::IndexAccess { .. } => {
                self.mark_projection_expr(ctx, expr_id)
            }

            Expr::Missing
            | Expr::IntLiteral { .. }
            | Expr::FloatLiteral { .. }
            | Expr::StringLiteral { .. }
            | Expr::CharLiteral { .. }
            | Expr::BoolLiteral { .. } => false,

            Expr::Unsafe { body } => {
                self.mark_escaping_exprs(ctx, *body);
                ctx.set_expr_source_value(expr_id, ctx.expr_source_value(*body));
                ctx.escaping_exprs.contains(body)
            }

            Expr::Cast { base, .. } => {
                self.mark_escaping_exprs(ctx, *base);
                if self.expr_may_carry_reference(ctx, expr_id) {
                    ctx.set_expr_source_value(expr_id, ctx.expr_source_value(*base));
                }
                ctx.escaping_exprs.contains(base)
            }

            Expr::Try { operand } => {
                self.mark_escaping_exprs(ctx, *operand);
                if self.expr_may_carry_reference(ctx, expr_id) {
                    ctx.set_expr_source_value(expr_id, ctx.expr_source_value(*operand));
                }
                ctx.escaping_exprs.contains(operand)
            }
        };

        if escapes {
            ctx.escaping_exprs.insert(expr_id);
        }
        escapes
    }

    fn mark_block_expr(&mut self, ctx: &mut EscapeCtx<'_>, expr_id: ExprId) -> bool {
        let Expr::Block { stmts, tail } = ctx.body.exprs[expr_id].clone() else {
            unreachable!("expected block expression");
        };
        let mut locals = HashSet::new();
        for stmt in stmts {
            if let Stmt::Let { pat, .. } = &ctx.body.stmts[stmt] {
                Self::collect_pattern_bindings(ctx.body, *pat, &mut locals);
            }
            self.escape_check_stmt(ctx, stmt);
        }
        let Some(tail) = tail else {
            return false;
        };
        self.mark_escaping_exprs(ctx, tail);
        let value = ctx.expr_source_value(tail);
        Self::mark_scoped_lifetime_sources(ctx, &value.sources, &locals);
        ctx.set_expr_source_value(expr_id, value);
        ctx.escaping_exprs.contains(&tail)
    }

    fn mark_unary_expr(&mut self, ctx: &mut EscapeCtx<'_>, expr_id: ExprId) -> bool {
        let Expr::Unary { operand, op } = ctx.body.exprs[expr_id] else {
            unreachable!("expected unary expression");
        };
        if matches!(op, UnaryOp::Ref | UnaryOp::MutRef) {
            self.mark_escaping_exprs(ctx, operand);
            self.record_ref(ctx, expr_id, operand);
            return false;
        }
        match self
            .type_result
            .operator_calls
            .get(&(ctx.body_id, expr_id))
            .cloned()
        {
            Some(OperatorCall::Function(fid)) => {
                let by_ref = self.hir.item_tree.functions[fid]
                    .params
                    .first()
                    .is_some_and(|param| matches!(param.ty, HirTypeRef::Ref(..)));
                let returned = self.handle_call_operand(ctx, Some(fid), 0, operand, by_ref);
                ctx.set_expr_source_value(expr_id, returned);
            }
            Some(OperatorCall::Trait(_)) => {
                let returned = self.handle_trait_operator_args(ctx, expr_id, &[operand]);
                ctx.set_expr_source_value(expr_id, returned);
            }
            None => {
                self.mark_escaping_exprs(ctx, operand);
            }
        }
        if op == UnaryOp::Deref && self.expr_may_carry_reference(ctx, expr_id) {
            Self::record_ref_chain(ctx, expr_id, operand);
        }
        ctx.escaping_exprs.contains(&operand)
    }

    fn mark_aggregate_expr(&mut self, ctx: &mut EscapeCtx<'_>, expr_id: ExprId) -> bool {
        match ctx.body.exprs[expr_id].clone() {
            Expr::Struct { fields, .. } => {
                for field in &fields {
                    self.mark_escaping_exprs(ctx, field.value);
                }
                for field in &fields {
                    Self::record_ref_chain(ctx, expr_id, field.value);
                }
                ctx.set_expr_source_fields(
                    expr_id,
                    fields
                        .iter()
                        .map(|field| ctx.expr_source_value(field.value))
                        .collect(),
                );
                fields
                    .iter()
                    .any(|field| ctx.escaping_exprs.contains(&field.value))
            }
            Expr::Array { elements } | Expr::Tuple { elements } => {
                for element in &elements {
                    self.mark_escaping_exprs(ctx, *element);
                }
                for element in &elements {
                    Self::record_ref_chain(ctx, expr_id, *element);
                }
                ctx.set_expr_source_fields(
                    expr_id,
                    elements
                        .iter()
                        .map(|element| ctx.expr_source_value(*element))
                        .collect(),
                );
                elements
                    .iter()
                    .any(|element| ctx.escaping_exprs.contains(element))
            }
            Expr::ArrayRepeat { value, len } => {
                self.mark_escaping_exprs(ctx, value);
                self.mark_escaping_exprs(ctx, len);
                Self::record_ref_chain(ctx, expr_id, value);
                ctx.set_expr_source_fields(expr_id, vec![ctx.expr_source_value(value)]);
                ctx.escaping_exprs.contains(&value)
            }
            _ => unreachable!("expected aggregate expression"),
        }
    }

    fn mark_path_expr(ctx: &mut EscapeCtx<'_>, expr_id: ExprId) -> bool {
        let Expr::Path { resolved, .. } = &ctx.body.exprs[expr_id] else {
            unreachable!("expected path expression");
        };
        match resolved {
            Some(ResolvedName::Param(index)) => {
                ctx.expr_sources
                    .entry(expr_id)
                    .or_default()
                    .insert(RefSource::ParamValue(*index));
                ctx.escaping_params.contains(index)
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
                ctx.expr_sources
                    .entry(expr_id)
                    .or_default()
                    .insert(RefSource::LocalValue(*id));
                if let Some(fields) = ctx.binding_source_fields.get(id).cloned() {
                    ctx.set_expr_source_fields(expr_id, fields);
                }
                ctx.escaping_locals.contains(id)
            }
            _ => false,
        }
    }

    fn mark_binary_expr(&mut self, ctx: &mut EscapeCtx<'_>, expr_id: ExprId) -> bool {
        let Expr::Binary { lhs, rhs, op } = ctx.body.exprs[expr_id] else {
            unreachable!("expected binary expression");
        };
        if op == BinaryOp::Assign {
            self.mark_escaping_exprs(ctx, lhs);
            self.mark_escaping_exprs(ctx, rhs);
            if let Some(sources) = ctx.expr_sources.get(&rhs).cloned() {
                if let Some(binding) = self.direct_local_root(ctx, lhs) {
                    ctx.binding_sources
                        .entry(binding)
                        .or_default()
                        .extend(sources);
                    ctx.binding_source_fields.remove(&binding);
                } else {
                    Self::mark_source_sink(ctx, &sources);
                }
            }
            return ctx.escaping_exprs.contains(&rhs);
        }
        match self
            .type_result
            .operator_calls
            .get(&(ctx.body_id, expr_id))
            .cloned()
        {
            Some(OperatorCall::Function(fid)) => {
                let returned = self.handle_operator_args(ctx, fid, lhs, rhs);
                ctx.set_expr_source_value(expr_id, returned);
            }
            Some(OperatorCall::Trait(_)) => {
                let returned = self.handle_trait_operator_args(ctx, expr_id, &[lhs, rhs]);
                ctx.set_expr_source_value(expr_id, returned);
            }
            None => {
                self.mark_escaping_exprs(ctx, lhs);
                self.mark_escaping_exprs(ctx, rhs);
            }
        }
        ctx.escaping_exprs.contains(&lhs) || ctx.escaping_exprs.contains(&rhs)
    }

    fn mark_lambda_expr(&mut self, ctx: &mut EscapeCtx<'_>, expr_id: ExprId) -> bool {
        let Expr::Lambda { body, .. } = ctx.body.exprs[expr_id] else {
            unreachable!("expected lambda expression");
        };
        let mut sources: RefSources = std::iter::once(RefSource::Lambda(expr_id)).collect();
        if let Some(info) = self.type_result.lambda_infos.get(&(ctx.body_id, expr_id)) {
            for capture in &info.captures {
                let captured = Self::capture_sources(&capture.place.source, capture.mode);
                if matches!(capture.mode, CaptureMode::Shared | CaptureMode::Mutable) {
                    Self::mark_address_taken(ctx, &captured);
                }
                sources.extend(captured);
            }
        }
        ctx.expr_sources.entry(expr_id).or_default().extend(sources);
        ctx.lambda_stack.push(expr_id);
        self.mark_escaping_exprs(ctx, body);
        if let Expr::Block {
            tail: Some(tail), ..
        } = &ctx.body.exprs[body]
        {
            Self::record_lambda_return_sources(ctx, expr_id, *tail);
        }
        ctx.lambda_stack.pop();
        false
    }

    fn mark_control_expr(&mut self, ctx: &mut EscapeCtx<'_>, expr_id: ExprId) -> bool {
        match ctx.body.exprs[expr_id].clone() {
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.mark_escaping_exprs(ctx, cond);
                self.mark_escaping_exprs(ctx, then_branch);
                if let Some(else_branch) = else_branch {
                    self.mark_escaping_exprs(ctx, else_branch);
                }
                let mut value = ctx.expr_source_value(then_branch);
                if let Some(else_branch) = else_branch {
                    value.merge(ctx.expr_source_value(else_branch));
                }
                ctx.set_expr_source_value(expr_id, value);
                ctx.escaping_exprs.contains(&then_branch)
                    || else_branch.is_some_and(|branch| ctx.escaping_exprs.contains(&branch))
            }
            Expr::While { condition, body } => {
                self.mark_escaping_exprs(ctx, condition);
                self.mark_escaping_exprs(ctx, body);
                ctx.escaping_exprs.contains(&body)
            }
            Expr::Loop { body } => {
                ctx.loop_break_stack.push(Vec::new());
                self.mark_escaping_exprs(ctx, body);
                let break_values = ctx
                    .loop_break_stack
                    .pop()
                    .expect("loop break stack must be present");
                let mut value = SourceValue::default();
                for break_value in break_values {
                    value.merge(ctx.expr_source_value(break_value));
                }
                ctx.set_expr_source_value(expr_id, value);
                ctx.escaping_exprs.contains(&body)
            }
            Expr::For {
                pat,
                iterable,
                body,
            } => {
                self.mark_escaping_exprs(ctx, iterable);
                let value = ctx.expr_source_value(iterable).iterated();
                self.bind_pattern_sources(ctx, pat, &value);
                self.mark_escaping_exprs(ctx, body);
                ctx.escaping_exprs.contains(&body)
            }
            Expr::Match { scrutinee, arms } => {
                self.mark_escaping_exprs(ctx, scrutinee);
                let scrutinee_value = ctx.expr_source_value(scrutinee);
                for arm in &arms {
                    self.bind_pattern_sources(ctx, arm.pat, &scrutinee_value);
                    if let Some(guard) = arm.guard {
                        self.mark_escaping_exprs(ctx, guard);
                    }
                    self.mark_escaping_exprs(ctx, arm.body);
                }
                let mut value = SourceValue::default();
                for arm in &arms {
                    value.merge(ctx.expr_source_value(arm.body));
                }
                ctx.set_expr_source_value(expr_id, value);
                arms.iter()
                    .any(|arm| ctx.escaping_exprs.contains(&arm.body))
            }
            _ => unreachable!("expected control-flow expression"),
        }
    }

    fn mark_projection_expr(&mut self, ctx: &mut EscapeCtx<'_>, expr_id: ExprId) -> bool {
        match ctx.body.exprs[expr_id].clone() {
            Expr::FieldAccess { base, field } => {
                self.mark_escaping_exprs(ctx, base);
                if self.expr_may_carry_reference(ctx, expr_id) {
                    let base_value = ctx.expr_source_value(base);
                    let value = self
                        .field_index(ctx, base, &field)
                        .map_or_else(|| base_value.flattened(), |index| base_value.project(index));
                    ctx.set_expr_source_value(expr_id, value);
                }
                ctx.escaping_exprs.contains(&base)
            }
            Expr::IndexAccess { base, index } => {
                self.mark_escaping_exprs(ctx, base);
                self.mark_escaping_exprs(ctx, index);
                if self.expr_may_carry_reference(ctx, expr_id) {
                    let base_value = ctx.expr_source_value(base);
                    let value = match &ctx.body.exprs[index] {
                        Expr::IntLiteral { value: index, .. } => {
                            usize::try_from(*index).ok().map_or_else(
                                || base_value.iterated(),
                                |index| base_value.project(index),
                            )
                        }
                        _ => base_value.iterated(),
                    };
                    ctx.set_expr_source_value(expr_id, value);
                }
                ctx.escaping_exprs.contains(&base) || ctx.escaping_exprs.contains(&index)
            }
            _ => unreachable!("expected projection expression"),
        }
    }

    /// Handle call arguments for escape: a ref passed to a local function
    /// only forces heap allocation when the callee's param actually escapes.
    fn handle_call_args(
        &mut self,
        ctx: &mut EscapeCtx<'_>,
        callee: ExprId,
        args: &[ExprId],
    ) -> SourceValue {
        let callee_fid = self.resolve_callee(ctx, callee);
        let mut returned = SourceValue::default();
        let mut inputs = Vec::new();
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
                RefSource::Lambda(lambda) => returned.sources.extend(
                    ctx.lambda_return_sources
                        .get(&lambda)
                        .into_iter()
                        .flatten()
                        .copied(),
                ),
                RefSource::LocalValue(binding) => pending.extend(
                    ctx.binding_sources
                        .get(&binding)
                        .into_iter()
                        .flatten()
                        .copied(),
                ),
                RefSource::Temporary(expr) => {
                    pending.extend(ctx.expr_sources.get(&expr).into_iter().flatten().copied());
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
            let value = self.handle_call_operand(ctx, callee_fid, 0, receiver, by_ref);
            returned.merge(value.clone());
            inputs.push(value);
        }

        let param_offset = usize::from(receiver.is_some());

        for (i, arg) in args.iter().enumerate() {
            let value = self.handle_call_operand(ctx, callee_fid, i + param_offset, *arg, false);
            returned.merge(value.clone());
            inputs.push(value);
        }
        if let Some(summary) = callee_fid.and_then(|fid| self.fn_summaries.get(&fid))
            && !summary.returned_fields.is_empty()
        {
            returned.fields = summary
                .returned_fields
                .iter()
                .map(|field| Self::instantiate_return_summary(field, &inputs))
                .collect();
        }
        returned
    }

    fn handle_operator_args(
        &mut self,
        ctx: &mut EscapeCtx<'_>,
        fid: FunctionId,
        lhs: ExprId,
        rhs: ExprId,
    ) -> SourceValue {
        let receiver_by_ref = self.hir.item_tree.functions[fid]
            .params
            .first()
            .is_some_and(|param| matches!(param.ty, HirTypeRef::Ref(..)));
        let mut returned = self.handle_call_operand(ctx, Some(fid), 0, lhs, receiver_by_ref);
        returned.merge(self.handle_call_operand(ctx, Some(fid), 1, rhs, false));
        returned
    }

    fn handle_trait_operator_args(
        &mut self,
        ctx: &mut EscapeCtx<'_>,
        result: ExprId,
        operands: &[ExprId],
    ) -> SourceValue {
        let mut returned = SourceValue::default();
        for operand in operands {
            self.mark_escaping_exprs(ctx, *operand);
            returned.merge(ctx.expr_source_value(*operand));
        }
        if self.expr_may_carry_reference(ctx, result) {
            returned
        } else {
            SourceValue::default()
        }
    }

    fn handle_call_operand(
        &mut self,
        ctx: &mut EscapeCtx<'_>,
        callee_fid: Option<FunctionId>,
        param_index: usize,
        operand: ExprId,
        auto_borrow: bool,
    ) -> SourceValue {
        self.mark_escaping_exprs(ctx, operand);
        let value = if auto_borrow
            && !self
                .type_result
                .expr_types
                .get(&(ctx.body_id, operand))
                .is_some_and(|ty| matches!(ty, Type::Ref(..)))
        {
            let sources = self.place_sources(ctx, operand);
            Self::mark_address_taken(ctx, &sources);
            SourceValue::from_sources(sources)
        } else {
            ctx.expr_source_value(operand)
        };
        let Some(summary) = callee_fid.and_then(|fid| self.fn_summaries.get(&fid)) else {
            Self::mark_source_sink(ctx, &value.sources);
            return SourceValue::default();
        };
        if summary.escaping.contains(&param_index) {
            Self::mark_source_sink(ctx, &value.sources);
        }
        if summary.lifetime_escaping.contains(&param_index) && !auto_borrow {
            Self::mark_lifetime_sources(ctx, &value.sources, true);
        }
        if summary.returned.contains(&param_index) {
            value
        } else {
            SourceValue::default()
        }
    }

    fn instantiate_return_summary(summary: &ReturnSummary, inputs: &[SourceValue]) -> SourceValue {
        let mut result = SourceValue::default();
        for param in &summary.params {
            if let Some(input) = inputs.get(*param) {
                result.merge(input.clone());
            }
        }
        if !summary.fields.is_empty() {
            result.fields = summary
                .fields
                .iter()
                .map(|field| Self::instantiate_return_summary(field, inputs))
                .collect();
        }
        result
    }

    fn resolve_callee(&self, ctx: &EscapeCtx<'_>, callee: ExprId) -> Option<FunctionId> {
        if let Some(Type::FunctionItem { function: fid, .. }) =
            self.type_result.expr_types.get(&(ctx.body_id, callee))
        {
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
            Stmt::Let {
                pat, init, else_, ..
            } => {
                let (pat, init) = (*pat, *init);
                let mut bindings = HashSet::new();
                Self::collect_pattern_bindings(ctx.body, pat, &mut bindings);
                if let Some(lambda) = ctx.lambda_stack.last().copied() {
                    ctx.lambda_locals
                        .entry(lambda)
                        .or_default()
                        .extend(bindings.iter().copied());
                }
                if let Some(init) = init {
                    self.mark_escaping_exprs(ctx, init);
                    let value = ctx.expr_source_value(init);
                    self.bind_pattern_sources(ctx, pat, &value);
                }
                if let Some(else_) = else_ {
                    self.mark_escaping_exprs(ctx, *else_);
                }
            }
            Stmt::Expr { expr } => {
                self.mark_escaping_exprs(ctx, *expr);
            }
            Stmt::Return { value } => {
                if let Some(v) = value {
                    self.mark_escaping_exprs(ctx, *v);
                    if let Some(lambda) = ctx.lambda_stack.last().copied() {
                        Self::record_lambda_return_sources(ctx, lambda, *v);
                    } else {
                        Self::mark_returning_sources(ctx, *v);
                    }
                }
            }
            Stmt::Break { value } => {
                if let Some(v) = value {
                    self.mark_escaping_exprs(ctx, *v);
                    if let Some(loop_values) = ctx.loop_break_stack.last_mut() {
                        loop_values.push(*v);
                    }
                }
            }
            Stmt::Continue | Stmt::Item { .. } => {}
        }
    }

    fn expr_may_carry_reference(&self, ctx: &EscapeCtx<'_>, expr_id: ExprId) -> bool {
        self.type_result
            .expr_types
            .get(&(ctx.body_id, expr_id))
            .is_none_or(type_may_carry_reference)
    }

    fn mark_returning_sources(ctx: &mut EscapeCtx<'_>, expr_id: ExprId) {
        let value = ctx.expr_source_value(expr_id);
        ctx.returned_value.merge(value);
        let Some(sources) = ctx.expr_sources.get(&expr_id).cloned() else {
            return;
        };
        Self::mark_lifetime_sources(ctx, &sources, false);
        let mut pending: Vec<RefSource> = sources.into_iter().collect();
        let mut seen = HashSet::new();
        while let Some(source) = pending.pop() {
            if !seen.insert(source) {
                continue;
            }
            match source {
                RefSource::Local(binding) => {
                    ctx.escaping_locals.insert(binding);
                }
                RefSource::LocalValue(binding) => {
                    if let Some(nested) = ctx.binding_sources.get(&binding) {
                        pending.extend(nested.iter().copied());
                    }
                }
                RefSource::Temporary(expr) => {
                    ctx.escaping_temporaries.insert(expr);
                    if let Some(nested) = ctx.expr_sources.get(&expr) {
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

    fn summarize_return_value(ctx: &EscapeCtx<'_>, value: &SourceValue) -> ReturnSummary {
        let mut params = HashSet::new();
        let mut pending = value.sources.iter().copied().collect::<Vec<_>>();
        let mut seen = RefSources::new();
        while let Some(source) = pending.pop() {
            if !seen.insert(source) {
                continue;
            }
            match source {
                RefSource::LocalValue(binding) => pending.extend(
                    ctx.binding_sources
                        .get(&binding)
                        .into_iter()
                        .flatten()
                        .copied(),
                ),
                RefSource::Temporary(expr) => {
                    pending.extend(ctx.expr_sources.get(&expr).into_iter().flatten().copied());
                }
                RefSource::ParamPlace(index) | RefSource::ParamValue(index) => {
                    params.insert(index);
                }
                RefSource::Local(_)
                | RefSource::LambdaParamPlace(..)
                | RefSource::LambdaParamValue(..)
                | RefSource::Lambda(_) => {}
            }
        }
        ReturnSummary {
            params,
            fields: value
                .fields
                .iter()
                .map(|field| Self::summarize_return_value(ctx, field))
                .collect(),
        }
    }

    fn record_lambda_return_sources(ctx: &mut EscapeCtx<'_>, lambda: ExprId, expr_id: ExprId) {
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
                RefSource::Local(binding) if locals.contains(&binding) => {
                    ctx.escaping_locals.insert(binding);
                    ctx.lifetime_escaping_locals.insert(binding);
                }
                RefSource::LocalValue(binding) if locals.contains(&binding) => {
                    if let Some(nested) = ctx.binding_sources.get(&binding) {
                        pending.extend(nested.iter().copied());
                    }
                }
                RefSource::Temporary(expr) => {
                    ctx.escaping_temporaries.insert(expr);
                    ctx.lifetime_escaping_temporaries.insert(expr);
                    if let Some(nested) = ctx.expr_sources.get(&expr) {
                        pending.extend(nested.iter().copied());
                    }
                }
                RefSource::LambdaParamPlace(owner, index) if owner == lambda => {
                    ctx.escaping_lambda_param_places.insert((owner, index));
                    ctx.lifetime_escaping_lambda_params.insert((owner, index));
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

    fn mark_lifetime_sources(
        ctx: &mut EscapeCtx<'_>,
        sources: &RefSources,
        include_param_values: bool,
    ) {
        let mut pending: Vec<RefSource> = sources.iter().copied().collect();
        let mut seen = HashSet::new();
        while let Some(source) = pending.pop() {
            if !seen.insert(source) {
                continue;
            }
            match source {
                RefSource::Local(binding) => {
                    ctx.lifetime_escaping_locals.insert(binding);
                }
                RefSource::LocalValue(binding) => {
                    if let Some(nested) = ctx.binding_sources.get(&binding) {
                        pending.extend(nested.iter().copied());
                    }
                }
                RefSource::Temporary(expr) => {
                    ctx.lifetime_escaping_temporaries.insert(expr);
                    if let Some(nested) = ctx.expr_sources.get(&expr) {
                        pending.extend(nested.iter().copied());
                    }
                }
                RefSource::ParamPlace(index) => {
                    ctx.lifetime_escaping_params.insert(index);
                }
                RefSource::LambdaParamPlace(lambda, index) => {
                    ctx.lifetime_escaping_lambda_params.insert((lambda, index));
                }
                RefSource::ParamValue(index) if include_param_values => {
                    ctx.lifetime_escaping_param_values.insert(index);
                }
                RefSource::ParamValue(_)
                | RefSource::LambdaParamValue(..)
                | RefSource::Lambda(_) => {}
            }
        }
    }

    fn mark_scoped_lifetime_sources(
        ctx: &mut EscapeCtx<'_>,
        sources: &RefSources,
        locals: &HashSet<PatternBindingId>,
    ) {
        let mut pending: Vec<RefSource> = sources.iter().copied().collect();
        let mut seen = HashSet::new();
        while let Some(source) = pending.pop() {
            if !seen.insert(source) {
                continue;
            }
            match source {
                RefSource::Local(binding) if locals.contains(&binding) => {
                    ctx.lifetime_escaping_locals.insert(binding);
                }
                RefSource::LocalValue(binding) => {
                    pending.extend(
                        ctx.binding_sources
                            .get(&binding)
                            .into_iter()
                            .flatten()
                            .copied(),
                    );
                }
                RefSource::Temporary(expr) => {
                    pending.extend(ctx.expr_sources.get(&expr).into_iter().flatten().copied());
                }
                RefSource::Local(_)
                | RefSource::ParamPlace(_)
                | RefSource::ParamValue(_)
                | RefSource::LambdaParamPlace(..)
                | RefSource::LambdaParamValue(..)
                | RefSource::Lambda(_) => {}
            }
        }
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
                RefSource::LocalValue(binding) => {
                    if let Some(nested) = ctx.binding_sources.get(&binding) {
                        pending.extend(nested.iter().copied());
                    }
                }
                RefSource::Temporary(expr) => {
                    changed |= ctx.escaping_temporaries.insert(expr);
                    if let Some(nested) = ctx.expr_sources.get(&expr) {
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

    fn capture_sources(source: &CaptureSource, mode: CaptureMode) -> RefSources {
        let by_ref = matches!(mode, CaptureMode::Shared | CaptureMode::Mutable);
        match source {
            CaptureSource::Pattern(id) if by_ref => {
                [RefSource::Local(*id), RefSource::LocalValue(*id)]
                    .into_iter()
                    .collect()
            }
            CaptureSource::Pattern(id) => std::iter::once(RefSource::LocalValue(*id)).collect(),
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
                RefSource::Local(binding) => {
                    ctx.address_taken_locals.insert(*binding);
                }
                RefSource::ParamPlace(index) => {
                    ctx.address_taken_params.insert(*index);
                }
                RefSource::LambdaParamPlace(lambda, index) => {
                    ctx.address_taken_lambda_params.insert((*lambda, *index));
                }
                RefSource::LocalValue(_)
                | RefSource::Temporary(_)
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
        Self::mark_address_taken(ctx, &sources);
        if !sources.is_empty() {
            ctx.expr_sources
                .entry(ref_expr)
                .or_default()
                .extend(sources);
        }
    }

    fn bind_pattern_sources(&self, ctx: &mut EscapeCtx<'_>, pat: PatId, value: &SourceValue) {
        match &ctx.body.pats[pat] {
            Pattern::Binding { .. } => {
                let binding = PatternBindingId {
                    pattern: pat,
                    field: None,
                };
                self.bind_pattern_source(ctx, binding, value);
            }
            Pattern::Reference { pattern, .. } => {
                self.bind_pattern_sources(ctx, *pattern, value);
            }
            Pattern::Tuple { elements } | Pattern::TupleStruct { elements, .. } => {
                let elements = elements.clone();
                for (index, element) in elements.into_iter().enumerate() {
                    self.bind_pattern_sources(ctx, element, &value.project(index));
                }
            }
            Pattern::Struct { .. } => {
                let mut bindings = HashSet::new();
                Self::collect_pattern_bindings(ctx.body, pat, &mut bindings);
                for binding in bindings {
                    self.bind_pattern_source(ctx, binding, &value.flattened());
                }
            }
            Pattern::Wildcard | Pattern::Literal(_) | Pattern::Path { .. } => {}
        }
    }

    fn bind_pattern_source(
        &self,
        ctx: &mut EscapeCtx<'_>,
        binding: PatternBindingId,
        value: &SourceValue,
    ) {
        let mode = self
            .type_result
            .pattern_binding_modes
            .get(&(ctx.body_id, binding))
            .copied()
            .unwrap_or(PatternBindingMode::Move);
        let value = match mode {
            PatternBindingMode::Ref | PatternBindingMode::RefMut => {
                Self::mark_address_taken(ctx, &value.sources);
                value.flattened()
            }
            PatternBindingMode::Move
                if self
                    .type_result
                    .pattern_binding_types
                    .get(&(ctx.body_id, binding))
                    .is_none_or(type_may_carry_reference) =>
            {
                value.clone()
            }
            PatternBindingMode::Move => SourceValue::default(),
        };
        ctx.bind_source_value(binding, value);
    }

    fn collect_pattern_bindings(body: &Body, pat: PatId, bindings: &mut HashSet<PatternBindingId>) {
        match &body.pats[pat] {
            Pattern::Binding { .. } => {
                bindings.insert(PatternBindingId {
                    pattern: pat,
                    field: None,
                });
            }
            Pattern::Reference { pattern, .. } => {
                Self::collect_pattern_bindings(body, *pattern, bindings);
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

    fn place_sources(&self, ctx: &EscapeCtx<'_>, expr_id: ExprId) -> RefSources {
        match &ctx.body.exprs[expr_id] {
            Expr::Path {
                resolved: Some(ResolvedName::PatternBinding(id)),
                ..
            } => [RefSource::Local(*id), RefSource::LocalValue(*id)]
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
            _ => std::iter::once(RefSource::Temporary(expr_id)).collect(),
        }
    }

    fn field_index(&self, ctx: &EscapeCtx<'_>, base: ExprId, field: &hir::Name) -> Option<usize> {
        let ty = self.type_result.expr_types.get(&(ctx.body_id, base))?;
        let ty = match ty {
            Type::Ref(inner, _) => inner.as_ref(),
            ty => ty,
        };
        match ty {
            Type::Struct(id, _) => self.hir.item_tree.structs[*id]
                .fields
                .iter()
                .position(|item| item.name == *field),
            Type::Tuple(elements) => field
                .0
                .parse::<usize>()
                .ok()
                .filter(|index| *index < elements.len()),
            _ => None,
        }
    }

    fn direct_local_root(&self, ctx: &EscapeCtx<'_>, expr_id: ExprId) -> Option<PatternBindingId> {
        match &ctx.body.exprs[expr_id] {
            Expr::Path {
                resolved: Some(ResolvedName::PatternBinding(id)),
                ..
            } => Some(*id),
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

    /// Propagate a flattened ref-source chain through an aggregate expression.
    fn record_ref_chain(ctx: &mut EscapeCtx<'_>, parent: ExprId, child: ExprId) {
        if let Some(sources) = ctx.expr_sources.get(&child).cloned() {
            ctx.expr_sources.entry(parent).or_default().extend(sources);
        }
    }

    fn propagate_escaping_to_locals(ctx: &mut EscapeCtx<'_>) {
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

            let locals: Vec<PatternBindingId> = ctx.escaping_locals.iter().copied().collect();
            for binding in &locals {
                if let Some(sources) = ctx.binding_sources.get(binding).cloned() {
                    changed |= Self::mark_sources(ctx, &sources);
                }
            }
        }
    }
}

fn type_may_carry_reference(ty: &Type) -> bool {
    match ty {
        Type::Ref(..)
        | Type::DynTrait { .. }
        | Type::Ptr { .. }
        | Type::Struct(..)
        | Type::Enum(..)
        | Type::Param(..)
        | Type::InferVar(..)
        | Type::Unknown
        | Type::Error
        | Type::FunctionItem { .. }
        | Type::Closure { .. }
        | Type::OpaqueCallable { .. } => true,
        Type::Tuple(elements) => elements.iter().any(type_may_carry_reference),
        Type::Slice(inner) | Type::Array(inner, _) => type_may_carry_reference(inner),
        Type::CallableConstraint(signature) => {
            signature.params.iter().any(type_may_carry_reference)
                || type_may_carry_reference(&signature.ret)
        }
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
    escaping_locals: HashSet<PatternBindingId>,
    escaping_temporaries: HashSet<ExprId>,
    escaping_params: HashSet<usize>,
    returned_params: HashSet<usize>,
    returned_value: SourceValue,
    escaping_param_places: HashSet<usize>,
    escaping_lambda_param_places: HashSet<(ExprId, usize)>,
    escaping_lambdas: HashSet<ExprId>,
    lifetime_escaping_locals: HashSet<PatternBindingId>,
    lifetime_escaping_temporaries: HashSet<ExprId>,
    lifetime_escaping_params: HashSet<usize>,
    lifetime_escaping_param_values: HashSet<usize>,
    lifetime_escaping_lambda_params: HashSet<(ExprId, usize)>,
    address_taken_locals: HashSet<PatternBindingId>,
    address_taken_params: HashSet<usize>,
    address_taken_lambda_params: HashSet<(ExprId, usize)>,
    lambda_stack: Vec<ExprId>,
    /// 每个正在分析的 `loop` 表达式收集其带值 break 的操作数。
    loop_break_stack: Vec<Vec<ExprId>>,
    lambda_locals: HashMap<ExprId, HashSet<PatternBindingId>>,
    lambda_return_sources: HashMap<ExprId, RefSources>,
    escaping_sources: RefSources,
    expr_sources: HashMap<ExprId, RefSources>,
    expr_source_fields: HashMap<ExprId, Vec<SourceValue>>,
    /// What each binding's value refers to. `let`, `match` arms and `for` all
    /// land here — `PatternBindingId` is unique per pattern site.
    binding_sources: HashMap<PatternBindingId, RefSources>,
    binding_source_fields: HashMap<PatternBindingId, Vec<SourceValue>>,
}

impl<'a> EscapeCtx<'a> {
    fn new(body_id: BodyId, body: &'a Body) -> Self {
        Self {
            body_id,
            body,
            escaping_exprs: HashSet::new(),
            escaping_locals: HashSet::new(),
            escaping_temporaries: HashSet::new(),
            escaping_params: HashSet::new(),
            returned_params: HashSet::new(),
            returned_value: SourceValue::default(),
            escaping_param_places: HashSet::new(),
            escaping_lambda_param_places: HashSet::new(),
            escaping_lambdas: HashSet::new(),
            lifetime_escaping_locals: HashSet::new(),
            lifetime_escaping_temporaries: HashSet::new(),
            lifetime_escaping_params: HashSet::new(),
            lifetime_escaping_param_values: HashSet::new(),
            lifetime_escaping_lambda_params: HashSet::new(),
            address_taken_locals: HashSet::new(),
            address_taken_params: HashSet::new(),
            address_taken_lambda_params: HashSet::new(),
            lambda_stack: Vec::new(),
            loop_break_stack: Vec::new(),
            lambda_locals: HashMap::new(),
            lambda_return_sources: HashMap::new(),
            escaping_sources: RefSources::new(),
            expr_sources: HashMap::new(),
            expr_source_fields: HashMap::new(),
            binding_sources: HashMap::new(),
            binding_source_fields: HashMap::new(),
        }
    }

    fn expr_source_value(&self, expr: ExprId) -> SourceValue {
        SourceValue {
            sources: self.expr_sources.get(&expr).cloned().unwrap_or_default(),
            fields: self
                .expr_source_fields
                .get(&expr)
                .cloned()
                .unwrap_or_default(),
        }
    }

    fn set_expr_source_fields(&mut self, expr: ExprId, fields: Vec<SourceValue>) {
        if fields.is_empty() {
            self.expr_source_fields.remove(&expr);
        } else {
            self.expr_source_fields.insert(expr, fields);
        }
    }

    fn set_expr_source_value(&mut self, expr: ExprId, value: SourceValue) {
        self.expr_sources
            .entry(expr)
            .or_default()
            .extend(value.sources);
        self.set_expr_source_fields(expr, value.fields);
    }

    fn bind_source_value(&mut self, binding: PatternBindingId, value: SourceValue) {
        self.binding_sources
            .entry(binding)
            .or_default()
            .extend(value.sources);
        if value.fields.is_empty() {
            self.binding_source_fields.remove(&binding);
        } else if let Some(fields) = self.binding_source_fields.get_mut(&binding) {
            if fields.len() == value.fields.len() {
                for (field, other) in fields.iter_mut().zip(value.fields) {
                    field.merge(other);
                }
            } else {
                self.binding_source_fields.remove(&binding);
            }
        } else {
            self.binding_source_fields.insert(binding, value.fields);
        }
    }
}
