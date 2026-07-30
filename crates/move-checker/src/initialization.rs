use std::collections::HashMap;

use hir::{
    HirFile,
    body::{BinaryOp, Body, Expr, ExprId, PatId, Pattern, PatternBindingId, ResolvedName, Stmt},
};
use rowan::TextRange;
use type_checker::{Diagnostic, LabelStyle, Severity, SourceLabel, Type, TypeCheckResult};

use crate::AnalysisResult;

#[derive(Clone, Copy, PartialEq, Eq)]
enum InitState {
    Uninitialized,
    Initialized,
    MaybeInitialized,
}

#[derive(Clone, Default)]
struct FlowState {
    bindings: HashMap<PatternBindingId, InitState>,
    reachable: bool,
}

impl FlowState {
    fn entry() -> Self {
        Self {
            bindings: HashMap::new(),
            reachable: true,
        }
    }

    fn merge_paths(left: Self, right: Self) -> Self {
        if !left.reachable {
            return right;
        }
        if !right.reachable {
            return left;
        }

        let mut bindings = left.bindings;
        for (id, right_state) in right.bindings {
            let state = bindings.entry(id).or_insert(right_state);
            *state = match (*state, right_state) {
                (InitState::Initialized, InitState::Initialized) => InitState::Initialized,
                (InitState::Uninitialized, InitState::Uninitialized) => InitState::Uninitialized,
                _ => InitState::MaybeInitialized,
            };
        }
        Self {
            bindings,
            reachable: true,
        }
    }
}

pub(crate) fn check(hir: &HirFile, type_result: &TypeCheckResult, result: &mut AnalysisResult) {
    for (function_id, _) in hir.item_tree.functions.iter() {
        let Some(body_id) = hir.function_bodies.get(&function_id).copied() else {
            continue;
        };
        let body = &hir.bodies[body_id];
        let mut checker = Checker {
            type_result,
            result,
            body_id,
            body,
            loop_depth: 0,
        };
        checker.analyze_expr(body.root_block, FlowState::entry());
    }
    for (const_id, _) in hir.item_tree.consts.iter() {
        let Some(body_id) = hir.const_bodies.get(&const_id).copied() else {
            continue;
        };
        let body = &hir.bodies[body_id];
        let mut checker = Checker {
            type_result,
            result,
            body_id,
            body,
            loop_depth: 0,
        };
        checker.analyze_expr(body.root_block, FlowState::entry());
    }
}

struct Checker<'a> {
    type_result: &'a TypeCheckResult,
    result: &'a mut AnalysisResult,
    body_id: hir::body::BodyId,
    body: &'a Body,
    loop_depth: usize,
}

