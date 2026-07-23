use std::collections::{HashMap, HashSet};

use hir::item_tree::{HirFunction, HirImpl, HirTrait, HirTypeRef, TraitId, TypeAliasId};
use rowan::TextRange;

use crate::{
    checker::TypeChecker,
    result::{LabelStyle, SourceLabel},
    types::{ConstArg, Type},
};

#[derive(Clone)]
struct CoherenceHeader {
    trait_id: TraitId,
    self_ty: Type,
    trait_args: Vec<Type>,
    self_ty_text: String,
    span: TextRange,
}

impl TypeChecker<'_> {
    pub(crate) fn check_traits(&mut self) {
        let traits = self
            .hir
            .item_tree
            .traits
            .iter()
            .map(|(id, tr)| (id, tr.clone()))
            .collect::<Vec<_>>();

        for (id, tr) in traits {
            self.check_trait_decl(id, &tr);
        }
    }

    pub(crate) fn check_impls(&mut self) {
        let impls = self
            .hir
            .item_tree
            .impls
            .iter()
            .map(|(_, imp)| imp.clone())
            .collect::<Vec<_>>();

        for imp in &impls {
            self.check_impl_decl(imp);
        }
        self.check_impl_coherence(&impls);
    }

    pub(crate) fn check_trait_ref_arities(&mut self) {
        let mut trait_refs = Vec::new();
        for (_, item) in self.hir.item_tree.structs.iter() {
            collect_bound_trait_refs(&mut trait_refs, &item.generic_bounds);
        }
        for (_, item) in self.hir.item_tree.enums.iter() {
            collect_bound_trait_refs(&mut trait_refs, &item.generic_bounds);
        }
        for (_, item) in self.hir.item_tree.functions.iter() {
            collect_bound_trait_refs(&mut trait_refs, &item.generic_bounds);
        }
        for (_, item) in self.hir.item_tree.traits.iter() {
            collect_bound_trait_refs(&mut trait_refs, &item.generic_bounds);
            collect_bound_trait_refs(&mut trait_refs, &item.supertraits);
            for method in item.methods.iter().filter(|method| !method.has_body) {
                collect_bound_trait_refs(&mut trait_refs, &method.generic_bounds);
            }
        }
        for (_, item) in self.hir.item_tree.impls.iter() {
            if let (Some(trait_ty), Some(range)) = (&item.trait_ty, item.trait_ty_range) {
                trait_refs.push((trait_ty.clone(), range));
            }
            collect_bound_trait_refs(&mut trait_refs, &item.generic_bounds);
        }

        for (trait_ref, range) in trait_refs {
            self.check_trait_ref_arity(&trait_ref, range);
        }
    }

    fn check_trait_ref_arity(&mut self, trait_ref: &HirTypeRef, range: TextRange) {
        let HirTypeRef::Named(path) = trait_ref else {
            return;
        };
        let Some(trait_id) = self.resolve_trait_ref(trait_ref) else {
            return;
        };
        let tr = &self.hir.item_tree.traits[trait_id];
        let total = tr.generics.len();
        let required = tr
            .generic_defaults
            .iter()
            .rposition(Option::is_none)
            .map_or(0, |index| index + 1);
        let actual = path.type_args.len();
        if (required..=total).contains(&actual) {
            return;
        }

        let expected = if required == total {
            total.to_string()
        } else {
            format!("between {required} and {total}")
        };
        self.diagnostic(
            "E0032",
            format!(
                "trait `{}` expects {expected} type argument(s), got {actual}",
                tr.name.0
            ),
            Some(range),
        );
    }

    fn check_impl_coherence(&mut self, impls: &[HirImpl]) {
        let mut headers = Vec::new();
        for (index, imp) in impls.iter().enumerate() {
            let Some(trait_ty) = imp.trait_ty.as_ref() else {
                continue;
            };
            let Some(trait_id) = self.resolve_trait_ref(trait_ty) else {
                continue;
            };

            let params = coherence_param_map(imp, index);
            let diagnostics_start = self.result.diagnostics.len();
            let self_ty =
                self.lower_type_ref_with_params_at(&imp.self_ty, &params, Some(imp.self_ty_range));
            let trait_args =
                self.trait_ref_args(trait_id, trait_ty, &self_ty, &params, imp.trait_ty_range);
            self.result.diagnostics.truncate(diagnostics_start);

            if !coherence_type_is_valid(&self_ty)
                || trait_args.iter().any(|ty| !coherence_type_is_valid(ty))
            {
                continue;
            }
            headers.push(CoherenceHeader {
                trait_id,
                self_ty,
                trait_args,
                self_ty_text: self.display_type_ref(&imp.self_ty),
                span: imp.self_ty_range,
            });
        }

        for (index, first) in headers.iter().enumerate() {
            for second in headers.iter().skip(index + 1) {
                if first.trait_id != second.trait_id || !coherence_headers_overlap(first, second) {
                    continue;
                }

                let trait_name = self.hir.item_tree.traits[first.trait_id].name.0.clone();
                self.diagnostic(
                    "E0047",
                    format!(
                        "conflicting implementations of trait `{trait_name}` for `{}`",
                        second.self_ty_text
                    ),
                    Some(second.span),
                );
                if let Some(diagnostic) = self.result.diagnostics.last_mut() {
                    diagnostic.labels.push(SourceLabel {
                        range: first.span,
                        message: "first implementation is here".into(),
                        style: LabelStyle::Secondary,
                    });
                }
            }
        }
    }

    pub(crate) fn validate_copy_impls(&mut self) {
        let Some(copy_trait) = self.find_lang_trait("copy") else {
            return;
        };
        let impls = self
            .hir
            .item_tree
            .impls
            .iter()
            .map(|(_, imp)| imp.clone())
            .collect::<Vec<_>>();

        for imp in impls {
            if imp
                .trait_ty
                .as_ref()
                .and_then(|trait_ty| self.resolve_trait_ref(trait_ty))
                != Some(copy_trait)
            {
                continue;
            }
            let params = crate::lowering::generic_param_map_with_consts(
                imp.generics.iter().map(|name| name.0.as_str()),
                imp.const_generics.iter().map(|name| name.0.as_str()),
            );
            let self_ty =
                self.lower_type_ref_with_params_at(&imp.self_ty, &params, Some(imp.self_ty_range));
            let bounds = self.lower_trait_env_bounds(&imp.generic_bounds, &params);
            let self_ty_text = self_ty.display(self.hir);
            let Some(fields) = self.copy_impl_fields(&self_ty) else {
                self.diagnostic(
                    "E0041",
                    format!("`Copy` cannot be implemented for `{self_ty_text}`"),
                    Some(imp.self_ty_range),
                );
                continue;
            };
            let non_copy = fields
                .into_iter()
                .filter_map(|(name, ty)| {
                    (!self
                        .result
                        .trait_env
                        .type_implements_assuming(&ty, copy_trait, &bounds))
                    .then(|| format!("{name}: {}", ty.display(self.hir)))
                })
                .collect::<Vec<_>>();
            if !non_copy.is_empty() {
                self.diagnostic(
                    "E0041",
                    format!(
                        "`Copy` impl for `{self_ty_text}` has non-Copy field(s): {}",
                        non_copy.join(", ")
                    ),
                    Some(imp.self_ty_range),
                );
            }
        }
    }

    fn copy_impl_fields(&mut self, ty: &Type) -> Option<Vec<(String, Type)>> {
        match ty {
            Type::Struct(struct_id, args) => {
                let strukt = self.hir.item_tree.structs[*struct_id].clone();
                let subst = strukt
                    .generics
                    .iter()
                    .chain(strukt.const_generics.iter())
                    .zip(args.iter())
                    .map(|(name, ty)| (name.0.clone(), ty.clone()))
                    .collect::<HashMap<_, _>>();
                Some(
                    strukt
                        .fields
                        .iter()
                        .map(|field| {
                            (
                                field.name.0.clone(),
                                self.lower_type_ref_with_params_at(
                                    &field.ty,
                                    &subst,
                                    Some(field.ty_range),
                                ),
                            )
                        })
                        .collect(),
                )
            }
            Type::Enum(enum_id, args) => {
                let enum_data = self.hir.item_tree.enums[*enum_id].clone();
                let subst = enum_data
                    .generics
                    .iter()
                    .chain(enum_data.const_generics.iter())
                    .zip(args.iter())
                    .map(|(name, ty)| (name.0.clone(), ty.clone()))
                    .collect::<HashMap<_, _>>();
                let mut fields = Vec::new();
                for variant in &enum_data.variants {
                    match &variant.kind {
                        hir::item_tree::HirVariantKind::Unit => {}
                        hir::item_tree::HirVariantKind::Tuple(items) => {
                            fields.extend(items.iter().enumerate().map(|(index, item)| {
                                (
                                    format!("{}.{index}", variant.name.0),
                                    self.lower_type_ref_with_params_at(
                                        item,
                                        &subst,
                                        variant
                                            .field_ranges
                                            .get(index)
                                            .copied()
                                            .or(Some(variant.name_range)),
                                    ),
                                )
                            }));
                        }
                        hir::item_tree::HirVariantKind::Struct(items) => {
                            fields.extend(items.iter().map(|item| {
                                (
                                    format!("{}.{}", variant.name.0, item.name.0),
                                    self.lower_type_ref_with_params_at(
                                        &item.ty,
                                        &subst,
                                        Some(item.ty_range),
                                    ),
                                )
                            }));
                        }
                    }
                }
                Some(fields)
            }
            Type::Tuple(items) => Some(
                items
                    .iter()
                    .enumerate()
                    .map(|(index, ty)| (index.to_string(), ty.clone()))
                    .collect(),
            ),
            Type::Array(inner, _) => Some(vec![("element".into(), *inner.clone())]),
            ty if ty.is_fundamentally_copy() => Some(Vec::new()),
            _ => None,
        }
    }

    fn check_trait_decl(&mut self, trait_id: hir::item_tree::TraitId, tr: &HirTrait) {
        for supertrait in &tr.supertraits {
            let Some(supertrait_id) = self.resolve_trait_ref(&supertrait.trait_ty) else {
                self.diagnostic(
                    "E0044",
                    format!(
                        "trait `{}` references unknown supertrait `{}`",
                        tr.name.0,
                        self.type_ref_source_text(&supertrait.trait_ty)
                    ),
                    Some(supertrait.trait_range),
                );
                continue;
            };
            if self.supertrait_reaches(supertrait_id, trait_id, &mut HashSet::new()) {
                self.diagnostic(
                    "E0044",
                    format!("supertrait cycle involving `{}`", tr.name.0),
                    Some(supertrait.trait_range),
                );
            }
        }

        let mut methods = HashSet::new();
        for method in &tr.methods {
            if !methods.insert(method.name.0.clone()) {
                self.diagnostic(
                    "E0020",
                    format!(
                        "trait `{}` has duplicate method `{}`",
                        tr.name.0, method.name.0
                    ),
                    Some(method.name_range),
                );
            }
        }

        let mut type_aliases = HashSet::new();
        for assoc in &tr.type_aliases {
            if !type_aliases.insert(assoc.name.0.clone()) {
                self.diagnostic(
                    "E0022",
                    format!(
                        "trait `{}` has duplicate associated type `{}`",
                        tr.name.0, assoc.name.0
                    ),
                    Some(assoc.name_range),
                );
            }
        }
    }

    fn check_impl_decl(&mut self, imp: &HirImpl) {
        self.check_impl_duplicates(imp);

        let Some(trait_ty) = imp.trait_ty.as_ref() else {
            return;
        };

        let self_ty_text = self.display_type_ref(&imp.self_ty);
        let Some(trait_id) = self.resolve_trait_ref(trait_ty) else {
            self.diagnostic(
                "E0023",
                format!(
                    "impl for `{}` references unknown trait `{}`",
                    self_ty_text,
                    self.type_ref_source_text(trait_ty)
                ),
                imp.trait_ty_range.or(Some(imp.self_ty_range)),
            );
            return;
        };

        let tr = self.hir.item_tree.traits[trait_id].clone();
        self.check_impl_paterson(imp, &self_ty_text);
        self.check_trait_impl(&self_ty_text, &tr, imp);
        self.check_supertrait_dependencies(&self_ty_text, trait_id, imp);
    }

    fn check_impl_paterson(&mut self, imp: &HirImpl, self_ty_text: &str) {
        let generics = imp
            .generics
            .iter()
            .map(|name| name.0.as_str())
            .collect::<HashSet<_>>();
        let self_size = type_ref_size(&imp.self_ty, &generics);

        for bound in &imp.generic_bounds {
            let bound_size = type_ref_size(&bound.target_ty, &generics);
            if bound_size < self_size {
                continue;
            }
            self.diagnostic(
                "E0037",
                format!(
                    "impl bound `{}` is not strictly smaller than implemented type `{}`",
                    self.type_ref_source_text(&bound.target_ty),
                    self_ty_text
                ),
                Some(bound.trait_range),
            );
        }
    }

    fn check_impl_duplicates(&mut self, imp: &HirImpl) {
        let impl_name = self.display_type_ref(&imp.self_ty);

        let mut methods = HashSet::new();
        for fid in &imp.methods {
            let method = &self.hir.item_tree.functions[*fid];
            if !methods.insert(method.name.0.clone()) {
                self.diagnostic(
                    "E0024",
                    format!(
                        "impl for `{}` has duplicate method `{}`",
                        impl_name, method.name.0
                    ),
                    Some(method.name_range),
                );
            }
        }

        let mut type_aliases = HashSet::new();
        for alias in &imp.type_aliases {
            let assoc = &self.hir.item_tree.type_aliases[*alias];
            if !type_aliases.insert(assoc.name.0.clone()) {
                self.diagnostic(
                    "E0025",
                    format!(
                        "impl for `{}` has duplicate associated type `{}`",
                        impl_name, assoc.name.0
                    ),
                    Some(assoc.name_range),
                );
            }
        }
    }

    fn check_trait_impl(&mut self, self_ty_text: &str, tr: &HirTrait, imp: &HirImpl) {
        let mut methods = HashMap::new();
        for fid in &imp.methods {
            let method = self.hir.item_tree.functions[*fid].clone();
            methods.entry(method.name.0.clone()).or_insert(method);
        }

        for required in &tr.methods {
            let Some(actual) = methods.get(&required.name.0) else {
                if required.has_body {
                    continue;
                }
                self.diagnostic(
                    "E0026",
                    format!(
                        "impl for `{}` of trait `{}` missing method `{}`",
                        self_ty_text, tr.name.0, required.name.0
                    ),
                    Some(imp.self_ty_range),
                );
                continue;
            };

            self.expect_method_signature(tr, imp, required, actual);
        }

        let provided_types = imp
            .type_aliases
            .iter()
            .map(|id| (*id, self.hir.item_tree.type_aliases[*id].name.0.clone()))
            .collect::<Vec<(TypeAliasId, String)>>();

        for required in &tr.type_aliases {
            let provided = provided_types
                .iter()
                .any(|(_, name)| *name == required.name.0);

            if required.ty.is_none() && !provided {
                self.diagnostic(
                    "E0027",
                    format!(
                        "impl for `{}` of trait `{}` missing associated type `{}`",
                        self_ty_text, tr.name.0, required.name.0
                    ),
                    Some(imp.self_ty_range),
                );
            }
        }
    }

    fn check_supertrait_dependencies(
        &mut self,
        self_ty_text: &str,
        trait_id: hir::item_tree::TraitId,
        imp: &HirImpl,
    ) {
        let params = crate::lowering::generic_param_map_with_consts(
            imp.generics.iter().map(|name| name.0.as_str()),
            imp.const_generics.iter().map(|name| name.0.as_str()),
        );
        let self_ty =
            self.lower_type_ref_with_params_at(&imp.self_ty, &params, Some(imp.self_ty_range));
        let trait_subst = self.trait_ref_subst(
            trait_id,
            imp.trait_ty.as_ref().unwrap(),
            &self_ty,
            &params,
            imp.trait_ty_range,
        );
        let supertraits = self.hir.item_tree.traits[trait_id].supertraits.clone();
        for supertrait in supertraits {
            let Some(required_trait) = self.resolve_trait_ref(&supertrait.trait_ty) else {
                continue;
            };
            let required_args = self.trait_ref_args(
                required_trait,
                &supertrait.trait_ty,
                &self_ty,
                &trait_subst,
                Some(supertrait.trait_range),
            );
            if !self.impl_exists_for_trait(required_trait, &self_ty, &required_args) {
                self.diagnostic(
                    "E0036",
                    format!(
                        "impl `{}` for `{}` requires `{}`",
                        self.hir.item_tree.traits[trait_id].name.0,
                        self_ty_text,
                        self.hir.item_tree.traits[required_trait].name.0
                    ),
                    Some(imp.self_ty_range),
                );
            }
        }
    }

    fn impl_exists_for_trait(
        &mut self,
        required_trait: hir::item_tree::TraitId,
        self_ty: &Type,
        required_args: &[Type],
    ) -> bool {
        let impls = self
            .hir
            .item_tree
            .impls
            .iter()
            .map(|(_, imp)| imp.clone())
            .collect::<Vec<_>>();
        impls.into_iter().any(|imp| {
            let Some(trait_ty) = imp.trait_ty.as_ref() else {
                return false;
            };
            if self.resolve_trait_ref(trait_ty) != Some(required_trait) {
                return false;
            }

            let params = crate::lowering::generic_param_map_with_consts(
                imp.generics.iter().map(|name| name.0.as_str()),
                imp.const_generics.iter().map(|name| name.0.as_str()),
            );
            let candidate_self =
                self.lower_type_ref_with_params_at(&imp.self_ty, &params, Some(imp.self_ty_range));
            let mut subst = HashMap::new();
            if !crate::lowering::collect_subst(&candidate_self, self_ty, &mut subst) {
                return false;
            }
            let mut params = params;
            params.extend(subst.clone());
            let candidate_args = self.trait_ref_args(
                required_trait,
                trait_ty,
                self_ty,
                &params,
                imp.trait_ty_range,
            );
            candidate_args.len() == required_args.len()
                && candidate_args
                    .iter()
                    .zip(required_args)
                    .all(|(candidate, required)| {
                        crate::lowering::collect_subst(candidate, required, &mut subst)
                    })
        })
    }

    fn expect_method_signature(
        &mut self,
        tr: &HirTrait,
        imp: &HirImpl,
        expected: &HirFunction,
        actual: &HirFunction,
    ) {
        let mut params = crate::lowering::generic_param_map_with_consts(
            imp.generics.iter().map(|name| name.0.as_str()),
            imp.const_generics.iter().map(|name| name.0.as_str()),
        );
        let self_ty =
            self.lower_type_ref_with_params_at(&imp.self_ty, &params, Some(imp.self_ty_range));
        params = self.trait_ref_subst(
            self.resolve_trait_ref(imp.trait_ty.as_ref().unwrap())
                .unwrap(),
            imp.trait_ty.as_ref().unwrap(),
            &self_ty,
            &params,
            imp.trait_ty_range,
        );
        let trait_name = tr.name.0.as_str();

        if expected.is_unsafe != actual.is_unsafe {
            self.diagnostic(
                "E0028",
                format!(
                    "impl method `{}` for trait `{}` safety mismatch: expected {}, got {}",
                    expected.name.0,
                    trait_name,
                    if expected.is_unsafe { "unsafe" } else { "safe" },
                    if actual.is_unsafe { "unsafe" } else { "safe" }
                ),
                Some(actual.name_range),
            );
        }

        if expected.params.len() != actual.params.len() {
            self.diagnostic(
                "E0028",
                format!(
                    "impl method `{}` for trait `{}` parameter count mismatch: expected {}, got {}",
                    expected.name.0,
                    trait_name,
                    expected.params.len(),
                    actual.params.len()
                ),
                Some(actual.name_range),
            );
        }

        for (index, expected_param) in expected.params.iter().enumerate() {
            let Some(actual_param) = actual.params.get(index) else {
                continue;
            };

            let expected_ty = self.lower_type_ref_with_params_at(
                &expected_param.ty,
                &params,
                Some(expected_param.ty_range),
            );
            let actual_ty = self.lower_type_ref_with_params_at(
                &actual_param.ty,
                &params,
                Some(actual_param.ty_range),
            );
            if !self.signature_types_match(&expected_ty, &actual_ty) {
                self.diagnostic(
                    "E0029",
                    format!(
                        "impl method `{}` for trait `{}` parameter {} type mismatch: expected {}, got {}",
                        expected.name.0,
                        trait_name,
                        index + 1,
                        expected_ty.display(self.hir),
                        actual_ty.display(self.hir)
                    ),
                    Some(actual_param.ty_range),
                );
            }
        }

        let expected_ret = expected
            .ret_type
            .as_ref()
            .map(|ty| {
                self.lower_type_ref_with_params_at(
                    ty,
                    &params,
                    expected.ret_type_range.or(Some(expected.name_range)),
                )
            })
            .unwrap_or(Type::Unit);
        let actual_ret = actual
            .ret_type
            .as_ref()
            .map(|ty| {
                self.lower_type_ref_with_params_at(
                    ty,
                    &params,
                    actual.ret_type_range.or(Some(actual.name_range)),
                )
            })
            .unwrap_or(Type::Unit);
        if !self.signature_types_match(&expected_ret, &actual_ret) {
            self.diagnostic(
                "E0030",
                format!(
                    "impl method `{}` for trait `{}` return type mismatch: expected {}, got {}",
                    expected.name.0,
                    trait_name,
                    expected_ret.display(self.hir),
                    actual_ret.display(self.hir)
                ),
                actual.ret_type_range.or(Some(actual.name_range)),
            );
        }
    }

    fn signature_types_match(&self, expected: &Type, actual: &Type) -> bool {
        expected.is_unknown_like()
            || actual.is_unknown_like()
            || expected == actual
            || self.numeric_assignable(expected, actual)
    }

    fn type_ref_source_text(&self, ty: &HirTypeRef) -> String {
        match ty {
            HirTypeRef::Named(path) => path.display(),
            HirTypeRef::Ref(inner, mutable) => {
                let kw = if *mutable { "&mut " } else { "&" };
                format!("{}{}", kw, self.type_ref_source_text(inner))
            }
            HirTypeRef::Tuple(elements) => {
                let inner = elements
                    .iter()
                    .map(|ty| self.type_ref_source_text(ty))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({inner})")
            }
            HirTypeRef::Array(inner, len) => {
                format!("[{}; {}]", self.type_ref_source_text(inner), len.display())
            }
            HirTypeRef::Const(value) => value.display(),
            HirTypeRef::Ptr { mutable, inner } => {
                let kind = if *mutable { "*mut" } else { "*const" };
                format!("{kind} {}", self.type_ref_source_text(inner))
            }
            HirTypeRef::Function {
                is_unsafe,
                params,
                ret,
            } => {
                let params = params
                    .iter()
                    .map(|param| self.type_ref_source_text(param))
                    .collect::<Vec<_>>()
                    .join(", ");
                let prefix = if *is_unsafe { "unsafe " } else { "" };
                format!(
                    "{prefix}fun({params}) -> {}",
                    self.type_ref_source_text(ret)
                )
            }
            HirTypeRef::Unknown => "_".to_string(),
            HirTypeRef::Error => "<error>".to_string(),
        }
    }
}

