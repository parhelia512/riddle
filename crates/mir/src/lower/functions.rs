use super::*;

impl<'a> LowerCtx<'a> {
    pub(super) fn reference_storage(&self, builder: &mut Builder, ty: Type) -> Value {
        if self.gc_enabled {
            builder.heap_alloc(ty)
        } else {
            builder.alloca(ty)
        }
    }

    pub(super) fn lower_expr_sequence(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        _owner: ExprId,
        _key_offset: usize,
        expressions: &[ExprId],
    ) -> Vec<Value> {
        expressions
            .iter()
            .copied()
            .map(|expression| self.lower_expr(builder, param_values, body, expression))
            .collect()
    }

    pub(super) fn lower_function(
        &mut self,
        fid: hir::item_tree::FunctionId,
        name: String,
        body_id: BodyId,
    ) -> Function {
        let body = &self.hir.bodies[body_id];
        self.expr_cache.clear();
        self.scope_map.clear();
        self.drop_scopes.clear();
        self.drop_slots.clear();
        self.temporary_drop_scopes.clear();
        self.temporary_drop_slots.clear();
        self.storage_bindings.clear();
        self.parameter_storage.clear();
        self.pattern_bindings.clear();
        self.capture_access.clear();
        self.current_lambda = None;
        self.current_body = Some(body_id);
        let old_current_function = self.current_function;
        let old_generic_subst = self.generic_subst.clone();
        let old_generic_tc_subst = self.generic_tc_subst.clone();
        let old_generic_const_subst = self.generic_const_subst.clone();
        let old_loop_targets = std::mem::take(&mut self.loop_targets);
        self.current_function = Some(fid);
        if !self.generic_subst.contains_key("Self")
            && let Some(self_ty) = self.impl_self_mir_type(fid)
        {
            self.generic_subst.insert("Self".into(), self_ty);
        }

        let func_item = &self.hir.item_tree.functions[fid];

        let ret_type = func_item
            .ret_type
            .as_ref()
            .map(|rt| self.convert_hir_type(rt))
            .unwrap_or(Type::Unit);

        let mut func = Function::new(name.clone(), ret_type);
        func.uses_c_string_abi = func_item.attrs.iter().any(|attr| attr.name.0 == "c_export");
        func.is_c_export =
            self.hir.item_tree.extern_function_ids.contains(&fid) || func.uses_c_string_abi;
        let mut param_values: Vec<Value> = Vec::new();

        for param in &func_item.params {
            let pty = self.convert_hir_type(&param.ty);
            let v = func.add_param(param.name.0.clone(), pty);
            param_values.push(v);
        }

        // Fix entry block start_value: params were allocated after the entry block
        // was created, so its start_value=0 overlaps with param values. Move it past
        // the last param.
        func.blocks[func.entry].start_value = func.next_value;

        // 降级函数体
        let is_unit_ret = func.ret_type == Type::Unit || func.ret_type == Type::Never;
        {
            let mut builder = Builder::new(&mut func);
            self.drop_scopes.push(Vec::new());
            for (index, (param, value)) in func_item.params.iter().zip(&param_values).enumerate() {
                let tc_ty = self.lower_hir_type_for_pattern(&param.ty, &self.generic_tc_subst);
                let needs_drop = self.type_needs_drop(&tc_ty, 0);
                let storage = if self.analysis.param_escapes(body_id, index) {
                    Some(self.reference_storage(&mut builder, self.convert_hir_type(&param.ty)))
                } else if param.is_mut
                    || self.analysis.param_needs_address(body_id, index)
                    || needs_drop
                {
                    Some(builder.alloca(self.convert_hir_type(&param.ty)))
                } else {
                    None
                };
                if let Some(storage) = storage {
                    builder.store(*value, storage);
                    self.parameter_storage
                        .insert(CaptureSource::Param(index), storage);
                }
                if needs_drop {
                    let place = storage.expect("Drop parameter has storage");
                    let source = CaptureSource::Param(index);
                    let slots = self.create_drop_slots(&mut builder, place, &tc_ty, Vec::new());
                    self.register_drop_slots(source, &slots);
                    self.drop_scopes[0].splice(0..0, slots.into_iter().rev());
                }
            }
            let root_result = self.lower_expr(&mut builder, &param_values, body, body.root_block);
            if builder.needs_return() {
                self.emit_current_drop_scope(&mut builder);
            }
            self.drop_scopes.pop();

            // Set the implicit return only when lowering did not terminate the block.
            if is_unit_ret && builder.needs_return() {
                builder.set_return(None);
            } else if builder.needs_return() {
                builder.set_return(Some(root_result));
            }
        }

        self.current_function = old_current_function;
        self.generic_subst = old_generic_subst;
        self.generic_tc_subst = old_generic_tc_subst;
        self.generic_const_subst = old_generic_const_subst;
        self.loop_targets = old_loop_targets;
        func
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn lower_lambda(
        &mut self,
        builder: &mut Builder,
        outer_params: &[Value],
        body_id: BodyId,
        expr_id: ExprId,
        params: &[hir::body::LambdaParam],
        lambda_body: ExprId,
        ty: &Type,
    ) -> Value {
        let Some(call_signature) = closure_call_signature(ty) else {
            return builder.unit_const();
        };
        let info = self
            .type_result
            .lambda_infos
            .get(&(body_id, expr_id))
            .cloned()
            .unwrap_or(LambdaInfo {
                captures: Vec::new(),
                kind: type_checker::ClosureKind::Fn,
            });
        let (name, needs_lowering) = match self.lambda_functions.get(&(body_id, expr_id)) {
            Some(name) => (name.clone(), false),
            None => {
                self.lambda_counter += 1;
                let name = format!("__riddle_lambda_{}", self.lambda_counter);
                self.lambda_functions
                    .insert((body_id, expr_id), name.clone());
                (name, true)
            }
        };
        let capture_types = info
            .captures
            .iter()
            .map(|capture| self.convert_type(&capture.ty))
            .collect::<Vec<_>>();
        let env_struct = StructType {
            name: format!("{}_env", name),
            fields: info
                .captures
                .iter()
                .zip(&capture_types)
                .enumerate()
                .map(|(index, (capture, ty))| {
                    let field_ty = match capture.mode {
                        CaptureMode::Shared | CaptureMode::Mutable => {
                            Type::Ptr(Box::new(ty.clone()))
                        }
                        CaptureMode::Value => ty.clone(),
                    };
                    (
                        format!(
                            "capture_{}_{}",
                            index,
                            self.capture_environment_name(capture)
                        ),
                        field_ty,
                    )
                })
                .collect(),
        };

        let heap_env = !info.captures.is_empty() && self.analysis.lambda_escapes(body_id, expr_id);
        let env_value = if info.captures.is_empty() {
            self.null_env(builder)
        } else {
            let env_ty = Type::Struct(env_struct.clone());
            let env_ptr = if heap_env {
                builder.heap_alloc(env_ty)
            } else {
                builder.alloca(env_ty)
            };
            for (index, (capture, capture_ty)) in
                info.captures.iter().zip(&capture_types).enumerate()
            {
                let field_ty = env_struct.fields[index].1.clone();
                let value = match capture.mode {
                    CaptureMode::Shared | CaptureMode::Mutable => {
                        self.capture_place(builder, outer_params, &capture.place, capture_ty)
                    }
                    CaptureMode::Value => {
                        self.capture_value(builder, outer_params, &capture.place, capture_ty)
                    }
                };
                let field = builder.field_ptr(env_ptr, index, field_ty);
                builder.store(value, field);
                if capture.mode == CaptureMode::Value && self.type_needs_drop(&capture.ty, 0) {
                    self.clear_drop_slots_for_capture(builder, &capture.place);
                }
            }
            builder.cast(CastOp::PtrToPtr, env_ptr, closure_env_type())
        };

        if needs_lowering {
            self.lower_lambda_function(
                body_id,
                expr_id,
                params,
                lambda_body,
                &name,
                &call_signature,
                &info,
                &capture_types,
                &env_struct,
            );
            self.lower_lambda_drop_function(
                &format!("{}_drop", name),
                &info,
                &capture_types,
                &env_struct,
                heap_env,
            );
        }

        let call = builder.function_ref(FuncRef::Local(name.clone()), Type::FnPtr(call_signature));
        let drop = builder.function_ref(
            FuncRef::Local(format!("{}_drop", name)),
            closure_drop_function_type(),
        );
        builder.struct_value(vec![call, env_value, drop], ty.clone())
    }

    pub(super) fn capture_environment_name(&self, capture: &LambdaCapture) -> String {
        let mut name = capture.name.clone();
        let root_ty = self.capture_root_type(&capture.place.source);
        for (position, projection) in capture.place.projections.iter().enumerate() {
            match projection {
                Projection::Field(index) => {
                    let field_name = if position == 0 {
                        match &root_ty {
                            Some(type_checker::Type::Struct(struct_id, _)) => {
                                self.hir.item_tree.structs[*struct_id]
                                    .fields
                                    .get(*index)
                                    .map(|field| field.name.0.as_str())
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };
                    name.push('_');
                    name.push_str(
                        &field_name
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("field_{index}")),
                    );
                }
                Projection::Index(Some(index)) => {
                    name.push_str(&format!("_{index}"));
                }
                Projection::Index(None) => name.push_str("_index"),
            }
        }
        name
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn lower_lambda_function(
        &mut self,
        body_id: BodyId,
        expr_id: ExprId,
        params: &[hir::body::LambdaParam],
        lambda_body: ExprId,
        name: &str,
        call_signature: &FnPtrType,
        info: &LambdaInfo,
        capture_types: &[Type],
        env_struct: &StructType,
    ) {
        let body = &self.hir.bodies[body_id];
        let old_expr_cache = std::mem::take(&mut self.expr_cache);
        let old_scope_map = std::mem::take(&mut self.scope_map);
        let old_drop_scopes = std::mem::take(&mut self.drop_scopes);
        let old_drop_slots = std::mem::take(&mut self.drop_slots);
        let old_temporary_drop_scopes = std::mem::take(&mut self.temporary_drop_scopes);
        let old_temporary_drop_slots = std::mem::take(&mut self.temporary_drop_slots);
        let old_storage_bindings = std::mem::take(&mut self.storage_bindings);
        let old_parameter_storage = std::mem::take(&mut self.parameter_storage);
        let old_pattern_bindings = std::mem::take(&mut self.pattern_bindings);
        let old_loop_targets = std::mem::take(&mut self.loop_targets);
        let old_capture_access = std::mem::take(&mut self.capture_access);
        let old_current_lambda = self.current_lambda.replace(expr_id);
        let old_current_body = self.current_body.replace(body_id);

        let mut function = Function::new(name.to_string(), (*call_signature.ret).clone());
        let env_param = function.add_param("__env".into(), closure_env_type());
        let param_values = params
            .iter()
            .zip(call_signature.params.iter().skip(1))
            .map(|(param, ty)| function.add_param(param.name.0.clone(), ty.clone()))
            .collect::<Vec<_>>();
        function.blocks[function.entry].start_value = function.next_value;
        let is_unit = matches!(function.ret_type, Type::Unit | Type::Never);
        {
            let mut lambda_builder = Builder::new(&mut function);
            self.drop_scopes.push(Vec::new());
            for (index, value) in param_values.iter().enumerate() {
                let ty = call_signature.params[index + 1].clone();
                let tc_ty =
                    self.lower_hir_type_for_pattern(&params[index].ty, &self.generic_tc_subst);
                let needs_drop = self.type_needs_drop(&tc_ty, 0);
                let storage = if self.analysis.lambda_param_escapes(body_id, expr_id, index) {
                    Some(self.reference_storage(&mut lambda_builder, ty.clone()))
                } else if params[index].is_mut
                    || self
                        .analysis
                        .lambda_param_needs_address(body_id, expr_id, index)
                    || needs_drop
                {
                    Some(lambda_builder.alloca(ty))
                } else {
                    None
                };
                if let Some(storage) = storage {
                    lambda_builder.store(*value, storage);
                    self.parameter_storage.insert(
                        CaptureSource::LambdaParam {
                            lambda: expr_id,
                            index,
                        },
                        storage,
                    );
                }
                if needs_drop {
                    let place = storage.expect("Drop lambda parameter has storage");
                    let source = CaptureSource::LambdaParam {
                        lambda: expr_id,
                        index,
                    };
                    let slots =
                        self.create_drop_slots(&mut lambda_builder, place, &tc_ty, Vec::new());
                    self.register_drop_slots(source, &slots);
                    self.drop_scopes[0].splice(0..0, slots.into_iter().rev());
                }
            }
            if !info.captures.is_empty() {
                let env_ptr_ty = Type::Ptr(Box::new(Type::Struct(env_struct.clone())));
                let env_ptr = lambda_builder.cast(CastOp::PtrToPtr, env_param, env_ptr_ty.clone());
                for (index, (capture, capture_ty)) in
                    info.captures.iter().zip(capture_types).enumerate()
                {
                    let field_ty = env_struct.fields[index].1.clone();
                    let field = lambda_builder.field_ptr(env_ptr, index, field_ty.clone());
                    let place = match capture.mode {
                        CaptureMode::Shared | CaptureMode::Mutable => {
                            lambda_builder.load(field, field_ty)
                        }
                        CaptureMode::Value => field,
                    };
                    self.capture_access.insert(
                        capture.place.clone(),
                        CaptureAccess {
                            place,
                            ty: capture_ty.clone(),
                        },
                    );
                }
            }
            let result = self.lower_expr(&mut lambda_builder, &param_values, body, lambda_body);
            if lambda_builder.needs_return() {
                self.emit_current_drop_scope(&mut lambda_builder);
            }
            self.drop_scopes.pop();
            if lambda_builder.needs_return() {
                lambda_builder.set_return((!is_unit).then_some(result));
            }
        }
        self.module.add_function(function);

        self.expr_cache = old_expr_cache;
        self.scope_map = old_scope_map;
        self.drop_scopes = old_drop_scopes;
        self.drop_slots = old_drop_slots;
        self.temporary_drop_scopes = old_temporary_drop_scopes;
        self.temporary_drop_slots = old_temporary_drop_slots;
        self.storage_bindings = old_storage_bindings;
        self.parameter_storage = old_parameter_storage;
        self.pattern_bindings = old_pattern_bindings;
        self.loop_targets = old_loop_targets;
        self.capture_access = old_capture_access;
        self.current_lambda = old_current_lambda;
        self.current_body = old_current_body;
    }

    pub(super) fn lower_lambda_drop_function(
        &mut self,
        name: &str,
        info: &LambdaInfo,
        capture_types: &[Type],
        env_struct: &StructType,
        heap_env: bool,
    ) {
        let mut function = Function::new(name.to_string(), Type::Unit);
        let env = function.add_param("__env".into(), closure_env_type());
        function.blocks[function.entry].start_value = function.next_value;
        {
            let mut builder = Builder::new(&mut function);
            let env_ptr_ty = Type::Ptr(Box::new(Type::Struct(env_struct.clone())));
            let env_ptr = builder.cast(CastOp::PtrToPtr, env, env_ptr_ty);
            for (index, (capture, capture_ty)) in
                info.captures.iter().zip(capture_types).enumerate()
            {
                if capture.mode == CaptureMode::Value && self.type_needs_drop(&capture.ty, 0) {
                    let field = builder.field_ptr(env_ptr, index, capture_ty.clone());
                    self.emit_drop_glue(&mut builder, field, &capture.ty);
                }
            }
            if heap_env {
                builder.heap_free(env_ptr);
            }
            if builder.needs_return() {
                builder.set_return(None);
            }
        }
        self.module.add_function(function);
    }

    pub(super) fn capture_value(
        &mut self,
        builder: &mut Builder,
        params: &[Value],
        capture: &CapturePlace,
        ty: &Type,
    ) -> Value {
        if let Some(access) = self.capture_access_for_place(builder, capture) {
            return builder.load(access.place, access.ty);
        }
        let place = self.capture_place(builder, params, capture, ty);
        builder.load(place, ty.clone())
    }

    pub(super) fn capture_place(
        &mut self,
        builder: &mut Builder,
        params: &[Value],
        capture: &CapturePlace,
        ty: &Type,
    ) -> Value {
        if let Some(access) = self.capture_access_for_place(builder, capture) {
            return access.place;
        }
        let root_ty = self.capture_root_mir_type(builder, params, &capture.source, ty);
        let root_place = match &capture.source {
            CaptureSource::Pattern(id) => self.binding_place(builder, *id),
            source @ CaptureSource::Param(..) => self.parameter_place(builder, source),
            source @ CaptureSource::LambdaParam { lambda, .. }
                if self.current_lambda == Some(*lambda) =>
            {
                self.parameter_place(builder, source)
            }
            CaptureSource::LambdaParam { .. } => None,
        }
        .unwrap_or_else(|| {
            let value = self.capture_root_value(builder, params, &capture.source, &root_ty);
            let place = self.reference_storage(builder, root_ty.clone());
            builder.store(value, place);
            place
        });
        self.project_capture_access(
            builder,
            CaptureAccess {
                place: root_place,
                ty: root_ty,
            },
            &capture.projections,
        )
        .map(|access| access.place)
        .unwrap_or(root_place)
    }

    pub(super) fn parameter_place(
        &self,
        _builder: &mut Builder,
        source: &CaptureSource,
    ) -> Option<Value> {
        self.parameter_storage.get(source).copied()
    }

    pub(super) fn capture_root_value(
        &mut self,
        builder: &mut Builder,
        params: &[Value],
        source: &CaptureSource,
        ty: &Type,
    ) -> Value {
        match source {
            CaptureSource::Pattern(id) => self
                .binding_value(builder, *id, ty)
                .unwrap_or_else(|| builder.unit_const()),
            CaptureSource::Param(index) => self
                .parameter_place(builder, source)
                .map(|place| builder.load(place, ty.clone()))
                .or_else(|| params.get(*index).copied())
                .unwrap_or_else(|| builder.unit_const()),
            CaptureSource::LambdaParam { lambda, index }
                if self.current_lambda == Some(*lambda) =>
            {
                self.parameter_place(builder, source)
                    .map(|place| builder.load(place, ty.clone()))
                    .or_else(|| params.get(*index).copied())
                    .unwrap_or_else(|| builder.unit_const())
            }
            CaptureSource::LambdaParam { .. } => builder.unit_const(),
        }
    }

    pub(super) fn capture_access_for_place(
        &mut self,
        builder: &mut Builder,
        requested: &CapturePlace,
    ) -> Option<CaptureAccess> {
        let (ancestor, access) = self
            .capture_access
            .iter()
            .filter(|(place, _)| place.is_prefix_of(requested))
            .max_by_key(|(place, _)| place.projections.len())
            .map(|(place, access)| (place.clone(), access.clone()))?;
        self.project_capture_access(
            builder,
            access,
            &requested.projections[ancestor.projections.len()..],
        )
    }

    pub(super) fn project_capture_access(
        &self,
        builder: &mut Builder,
        mut access: CaptureAccess,
        projections: &[Projection],
    ) -> Option<CaptureAccess> {
        for projection in projections {
            let ty = match (projection, &access.ty) {
                (Projection::Field(index), Type::Struct(strukt)) => {
                    strukt.fields.get(*index)?.1.clone()
                }
                (Projection::Field(index), Type::Tuple(elements)) => elements.get(*index)?.clone(),
                (Projection::Index(Some(_)), Type::Array(element, _)) => *element.clone(),
                (Projection::Index(None), _) => return None,
                _ => return None,
            };
            access.place = match projection {
                Projection::Field(index) => builder.field_ptr(access.place, *index, ty.clone()),
                Projection::Index(Some(index)) => {
                    let index = builder.iconst(*index as u64, IntTy::Usize);
                    builder.index_ptr(access.place, index, ty.clone())
                }
                Projection::Index(None) => return None,
            };
            access.ty = ty;
        }
        Some(access)
    }

    pub(super) fn capture_root_mir_type(
        &self,
        builder: &Builder,
        params: &[Value],
        source: &CaptureSource,
        fallback: &Type,
    ) -> Type {
        if let Some(tc_ty) = self.capture_root_type(source) {
            return self.convert_type(&tc_ty);
        }
        let index = match source {
            CaptureSource::Param(index) | CaptureSource::LambdaParam { index, .. } => *index,
            CaptureSource::Pattern(_) => return fallback.clone(),
        };
        params
            .get(index)
            .and_then(|value| {
                builder
                    .func
                    .params
                    .iter()
                    .find(|param| param.value == *value)
                    .map(|param| param.ty.clone())
            })
            .unwrap_or_else(|| fallback.clone())
    }

    pub(super) fn capture_root_type(&self, source: &CaptureSource) -> Option<type_checker::Type> {
        match source {
            CaptureSource::Pattern(id) => self.current_body.and_then(|body_id| {
                self.type_result
                    .pattern_binding_types
                    .get(&(body_id, *id))
                    .cloned()
            }),
            CaptureSource::Param(index) => self.current_function.and_then(|function| {
                self.hir.item_tree.functions[function]
                    .params
                    .get(*index)
                    .map(|param| self.lower_hir_type_for_pattern(&param.ty, &self.generic_tc_subst))
            }),
            CaptureSource::LambdaParam { lambda, index } => {
                let body_id = self.current_body?;
                let Expr::Lambda { params, .. } = &self.hir.bodies[body_id].exprs[*lambda] else {
                    return None;
                };
                params
                    .get(*index)
                    .map(|param| self.lower_hir_type_for_pattern(&param.ty, &self.generic_tc_subst))
            }
        }
    }

    pub(super) fn null_env(&self, builder: &mut Builder) -> Value {
        let zero = builder.iconst(0, IntTy::Usize);
        builder.cast(CastOp::IntToPtr, zero, closure_env_type())
    }

    pub(super) fn lower_function_value(
        &mut self,
        builder: &mut Builder,
        fid: hir::item_tree::FunctionId,
        args: &[type_checker::Type],
        ty: &Type,
    ) -> Value {
        let Some(signature) = closure_call_signature(ty) else {
            return builder.unit_const();
        };
        let args = args
            .iter()
            .map(|arg| self.substitute_tc_type(arg))
            .collect::<Vec<_>>();
        let key = (fid, args.clone());
        let adapter = if let Some(name) = self.function_adapters.get(&key) {
            name.clone()
        } else {
            let target = self
                .mono_function_name_for_args(fid, &args)
                .unwrap_or_else(|| self.function_name(fid));
            let name = format!("__riddle_fn_adapter_{}", target);
            self.function_adapters.insert(key, name.clone());

            let mut function = Function::new(name.clone(), (*signature.ret).clone());
            function.add_param("__env".into(), closure_env_type());
            let parameter_names = self.hir.item_tree.functions[fid]
                .params
                .iter()
                .map(|param| param.name.0.clone())
                .collect::<Vec<_>>();
            let arguments = signature
                .params
                .iter()
                .skip(1)
                .enumerate()
                .map(|(index, param_ty)| {
                    let param_name = parameter_names
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| format!("p{}", index));
                    function.add_param(param_name, param_ty.clone())
                })
                .collect::<Vec<_>>();
            function.blocks[function.entry].start_value = function.next_value;
            let is_extern = self.hir.item_tree.extern_function_ids.contains(&fid)
                && !self.hir.function_bodies.contains_key(&fid);
            let target = if is_extern {
                FuncRef::Extern(target)
            } else {
                FuncRef::Local(target)
            };
            {
                let mut adapter_builder = Builder::new(&mut function);
                let result = adapter_builder.call(target, arguments, (*signature.ret).clone());
                adapter_builder.set_return(
                    (!matches!(signature.ret.as_ref(), Type::Unit | Type::Never)).then_some(result),
                );
            }
            self.module.add_function(function);
            name
        };

        let call = builder.function_ref(FuncRef::Local(adapter), Type::FnPtr(signature));
        let env = self.null_env(builder);
        let drop_name = self.ensure_noop_closure_drop();
        let drop = builder.function_ref(FuncRef::Local(drop_name), closure_drop_function_type());
        builder.struct_value(vec![call, env, drop], ty.clone())
    }

    pub(super) fn ensure_noop_closure_drop(&mut self) -> String {
        let name = "__riddle_closure_drop_noop".to_string();
        if self
            .module
            .functions
            .values()
            .all(|function| function.name != name)
        {
            let mut function = Function::new(name.clone(), Type::Unit);
            function.add_param("__env".into(), closure_env_type());
            function.blocks[function.entry].start_value = function.next_value;
            Builder::new(&mut function).set_return(None);
            self.module.add_function(function);
        }
        name
    }
}