impl Checker<'_> {
    fn analyze_expr(&mut self, expr_id: ExprId, state: FlowState) -> FlowState {
        if !state.reachable {
            return state;
        }

        let state = match self.body.exprs[expr_id].clone() {
            Expr::Missing
            | Expr::IntLiteral { .. }
            | Expr::FloatLiteral { .. }
            | Expr::StringLiteral { .. }
            | Expr::CharLiteral { .. }
            | Expr::BoolLiteral { .. } => state,

            Expr::Path { resolved, path } => {
                if let Some(ResolvedName::PatternBinding(id)) = resolved
                    && let Some(init) = state.bindings.get(&id)
                    && *init != InitState::Initialized
                {
                    self.uninitialized(path.display(), self.expr_range(expr_id));
                }
                state
            }

            Expr::Binary { lhs, rhs, op } => {
                if op == BinaryOp::Assign {
                    let state = self.analyze_expr(rhs, state);
                    let state = self.analyze_place(lhs, state);
                    self.assign(lhs, state, false)
                } else if op.is_assignment() {
                    let state = self.analyze_expr(lhs, state);
                    let state = self.analyze_expr(rhs, state);
                    let state = self.analyze_place(lhs, state);
                    self.assign(lhs, state, true)
                } else {
                    let state = self.analyze_expr(lhs, state);
                    self.analyze_expr(rhs, state)
                }
            }

            Expr::Unary { operand, .. } => self.analyze_expr(operand, state),
            Expr::Block { stmts, tail } => {
                let mut state = state;
                for stmt in stmts {
                    state = self.analyze_stmt(stmt, state);
                    if !state.reachable {
                        break;
                    }
                }
                tail.map(|tail| self.analyze_expr(tail, state.clone()))
                    .unwrap_or(state)
            }
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let state = self.analyze_expr(cond, state);
                if !state.reachable {
                    state
                } else {
                    let then_state = self.analyze_expr(then_branch, state.clone());
                    let else_state = else_branch
                        .map(|branch| self.analyze_expr(branch, state.clone()))
                        .unwrap_or(state);
                    FlowState::merge_paths(then_state, else_state)
                }
            }
            Expr::While { condition, body } => {
                self.loop_depth += 1;
                let condition_state = self.analyze_expr(condition, state);
                let body_state = if condition_state.reachable {
                    self.analyze_expr(body, condition_state.clone())
                } else {
                    condition_state.clone()
                };
                self.loop_depth -= 1;
                FlowState::merge_paths(condition_state, body_state)
            }
            Expr::For {
                pat,
                iterable,
                body,
            } => {
                let iterable_state = self.analyze_expr(iterable, state);
                if !iterable_state.reachable {
                    iterable_state
                } else {
                    self.loop_depth += 1;
                    let body_state = self.analyze_expr(body, iterable_state.clone());
                    self.loop_depth -= 1;
                    let _ = pat;
                    FlowState::merge_paths(iterable_state, body_state)
                }
            }
            Expr::Match { scrutinee, arms } => {
                let state = self.analyze_expr(scrutinee, state);
                if !state.reachable {
                    state
                } else {
                    let mut merged = FlowState {
                        bindings: HashMap::new(),
                        reachable: false,
                    };
                    for arm in arms {
                        let mut arm_state = state.clone();
                        self.bind_pattern(arm.pat, &mut arm_state);
                        if let Some(guard) = arm.guard {
                            arm_state = self.analyze_expr(guard, arm_state);
                        }
                        arm_state = self.analyze_expr(arm.body, arm_state);
                        merged = FlowState::merge_paths(merged, arm_state);
                    }
                    if merged.reachable { merged } else { state }
                }
            }
            Expr::Array { elements } | Expr::Tuple { elements } => elements
                .into_iter()
                .try_fold(state, |state, element| {
                    let state = self.analyze_expr(element, state);
                    state.reachable.then_some(state)
                })
                .unwrap_or_else(|| FlowState {
                    bindings: HashMap::new(),
                    reachable: false,
                }),
            Expr::ArrayRepeat { value, len } => {
                let state = self.analyze_expr(value, state);
                self.analyze_expr(len, state)
            }
            Expr::Struct { fields, .. } => fields
                .into_iter()
                .try_fold(state, |state, field| {
                    let state = self.analyze_expr(field.value, state);
                    state.reachable.then_some(state)
                })
                .unwrap_or_else(|| FlowState {
                    bindings: HashMap::new(),
                    reachable: false,
                }),
            Expr::Call { callee, args, .. } => {
                let state = self.analyze_expr(callee, state);
                let state = args.into_iter().try_fold(state, |state, arg| {
                    let state = self.analyze_expr(arg, state);
                    state.reachable.then_some(state)
                });
                state.unwrap_or_else(|| FlowState {
                    bindings: HashMap::new(),
                    reachable: false,
                })
            }
            Expr::Lambda { body, .. } => {
                let _ = self.analyze_expr(body, state.clone());
                state
            }
            Expr::FieldAccess { base, .. } => self.analyze_expr(base, state),
            Expr::IndexAccess { base, index } => {
                let state = self.analyze_expr(base, state);
                self.analyze_expr(index, state)
            }
            Expr::Unsafe { body } | Expr::Cast { base: body, .. } | Expr::Try { operand: body } => {
                self.analyze_expr(body, state)
            }
        };

        if state.reachable
            && self
                .type_result
                .expr_types
                .get(&(self.body_id, expr_id))
                .is_some_and(|ty| matches!(ty, Type::Never))
        {
            FlowState {
                bindings: state.bindings,
                reachable: false,
            }
        } else {
            state
        }
    }

    fn analyze_stmt(&mut self, stmt_id: hir::body::StmtId, mut state: FlowState) -> FlowState {
        if !state.reachable {
            return state;
        }
        match self.body.stmts[stmt_id].clone() {
            Stmt::Let { pat, init, .. } => {
                if let Some(init) = init {
                    let mut state = self.analyze_expr(init, state);
                    if state.reachable {
                        self.bind_pattern(pat, &mut state);
                    }
                    state
                } else {
                    self.bind_delayed_pattern(pat, &mut state);
                    state
                }
            }
            Stmt::Expr { expr } => self.analyze_expr(expr, state),
            Stmt::Return { value } => {
                let state = value
                    .map(|value| self.analyze_expr(value, state.clone()))
                    .unwrap_or(state);
                FlowState {
                    bindings: state.bindings,
                    reachable: false,
                }
            }
            Stmt::Break | Stmt::Continue => FlowState {
                bindings: state.bindings,
                reachable: false,
            },
            Stmt::Item { .. } => state,
        }
    }

    fn analyze_place(&mut self, expr_id: ExprId, state: FlowState) -> FlowState {
        match self.body.exprs[expr_id].clone() {
            Expr::Path { .. } => state,
            Expr::FieldAccess { base, .. } => self.analyze_place(base, state),
            Expr::IndexAccess { base, index } => {
                let state = self.analyze_place(base, state);
                self.analyze_expr(index, state)
            }
            Expr::Unary {
                operand,
                op: hir::body::UnaryOp::Deref,
            } => self.analyze_expr(operand, state),
            _ => self.analyze_expr(expr_id, state),
        }
    }

    fn assign(&mut self, lhs: ExprId, mut state: FlowState, compound: bool) -> FlowState {
        let Some((binding, direct)) = self.local_assignment(lhs) else {
            return state;
        };
        let Some(current) = state.bindings.get(&binding).copied() else {
            return state;
        };
        let is_mut = self.binding_is_mut(binding);
        let name = self.binding_name(binding);

        if compound {
            if current == InitState::Initialized && !is_mut {
                self.immutable_assignment(name, self.expr_range(lhs));
            }
            return state;
        }

        if !direct {
            if current != InitState::Initialized {
                self.uninitialized(name, self.expr_range(lhs));
            } else if !is_mut {
                self.immutable_assignment(name, self.expr_range(lhs));
            }
            return state;
        }

        match current {
            InitState::Uninitialized => {
                if self.loop_depth > 0 && !is_mut {
                    self.immutable_assignment(name, self.expr_range(lhs));
                }
                state.bindings.insert(binding, InitState::Initialized);
            }
            InitState::Initialized | InitState::MaybeInitialized => {
                if !is_mut {
                    self.immutable_assignment(name, self.expr_range(lhs));
                } else {
                    state.bindings.insert(binding, InitState::Initialized);
                }
            }
        }
        state
    }

    fn bind_delayed_pattern(&self, pat: PatId, state: &mut FlowState) {
        let mut ids = Vec::new();
        collect_pattern_bindings(self.body, pat, &mut ids);
        for (id, _) in ids {
            state.bindings.insert(id, InitState::Uninitialized);
        }
    }

    fn bind_pattern(&self, pat: PatId, state: &mut FlowState) {
        let mut ids = Vec::new();
        collect_pattern_bindings(self.body, pat, &mut ids);
        for (id, _) in ids {
            state.bindings.remove(&id);
        }
    }

    fn local_assignment(&self, expr_id: ExprId) -> Option<(PatternBindingId, bool)> {
        match &self.body.exprs[expr_id] {
            Expr::Path {
                resolved: Some(ResolvedName::PatternBinding(id)),
                ..
            } => Some((*id, true)),
            Expr::FieldAccess { base, .. } | Expr::IndexAccess { base, .. } => {
                self.local_assignment(*base).map(|(id, _)| (id, false))
            }
            _ => None,
        }
    }

    fn binding_is_mut(&self, id: PatternBindingId) -> bool {
        match &self.body.pats[id.pattern] {
            Pattern::Binding { is_mut, .. } => *is_mut,
            Pattern::Struct { fields, .. } => id
                .field
                .and_then(|index| fields.get(index))
                .and_then(|field| field.pat)
                .is_some_and(|pat| {
                    self.binding_is_mut(PatternBindingId {
                        pattern: pat,
                        field: None,
                    })
                }),
            _ => false,
        }
    }

    fn binding_name(&self, id: PatternBindingId) -> String {
        match &self.body.pats[id.pattern] {
            Pattern::Binding { name, .. } => name.0.clone(),
            Pattern::Struct { fields, .. } => id
                .field
                .and_then(|index| fields.get(index))
                .map(|field| field.name.0.clone())
                .unwrap_or_else(|| "_".into()),
            _ => "_".into(),
        }
    }

    fn expr_range(&self, expr_id: ExprId) -> Option<TextRange> {
        self.body.source_map.expr_ranges.get(&expr_id).copied()
    }

    fn uninitialized(&mut self, name: String, span: Option<TextRange>) {
        self.diagnostic(
            "E0059",
            format!("use of uninitialized binding `{name}`"),
            span,
        );
    }

    fn immutable_assignment(&mut self, name: String, span: Option<TextRange>) {
        self.diagnostic(
            "E0031",
            format!("cannot assign to `{name}`, as it is not declared as mutable"),
            span,
        );
    }

    fn diagnostic(&mut self, code: &'static str, message: String, span: Option<TextRange>) {
        let Some(span) = span else { return };
        let notes = match code {
            "E0059" => vec!["assign the binding on every path before reading it".into()],
            "E0031" => vec!["add `mut` to the `let` binding if reassignment is intended".into()],
            _ => Vec::new(),
        };
        self.result.diagnostics.push(Diagnostic {
            code,
            severity: Severity::Error,
            message,
            labels: vec![SourceLabel {
                range: span,
                message: String::new(),
                style: LabelStyle::Primary,
            }],
            help: None,
            notes,
        });
    }
}

fn collect_pattern_bindings(body: &Body, pat: PatId, bindings: &mut Vec<(PatternBindingId, bool)>) {
    match &body.pats[pat] {
        Pattern::Binding { is_mut, .. } => bindings.push((
            PatternBindingId {
                pattern: pat,
                field: None,
            },
            *is_mut,
        )),
        Pattern::Reference { pattern, .. } => collect_pattern_bindings(body, *pattern, bindings),
        Pattern::Tuple { elements } | Pattern::TupleStruct { elements, .. } => {
            for element in elements {
                collect_pattern_bindings(body, *element, bindings);
            }
        }
        Pattern::Struct { fields, .. } => {
            for (index, field) in fields.iter().enumerate() {
                if let Some(field_pat) = field.pat {
                    collect_pattern_bindings(body, field_pat, bindings);
                } else {
                    bindings.push((
                        PatternBindingId {
                            pattern: pat,
                            field: Some(index),
                        },
                        false,
                    ));
                }
            }
        }
        Pattern::Wildcard | Pattern::Literal(_) | Pattern::Path { .. } => {}
    }
}