fn collect_bound_trait_refs(
    output: &mut Vec<(HirTypeRef, TextRange)>,
    bounds: &[hir::item_tree::HirGenericBound],
) {
    output.extend(
        bounds
            .iter()
            .map(|bound| (bound.trait_ty.clone(), bound.trait_range)),
    );
}

fn coherence_param_map(imp: &HirImpl, index: usize) -> HashMap<String, Type> {
    let prefix = format!("__coherence_{index}_");
    imp.generics
        .iter()
        .map(|name| (name.0.clone(), Type::Param(format!("{prefix}{}", name.0))))
        .chain(imp.const_generics.iter().map(|name| {
            (
                name.0.clone(),
                Type::Const(ConstArg::Param(format!("{prefix}{}", name.0))),
            )
        }))
        .collect()
}

fn coherence_type_is_valid(ty: &Type) -> bool {
    match ty {
        Type::Unknown | Type::Error | Type::InferVar(_) => false,
        Type::Ref(inner, _) | Type::Ptr { inner, .. } => coherence_type_is_valid(inner),
        Type::Tuple(elements) => elements.iter().all(coherence_type_is_valid),
        Type::Array(inner, len) => coherence_type_is_valid(inner) && coherence_const_is_valid(len),
        Type::Struct(_, args) | Type::Enum(_, args) => args.iter().all(coherence_type_is_valid),
        Type::Fn { params, ret, .. } => {
            params.iter().all(coherence_type_is_valid) && coherence_type_is_valid(ret)
        }
        Type::Const(value) => coherence_const_is_valid(value),
        _ => true,
    }
}

