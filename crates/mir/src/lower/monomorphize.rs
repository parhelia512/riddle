use super::*;

impl<'a> LowerCtx<'a> {
    pub(super) fn mono_method_name(
        &mut self,
        fid: hir::item_tree::FunctionId,
        base: ExprId,
        rhs: Option<ExprId>,
    ) -> Option<String> {
        let body_id = self.current_body?;
        let receiver_ty = self.type_result.expr_types.get(&(body_id, base))?;
        let rhs_ty = rhs.and_then(|rhs| self.type_result.expr_types.get(&(body_id, rhs)));
        self.mono_method_name_for_receiver(fid, receiver_ty, rhs_ty)
    }

    pub(super) fn mono_method_name_for_receiver(
        &mut self,
        fid: hir::item_tree::FunctionId,
        receiver_ty: &type_checker::Type,
        rhs_ty: Option<&type_checker::Type>,
    ) -> Option<String> {
        let receiver_ty = self.substitute_tc_type(receiver_ty);
        if self.default_methods.contains_key(&fid) {
            let receiver_mir_ty = self.convert_type(&receiver_ty);
            let trait_id = self.default_methods[&fid];
            let trait_generics = self.hir.item_tree.traits[trait_id].generics.clone();
            let rhs_ty = rhs_ty.map(|ty| self.substitute_tc_type(ty));
            let trait_args = trait_generics
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    if index == 0 {
                        rhs_ty.clone().unwrap_or_else(|| receiver_ty.clone())
                    } else {
                        receiver_ty.clone()
                    }
                })
                .collect::<Vec<_>>();
            let suffix = std::iter::once(mono_type_name(&receiver_mir_ty))
                .chain(
                    trait_args
                        .iter()
                        .map(|arg| mono_type_name(&self.convert_type(arg))),
                )
                .collect::<Vec<_>>()
                .join("_");
            let key = (fid, suffix.clone());
            if let Some(name) = self.mono_methods.get(&key) {
                return Some(name.clone());
            }

