use rowan::TextRange;

use hir::{
    HirFile,
    body::BodyId,
    item_tree::{FunctionId, HirFunction, HirTypeRef, StructId},
};

use crate::{
    context::BodyCtx,
    result::{Diagnostic, LabelStyle, Severity, SourceLabel, TypeCheckResult},
    trait_env::{TraitAssocConstraint, TraitBound},
    types::{FloatTy, IntTy, Type},
};

pub struct TypeChecker<'a> {
    pub(crate) hir: &'a HirFile,
    pub(crate) result: TypeCheckResult,
    pub(crate) generic_edges: Vec<GenericEdge>,
}

pub(crate) struct GenericEdge {
    pub(crate) caller: FunctionId,
    pub(crate) callee: FunctionId,
    pub(crate) grows: bool,
    pub(crate) span: Option<TextRange>,
}

pub fn check_hir(hir: &HirFile) -> TypeCheckResult {
    TypeChecker::new(hir).check()
}

impl<'a> TypeChecker<'a> {
    pub fn new(hir: &'a HirFile) -> Self {
        Self {
            hir,
            result: TypeCheckResult::default(),
            generic_edges: Vec::new(),
        }
    }

    pub fn check(mut self) -> TypeCheckResult {
        self.check_struct_layouts();
        self.check_traits();
        self.check_impls();
        self.build_trait_env();
        self.check_function_bodies();
        self.check_generic_recursion();
        self.result
    }

    pub(crate) fn build_trait_env(&mut self) {
        for (tid, tr) in self.hir.item_tree.traits.iter() {
            if tr
                .attrs
                .iter()
                .any(|attr| attr.name.0 == "lang" && attr.value.as_deref() == Some("copy"))
            {
                self.result.trait_env.set_copy_trait(tid);
                break;
            }
        }
        for (_, imp) in self.hir.item_tree.impls.iter() {
            let Some(trait_ty) = &imp.trait_ty else {
                continue;
            };
            let Some(trait_id) = self.resolve_trait_ref(trait_ty) else {
                continue;
            };
            let params = crate::lowering::generic_param_map_with_consts(
                imp.generics.iter().map(|name| name.0.as_str()),
                imp.const_generics.iter().map(|name| name.0.as_str()),
            );
            let self_ty = self.lower_type_ref_with_params(&imp.self_ty, &params);
            let bounds = self.lower_trait_env_bounds(&imp.generic_bounds, &params);
            let assoc_types = imp
                .type_aliases
                .iter()
                .filter_map(|alias_id| {
                    let alias = &self.hir.item_tree.type_aliases[*alias_id];
                    alias.ty.as_ref().map(|ty| {
                        (
                            alias.name.0.clone(),
                            self.lower_type_ref_with_params(ty, &params),
                        )
                    })
                })
                .collect();
            self.result
                .trait_env
                .insert_impl(trait_id, self_ty, bounds, assoc_types);
        }
    }

    fn lower_trait_env_bounds(
        &mut self,
        bounds: &[hir::item_tree::HirGenericBound],
        params: &std::collections::HashMap<String, Type>,
    ) -> Vec<TraitBound> {
        bounds
            .iter()
            .filter_map(|bound| {
                let trait_id = self.resolve_trait_ref(&bound.trait_ty)?;
                Some(TraitBound {
                    ty: self.lower_type_ref_with_params(&bound.target_ty, params),
                    trait_id,
                    assoc_constraints: bound
                        .assoc_constraints
                        .iter()
                        .map(|constraint| TraitAssocConstraint {
                            name: constraint.name.0.clone(),
                            ty: self.lower_type_ref_with_params(&constraint.ty, params),
                        })
                        .collect(),
                })
            })
            .collect()
    }

    pub(crate) fn check_function_bodies(&mut self) {
        for (fid, function) in self.hir.item_tree.functions.iter() {
            if let Some(body_id) = self.hir.function_bodies.get(&fid).copied() {
                let outer_generics = self.impl_generic_names(fid);
                let outer_const_generics = self.impl_const_generic_names(fid);
                self.check_function(fid, function, body_id, outer_generics, outer_const_generics);
            }
        }
    }

