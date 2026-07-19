use std::collections::{HashMap, HashSet};

use hir::{
    HirFile,
    body::{
        BinaryOp, Body, BodyId, Expr, ExprId, PatId, Pattern, ResolvedName, Stmt, StmtId, UnaryOp,
    },
    item_tree::FunctionId,
};
use type_checker::{CaptureSource, Type, TypeCheckResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum FlowKind {
    Inherit,
    Shared,
    Mutable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SummaryOrigin {
    pub(crate) param: usize,
    pub(crate) kind: FlowKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FunctionSummary {
    pub(crate) origins: HashSet<SummaryOrigin>,
    pub(crate) opaque: bool,
}

#[derive(Debug, Default)]
pub(crate) struct ReferenceFlow {
    summaries: HashMap<FunctionId, FunctionSummary>,
}

impl ReferenceFlow {
    pub(crate) fn build(hir: &HirFile, type_result: &TypeCheckResult) -> Self {
        let mut summaries = hir
            .function_bodies
            .keys()
            .copied()
            .map(|fid| (fid, FunctionSummary::default()))
            .collect::<HashMap<_, _>>();

        loop {
            let previous = summaries.clone();
            for (fid, body_id) in &hir.function_bodies {
                let summary = SummaryAnalyzer::new(hir, type_result, &previous, *body_id)
                    .analyze_function(*fid);
                summaries.insert(*fid, summary);
            }
            if summaries == previous {
                break;
            }
        }

        Self { summaries }
    }

    pub(crate) fn summary(&self, fid: FunctionId) -> Option<&FunctionSummary> {
        self.summaries.get(&fid)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FlowValue {
    origins: HashSet<SummaryOrigin>,
    opaque: bool,
}

impl FlowValue {
    fn from_param(param: usize) -> Self {
        Self {
            origins: [SummaryOrigin {
                param,
                kind: FlowKind::Inherit,
            }]
            .into_iter()
            .collect(),
            opaque: false,
        }
    }

    fn merge(&mut self, other: Self) {
        self.origins.extend(other.origins);
        self.opaque |= other.opaque;
    }

    fn with_kind(mut self, kind: FlowKind) -> Self {
        self.origins = self
            .origins
            .into_iter()
            .map(|origin| SummaryOrigin {
                param: origin.param,
                kind,
            })
            .collect();
        self
    }
}

struct SummaryAnalyzer<'a> {
    hir: &'a HirFile,
    type_result: &'a TypeCheckResult,
    summaries: &'a HashMap<FunctionId, FunctionSummary>,
    body_id: BodyId,
    body: &'a Body,
    locals: HashMap<StmtId, FlowValue>,
    pattern_sources: Vec<HashMap<String, FlowValue>>,
    returned: FlowValue,
}

impl<'a> SummaryAnalyzer<'a> {
    fn new(
        hir: &'a HirFile,
        type_result: &'a TypeCheckResult,
        summaries: &'a HashMap<FunctionId, FunctionSummary>,
        body_id: BodyId,
    ) -> Self {
        Self {
            hir,
            type_result,
            summaries,
            body_id,
            body: &hir.bodies[body_id],
            locals: HashMap::new(),
            pattern_sources: Vec::new(),
            returned: FlowValue::default(),
        }
    }

    fn analyze_function(mut self, fid: FunctionId) -> FunctionSummary {
        let tail = self.analyze_expr(self.body.root_block);
        self.returned.merge(tail);
        let param_count = self.hir.item_tree.functions[fid].params.len();
        self.returned
            .origins
            .retain(|origin| origin.param < param_count);
        FunctionSummary {
            origins: self.returned.origins,
            opaque: self.returned.opaque,
        }
    }

    fn analyze_expr(&mut self, expr_id: ExprId) -> FlowValue {
        let expr = &self.body.exprs[expr_id];
        let mut value = match expr {
            Expr::Missing
            | Expr::IntLiteral { .. }
            | Expr::FloatLiteral { .. }
            | Expr::StringLiteral { .. }
            | Expr::CharLiteral { .. }
            | Expr::BoolLiteral { .. } => FlowValue::default(),

            Expr::Path { path, resolved } => match resolved {
                Some(ResolvedName::Param(index)) => FlowValue::from_param(*index),
                Some(ResolvedName::Local(stmt)) => {
                    self.locals.get(stmt).cloned().unwrap_or_default()
                }
                Some(ResolvedName::Unresolved) | None => path
                    .as_single_name()
                    .and_then(|name| {
                        self.pattern_sources
                            .iter()
                            .rev()
                            .find_map(|scope| scope.get(&name.0).cloned())
                    })
                    .unwrap_or_default(),
                _ => FlowValue::default(),
            },

            Expr::Unary { operand, op } => {
                let operand_value = self.analyze_expr(*operand);
                match op {
                    UnaryOp::Ref => self
                        .place_value(*operand)
                        .with_kind(FlowKind::Shared)
                        .or_opaque_reference(),
                    UnaryOp::MutRef => self
                        .place_value(*operand)
                        .with_kind(FlowKind::Mutable)
                        .or_opaque_reference(),
                    UnaryOp::Deref => operand_value,
                    _ => FlowValue::default(),
                }
            }

            Expr::Struct { fields, .. } => {
                let mut result = FlowValue::default();
                for field in fields {
                    result.merge(self.analyze_expr(field.value));
                }
                result
            }

            Expr::Array { elements } => {
                let mut result = FlowValue::default();
                for element in elements {
                    result.merge(self.analyze_expr(*element));
                }
                result
            }

            Expr::ArrayRepeat { value, len } => {
                let result = self.analyze_expr(*value);
                self.analyze_expr(*len);
                result
            }

            Expr::Binary { lhs, rhs, op } => {
                self.analyze_expr(*lhs);
                let rhs_value = self.analyze_expr(*rhs);
                if *op == BinaryOp::Assign
                    && let Some(stmt) = self.direct_local_root(*lhs)
                {
                    self.locals.insert(stmt, rhs_value);
                }
                FlowValue::default()
            }

            Expr::Block { stmts, tail } => {
                for stmt in stmts {
                    self.analyze_stmt(*stmt);
                }
                tail.map(|tail| self.analyze_expr(tail)).unwrap_or_default()
            }

            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.analyze_expr(*cond);
                let entry = self.locals.clone();

                self.locals = entry.clone();
                let then_value = self.analyze_expr(*then_branch);
                let then_locals = self.locals.clone();

                self.locals = entry.clone();
                let else_value = else_branch
                    .map(|branch| self.analyze_expr(branch))
                    .unwrap_or_default();
                let else_locals = self.locals.clone();

                self.locals = merge_locals(entry, then_locals, else_locals);
                let mut result = then_value;
                result.merge(else_value);
                result
            }

            Expr::While { condition, body } => {
                self.analyze_expr(*condition);
                let entry = self.locals.clone();
                self.analyze_expr(*body);
                self.locals = merge_two_locals(entry, self.locals.clone());
                FlowValue::default()
            }

            Expr::For {
                pat,
                iterable,
                body,
            } => {
                let iterable_value = self.analyze_expr(*iterable);
                let entry = self.locals.clone();
                self.push_pattern_sources(*pat, &iterable_value);
                self.analyze_expr(*body);
                self.pattern_sources.pop();
                self.locals = merge_two_locals(entry, self.locals.clone());
                FlowValue::default()
            }

            Expr::Match { scrutinee, arms } => {
                let scrutinee_value = self.analyze_expr(*scrutinee);
                let entry = self.locals.clone();
                let mut result = FlowValue::default();
                let mut merged_locals = entry.clone();
                for arm in arms {
                    self.locals = entry.clone();
                    self.push_pattern_sources(arm.pat, &scrutinee_value);
                    if let Some(guard) = arm.guard {
                        self.analyze_expr(guard);
                    }
                    result.merge(self.analyze_expr(arm.body));
                    self.pattern_sources.pop();
                    merged_locals = merge_two_locals(merged_locals, self.locals.clone());
                }
                self.locals = merged_locals;
                result
            }

            Expr::Call { callee, args } => self.analyze_call(*callee, args, expr_id),

            Expr::Lambda { body, .. } => {
                self.analyze_expr(*body);
                let mut result = FlowValue::default();
                if let Some(info) = self.type_result.lambda_infos.get(&(self.body_id, expr_id)) {
                    for capture in &info.captures {
                        match capture.source {
                            CaptureSource::Local(stmt) => {
                                result.merge(self.locals.get(&stmt).cloned().unwrap_or_default())
                            }
                            CaptureSource::Param(index) => {
                                result.merge(FlowValue::from_param(index));
                            }
                            CaptureSource::LambdaParam { .. } => result.opaque = true,
                        }
                    }
                }
                result
            }

            Expr::FieldAccess { base, .. } => self.analyze_expr(*base),

            Expr::IndexAccess { base, index } => {
                let result = self.analyze_expr(*base);
                self.analyze_expr(*index);
                result
            }

            Expr::Unsafe { body } => self.analyze_expr(*body),
            Expr::Cast { base, .. } => self.analyze_expr(*base),
        };

        if !self.expr_may_carry_reference(expr_id) {
            value = FlowValue::default();
        }
        value
    }

    fn analyze_stmt(&mut self, stmt_id: StmtId) {
        match &self.body.stmts[stmt_id] {
            Stmt::Let { init, .. } => {
                let value = init.map(|init| self.analyze_expr(init)).unwrap_or_default();
                self.locals.insert(stmt_id, value);
            }
            Stmt::Expr { expr } => {
                self.analyze_expr(*expr);
            }
            Stmt::Return { value } => {
                if let Some(value) = value {
                    let returned = self.analyze_expr(*value);
                    self.returned.merge(returned);
                }
            }
            Stmt::Break | Stmt::Continue | Stmt::Item { .. } => {}
        }
    }

    fn analyze_call(&mut self, callee: ExprId, args: &[ExprId], call: ExprId) -> FlowValue {
        let callee_value = self.analyze_expr(callee);
        let mut inputs = Vec::new();
        if let Expr::FieldAccess { base, .. } = &self.body.exprs[callee] {
            inputs.push(self.analyze_expr(*base));
        }
        inputs.extend(args.iter().map(|arg| self.analyze_expr(*arg)));

        if matches!(
            self.body.exprs[callee],
            Expr::Path {
                resolved: Some(ResolvedName::EnumVariant(..)),
                ..
            }
        ) {
            return merge_values(inputs);
        }

        if let Some(fid) = self.resolve_callee(callee)
            && let Some(summary) = self.summaries.get(&fid)
        {
            let mut result = FlowValue {
                origins: HashSet::new(),
                opaque: summary.opaque,
            };
            for summary_origin in &summary.origins {
                let Some(input) = inputs.get(summary_origin.param) else {
                    continue;
                };
                let input = match summary_origin.kind {
                    FlowKind::Inherit => input.clone(),
                    kind => input.clone().with_kind(kind),
                };
                result.merge(input);
            }
            return result;
        }

        if !self.expr_may_carry_reference(call) {
            return FlowValue::default();
        }
        let mut result = callee_value;
        result.merge(merge_values(inputs));
        result.opaque = true;
        result
    }

    fn resolve_callee(&self, callee: ExprId) -> Option<FunctionId> {
        match self.type_result.expr_types.get(&(self.body_id, callee)) {
            Some(Type::Function(fid)) if self.hir.function_bodies.contains_key(fid) => Some(*fid),
            _ => None,
        }
    }

    fn place_value(&self, expr_id: ExprId) -> FlowValue {
        match &self.body.exprs[expr_id] {
            Expr::Path {
                resolved: Some(ResolvedName::Param(index)),
                ..
            } => FlowValue::from_param(*index),
            Expr::Path {
                resolved: Some(ResolvedName::Local(stmt)),
                ..
            } => self.locals.get(stmt).cloned().unwrap_or_default(),
            Expr::Path { path, .. } => path
                .as_single_name()
                .and_then(|name| {
                    self.pattern_sources
                        .iter()
                        .rev()
                        .find_map(|scope| scope.get(&name.0).cloned())
                })
                .unwrap_or_default(),
            Expr::FieldAccess { base, .. } | Expr::IndexAccess { base, .. } => {
                self.place_value(*base)
            }
            Expr::Unary {
                operand,
                op: UnaryOp::Deref,
            } => self.place_value(*operand),
            _ => FlowValue::default(),
        }
    }

    fn expr_may_carry_reference(&self, expr_id: ExprId) -> bool {
        self.type_result
            .expr_types
            .get(&(self.body_id, expr_id))
            .is_none_or(|ty| type_may_carry_reference(self.hir, ty))
    }

    fn direct_local_root(&self, expr_id: ExprId) -> Option<StmtId> {
        match &self.body.exprs[expr_id] {
            Expr::Path {
                resolved: Some(ResolvedName::Local(stmt)),
                ..
            } => Some(*stmt),
            Expr::FieldAccess { base, .. } | Expr::IndexAccess { base, .. } => {
                self.direct_local_root(*base)
            }
            _ => None,
        }
    }

    fn push_pattern_sources(&mut self, pat: PatId, value: &FlowValue) {
        let mut names = HashSet::new();
        collect_pattern_bindings(self.body, pat, &mut names);
        self.pattern_sources.push(
            names
                .into_iter()
                .map(|name| (name, value.clone()))
                .collect(),
        );
    }
}

trait OpaqueReference {
    fn or_opaque_reference(self) -> Self;
}

impl OpaqueReference for FlowValue {
    fn or_opaque_reference(mut self) -> Self {
        if self.origins.is_empty() {
            self.opaque = true;
        }
        self
    }
}

fn collect_pattern_bindings(body: &Body, pat: PatId, names: &mut HashSet<String>) {
    match &body.pats[pat] {
        Pattern::Binding { name } => {
            names.insert(name.0.clone());
        }
        Pattern::Tuple { elements } | Pattern::TupleStruct { elements, .. } => {
            for element in elements {
                collect_pattern_bindings(body, *element, names);
            }
        }
        Pattern::Struct { fields, .. } => {
            for field in fields {
                if let Some(pat) = field.pat {
                    collect_pattern_bindings(body, pat, names);
                } else {
                    names.insert(field.name.0.clone());
                }
            }
        }
        Pattern::Wildcard | Pattern::Literal(_) | Pattern::Path { .. } => {}
    }
}

fn merge_values(values: impl IntoIterator<Item = FlowValue>) -> FlowValue {
    let mut result = FlowValue::default();
    for value in values {
        result.merge(value);
    }
    result
}

fn merge_two_locals(
    mut left: HashMap<StmtId, FlowValue>,
    right: HashMap<StmtId, FlowValue>,
) -> HashMap<StmtId, FlowValue> {
    for (stmt, value) in right {
        left.entry(stmt).or_default().merge(value);
    }
    left
}

fn merge_locals(
    entry: HashMap<StmtId, FlowValue>,
    left: HashMap<StmtId, FlowValue>,
    right: HashMap<StmtId, FlowValue>,
) -> HashMap<StmtId, FlowValue> {
    merge_two_locals(merge_two_locals(entry, left), right)
}

pub(crate) fn type_may_carry_reference(hir: &HirFile, ty: &Type) -> bool {
    match ty {
        Type::Ref(..) => true,
        Type::Ptr { .. } => false,
        Type::Tuple(elements) => elements
            .iter()
            .any(|element| type_may_carry_reference(hir, element)),
        Type::Array(inner, _) => type_may_carry_reference(hir, inner),
        Type::Struct(id, args) => {
            args.iter().any(|arg| type_may_carry_reference(hir, arg))
                || hir.item_tree.structs[*id]
                    .fields
                    .iter()
                    .any(|field| hir_type_may_carry_reference(&field.ty))
        }
        Type::Enum(id, args) => {
            args.iter().any(|arg| type_may_carry_reference(hir, arg))
                || hir.item_tree.enums[*id]
                    .variants
                    .iter()
                    .any(|variant| match &variant.kind {
                        hir::item_tree::HirVariantKind::Unit => false,
                        hir::item_tree::HirVariantKind::Tuple(fields) => {
                            fields.iter().any(hir_type_may_carry_reference)
                        }
                        hir::item_tree::HirVariantKind::Struct(fields) => fields
                            .iter()
                            .any(|field| hir_type_may_carry_reference(&field.ty)),
                    })
        }
        Type::Param(..) | Type::InferVar(..) | Type::Unknown | Type::Error => true,
        Type::Function(..) => false,
        Type::Fn { params, ret, .. } => {
            params
                .iter()
                .any(|param| type_may_carry_reference(hir, param))
                || type_may_carry_reference(hir, ret)
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

fn hir_type_may_carry_reference(ty: &hir::item_tree::HirTypeRef) -> bool {
    match ty {
        hir::item_tree::HirTypeRef::Ref(..) => true,
        hir::item_tree::HirTypeRef::Ptr { .. } => false,
        hir::item_tree::HirTypeRef::Tuple(elements) => {
            elements.iter().any(hir_type_may_carry_reference)
        }
        hir::item_tree::HirTypeRef::Array(inner, _) => hir_type_may_carry_reference(inner),
        hir::item_tree::HirTypeRef::Function { params, ret, .. } => {
            params.iter().any(hir_type_may_carry_reference) || hir_type_may_carry_reference(ret)
        }
        hir::item_tree::HirTypeRef::Named(path) => {
            path.type_args.iter().any(hir_type_may_carry_reference)
        }
        hir::item_tree::HirTypeRef::Const(_)
        | hir::item_tree::HirTypeRef::Unknown
        | hir::item_tree::HirTypeRef::Error => false,
    }
}