            let mono_name = format!("{}__{}", self.method_symbol_base(fid), suffix);
            self.mono_methods.insert(key, mono_name.clone());
            let mut tc_subst = HashMap::from([("Self".into(), receiver_ty)]);
            tc_subst.extend(
                trait_generics
                    .iter()
                    .zip(trait_args)
                    .map(|(name, ty)| (name.0.clone(), ty)),
            );
            let mir_subst = tc_subst
                .iter()
                .map(|(name, ty)| (name.clone(), self.convert_type(ty)))
                .collect();
            let old_subst = std::mem::replace(&mut self.generic_subst, mir_subst);
            let old_tc_subst = std::mem::replace(&mut self.generic_tc_subst, tc_subst);
            let old_expr_cache = std::mem::take(&mut self.expr_cache);
            let old_scope_map = std::mem::take(&mut self.scope_map);
            let old_drop_scopes = std::mem::take(&mut self.drop_scopes);
            let old_drop_slots = std::mem::take(&mut self.drop_slots);
            let old_temporary_drop_scopes = std::mem::take(&mut self.temporary_drop_scopes);
            let old_temporary_drop_slots = std::mem::take(&mut self.temporary_drop_slots);
            let old_storage_bindings = std::mem::take(&mut self.storage_bindings);
            let old_parameter_storage = std::mem::take(&mut self.parameter_storage);
            let old_pattern_bindings = std::mem::take(&mut self.pattern_bindings);
            let old_capture_access = std::mem::take(&mut self.capture_access);
            let old_current_lambda = self.current_lambda;
            let old_current_body = self.current_body;
            let body_id = *self.hir.function_bodies.get(&fid)?;
            let func = self.lower_function(fid, mono_name.clone(), body_id);
            self.expr_cache = old_expr_cache;
            self.scope_map = old_scope_map;
            self.drop_scopes = old_drop_scopes;
            self.drop_slots = old_drop_slots;
            self.temporary_drop_scopes = old_temporary_drop_scopes;
            self.temporary_drop_slots = old_temporary_drop_slots;
            self.storage_bindings = old_storage_bindings;
            self.parameter_storage = old_parameter_storage;
            self.pattern_bindings = old_pattern_bindings;
            self.capture_access = old_capture_access;
            self.current_lambda = old_current_lambda;
            self.current_body = old_current_body;
            self.generic_subst = old_subst;
            self.generic_tc_subst = old_tc_subst;
            self.module.add_function(func);
            return Some(mono_name);
        }
        let imp = self.impl_for_method(fid)?.clone();
        if imp.generics.is_empty() && imp.const_generics.is_empty() {
            return None;
        }
        let receiver_mir_ty = self.convert_type(&receiver_ty);
        let subst = self
            .impl_mir_subst(&imp, &receiver_ty)
            .or_else(|| match &receiver_ty {
                type_checker::Type::Ref(inner, _) => self.impl_mir_subst(&imp, inner),
                _ => None,
            })?;
        let type_subst = subst
            .types
            .iter()
            .map(|(name, ty)| (name.as_str(), ty))
            .collect::<HashMap<_, _>>();
        let const_subst = subst
            .consts
            .iter()
            .map(|(name, value)| (name.as_str(), *value))
            .collect::<HashMap<_, _>>();
        let trait_args = match imp.trait_ty.as_ref() {
            Some(hir::item_tree::HirTypeRef::Named(path)) => path
                .type_args
                .iter()
                .map(|arg| {
                    mono_type_name(&self.convert_hir_type_with_substs(
                        arg,
                        &type_subst,
                        &const_subst,
                    ))
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let suffix = std::iter::once(mono_type_name(&receiver_mir_ty))
            .chain(trait_args)
            .collect::<Vec<_>>()
            .join("_");
        let key = (fid, suffix.clone());
        if let Some(name) = self.mono_methods.get(&key) {
            return Some(name.clone());
        }
        let original_name = self.method_symbol_base(fid);
        let mono_name = format!("{}__{}", original_name, suffix);
        self.mono_methods.insert(key, mono_name.clone());
        let old_subst = std::mem::replace(&mut self.generic_subst, subst.types);
        let old_tc_subst = std::mem::replace(&mut self.generic_tc_subst, subst.tc_types);
        let old_const_subst = std::mem::replace(&mut self.generic_const_subst, subst.consts);
        let old_expr_cache = std::mem::take(&mut self.expr_cache);
        let old_scope_map = std::mem::take(&mut self.scope_map);
        let old_drop_scopes = std::mem::take(&mut self.drop_scopes);
        let old_drop_slots = std::mem::take(&mut self.drop_slots);
        let old_temporary_drop_scopes = std::mem::take(&mut self.temporary_drop_scopes);
        let old_temporary_drop_slots = std::mem::take(&mut self.temporary_drop_slots);
        let old_storage_bindings = std::mem::take(&mut self.storage_bindings);
        let old_parameter_storage = std::mem::take(&mut self.parameter_storage);
        let old_pattern_bindings = std::mem::take(&mut self.pattern_bindings);
        let old_capture_access = std::mem::take(&mut self.capture_access);
        let old_current_lambda = self.current_lambda;
        let old_current_body = self.current_body;
        let body_id = *self.hir.function_bodies.get(&fid)?;
        let func = self.lower_function(fid, mono_name.clone(), body_id);
        self.expr_cache = old_expr_cache;
        self.scope_map = old_scope_map;
        self.drop_scopes = old_drop_scopes;
        self.drop_slots = old_drop_slots;
        self.temporary_drop_scopes = old_temporary_drop_scopes;
        self.temporary_drop_slots = old_temporary_drop_slots;
        self.storage_bindings = old_storage_bindings;
        self.parameter_storage = old_parameter_storage;
        self.pattern_bindings = old_pattern_bindings;
        self.capture_access = old_capture_access;
        self.current_lambda = old_current_lambda;
        self.current_body = old_current_body;
        self.generic_subst = old_subst;
        self.generic_tc_subst = old_tc_subst;
        self.generic_const_subst = old_const_subst;
        self.module.add_function(func);
        Some(mono_name)
    }

    pub(super) fn mono_function_name(
        &mut self,
        fid: hir::item_tree::FunctionId,
        callee: ExprId,
    ) -> Option<String> {
        let body_id = self.current_body?;
        let tc_args = self
            .type_result
            .generic_calls
            .get(&(body_id, callee))?
            .args
            .clone();
        self.mono_function_name_for_args(fid, &tc_args)
    }

    pub(super) fn mono_function_name_for_args(
        &mut self,
        fid: hir::item_tree::FunctionId,
        tc_args: &[type_checker::Type],
    ) -> Option<String> {
        if !self.hir.function_bodies.contains_key(&fid) {
            return None;
        }
        let function = self.hir.item_tree.functions[fid].clone();
        let imp = self.impl_for_method(fid).cloned();
        let outer_generics = imp
            .as_ref()
            .map(|imp| imp.generics.as_slice())
            .unwrap_or_default();
        let outer_const_generics = imp
            .as_ref()
            .map(|imp| imp.const_generics.as_slice())
            .unwrap_or_default();
        if outer_generics.is_empty()
            && outer_const_generics.is_empty()
            && function.generics.is_empty()
            && function.implicit_generics.is_empty()
            && function.const_generics.is_empty()
        {
            return None;
        }
        let tc_args = tc_args
            .iter()
            .map(|arg| self.substitute_tc_type(arg))
            .collect::<Vec<_>>();
        let type_names = outer_generics
            .iter()
            .chain(function.generics.iter())
            .chain(function.implicit_generics.iter())
            .map(|name| name.0.clone())
            .collect::<Vec<_>>();
        let const_names = outer_const_generics
            .iter()
            .chain(function.const_generics.iter())
            .map(|name| name.0.clone())
            .collect::<Vec<_>>();
        let tc_subst = type_names
            .iter()
            .chain(const_names.iter())
            .zip(tc_args.iter())
            .map(|(name, ty)| (name.clone(), ty.clone()))
            .collect::<HashMap<_, _>>();
        let outer_tc_subst = outer_generics
            .iter()
            .zip(tc_args.iter())
            .map(|(name, ty)| (name.0.clone(), ty.clone()))
            .chain(
                outer_const_generics
                    .iter()
                    .zip(tc_args.iter().skip(type_names.len()))
                    .map(|(name, ty)| (name.0.clone(), ty.clone())),
            )
            .collect::<HashMap<_, _>>();
        let mut subst = type_names
            .iter()
            .zip(tc_args.iter())
            .map(|(name, ty)| (name.clone(), self.convert_type(ty)))
            .collect::<HashMap<_, _>>();
        let const_subst = const_names
            .iter()
            .zip(tc_args.iter().skip(type_names.len()))
            .filter_map(|(name, ty)| tc_const_arg_to_usize(ty).map(|value| (name.clone(), value)))
            .collect::<HashMap<_, _>>();
        let self_tc_ty = imp
            .as_ref()
            .map(|imp| self.lower_hir_type_for_pattern(&imp.self_ty, &outer_tc_subst));
        let self_mir_ty = self_tc_ty.as_ref().map(|ty| self.convert_type(ty));
        if let Some(self_ty) = &self_mir_ty {
            subst.insert("Self".into(), self_ty.clone());
        }
        let mut suffix_args = self_mir_ty
            .as_ref()
            .map(|ty| vec![mono_type_name(ty)])
            .unwrap_or_default();
        let outer_const_start = type_names.len();
        let outer_const_end = outer_const_start + outer_const_generics.len();
        for (index, arg) in tc_args.iter().enumerate() {
            if self_mir_ty.is_some()
                && (index < outer_generics.len()
                    || (outer_const_start..outer_const_end).contains(&index))
            {
                continue;
            }
            suffix_args.push(
                tc_const_arg_to_usize(arg)
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| mono_type_name(&self.convert_type(arg))),
            );
        }
        let suffix = suffix_args.join("_");
        let key = (fid, suffix.clone());
        if let Some(name) = self.mono_functions.get(&key) {
            return Some(name.clone());
        }

        let mut tc_subst = tc_subst;
        if let Some(self_ty) = self_tc_ty {
            tc_subst.insert("Self".into(), self_ty);
        }
        let mono_name = format!("{}__{}", self.method_symbol_base(fid), suffix);
        self.mono_functions.insert(key, mono_name.clone());

        let old_subst = std::mem::replace(&mut self.generic_subst, subst);
        let old_tc_subst = std::mem::replace(&mut self.generic_tc_subst, tc_subst);
        let old_const_subst = std::mem::replace(&mut self.generic_const_subst, const_subst);
        let old_expr_cache = std::mem::take(&mut self.expr_cache);
        let old_scope_map = std::mem::take(&mut self.scope_map);
        let old_drop_scopes = std::mem::take(&mut self.drop_scopes);
        let old_drop_slots = std::mem::take(&mut self.drop_slots);
        let old_temporary_drop_scopes = std::mem::take(&mut self.temporary_drop_scopes);
        let old_temporary_drop_slots = std::mem::take(&mut self.temporary_drop_slots);
        let old_storage_bindings = std::mem::take(&mut self.storage_bindings);
        let old_parameter_storage = std::mem::take(&mut self.parameter_storage);
        let old_pattern_bindings = std::mem::take(&mut self.pattern_bindings);
        let old_capture_access = std::mem::take(&mut self.capture_access);
        let old_current_lambda = self.current_lambda;
        let old_current_body = self.current_body;
        let body_id = *self.hir.function_bodies.get(&fid)?;
        let func = self.lower_function(fid, mono_name.clone(), body_id);
        self.expr_cache = old_expr_cache;
        self.scope_map = old_scope_map;
        self.drop_scopes = old_drop_scopes;
        self.drop_slots = old_drop_slots;
        self.temporary_drop_scopes = old_temporary_drop_scopes;
        self.temporary_drop_slots = old_temporary_drop_slots;
        self.storage_bindings = old_storage_bindings;
        self.parameter_storage = old_parameter_storage;
        self.pattern_bindings = old_pattern_bindings;
        self.capture_access = old_capture_access;
        self.current_lambda = old_current_lambda;
        self.current_body = old_current_body;
        self.generic_subst = old_subst;
        self.generic_tc_subst = old_tc_subst;
        self.generic_const_subst = old_const_subst;
        self.module.add_function(func);
        Some(mono_name)
    }

    pub(super) fn impl_for_method(
        &self,
        fid: hir::item_tree::FunctionId,
    ) -> Option<&hir::item_tree::HirImpl> {
        self.method_impls
            .get(&fid)
            .map(|impl_id| &self.hir.item_tree.impls[*impl_id])
    }

    pub(super) fn builtin_operator_for_method(
        &self,
        fid: hir::item_tree::FunctionId,
    ) -> Option<BuiltinOperator> {
        let imp = self.impl_for_method(fid)?;
        if !imp.generics.is_empty() || !imp.const_generics.is_empty() {
            return None;
        }
        let scalar = primitive_scalar_name(&imp.self_ty)?;
        let trait_id = self.resolve_trait_ref(imp.trait_ty.as_ref()?)?;
        let trait_item = &self.hir.item_tree.traits[trait_id];
        let lang_item = self.type_result.trait_env.lang_items.lang_of(trait_id)?;
        let function = &self.hir.item_tree.functions[fid];
        if !function.generics.is_empty() || !function.const_generics.is_empty() {
            return None;
        }
        let method = function.name.0.as_str();
        let op = builtin_operator(lang_item.as_str(), method)?;
        builtin_operator_supports(op, scalar).then_some(())?;
        trait_operator_contract(trait_item, method, op).then_some(())?;
        self.impl_operator_contract(imp, function, op).then_some(op)
    }

    pub(super) fn impl_operator_contract(
        &self,
        imp: &hir::item_tree::HirImpl,
        function: &hir::item_tree::HirFunction,
        op: BuiltinOperator,
    ) -> bool {
        if !operator_params_match(function, &imp.self_ty, op) {
            return false;
        }
        match op {
            BuiltinOperator::Assign(_) => returns_unit(function),
            BuiltinOperator::Binary(_) | BuiltinOperator::Unary(_) => {
                let Some(ret) = function.ret_type.as_ref() else {
                    return false;
                };
                if type_matches_self(ret, &imp.self_ty) {
                    return true;
                }
                is_self_output(ret)
                    && imp.type_aliases.iter().any(|alias_id| {
                        let alias = &self.hir.item_tree.type_aliases[*alias_id];
                        alias.name.0 == "Output"
                            && alias
                                .ty
                                .as_ref()
                                .is_some_and(|ty| type_matches_self(ty, &imp.self_ty))
                    })
            }
        }
    }

    pub(super) fn function_name(&self, fid: hir::item_tree::FunctionId) -> String {
        self.static_method_name(fid)
            .unwrap_or_else(|| self.hir.item_tree.functions[fid].name.0.clone())
    }

    pub(super) fn method_symbol_base(&self, fid: hir::item_tree::FunctionId) -> String {
        let name = self.hir.item_tree.functions[fid].name.0.clone();
        let collides_with_free_function =
            self.hir
                .item_tree
                .functions
                .iter()
                .any(|(other_fid, function)| {
                    other_fid != fid
                        && !self.method_impls.contains_key(&other_fid)
                        && !self.default_methods.contains_key(&other_fid)
                        && function.name.0 == name
                });
        if collides_with_free_function {
            format!("method::{}::{name}", fid.into_raw().into_u32())
        } else if self.impl_for_method(fid).is_some_and(|imp| {
            let trait_args: &[hir::item_tree::HirTypeRef] = match &imp.trait_ty {
                Some(hir::item_tree::HirTypeRef::Named(path)) => &path.type_args,
                _ => &[],
            };
            self.method_impls.iter().any(|(other_fid, other_impl)| {
                let other = &self.hir.item_tree.impls[*other_impl];
                let other_trait_args: &[hir::item_tree::HirTypeRef] = match &other.trait_ty {
                    Some(hir::item_tree::HirTypeRef::Named(path)) => &path.type_args,
                    _ => &[],
                };
                *other_fid != fid
                    && self.hir.item_tree.functions[*other_fid].name.0 == name
                    && other.self_ty == imp.self_ty
                    && other_trait_args == trait_args
            })
        }) {
            format!("{name}__trait{}", fid.into_raw().into_u32())
        } else {
            name
        }
    }

    pub(super) fn static_method_name(&self, fid: hir::item_tree::FunctionId) -> Option<String> {
        let imp = self.impl_for_method(fid)?;
        if !imp.generics.is_empty() || !imp.const_generics.is_empty() {
            return None;
        }
        let self_ty = self.convert_hir_type(&imp.self_ty);
        let trait_args = match imp.trait_ty.as_ref() {
            Some(hir::item_tree::HirTypeRef::Named(path)) => path
                .type_args
                .iter()
                .map(|arg| mono_type_name(&self.convert_hir_type(arg)))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let suffix = std::iter::once(mono_type_name(&self_ty))
            .chain(trait_args)
            .collect::<Vec<_>>()
            .join("_");
        Some(format!("{}__{}", self.method_symbol_base(fid), suffix))
    }

    pub(super) fn impl_self_mir_type(&self, fid: hir::item_tree::FunctionId) -> Option<Type> {
        self.impl_for_method(fid)
            .map(|imp| self.convert_hir_type(&imp.self_ty))
    }

    pub(super) fn impl_type_matches(
        &self,
        imp: &hir::item_tree::HirImpl,
        receiver_ty: &type_checker::Type,
    ) -> bool {
        let receiver_mir_ty = self.convert_type(receiver_ty);
        if imp.generics.is_empty() && imp.const_generics.is_empty() {
            return self.convert_hir_type(&imp.self_ty) == receiver_mir_ty;
        }
        self.impl_mir_subst(imp, receiver_ty)
            .map(|subst| {
                let type_subst = subst
                    .types
                    .iter()
                    .map(|(name, ty)| (name.as_str(), ty))
                    .collect::<HashMap<_, _>>();
                let const_subst = subst
                    .consts
                    .iter()
                    .map(|(name, value)| (name.as_str(), *value))
                    .collect::<HashMap<_, _>>();
                self.convert_hir_type_with_substs(&imp.self_ty, &type_subst, &const_subst)
                    == receiver_mir_ty
            })
            .unwrap_or(false)
    }

    pub(super) fn impl_trait_args_match(
        &self,
        imp: &hir::item_tree::HirImpl,
        receiver_ty: &type_checker::Type,
        rhs_ty: Option<&type_checker::Type>,
    ) -> bool {
        let Some(rhs_ty) = rhs_ty else {
            return true;
        };
        let Some(trait_ty) = imp.trait_ty.as_ref() else {
            return false;
        };
        let Some(trait_id) = self.resolve_trait_ref(trait_ty) else {
            return false;
        };
        let tr = &self.hir.item_tree.traits[trait_id];
        let Some(default) = tr.generic_defaults.first() else {
            return true;
        };
        let explicit = match trait_ty {
            hir::item_tree::HirTypeRef::Named(path) => path.type_args.first(),
            _ => None,
        };
        let Some(expected) = explicit.or(default.as_ref()) else {
            return false;
        };

        let receiver_ty = self.substitute_tc_type(receiver_ty);
        let receiver_mir = self.convert_type(&receiver_ty);
        let subst = self.impl_mir_subst(imp, &receiver_ty).unwrap_or_default();
        let mut type_subst = subst
            .types
            .iter()
            .map(|(name, ty)| (name.as_str(), ty))
            .collect::<HashMap<_, _>>();
        type_subst.insert("Self", &receiver_mir);
        let expected = self.convert_hir_type_with_substs(expected, &type_subst, &HashMap::new());
        let actual = self.convert_type(&self.substitute_tc_type(rhs_ty));
        expected == actual
    }

    pub(super) fn resolve_trait_ref(
        &self,
        ty: &hir::item_tree::HirTypeRef,
    ) -> Option<hir::item_tree::TraitId> {
        let hir::item_tree::HirTypeRef::Named(path) = ty else {
            return None;
        };
        let name = path.segments.last()?.0.as_str();
        self.hir
            .item_tree
            .traits
            .iter()
            .find_map(|(id, tr)| (tr.name.0 == name).then_some(id))
    }

    pub(super) fn impl_mir_subst(
        &self,
        imp: &hir::item_tree::HirImpl,
        receiver_ty: &type_checker::Type,
    ) -> Option<MirSubst> {
        let mut subst = MirSubst::default();
        match receiver_ty {
            type_checker::Type::Struct(_, args) | type_checker::Type::Enum(_, args) => {
                for (name, ty) in imp.generics.iter().zip(args.iter()) {
                    subst.types.insert(name.0.clone(), self.convert_type(ty));
                    subst.tc_types.insert(name.0.clone(), ty.clone());
                }
                for (name, ty) in imp
                    .const_generics
                    .iter()
                    .zip(args.iter().skip(imp.generics.len()))
                {
                    if let Some(value) = tc_const_arg_to_usize(ty) {
                        subst.consts.insert(name.0.clone(), value);
                        subst.tc_types.insert(name.0.clone(), ty.clone());
                    }
                }
                Some(subst)
            }
            type_checker::Type::Array(inner, len) => {
                let hir::item_tree::HirTypeRef::Array(pattern_inner, pattern_len) = &imp.self_ty
                else {
                    return None;
                };
                let generics = imp
                    .generics
                    .iter()
                    .map(|name| name.0.as_str())
                    .collect::<HashSet<_>>();
                if !self.collect_hir_type_subst(
                    pattern_inner,
                    inner,
                    &generics,
                    &mut subst.types,
                    &mut subst.tc_types,
                ) {
                    return None;
                }
                if let hir::item_tree::HirConstArg::Param(name) = pattern_len
                    && let Some(value) = len.as_usize()
                {
                    subst.consts.insert(name.0.clone(), value);
                    subst
                        .tc_types
                        .insert(name.0.clone(), type_checker::Type::Const(len.clone()));
                }
                Some(subst)
            }
            type_checker::Type::Slice(inner) => {
                let hir::item_tree::HirTypeRef::Slice(pattern_inner) = &imp.self_ty else {
                    return None;
                };
                let generics = imp
                    .generics
                    .iter()
                    .map(|name| name.0.as_str())
                    .collect::<HashSet<_>>();
                self.collect_hir_type_subst(
                    pattern_inner,
                    inner,
                    &generics,
                    &mut subst.types,
                    &mut subst.tc_types,
                )
                .then_some(subst)
            }
            type_checker::Type::Ref(inner, mutable) => {
                let hir::item_tree::HirTypeRef::Ref(pattern_inner, pattern_mut) = &imp.self_ty
                else {
                    return None;
                };
                if mutable != pattern_mut {
                    return None;
                }
                let generics = imp
                    .generics
                    .iter()
                    .map(|name| name.0.as_str())
                    .collect::<HashSet<_>>();
                let (pattern_inner, pattern_len, actual_inner, actual_len) =
                    match (pattern_inner.as_ref(), inner.as_ref()) {
                        (
                            hir::item_tree::HirTypeRef::Array(pattern_inner, pattern_len),
                            type_checker::Type::Array(actual_inner, actual_len),
                        ) => (
                            pattern_inner.as_ref(),
                            Some(pattern_len),
                            actual_inner.as_ref(),
                            Some(actual_len),
                        ),
                        (pattern_inner, actual_inner) => (pattern_inner, None, actual_inner, None),
                    };
                if !self.collect_hir_type_subst(
                    pattern_inner,
                    actual_inner,
                    &generics,
                    &mut subst.types,
                    &mut subst.tc_types,
                ) {
                    return None;
                }
                if let (Some(pattern_len), Some(actual_len)) = (pattern_len, actual_len)
                    && let hir::item_tree::HirConstArg::Param(name) = pattern_len
                    && let Some(value) = actual_len.as_usize()
                {
                    subst.consts.insert(name.0.clone(), value);
                    subst.tc_types.insert(
                        name.0.clone(),
                        type_checker::Type::Const(actual_len.clone()),
                    );
                }
                Some(subst)
            }
            type_checker::Type::Ptr { inner, mutable } => {
                let hir::item_tree::HirTypeRef::Ptr {
                    inner: pattern_inner,
                    mutable: pattern_mut,
                } = &imp.self_ty
                else {
                    return None;
                };
                if mutable != pattern_mut {
                    return None;
                }
                let generics = imp
                    .generics
                    .iter()
                    .map(|name| name.0.as_str())
                    .collect::<HashSet<_>>();
                self.collect_hir_type_subst(
                    pattern_inner,
                    inner,
                    &generics,
                    &mut subst.types,
                    &mut subst.tc_types,
                )
                .then_some(subst)
            }
            _ => None,
        }
    }

    pub(super) fn collect_hir_type_subst(
        &self,
        pattern: &hir::item_tree::HirTypeRef,
        actual: &type_checker::Type,
        generics: &HashSet<&str>,
        subst: &mut HashMap<String, Type>,
        tc_subst: &mut HashMap<String, type_checker::Type>,
    ) -> bool {
        match pattern {
            hir::item_tree::HirTypeRef::Named(path)
                if path
                    .as_single_name()
                    .is_some_and(|name| generics.contains(name.0.as_str())) =>
            {
                let name = path.as_single_name().unwrap().0.clone();
                match (subst.get(&name), tc_subst.get(&name)) {
                    (Some(existing), Some(tc_existing)) => {
                        existing == &self.convert_type(actual) && tc_existing == actual
                    }
                    (None, None) => {
                        subst.insert(name.clone(), self.convert_type(actual));
                        tc_subst.insert(name, actual.clone());
                        true
                    }
                    _ => false,
                }
            }
            hir::item_tree::HirTypeRef::Ref(inner, expected_mut) => match actual {
                type_checker::Type::Ref(actual_inner, actual_mut) => {
                    expected_mut == actual_mut
                        && self.collect_hir_type_subst(
                            inner,
                            actual_inner,
                            generics,
                            subst,
                            tc_subst,
                        )
                }
                _ => false,
            },
            hir::item_tree::HirTypeRef::Ptr { inner, .. } => match actual {
                type_checker::Type::Ptr {
                    inner: actual_inner,
                    ..
                } => self.collect_hir_type_subst(inner, actual_inner, generics, subst, tc_subst),
                _ => false,
            },
            hir::item_tree::HirTypeRef::Array(inner, _) => match actual {
                type_checker::Type::Array(actual_inner, _) => {
                    self.collect_hir_type_subst(inner, actual_inner, generics, subst, tc_subst)
                }
                _ => false,
            },
            hir::item_tree::HirTypeRef::Slice(inner) => match actual {
                type_checker::Type::Slice(actual_inner) => {
                    self.collect_hir_type_subst(inner, actual_inner, generics, subst, tc_subst)
                }
                _ => false,
            },
            _ => true,
        }
    }

    pub(super) fn convert_hir_type_with_substs(
        &self,
        t: &hir::item_tree::HirTypeRef,
        subst: &HashMap<&str, &Type>,
        const_subst: &HashMap<&str, usize>,
    ) -> Type {
        match t {
            hir::item_tree::HirTypeRef::Never => Type::Never,
            hir::item_tree::HirTypeRef::Named(path) => {
                if let Some(ResolvedName::TypeAlias(alias)) =
                    self.hir.type_resolutions.get(&path.range)
                    && let Some(ty) = &self.hir.item_tree.type_aliases[*alias].ty
                {
                    return self.convert_hir_type_with_substs(ty, subst, const_subst);
                }
                if let Some(name) = path.as_single_name().map(|name| name.0.as_str())
                    && let Some(ty) = subst.get(name)
                {
                    return (*ty).clone();
                }
                if let Some(name) = path.segments.last().map(|n| n.0.as_str()) {
                    for (sid, s) in self.hir.item_tree.structs.iter() {
                        if s.name.0 == name {
                            return self.convert_struct_type_from_hir_args_with_substs(
                                sid,
                                &path.type_args,
                                subst,
                                const_subst,
                            );
                        }
                    }
                    for (eid, e) in self.hir.item_tree.enums.iter() {
                        if e.name.0 == name {
                            return self.convert_enum_type_from_hir_args_with_substs(
                                eid,
                                &path.type_args,
                                subst,
                                const_subst,
                            );
                        }
                    }
                }
                self.convert_hir_type(t)
            }
            hir::item_tree::HirTypeRef::Ref(inner, mutable) => Type::Ref(
                Box::new(self.convert_hir_type_with_substs(inner, subst, const_subst)),
                *mutable,
            ),
            hir::item_tree::HirTypeRef::Ptr { inner, .. } => Type::Ptr(Box::new(
                self.convert_hir_type_with_substs(inner, subst, const_subst),
            )),
            hir::item_tree::HirTypeRef::Tuple(elems) if elems.is_empty() => Type::Unit,
            hir::item_tree::HirTypeRef::Tuple(elems) => Type::Tuple(
                elems
                    .iter()
                    .map(|elem| self.convert_hir_type_with_substs(elem, subst, const_subst))
                    .collect(),
            ),
            hir::item_tree::HirTypeRef::Slice(inner) => Type::Slice(Box::new(
                self.convert_hir_type_with_substs(inner, subst, const_subst),
            )),
            hir::item_tree::HirTypeRef::Array(inner, len) => Type::Array(
                Box::new(self.convert_hir_type_with_substs(inner, subst, const_subst)),
                self.hir_const_arg_to_usize(len, const_subst),
            ),
            hir::item_tree::HirTypeRef::Const(_) => Type::Unit,
            hir::item_tree::HirTypeRef::ImplTrait { .. } => Type::Unit,
            hir::item_tree::HirTypeRef::Unknown | hir::item_tree::HirTypeRef::Error => Type::Unit,
        }
    }

    pub(super) fn convert_struct_type_from_hir_args_with_substs(
        &self,
        sid: hir::item_tree::StructId,
        args: &[hir::item_tree::HirTypeRef],
        subst: &HashMap<&str, &Type>,
        const_subst: &HashMap<&str, usize>,
    ) -> Type {
        let s = &self.hir.item_tree.structs[sid];
        let type_count = s.generics.len();
        let mir_args = args
            .iter()
            .take(type_count)
            .map(|arg| self.convert_hir_type_with_substs(arg, subst, const_subst))
            .collect::<Vec<_>>();
        let const_args = args
            .iter()
            .skip(type_count)
            .map(|arg| self.hir_type_ref_const_arg_to_usize(arg, const_subst))
            .collect::<Vec<_>>();
        self.convert_struct_type_from_parts(sid, &mir_args, &const_args)
    }

    pub(super) fn convert_enum_type_from_hir_args_with_substs(
        &self,
        eid: hir::item_tree::EnumId,
        args: &[hir::item_tree::HirTypeRef],
        subst: &HashMap<&str, &Type>,
        const_subst: &HashMap<&str, usize>,
    ) -> Type {
        let e = &self.hir.item_tree.enums[eid];
        let type_count = e.generics.len();
        let mir_args = args
            .iter()
            .take(type_count)
            .map(|arg| self.convert_hir_type_with_substs(arg, subst, const_subst))
            .collect::<Vec<_>>();
        let const_args = args
            .iter()
            .skip(type_count)
            .map(|arg| self.hir_type_ref_const_arg_to_usize(arg, const_subst))
            .collect::<Vec<_>>();
        self.convert_enum_type_from_parts(eid, &mir_args, &const_args)
    }

    pub(super) fn hir_type_ref_const_arg_to_usize(
        &self,
        ty: &hir::item_tree::HirTypeRef,
        const_subst: &HashMap<&str, usize>,
    ) -> usize {
        match ty {
            hir::item_tree::HirTypeRef::Const(value) => {
                self.hir_const_arg_to_usize(value, const_subst)
            }
            hir::item_tree::HirTypeRef::Named(path) => path
                .as_single_name()
                .and_then(|name| const_subst.get(name.0.as_str()).copied())
                .or_else(|| {
                    path.as_single_name()
                        .and_then(|name| self.generic_const_subst.get(&name.0).copied())
                })
                .unwrap_or(0),
            _ => 0,
        }
    }

    pub(super) fn hir_const_arg_to_usize(
        &self,
        arg: &hir::item_tree::HirConstArg,
        const_subst: &HashMap<&str, usize>,
    ) -> usize {
        match arg {
            hir::item_tree::HirConstArg::Value(value) => *value,
            hir::item_tree::HirConstArg::Param(name) => const_subst
                .get(name.0.as_str())
                .copied()
                .or_else(|| self.generic_const_subst.get(&name.0).copied())
                .unwrap_or(0),
            hir::item_tree::HirConstArg::Unknown | hir::item_tree::HirConstArg::Error => 0,
        }
    }
}