    pub(crate) fn check_function(
        &mut self,
        function_id: FunctionId,
        function: &HirFunction,
        body_id: BodyId,
        outer_generics: Vec<String>,
        outer_const_generics: Vec<String>,
    ) {
        let body = &self.hir.bodies[body_id];
        let mut params = crate::lowering::generic_param_map_with_consts(
            outer_generics
                .iter()
                .map(String::as_str)
                .chain(function.generics.iter().map(|name| name.0.as_str())),
            outer_const_generics
                .iter()
                .map(String::as_str)
                .chain(function.const_generics.iter().map(|name| name.0.as_str())),
        );
        if let Some(self_ty_ref) = self.impl_self_ty_ref(function_id).cloned() {
            let self_ty = self.lower_type_ref_with_params(&self_ty_ref, &params);
            params.insert("Self".into(), self_ty);
        }
        let return_ty = function
            .ret_type
            .as_ref()
            .map(|ty| self.lower_type_ref_with_params(ty, &params))
            .unwrap_or(Type::Unit);
        let mut ctx = BodyCtx::new(
            body_id,
            body,
            function_id,
            function,
            return_ty.clone(),
            params,
        );
        self.check_type_bounds(&ctx, &return_ty, None);
        for param in &function.params {
            let param_ty = self.lower_type_ref_with_params(&param.ty, &ctx.generic_params);
            self.check_type_bounds(&ctx, &param_ty, None);
        }
        let actual = self.check_expr_expected(&mut ctx, body.root_block, &return_ty);

        // Only check tail-type compatibility; return-statement-only functions
        // are validated inside check_stmt when the return is encountered.
        // ponytail: full return-path analysis needed to catch missing returns
        if self.has_tail(body, body.root_block) && !actual.is_never() {
            self.expect_assignable(
                &return_ty,
                &actual,
                "function return type",
                ctx.expr_range(body.root_block),
            );
        }
    }

    pub(crate) fn impl_generic_names(&self, function_id: FunctionId) -> Vec<String> {
        self.hir
            .item_tree
            .impls
            .iter()
            .find_map(|(_, imp)| {
                imp.methods
                    .contains(&function_id)
                    .then(|| imp.generics.iter().map(|name| name.0.clone()).collect())
            })
            .unwrap_or_default()
    }