fn coherence_const_is_valid(value: &ConstArg) -> bool {
    !matches!(value, ConstArg::Unknown | ConstArg::Error)
}

fn coherence_headers_overlap(lhs: &CoherenceHeader, rhs: &CoherenceHeader) -> bool {
    let mut type_subst = HashMap::new();
    let mut const_subst = HashMap::new();
    unify_coherence_type(
        &lhs.self_ty,
        &rhs.self_ty,
        &mut type_subst,
        &mut const_subst,
    ) && lhs.trait_args.len() == rhs.trait_args.len()
        && lhs
            .trait_args
            .iter()
            .zip(&rhs.trait_args)
            .all(|(lhs, rhs)| unify_coherence_type(lhs, rhs, &mut type_subst, &mut const_subst))
}

fn unify_coherence_type(
    lhs: &Type,
    rhs: &Type,
    type_subst: &mut HashMap<String, Type>,
    const_subst: &mut HashMap<String, ConstArg>,
) -> bool {
    let lhs = resolve_coherence_type(lhs, type_subst);
    let rhs = resolve_coherence_type(rhs, type_subst);
    if lhs == rhs {
        return true;
    }
    match (&lhs, &rhs) {
        (Type::Param(name), other) => bind_coherence_type(name, other, type_subst),
        (other, Type::Param(name)) => bind_coherence_type(name, other, type_subst),
        (Type::Ref(lhs, lhs_mut), Type::Ref(rhs, rhs_mut)) => {
            lhs_mut == rhs_mut && unify_coherence_type(lhs, rhs, type_subst, const_subst)
        }
        (
            Type::Ptr {
                mutable: lhs_mut,
                inner: lhs,
            },
            Type::Ptr {
                mutable: rhs_mut,
                inner: rhs,
            },
        ) => lhs_mut == rhs_mut && unify_coherence_type(lhs, rhs, type_subst, const_subst),
        (Type::Tuple(lhs), Type::Tuple(rhs)) => {
            lhs.len() == rhs.len()
                && lhs
                    .iter()
                    .zip(rhs)
                    .all(|(lhs, rhs)| unify_coherence_type(lhs, rhs, type_subst, const_subst))
        }
        (Type::Array(lhs, lhs_len), Type::Array(rhs, rhs_len)) => {
            unify_coherence_type(lhs, rhs, type_subst, const_subst)
                && unify_coherence_const(lhs_len, rhs_len, const_subst)
        }
        (Type::Struct(lhs_id, lhs), Type::Struct(rhs_id, rhs)) => {
            lhs_id == rhs_id
                && lhs.len() == rhs.len()
                && lhs
                    .iter()
                    .zip(rhs)
                    .all(|(lhs, rhs)| unify_coherence_type(lhs, rhs, type_subst, const_subst))
        }
        (Type::Enum(lhs_id, lhs), Type::Enum(rhs_id, rhs)) => {
            lhs_id == rhs_id
                && lhs.len() == rhs.len()
                && lhs
                    .iter()
                    .zip(rhs)
                    .all(|(lhs, rhs)| unify_coherence_type(lhs, rhs, type_subst, const_subst))
        }
        (
            Type::Fn {
                is_unsafe: lhs_unsafe,
                kind: lhs_kind,
                params: lhs_params,
                ret: lhs_ret,
            },
            Type::Fn {
                is_unsafe: rhs_unsafe,
                kind: rhs_kind,
                params: rhs_params,
                ret: rhs_ret,
            },
        ) => {
            lhs_unsafe == rhs_unsafe
                && lhs_kind == rhs_kind
                && lhs_params.len() == rhs_params.len()
                && lhs_params
                    .iter()
                    .zip(rhs_params)
                    .all(|(lhs, rhs)| unify_coherence_type(lhs, rhs, type_subst, const_subst))
                && unify_coherence_type(lhs_ret, rhs_ret, type_subst, const_subst)
        }
        (Type::Const(lhs), Type::Const(rhs)) => unify_coherence_const(lhs, rhs, const_subst),
        _ => false,
    }
}

