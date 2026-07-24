use std::collections::{HashMap, HashSet};

use rowan::TextRange;

use hir::{
    HirFile,
    body::{BodyId, ExprId},
    item_tree::{
        EnumId, FunctionId, HirConstArg, HirFunction, HirPath, HirTrait, HirTypeAlias, HirTypeRef,
        HirVariantKind, InternalAttrTarget, PathAnchor, StructId,
    },
};

use crate::{
    context::BodyCtx,
    lang_items::{LangItem, RegisterResult},
    result::{Diagnostic, LabelStyle, Severity, SourceLabel, TypeCheckResult},
    trait_env::{TraitAssocConstraint, TraitBound},
    types::{ClosureKind, FloatTy, IntTy, Type},
};

pub struct TypeChecker<'a> {
    pub(crate) hir: &'a HirFile,
    pub(crate) result: TypeCheckResult,
    pub(crate) generic_edges: Vec<GenericEdge>,
    infinite_layout_types: HashSet<NominalType>,
    next_infer: u32,
    infer_values: HashMap<u32, Type>,
    pending_lambdas: Vec<PendingLambda>,
}

struct PendingLambda {
    body_id: BodyId,
    expr: ExprId,
    params: Vec<(String, Option<TextRange>, Type)>,
}

#[derive(Debug, Clone)]
pub(crate) struct GenericEdge {
    pub(crate) caller: FunctionId,
    pub(crate) callee: FunctionId,
    pub(crate) grows: bool,
    pub(crate) span: Option<TextRange>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum NominalType {
    Struct(StructId),
    Enum(EnumId),
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
            infinite_layout_types: HashSet::new(),
            next_infer: 0,
            infer_values: HashMap::new(),
            pending_lambdas: Vec::new(),
        }
    }

    pub fn check(mut self) -> TypeCheckResult {
        self.check_value_type_declarations();
        self.check_type_layouts();
        self.check_traits();
        self.check_trait_ref_arities();
        self.check_impls();
        self.build_trait_env();
        self.validate_copy_impls();
        self.check_function_bodies();
        self.check_generic_recursion();
        self.result
    }

    pub(crate) fn check_value_type_declarations(&mut self) {
        let structs = self
            .hir
            .item_tree
            .structs
            .iter()
            .map(|(_, item)| item.clone())
            .collect::<Vec<_>>();
        for item in structs {
            let params = crate::lowering::generic_param_map_with_consts(
                item.generics.iter().map(|name| name.0.as_str()),
                item.const_generics.iter().map(|name| name.0.as_str()),
            );
            for field in item.fields {
                let ty =
                    self.lower_type_ref_with_params_at(&field.ty, &params, Some(field.ty_range));
                self.expect_sized_value(&ty, Some(field.ty_range));
            }
        }

        let enums = self
            .hir
            .item_tree
            .enums
            .iter()
            .map(|(_, item)| item.clone())
            .collect::<Vec<_>>();
        for item in enums {
            let params = crate::lowering::generic_param_map_with_consts(
                item.generics.iter().map(|name| name.0.as_str()),
                item.const_generics.iter().map(|name| name.0.as_str()),
            );
            for variant in item.variants {
                let field_ranges = variant.field_ranges;
                match variant.kind {
                    HirVariantKind::Unit => {}
                    HirVariantKind::Tuple(fields) => {
                        for (field, range) in fields.into_iter().zip(field_ranges) {
                            let ty =
                                self.lower_type_ref_with_params_at(&field, &params, Some(range));
                            self.expect_sized_value(&ty, Some(range));
                        }
                    }
                    HirVariantKind::Struct(fields) => {
                        for field in fields {
                            let ty = self.lower_type_ref_with_params_at(
                                &field.ty,
                                &params,
                                Some(field.ty_range),
                            );
                            self.expect_sized_value(&ty, Some(field.ty_range));
                        }
                    }
                }
            }
        }

        let functions = self
            .hir
            .item_tree
            .functions
            .iter()
            .map(|(id, function)| (id, function.clone()))
            .collect::<Vec<_>>();
        for (id, function) in functions {
            let outer_generics = self.impl_generic_names(id);
            let outer_const_generics = self.impl_const_generic_names(id);
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
            if let Some(self_ty_ref) = self.impl_self_ty_ref(id).cloned() {
                let range = self.impl_self_ty_range(id).unwrap_or(function.name_range);
                let self_ty =
                    self.lower_type_ref_with_params_at(&self_ty_ref, &params, Some(range));
                let owner = self
                    .hir
                    .item_tree
                    .impls
                    .iter()
                    .find_map(|(_, imp)| imp.methods.contains(&id).then(|| imp.clone()));
                if let Some((imp, trait_id)) = owner.as_ref().and_then(|imp| {
                    imp.trait_ty
                        .as_ref()
                        .and_then(|trait_ty| self.resolve_trait_ref(trait_ty))
                        .map(|trait_id| (imp, trait_id))
                }) {
                    params = self.trait_ref_subst(
                        trait_id,
                        imp.trait_ty.as_ref().unwrap(),
                        &self_ty,
                        &params,
                        imp.trait_ty_range,
                    );
                } else {
                    params.insert("Self".into(), self_ty);
                }
            } else if self.trait_for_default_method(id).is_some() {
                params.insert("Self".into(), Type::Param("Self".into()));
            }
            self.check_function_value_types(&function, &params);
        }

        let traits = self
            .hir
            .item_tree
            .traits
            .iter()
            .map(|(_, item)| item.clone())
            .collect::<Vec<_>>();
        for item in traits {
            for method in item.methods {
                let mut params = crate::lowering::generic_param_map_with_consts(
                    item.generics
                        .iter()
                        .chain(method.generics.iter())
                        .map(|name| name.0.as_str()),
                    method.const_generics.iter().map(|name| name.0.as_str()),
                );
                params.insert("Self".into(), Type::Param("Self".into()));
                self.check_function_value_types(&method, &params);
            }
            for alias in item.type_aliases {
                let Some(alias_ty) = alias.ty else {
                    continue;
                };
                let params = HashMap::from([("Self".into(), Type::Param("Self".into()))]);
                let range = alias.ty_range.unwrap_or(alias.name_range);
                let ty = self.lower_type_ref_with_params_at(&alias_ty, &params, Some(range));
                self.expect_sized_value(&ty, Some(range));
            }
        }

        let consts = self
            .hir
            .item_tree
            .consts
            .iter()
            .map(|(id, item)| (id, item.clone()))
            .collect::<Vec<_>>();
        for (id, item) in consts {
            let params = self.impl_item_type_params(|imp| imp.consts.contains(&id));
            let ty = self.lower_type_ref_with_params_at(&item.ty, &params, Some(item.ty_range));
            self.expect_sized_value(&ty, Some(item.ty_range));
        }

        let aliases = self
            .hir
            .item_tree
            .type_aliases
            .iter()
            .map(|(id, item)| (id, item.clone()))
            .collect::<Vec<_>>();
        for (id, item) in aliases {
            let Some(alias) = item.ty else {
                continue;
            };
            let params = self.impl_item_type_params(|imp| imp.type_aliases.contains(&id));
            let range = item.ty_range.unwrap_or(item.name_range);
            let ty = self.lower_type_ref_with_params_at(&alias, &params, Some(range));
            self.expect_sized_value(&ty, Some(range));
        }
    }

    fn check_function_value_types(
        &mut self,
        function: &HirFunction,
        params: &HashMap<String, Type>,
    ) {
        for param in &function.params {
            let ty = self.lower_type_ref_with_params_at(&param.ty, params, Some(param.ty_range));
            self.expect_sized_value(&ty, Some(param.ty_range));
        }
        if let Some(return_ty) = &function.ret_type {
            let range = function.ret_type_range.unwrap_or(function.name_range);
            let ty = self.lower_type_ref_with_params_at(return_ty, params, Some(range));
            self.expect_sized_value(&ty, Some(range));
        }
    }

    fn impl_item_type_params(
        &mut self,
        owns: impl Fn(&hir::item_tree::HirImpl) -> bool,
    ) -> HashMap<String, Type> {
        let owner = self
            .hir
            .item_tree
            .impls
            .iter()
            .find_map(|(_, imp)| owns(imp).then(|| imp.clone()));
        let Some(owner) = owner else {
            return HashMap::new();
        };
        let mut params = crate::lowering::generic_param_map_with_consts(
            owner.generics.iter().map(|name| name.0.as_str()),
            owner.const_generics.iter().map(|name| name.0.as_str()),
        );
        let self_ty =
            self.lower_type_ref_with_params_at(&owner.self_ty, &params, Some(owner.self_ty_range));
        params.insert("Self".into(), self_ty);
        params
    }

    pub(crate) fn build_trait_env(&mut self) {
        let invalid_internal_attrs = self.validate_internal_attrs();

        // ── Validate and register #[lang] traits ──────────────────────────────
        for (tid, tr) in self.hir.item_tree.traits.iter() {
            for attr in &tr.attrs {
                if attr.name.0 != "lang" {
                    continue;
                }
                if invalid_internal_attrs.contains(&attr.range) {
                    continue;
                }
                let Some(lang) = attr.value.as_deref() else {
                    continue;
                };

                // Unknown lang name.
                let Some(item) = LangItem::from_name(lang) else {
                    self.result.diagnostics.push(Diagnostic {
                        code: "E0053",
                        severity: Severity::Error,
                        message: format!("unknown lang item `{lang}`"),
                        labels: vec![SourceLabel {
                            range: tr.name_range,
                            message: String::new(),
                            style: LabelStyle::Primary,
                        }],
                        help: Some(
                            "recognized lang items: copy, clone, partial_eq, eq, partial_ord, ord, \
                             add, sub, mul, div, rem, neg, not, bitand, bitor, bitxor, \
                             shl, shr, and the *_assign variants"
                                .to_string(),
                        ),
                        notes: Vec::new(),
                    });
                    continue;
                };

                // Signature validation.
                if let Some(reason) = validate_lang_item_signature(item, tr) {
                    self.result.diagnostics.push(Diagnostic {
                        code: "E0053",
                        severity: Severity::Error,
                        message: format!(
                            "lang item `{}` has an invalid trait signature: {reason}",
                            item.as_str()
                        ),
                        labels: vec![SourceLabel {
                            range: tr.name_range,
                            message: String::new(),
                            style: LabelStyle::Primary,
                        }],
                        help: None,
                        notes: Vec::new(),
                    });
                    continue;
                }

                // Duplicate lang item / duplicate trait annotation.
                let generic_count = tr.generics.len();
                match self
                    .result
                    .trait_env
                    .register_lang_item(item, tid, generic_count)
                {
                    RegisterResult::Ok => {}
                    RegisterResult::DuplicateItem => {
                        let notes = vec!["first definition is elsewhere in the source".into()];
                        self.result.diagnostics.push(Diagnostic {
                            code: "E0053",
                            severity: Severity::Error,
                            message: format!(
                                "lang item `{}` is defined more than once",
                                item.as_str()
                            ),
                            labels: vec![SourceLabel {
                                range: tr.name_range,
                                message: "duplicate definition here".into(),
                                style: LabelStyle::Primary,
                            }],
                            help: None,
                            notes,
                        });
                    }
                    RegisterResult::DuplicateTrait => {
                        self.result.diagnostics.push(Diagnostic {
                            code: "E0053",
                            severity: Severity::Error,
                            message: "a trait can carry at most one `#[lang]` attribute; \
                                      this trait is already registered as a different lang item"
                                .to_string(),
                            labels: vec![SourceLabel {
                                range: tr.name_range,
                                message: "second `#[lang]` attribute here".into(),
                                style: LabelStyle::Primary,
                            }],
                            help: None,
                            notes: Vec::new(),
                        });
                    }
                }
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
            let self_ty =
                self.lower_type_ref_with_params_at(&imp.self_ty, &params, Some(imp.self_ty_range));
            let trait_args =
                self.trait_ref_args(trait_id, trait_ty, &self_ty, &params, imp.trait_ty_range);
            let bounds = self.lower_trait_env_bounds(&imp.generic_bounds, &params);
            let assoc_types = imp
                .type_aliases
                .iter()
                .filter_map(|alias_id| {
                    let alias = &self.hir.item_tree.type_aliases[*alias_id];
                    alias.ty.as_ref().map(|ty| {
                        (
                            alias.name.0.clone(),
                            self.lower_type_ref_with_params_at(
                                ty,
                                &params,
                                alias.ty_range.or(Some(alias.name_range)),
                            ),
                        )
                    })
                })
                .collect();
            self.result
                .trait_env
                .insert_impl(trait_id, self_ty, trait_args, bounds, assoc_types);
        }
    }

    fn validate_internal_attrs(&mut self) -> HashSet<TextRange> {
        let mut invalid = HashSet::new();
        for internal in &self.hir.internal_attrs {
            let attr = &internal.attr;
            let is_user_attr =
                self.hir.std_loaded && self.hir.package_for_range(attr.range).is_some();
            let (code, message) = if is_user_attr {
                (
                    "E0049",
                    format!("`{}` is reserved for the standard library", attr.raw),
                )
            } else {
                match attr.name.0.as_str() {
                    "lang" if internal.target != InternalAttrTarget::Trait => (
                        "E0053",
                        "`#[lang]` can only be applied to a trait".to_string(),
                    ),
                    "lang" if attr.value.is_none() => (
                        "E0053",
                        "`#[lang]` requires a string value: write `#[lang = \"...\"]`".to_string(),
                    ),
                    "fundamental" if internal.target != InternalAttrTarget::FundamentalType => (
                        "E0053",
                        "`#[fundamental]` can only be applied to a struct or enum".to_string(),
                    ),
                    "fundamental" if attr.value.is_some() => (
                        "E0053",
                        "`#[fundamental]` does not accept a value".to_string(),
                    ),
                    _ => continue,
                }
            };
            invalid.insert(attr.range);
            self.result.diagnostics.push(Diagnostic {
                code,
                severity: Severity::Error,
                message,
                labels: vec![SourceLabel {
                    range: attr.range,
                    message: String::new(),
                    style: LabelStyle::Primary,
                }],
                help: None,
                notes: Vec::new(),
            });
        }
        invalid
    }

    pub(crate) fn lower_trait_env_bounds(
        &mut self,
        bounds: &[hir::item_tree::HirGenericBound],
        params: &HashMap<String, Type>,
    ) -> Vec<TraitBound> {
        bounds
            .iter()
            .filter_map(|bound| {
                let trait_id = self.resolve_trait_ref(&bound.trait_ty)?;
                let ty = self.lower_type_ref_with_params_at(
                    &bound.target_ty,
                    params,
                    Some(bound.target_range),
                );
                let trait_args = self.trait_ref_args(
                    trait_id,
                    &bound.trait_ty,
                    &ty,
                    params,
                    Some(bound.trait_range),
                );
                Some(TraitBound {
                    ty,
                    trait_id,
                    trait_args,
                    assoc_constraints: bound
                        .assoc_constraints
                        .iter()
                        .map(|constraint| TraitAssocConstraint {
                            name: constraint.name.0.clone(),
                            ty: self.lower_type_ref_with_params_at(
                                &constraint.ty,
                                params,
                                Some(constraint.range),
                            ),
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
            let self_ty_range = self
                .hir
                .item_tree
                .impls
                .iter()
                .find_map(|(_, imp)| {
                    imp.methods
                        .contains(&function_id)
                        .then_some(imp.self_ty_range)
                })
                .unwrap_or(function.name_range);
            let self_ty =
                self.lower_type_ref_with_params_at(&self_ty_ref, &params, Some(self_ty_range));
            let owner = self
                .hir
                .item_tree
                .impls
                .iter()
                .find_map(|(_, imp)| imp.methods.contains(&function_id).then(|| imp.clone()));
            if let Some((imp, trait_id)) = owner.as_ref().and_then(|imp| {
                imp.trait_ty
                    .as_ref()
                    .and_then(|trait_ty| self.resolve_trait_ref(trait_ty))
                    .map(|trait_id| (imp, trait_id))
            }) {
                params = self.trait_ref_subst(
                    trait_id,
                    imp.trait_ty.as_ref().unwrap(),
                    &self_ty,
                    &params,
                    imp.trait_ty_range,
                );
            } else {
                params.insert("Self".into(), self_ty);
            }
        } else if self.trait_for_default_method(function_id).is_some() {
            params.insert("Self".into(), Type::Param("Self".into()));
        }
        let return_ty = function
            .ret_type
            .as_ref()
            .map(|ty| {
                self.lower_type_ref_with_params_at(
                    ty,
                    &params,
                    function.ret_type_range.or(Some(function.name_range)),
                )
            })
            .unwrap_or(Type::Unit);
        let mut ctx = BodyCtx::new(
            body_id,
            body,
            function_id,
            function,
            return_ty.clone(),
            params,
        );
        self.check_type_bounds_inner(
            &ctx,
            &return_ty,
            function.ret_type_range.or(Some(function.name_range)),
        );
        for param in &function.params {
            let param_ty = self.lower_type_ref_with_params_at(
                &param.ty,
                &ctx.generic_params,
                Some(param.ty_range),
            );
            self.check_type_bounds_inner(&ctx, &param_ty, Some(param.ty_range));
        }
        let actual = self.check_expr_expected(&mut ctx, body.root_block, &return_ty);

        if !actual.is_never() {
            self.expect_assignable(
                &return_ty,
                &actual,
                "function return",
                ctx.expr_range(body.root_block),
            );
        }
        self.finish_inference(body_id);
    }

    pub(crate) fn fresh_infer(&mut self) -> Type {
        let id = self.next_infer;
        self.next_infer += 1;
        Type::InferVar(id)
    }

    pub(crate) fn resolve_type(&self, ty: &Type) -> Type {
        match ty {
            Type::InferVar(id) => self
                .infer_values
                .get(id)
                .map(|value| self.resolve_type(value))
                .unwrap_or_else(|| ty.clone()),
            Type::Ref(inner, mutable) => Type::Ref(Box::new(self.resolve_type(inner)), *mutable),
            Type::Ptr { mutable, inner } => Type::Ptr {
                mutable: *mutable,
                inner: Box::new(self.resolve_type(inner)),
            },
            Type::Tuple(elements) => Type::Tuple(
                elements
                    .iter()
                    .map(|item| self.resolve_type(item))
                    .collect(),
            ),
            Type::Array(inner, len) => Type::Array(Box::new(self.resolve_type(inner)), len.clone()),
            Type::Struct(id, args) => {
                Type::Struct(*id, args.iter().map(|arg| self.resolve_type(arg)).collect())
            }
            Type::Enum(id, args) => {
                Type::Enum(*id, args.iter().map(|arg| self.resolve_type(arg)).collect())
            }
            Type::Fn {
                is_unsafe,
                kind,
                params,
                ret,
            } => Type::Fn {
                is_unsafe: *is_unsafe,
                kind: *kind,
                params: params
                    .iter()
                    .map(|param| self.resolve_type(param))
                    .collect(),
                ret: Box::new(self.resolve_type(ret)),
            },
            _ => ty.clone(),
        }
    }

    pub(crate) fn callable_type(&mut self, ty: &Type) -> Type {
        let ty = self.resolve_type(ty);
        let Type::Function(fid) = ty else {
            return ty;
        };
        let function = self.hir.item_tree.functions[fid].clone();
        Type::Fn {
            is_unsafe: function.is_unsafe,
            kind: ClosureKind::Fn,
            params: function
                .params
                .iter()
                .map(|param| {
                    self.lower_type_ref_with_params_at(
                        &param.ty,
                        &HashMap::new(),
                        Some(param.ty_range),
                    )
                })
                .collect(),
            ret: Box::new(
                function
                    .ret_type
                    .as_ref()
                    .map(|ret| {
                        self.lower_type_ref_with_params_at(
                            ret,
                            &HashMap::new(),
                            function.ret_type_range.or(Some(function.name_range)),
                        )
                    })
                    .unwrap_or(Type::Unit),
            ),
        }
    }

    pub(crate) fn unify_types(&mut self, lhs: &Type, rhs: &Type) -> bool {
        let lhs = self.callable_type(lhs);
        let rhs = self.callable_type(rhs);
        match (&lhs, &rhs) {
            (Type::InferVar(id), ty) | (ty, Type::InferVar(id)) => {
                if matches!(ty, Type::InferVar(other) if other == id) {
                    true
                } else {
                    self.infer_values.insert(*id, ty.clone());
                    true
                }
            }
            (Type::Ref(a, am), Type::Ref(b, bm)) => am == bm && self.unify_types(a, b),
            (
                Type::Ptr {
                    mutable: am,
                    inner: a,
                },
                Type::Ptr {
                    mutable: bm,
                    inner: b,
                },
            ) => am == bm && self.unify_types(a, b),
            (Type::Tuple(a), Type::Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(a, b)| self.unify_types(a, b))
            }
            (Type::Array(a, al), Type::Array(b, bl)) => al == bl && self.unify_types(a, b),
            (Type::Struct(a, aa), Type::Struct(b, ba)) => {
                a == b
                    && aa.len() == ba.len()
                    && aa.iter().zip(ba).all(|(a, b)| self.unify_types(a, b))
            }
            (Type::Enum(a, aa), Type::Enum(b, ba)) => {
                a == b
                    && aa.len() == ba.len()
                    && aa.iter().zip(ba).all(|(a, b)| self.unify_types(a, b))
            }
            (
                Type::Fn {
                    is_unsafe: expected_unsafe,
                    kind: expected_kind,
                    params: ap,
                    ret: ar,
                },
                Type::Fn {
                    is_unsafe: actual_unsafe,
                    kind: actual_kind,
                    params: bp,
                    ret: br,
                },
            ) => {
                (!*actual_unsafe || *expected_unsafe)
                    && expected_kind.accepts(*actual_kind)
                    && ap.len() == bp.len()
                    && ap.iter().zip(bp).all(|(a, b)| self.unify_types(a, b))
                    && self.unify_types(ar, br)
            }
            _ => lhs == rhs || self.numeric_assignable(&lhs, &rhs),
        }
    }

    pub(crate) fn record_lambda(
        &mut self,
        body_id: BodyId,
        expr: ExprId,
        params: Vec<(String, Option<TextRange>, Type)>,
    ) {
        self.pending_lambdas.push(PendingLambda {
            body_id,
            expr,
            params,
        });
    }

    fn finish_inference(&mut self, body_id: BodyId) {
        let exprs = self
            .result
            .expr_types
            .iter()
            .filter(|((bid, _), _)| *bid == body_id)
            .map(|(key, ty)| (*key, self.resolve_type(ty)))
            .collect::<Vec<_>>();
        for (key, ty) in exprs {
            self.result.expr_types.insert(key, ty);
        }
        let lambdas = self
            .result
            .lambda_infos
            .iter()
            .filter(|((checked_body, _), _)| *checked_body == body_id)
            .map(|(key, info)| {
                let mut info = info.clone();
                for capture in &mut info.captures {
                    capture.ty = self.resolve_type(&capture.ty);
                }
                (*key, info)
            })
            .collect::<Vec<_>>();
        for (key, info) in lambdas {
            self.result.lambda_infos.insert(key, info);
        }
        let pending = self
            .pending_lambdas
            .iter()
            .filter(|lambda| lambda.body_id == body_id)
            .map(|lambda| {
                (
                    lambda.expr,
                    lambda
                        .params
                        .iter()
                        .map(|(name, range, ty)| (name.clone(), *range, self.resolve_type(ty)))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        for (_, params) in pending {
            for (name, range, ty) in params {
                if matches!(ty, Type::InferVar(_)) {
                    self.diagnostic(
                        "E0045",
                        format!("cannot infer type of anonymous function parameter `{name}`"),
                        range,
                    );
                }
            }
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
            .or_else(|| {
                self.hir.item_tree.traits.iter().find_map(|(_, tr)| {
                    tr.default_methods
                        .contains(&function_id)
                        .then(|| tr.generics.iter().map(|name| name.0.clone()).collect())
                })
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

    pub(crate) fn trait_for_default_method(
        &self,
        function_id: FunctionId,
    ) -> Option<hir::item_tree::TraitId> {
        self.hir.item_tree.traits.iter().find_map(|(trait_id, tr)| {
            tr.default_methods
                .contains(&function_id)
                .then_some(trait_id)
        })
    }

    pub(crate) fn impl_self_ty_range(&self, function_id: FunctionId) -> Option<TextRange> {
        self.hir.item_tree.impls.iter().find_map(|(_, imp)| {
            imp.methods
                .contains(&function_id)
                .then_some(imp.self_ty_range)
        })
    }

    pub(crate) fn check_type_layouts(&mut self) {
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
                self.type_ref_contains_inline_type(
                    &field.ty,
                    NominalType::Struct(id),
                    &mut Vec::new(),
                )
                .then_some(field.ty_range)
            }) {
                self.infinite_layout_types.insert(NominalType::Struct(id));
                self.diagnostic(
                    "E0072",
                    format!("recursive type `{name}` has infinite size"),
                    Some(name_range),
                );
                self.result
                    .diagnostics
                    .last_mut()
                    .unwrap()
                    .labels
                    .push(SourceLabel {
                        range: field_range,
                        message: "recursive field".into(),
                        style: LabelStyle::Secondary,
                    });
            }
        }

        let enums = self
            .hir
            .item_tree
            .enums
            .iter()
            .map(|(id, item)| {
                (
                    id,
                    item.name.0.clone(),
                    item.name_range,
                    item.variants.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (id, name, name_range, variants) in enums {
            let target = NominalType::Enum(id);
            let recursive_field =
                variants.iter().find_map(|variant| match &variant.kind {
                    HirVariantKind::Unit => None,
                    HirVariantKind::Tuple(fields) => fields
                        .iter()
                        .zip(&variant.field_ranges)
                        .find_map(|(field, range)| {
                            self.type_ref_contains_inline_type(field, target, &mut Vec::new())
                                .then_some(*range)
                        }),
                    HirVariantKind::Struct(fields) => fields.iter().find_map(|field| {
                        self.type_ref_contains_inline_type(&field.ty, target, &mut Vec::new())
                            .then_some(field.ty_range)
                    }),
                });
            if let Some(field_range) = recursive_field {
                self.infinite_layout_types.insert(target);
                self.diagnostic(
                    "E0072",
                    format!("recursive type `{name}` has infinite size"),
                    Some(name_range),
                );
                self.result
                    .diagnostics
                    .last_mut()
                    .unwrap()
                    .labels
                    .push(SourceLabel {
                        range: field_range,
                        message: "recursive field".into(),
                        style: LabelStyle::Secondary,
                    });
            }
        }
    }

    fn type_ref_contains_inline_type(
        &self,
        ty: &HirTypeRef,
        target: NominalType,
        seen: &mut Vec<NominalType>,
    ) -> bool {
        match ty {
            HirTypeRef::Named(path) => {
                let Some(name) = path.as_single_name().map(|name| name.0.as_str()) else {
                    return false;
                };
                let Some(current) = self.find_nominal_type(name) else {
                    return false;
                };
                if current == target {
                    return true;
                }
                if seen.contains(&current) {
                    return false;
                }
                seen.push(current);
                let found = self.nominal_type_contains_inline_target(current, target, seen);
                seen.pop();
                found
            }
            HirTypeRef::Tuple(elements) => elements
                .iter()
                .any(|ty| self.type_ref_contains_inline_type(ty, target, seen)),
            HirTypeRef::Array(_, HirConstArg::Value(0)) => false,
            HirTypeRef::Array(inner, _) => self.type_ref_contains_inline_type(inner, target, seen),
            HirTypeRef::Const(_) => false,
            HirTypeRef::Ref(_, _) | HirTypeRef::Ptr { .. } => false,
            HirTypeRef::Function { .. } => false,
            HirTypeRef::Unknown | HirTypeRef::Error => false,
        }
    }

    fn find_nominal_type(&self, name: &str) -> Option<NominalType> {
        self.hir
            .item_tree
            .structs
            .iter()
            .find_map(|(id, item)| (item.name.0 == name).then_some(NominalType::Struct(id)))
            .or_else(|| {
                self.hir
                    .item_tree
                    .enums
                    .iter()
                    .find_map(|(id, item)| (item.name.0 == name).then_some(NominalType::Enum(id)))
            })
    }

    fn nominal_type_contains_inline_target(
        &self,
        current: NominalType,
        target: NominalType,
        seen: &mut Vec<NominalType>,
    ) -> bool {
        match current {
            NominalType::Struct(id) => self.hir.item_tree.structs[id]
                .fields
                .iter()
                .any(|field| self.type_ref_contains_inline_type(&field.ty, target, seen)),
            NominalType::Enum(id) => {
                self.hir.item_tree.enums[id]
                    .variants
                    .iter()
                    .any(|variant| match &variant.kind {
                        HirVariantKind::Unit => false,
                        HirVariantKind::Tuple(fields) => fields
                            .iter()
                            .any(|field| self.type_ref_contains_inline_type(field, target, seen)),
                        HirVariantKind::Struct(fields) => fields.iter().any(|field| {
                            self.type_ref_contains_inline_type(&field.ty, target, seen)
                        }),
                    })
            }
        }
    }

    pub(crate) fn type_has_infinite_layout(&self, ty: &Type) -> bool {
        match ty {
            Type::Struct(id, _) => self
                .infinite_layout_types
                .contains(&NominalType::Struct(*id)),
            Type::Enum(id, _) => self.infinite_layout_types.contains(&NominalType::Enum(*id)),
            Type::Tuple(fields) => fields
                .iter()
                .any(|field| self.type_has_infinite_layout(field)),
            Type::Array(_, crate::ConstArg::Value(0)) => false,
            Type::Array(inner, _) => self.type_has_infinite_layout(inner),
            Type::Ref(_, _) | Type::Ptr { .. } => false,
            _ => false,
        }
    }

    pub(crate) fn check_generic_recursion(&mut self) {
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
        if self.unify_types(&lhs, &rhs) {
            return self.resolve_type(&lhs);
        }
        if matches!((&lhs, &rhs), (Type::Fn { .. }, Type::Fn { .. }))
            && self.unify_types(&rhs, &lhs)
        {
            return self.resolve_type(&rhs);
        }
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
        if self.unify_types(expected, actual) {
            return;
        }
        let expected = self.resolve_type(expected);
        let actual = self.resolve_type(actual);
        if expected.is_unknown_like()
            || actual.is_unknown_like()
            || expected == actual
            || self.numeric_assignable(&expected, &actual)
            || self.structural_assignable(&expected, &actual)
        {
            return;
        }
        if actual.is_never() {
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

    pub(crate) fn expect_sized_value(&mut self, ty: &Type, span: Option<TextRange>) {
        if ty.is_valid_value_type() {
            return;
        }
        self.diagnostic(
            "E0043",
            format!(
                "type `{}` contains unsized `str` in a position that requires a sized type",
                ty.display(self.hir)
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
        let span = span.expect("type-checker diagnostics require a source range");
        let message = message.into();
        let notes = match code {
            "E0001" => vec!["expected one type but found another; consider an explicit type annotation or cast".into()],
            "E0002" => vec!["all branches must produce values of the same type; ensure both branches return compatible types".into()],
            "E0003" => vec!["this operation requires a numeric or `char` type".into()],
            "E0004" => vec!["only functions can be called".into()],
            "E0005" => vec!["check the function declaration for the expected parameter count".into()],
            "E0006" => vec!["check the struct definition for available fields".into()],
            "E0007" => vec!["add the missing field to the struct literal".into()],
            "E0008" => vec!["only references can be dereferenced, and only arrays can be indexed".into()],
            "E0009" => vec!["check that the path names a struct definition".into()],
            "E0010" => vec!["ensure tuple element counts match".into()],
            "E0011" => vec!["use a valid numeric suffix and keep the literal within that type's range".into()],
            "E0012" => vec!["this source and target type pair does not support `as` conversion".into()],
            "E0013" => vec!["check the impl block and receiver type".into()],
            "E0020" | "E0024" => vec!["remove the duplicate definition".into()],
            "E0031" if message == "cannot call a mutable closure through an immutable binding" => {
                vec!["add `mut` to the closure binding because calling it may update captured state".into()]
            }
            "E0031" => vec!["add `mut` to the `let` binding if reassignment is intended".into()],
            "E0033" => vec!["recursive generic calls must reuse the same type arguments; wrapping them requires infinitely many instantiations".into()],
            "E0035" => vec!["the inferred type must implement every trait bound on the generic parameter".into()],
            "E0036" => vec!["add the required trait impl for this type".into()],
            "E0037" => vec!["make impl where-clause bounds structurally smaller than the implemented type".into()],
            "E0038" => vec!["use a variant pattern whose shape and fields match the enum declaration".into()],
            "E0039" => vec!["cover every possible case or add a wildcard arm".into()],
            "E0041" => vec!["every field must implement `Copy`; add the required generic bounds or remove the impl".into()],
            "E0042" => vec!["move this statement inside a `while` or `for` loop".into()],
            "E0043" => vec!["use `&str` or a raw pointer; unsized `str` must be behind a reference or pointer".into()],
            "E0044" => vec!["define every supertrait and remove cycles from the trait hierarchy".into()],
            "E0045" => vec!["add an explicit parameter type or use the function where its signature is known".into()],
            "E0047" => vec!["remove the duplicate or overlapping trait implementation".into()],
            "E0048" => vec!["define the trait or a participating nominal type in the current package".into()],
            "E0072" => vec!["insert indirection such as `&`, `*const`, or `*mut` to break the cycle".into()],
            "E0022" | "E0025" => vec!["remove the duplicate associated type".into()],
            "E0023" => vec!["define the trait or import it into scope".into()],
            "E0026" => vec!["add an implementation for the required method".into()],
            "E0027" => vec!["add a type definition for the required associated type".into()],
            "E0028" | "E0029" | "E0030" => vec!["the method signature must exactly match the trait declaration: check parameter count, types, and return type".into()],
            _ => Vec::new(),
        };
        let help = (code == "E0046").then(|| "wrap this operation in `unsafe { ... }`".to_string());
        self.result.diagnostics.push(Diagnostic {
            code,
            severity: Severity::Error,
            message,
            labels: vec![SourceLabel {
                range: span,
                message: String::new(),
                style: LabelStyle::Primary,
            }],
            help,
            notes,
        });
    }

    pub(crate) fn int_literal_type(
        &mut self,
        suffix: Option<&str>,
        expected: Option<&Type>,
        span: Option<TextRange>,
    ) -> Type {
        if let Some(suffix) = suffix {
            return IntTy::parse(suffix).map(Type::Int).unwrap_or_else(|| {
                self.diagnostic(
                    "E0011",
                    format!("unknown integer literal suffix `{suffix}`"),
                    span,
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
        span: Option<TextRange>,
    ) -> Type {
        if let Some(suffix) = suffix {
            return FloatTy::parse(suffix).map(Type::Float).unwrap_or_else(|| {
                self.diagnostic(
                    "E0011",
                    format!("unknown float literal suffix `{suffix}`"),
                    span,
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
            (Type::Ref(expected_inner, expected_mut), Type::Ref(actual_inner, actual_mut)) => {
                expected_mut == actual_mut
                    && (expected_inner == actual_inner
                        || self.numeric_assignable(expected_inner, actual_inner)
                        || self.structural_assignable(expected_inner, actual_inner))
            }
            (
                Type::Ptr {
                    mutable: expected_mut,
                    inner: expected_inner,
                },
                Type::Ptr {
                    mutable: actual_mut,
                    inner: actual_inner,
                },
            ) => {
                expected_mut == actual_mut
                    && (expected_inner == actual_inner
                        || self.numeric_assignable(expected_inner, actual_inner)
                        || self.structural_assignable(expected_inner, actual_inner))
            }
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

    /// If `ctx` is not inside `unsafe {}`, emit E0046.
    pub(crate) fn require_unsafe(
        &mut self,
        ctx: &BodyCtx<'_>,
        operation: &str,
        span: Option<rowan::TextRange>,
    ) {
        if ctx.unsafe_depth == 0 {
            self.diagnostic(
                "E0046",
                format!("{operation} requires an unsafe block"),
                span,
            );
        }
    }
}

// ── Lang item signature validation ──────────────────────────────────────────
//
// Called from `build_trait_env` before a `#[lang = "..."]` trait is
// registered.  Returns `None` when the trait satisfies the contract, or
// `Some(reason)` when it does not.

pub(crate) fn validate_lang_item_signature(
    item: crate::lang_items::LangItem,
    tr: &HirTrait,
) -> Option<String> {
    use crate::lang_items::LangItem;
    let self_ty = HirTypeRef::Named(HirPath {
        anchor: PathAnchor::Plain,
        segments: vec![hir::Name("Self".into())],
        type_args: Vec::new(),
    });

    match item {
        // Marker traits — no method/assoc-type contract required.
        LangItem::Copy | LangItem::Eq => {
            (!tr.generics.is_empty()).then(|| "marker trait must not be generic".into())
        }

        // Clone — must have `clone(&self) -> Self`.
        LangItem::Clone => {
            if !tr.generics.is_empty() {
                return Some("`Clone` trait must not be generic".into());
            }
            let Some(method) = tr.methods.iter().find(|m| m.name.0 == "clone") else {
                return Some("must define a method named `clone`".into());
            };
            if !method.generics.is_empty() || !method.const_generics.is_empty() {
                return Some("`clone` must not carry its own generic parameters".into());
            }
            if method.params.len() != 1 {
                return Some("`clone` must take exactly one parameter (`&self`)".into());
            }
            if !matches!(&method.params[0].ty,
                HirTypeRef::Ref(inner, false) if lang_type_is_self(inner, &self_ty))
            {
                return Some("first parameter of `clone` must be `&self`".into());
            }
            if !method
                .ret_type
                .as_ref()
                .is_some_and(|ty| lang_type_is_self(ty, &self_ty))
            {
                return Some("`clone` must return `Self`".into());
            }
            None
        }

        // Comparison traits.
        LangItem::PartialEq => {
            if !lang_has_valid_rhs_generic(tr, &self_ty) {
                return Some(
                    "comparison trait must have at most one generic type \
                     parameter `Rhs = Self`"
                        .into(),
                );
            }
            validate_comparison_method_sig(tr, "eq", &self_ty)
        }

        LangItem::PartialOrd => {
            if !lang_has_valid_rhs_generic(tr, &self_ty) {
                return Some(
                    "comparison trait must have at most one generic type \
                     parameter `Rhs = Self`"
                        .into(),
                );
            }
            let mut found = false;
            for name in ["lt", "gt", "le", "ge"] {
                if tr.methods.iter().any(|method| method.name.0 == name) {
                    found = true;
                    if let Some(reason) = validate_comparison_method_sig(tr, name, &self_ty) {
                        return Some(reason);
                    }
                }
            }
            if tr
                .methods
                .iter()
                .any(|method| method.name.0 == "partial_cmp")
            {
                found = true;
                if let Some(reason) = validate_partial_cmp(tr, &self_ty) {
                    return Some(reason);
                }
            }
            (!found).then(|| {
                "must define at least one ordering method \
                 (`lt`, `gt`, `le`, `ge`, or `partial_cmp`)"
                    .into()
            })
        }

        LangItem::Ord => validate_ord_cmp(tr, &self_ty),

        // Binary operators — (self, rhs) → Self::Output.
        LangItem::Add
        | LangItem::Sub
        | LangItem::Mul
        | LangItem::Div
        | LangItem::Rem
        | LangItem::BitAnd
        | LangItem::BitOr
        | LangItem::BitXor
        | LangItem::Shl
        | LangItem::Shr => validate_binary_operator_trait(tr, item.as_str(), &self_ty),

        // Unary operators — (self) → Self::Output.
        LangItem::Neg | LangItem::Not => validate_unary_operator_trait(tr, item.as_str(), &self_ty),

        // Assign operators — (&mut self, rhs) → ().
        LangItem::AddAssign
        | LangItem::SubAssign
        | LangItem::MulAssign
        | LangItem::DivAssign
        | LangItem::RemAssign
        | LangItem::BitAndAssign
        | LangItem::BitOrAssign
        | LangItem::BitXorAssign
        | LangItem::ShlAssign
        | LangItem::ShrAssign => validate_assign_operator_trait(tr, item.as_str(), &self_ty),
    }
}

fn lang_type_is_self(ty: &HirTypeRef, self_ty: &HirTypeRef) -> bool {
    ty == self_ty
        || matches!(
            ty,
            HirTypeRef::Named(path)
                if path.as_single_name().is_some_and(|name| name.0 == "Self")
        )
}

fn lang_is_self_output(ty: &HirTypeRef) -> bool {
    let HirTypeRef::Named(path) = ty else {
        return false;
    };
    matches!(path.anchor, PathAnchor::Plain)
        && path.segments.len() == 2
        && path.segments[0].0 == "Self"
        && path.segments[1].0 == "Output"
        && path.type_args.is_empty()
}

fn lang_has_output_assoc_type(tr: &HirTrait) -> bool {
    tr.type_aliases
        .iter()
        .any(|alias: &HirTypeAlias| alias.name.0 == "Output" && alias.ty.is_none())
}

fn lang_returns_unit(function: &HirFunction) -> bool {
    function
        .ret_type
        .as_ref()
        .is_none_or(|ty| matches!(ty, HirTypeRef::Tuple(elements) if elements.is_empty()))
}

/// Returns None if the trait's RHS generic setup is valid for an operator
/// lang item (0 or 1 type param, if 1 it must default to `Self`).
fn lang_has_valid_rhs_generic(tr: &HirTrait, self_ty: &HirTypeRef) -> bool {
    tr.generics.len() <= 1
        && if tr.generics.is_empty() {
            true
        } else {
            tr.generic_defaults
                .first()
                .and_then(Option::as_ref)
                .is_some_and(|default| lang_type_is_self(default, self_ty))
        }
}

fn lang_type_is_rhs(tr: &HirTrait, ty: &HirTypeRef) -> bool {
    match tr.generics.first() {
        Some(rhs) => matches!(
            ty,
            HirTypeRef::Named(path)
                if path.as_single_name().is_some_and(|name| name == rhs)
        ),
        None => true,
    }
}

fn lang_ref_is_rhs(tr: &HirTrait, ty: &HirTypeRef) -> bool {
    matches!(ty, HirTypeRef::Ref(inner, false) if lang_type_is_rhs(tr, inner))
}

fn validate_binary_operator_trait(
    tr: &HirTrait,
    method_name: &str,
    self_ty: &HirTypeRef,
) -> Option<String> {
    let Some(method) = tr.methods.iter().find(|m| m.name.0 == method_name) else {
        return Some(format!("must define a method named `{method_name}`"));
    };
    if !method.generics.is_empty() || !method.const_generics.is_empty() {
        return Some(format!(
            "`{method_name}` must not carry its own generic parameters"
        ));
    }
    if !lang_has_valid_rhs_generic(tr, self_ty) {
        return Some(format!(
            "`#[lang = \"{method_name}\"]` trait must have at most one \
             generic type parameter `Rhs = Self`"
        ));
    }
    if !lang_has_output_assoc_type(tr) {
        return Some(format!(
            "`#[lang = \"{method_name}\"]` trait must declare an \
             associated type `Output` (without a default)"
        ));
    }
    if method.params.len() != 2 {
        return Some(format!(
            "`{method_name}` must take exactly 2 parameters (`self` and `rhs`)"
        ));
    }
    if !lang_type_is_self(&method.params[0].ty, self_ty) {
        return Some(format!("first parameter of `{method_name}` must be `self`"));
    }
    if !lang_type_is_rhs(tr, &method.params[1].ty) {
        return Some(format!(
            "second parameter of `{method_name}` must match `Rhs`"
        ));
    }
    if !method.ret_type.as_ref().is_some_and(lang_is_self_output) {
        return Some(format!("`{method_name}` must return `Self::Output`"));
    }
    None
}

fn validate_unary_operator_trait(
    tr: &HirTrait,
    method_name: &str,
    self_ty: &HirTypeRef,
) -> Option<String> {
    if !tr.generics.is_empty() {
        return Some(format!(
            "`#[lang = \"{method_name}\"]` trait must not be generic"
        ));
    }
    let Some(method) = tr.methods.iter().find(|m| m.name.0 == method_name) else {
        return Some(format!("must define a method named `{method_name}`"));
    };
    if !method.generics.is_empty() || !method.const_generics.is_empty() {
        return Some(format!(
            "`{method_name}` must not carry its own generic parameters"
        ));
    }
    if !lang_has_output_assoc_type(tr) {
        return Some(format!(
            "`#[lang = \"{method_name}\"]` trait must declare an associated type `Output`"
        ));
    }
    if method.params.len() != 1 {
        return Some(format!(
            "`{method_name}` must take exactly 1 parameter (`self`)"
        ));
    }
    if !lang_type_is_self(&method.params[0].ty, self_ty) {
        return Some(format!("first parameter of `{method_name}` must be `self`"));
    }
    if !method.ret_type.as_ref().is_some_and(lang_is_self_output) {
        return Some(format!("`{method_name}` must return `Self::Output`"));
    }
    None
}

fn validate_assign_operator_trait(
    tr: &HirTrait,
    method_name: &str,
    self_ty: &HirTypeRef,
) -> Option<String> {
    let Some(method) = tr.methods.iter().find(|m| m.name.0 == method_name) else {
        return Some(format!("must define a method named `{method_name}`"));
    };
    if !method.generics.is_empty() || !method.const_generics.is_empty() {
        return Some(format!(
            "`{method_name}` must not carry its own generic parameters"
        ));
    }
    if !lang_has_valid_rhs_generic(tr, self_ty) {
        return Some(format!(
            "`#[lang = \"{method_name}\"]` trait must have at most one \
             generic type parameter `Rhs = Self`"
        ));
    }
    if method.params.len() != 2 {
        return Some(format!(
            "`{method_name}` must take exactly 2 parameters (`&mut self` and `rhs`)"
        ));
    }
    if !matches!(&method.params[0].ty,
        HirTypeRef::Ref(inner, true) if lang_type_is_self(inner, self_ty))
    {
        return Some(format!(
            "first parameter of `{method_name}` must be `&mut self`"
        ));
    }
    if !lang_type_is_rhs(tr, &method.params[1].ty) {
        return Some(format!(
            "second parameter of `{method_name}` must match `Rhs`"
        ));
    }
    if !lang_returns_unit(method) {
        return Some(format!("`{method_name}` must return `()` (unit)"));
    }
    None
}

fn validate_comparison_method_sig(
    tr: &HirTrait,
    method_name: &str,
    self_ty: &HirTypeRef,
) -> Option<String> {
    let Some(method) = tr.methods.iter().find(|m| m.name.0 == method_name) else {
        return Some(format!("must define a method named `{method_name}`"));
    };
    if !method.generics.is_empty() || !method.const_generics.is_empty() {
        return Some(format!(
            "`{method_name}` must not carry its own generic parameters"
        ));
    }
    if method.params.len() != 2 {
        return Some(format!(
            "`{method_name}` must take exactly 2 parameters (`&self` and `&Rhs`)"
        ));
    }
    if !matches!(&method.params[0].ty,
        HirTypeRef::Ref(inner, false) if lang_type_is_self(inner, self_ty))
    {
        return Some(format!(
            "first parameter of `{method_name}` must be `&self`"
        ));
    }
    if !lang_ref_is_rhs(tr, &method.params[1].ty) {
        return Some(format!(
            "second parameter of `{method_name}` must be `&Rhs`"
        ));
    }
    let is_bool = |ty: &HirTypeRef| {
        matches!(ty, HirTypeRef::Named(path)
            if path.as_single_name().is_some_and(|n| n.0 == "bool")
                && path.type_args.is_empty())
    };
    if !method.ret_type.as_ref().is_some_and(is_bool) {
        return Some(format!("`{method_name}` must return `bool`"));
    }
    None
}

fn validate_partial_cmp(tr: &HirTrait, self_ty: &HirTypeRef) -> Option<String> {
    let method = tr
        .methods
        .iter()
        .find(|method| method.name.0 == "partial_cmp")?;
    if !method.generics.is_empty() || !method.const_generics.is_empty() {
        return Some("`partial_cmp` must not carry its own generic parameters".into());
    }
    if method.params.len() != 2 {
        return Some("`partial_cmp` must take exactly 2 parameters (`&self` and `&Rhs`)".into());
    }
    if !matches!(&method.params[0].ty,
        HirTypeRef::Ref(inner, false) if lang_type_is_self(inner, self_ty))
    {
        return Some("first parameter of `partial_cmp` must be `&self`".into());
    }
    if !lang_ref_is_rhs(tr, &method.params[1].ty) {
        return Some("second parameter of `partial_cmp` must be `&Rhs`".into());
    }
    if !method
        .ret_type
        .as_ref()
        .is_some_and(lang_is_option_ordering)
    {
        return Some("`partial_cmp` must return `Option<Ordering>`".into());
    }
    None
}

fn lang_is_ordering(ty: &HirTypeRef) -> bool {
    matches!(
        ty,
        HirTypeRef::Named(path)
            if path.segments.last().is_some_and(|name| name.0 == "Ordering")
                && path.type_args.is_empty()
    )
}

fn lang_is_option_ordering(ty: &HirTypeRef) -> bool {
    matches!(
        ty,
        HirTypeRef::Named(path)
            if path.segments.last().is_some_and(|name| name.0 == "Option")
                && path.type_args.len() == 1
                && lang_is_ordering(&path.type_args[0])
    )
}

fn validate_ord_cmp(tr: &HirTrait, self_ty: &HirTypeRef) -> Option<String> {
    if !tr.generics.is_empty() {
        return Some("`Ord` trait must not be generic".into());
    }
    let Some(method) = tr.methods.iter().find(|m| m.name.0 == "cmp") else {
        return Some("must define a method named `cmp`".into());
    };
    if !method.generics.is_empty() || !method.const_generics.is_empty() {
        return Some("`cmp` must not carry its own generic parameters".into());
    }
    if method.params.len() != 2 {
        return Some("`cmp` must take exactly 2 parameters (`&self` and `&Self`)".into());
    }
    if !matches!(&method.params[0].ty,
        HirTypeRef::Ref(inner, false) if lang_type_is_self(inner, self_ty))
    {
        return Some("first parameter of `cmp` must be `&self`".into());
    }
    if !matches!(&method.params[1].ty,
        HirTypeRef::Ref(inner, false) if lang_type_is_self(inner, self_ty))
    {
        return Some("second parameter of `cmp` must be `&Self`".into());
    }
    if !method.ret_type.as_ref().is_some_and(lang_is_ordering) {
        return Some("`cmp` must return `Ordering`".into());
    }
    None
}
