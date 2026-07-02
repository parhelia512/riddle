use std::collections::{HashMap, HashSet};

use rowan::TextRange;

use escape_analysis::EscapeResult;
use hir::{
    HirFile,
    body::{Body, BodyId, Expr, ExprId, ResolvedName, SourceMap, Stmt, StmtId, UnaryOp},
    item_tree::FunctionId,
    place::Place,
};
use type_checker::{
    Diagnostic, LabelStyle, Severity, SourceLabel, TraitEnv, Type, TypeCheckResult,
};

#[derive(Debug, Default)]
pub struct AnalysisResult {
    pub diagnostics: Vec<Diagnostic>,
}

/// Run move/borrow checking. Escape analysis results are consumed read-only
/// to skip borrow checks for heap-allocated (escaping) locals.
pub fn analyze(
    hir: &HirFile,
    type_result: &TypeCheckResult,
    escape_result: &EscapeResult,
) -> AnalysisResult {
    let mut a = Analyzer {
        hir,
        type_result,
        trait_env: &type_result.trait_env,
        escape_result,
        result: AnalysisResult::default(),
    };
    a.analyze_all_bodies();
    a.result
}

struct Analyzer<'a> {
    hir: &'a HirFile,
    type_result: &'a TypeCheckResult,
    trait_env: &'a TraitEnv,
    escape_result: &'a EscapeResult,
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
        // Build per-body escaping set from global escape result
        let escaping_locals: HashSet<StmtId> = self
            .escape_result
            .escaping_locals
            .iter()
            .filter(|(bid, _)| *bid == body_id)
            .map(|(_, stmt)| *stmt)
            .collect();
        let mut ctx = BodyCtx::new(body_id, body, escaping_locals);
        ctx.seed_params(
            self.hir.item_tree.functions[function_id]
                .params
                .iter()
                .map(|param| param.name.0.as_str()),
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
                if let Some(name) = path.as_single_name() {
                    if let Some(moved) = ctx.bindings.get(&name.0) {
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
                        return;
                    }
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
            }

            Expr::Struct { fields, .. } => {
                for field in fields {
                    self.move_check_expr(ctx, field.value);
                    self.consume_if_local(ctx, field.value);
                }
            }

            Expr::Binary { lhs, rhs, op } => {
                self.move_check_expr(ctx, *lhs);
                self.move_check_expr(ctx, *rhs);
                if op.is_assignment() {
                    if let Some(lhs_place) = self.place_from_expr(ctx, *lhs) {
                        if !ctx.escaping_locals.contains(&lhs_place.local) {
                            if self.has_any_borrow(ctx, &lhs_place) {
                                let name = self.expr_name(ctx, *lhs);
                                self.diag(
                                    format!("cannot assign to `{}` while borrowed", name),
                                    span,
                                    "E0303",
                                );
                            }
                        }
                    }
                    self.consume_if_local(ctx, *rhs);
                }
            }

            Expr::Unary { operand, op } => {
                self.move_check_expr(ctx, *operand);
                match op {
                    UnaryOp::Ref | UnaryOp::MutRef => {
                        if let Some(place) = self.place_from_expr(ctx, *operand) {
                            if !ctx.escaping_locals.contains(&place.local) {
                                if *op == UnaryOp::Ref {
                                    if self.has_mut_borrow(ctx, &place) {
                                        let name = self.expr_name(ctx, *operand);
                                        self.diag(
                                            format!("cannot borrow `{}` as immutable because it is also borrowed as mutable", name),
                                            span,
                                            "E0301",
                                        );
                                    } else {
                                        ctx.shared_borrows.entry(place).or_default().push(
                                            BorrowRecord {
                                                expr_id,
                                                scope_depth: ctx.scope_depth,
                                            },
                                        );
                                    }
                                } else {
                                    if self.has_shared_borrow(ctx, &place) {
                                        let name = self.expr_name(ctx, *operand);
                                        self.diag(
                                            format!("cannot borrow `{}` as mutable because it is also borrowed as immutable", name),
                                            span,
                                            "E0300",
                                        );
                                    } else if self.has_mut_borrow(ctx, &place) {
                                        let name = self.expr_name(ctx, *operand);
                                        self.diag(
                                            format!("cannot borrow `{}` as mutable more than once at a time", name),
                                            span,
                                            "E0302",
                                        );
                                    } else {
                                        ctx.mutable_borrows.entry(place).or_default().push(
                                            BorrowRecord {
                                                expr_id,
                                                scope_depth: ctx.scope_depth,
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }

            Expr::Block { stmts, tail } => {
                ctx.push_scope();
                for stmt in stmts {
                    self.move_check_stmt(ctx, *stmt);
                }
                if let Some(tail) = tail {
                    self.move_check_expr(ctx, *tail);
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
            }

            Expr::Array { elements } => {
                for el in elements {
                    self.move_check_expr(ctx, *el);
                    self.consume_if_local(ctx, *el);
                }
            }

            Expr::ArrayRepeat { value, len } => {
                self.move_check_expr(ctx, *value);
                self.consume_if_local(ctx, *value);
                self.move_check_expr(ctx, *len);
            }

            Expr::Call { callee, args } => {
                if let Expr::FieldAccess { base, .. } = &ctx.body.exprs[*callee] {
                    if let Some(place) = self.place_from_expr(ctx, *base) {
                        if ctx.moved_places.iter().any(|m| place_overlaps(m, &place)) {
                            let extra = self.move_site_labels(ctx, &place);
                            let label = self.expr_name(ctx, *base);
                            self.diag_with_labels(
                                format!("use of moved value: `{}`", label),
                                span,
                                "E0100",
                                &extra,
                            );
                        }
                    }
                }
                self.move_check_expr(ctx, *callee);
                for arg in args {
                    self.move_check_expr(ctx, *arg);
                    self.consume_if_local(ctx, *arg);
                }
            }

            Expr::Unsafe { body } => {
                self.move_check_expr(ctx, *body);
            }

            Expr::Cast { base, .. } => {
                self.move_check_expr(ctx, *base);
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
                if let Some(place) = self.place_from_expr(ctx, expr_id) {
                    if ctx.moved_places.iter().any(|m| place_overlaps(m, &place)) {
                        let extra = self.move_site_labels(ctx, &place);
                        self.diag_with_labels(
                            format!("use of moved field: `{}`", field.0),
                            span,
                            "E0100",
                            &extra,
                        );
                    }
                }
            }

            Expr::IndexAccess { base, index } => {
                self.move_check_expr(ctx, *base);
                self.move_check_expr(ctx, *index);
                if let Some(place) = self.place_from_expr(ctx, expr_id) {
                    if ctx.moved_places.iter().any(|m| place_overlaps(m, &place)) {
                        let extra = self.move_site_labels(ctx, &place);
                        self.diag_with_labels(
                            "use of moved value from array".into(),
                            span,
                            "E0100",
                            &extra,
                        );
                    }
                }
            }
        }
    }

    fn move_check_stmt(&mut self, ctx: &mut BodyCtx<'_>, stmt_id: StmtId) {
        let s = &ctx.body.stmts[stmt_id];
        match s {
            Stmt::Let { init, .. } => {
                if let Some(init) = init {
                    self.move_check_expr(ctx, *init);
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
            Stmt::Item { .. } => {}
        }
    }

    fn consume_if_local(&mut self, ctx: &mut BodyCtx<'_>, expr_id: ExprId) {
        if let Expr::Path { path, resolved } = &ctx.body.exprs[expr_id] {
            if let Some(name) = path.as_single_name() {
                if ctx.bindings.contains(&name.0) {
                    let ty = self
                        .type_result
                        .expr_types
                        .get(&(ctx.body_id, expr_id))
                        .cloned()
                        .unwrap_or(Type::Unknown);
                    if !self.trait_env.type_is_copy(&ty) {
                        ctx.bindings.mark_moved(&name.0);
                        // Record move site for secondary label.
                        let span = ctx.expr_range(expr_id);
                        if let Some(ResolvedName::Local(stmt)) = resolved {
                            let p = Place::root(*stmt);
                            ctx.moved_sites.insert(p, (span, "value moved here".into()));
                        }
                    }
                    return;
                }
            }
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
        if self.trait_env.type_is_copy(&ty) {
            return;
        }
        if !ctx.escaping_locals.contains(&place.local) {
            if self.has_any_borrow(ctx, &place) {
                let name = self.expr_name(ctx, expr_id);
                self.diag(
                    format!("cannot move `{}` while borrowed", name),
                    ctx.expr_range(expr_id),
                    "E0304",
                );
                return;
            }
        }
        ctx.moved_places.insert(place.clone());
        let span = ctx.expr_range(expr_id);
        let desc = "value moved here".to_string();
        ctx.moved_sites.insert(place, (span, desc));
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
        self.has_shared_borrow(ctx, place) || self.has_mut_borrow(ctx, place)
    }

    fn has_shared_borrow(&self, ctx: &BodyCtx<'_>, place: &Place) -> bool {
        ctx.shared_borrows
            .keys()
            .any(|b| b.is_prefix_of(place) || place.is_prefix_of(b))
    }

    fn has_mut_borrow(&self, ctx: &BodyCtx<'_>, place: &Place) -> bool {
        ctx.mutable_borrows
            .keys()
            .any(|b| b.is_prefix_of(place) || place.is_prefix_of(b))
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
            Some((_, &(ref span, ref desc))) => match span {
                Some(range) => vec![(*range, desc.clone(), LabelStyle::Secondary)],
                None => vec![],
            },
            None => vec![],
        }
    }

    fn diag_with_labels(
        &mut self,
        message: String,
        span: Option<TextRange>,
        code: &'static str,
        extra_labels: &[(TextRange, String, LabelStyle)],
    ) {
        let help = match code {
            "E0100" => Some("consider borrowing with `&`".into()),
            "E0300" => Some("cannot borrow as mutable while already borrowed as immutable".into()),
            "E0301" => Some("cannot borrow as immutable while already borrowed as mutable".into()),
            "E0302" => Some("cannot borrow as mutable more than once at a time".into()),
            "E0303" => {
                Some("cannot assign while the value is borrowed — the borrow must end first".into())
            }
            "E0304" => {
                Some("cannot move while the value is borrowed — the borrow must end first".into())
            }
            _ => None,
        };
        let mut labels: Vec<SourceLabel> = span
            .map(|r| {
                vec![SourceLabel {
                    range: r,
                    message: String::new(),
                    style: LabelStyle::Primary,
                }]
            })
            .unwrap_or_default();
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
            help,
            notes: Vec::new(),
        });
    }
}

// ═══════════════════════════════════════════════════════════
// Context types
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Clone)]
struct BorrowRecord {
    /// The expression that created this borrow.
    expr_id: ExprId,
    /// The block scope depth at which this borrow was created.
    scope_depth: usize,
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

    // Borrow tracking
    shared_borrows: HashMap<Place, Vec<BorrowRecord>>,
    mutable_borrows: HashMap<Place, Vec<BorrowRecord>>,
    scope_depth: usize,

    // Escape results from escape-analysis pass (read-only)
    escaping_locals: HashSet<StmtId>,
}

impl<'a> BodyCtx<'a> {
    fn new(body_id: BodyId, body: &'a Body, escaping_locals: HashSet<StmtId>) -> Self {
        Self {
            body_id,
            body,
            source_map: &body.source_map,
            bindings: MoveBindings::default(),
            moved_places: HashSet::new(),
            moved_sites: HashMap::new(),
            shared_borrows: HashMap::new(),
            mutable_borrows: HashMap::new(),
            scope_depth: 0,
            escaping_locals,
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
        self.shared_borrows.retain(|_, records| {
            records.retain(|r| r.scope_depth <= current);
            !records.is_empty()
        });
        self.mutable_borrows.retain(|_, records| {
            records.retain(|r| r.scope_depth <= current);
            !records.is_empty()
        });
    }
    fn expr_range(&self, id: ExprId) -> Option<TextRange> {
        self.source_map.expr_ranges.get(&id).copied()
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
