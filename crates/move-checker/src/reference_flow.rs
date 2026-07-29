use std::collections::{HashMap, HashSet};

use hir::{
    HirFile,
    body::{
        BinaryOp, Body, BodyId, Expr, ExprId, PatId, Pattern, PatternBindingId, ResolvedName, Stmt,
        StmtId, UnaryOp,
    },
    item_tree::FunctionId,
};
use type_checker::{CaptureSource, PatternBindingMode, Type, TypeCheckResult};

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
    pub(crate) fields: Vec<FunctionSummary>,
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
            .filter(|fid| !is_std_builtin(hir, *fid))
            .map(|fid| (fid, FunctionSummary::default()))
            .collect::<HashMap<_, _>>();

        loop {
            let previous = summaries.clone();
            for (fid, body_id) in &hir.function_bodies {
                if is_std_builtin(hir, *fid) {
                    continue;
                }
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

fn is_std_builtin(hir: &HirFile, fid: FunctionId) -> bool {
    hir.std_loaded
        && hir
            .package_for_range(hir.item_tree.functions[fid].name_range)
            .is_none()
        && hir.item_tree.functions[fid]
            .attrs
            .iter()
            .any(|attr| attr.name.0 == "builtin")
}

type FlowValue = FunctionSummary;

impl FunctionSummary {
    fn from_param(param: usize) -> Self {
        Self {
            origins: [SummaryOrigin {
                param,
                kind: FlowKind::Inherit,
            }]
            .into_iter()
            .collect(),
            opaque: false,
            fields: Vec::new(),
        }
    }

    fn merge(&mut self, other: Self) {
        if self.is_empty() {
            *self = other;
            return;
        }
        if other.is_empty() {
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
        self.fields = self
            .fields
            .into_iter()
            .map(|field| field.with_kind(kind))
            .collect();
        self
    }

    fn from_fields(fields: Vec<Self>) -> Self {
        let mut value = Self::default();
        for field in &fields {
            value.origins.extend(field.origins.iter().copied());
            value.opaque |= field.opaque;
        }
        value.fields = fields;
        value
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
        merge_values(self.fields.iter().cloned())
    }

    fn flattened(&self) -> Self {
        Self {
            origins: self.origins.clone(),
            opaque: self.opaque,
            fields: Vec::new(),
        }
    }

    fn retain_params(&mut self, param_count: usize) {
        self.origins.retain(|origin| origin.param < param_count);
        for field in &mut self.fields {
            field.retain_params(param_count);
        }
    }

    fn is_empty(&self) -> bool {
        self.origins.is_empty() && !self.opaque && self.fields.is_empty()
    }
}

struct SummaryAnalyzer<'a> {
    hir: &'a HirFile,
    type_result: &'a TypeCheckResult,
    summaries: &'a HashMap<FunctionId, FunctionSummary>,
    body_id: BodyId,
    body: &'a Body,
    /// Provenance per binding. `let`, `match` arms and `for` all land here —
    /// `PatternBindingId` is unique per pattern site, so one flat map suffices.
    locals: HashMap<PatternBindingId, FlowValue>,
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
            returned: FlowValue::default(),
        }
    }

    fn analyze_function(mut self, fid: FunctionId) -> FunctionSummary {
        let tail = self.analyze_expr(self.body.root_block);
        self.returned.merge(tail);
        let param_count = self.hir.item_tree.functions[fid].params.len();
        self.returned.retain_params(param_count);
        self.returned
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

            Expr::Path { resolved, .. } => match resolved {
                Some(ResolvedName::Param(index)) => FlowValue::from_param(*index),
                Some(ResolvedName::PatternBinding(id)) => {
                    self.locals.get(id).cloned().unwrap_or_default()
                }
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
                merge_values(fields.iter().map(|field| self.analyze_expr(field.value)))
            }

            Expr::Array { elements } | Expr::Tuple { elements } => FlowValue::from_fields(
                elements
                    .iter()
                    .map(|element| self.analyze_expr(*element))
                    .collect(),
            ),

            Expr::ArrayRepeat { value, len } => {
                let result = self.analyze_expr(*value);
                self.analyze_expr(*len);
                result
            }

            Expr::Binary { lhs, rhs, op } => {
                self.analyze_expr(*lhs);
                let rhs_value = self.analyze_expr(*rhs);
                if *op == BinaryOp::Assign
                    && let Some((binding, direct)) = self.local_assignment(*lhs)
                {
                    if direct {
                        self.locals.insert(binding, rhs_value);
                    } else {
                        self.locals.entry(binding).or_default().merge(rhs_value);
                    }
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
                let iterable_value = self.analyze_expr(*iterable).iterated();
                let entry = self.locals.clone();
                self.bind_pattern_sources(*pat, &iterable_value);
                self.analyze_expr(*body);
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
                    self.bind_pattern_sources(arm.pat, &scrutinee_value);
                    if let Some(guard) = arm.guard {
                        self.analyze_expr(guard);
                    }
                    result.merge(self.analyze_expr(arm.body));
                    merged_locals = merge_two_locals(merged_locals, self.locals.clone());
                }
                self.locals = merged_locals;
                result
            }

            Expr::Call { callee, args, .. } => self.analyze_call(*callee, args, expr_id),

            Expr::Lambda { body, .. } => {
                self.analyze_expr(*body);
                let mut result = FlowValue::default();
                if let Some(info) = self.type_result.lambda_infos.get(&(self.body_id, expr_id)) {
                    for capture in &info.captures {
                        match capture.source {
                            CaptureSource::Param(index) => {
                                result.merge(FlowValue::from_param(index));
                            }
                            CaptureSource::Pattern(id) => {
                                result.merge(self.locals.get(&id).cloned().unwrap_or_default())
                            }
                            CaptureSource::LambdaParam { .. } => result.opaque = true,
                        }
                    }
                }
                result
            }

            Expr::FieldAccess { base, field } => {
                let value = self.analyze_expr(*base);
                self.field_index(*base, field)
                    .map(|index| value.project(index))
                    .unwrap_or_else(|| value.flattened())
            }

            Expr::IndexAccess { base, index } => {
                let value = self.analyze_expr(*base);
                self.analyze_expr(*index);
                match &self.body.exprs[*index] {
                    Expr::IntLiteral { value: index, .. } => usize::try_from(*index)
                        .ok()
                        .map(|index| value.project(index))
                        .unwrap_or_else(|| value.iterated()),
                    _ => value.iterated(),
                }
            }

            Expr::Unsafe { body } => self.analyze_expr(*body),
            Expr::Cast { base, .. } => self.analyze_expr(*base),
        };

        if !self.expr_may_carry_provenance(expr_id) {
            value = FlowValue::default();
        }
        value
    }

    fn analyze_stmt(&mut self, stmt_id: StmtId) {
        match &self.body.stmts[stmt_id] {
            Stmt::Let { pat, init, .. } => {
                let (pat, init) = (*pat, *init);
                let value = init.map(|init| self.analyze_expr(init)).unwrap_or_default();
                self.bind_pattern_sources(pat, &value);
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
            return FlowValue::from_fields(inputs);
        }

        if let Some(fid) = self.resolve_callee(callee)
            && let Some(summary) = self.summaries.get(&fid)
        {
            return instantiate_summary(summary, &inputs);
        }

        if !self.expr_may_carry_provenance(call) {
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
                resolved: Some(ResolvedName::PatternBinding(id)),
                ..
            } => self.locals.get(id).cloned().unwrap_or_default(),
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

    fn expr_may_carry_provenance(&self, expr_id: ExprId) -> bool {
        self.type_result
            .expr_types
            .get(&(self.body_id, expr_id))
            .is_none_or(|ty| type_may_carry_provenance(self.hir, ty))
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

    fn bind_pattern_sources(&mut self, pat: PatId, value: &FlowValue) {
        match &self.body.pats[pat] {
            Pattern::Binding { .. } => {
                let binding = PatternBindingId {
                    pattern: pat,
                    field: None,
                };
                self.locals
                    .insert(binding, self.pattern_binding_value(binding, value));
            }
            Pattern::Reference { pattern, .. } => {
                self.bind_pattern_sources(*pattern, value);
            }
            Pattern::Tuple { elements } | Pattern::TupleStruct { elements, .. } => {
                for (index, element) in elements.iter().enumerate() {
                    self.bind_pattern_sources(*element, &value.project(index));
                }
            }
            Pattern::Struct { fields, .. } => {
                for (binding_index, field) in fields.iter().enumerate() {
                    let Some(index) = self.pattern_field_index(pat, &field.name) else {
                        continue;
                    };
                    let field_value = value.project(index);
                    if let Some(field_pat) = field.pat {
                        self.bind_pattern_sources(field_pat, &field_value);
                    } else {
                        let binding = PatternBindingId {
                            pattern: pat,
                            field: Some(binding_index),
                        };
                        self.locals
                            .insert(binding, self.pattern_binding_value(binding, &field_value));
                    }
                }
            }
            Pattern::Wildcard | Pattern::Literal(_) | Pattern::Path { .. } => {}
        }
    }

    fn pattern_binding_value(&self, binding: PatternBindingId, value: &FlowValue) -> FlowValue {
        match self
            .type_result
            .pattern_binding_modes
            .get(&(self.body_id, binding))
            .copied()
            .unwrap_or(PatternBindingMode::Move)
        {
            PatternBindingMode::Ref => value
                .clone()
                .with_kind(FlowKind::Shared)
                .or_opaque_reference(),
            PatternBindingMode::RefMut => value
                .clone()
                .with_kind(FlowKind::Mutable)
                .or_opaque_reference(),
            PatternBindingMode::Move
                if self
                    .type_result
                    .pattern_binding_types
                    .get(&(self.body_id, binding))
                    .is_none_or(|ty| type_may_carry_provenance(self.hir, ty)) =>
            {
                value.clone()
            }
            PatternBindingMode::Move => FlowValue::default(),
        }
    }

    fn field_index(&self, base: ExprId, field: &hir::Name) -> Option<usize> {
        match self.type_result.expr_types.get(&(self.body_id, base))? {
            Type::Ref(inner, _) => self.field_index_for_type(inner, field),
            ty => self.field_index_for_type(ty, field),
        }
    }

    fn field_index_for_type(&self, ty: &Type, field: &hir::Name) -> Option<usize> {
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

    fn pattern_field_index(&self, pat: PatId, field: &hir::Name) -> Option<usize> {
        match self.type_result.pattern_types.get(&(self.body_id, pat))? {
            Type::Struct(id, _) => self.hir.item_tree.structs[*id]
                .fields
                .iter()
                .position(|item| item.name == *field),
            Type::Enum(id, _) => {
                let Pattern::Struct { path, .. } = &self.body.pats[pat] else {
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

fn merge_values(values: impl IntoIterator<Item = FlowValue>) -> FlowValue {
    let mut result = FlowValue::default();
    for value in values {
        result.merge(value);
    }
    result
}

fn instantiate_summary(summary: &FunctionSummary, inputs: &[FlowValue]) -> FlowValue {
    let mut result = FlowValue::default();
    for origin in &summary.origins {
        let Some(input) = inputs.get(origin.param) else {
            continue;
        };
        result.merge(match origin.kind {
            FlowKind::Inherit => input.clone(),
            kind => input.clone().with_kind(kind),
        });
    }
    if !summary.fields.is_empty() {
        result.fields = summary
            .fields
            .iter()
            .map(|field| instantiate_summary(field, inputs))
            .collect();
    }
    if summary.opaque {
        result.merge(merge_values(inputs.iter().cloned()));
        result.opaque = true;
    }
    result
}

fn merge_two_locals(
    mut left: HashMap<PatternBindingId, FlowValue>,
    right: HashMap<PatternBindingId, FlowValue>,
) -> HashMap<PatternBindingId, FlowValue> {
    for (binding, value) in right {
        left.entry(binding).or_default().merge(value);
    }
    left
}

fn merge_locals(
    entry: HashMap<PatternBindingId, FlowValue>,
    left: HashMap<PatternBindingId, FlowValue>,
    right: HashMap<PatternBindingId, FlowValue>,
) -> HashMap<PatternBindingId, FlowValue> {
    merge_two_locals(merge_two_locals(entry, left), right)
}

pub(crate) fn type_may_carry_reference(hir: &HirFile, ty: &Type) -> bool {
    type_may_carry_flow(hir, ty, false)
}

fn type_may_carry_provenance(hir: &HirFile, ty: &Type) -> bool {
    type_may_carry_flow(hir, ty, true)
}

fn type_may_carry_flow(hir: &HirFile, ty: &Type, through_raw_pointer: bool) -> bool {
    match ty {
        Type::Ref(..) => true,
        Type::Ptr { .. } => through_raw_pointer,
        Type::Tuple(elements) => elements
            .iter()
            .any(|element| type_may_carry_flow(hir, element, through_raw_pointer)),
        Type::Slice(inner) | Type::Array(inner, _) => {
            type_may_carry_flow(hir, inner, through_raw_pointer)
        }
        Type::Struct(id, args) => {
            args.iter()
                .any(|arg| type_may_carry_flow(hir, arg, through_raw_pointer))
                || hir.item_tree.structs[*id]
                    .fields
                    .iter()
                    .any(|field| hir_type_may_carry_flow(&field.ty, through_raw_pointer))
        }
        Type::Enum(id, args) => {
            args.iter()
                .any(|arg| type_may_carry_flow(hir, arg, through_raw_pointer))
                || hir.item_tree.enums[*id]
                    .variants
                    .iter()
                    .any(|variant| match &variant.kind {
                        hir::item_tree::HirVariantKind::Unit => false,
                        hir::item_tree::HirVariantKind::Tuple(fields) => fields
                            .iter()
                            .any(|field| hir_type_may_carry_flow(field, through_raw_pointer)),
                        hir::item_tree::HirVariantKind::Struct(fields) => fields
                            .iter()
                            .any(|field| hir_type_may_carry_flow(&field.ty, through_raw_pointer)),
                    })
        }
        Type::Param(..) | Type::InferVar(..) | Type::Unknown | Type::Error => true,
        Type::Function(..) => false,
        Type::Fn { params, ret, .. } => {
            params
                .iter()
                .any(|param| type_may_carry_flow(hir, param, through_raw_pointer))
                || type_may_carry_flow(hir, ret, through_raw_pointer)
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

fn hir_type_may_carry_flow(ty: &hir::item_tree::HirTypeRef, through_raw_pointer: bool) -> bool {
    match ty {
        hir::item_tree::HirTypeRef::Ref(..) => true,
        hir::item_tree::HirTypeRef::Ptr { .. } => through_raw_pointer,
        hir::item_tree::HirTypeRef::Tuple(elements) => elements
            .iter()
            .any(|element| hir_type_may_carry_flow(element, through_raw_pointer)),
        hir::item_tree::HirTypeRef::Slice(inner) | hir::item_tree::HirTypeRef::Array(inner, _) => {
            hir_type_may_carry_flow(inner, through_raw_pointer)
        }
        hir::item_tree::HirTypeRef::Function { params, ret, .. } => {
            params
                .iter()
                .any(|param| hir_type_may_carry_flow(param, through_raw_pointer))
                || hir_type_may_carry_flow(ret, through_raw_pointer)
        }
        hir::item_tree::HirTypeRef::Named(path) => path
            .type_args
            .iter()
            .any(|arg| hir_type_may_carry_flow(arg, through_raw_pointer)),
        hir::item_tree::HirTypeRef::Never
        | hir::item_tree::HirTypeRef::Const(_)
        | hir::item_tree::HirTypeRef::Unknown
        | hir::item_tree::HirTypeRef::Error => false,
    }
}