    pub(crate) fn impl_const_generic_names(&self, function_id: FunctionId) -> Vec<String> {
        self.hir
            .item_tree
            .impls
            .iter()
            .find_map(|(_, imp)| {
                imp.methods.contains(&function_id).then(|| {
                    imp.const_generics
                        .iter()
                        .map(|name| name.0.clone())
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    pub(crate) fn impl_self_ty_ref(&self, function_id: FunctionId) -> Option<&HirTypeRef> {
        self.hir
            .item_tree
            .impls
            .iter()
            .find_map(|(_, imp)| imp.methods.contains(&function_id).then_some(&imp.self_ty))
    }

    pub(crate) fn find_lang_trait(&self, lang: &str) -> Option<hir::item_tree::TraitId> {
        self.hir.item_tree.traits.iter().find_map(|(id, tr)| {
            tr.attrs
                .iter()
                .any(|attr| attr.name.0 == "lang" && attr.value.as_deref() == Some(lang))
                .then_some(id)
        })
    }

    pub(crate) fn trait_lang(&self, trait_id: hir::item_tree::TraitId) -> Option<&str> {
        self.hir.item_tree.traits[trait_id]
            .attrs
            .iter()
            .find_map(|attr| {
                (attr.name.0 == "lang")
                    .then(|| attr.value.as_deref())
                    .flatten()
            })
    }

    fn has_tail(&self, body: &hir::body::Body, expr: hir::body::ExprId) -> bool {
        matches!(
            &body.exprs[expr],
            hir::body::Expr::Block { tail: Some(_), .. }
        )
    }

    fn check_struct_layouts(&mut self) {
        let structs = self
            .hir
            .item_tree
            .structs
            .iter()
            .map(|(id, strukt)| {
                (
                    id,
                    strukt.name.0.clone(),
                    strukt.name_range,
                    strukt.fields.clone(),
                )
            })
            .collect::<Vec<_>>();

        for (id, name, name_range, fields) in structs {
            if let Some(field_range) = fields.iter().find_map(|field| {
                self.type_ref_contains_inline_struct(&field.ty, id, &mut Vec::new())
                    .then_some(field.ty_range)
            }) {
                self.diagnostic(
                    "E0072",
                    format!("recursive type `{name}` has infinite size"),
                    Some(name_range),
                );
                self.result.diagnostics.last_mut().unwrap().labels.push(
                    crate::result::SourceLabel {
                        range: field_range,
                        message: "recursive field".into(),
                        style: crate::result::LabelStyle::Secondary,
                    },
                );
            }
        }
    }

    fn type_ref_contains_inline_struct(
        &self,
        ty: &HirTypeRef,
        target: StructId,
        seen: &mut Vec<StructId>,
    ) -> bool {
        match ty {
            HirTypeRef::Named(path) => {
                let Some(name) = path.as_single_name().map(|name| name.0.as_str()) else {
                    return false;
                };
                let Some((sid, strukt)) = self
                    .hir
                    .item_tree
                    .structs
                    .iter()
                    .find(|(_, strukt)| strukt.name.0 == name)
                else {
                    return false;
                };
                if sid == target {
                    return true;
                }
                if seen.contains(&sid) {
                    return false;
                }
                seen.push(sid);
                let found = strukt
                    .fields
                    .iter()
                    .any(|field| self.type_ref_contains_inline_struct(&field.ty, target, seen));
                seen.pop();
                found
            }
            HirTypeRef::Tuple(elements) => elements
                .iter()
                .any(|ty| self.type_ref_contains_inline_struct(ty, target, seen)),
            HirTypeRef::Array(inner, _) => {
                self.type_ref_contains_inline_struct(inner, target, seen)
            }
            HirTypeRef::Const(_) => false,
            HirTypeRef::Ref(_, _) | HirTypeRef::Ptr { .. } => false,
            HirTypeRef::Unknown | HirTypeRef::Error => false,
        }
    }

    fn check_generic_recursion(&mut self) {
        for i in 0..self.generic_edges.len() {
            if !self.generic_edges[i].grows {
                continue;
            }
            let caller = self.generic_edges[i].caller;
            let callee = self.generic_edges[i].callee;
            if !self.reaches(callee, caller) {
                continue;
            }

            let callee_name = self.hir.item_tree.functions[callee].name.0.clone();
            self.diagnostic(
                "E0033",
                format!("generic recursion grows type arguments while calling `{callee_name}`"),
                self.generic_edges[i].span,
            );
        }
    }

    fn reaches(&self, from: FunctionId, target: FunctionId) -> bool {
        let mut seen = Vec::new();
        let mut stack = vec![from];

        while let Some(next) = stack.pop() {
            if next == target {
                return true;
            }
            if seen.contains(&next) {
                continue;
            }
            seen.push(next);
            stack.extend(
                self.generic_edges
                    .iter()
                    .filter_map(|edge| (edge.caller == next).then_some(edge.callee)),
            );
        }

        false
    }

    pub(crate) fn join_branch_types(
        &mut self,
        lhs: Type,
        rhs: Type,
        context: &str,
        span: Option<TextRange>,
    ) -> Type {
        if lhs.is_never() {
            return rhs;
        }
        if rhs.is_never() {
            return lhs;
        }
        if lhs.is_unknown_like() {
            return rhs;
        }
        if rhs.is_unknown_like() {
            return lhs;
        }
        if let Some(ty) = self.join_numeric_types(&lhs, &rhs) {
            ty
        } else if lhs == rhs {
            lhs
        } else {
            self.diagnostic(
                "E0002",
                format!(
                    "{} have incompatible types: {} and {}",
                    context,
                    lhs.display(self.hir),
                    rhs.display(self.hir)
                ),
                span,
            );
            Type::Error
        }
    }

    pub(crate) fn expect_numeric(&mut self, ty: &Type, context: &str, span: Option<TextRange>) {
        if ty.is_unknown_like() || ty.is_numeric() {
            return;
        }
        self.diagnostic(
            "E0003",
            format!("{} must be numeric, got {}", context, ty.display(self.hir)),
            span,
        );
    }

    pub(crate) fn expect_assignable(
        &mut self,
        expected: &Type,
        actual: &Type,
        context: &str,
        span: Option<TextRange>,
    ) {
        if expected.is_unknown_like()
            || actual.is_unknown_like()
            || expected == actual
            || self.numeric_assignable(expected, actual)
            || self.structural_assignable(expected, actual)
        {
            return;
        }
        if actual.is_never() {
            return;
        }
        // str → &str coercion (unsized coercion for string literals)
        if matches!(actual, Type::Str)
            && matches!(expected, Type::Ref(inner, _) if matches!(**inner, Type::Str))
        {
            return;
        }
        self.diagnostic(
            "E0001",
            format!(
                "{} type mismatch: expected {}, got {}",
                context,
                expected.display(self.hir),
                actual.display(self.hir)
            ),
            span,
        );
    }

    pub(crate) fn diagnostic(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
        span: Option<TextRange>,
    ) {
        let help: Option<String> = match code {
            "E0001" => Some("expected one type but found another — consider an explicit type annotation or cast".into()),
            "E0002" => Some("all branches must produce values of the same type — consider ensuring both branches return compatible types".into()),
            "E0003" => Some("this operation requires a numeric or `char` type".into()),
            "E0004" => Some("this value is not callable — only functions can be called".into()),
            "E0005" => Some("the number of arguments does not match the function's parameter count".into()),
            "E0006" => Some("this field does not exist on the struct — check the struct definition for available fields".into()),
            "E0007" => Some("a required field is missing from the struct literal — add the missing field".into()),
            "E0008" => Some("this type does not support this operation — only references can be dereferenced, only arrays can be indexed".into()),
            "E0009" => Some("this path does not resolve to a struct type — check the type definition".into()),
            "E0010" => Some("the pattern does not match the expected type — ensure tuple element counts match".into()),
            "E0011" => Some("this numeric suffix is not recognized — valid suffixes are i8, i16, i32, i64, u8, u16, u32, u64, f32, f64".into()),
            "E0012" => Some("this type cast is not supported between the source and target types".into()),
            "E0013" => Some("this method does not exist for the receiver type — check the impl block and receiver type".into()),
            "E0020" | "E0024" => Some("duplicate definition — remove the duplicate".into()),
            "E0031" => Some("cannot assign twice to an immutable variable — add `mut` to the `let` binding".into()),
            "E0033" => Some("recursive generic calls must reuse the same type arguments; wrapping them requires infinitely many instantiations".into()),
            "E0035" => Some("the inferred type must implement every trait bound on the generic parameter".into()),
            "E0036" => Some("add the required comparison trait impl for this type".into()),
            "E0037" => Some("make impl where-clause bounds structurally smaller than the implemented type".into()),
            "E0072" => Some("insert some indirection, such as `&`, `*const`, or `*mut`, to break the cycle".into()),
            "E0021" => Some("trait method declarations should not have a body".into()),
            "E0022" | "E0025" => Some("duplicate associated type — remove the duplicate".into()),
            "E0023" => Some("this trait is not defined or not in scope".into()),
            "E0026" => Some("this method is required by the trait — add an implementation".into()),
            "E0027" => Some("this associated type is required by the trait — add a type definition".into()),
            "E0028" | "E0029" | "E0030" => Some("the method signature must exactly match the trait declaration — check parameter count, types, and return type".into()),
            _ => None,
        };
        self.result.diagnostics.push(Diagnostic {
            code,
            severity: Severity::Error,
            message: message.into(),
            labels: span
                .map(|r| {
                    vec![SourceLabel {
                        range: r,
                        message: String::new(),
                        style: LabelStyle::Primary,
                    }]
                })
                .unwrap_or_default(),
            help,
            notes: Vec::new(),
        });
    }

    pub(crate) fn int_literal_type(
        &mut self,
        suffix: Option<&str>,
        expected: Option<&Type>,
    ) -> Type {
        if let Some(suffix) = suffix {
            return IntTy::parse(suffix).map(Type::Int).unwrap_or_else(|| {
                self.diagnostic(
                    "E0011",
                    format!("unknown integer literal suffix `{suffix}`"),
                    None,
                );
                Type::Error
            });
        }
        match expected {
            Some(Type::Int(ty)) => Type::Int(*ty),
            Some(Type::InferInt) => Type::InferInt,
            _ => Type::InferInt,
        }
    }

    pub(crate) fn float_literal_type(
        &mut self,
        suffix: Option<&str>,
        expected: Option<&Type>,
    ) -> Type {
        if let Some(suffix) = suffix {
            return FloatTy::parse(suffix).map(Type::Float).unwrap_or_else(|| {
                self.diagnostic(
                    "E0011",
                    format!("unknown float literal suffix `{suffix}`"),
                    None,
                );
                Type::Error
            });
        }
        match expected {
            Some(Type::Float(ty)) => Type::Float(*ty),
            Some(Type::InferFloat) => Type::InferFloat,
            _ => Type::InferFloat,
        }
    }

    pub(crate) fn numeric_assignable(&self, expected: &Type, actual: &Type) -> bool {
        matches!(
            (expected, actual),
            (Type::Int(_), Type::InferInt)
                | (Type::InferInt, Type::Int(_))
                | (Type::Float(_), Type::InferFloat)
                | (Type::InferFloat, Type::Float(_))
                | (Type::InferInt, Type::InferInt)
                | (Type::InferFloat, Type::InferFloat)
        )
    }

    fn structural_assignable(&self, expected: &Type, actual: &Type) -> bool {
        match (expected, actual) {
            (Type::Array(expected_inner, expected_len), Type::Array(actual_inner, actual_len)) => {
                expected_len == actual_len
                    && (expected_inner == actual_inner
                        || self.numeric_assignable(expected_inner, actual_inner)
                        || self.structural_assignable(expected_inner, actual_inner))
            }
            _ => false,
        }
    }

    pub(crate) fn join_numeric_types(&self, lhs: &Type, rhs: &Type) -> Option<Type> {
        match (lhs, rhs) {
            (Type::Int(a), Type::Int(b)) if a == b => Some(Type::Int(*a)),
            (Type::Float(a), Type::Float(b)) if a == b => Some(Type::Float(*a)),
            (Type::Int(ty), Type::InferInt) | (Type::InferInt, Type::Int(ty)) => {
                Some(Type::Int(*ty))
            }
            (Type::Float(ty), Type::InferFloat) | (Type::InferFloat, Type::Float(ty)) => {
                Some(Type::Float(*ty))
            }
            (Type::InferInt, Type::InferInt) => Some(Type::InferInt),
            (Type::InferFloat, Type::InferFloat) => Some(Type::InferFloat),
            _ => None,
        }
    }
}