fn resolve_coherence_type(ty: &Type, subst: &HashMap<String, Type>) -> Type {
    let mut current = ty.clone();
    let mut seen = HashSet::new();
    loop {
        let Type::Param(name) = &current else {
            return current;
        };
        if !seen.insert(name.clone()) {
            return current;
        }
        let Some(next) = subst.get(name) else {
            return current;
        };
        current = next.clone();
    }
}

fn bind_coherence_type(name: &str, value: &Type, subst: &mut HashMap<String, Type>) -> bool {
    let value = resolve_coherence_type(value, subst);
    if value == Type::Param(name.to_string()) {
        return true;
    }
    if coherence_type_occurs(name, &value, subst) {
        return false;
    }
    subst.insert(name.to_string(), value);
    true
}

fn coherence_type_occurs(name: &str, ty: &Type, subst: &HashMap<String, Type>) -> bool {
    let ty = resolve_coherence_type(ty, subst);
    match &ty {
        Type::Param(other) => other == name,
        Type::Ref(inner, _) | Type::Ptr { inner, .. } => coherence_type_occurs(name, inner, subst),
        Type::Tuple(elements) => elements
            .iter()
            .any(|element| coherence_type_occurs(name, element, subst)),
        Type::Array(inner, _) => coherence_type_occurs(name, inner, subst),
        Type::Struct(_, args) | Type::Enum(_, args) => args
            .iter()
            .any(|arg| coherence_type_occurs(name, arg, subst)),
        Type::Fn { params, ret, .. } => {
            params
                .iter()
                .any(|param| coherence_type_occurs(name, param, subst))
                || coherence_type_occurs(name, ret, subst)
        }
        _ => false,
    }
}

