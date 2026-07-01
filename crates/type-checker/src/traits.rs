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
            .map(|(_, tr)| tr.clone())
            .collect::<Vec<_>>();

        for tr in traits {
            self.check_trait_decl(&tr);
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

    fn check_trait_decl(&mut self, tr: &HirTrait) {
        let mut methods = HashSet::new();
        for method in &tr.methods {
            if !methods.insert(method.name.0.clone()) {
                self.diagnostic(
                    "E0020",
                    format!(
                        "trait `{}` has duplicate method `{}`",
                        tr.name.0, method.name.0
                    ),
                    None,
                );
            }

            if method.has_body {
                self.diagnostic(
                    "E0021",
                    format!(
                        "trait method `{}::{}` must not have a body",
                        tr.name.0, method.name.0
                    ),
                    None,
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
                    None,
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
                None,
            );
            return;
        };

        let tr = self.hir.item_tree.traits[trait_id].clone();
        self.check_trait_impl(&self_ty_text, &tr, imp);
        self.check_lang_trait_dependencies(&self_ty_text, trait_id);
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
                    None,
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
                    None,
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
                self.diagnostic(
                    "E0026",
                    format!(
                        "impl for `{}` of trait `{}` missing method `{}`",
                        self_ty_text, tr.name.0, required.name.0
                    ),
                    None,
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
                    None,
                );
            }
        }
    }

    fn check_lang_trait_dependencies(
        &mut self,
        self_ty_text: &str,
        trait_id: hir::item_tree::TraitId,
    ) {
        let Some(lang) = self.trait_lang(trait_id).map(str::to_string) else {
            return;
        };
        let deps: &[(&str, &str)] = match lang.as_str() {
            "eq" => &[("partial_eq", "PartialEq")],
            "partial_ord" => &[("partial_eq", "PartialEq")],
            "ord" => &[("eq", "Eq"), ("partial_ord", "PartialOrd")],
            _ => &[],
        };

        for (required_lang, required_name) in deps {
            if !self.impl_exists_for_lang_trait(required_lang, self_ty_text) {
                self.diagnostic(
                    "E0036",
                    format!(
                        "impl `{}` for `{}` requires `{}`",
                        self.hir.item_tree.traits[trait_id].name.0, self_ty_text, required_name
                    ),
                    None,
                );
            }
        }
    }

    fn impl_exists_for_lang_trait(&mut self, lang: &str, self_ty_text: &str) -> bool {
        let Some(required_trait) = self.find_lang_trait(lang) else {
            return false;
        };
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
        let mut params =
            crate::lowering::generic_param_map(imp.generics.iter().map(|name| name.0.as_str()));
        let self_ty = self.lower_type_ref_with_params(&imp.self_ty, &params);
        params.insert("Self".into(), self_ty);

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
                None,
            );
        }

        for (index, expected_param) in expected.params.iter().enumerate() {
            let Some(actual_param) = actual.params.get(index) else {
                continue;
            };

            let expected_ty = self.lower_type_ref_with_params(&expected_param.ty, &params);
            let actual_ty = self.lower_type_ref_with_params(&actual_param.ty, &params);
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
                    None,
                );
            }
        }

        let expected_ret = expected
            .ret_type
            .as_ref()
            .map(|ty| self.lower_type_ref_with_params(ty, &params))
            .unwrap_or(Type::Unit);
        let actual_ret = actual
            .ret_type
            .as_ref()
            .map(|ty| self.lower_type_ref_with_params(ty, &params))
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
                None,
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
                format!("[{}; {}]", self.type_ref_source_text(inner), len)
            }
            HirTypeRef::Ptr { mutable, inner } => {
                let kind = if *mutable { "*mut" } else { "*const" };
                format!("{kind} {}", self.type_ref_source_text(inner))
            }
            HirTypeRef::Unknown => "_".to_string(),
            HirTypeRef::Error => "<error>".to_string(),
        }
    }
}
