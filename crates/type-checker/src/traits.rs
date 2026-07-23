use std::collections::{HashMap, HashSet};

use hir::item_tree::{HirFunction, HirImpl, HirTrait, HirTypeRef, TypeAliasId};

use crate::{checker::TypeChecker, types::Type};

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

        for imp in impls {
            self.check_impl_decl(&imp);
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
        self.check_supertrait_dependencies(&self_ty_text, trait_id, imp.self_ty_range);
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

            self.expect_method_signature(&tr.name.0, imp, required, actual);
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
        span: rowan::TextRange,
    ) {
        let supertraits = self.hir.item_tree.traits[trait_id].supertraits.clone();
        for supertrait in supertraits {
            let Some(required_trait) = self.resolve_trait_ref(&supertrait.trait_ty) else {
                continue;
            };
            if !self.impl_exists_for_trait(required_trait, self_ty_text) {
                self.diagnostic(
                    "E0036",
                    format!(
                        "impl `{}` for `{}` requires `{}`",
                        self.hir.item_tree.traits[trait_id].name.0,
                        self_ty_text,
                        self.hir.item_tree.traits[required_trait].name.0
                    ),
                    Some(span),
                );
            }
        }
    }

    fn impl_exists_for_trait(
        &mut self,
        required_trait: hir::item_tree::TraitId,
        self_ty_text: &str,
    ) -> bool {
        let impls = self
            .hir
            .item_tree
            .impls
            .iter()
            .map(|(_, imp)| imp.clone())
            .collect::<Vec<_>>();
        impls.into_iter().any(|imp| {
            imp.trait_ty
                .as_ref()
                .and_then(|trait_ty| self.resolve_trait_ref(trait_ty))
                == Some(required_trait)
                && self.display_type_ref(&imp.self_ty) == self_ty_text
        })
    }

    fn expect_method_signature(
        &mut self,
        trait_name: &str,
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
        params.insert("Self".into(), self_ty);

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