fn unify_coherence_const(
    lhs: &ConstArg,
    rhs: &ConstArg,
    subst: &mut HashMap<String, ConstArg>,
) -> bool {
    let lhs = resolve_coherence_const(lhs, subst);
    let rhs = resolve_coherence_const(rhs, subst);
    if lhs == rhs {
        return true;
    }
    match (&lhs, &rhs) {
        (ConstArg::Param(name), value) => bind_coherence_const(name, value, subst),
        (value, ConstArg::Param(name)) => bind_coherence_const(name, value, subst),
        _ => false,
    }
}

fn resolve_coherence_const(value: &ConstArg, subst: &HashMap<String, ConstArg>) -> ConstArg {
    let mut current = value.clone();
    let mut seen = HashSet::new();
    loop {
        let ConstArg::Param(name) = &current else {
            return current;
        };
        if !seen.insert(name.clone()) {
            return current;
        }
        let Some(next) = subst.get(name) else {
            return current;
        };
        current = next.clone();
    }
}

fn bind_coherence_const(
    name: &str,
    value: &ConstArg,
    subst: &mut HashMap<String, ConstArg>,
) -> bool {
    let value = resolve_coherence_const(value, subst);
    if value == ConstArg::Param(name.to_string()) {
        return true;
    }
    subst.insert(name.to_string(), value);
    true
}

fn type_ref_size(ty: &HirTypeRef, generics: &HashSet<&str>) -> usize {
    match ty {
        HirTypeRef::Named(path)
            if matches!(path.anchor, hir::item_tree::PathAnchor::Plain)
                && path.segments.len() == 1
                && path.type_args.is_empty()
                && generics.contains(path.segments[0].0.as_str()) =>
        {
            0
        }
        HirTypeRef::Named(path) => {
            1 + path
                .type_args
                .iter()
                .map(|arg| type_ref_size(arg, generics))
                .sum::<usize>()
        }
        HirTypeRef::Ref(inner, _) | HirTypeRef::Ptr { inner, .. } => {
            1 + type_ref_size(inner, generics)
        }
        HirTypeRef::Tuple(elements) => {
            1 + elements
                .iter()
                .map(|element| type_ref_size(element, generics))
                .sum::<usize>()
        }
        HirTypeRef::Array(inner, _) => 1 + type_ref_size(inner, generics),
        HirTypeRef::Function { .. } => 1,
        HirTypeRef::Const(_) | HirTypeRef::Unknown | HirTypeRef::Error => 0,
    }
}
