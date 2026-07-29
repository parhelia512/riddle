use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};

use escape_analysis::EscapeResult;
use hir::{
    HirFile,
    body::{
        BinaryOp as HirBinOp, Body, BodyId, Expr, ExprId, LiteralPattern, MatchArm, PatId, Pattern,
        PatternBindingId, ResolvedName, Stmt, StmtId, UnaryOp as HirUnOp,
    },
    item_tree::{HirTypeRef, PathAnchor},
    place::Projection,
};
use type_checker::{
    CaptureMode, CapturePlace, CaptureSource, LambdaCapture, LambdaInfo, OperatorCall,
    PatternBindingMode, TypeCheckResult,
};

use crate::builder::Builder;
use crate::func::Function;
use crate::instr::*;
use crate::module::Module;
use crate::types::*;
use crate::value::{BlockId, FuncRef, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuiltinOperator {
    Binary(BinOp),
    Unary(UnOp),
    Assign(BinOp),
}

/// Lower a type-checked HIR module into MIR.
///
/// `analysis` determines whether each local variable escapes its scope;
/// non-escaping locals can be stack-allocated, while escaping ones require
/// GC-managed heap allocation in GC'd backends.
pub fn lower_hir(
    hir: &HirFile,
    type_result: &TypeCheckResult,
    escape_result: &EscapeResult,
    moved_exprs: &HashSet<(BodyId, ExprId)>,
) -> Module {
    let method_impls = hir
        .item_tree
        .impls
        .iter()
        .flat_map(|(impl_id, imp)| {
            imp.methods
                .iter()
                .copied()
                .map(move |function_id| (function_id, impl_id))
        })
        .collect();
    let default_methods = hir
        .item_tree
        .traits
        .iter()
        .flat_map(|(trait_id, tr)| {
            tr.default_methods
                .iter()
                .copied()
                .map(move |function_id| (function_id, trait_id))
        })
        .collect();
    let mut ctx = LowerCtx {
        hir,
        type_result,
        analysis: escape_result,
        moved_exprs,
        module: Module::new("main"),
        method_impls,
        default_methods,
        expr_cache: HashMap::new(),
        current_body: None,
        current_function: None,
        scope_map: HashMap::new(),
        drop_scopes: Vec::new(),
        drop_slots: HashMap::new(),
        storage_bindings: HashSet::new(),
        parameter_storage: HashMap::new(),
        pattern_bindings: Vec::new(),
        generic_subst: HashMap::new(),
        generic_tc_subst: HashMap::new(),
        generic_const_subst: HashMap::new(),
        mono_functions: HashMap::new(),
        mono_methods: HashMap::new(),
        loop_targets: Vec::new(),
        lambda_functions: HashMap::new(),
        function_adapters: HashMap::new(),
        capture_access: HashMap::new(),
        current_lambda: None,
        lambda_counter: 0,
        active_consts: HashSet::new(),
    };

    // 遍历所有有函数体的函数
    for (fid, func) in hir.item_tree.functions.iter() {
        if ctx.builtin_operator_for_method(fid).is_some()
            || ctx.default_methods.contains_key(&fid)
            || (hir.std_loaded
                && hir.package_for_range(func.name_range).is_none()
                && func.attrs.iter().any(|attr| attr.name.0 == "builtin"))
            || !func.generics.is_empty()
            || !func.implicit_generics.is_empty()
            || ctx
                .impl_for_method(fid)
                .map(|imp| !imp.generics.is_empty() || !imp.const_generics.is_empty())
                .unwrap_or(false)
        {
            continue;
        }
        if let Some(body_id) = hir.function_bodies.get(&fid).copied() {
            let mir_func = ctx.lower_function(fid, ctx.function_name(fid), body_id);
            ctx.module.add_function(mir_func);
        }
    }

    // 注册 extern 函数声明
    let extern_funcs: Vec<_> = hir
        .item_tree
        .extern_function_ids
        .iter()
        .filter(|&&fid| !hir.function_bodies.contains_key(&fid))
        .map(|&fid| {
            let func = &hir.item_tree.functions[fid];
            (func.name.0.clone(), func)
        })
        .collect();
    for (name, func) in &extern_funcs {
        let params: Vec<Type> = func
            .params
            .iter()
            .map(|p| ctx.convert_hir_type(&p.ty))
            .collect();
        let ret_type = func
            .ret_type
            .as_ref()
            .map(|rt| ctx.convert_hir_type(rt))
            .unwrap_or(Type::Unit);
        ctx.module.add_extern(name.clone(), params, ret_type);
    }

    ctx.module
}

struct LowerCtx<'a> {
    hir: &'a HirFile,
    type_result: &'a TypeCheckResult,
    analysis: &'a EscapeResult,
    moved_exprs: &'a HashSet<(BodyId, ExprId)>,
    module: Module,
    method_impls: HashMap<hir::item_tree::FunctionId, hir::item_tree::ImplId>,
    default_methods: HashMap<hir::item_tree::FunctionId, hir::item_tree::TraitId>,
    expr_cache: HashMap<ExprId, Value>,
    /// The BodyId currently being lowered, used to look up expr_types.
    current_body: Option<BodyId>,
    current_function: Option<hir::item_tree::FunctionId>,
    /// Maps a `let` binding → its Value (or storage pointer, see below).
    scope_map: HashMap<PatternBindingId, Value>,
    drop_scopes: Vec<Vec<DropSlot>>,
    drop_slots: HashMap<CaptureSource, Vec<DropSlot>>,
    /// Bindings backed by storage rather than a direct SSA value.
    storage_bindings: HashSet<PatternBindingId>,
    parameter_storage: HashMap<CaptureSource, Value>,
    pattern_bindings: Vec<HashMap<PatternBindingId, PatternBindingValue>>,
    generic_subst: HashMap<String, Type>,
    generic_tc_subst: HashMap<String, type_checker::Type>,
    generic_const_subst: HashMap<String, usize>,
    mono_functions: HashMap<(hir::item_tree::FunctionId, String), String>,
    mono_methods: HashMap<(hir::item_tree::FunctionId, String), String>,
    loop_targets: Vec<LoopTargets>,
    lambda_functions: HashMap<(BodyId, ExprId), String>,
    function_adapters: HashMap<(hir::item_tree::FunctionId, Vec<type_checker::Type>), String>,
    capture_access: HashMap<CapturePlace, CaptureAccess>,
    current_lambda: Option<ExprId>,
    lambda_counter: u32,
    active_consts: HashSet<hir::item_tree::ConstId>,
}

#[derive(Clone)]
struct CaptureAccess {
    place: Value,
    ty: Type,
}

#[derive(Clone)]
struct DropSlot {
    place: Value,
    flag: Value,
    ty: type_checker::Type,
    projection: Vec<DropProjection>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum DropProjection {
    Field(usize),
    Index(usize),
}

/// Where a `let` binding's data lives: a stable slot it can take the address
/// of, or a plain SSA value.
#[derive(Clone, Copy)]
enum LetSource {
    Place(Value),
    Value(Value),
}

#[derive(Clone)]
struct PatternBindingValue {
    value: Value,
    ty: Type,
    tc_ty: type_checker::Type,
    place: Option<Value>,
    projection: Vec<DropProjection>,
}

impl PatternBindingValue {
    fn direct(
        value: Value,
        ty: Type,
        tc_ty: type_checker::Type,
        place: Option<Value>,
        projection: Vec<DropProjection>,
    ) -> Self {
        Self {
            value,
            ty,
            tc_ty,
            place,
            projection,
        }
    }
}

#[derive(Clone, Copy)]
struct LoopTargets {
    break_block: BlockId,
    continue_block: BlockId,
    drop_depth: usize,
}

#[derive(Default)]
struct MirSubst {
    types: HashMap<String, Type>,
    tc_types: HashMap<String, type_checker::Type>,
    consts: HashMap<String, usize>,
}

enum TypePattern {
    Other,
    EnumVariant {
        enum_id: hir::item_tree::EnumId,
        variant_index: usize,
        args: Vec<type_checker::Type>,
    },
}

impl<'a> LowerCtx<'a> {
    fn lower_function(
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
        func.is_c_export = self.hir.item_tree.extern_function_ids.contains(&fid);
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
                    Some(builder.heap_alloc(self.convert_hir_type(&param.ty)))
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
    fn lower_lambda(
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

        let env_value = if info.captures.is_empty() {
            self.null_env(builder)
        } else {
            let env_ty = Type::Struct(env_struct.clone());
            let env_ptr = if self.analysis.lambda_escapes(body_id, expr_id) {
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
            );
        }

        let call = builder.function_ref(FuncRef::Local(name.clone()), Type::FnPtr(call_signature));
        let drop = builder.function_ref(
            FuncRef::Local(format!("{}_drop", name)),
            closure_drop_function_type(),
        );
        builder.struct_value(vec![call, env_value, drop], ty.clone())
    }

    fn capture_environment_name(&self, capture: &LambdaCapture) -> String {
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
    fn lower_lambda_function(
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
                    Some(lambda_builder.heap_alloc(ty.clone()))
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
        self.storage_bindings = old_storage_bindings;
        self.parameter_storage = old_parameter_storage;
        self.pattern_bindings = old_pattern_bindings;
        self.loop_targets = old_loop_targets;
        self.capture_access = old_capture_access;
        self.current_lambda = old_current_lambda;
        self.current_body = old_current_body;
    }

    fn lower_lambda_drop_function(
        &mut self,
        name: &str,
        info: &LambdaInfo,
        capture_types: &[Type],
        env_struct: &StructType,
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
            if builder.needs_return() {
                builder.set_return(None);
            }
        }
        self.module.add_function(function);
    }

    fn capture_value(
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

    fn capture_place(
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
            CaptureSource::Param(index) => self
                .parameter_storage
                .get(&CaptureSource::Param(*index))
                .copied(),
            source @ CaptureSource::LambdaParam { lambda, .. }
                if self.current_lambda == Some(*lambda) =>
            {
                self.parameter_storage.get(source).copied()
            }
            CaptureSource::LambdaParam { .. } => None,
        }
        .unwrap_or_else(|| {
            let value = self.capture_root_value(builder, params, &capture.source, &root_ty);
            let place = builder.heap_alloc(root_ty.clone());
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

    fn capture_root_value(
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
                .parameter_storage
                .get(source)
                .copied()
                .map(|place| builder.load(place, ty.clone()))
                .or_else(|| params.get(*index).copied())
                .unwrap_or_else(|| builder.unit_const()),
            CaptureSource::LambdaParam { lambda, index }
                if self.current_lambda == Some(*lambda) =>
            {
                self.parameter_storage
                    .get(source)
                    .copied()
                    .map(|place| builder.load(place, ty.clone()))
                    .or_else(|| params.get(*index).copied())
                    .unwrap_or_else(|| builder.unit_const())
            }
            CaptureSource::LambdaParam { .. } => builder.unit_const(),
        }
    }

    fn capture_access_for_place(
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

    fn project_capture_access(
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

    fn capture_root_mir_type(
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

    fn capture_root_type(&self, source: &CaptureSource) -> Option<type_checker::Type> {
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

    fn null_env(&self, builder: &mut Builder) -> Value {
        let zero = builder.iconst(0, IntTy::Usize);
        builder.cast(CastOp::IntToPtr, zero, closure_env_type())
    }

    fn lower_function_value(
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

    fn ensure_noop_closure_drop(&mut self) -> String {
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

    // 表达式降级

    fn lower_expr(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        expr_id: ExprId,
    ) -> Value {
        // 命中缓存直接返回
        if let Some(&v) = self.expr_cache.get(&expr_id) {
            return v;
        }

        let expr = &body.exprs[expr_id];

        // 从类型检查结果中查表达式类型
        let tc_type = self
            .current_body
            .and_then(|bid| self.type_result.expr_types.get(&(bid, expr_id)));
        let mir_type = tc_type.map(|t| self.convert_type(t)).unwrap_or(Type::Unit);
        let diverges = matches!(mir_type, Type::Never);

        let value = match expr {
            Expr::Missing => builder.unit_const(),

            Expr::IntLiteral { value, suffix } => {
                let ty = parse_int_suffix(suffix.as_deref());
                builder.iconst(*value, ty)
            }

            Expr::FloatLiteral { value, suffix } => {
                // HIR 中 value 已经是 f64，直接使用
                let ty = parse_float_suffix(suffix.as_deref());
                builder.fconst(*value, ty)
            }

            Expr::StringLiteral { value } => builder.sconst(value.clone()),

            Expr::CharLiteral { value } => builder.char_const(value.chars().next().unwrap_or('\0')),

            Expr::BoolLiteral { value } => builder.bconst(*value),

            Expr::Path { path, resolved } => match resolved {
                Some(ResolvedName::Param(idx)) => {
                    let capture = CapturePlace::root(CaptureSource::Param(*idx));
                    let value =
                        if let Some(access) = self.capture_access_for_place(builder, &capture) {
                            builder.load(access.place, access.ty)
                        } else if let Some(storage) = self
                            .parameter_storage
                            .get(&CaptureSource::Param(*idx))
                            .copied()
                        {
                            builder.load(storage, mir_type.clone())
                        } else {
                            param_values
                                .get(*idx)
                                .copied()
                                .unwrap_or_else(|| builder.unit_const())
                        };
                    self.clear_drop_flags_if_moved(builder, body, expr_id);
                    value
                }
                Some(ResolvedName::LambdaParam { lambda, index }) => {
                    let source = CaptureSource::LambdaParam {
                        lambda: *lambda,
                        index: *index,
                    };
                    let value = if self.current_lambda == Some(*lambda) {
                        self.parameter_storage
                            .get(&source)
                            .copied()
                            .map(|place| builder.load(place, mir_type.clone()))
                            .or_else(|| param_values.get(*index).copied())
                            .unwrap_or_else(|| builder.unit_const())
                    } else if let Some(access) =
                        self.capture_access_for_place(builder, &CapturePlace::root(source.clone()))
                    {
                        builder.load(access.place, access.ty)
                    } else {
                        builder.unit_const()
                    };
                    self.clear_drop_flags_if_moved(builder, body, expr_id);
                    value
                }
                Some(ResolvedName::PatternBinding(id)) => {
                    let source = CaptureSource::Pattern(*id);
                    let value = if let Some(access) =
                        self.capture_access_for_place(builder, &CapturePlace::root(source))
                    {
                        builder.load(access.place, access.ty)
                    } else {
                        self.binding_value(builder, *id, &mir_type)
                            .unwrap_or_else(|| builder.unit_const())
                    };
                    self.clear_drop_flags_if_moved(builder, body, expr_id);
                    value
                }
                Some(ResolvedName::Function(fid)) => {
                    let args = match tc_type {
                        Some(type_checker::Type::FunctionItem { args, .. }) => args.clone(),
                        _ => Vec::new(),
                    };
                    self.lower_function_value(builder, *fid, &args, &mir_type)
                }
                Some(ResolvedName::Const(const_id)) => self.lower_const_value(builder, *const_id),
                Some(ResolvedName::EnumVariant(enum_id, idx)) => {
                    self.lower_enum_variant_value(builder, *enum_id, *idx, Vec::new(), mir_type)
                }
                _ => path
                    .as_single_name()
                    .and_then(|name| {
                        self.generic_const_subst
                            .get(&name.0)
                            .map(|value| builder.iconst(*value as u64, IntTy::Usize))
                    })
                    .unwrap_or_else(|| builder.unit_const()),
            },

            Expr::Binary { lhs, rhs, op } => {
                if let Some(call) = self
                    .current_body
                    .and_then(|bid| self.type_result.operator_calls.get(&(bid, expr_id)))
                    .cloned()
                {
                    let function = match call {
                        OperatorCall::Function(fid) => Some(fid),
                        OperatorCall::Trait(call) => {
                            let lhs_ty = self
                                .current_body
                                .and_then(|bid| self.type_result.expr_types.get(&(bid, *lhs)));
                            let rhs_ty = self
                                .current_body
                                .and_then(|bid| self.type_result.expr_types.get(&(bid, *rhs)));
                            lhs_ty.and_then(|lhs_ty| {
                                self.find_trait_impl_method(
                                    call.trait_id,
                                    &call.method,
                                    lhs_ty,
                                    rhs_ty,
                                )
                            })
                        }
                    };
                    let Some(function) = function else {
                        return builder.unit_const();
                    };
                    let receiver_ty = self.hir.item_tree.functions[function]
                        .params
                        .first()
                        .map(|param| param.ty.clone());
                    let rhs_ty = self.hir.item_tree.functions[function]
                        .params
                        .get(1)
                        .map(|param| param.ty.clone());
                    let lv = if let Some(receiver_ty) = receiver_ty {
                        self.lower_receiver_arg(builder, param_values, body, *lhs, &receiver_ty)
                    } else {
                        self.lower_expr(builder, param_values, body, *lhs)
                    };
                    let rv = if let Some(rhs_ty) = rhs_ty {
                        self.lower_receiver_arg(builder, param_values, body, *rhs, &rhs_ty)
                    } else {
                        self.lower_expr(builder, param_values, body, *rhs)
                    };
                    return self.lower_operator_call(
                        builder,
                        *lhs,
                        Some(*rhs),
                        function,
                        vec![lv, rv],
                        mir_type,
                    );
                }

                if op.is_assignment() {
                    let rv = self.lower_expr(builder, param_values, body, *rhs);
                    let lv = self.lower_lvalue(builder, param_values, body, *lhs);
                    return match op {
                        HirBinOp::Assign => {
                            let lhs_ty = self
                                .current_body
                                .and_then(|body_id| {
                                    self.type_result.expr_types.get(&(body_id, *lhs))
                                })
                                .cloned();
                            let assignment_slots = self
                                .drop_place_from_expr(body, *lhs)
                                .and_then(|(source, projection)| {
                                    self.drop_slots.get(&source).map(|slots| {
                                        slots
                                            .iter()
                                            .filter(|slot| {
                                                projection.is_empty()
                                                    || slot
                                                        .projection
                                                        .starts_with(projection.as_slice())
                                            })
                                            .cloned()
                                            .collect::<Vec<_>>()
                                    })
                                })
                                .unwrap_or_default();
                            // Raw-pointer indexing addresses uninitialized storage in
                            // containers such as `Vector<T>`; there is no old value to drop.
                            let raw_pointer_index = matches!(
                                &body.exprs[*lhs],
                                Expr::IndexAccess { base, .. }
                                    if self
                                        .current_body
                                        .and_then(|body_id| {
                                            self.type_result.expr_types.get(&(body_id, *base))
                                        })
                                        .is_some_and(|ty| {
                                            matches!(ty, type_checker::Type::Ptr { .. })
                                        })
                            );
                            if !raw_pointer_index
                                && let Some(ty) = lhs_ty.as_ref()
                                && self.type_needs_drop(ty, 0)
                            {
                                if assignment_slots.is_empty() {
                                    self.emit_drop_glue(builder, lv, ty);
                                } else {
                                    for slot in &assignment_slots {
                                        self.emit_drop_slot(builder, slot);
                                    }
                                }
                            }
                            builder.store(rv, lv);
                            for slot in &assignment_slots {
                                let active = builder.bconst(true);
                                builder.store(active, slot.flag);
                            }
                            builder.unit_const()
                        }
                        _ => {
                            let base_op = op.compound_base().unwrap();
                            let value_ty = self
                                .current_body
                                .and_then(|bid| self.type_result.expr_types.get(&(bid, *lhs)))
                                .map(|t| self.convert_type(t))
                                .unwrap_or(mir_type);
                            let current = builder.load(lv, value_ty.clone());
                            let updated =
                                builder.binop(convert_binop(&base_op), current, rv, value_ty);
                            builder.store(updated, lv);
                            builder.unit_const()
                        }
                    };
                }

                let lv = self.lower_expr(builder, param_values, body, *lhs);
                let rv = self.lower_expr(builder, param_values, body, *rhs);

                match op {
                    HirBinOp::Eq
                    | HirBinOp::Neq
                    | HirBinOp::Lt
                    | HirBinOp::Gt
                    | HirBinOp::LtEq
                    | HirBinOp::GtEq => {
                        let lhs_ty = self
                            .current_body
                            .and_then(|bid| self.type_result.expr_types.get(&(bid, *lhs)))
                            .cloned()
                            .unwrap_or(type_checker::Type::Error);
                        let rhs_ty = self
                            .current_body
                            .and_then(|bid| self.type_result.expr_types.get(&(bid, *rhs)))
                            .cloned()
                            .unwrap_or(type_checker::Type::Error);
                        self.lower_comparison(builder, op, lv, rv, &lhs_ty, &rhs_ty)
                    }
                    _ => {
                        let binop = convert_binop(op);
                        builder.binop(binop, lv, rv, mir_type)
                    }
                }
            }

            Expr::Unary { operand, op } => {
                if let Some(call) = self
                    .current_body
                    .and_then(|bid| self.type_result.operator_calls.get(&(bid, expr_id)))
                    .cloned()
                {
                    let function = match call {
                        OperatorCall::Function(fid) => Some(fid),
                        OperatorCall::Trait(call) => self
                            .current_body
                            .and_then(|bid| self.type_result.expr_types.get(&(bid, *operand)))
                            .and_then(|operand_ty| {
                                self.find_trait_impl_method(
                                    call.trait_id,
                                    &call.method,
                                    operand_ty,
                                    None,
                                )
                            }),
                    };
                    let Some(function) = function else {
                        return builder.unit_const();
                    };
                    let receiver_ty = self.hir.item_tree.functions[function]
                        .params
                        .first()
                        .map(|param| param.ty.clone());
                    let value = if let Some(receiver_ty) = receiver_ty {
                        self.lower_receiver_arg(builder, param_values, body, *operand, &receiver_ty)
                    } else {
                        self.lower_expr(builder, param_values, body, *operand)
                    };
                    return self.lower_operator_call(
                        builder,
                        *operand,
                        None,
                        function,
                        vec![value],
                        mir_type,
                    );
                }
                if matches!(op, HirUnOp::Neg)
                    && let Expr::IntLiteral { value, .. } = &body.exprs[*operand]
                    && let Type::Int(ty) = mir_type
                {
                    return builder.negative_iconst(*value, ty);
                }
                let ov = if matches!(op, HirUnOp::Ref | HirUnOp::MutRef) {
                    self.lower_lvalue(builder, param_values, body, *operand)
                } else {
                    self.lower_expr(builder, param_values, body, *operand)
                };
                // +x is a no-op, return operand directly
                if matches!(op, HirUnOp::Pos) {
                    return ov;
                }
                let unop = convert_unop(op);
                builder.unop(unop, ov, mir_type)
            }

            Expr::Block { stmts, tail } => {
                self.drop_scopes.push(Vec::new());
                // 块：顺序执行语句，尾表达式返回值
                for &stmt in stmts {
                    self.lower_stmt(builder, param_values, body, stmt);
                    if !builder.needs_return() {
                        break;
                    }
                }
                let result = if !builder.needs_return() {
                    builder.unit_const()
                } else {
                    match tail {
                        Some(tail_expr) => self.lower_expr(builder, param_values, body, *tail_expr),
                        None => builder.unit_const(),
                    }
                };
                if builder.needs_return() {
                    self.emit_current_drop_scope(builder);
                }
                self.drop_scopes.pop();
                result
            }

            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let cv = self.lower_expr(builder, param_values, body, *cond);
                let then_block = builder.func.new_block_labeled("then");
                let else_block = builder.func.new_block_labeled("else");
                let merge_block = builder.func.new_block_labeled("merge");

                builder.set_cond_branch(cv, then_block, else_block);

                // then 分支
                builder.switch_to_block(then_block);
                let tv = self.lower_expr(builder, param_values, body, *then_branch);
                let then_exit = builder.current_block;
                let mut phi_args = Vec::new();
                if builder.needs_return() {
                    builder.set_branch(merge_block);
                    phi_args.push((tv, then_exit));
                }

                // else 分支
                builder.switch_to_block(else_block);
                let ev = match else_branch {
                    Some(eb) => self.lower_expr(builder, param_values, body, *eb),
                    None => builder.unit_const(),
                };
                let else_exit = builder.current_block;
                if builder.needs_return() {
                    builder.set_branch(merge_block);
                    phi_args.push((ev, else_exit));
                }

                // merge 块：用 phi 节点合并两条路径的值
                builder.switch_to_block(merge_block);
                match phi_args.len() {
                    0 => builder.unit_const(),
                    _ => {
                        let phi = Inst::new(InstKind::Phi(phi_args), mir_type.clone());
                        builder.func.push_inst(merge_block, phi)
                    }
                }
            }

            Expr::While {
                condition,
                body: while_body,
            } => {
                let cond_block = builder.func.new_block_labeled("while_cond");
                let body_block = builder.func.new_block_labeled("while_body");
                let exit_block = builder.func.new_block_labeled("while_exit");

                // 跳转到条件块
                builder.set_branch(cond_block);

                // 条件块：计算条件，条件分支
                builder.switch_to_block(cond_block);
                let cv = self.lower_expr(builder, param_values, body, *condition);
                builder.set_cond_branch(cv, body_block, exit_block);

                // 循环体：执行后跳回条件块
                builder.switch_to_block(body_block);
                self.loop_targets.push(LoopTargets {
                    break_block: exit_block,
                    continue_block: cond_block,
                    drop_depth: self.drop_scopes.len(),
                });
                self.lower_expr(builder, param_values, body, *while_body);
                self.loop_targets.pop();
                if builder.needs_return() {
                    builder.set_branch(cond_block);
                }

                // 出口块
                builder.switch_to_block(exit_block);
                builder.unit_const()
            }

            Expr::For {
                pat,
                iterable,
                body: for_body,
            } => self.lower_for_expr(
                builder,
                param_values,
                body,
                expr_id,
                *pat,
                *iterable,
                *for_body,
            ),

            Expr::Match { scrutinee, arms } => {
                self.lower_match_expr(builder, param_values, body, *scrutinee, arms, mir_type)
            }

            Expr::Array { elements } => {
                let vals: Vec<Value> = elements
                    .iter()
                    .map(|e| self.lower_expr(builder, param_values, body, *e))
                    .collect();
                builder.array_value(vals, mir_type)
            }

            Expr::Tuple { elements } => {
                let values = elements
                    .iter()
                    .map(|element| self.lower_expr(builder, param_values, body, *element))
                    .collect();
                builder.tuple_value(values, mir_type)
            }

            Expr::ArrayRepeat { value, .. } => {
                let len = match tc_type {
                    Some(type_checker::Type::Array(_, len)) => len.as_usize().unwrap_or(0),
                    _ => 0,
                };
                let val = self.lower_expr(builder, param_values, body, *value);
                builder.array_value(vec![val; len], mir_type)
            }

            Expr::Struct {
                fields, resolved, ..
            } => {
                if let Some(ResolvedName::EnumVariant(enum_id, variant_index)) = resolved {
                    let values = match &self.hir.item_tree.enums[*enum_id].variants[*variant_index]
                        .kind
                    {
                        hir::item_tree::HirVariantKind::Struct(expected_fields) => expected_fields
                            .iter()
                            .filter_map(|expected| {
                                fields.iter().find(|field| field.name == expected.name).map(
                                    |field| {
                                        self.lower_expr(builder, param_values, body, field.value)
                                    },
                                )
                            })
                            .collect(),
                        _ => Vec::new(),
                    };
                    self.lower_enum_variant_value(
                        builder,
                        *enum_id,
                        *variant_index,
                        values,
                        mir_type,
                    )
                } else {
                    let vals: Vec<Value> = fields
                        .iter()
                        .map(|f| self.lower_expr(builder, param_values, body, f.value))
                        .collect();
                    builder.struct_value(vals, mir_type)
                }
            }

            Expr::Call { callee, args, .. } => {
                if let Expr::Path {
                    resolved: Some(ResolvedName::EnumVariant(enum_id, variant_index)),
                    ..
                } = &body.exprs[*callee]
                {
                    let arg_vals = args
                        .iter()
                        .map(|arg| self.lower_expr(builder, param_values, body, *arg))
                        .collect();
                    self.lower_enum_variant_value(
                        builder,
                        *enum_id,
                        *variant_index,
                        arg_vals,
                        mir_type,
                    )
                } else if let Some(value) = self.lower_builtin_call(builder, *callee) {
                    value
                } else if self.callee_function_id(*callee).is_none() {
                    let callee_value = self.lower_expr(builder, param_values, body, *callee);
                    let mut arg_values = args
                        .iter()
                        .map(|arg| self.lower_expr(builder, param_values, body, *arg))
                        .collect::<Vec<_>>();
                    let callee_ty = self
                        .current_body
                        .and_then(|body_id| self.type_result.expr_types.get(&(body_id, *callee)))
                        .map(|ty| self.convert_type(ty))
                        .unwrap_or(Type::Unit);
                    if let Some(signature) = closure_call_signature(&callee_ty) {
                        let call =
                            builder.extract_value(callee_value, 0, Type::FnPtr(signature.clone()));
                        let env = builder.extract_value(callee_value, 1, closure_env_type());
                        arg_values.insert(0, env);
                        builder.call_indirect(call, arg_values, mir_type)
                    } else {
                        builder.call_indirect(callee_value, arg_values, mir_type)
                    }
                } else {
                    let target_fid = self.callee_function_id(*callee);
                    let method_target = match (target_fid, &body.exprs[*callee]) {
                        (Some(fid), Expr::FieldAccess { base, .. }) => {
                            Some((self.actual_method_fid(*callee, fid, *base), *base))
                        }
                        _ => None,
                    };
                    if let Some((fid, base)) = method_target
                        && let Some(op) = self.builtin_operator_for_method(fid)
                    {
                        return self.lower_builtin_operator_method_call(
                            builder,
                            param_values,
                            body,
                            base,
                            args,
                            op,
                        );
                    }

                    let name = if let Some((fid, base)) = method_target {
                        self.mono_function_name(fid, *callee)
                            .or_else(|| self.mono_method_name(fid, base, args.first().copied()))
                            .unwrap_or_else(|| self.function_name(fid))
                    } else {
                        target_fid
                            .map(|fid| {
                                self.mono_function_name(fid, *callee)
                                    .unwrap_or_else(|| self.function_name(fid))
                            })
                            .unwrap_or_else(|| callee_name(body, *callee))
                    };
                    let mut arg_vals: Vec<Value> = Vec::new();
                    if let Some((receiver_fid, base)) = method_target
                        && let Some(receiver) =
                            self.hir.item_tree.functions[receiver_fid].params.first()
                    {
                        arg_vals.push(self.lower_receiver_arg(
                            builder,
                            param_values,
                            body,
                            base,
                            &receiver.ty,
                        ));
                    }
                    arg_vals.extend(
                        args.iter()
                            .map(|a| self.lower_expr(builder, param_values, body, *a)),
                    );
                    // 检查是否是 extern 函数调用
                    let is_extern = target_fid
                        .map(|fid| {
                            self.hir.item_tree.extern_function_ids.contains(&fid)
                                && !self.hir.function_bodies.contains_key(&fid)
                        })
                        .unwrap_or(false);
                    let func_ref = if is_extern {
                        FuncRef::Extern(name)
                    } else {
                        FuncRef::Local(name)
                    };
                    builder.call(func_ref, arg_vals, mir_type)
                }
            }

            Expr::Lambda {
                params,
                body: lambda_body,
                ..
            } => {
                let body_id = self.current_body.expect("lambda outside of a body");
                self.lower_lambda(
                    builder,
                    param_values,
                    body_id,
                    expr_id,
                    params,
                    *lambda_body,
                    &mir_type,
                )
            }

            Expr::FieldAccess { base, field } => {
                let captured = self
                    .capture_place_from_expr(body, expr_id)
                    .and_then(|place| self.capture_access_for_place(builder, &place))
                    .filter(|access| access.ty == mir_type);
                let value = if let Some(access) = captured {
                    builder.load(access.place, access.ty)
                } else {
                    let bv = self.lower_expr(builder, param_values, body, *base);
                    let field_idx = self.resolve_field_index(*base, field);
                    builder.extract_value(bv, field_idx, mir_type)
                };
                self.clear_drop_flags_if_moved(builder, body, expr_id);
                value
            }

            Expr::IndexAccess { base, index } => {
                let captured = self
                    .capture_place_from_expr(body, expr_id)
                    .and_then(|place| self.capture_access_for_place(builder, &place))
                    .filter(|access| access.ty == mir_type);
                if let Some(access) = captured {
                    let value = builder.load(access.place, access.ty);
                    self.clear_drop_flags_if_moved(builder, body, expr_id);
                    value
                } else {
                    let base_val = self.lower_expr(builder, param_values, body, *base);
                    let index_val = self.lower_expr(builder, param_values, body, *index);
                    let ptr = if let Some(len) = self.index_len(builder, base_val, *base) {
                        builder.checked_index_ptr(base_val, index_val, len, mir_type.clone())
                    } else {
                        builder.index_ptr(base_val, index_val, mir_type.clone())
                    };
                    let value = builder.load(ptr, mir_type);
                    self.clear_drop_flags_if_moved(builder, body, expr_id);
                    self.clear_dynamic_index_drop_flags_if_moved(
                        builder, body, expr_id, *base, *index, index_val,
                    );
                    value
                }
            }

            Expr::Unsafe { body: body_expr } => {
                self.lower_expr(builder, param_values, body, *body_expr)
            }

            Expr::Cast { base, target: _ } => {
                let base_val = self.lower_expr(builder, param_values, body, *base);
                let base_tc_ty = self
                    .current_body
                    .and_then(|bid| self.type_result.expr_types.get(&(bid, *base)))
                    .cloned();
                let base_mir_ty = base_tc_ty
                    .as_ref()
                    .map(|t| self.convert_type(t))
                    .unwrap_or(Type::Unit);

                if is_raw_parts_to_slice_cast(&base_mir_ty, &mir_type) {
                    let Type::Tuple(parts) = &base_mir_ty else {
                        unreachable!();
                    };
                    let data = builder.extract_value(base_val, 0, parts[0].clone());
                    let len = builder.extract_value(base_val, 1, Type::Int(IntTy::Usize));
                    builder.struct_value(vec![data, len], mir_type)
                } else if is_slice_to_raw_parts_cast(&base_mir_ty, &mir_type) {
                    let Type::Tuple(parts) = &mir_type else {
                        unreachable!();
                    };
                    let data = builder.extract_value(base_val, 0, parts[0].clone());
                    let len = builder.extract_value(base_val, 1, Type::Int(IntTy::Usize));
                    builder.struct_value(vec![data, len], mir_type)
                } else if is_byte_str_layout_cast(&base_mir_ty, &mir_type) {
                    let data = builder.extract_value(
                        base_val,
                        0,
                        Type::Ptr(Box::new(Type::Int(IntTy::U8))),
                    );
                    let len = builder.extract_value(base_val, 1, Type::Int(IntTy::Usize));
                    builder.struct_value(vec![data, len], mir_type)
                } else {
                    // auto-unwrap Ref
                    let (base_val, base_mir_ty) = if let Type::Ref(inner, _) = &base_mir_ty {
                        (builder.load(base_val, *inner.clone()), *inner.clone())
                    } else {
                        (base_val, base_mir_ty)
                    };

                    let cast_op = determine_cast_op(&base_mir_ty, &mir_type);
                    builder.cast(cast_op, base_val, mir_type)
                }
            }
        };

        let value = self.apply_expr_coercion(builder, expr_id, value);
        if diverges && builder.needs_return() {
            builder.set_unreachable();
        }
        self.expr_cache.insert(expr_id, value);
        value
    }

    fn apply_expr_coercion(&self, builder: &mut Builder, expr_id: ExprId, value: Value) -> Value {
        let Some(body_id) = self.current_body else {
            return value;
        };
        let Some(target) = self.type_result.expr_coercions.get(&(body_id, expr_id)) else {
            return value;
        };
        let Some(actual) = self.type_result.expr_types.get(&(body_id, expr_id)) else {
            return value;
        };
        let (type_checker::Type::Ref(actual, _), type_checker::Type::Ref(target, target_mut)) =
            (actual, target)
        else {
            return value;
        };
        let (
            type_checker::Type::Array(_, type_checker::ConstArg::Value(len)),
            type_checker::Type::Slice(_),
        ) = (actual.as_ref(), target.as_ref())
        else {
            return value;
        };
        let len = builder.iconst(*len as u64, IntTy::Usize);
        builder.struct_value(
            vec![value, len],
            Type::Ref(Box::new(self.convert_type(target.as_ref())), *target_mut),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_for_expr(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        for_expr: ExprId,
        pat: PatId,
        iterable: ExprId,
        for_body: ExprId,
    ) -> Value {
        if let Some(info) = self
            .current_body
            .and_then(|bid| self.type_result.for_loops.get(&(bid, for_expr)))
            .cloned()
        {
            return self.lower_iterator_for_expr(
                builder,
                param_values,
                body,
                pat,
                iterable,
                for_body,
                &info,
            );
        }

        if let Some((item_ty, len)) = self.array_iter_info(iterable) {
            return self.lower_array_for_expr(
                builder,
                param_values,
                body,
                pat,
                iterable,
                for_body,
                item_ty,
                len,
            );
        }

        let iterable_value = self.lower_expr(builder, param_values, body, iterable);
        if !self.is_std_range_expr(iterable) {
            return builder.unit_const();
        }

        let i32_ty = Type::Int(IntTy::I32);
        let start = builder.extract_value(iterable_value, 0, i32_ty.clone());
        let end = builder.extract_value(iterable_value, 1, i32_ty.clone());
        let cursor = builder.alloca(i32_ty.clone());
        builder.store(start, cursor);

        let cond_block = builder.func.new_block_labeled("for_cond");
        let body_block = builder.func.new_block_labeled("for_body");
        let step_block = builder.func.new_block_labeled("for_step");
        let exit_block = builder.func.new_block_labeled("for_exit");

        builder.set_branch(cond_block);

        builder.switch_to_block(cond_block);
        let current = builder.load(cursor, i32_ty.clone());
        let keep_going = builder.cmp(CmpOp::Lt, current, end);
        builder.set_cond_branch(keep_going, body_block, exit_block);

        builder.switch_to_block(body_block);
        self.push_pattern_binding(body, pat, current, i32_ty.clone());
        self.loop_targets.push(LoopTargets {
            break_block: exit_block,
            continue_block: step_block,
            drop_depth: self.drop_scopes.len(),
        });
        self.lower_expr(builder, param_values, body, for_body);
        self.loop_targets.pop();
        self.pattern_bindings.pop();
        if builder.needs_return() {
            builder.set_branch(step_block);
        }

        builder.switch_to_block(step_block);
        let one = builder.iconst(1, IntTy::I32);
        let next = builder.binop(BinOp::Add, current, one, i32_ty);
        builder.store(next, cursor);
        builder.set_branch(cond_block);

        builder.switch_to_block(exit_block);
        builder.unit_const()
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_array_for_expr(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        pat: PatId,
        iterable: ExprId,
        for_body: ExprId,
        item_ty: Type,
        len: usize,
    ) -> Value {
        let body_id = self.current_body.expect("for loop outside a function body");
        let item_tc_ty = match self
            .type_result
            .expr_types
            .get(&(body_id, iterable))
            .map(|ty| self.substitute_tc_type(ty))
        {
            Some(type_checker::Type::Array(item, _)) => *item,
            _ => type_checker::Type::Unknown,
        };
        let array_tc_ty = type_checker::Type::Array(
            Box::new(item_tc_ty.clone()),
            type_checker::ConstArg::Value(len),
        );
        let array_ty = Type::Array(Box::new(item_ty.clone()), len);
        let iterable_value = self.lower_expr(builder, param_values, body, iterable);
        let iterable_place = builder.alloca(array_ty);
        builder.store(iterable_value, iterable_place);

        let owner_slots = self.create_drop_slots(builder, iterable_place, &array_tc_ty, Vec::new());
        self.drop_scopes
            .push(owner_slots.iter().cloned().rev().collect());

        let index_ty = Type::Int(IntTy::I32);
        let zero = builder.iconst(0, IntTy::I32);
        let end = builder.iconst(len as u64, IntTy::I32);
        let cursor = builder.alloca(index_ty.clone());
        builder.store(zero, cursor);

        let cond_block = builder.func.new_block_labeled("for_array_cond");
        let body_block = builder.func.new_block_labeled("for_array_body");
        let step_block = builder.func.new_block_labeled("for_array_step");
        let exit_block = builder.func.new_block_labeled("for_array_exit");

        builder.set_branch(cond_block);

        builder.switch_to_block(cond_block);
        let current = builder.load(cursor, index_ty.clone());
        let keep_going = builder.cmp(CmpOp::Lt, current, end);
        builder.set_cond_branch(keep_going, body_block, exit_block);

        builder.switch_to_block(body_block);
        let item_ptr = builder.index_ptr(iterable_place, current, item_ty.clone());
        let item = builder.load(item_ptr, item_ty.clone());
        self.clear_indexed_drop_slots(builder, &owner_slots, current, IntTy::I32);
        let item_place = self.type_needs_drop(&item_tc_ty, 0).then(|| {
            let place = builder.alloca(item_ty.clone());
            builder.store(item, place);
            place
        });
        self.push_match_pattern_bindings(builder, body, pat, item, item_place, &item_tc_ty);
        let pattern_sources =
            self.push_pattern_drop_scope(builder, body, pat, item_place, &item_tc_ty, true);
        let item_drop_depth = self.drop_scopes.len() - 1;
        self.loop_targets.push(LoopTargets {
            break_block: exit_block,
            continue_block: step_block,
            drop_depth: item_drop_depth,
        });
        self.lower_expr(builder, param_values, body, for_body);
        self.loop_targets.pop();
        if builder.needs_return() {
            self.emit_current_drop_scope(builder);
            builder.set_branch(step_block);
        }
        self.pop_pattern_drop_scope(pattern_sources);
        self.pattern_bindings.pop();

        builder.switch_to_block(step_block);
        let one = builder.iconst(1, IntTy::I32);
        let next = builder.binop(BinOp::Add, current, one, index_ty);
        builder.store(next, cursor);
        builder.set_branch(cond_block);

        builder.switch_to_block(exit_block);
        self.emit_current_drop_scope(builder);
        self.drop_scopes.pop();
        builder.unit_const()
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_iterator_for_expr(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        pat: PatId,
        iterable: ExprId,
        for_body: ExprId,
        info: &type_checker::ForLoopInfo,
    ) -> Value {
        let body_id = self.current_body.expect("for loop outside a function body");
        let iterable_ty = self
            .type_result
            .expr_types
            .get(&(body_id, iterable))
            .cloned()
            .map(|ty| self.substitute_tc_type(&ty))
            .expect("missing iterable type for checked for loop");
        let iter_tc_ty = self.substitute_tc_type(&info.iter_ty);
        let next_tc_ty = self.substitute_tc_type(&info.next_ty);

        let iterable_value = self.lower_expr(builder, param_values, body, iterable);
        let iter_ty = self.convert_type(&iter_tc_ty);
        let item_ty = self.convert_type(&info.item_ty);
        let option_ty = self.convert_type(&next_tc_ty);
        let into_iter_fid = self
            .find_trait_impl_method(
                info.into_iter.trait_id,
                &info.into_iter.method,
                &iterable_ty,
                None,
            )
            .expect("missing IntoIterator impl method for checked for loop");
        let next_fid = self
            .find_trait_impl_method(info.next.trait_id, &info.next.method, &iter_tc_ty, None)
            .expect("missing Iterator impl method for checked for loop");
        let into_iter_name = self
            .mono_method_name_for_receiver(into_iter_fid, &iterable_ty, None)
            .unwrap_or_else(|| self.function_name(into_iter_fid));
        let next_name = self
            .mono_method_name_for_receiver(next_fid, &iter_tc_ty, None)
            .unwrap_or_else(|| self.function_name(next_fid));

        let iter_value = builder.call(
            FuncRef::Local(into_iter_name),
            vec![iterable_value],
            iter_ty.clone(),
        );
        // `IntoIterator::into_iter(self)` consumes the owner. Its destructor
        // must not run again after the iterator takes over the allocation.
        if !matches!(iterable_ty, type_checker::Type::Ref(..))
            && let Some((source, _)) = self.drop_place_from_expr(body, iterable)
        {
            self.clear_drop_slots_for_source(builder, &source);
        }
        let iter_slot = builder.alloca(iter_ty.clone());
        builder.store(iter_value, iter_slot);
        let iter_owner_slots = self.create_drop_slots(builder, iter_slot, &iter_tc_ty, Vec::new());
        self.drop_scopes
            .push(iter_owner_slots.iter().cloned().rev().collect());
        // ponytail: array IntoIterator is sequential; add ManuallyDrop-like storage before
        // permitting custom array iterators that yield elements out of order.
        let array_cursor = matches!(iterable_ty, type_checker::Type::Array(..)).then(|| {
            let cursor = builder.alloca(Type::Int(IntTy::Usize));
            let zero = builder.iconst(0, IntTy::Usize);
            builder.store(zero, cursor);
            cursor
        });

        let cond_block = builder.func.new_block_labeled("for_iter_cond");
        let body_block = builder.func.new_block_labeled("for_iter_body");
        let exit_block = builder.func.new_block_labeled("for_iter_exit");

        builder.set_branch(cond_block);

        builder.switch_to_block(cond_block);
        let next_receiver = match self.hir.item_tree.functions[next_fid]
            .params
            .first()
            .map(|param| &param.ty)
        {
            Some(hir::item_tree::HirTypeRef::Ref(_, mutable)) => {
                let op = if *mutable {
                    HirUnOp::MutRef
                } else {
                    HirUnOp::Ref
                };
                builder.unop(
                    convert_unop(&op),
                    iter_slot,
                    Type::Ref(Box::new(iter_ty), *mutable),
                )
            }
            _ => iter_slot,
        };
        let next_value = builder.call(
            FuncRef::Local(next_name),
            vec![next_receiver],
            option_ty.clone(),
        );
        let tag = builder.extract_value(next_value, 0, Type::Int(IntTy::U32));
        let some_tag = builder.iconst(info.some_variant as u64, IntTy::U32);
        let has_item = builder.cmp(CmpOp::Eq, tag, some_tag);
        builder.set_cond_branch(has_item, body_block, exit_block);

        builder.switch_to_block(body_block);
        let option_id = match next_tc_ty {
            type_checker::Type::Enum(enum_id, _) => enum_id,
            _ => unreachable!("checked Iterator::next result is not an enum"),
        };
        let payload_index =
            1 + self.enum_payload_offset(&self.hir.item_tree.enums[option_id], info.some_variant);
        let item = builder.extract_value(next_value, payload_index, item_ty.clone());
        if let Some(cursor) = array_cursor {
            let current = builder.load(cursor, Type::Int(IntTy::Usize));
            self.clear_indexed_drop_slots(builder, &iter_owner_slots, current, IntTy::Usize);
            let one = builder.iconst(1, IntTy::Usize);
            let next = builder.binop(BinOp::Add, current, one, Type::Int(IntTy::Usize));
            builder.store(next, cursor);
        }
        let item_place = self.type_needs_drop(&info.item_ty, 0).then(|| {
            let place = builder.alloca(item_ty.clone());
            builder.store(item, place);
            place
        });
        self.push_match_pattern_bindings(builder, body, pat, item, item_place, &info.item_ty);
        let pattern_sources =
            self.push_pattern_drop_scope(builder, body, pat, item_place, &info.item_ty, true);
        let item_drop_depth = self.drop_scopes.len() - 1;
        self.loop_targets.push(LoopTargets {
            break_block: exit_block,
            continue_block: cond_block,
            drop_depth: item_drop_depth,
        });
        self.lower_expr(builder, param_values, body, for_body);
        self.loop_targets.pop();
        if builder.needs_return() {
            self.emit_current_drop_scope(builder);
            builder.set_branch(cond_block);
        }
        self.pop_pattern_drop_scope(pattern_sources);
        self.pattern_bindings.pop();

        builder.switch_to_block(exit_block);
        self.emit_current_drop_scope(builder);
        self.drop_scopes.pop();
        builder.unit_const()
    }

    fn lower_match_expr(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        scrutinee: ExprId,
        arms: &[MatchArm],
        result_ty: Type,
    ) -> Value {
        let scrutinee_value = self.lower_expr(builder, param_values, body, scrutinee);
        let scrutinee_ty = self
            .current_body
            .and_then(|body_id| self.type_result.expr_types.get(&(body_id, scrutinee)))
            .cloned()
            .unwrap_or(type_checker::Type::Unknown);
        let scrutinee_place = self.type_needs_drop(&scrutinee_ty, 0).then(|| {
            let place = builder.alloca(self.convert_type(&scrutinee_ty));
            builder.store(scrutinee_value, place);
            place
        });
        let scrutinee_source = self.drop_place_from_expr(body, scrutinee);
        let merge_block = builder.func.new_block_labeled("match_merge");
        let mut next_test = builder.current_block;
        let mut phi_args = Vec::new();

        for arm in arms {
            builder.switch_to_block(next_test);
            let arm_block = builder.func.new_block_labeled("match_arm");
            let miss_block = builder.func.new_block_labeled("match_next");
            match self.lower_pattern_condition(
                builder,
                body,
                arm.pat,
                scrutinee_value,
                &scrutinee_ty,
            ) {
                Some(condition) => builder.set_cond_branch(condition, arm_block, miss_block),
                None => builder.set_branch(arm_block),
            }

            builder.switch_to_block(arm_block);
            self.push_match_pattern_bindings(
                builder,
                body,
                arm.pat,
                scrutinee_value,
                scrutinee_place,
                &scrutinee_ty,
            );

            if let Some(guard) = arm.guard {
                let guarded_body = builder.func.new_block_labeled("match_guarded_arm");
                let guard_value = self.lower_expr(builder, param_values, body, guard);
                builder.set_cond_branch(guard_value, guarded_body, miss_block);
                builder.switch_to_block(guarded_body);
            }

            if let Some((source, projection)) = &scrutinee_source {
                self.transfer_pattern_drop_flags(builder, source, projection);
            }

            let pattern_sources = self.push_pattern_drop_scope(
                builder,
                body,
                arm.pat,
                scrutinee_place,
                &scrutinee_ty,
                scrutinee_source.is_none() || matches!(scrutinee_ty, type_checker::Type::Enum(..)),
            );
            let arm_value = self.lower_expr(builder, param_values, body, arm.body);
            if builder.needs_return() {
                self.emit_current_drop_scope(builder);
                let arm_exit = builder.current_block;
                builder.set_branch(merge_block);
                phi_args.push((arm_value, arm_exit));
            }
            self.pop_pattern_drop_scope(pattern_sources);
            self.pattern_bindings.pop();
            next_test = miss_block;
        }

        builder.switch_to_block(next_test);
        builder.set_unreachable();
        builder.switch_to_block(merge_block);
        match phi_args.len() {
            0 => builder.unit_const(),
            _ => {
                let phi = Inst::new(InstKind::Phi(phi_args), result_ty);
                builder.func.push_inst(merge_block, phi)
            }
        }
    }

    fn lower_pattern_condition(
        &mut self,
        builder: &mut Builder,
        body: &Body,
        pat: PatId,
        value: Value,
        value_ty: &type_checker::Type,
    ) -> Option<Value> {
        let (value, _, value_ty) = self.adjust_pattern_value(builder, pat, value, None, value_ty);
        let value_ty = &value_ty;
        let pattern = body.pats[pat].clone();
        match pattern {
            Pattern::Wildcard => None,
            Pattern::Reference { pattern, .. } => {
                let type_checker::Type::Ref(inner, _) = value_ty else {
                    return Some(builder.bconst(false));
                };
                let inner_value = builder.load(value, self.convert_type(inner));
                self.lower_pattern_condition(builder, body, pattern, inner_value, inner)
            }
            Pattern::Binding { name, .. } => {
                let TypePattern::EnumVariant {
                    enum_id,
                    variant_index,
                    args,
                } = self.classify_type_pattern(value_ty, Some(&name.0))
                else {
                    return None;
                };
                Some(self.lower_variant_tag_condition(
                    builder,
                    value,
                    enum_id,
                    variant_index,
                    &args,
                ))
            }
            Pattern::Struct { ref fields, .. }
                if matches!(value_ty, type_checker::Type::Struct(_, _)) =>
            {
                let type_checker::Type::Struct(struct_id, args) = value_ty else {
                    unreachable!();
                };
                let field_types = self.struct_pattern_field_types(*struct_id, args);
                let mut condition = None;
                for field in fields {
                    let Some(child) = field.pat else {
                        continue;
                    };
                    let Some((index, (_, child_ty))) = field_types
                        .iter()
                        .enumerate()
                        .find(|(_, (name, _))| *name == field.name.0)
                    else {
                        continue;
                    };
                    let child_value =
                        builder.extract_value(value, index, self.convert_type(child_ty));
                    let child_condition =
                        self.lower_pattern_condition(builder, body, child, child_value, child_ty);
                    condition = self.and_pattern_conditions(builder, condition, child_condition);
                }
                condition
            }
            Pattern::Path { ref path }
            | Pattern::TupleStruct { ref path, .. }
            | Pattern::Struct { ref path, .. } => {
                let name = path.segments.last().map(|name| name.0.as_str());
                let TypePattern::EnumVariant {
                    enum_id,
                    variant_index,
                    args,
                } = self.classify_type_pattern(value_ty, name)
                else {
                    return Some(builder.bconst(false));
                };
                let mut condition = Some(self.lower_variant_tag_condition(
                    builder,
                    value,
                    enum_id,
                    variant_index,
                    &args,
                ));
                let payloads = self.enum_variant_payload_types(enum_id, &args, variant_index);
                let offset =
                    self.enum_payload_offset(&self.hir.item_tree.enums[enum_id], variant_index);

                match pattern {
                    Pattern::TupleStruct { elements, .. } => {
                        for (index, child) in elements.into_iter().enumerate() {
                            let Some((_, child_ty)) = payloads.get(index) else {
                                break;
                            };
                            let child_value = builder.extract_value(
                                value,
                                1 + offset + index,
                                self.convert_type(child_ty),
                            );
                            let child_condition = self.lower_pattern_condition(
                                builder,
                                body,
                                child,
                                child_value,
                                child_ty,
                            );
                            condition =
                                self.and_pattern_conditions(builder, condition, child_condition);
                        }
                    }
                    Pattern::Struct { fields, .. } => {
                        for field in fields {
                            let Some(child) = field.pat else {
                                continue;
                            };
                            let Some((index, (_, child_ty))) = payloads
                                .iter()
                                .enumerate()
                                .find(|(_, (name, _))| name.as_deref() == Some(&field.name.0))
                            else {
                                continue;
                            };
                            let child_value = builder.extract_value(
                                value,
                                1 + offset + index,
                                self.convert_type(child_ty),
                            );
                            let child_condition = self.lower_pattern_condition(
                                builder,
                                body,
                                child,
                                child_value,
                                child_ty,
                            );
                            condition =
                                self.and_pattern_conditions(builder, condition, child_condition);
                        }
                    }
                    _ => {}
                }
                condition
            }
            Pattern::Literal(literal) => {
                let literal_value = self.lower_literal_pattern(builder, &literal, value_ty);
                Some(builder.cmp(CmpOp::Eq, value, literal_value))
            }
            Pattern::Tuple { elements } => {
                let type_checker::Type::Tuple(element_types) = value_ty else {
                    return Some(builder.bconst(false));
                };
                let mut condition = None;
                for (index, child) in elements.into_iter().enumerate() {
                    let Some(child_ty) = element_types.get(index) else {
                        break;
                    };
                    let child_value =
                        builder.extract_value(value, index, self.convert_type(child_ty));
                    let child_condition =
                        self.lower_pattern_condition(builder, body, child, child_value, child_ty);
                    condition = self.and_pattern_conditions(builder, condition, child_condition);
                }
                condition
            }
        }
    }

    fn lower_variant_tag_condition(
        &self,
        builder: &mut Builder,
        value: Value,
        _enum_id: hir::item_tree::EnumId,
        variant_index: usize,
        _args: &[type_checker::Type],
    ) -> Value {
        let tag = builder.extract_value(value, 0, Type::Int(IntTy::U32));
        let expected = builder.iconst(variant_index as u64, IntTy::U32);
        builder.cmp(CmpOp::Eq, tag, expected)
    }

    fn and_pattern_conditions(
        &self,
        builder: &mut Builder,
        lhs: Option<Value>,
        rhs: Option<Value>,
    ) -> Option<Value> {
        match (lhs, rhs) {
            (Some(lhs), Some(rhs)) => Some(builder.binop(BinOp::BitAnd, lhs, rhs, Type::Bool)),
            (Some(value), None) | (None, Some(value)) => Some(value),
            (None, None) => None,
        }
    }

    fn lower_literal_pattern(
        &self,
        builder: &mut Builder,
        literal: &LiteralPattern,
        expected: &type_checker::Type,
    ) -> Value {
        match literal {
            LiteralPattern::Int { value, suffix, .. } => {
                let ty = match self.convert_type(expected) {
                    Type::Int(ty) => ty,
                    _ => parse_int_suffix(suffix.as_deref()),
                };
                builder.iconst(*value, ty)
            }
            LiteralPattern::Float { value, suffix, .. } => {
                let ty = match self.convert_type(expected) {
                    Type::Float(ty) => ty,
                    _ => parse_float_suffix(suffix.as_deref()),
                };
                builder.fconst(*value, ty)
            }
            LiteralPattern::String(value) => builder.sconst(value.clone()),
            LiteralPattern::Char(value) => builder.char_const(value.chars().next().unwrap_or('\0')),
            LiteralPattern::Bool(value) => builder.bconst(*value),
        }
    }

    fn push_match_pattern_bindings(
        &mut self,
        builder: &mut Builder,
        body: &Body,
        pat: PatId,
        value: Value,
        place: Option<Value>,
        value_ty: &type_checker::Type,
    ) {
        let mut scope = HashMap::new();
        self.collect_match_pattern_bindings(
            builder,
            body,
            pat,
            value,
            place,
            value_ty,
            Vec::new(),
            &mut scope,
        );
        self.pattern_bindings.push(scope);
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_match_pattern_bindings(
        &mut self,
        builder: &mut Builder,
        body: &Body,
        pat: PatId,
        value: Value,
        place: Option<Value>,
        value_ty: &type_checker::Type,
        projection: Vec<DropProjection>,
        scope: &mut HashMap<PatternBindingId, PatternBindingValue>,
    ) {
        let (value, place, value_ty) =
            self.adjust_pattern_value(builder, pat, value, place, value_ty);
        let value_ty = &value_ty;
        match body.pats[pat].clone() {
            Pattern::Binding { name, .. } => {
                if !matches!(
                    self.classify_type_pattern(value_ty, Some(&name.0)),
                    TypePattern::EnumVariant { .. }
                ) {
                    self.insert_match_pattern_binding(
                        builder,
                        PatternBindingId {
                            pattern: pat,
                            field: None,
                        },
                        value,
                        place,
                        value_ty,
                        projection,
                        scope,
                    );
                }
            }
            Pattern::Reference { pattern, .. } => {
                let type_checker::Type::Ref(inner, _) = value_ty else {
                    return;
                };
                let inner_value = builder.load(value, self.convert_type(inner));
                self.collect_match_pattern_bindings(
                    builder,
                    body,
                    pattern,
                    inner_value,
                    None,
                    inner,
                    projection,
                    scope,
                );
            }
            Pattern::Tuple { elements } => {
                let type_checker::Type::Tuple(element_types) = value_ty else {
                    return;
                };
                for (index, child) in elements.into_iter().enumerate() {
                    let Some(child_ty) = element_types.get(index) else {
                        break;
                    };
                    let child_value =
                        builder.extract_value(value, index, self.convert_type(child_ty));
                    let child_place = place
                        .map(|place| builder.field_ptr(place, index, self.convert_type(child_ty)));
                    let mut child_projection = projection.clone();
                    child_projection.push(DropProjection::Field(index));
                    self.collect_match_pattern_bindings(
                        builder,
                        body,
                        child,
                        child_value,
                        child_place,
                        child_ty,
                        child_projection,
                        scope,
                    );
                }
            }
            Pattern::TupleStruct { path, elements } => {
                let name = path.segments.last().map(|name| name.0.as_str());
                let TypePattern::EnumVariant {
                    enum_id,
                    variant_index,
                    args,
                } = self.classify_type_pattern(value_ty, name)
                else {
                    return;
                };
                let payloads = self.enum_variant_payload_types(enum_id, &args, variant_index);
                let offset =
                    self.enum_payload_offset(&self.hir.item_tree.enums[enum_id], variant_index);
                for (index, child) in elements.into_iter().enumerate() {
                    let Some((_, child_ty)) = payloads.get(index) else {
                        break;
                    };
                    let child_value = builder.extract_value(
                        value,
                        1 + offset + index,
                        self.convert_type(child_ty),
                    );
                    let field_index = 1 + offset + index;
                    let child_place = place.map(|place| {
                        builder.field_ptr(place, field_index, self.convert_type(child_ty))
                    });
                    let mut child_projection = projection.clone();
                    child_projection.push(DropProjection::Field(field_index));
                    self.collect_match_pattern_bindings(
                        builder,
                        body,
                        child,
                        child_value,
                        child_place,
                        child_ty,
                        child_projection,
                        scope,
                    );
                }
            }
            Pattern::Struct { path, fields } => {
                if let type_checker::Type::Struct(struct_id, args) = value_ty {
                    let field_types = self.struct_pattern_field_types(*struct_id, args);
                    for (binding_index, field) in fields.into_iter().enumerate() {
                        let Some((index, (_, child_ty))) = field_types
                            .iter()
                            .enumerate()
                            .find(|(_, (name, _))| *name == field.name.0)
                        else {
                            continue;
                        };
                        let child_value =
                            builder.extract_value(value, index, self.convert_type(child_ty));
                        let child_place = place.map(|place| {
                            builder.field_ptr(place, index, self.convert_type(child_ty))
                        });
                        let mut child_projection = projection.clone();
                        child_projection.push(DropProjection::Field(index));
                        if let Some(child) = field.pat {
                            self.collect_match_pattern_bindings(
                                builder,
                                body,
                                child,
                                child_value,
                                child_place,
                                child_ty,
                                child_projection,
                                scope,
                            );
                        } else {
                            self.insert_match_pattern_binding(
                                builder,
                                PatternBindingId {
                                    pattern: pat,
                                    field: Some(binding_index),
                                },
                                child_value,
                                child_place,
                                child_ty,
                                child_projection,
                                scope,
                            );
                        }
                    }
                    return;
                }
                let name = path.segments.last().map(|name| name.0.as_str());
                let TypePattern::EnumVariant {
                    enum_id,
                    variant_index,
                    args,
                } = self.classify_type_pattern(value_ty, name)
                else {
                    return;
                };
                let payloads = self.enum_variant_payload_types(enum_id, &args, variant_index);
                let offset =
                    self.enum_payload_offset(&self.hir.item_tree.enums[enum_id], variant_index);
                for (binding_index, field) in fields.into_iter().enumerate() {
                    let Some((index, (_, child_ty))) = payloads
                        .iter()
                        .enumerate()
                        .find(|(_, (name, _))| name.as_deref() == Some(&field.name.0))
                    else {
                        continue;
                    };
                    let child_value = builder.extract_value(
                        value,
                        1 + offset + index,
                        self.convert_type(child_ty),
                    );
                    let field_index = 1 + offset + index;
                    let child_place = place.map(|place| {
                        builder.field_ptr(place, field_index, self.convert_type(child_ty))
                    });
                    let mut child_projection = projection.clone();
                    child_projection.push(DropProjection::Field(field_index));
                    if let Some(child) = field.pat {
                        self.collect_match_pattern_bindings(
                            builder,
                            body,
                            child,
                            child_value,
                            child_place,
                            child_ty,
                            child_projection,
                            scope,
                        );
                    } else {
                        self.insert_match_pattern_binding(
                            builder,
                            PatternBindingId {
                                pattern: pat,
                                field: Some(binding_index),
                            },
                            child_value,
                            child_place,
                            child_ty,
                            child_projection,
                            scope,
                        );
                    }
                }
            }
            Pattern::Wildcard | Pattern::Literal(_) | Pattern::Path { .. } => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_match_pattern_binding(
        &mut self,
        builder: &mut Builder,
        id: PatternBindingId,
        value: Value,
        place: Option<Value>,
        value_ty: &type_checker::Type,
        projection: Vec<DropProjection>,
        scope: &mut HashMap<PatternBindingId, PatternBindingValue>,
    ) {
        let mode = self.pattern_binding_mode(id);
        let binding_ty = self.pattern_binding_type(id, value_ty);
        let mir_ty = self.convert_type(&binding_ty);
        let (value, place) = match mode {
            PatternBindingMode::Move => (value, place),
            PatternBindingMode::Ref | PatternBindingMode::RefMut => {
                let place = self.materialize_pattern_place(builder, value, place, value_ty);
                let op = if mode == PatternBindingMode::RefMut {
                    UnOp::MutRef
                } else {
                    UnOp::Ref
                };
                (builder.unop(op, place, mir_ty.clone()), None)
            }
        };
        scope.insert(
            id,
            PatternBindingValue::direct(value, mir_ty, binding_ty, place, projection),
        );
    }

    fn adjust_pattern_value(
        &mut self,
        builder: &mut Builder,
        pat: PatId,
        mut value: Value,
        mut place: Option<Value>,
        value_ty: &type_checker::Type,
    ) -> (Value, Option<Value>, type_checker::Type) {
        let target = self.pattern_type(pat).unwrap_or_else(|| value_ty.clone());
        let mut current = value_ty.clone();
        while current != target {
            let type_checker::Type::Ref(inner, _) = current else {
                break;
            };
            let inner = *inner;
            let reference = value;
            value = builder.load(reference, self.convert_type(&inner));
            place = Some(reference);
            current = inner;
        }
        (value, place, current)
    }

    fn materialize_pattern_place(
        &self,
        builder: &mut Builder,
        value: Value,
        place: Option<Value>,
        value_ty: &type_checker::Type,
    ) -> Value {
        place.unwrap_or_else(|| {
            let place = builder.heap_alloc(self.convert_type(value_ty));
            builder.store(value, place);
            place
        })
    }

    fn pattern_type(&self, pat: PatId) -> Option<type_checker::Type> {
        self.current_body
            .and_then(|body_id| self.type_result.pattern_types.get(&(body_id, pat)))
            .cloned()
    }

    fn pattern_binding_mode(&self, id: PatternBindingId) -> PatternBindingMode {
        self.current_body
            .and_then(|body_id| self.type_result.pattern_binding_modes.get(&(body_id, id)))
            .copied()
            .unwrap_or(PatternBindingMode::Move)
    }

    fn pattern_binding_type(
        &self,
        id: PatternBindingId,
        fallback: &type_checker::Type,
    ) -> type_checker::Type {
        self.current_body
            .and_then(|body_id| self.type_result.pattern_binding_types.get(&(body_id, id)))
            .cloned()
            .unwrap_or_else(|| fallback.clone())
    }

    fn push_pattern_drop_scope(
        &mut self,
        builder: &mut Builder,
        body: &Body,
        pat: PatId,
        place: Option<Value>,
        value_ty: &type_checker::Type,
        owns_whole_value: bool,
    ) -> Vec<CaptureSource> {
        let mut slots = place
            .map(|place| self.create_pattern_owner_slots(builder, body, pat, place, value_ty))
            .unwrap_or_default();
        if !owns_whole_value {
            let moved = self.moved_pattern_projections();
            slots.retain(|slot| {
                moved.iter().any(|projection| {
                    slot.projection.starts_with(projection)
                        || projection.starts_with(&slot.projection)
                })
            });
        }
        let mut sources = Vec::new();
        if let Some(bindings) = self.pattern_bindings.last() {
            for (id, binding) in bindings {
                if self.pattern_binding_mode(*id) != PatternBindingMode::Move
                    || !self.type_needs_drop(&binding.tc_ty, 0)
                {
                    continue;
                }
                let binding_slots = slots
                    .iter()
                    .filter(|slot| slot.projection.starts_with(&binding.projection))
                    .cloned()
                    .collect::<Vec<_>>();
                if !binding_slots.is_empty() {
                    let source = CaptureSource::Pattern(*id);
                    self.drop_slots.insert(source.clone(), binding_slots);
                    sources.push(source);
                }
            }
        }
        self.drop_scopes
            .push(slots.into_iter().rev().collect::<Vec<_>>());
        sources
    }

    fn moved_pattern_projections(&self) -> Vec<Vec<DropProjection>> {
        self.pattern_bindings
            .last()
            .into_iter()
            .flat_map(|bindings| bindings.iter())
            .filter(|(id, binding)| {
                self.pattern_binding_mode(**id) == PatternBindingMode::Move
                    && !self.type_result.trait_env.type_is_copy(&binding.tc_ty)
            })
            .map(|(_, binding)| binding.projection.clone())
            .collect()
    }

    fn transfer_pattern_drop_flags(
        &self,
        builder: &mut Builder,
        source: &CaptureSource,
        base_projection: &[DropProjection],
    ) {
        let moved = self.moved_pattern_projections();
        let flags = self
            .drop_slots
            .get(source)
            .into_iter()
            .flatten()
            .filter(|slot| {
                moved.iter().any(|projection| {
                    let mut full = base_projection.to_vec();
                    full.extend(projection.iter().cloned());
                    slot.projection.starts_with(&full) || full.starts_with(&slot.projection)
                })
            })
            .map(|slot| slot.flag)
            .collect::<HashSet<_>>();
        for flag in flags {
            let inactive = builder.bconst(false);
            builder.store(inactive, flag);
        }
    }

    fn pop_pattern_drop_scope(&mut self, sources: Vec<CaptureSource>) {
        self.drop_scopes.pop();
        for source in sources {
            self.drop_slots.remove(&source);
        }
    }

    fn create_pattern_owner_slots(
        &mut self,
        builder: &mut Builder,
        body: &Body,
        pat: PatId,
        place: Value,
        value_ty: &type_checker::Type,
    ) -> Vec<DropSlot> {
        let value_ty = self.substitute_tc_type(value_ty);
        if self.type_result.trait_env.type_has_explicit_drop(&value_ty) {
            return self.create_drop_slots(builder, place, &value_ty, Vec::new());
        }
        let variant = match &body.pats[pat] {
            Pattern::TupleStruct { path, .. } | Pattern::Struct { path, .. } => {
                path.segments.last().and_then(|name| {
                    match self.classify_type_pattern(&value_ty, Some(&name.0)) {
                        TypePattern::EnumVariant {
                            enum_id,
                            variant_index,
                            args,
                        } => Some((enum_id, variant_index, args)),
                        TypePattern::Other => None,
                    }
                })
            }
            _ => None,
        };
        let Some((enum_id, variant_index, args)) = variant else {
            return self.create_drop_slots(builder, place, &value_ty, Vec::new());
        };

        let fields = self.enum_variant_payload_types(enum_id, &args, variant_index);
        let offset = self.enum_payload_offset(&self.hir.item_tree.enums[enum_id], variant_index);
        let mut slots = Vec::new();
        for (index, (_, field_ty)) in fields.into_iter().enumerate() {
            if !self.type_needs_drop(&field_ty, 0) {
                continue;
            }
            let field_index = 1 + offset + index;
            let field_place = builder.field_ptr(place, field_index, self.convert_type(&field_ty));
            slots.extend(self.create_drop_slots(
                builder,
                field_place,
                &field_ty,
                vec![DropProjection::Field(field_index)],
            ));
        }
        slots
    }

    fn classify_type_pattern(
        &self,
        value_ty: &type_checker::Type,
        name: Option<&str>,
    ) -> TypePattern {
        let type_checker::Type::Enum(enum_id, args) = value_ty else {
            return TypePattern::Other;
        };
        let Some(name) = name else {
            return TypePattern::Other;
        };
        self.hir.item_tree.enums[*enum_id]
            .variants
            .iter()
            .position(|variant| variant.name.0 == name)
            .map(|variant_index| TypePattern::EnumVariant {
                enum_id: *enum_id,
                variant_index,
                args: args.clone(),
            })
            .unwrap_or(TypePattern::Other)
    }

    fn enum_variant_payload_types(
        &self,
        enum_id: hir::item_tree::EnumId,
        args: &[type_checker::Type],
        variant_index: usize,
    ) -> Vec<(Option<String>, type_checker::Type)> {
        let enum_data = &self.hir.item_tree.enums[enum_id];
        let subst = enum_data
            .generics
            .iter()
            .chain(enum_data.const_generics.iter())
            .zip(args.iter())
            .map(|(name, ty)| (name.0.clone(), ty.clone()))
            .collect::<HashMap<_, _>>();
        let Some(variant) = enum_data.variants.get(variant_index) else {
            return Vec::new();
        };
        match &variant.kind {
            hir::item_tree::HirVariantKind::Unit => Vec::new(),
            hir::item_tree::HirVariantKind::Tuple(items) => items
                .iter()
                .map(|ty| (None, self.lower_hir_type_for_pattern(ty, &subst)))
                .collect(),
            hir::item_tree::HirVariantKind::Struct(items) => items
                .iter()
                .map(|field| {
                    (
                        Some(field.name.0.clone()),
                        self.lower_hir_type_for_pattern(&field.ty, &subst),
                    )
                })
                .collect(),
        }
    }

    fn struct_pattern_field_types(
        &self,
        struct_id: hir::item_tree::StructId,
        args: &[type_checker::Type],
    ) -> Vec<(String, type_checker::Type)> {
        let strukt = &self.hir.item_tree.structs[struct_id];
        let subst = strukt
            .generics
            .iter()
            .chain(strukt.const_generics.iter())
            .zip(args.iter())
            .map(|(name, ty)| (name.0.clone(), ty.clone()))
            .collect::<HashMap<_, _>>();
        strukt
            .fields
            .iter()
            .map(|field| {
                (
                    field.name.0.clone(),
                    self.lower_hir_type_for_pattern(&field.ty, &subst),
                )
            })
            .collect()
    }

    fn lower_hir_type_for_pattern(
        &self,
        ty: &hir::item_tree::HirTypeRef,
        subst: &HashMap<String, type_checker::Type>,
    ) -> type_checker::Type {
        use hir::item_tree::{HirConstArg, HirTypeRef};
        use type_checker::{ConstArg, FloatTy as TcFloatTy, IntTy as TcIntTy};

        match ty {
            HirTypeRef::Never => type_checker::Type::Never,
            HirTypeRef::Named(path) => {
                let Some(name) = path.as_single_name().map(|name| name.0.as_str()) else {
                    return type_checker::Type::Unknown;
                };
                if let Some(ty) = subst.get(name) {
                    return ty.clone();
                }
                match name {
                    "i8" => type_checker::Type::Int(TcIntTy::I8),
                    "i16" => type_checker::Type::Int(TcIntTy::I16),
                    "i32" => type_checker::Type::Int(TcIntTy::I32),
                    "i64" => type_checker::Type::Int(TcIntTy::I64),
                    "isize" => type_checker::Type::Int(TcIntTy::Isize),
                    "u8" => type_checker::Type::Int(TcIntTy::U8),
                    "u16" => type_checker::Type::Int(TcIntTy::U16),
                    "u32" => type_checker::Type::Int(TcIntTy::U32),
                    "u64" => type_checker::Type::Int(TcIntTy::U64),
                    "usize" => type_checker::Type::Int(TcIntTy::Usize),
                    "f32" => type_checker::Type::Float(TcFloatTy::F32),
                    "f64" => type_checker::Type::Float(TcFloatTy::F64),
                    "bool" => type_checker::Type::Bool,
                    "str" => type_checker::Type::Str,
                    "char" => type_checker::Type::Char,
                    _ => {
                        let args = path
                            .type_args
                            .iter()
                            .map(|arg| self.lower_hir_type_for_pattern(arg, subst))
                            .collect::<Vec<_>>();
                        if let Some((id, _)) = self
                            .hir
                            .item_tree
                            .structs
                            .iter()
                            .find(|(_, item)| item.name.0 == name)
                        {
                            type_checker::Type::Struct(id, args)
                        } else if let Some((id, _)) = self
                            .hir
                            .item_tree
                            .enums
                            .iter()
                            .find(|(_, item)| item.name.0 == name)
                        {
                            type_checker::Type::Enum(id, args)
                        } else {
                            type_checker::Type::Unknown
                        }
                    }
                }
            }
            HirTypeRef::Ref(inner, mutable) => type_checker::Type::Ref(
                Box::new(self.lower_hir_type_for_pattern(inner, subst)),
                *mutable,
            ),
            HirTypeRef::Ptr { mutable, inner } => type_checker::Type::Ptr {
                mutable: *mutable,
                inner: Box::new(self.lower_hir_type_for_pattern(inner, subst)),
            },
            HirTypeRef::Tuple(items) if items.is_empty() => type_checker::Type::Unit,
            HirTypeRef::Tuple(items) => type_checker::Type::Tuple(
                items
                    .iter()
                    .map(|item| self.lower_hir_type_for_pattern(item, subst))
                    .collect(),
            ),
            HirTypeRef::Slice(inner) => {
                type_checker::Type::Slice(Box::new(self.lower_hir_type_for_pattern(inner, subst)))
            }
            HirTypeRef::Array(inner, len) => type_checker::Type::Array(
                Box::new(self.lower_hir_type_for_pattern(inner, subst)),
                match len {
                    HirConstArg::Value(value) => ConstArg::Value(*value),
                    HirConstArg::Param(name) => match subst.get(&name.0) {
                        Some(type_checker::Type::Const(value)) => value.clone(),
                        _ => ConstArg::Param(name.0.clone()),
                    },
                    HirConstArg::Unknown => ConstArg::Unknown,
                    HirConstArg::Error => ConstArg::Error,
                },
            ),
            HirTypeRef::Const(value) => type_checker::Type::Const(match value {
                HirConstArg::Value(value) => ConstArg::Value(*value),
                HirConstArg::Param(name) => ConstArg::Param(name.0.clone()),
                HirConstArg::Unknown => ConstArg::Unknown,
                HirConstArg::Error => ConstArg::Error,
            }),
            HirTypeRef::ImplTrait {
                trait_ty,
                trait_range,
                callable,
                hidden,
            } => {
                if let Some(hidden) = hidden {
                    return subst
                        .get(&hidden.0)
                        .cloned()
                        .unwrap_or_else(|| type_checker::Type::Param(hidden.0.clone()));
                }
                let kind = match trait_ty.as_ref() {
                    HirTypeRef::Named(path) => {
                        match path.segments.last().map(|name| name.0.as_str()) {
                            Some("Fn") => type_checker::ClosureKind::Fn,
                            Some("FnMut") => type_checker::ClosureKind::FnMut,
                            Some("FnOnce") => type_checker::ClosureKind::FnOnce,
                            _ => return type_checker::Type::Unknown,
                        }
                    }
                    _ => return type_checker::Type::Unknown,
                };
                let Some(callable) = callable else {
                    return type_checker::Type::Unknown;
                };
                type_checker::Type::OpaqueCallable {
                    id: type_checker::OpaqueCallableId(*trait_range),
                    signature: type_checker::CallableSignature {
                        is_unsafe: false,
                        kind,
                        params: callable
                            .params
                            .iter()
                            .map(|param| self.lower_hir_type_for_pattern(param, subst))
                            .collect(),
                        ret: Box::new(self.lower_hir_type_for_pattern(&callable.ret, subst)),
                    },
                }
            }
            HirTypeRef::Unknown => type_checker::Type::Unknown,
            HirTypeRef::Error => type_checker::Type::Error,
        }
    }

    fn lower_enum_variant_value(
        &mut self,
        builder: &mut Builder,
        enum_id: hir::item_tree::EnumId,
        variant_index: usize,
        args: Vec<Value>,
        ty: Type,
    ) -> Value {
        let tag = builder.iconst(variant_index as u64, IntTy::U32);
        let offset = self.enum_payload_offset(&self.hir.item_tree.enums[enum_id], variant_index);
        let mut fields = vec![(0, tag)];
        fields.extend(
            args.into_iter()
                .enumerate()
                .map(|(index, value)| (1 + offset + index, value)),
        );
        builder.sparse_struct_value(fields, ty)
    }

    fn enum_payload_offset(
        &self,
        enum_data: &hir::item_tree::HirEnum,
        variant_index: usize,
    ) -> usize {
        enum_data
            .variants
            .iter()
            .take(variant_index)
            .map(|variant| match &variant.kind {
                hir::item_tree::HirVariantKind::Unit => 0,
                hir::item_tree::HirVariantKind::Tuple(items) => items.len(),
                hir::item_tree::HirVariantKind::Struct(items) => items.len(),
            })
            .sum()
    }

    /// Read a binding. `let` bindings live in `scope_map`; `match`/`for` arm
    /// bindings live in the arm-scoped `pattern_bindings` stack.
    fn binding_value(
        &mut self,
        builder: &mut Builder,
        id: PatternBindingId,
        ty: &Type,
    ) -> Option<Value> {
        if let Some(value) = self.scope_map.get(&id).copied() {
            return Some(if self.storage_bindings.contains(&id) {
                builder.load(value, ty.clone())
            } else {
                value
            });
        }
        let binding = self
            .pattern_bindings
            .iter()
            .rev()
            .find_map(|scope| scope.get(&id).cloned())?;
        Some(match binding.place {
            Some(place) => builder.load(place, binding.ty),
            None => binding.value,
        })
    }

    /// The address of a binding, materializing one if it only had a value.
    fn binding_place(&mut self, builder: &mut Builder, id: PatternBindingId) -> Option<Value> {
        if self.storage_bindings.contains(&id) {
            return self.scope_map.get(&id).copied();
        }
        let binding = self
            .pattern_bindings
            .iter_mut()
            .rev()
            .find_map(|scope| scope.get_mut(&id))?;
        if let Some(place) = binding.place {
            return Some(place);
        }
        let place = builder.heap_alloc(binding.ty.clone());
        builder.store(binding.value, place);
        binding.place = Some(place);
        Some(place)
    }

    fn push_pattern_binding(&mut self, body: &Body, pat: PatId, value: Value, ty: Type) {
        let mut scope = HashMap::new();
        if matches!(body.pats[pat], Pattern::Binding { .. }) {
            scope.insert(
                PatternBindingId {
                    pattern: pat,
                    field: None,
                },
                PatternBindingValue::direct(
                    value,
                    ty,
                    type_checker::Type::Int(type_checker::IntTy::I32),
                    None,
                    Vec::new(),
                ),
            );
        }
        self.pattern_bindings.push(scope);
    }

    fn is_std_range_expr(&self, expr: ExprId) -> bool {
        self.current_body
            .and_then(|bid| self.type_result.expr_types.get(&(bid, expr)))
            .and_then(|ty| match ty {
                type_checker::Type::Struct(sid, _) => Some(*sid),
                _ => None,
            })
            .map(|sid| {
                let s = &self.hir.item_tree.structs[sid];
                if s.name.0 != "Range" || s.fields.len() != 2 {
                    return false;
                }
                let is_i32 = |ty: &HirTypeRef| {
                    matches!(ty, HirTypeRef::Named(p)
                        if p.anchor == PathAnchor::Plain
                            && p.segments.len() == 1
                            && p.segments[0].0 == "i32"
                            && p.type_args.is_empty())
                };
                s.fields[0].name.0 == "start"
                    && s.fields[1].name.0 == "end"
                    && is_i32(&s.fields[0].ty)
                    && is_i32(&s.fields[1].ty)
            })
            .unwrap_or(false)
    }

    fn array_iter_info(&self, expr: ExprId) -> Option<(Type, usize)> {
        self.current_body
            .and_then(|bid| self.type_result.expr_types.get(&(bid, expr)))
            .and_then(|ty| match ty {
                type_checker::Type::Array(inner, len) => {
                    Some((self.convert_type(inner), len.as_usize()?))
                }
                _ => None,
            })
    }

    fn callee_function_id(&self, callee: ExprId) -> Option<hir::item_tree::FunctionId> {
        self.current_body
            .and_then(|bid| self.type_result.expr_types.get(&(bid, callee)))
            .and_then(|ty| match ty {
                type_checker::Type::FunctionItem { function: fid, .. } => Some(*fid),
                _ => None,
            })
    }

    fn lower_builtin_call(&mut self, builder: &mut Builder, callee: ExprId) -> Option<Value> {
        let fid = self.callee_function_id(callee)?;
        let function = &self.hir.item_tree.functions[fid];
        if !self.hir.std_loaded || self.hir.package_for_range(function.name_range).is_some() {
            return None;
        }
        let builtin = function
            .attrs
            .iter()
            .find(|attr| attr.name.0 == "builtin")
            .and_then(|attr| attr.value.clone())?;
        let body_id = self.current_body?;
        let generic_call = self.type_result.generic_calls.get(&(body_id, callee));
        match builtin.as_str() {
            "size_of" => {
                let ty = self.convert_type(generic_call?.args.first()?);
                Some(builder.size_of(ty))
            }
            _ => None,
        }
    }

    fn lower_operator_call(
        &mut self,
        builder: &mut Builder,
        lhs: ExprId,
        rhs: Option<ExprId>,
        fid: hir::item_tree::FunctionId,
        args: Vec<Value>,
        ret_ty: Type,
    ) -> Value {
        let name = self
            .mono_method_name(fid, lhs, rhs)
            .unwrap_or_else(|| self.function_name(fid));
        builder.call(FuncRef::Local(name), args, ret_ty)
    }

    fn lower_comparison(
        &mut self,
        builder: &mut Builder,
        op: &HirBinOp,
        lhs: Value,
        rhs: Value,
        lhs_ty: &type_checker::Type,
        rhs_ty: &type_checker::Type,
    ) -> Value {
        let lhs_ty = self.substitute_tc_type(lhs_ty);
        let rhs_ty = self.substitute_tc_type(rhs_ty);

        match (&lhs_ty, &rhs_ty) {
            (type_checker::Type::Tuple(lhs_elements), type_checker::Type::Tuple(rhs_elements))
                if lhs_elements.len() == rhs_elements.len() =>
            {
                let elements = lhs_elements
                    .iter()
                    .zip(rhs_elements)
                    .enumerate()
                    .map(|(index, (lhs_ty, rhs_ty))| {
                        (
                            builder.extract_value(lhs, index, self.convert_type(lhs_ty)),
                            builder.extract_value(rhs, index, self.convert_type(rhs_ty)),
                            lhs_ty.clone(),
                            rhs_ty.clone(),
                        )
                    })
                    .collect();
                return self.lower_aggregate_comparison(builder, op, elements);
            }
            (
                type_checker::Type::Array(lhs_inner, lhs_len),
                type_checker::Type::Array(rhs_inner, rhs_len),
            ) if lhs_len == rhs_len => {
                let Some(len) = lhs_len.as_usize() else {
                    return self.lower_comparison_leaf(builder, op, lhs, rhs, &lhs_ty, &rhs_ty);
                };
                let lhs_mir_ty = self.convert_type(lhs_inner);
                let rhs_mir_ty = self.convert_type(rhs_inner);
                let elements = (0..len)
                    .map(|index| {
                        let index_value = builder.iconst(index as u64, IntTy::Usize);
                        let lhs_ptr = builder.index_ptr(lhs, index_value, lhs_mir_ty.clone());
                        let rhs_ptr = builder.index_ptr(rhs, index_value, rhs_mir_ty.clone());
                        (
                            builder.load(lhs_ptr, lhs_mir_ty.clone()),
                            builder.load(rhs_ptr, rhs_mir_ty.clone()),
                            lhs_inner.as_ref().clone(),
                            rhs_inner.as_ref().clone(),
                        )
                    })
                    .collect();
                return self.lower_aggregate_comparison(builder, op, elements);
            }
            _ => {}
        }

        self.lower_comparison_leaf(builder, op, lhs, rhs, &lhs_ty, &rhs_ty)
    }

    fn lower_aggregate_comparison(
        &mut self,
        builder: &mut Builder,
        op: &HirBinOp,
        elements: Vec<(Value, Value, type_checker::Type, type_checker::Type)>,
    ) -> Value {
        match op {
            HirBinOp::Eq => self.lower_aggregate_equality(builder, elements, false),
            HirBinOp::Neq => self.lower_aggregate_equality(builder, elements, true),
            HirBinOp::Lt | HirBinOp::Gt | HirBinOp::LtEq | HirBinOp::GtEq => {
                self.lower_aggregate_ordering(builder, *op, elements)
            }
            _ => unreachable!("aggregate comparison called with non-comparison op"),
        }
    }

    fn lower_aggregate_equality(
        &mut self,
        builder: &mut Builder,
        elements: Vec<(Value, Value, type_checker::Type, type_checker::Type)>,
        negate: bool,
    ) -> Value {
        if elements.is_empty() {
            return builder.bconst(!negate);
        }

        let merge_block = builder.func.new_block_labeled("cmp_merge");
        let mut phi_args = Vec::with_capacity(elements.len() + 1);
        for (lhs, rhs, lhs_ty, rhs_ty) in elements {
            let equal = self.lower_comparison(builder, &HirBinOp::Eq, lhs, rhs, &lhs_ty, &rhs_ty);
            let next_block = builder.func.new_block_labeled("cmp_next");
            let result_block = builder.func.new_block_labeled("cmp_result");
            builder.set_cond_branch(equal, next_block, result_block);

            builder.switch_to_block(result_block);
            let result = builder.bconst(negate);
            let result_exit = builder.current_block;
            builder.set_branch(merge_block);
            phi_args.push((result, result_exit));
            builder.switch_to_block(next_block);
        }

        let result = builder.bconst(!negate);
        let result_exit = builder.current_block;
        builder.set_branch(merge_block);
        phi_args.push((result, result_exit));

        builder.switch_to_block(merge_block);
        builder
            .func
            .push_inst(merge_block, Inst::new(InstKind::Phi(phi_args), Type::Bool))
    }

    fn lower_aggregate_ordering(
        &mut self,
        builder: &mut Builder,
        op: HirBinOp,
        elements: Vec<(Value, Value, type_checker::Type, type_checker::Type)>,
    ) -> Value {
        if elements.is_empty() {
            return builder.bconst(matches!(op, HirBinOp::LtEq | HirBinOp::GtEq));
        }

        let merge_block = builder.func.new_block_labeled("cmp_merge");
        let mut phi_args = Vec::with_capacity(elements.len() + 1);
        for (lhs, rhs, lhs_ty, rhs_ty) in elements {
            let equal = self.lower_comparison(builder, &HirBinOp::Eq, lhs, rhs, &lhs_ty, &rhs_ty);
            let next_block = builder.func.new_block_labeled("cmp_next");
            let result_block = builder.func.new_block_labeled("cmp_result");
            builder.set_cond_branch(equal, next_block, result_block);

            builder.switch_to_block(result_block);
            let decision_op = match op {
                HirBinOp::Lt | HirBinOp::LtEq => HirBinOp::Lt,
                HirBinOp::Gt | HirBinOp::GtEq => HirBinOp::Gt,
                _ => unreachable!("non-ordering op in aggregate ordering"),
            };
            let result = self.lower_comparison(builder, &decision_op, lhs, rhs, &lhs_ty, &rhs_ty);
            let result_exit = builder.current_block;
            if builder.needs_return() {
                builder.set_branch(merge_block);
                phi_args.push((result, result_exit));
            }
            builder.switch_to_block(next_block);
        }

        let result = builder.bconst(matches!(op, HirBinOp::LtEq | HirBinOp::GtEq));
        let result_exit = builder.current_block;
        builder.set_branch(merge_block);
        phi_args.push((result, result_exit));

        builder.switch_to_block(merge_block);
        builder
            .func
            .push_inst(merge_block, Inst::new(InstKind::Phi(phi_args), Type::Bool))
    }

    fn lower_comparison_leaf(
        &mut self,
        builder: &mut Builder,
        op: &HirBinOp,
        lhs: Value,
        rhs: Value,
        lhs_ty: &type_checker::Type,
        rhs_ty: &type_checker::Type,
    ) -> Value {
        if matches!(
            (lhs_ty, rhs_ty),
            (type_checker::Type::Unit, type_checker::Type::Unit)
        ) {
            return builder.bconst(matches!(op, HirBinOp::Eq | HirBinOp::LtEq | HirBinOp::GtEq));
        }
        if builtin_comparison_types(op, lhs_ty, rhs_ty) {
            return builder.cmp(convert_cmp_op(op), lhs, rhs);
        }

        if let Some((lang, method)) = comparison_trait(op)
            && let Some(lang_item) = type_checker::lang_items::LangItem::from_name(lang)
            && let Some(trait_id) = self.type_result.trait_env.lang_items.get(lang_item)
            && let Some(fid) = self.find_trait_impl_method(trait_id, method, lhs_ty, Some(rhs_ty))
        {
            let Some(receiver_ty) = self.hir.item_tree.functions[fid]
                .params
                .first()
                .map(|param| param.ty.clone())
            else {
                return builder.cmp(convert_cmp_op(op), lhs, rhs);
            };
            let Some(rhs_param_ty) = self.hir.item_tree.functions[fid]
                .params
                .get(1)
                .map(|param| param.ty.clone())
            else {
                return builder.cmp(convert_cmp_op(op), lhs, rhs);
            };
            let lhs_arg = self.lower_comparison_arg(builder, lhs, lhs_ty, &receiver_ty);
            let rhs_arg = self.lower_comparison_arg(builder, rhs, rhs_ty, &rhs_param_ty);
            let name = self
                .mono_method_name_for_receiver(fid, lhs_ty, Some(rhs_ty))
                .unwrap_or_else(|| self.function_name(fid));
            return builder.call(FuncRef::Local(name), vec![lhs_arg, rhs_arg], Type::Bool);
        }

        builder.cmp(convert_cmp_op(op), lhs, rhs)
    }

    fn lower_comparison_arg(
        &self,
        builder: &mut Builder,
        value: Value,
        actual_ty: &type_checker::Type,
        expected: &hir::item_tree::HirTypeRef,
    ) -> Value {
        let actual_mir_ty = self.convert_type(actual_ty);
        match expected {
            hir::item_tree::HirTypeRef::Ref(_, _) if matches!(actual_mir_ty, Type::Ref(_, _)) => {
                value
            }
            hir::item_tree::HirTypeRef::Ref(_, mutable) => {
                let place = builder.alloca(actual_mir_ty.clone());
                builder.store(value, place);
                builder.unop(
                    if *mutable { UnOp::MutRef } else { UnOp::Ref },
                    place,
                    Type::Ref(Box::new(actual_mir_ty), *mutable),
                )
            }
            _ => value,
        }
    }

    fn lower_builtin_operator_method_call(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        base: ExprId,
        args: &[ExprId],
        op: BuiltinOperator,
    ) -> Value {
        let value_ty = self
            .current_body
            .and_then(|bid| self.type_result.expr_types.get(&(bid, base)))
            .map(|ty| self.convert_type(ty))
            .unwrap_or(Type::Unit);
        match op {
            BuiltinOperator::Binary(op) => {
                let lhs = self.lower_expr(builder, param_values, body, base);
                let rhs = self.lower_expr(
                    builder,
                    param_values,
                    body,
                    *args.first().expect("checked binary operator missing rhs"),
                );
                builder.binop(op, lhs, rhs, value_ty)
            }
            BuiltinOperator::Unary(op) => {
                let operand = self.lower_expr(builder, param_values, body, base);
                builder.unop(op, operand, value_ty)
            }
            BuiltinOperator::Assign(op) => {
                let place = self.lower_lvalue(builder, param_values, body, base);
                let rhs = self.lower_expr(
                    builder,
                    param_values,
                    body,
                    *args
                        .first()
                        .expect("checked assignment operator missing rhs"),
                );
                let lhs = builder.load(place, value_ty.clone());
                let value = builder.binop(op, lhs, rhs, value_ty);
                builder.store(value, place);
                builder.unit_const()
            }
        }
    }

    fn actual_method_fid(
        &mut self,
        callee: ExprId,
        fid: hir::item_tree::FunctionId,
        base: ExprId,
    ) -> hir::item_tree::FunctionId {
        let Some(body_id) = self.current_body else {
            return fid;
        };
        let Some(receiver_ty) = self.type_result.expr_types.get(&(body_id, base)) else {
            return fid;
        };
        if let Some(call) = self.type_result.trait_method_calls.get(&(body_id, callee)) {
            return self
                .find_trait_impl_method(call.trait_id, &call.method, receiver_ty, None)
                .unwrap_or(fid);
        }
        let Some(imp) = self.impl_for_method(fid) else {
            return fid;
        };
        if self.impl_type_matches(imp, receiver_ty) {
            return fid;
        }
        let Some(trait_ty) = &imp.trait_ty else {
            return fid;
        };
        let Some(trait_id) = self.resolve_trait_ref(trait_ty) else {
            return fid;
        };
        let method_name = &self.hir.item_tree.functions[fid].name;
        self.find_trait_impl_method(trait_id, &method_name.0, receiver_ty, None)
            .unwrap_or(fid)
    }

    fn find_trait_impl_method(
        &self,
        trait_id: hir::item_tree::TraitId,
        method_name: &str,
        receiver_ty: &type_checker::Type,
        rhs_ty: Option<&type_checker::Type>,
    ) -> Option<hir::item_tree::FunctionId> {
        let receiver_ty = self.substitute_tc_type(receiver_ty);
        let dereferenced = match &receiver_ty {
            type_checker::Type::Ref(inner, _) => Some(inner.as_ref()),
            _ => None,
        };
        for receiver_ty in std::iter::once(&receiver_ty).chain(dereferenced) {
            for (_, candidate) in self.hir.item_tree.impls.iter() {
                let Some(candidate_trait) = candidate.trait_ty.as_ref() else {
                    continue;
                };
                if self.resolve_trait_ref(candidate_trait) != Some(trait_id)
                    || !self.impl_type_matches(candidate, receiver_ty)
                    || !self.impl_trait_args_match(candidate, receiver_ty, rhs_ty)
                {
                    continue;
                }
                return candidate
                    .methods
                    .iter()
                    .copied()
                    .find(|candidate_fid| {
                        self.hir.item_tree.functions[*candidate_fid].name.0 == method_name
                    })
                    .or_else(|| self.default_method(trait_id, method_name));
            }
        }
        None
    }

    fn default_method(
        &self,
        trait_id: hir::item_tree::TraitId,
        method_name: &str,
    ) -> Option<hir::item_tree::FunctionId> {
        self.hir.item_tree.traits[trait_id]
            .default_methods
            .iter()
            .copied()
            .find(|fid| self.hir.item_tree.functions[*fid].name.0 == method_name)
    }

    fn lower_receiver_arg(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        base: ExprId,
        expected: &hir::item_tree::HirTypeRef,
    ) -> Value {
        let base_ty = self
            .current_body
            .and_then(|bid| self.type_result.expr_types.get(&(bid, base)))
            .map(|t| self.convert_type(t))
            .unwrap_or(Type::Unit);

        match expected {
            hir::item_tree::HirTypeRef::Ref(_, _) if matches!(base_ty, Type::Ref(_, _)) => {
                self.lower_expr(builder, param_values, body, base)
            }
            hir::item_tree::HirTypeRef::Ref(_, true) => {
                let place = self.lower_lvalue(builder, param_values, body, base);
                builder.unop(
                    convert_unop(&HirUnOp::MutRef),
                    place,
                    Type::Ref(Box::new(base_ty), true),
                )
            }
            hir::item_tree::HirTypeRef::Ref(_, mutable) => {
                let base_val = self.lower_lvalue(builder, param_values, body, base);
                let expected_ty = Type::Ref(Box::new(base_ty), *mutable);
                builder.unop(convert_unop(&HirUnOp::Ref), base_val, expected_ty)
            }
            _ => self.lower_expr(builder, param_values, body, base),
        }
    }

    fn resolve_field_index(&self, base: ExprId, field_name: &hir::Name) -> usize {
        let Some(body_id) = self.current_body else {
            return 0;
        };
        resolve_field_index(self.hir, self.type_result, body_id, base, field_name)
    }

    fn capture_place_from_expr(&self, body: &Body, expr_id: ExprId) -> Option<CapturePlace> {
        match &body.exprs[expr_id] {
            Expr::Path {
                resolved: Some(resolved),
                ..
            } => {
                let source = match resolved {
                    ResolvedName::PatternBinding(id) => CaptureSource::Pattern(*id),
                    ResolvedName::Param(index) => CaptureSource::Param(*index),
                    ResolvedName::LambdaParam { lambda, index } => CaptureSource::LambdaParam {
                        lambda: *lambda,
                        index: *index,
                    },
                    _ => return None,
                };
                Some(CapturePlace::root(source))
            }
            Expr::FieldAccess { base, field } => {
                let mut place = self.capture_place_from_expr(body, *base)?;
                let base_ty = self
                    .current_body
                    .and_then(|body_id| self.type_result.expr_types.get(&(body_id, *base)));
                if matches!(base_ty, Some(type_checker::Type::Struct(..))) {
                    place
                        .projections
                        .push(Projection::Field(self.resolve_field_index(*base, field)));
                }
                Some(place)
            }
            Expr::IndexAccess { base, index } => {
                let mut place = self.capture_place_from_expr(body, *base)?;
                if let Expr::IntLiteral { value, .. } = body.exprs[*index]
                    && let Ok(index) = usize::try_from(value)
                {
                    place.projections.push(Projection::Index(Some(index)));
                }
                Some(place)
            }
            Expr::Unary {
                operand,
                op: HirUnOp::Deref,
            } => self.capture_place_from_expr(body, *operand),
            _ => None,
        }
    }

    /// Resolve a path as an lvalue (storage location) without loading.
    /// For mut bindings: returns the alloca pointer directly.
    /// For non-mut bindings / params: returns the value as-is (SSA values are
    /// immutable, so treating them as both value and location is safe).
    fn lower_lvalue(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        expr_id: ExprId,
    ) -> Value {
        if let Some(access) = self
            .capture_place_from_expr(body, expr_id)
            .and_then(|place| self.capture_access_for_place(builder, &place))
            && self
                .current_body
                .and_then(|body_id| self.type_result.expr_types.get(&(body_id, expr_id)))
                .map(|ty| self.convert_type(ty))
                .is_some_and(|ty| ty == access.ty)
        {
            return access.place;
        }
        let expr = &body.exprs[expr_id];
        match expr {
            Expr::Path { resolved, .. } => match resolved {
                Some(ResolvedName::Param(idx)) => self
                    .parameter_storage
                    .get(&CaptureSource::Param(*idx))
                    .copied()
                    .or_else(|| param_values.get(*idx).copied())
                    .unwrap_or_else(|| builder.unit_const()),
                Some(ResolvedName::LambdaParam { lambda, index }) => {
                    let source = CaptureSource::LambdaParam {
                        lambda: *lambda,
                        index: *index,
                    };
                    self.parameter_storage
                        .get(&source)
                        .copied()
                        .or_else(|| {
                            (self.current_lambda == Some(*lambda))
                                .then(|| param_values.get(*index).copied())
                                .flatten()
                        })
                        .unwrap_or_else(|| builder.unit_const())
                }
                Some(ResolvedName::PatternBinding(id)) => {
                    // A `let` binding without storage has no address; the
                    // SSA value doubles as its location, as before.
                    self.scope_map
                        .get(id)
                        .copied()
                        .or_else(|| self.binding_place(builder, *id))
                        .unwrap_or_else(|| builder.unit_const())
                }
                _ => builder.unit_const(),
            },
            Expr::IndexAccess { base, index } => {
                let base_val = self.lower_place_base(builder, param_values, body, *base);
                let index_val = self.lower_expr(builder, param_values, body, *index);
                let mir_type = self
                    .current_body
                    .and_then(|bid| self.type_result.expr_types.get(&(bid, expr_id)))
                    .map(|t| self.convert_type(t))
                    .unwrap_or(Type::Unit);
                if let Some(len) = self.index_len(builder, base_val, *base) {
                    builder.checked_index_ptr(base_val, index_val, len, mir_type)
                } else {
                    builder.index_ptr(base_val, index_val, mir_type)
                }
            }
            Expr::FieldAccess { base, field } => {
                let base_val = self.lower_place_base(builder, param_values, body, *base);
                let field_idx = self.resolve_field_index(*base, field);
                let field_ty = self
                    .current_body
                    .and_then(|bid| self.type_result.expr_types.get(&(bid, expr_id)))
                    .map(|t| self.convert_type(t))
                    .unwrap_or(Type::Unit);
                builder.field_ptr(base_val, field_idx, field_ty)
            }
            Expr::Unary {
                operand,
                op: HirUnOp::Deref,
            } => self.lower_expr(builder, param_values, body, *operand),
            _ => {
                let ty = self
                    .current_body
                    .and_then(|body_id| self.type_result.expr_types.get(&(body_id, expr_id)))
                    .map(|ty| self.convert_type(ty))
                    .unwrap_or(Type::Unit);
                let value = self.lower_expr(builder, param_values, body, expr_id);
                let place = if self
                    .current_body
                    .is_some_and(|body_id| self.analysis.temporary_escapes(body_id, expr_id))
                {
                    builder.heap_alloc(ty)
                } else {
                    builder.alloca(ty)
                };
                builder.store(value, place);
                place
            }
        }
    }

    fn lower_place_base(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        base: ExprId,
    ) -> Value {
        let indirect = self
            .current_body
            .and_then(|body_id| self.type_result.expr_types.get(&(body_id, base)))
            .is_some_and(|ty| {
                matches!(
                    ty,
                    type_checker::Type::Ref(..) | type_checker::Type::Ptr { .. }
                )
            });
        if indirect {
            self.lower_expr(builder, param_values, body, base)
        } else {
            self.lower_lvalue(builder, param_values, body, base)
        }
    }

    // 语句降级

    fn lower_stmt(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        stmt_id: StmtId,
    ) {
        let stmt = &body.stmts[stmt_id];
        match stmt {
            Stmt::Let { pat, init, ty, .. } => {
                let (pat, init) = (*pat, *init);
                let value_ty = init
                    .and_then(|init_expr| self.adjusted_expr_type(init_expr))
                    .cloned()
                    .or_else(|| {
                        self.current_body
                            .and_then(|body_id| self.type_result.pattern_types.get(&(body_id, pat)))
                            .cloned()
                    })
                    .unwrap_or_else(|| self.lower_hir_type_for_pattern(ty, &self.generic_tc_subst));
                let needs_drop = self.type_needs_drop(&value_ty, 0);
                let delayed = init.is_none();
                // A destructuring `let` keeps one slot for the whole value and
                // hands out projections, so the slot has to satisfy whichever
                // of its bindings is the most demanding.
                let bindings = let_pattern_bindings(body, pat);
                let is_mut = bindings.iter().any(|(_, is_mut)| *is_mut);
                let escapes = self.current_body.is_some_and(|bid| {
                    bindings
                        .iter()
                        .any(|(id, _)| self.analysis.escapes(bid, *id))
                });
                let needs_address = self.current_body.is_some_and(|bid| {
                    bindings
                        .iter()
                        .any(|(id, _)| self.analysis.local_needs_address(bid, *id))
                });
                let root = PatternBindingId {
                    pattern: pat,
                    field: None,
                };

                let val = if escapes {
                    // Use the checked binding type for delayed declarations.
                    let alloc_ty = init
                        .and_then(|init_expr| self.adjusted_expr_type(init_expr))
                        .map(|t| self.convert_type(t))
                        .unwrap_or_else(|| self.convert_type(&value_ty));
                    let ptr = builder.heap_alloc(alloc_ty);
                    if let Some(init_expr) = init {
                        let init_val = self.lower_expr(builder, param_values, body, init_expr);
                        builder.store(init_val, ptr);
                    }
                    self.storage_bindings.insert(root);
                    ptr
                } else if delayed || is_mut || needs_address || needs_drop {
                    // Mutable and captured-by-reference bindings need stable stack storage.
                    let alloc_ty = init
                        .and_then(|init_expr| self.adjusted_expr_type(init_expr))
                        .map(|t| self.convert_type(t))
                        .unwrap_or_else(|| self.convert_type(&value_ty));
                    let ptr = builder.alloca(alloc_ty);
                    if let Some(init_expr) = init {
                        let init_val = self.lower_expr(builder, param_values, body, init_expr);
                        builder.store(init_val, ptr);
                    }
                    self.storage_bindings.insert(root);
                    ptr
                } else if let Some(init_expr) = init {
                    self.lower_expr(builder, param_values, body, init_expr)
                } else {
                    builder.unit_const()
                };
                self.scope_map.insert(root, val);
                let slots = if needs_drop {
                    let slots = self.create_drop_slots(builder, val, &value_ty, Vec::new());
                    if delayed {
                        for slot in &slots {
                            let inactive = builder.bconst(false);
                            builder.store(inactive, slot.flag);
                        }
                    }
                    slots
                } else {
                    Vec::new()
                };

                let mut bound = vec![(root, Vec::new())];
                if !matches!(body.pats[pat], Pattern::Binding { .. }) {
                    let source = if self.storage_bindings.contains(&root) {
                        LetSource::Place(val)
                    } else {
                        LetSource::Value(val)
                    };
                    bound.clear();
                    self.bind_let_pattern(
                        builder,
                        body,
                        pat,
                        source,
                        &value_ty,
                        Vec::new(),
                        &mut bound,
                    );
                }
                // Drop flags hang off each binding, not off the slot, so moving
                // one element out of a destructured `let` only disarms its own
                // fields.
                for (id, projection) in bound {
                    let owned = slots
                        .iter()
                        .filter(|slot| slot.projection.starts_with(&projection))
                        .cloned()
                        .collect::<Vec<_>>();
                    if !owned.is_empty() {
                        self.register_drop_slots(CaptureSource::Pattern(id), &owned);
                    }
                }
                if let Some(scope) = self.drop_scopes.last_mut() {
                    scope.extend(slots.into_iter().rev());
                }
            }
            Stmt::Expr { expr } => {
                self.lower_expr(builder, param_values, body, *expr);
            }
            Stmt::Return { value } => {
                let rv = value.map(|v| self.lower_expr(builder, param_values, body, v));
                self.emit_drop_scopes_since(builder, 0);
                builder.set_return(rv);
            }
            Stmt::Break => {
                let target = *self
                    .loop_targets
                    .last()
                    .expect("break statement outside a checked loop");
                self.emit_drop_scopes_since(builder, target.drop_depth);
                builder.set_branch(target.break_block);
            }
            Stmt::Continue => {
                let target = *self
                    .loop_targets
                    .last()
                    .expect("continue statement outside a checked loop");
                self.emit_drop_scopes_since(builder, target.drop_depth);
                builder.set_branch(target.continue_block);
            }
            Stmt::Item { .. } => {}
        }
    }

    /// Bind the elements of a destructuring `let`. The whole initializer already
    /// lives in one slot, so each binding is a projection of it rather than a
    /// separate allocation — that keeps `&a` valid for `let (a, b) = pair`.
    #[allow(clippy::too_many_arguments)]
    fn bind_let_pattern(
        &mut self,
        builder: &mut Builder,
        body: &Body,
        pat: PatId,
        source: LetSource,
        value_ty: &type_checker::Type,
        projection: Vec<DropProjection>,
        bound: &mut Vec<(PatternBindingId, Vec<DropProjection>)>,
    ) {
        let (source, value_ty) = self.adjust_let_pattern_source(builder, pat, source, value_ty);
        let value_ty = &value_ty;
        match body.pats[pat].clone() {
            Pattern::Binding { .. } => self.bind_let_element(
                builder,
                body,
                PatternBindingId {
                    pattern: pat,
                    field: None,
                },
                source,
                value_ty,
                projection,
                bound,
            ),
            Pattern::Reference { pattern, .. } => {
                let type_checker::Type::Ref(inner, _) = value_ty else {
                    return;
                };
                let reference = match source {
                    LetSource::Place(place) => builder.load(place, self.convert_type(value_ty)),
                    LetSource::Value(value) => value,
                };
                let inner_value = builder.load(reference, self.convert_type(inner));
                self.bind_let_pattern(
                    builder,
                    body,
                    pattern,
                    LetSource::Value(inner_value),
                    inner,
                    projection,
                    bound,
                );
            }
            Pattern::Tuple { elements } => {
                let type_checker::Type::Tuple(element_types) = value_ty else {
                    return;
                };
                let element_types = element_types.clone();
                for (index, child) in elements.into_iter().enumerate() {
                    let Some(child_ty) = element_types.get(index) else {
                        break;
                    };
                    let child_source = self.project(builder, source, index, child_ty);
                    let mut child_projection = projection.clone();
                    child_projection.push(DropProjection::Field(index));
                    self.bind_let_pattern(
                        builder,
                        body,
                        child,
                        child_source,
                        child_ty,
                        child_projection,
                        bound,
                    );
                }
            }
            Pattern::Struct { fields, .. } => {
                let type_checker::Type::Struct(struct_id, args) = value_ty else {
                    return;
                };
                let field_types = self.struct_pattern_field_types(*struct_id, args);
                for (binding_index, field) in fields.into_iter().enumerate() {
                    let Some((index, (_, child_ty))) = field_types
                        .iter()
                        .enumerate()
                        .find(|(_, (name, _))| *name == field.name.0)
                    else {
                        continue;
                    };
                    let child_ty = child_ty.clone();
                    let child_source = self.project(builder, source, index, &child_ty);
                    let mut child_projection = projection.clone();
                    child_projection.push(DropProjection::Field(index));
                    match field.pat {
                        Some(child) => self.bind_let_pattern(
                            builder,
                            body,
                            child,
                            child_source,
                            &child_ty,
                            child_projection,
                            bound,
                        ),
                        None => self.bind_let_element(
                            builder,
                            body,
                            PatternBindingId {
                                pattern: pat,
                                field: Some(binding_index),
                            },
                            child_source,
                            &child_ty,
                            child_projection,
                            bound,
                        ),
                    }
                }
            }
            // ponytail: enum patterns are refutable and rejected by E0057, and
            // Riddle has no tuple structs, so nothing else can reach a `let`.
            Pattern::Wildcard
            | Pattern::Literal(_)
            | Pattern::Path { .. }
            | Pattern::TupleStruct { .. } => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn bind_let_element(
        &mut self,
        builder: &mut Builder,
        body: &Body,
        id: PatternBindingId,
        source: LetSource,
        value_ty: &type_checker::Type,
        projection: Vec<DropProjection>,
        bound: &mut Vec<(PatternBindingId, Vec<DropProjection>)>,
    ) {
        let mode = self.pattern_binding_mode(id);
        let binding_ty = self.pattern_binding_type(id, value_ty);
        match mode {
            PatternBindingMode::Move => match source {
                LetSource::Place(place) => {
                    self.storage_bindings.insert(id);
                    self.scope_map.insert(id, place);
                }
                LetSource::Value(value) => {
                    self.bind_let_value(builder, body, id, value, &binding_ty);
                }
            },
            PatternBindingMode::Ref | PatternBindingMode::RefMut => {
                let place = match source {
                    LetSource::Place(place) => place,
                    LetSource::Value(value) => {
                        self.materialize_pattern_place(builder, value, None, value_ty)
                    }
                };
                let op = if mode == PatternBindingMode::RefMut {
                    UnOp::MutRef
                } else {
                    UnOp::Ref
                };
                let value = builder.unop(op, place, self.convert_type(&binding_ty));
                self.bind_let_value(builder, body, id, value, &binding_ty);
            }
        }
        if mode == PatternBindingMode::Move {
            bound.push((id, projection));
        }
    }

    fn bind_let_value(
        &mut self,
        builder: &mut Builder,
        body: &Body,
        id: PatternBindingId,
        value: Value,
        value_ty: &type_checker::Type,
    ) {
        let is_mut = id.field.is_none()
            && matches!(body.pats[id.pattern], Pattern::Binding { is_mut: true, .. });
        let escapes = self
            .current_body
            .is_some_and(|body_id| self.analysis.escapes(body_id, id));
        let needs_address = self
            .current_body
            .is_some_and(|body_id| self.analysis.local_needs_address(body_id, id));
        let place = if escapes {
            Some(builder.heap_alloc(self.convert_type(value_ty)))
        } else if is_mut || needs_address || self.type_needs_drop(value_ty, 0) {
            Some(builder.alloca(self.convert_type(value_ty)))
        } else {
            None
        };
        if let Some(place) = place {
            builder.store(value, place);
            self.storage_bindings.insert(id);
            self.scope_map.insert(id, place);
        } else {
            self.scope_map.insert(id, value);
        }
    }

    fn adjust_let_pattern_source(
        &mut self,
        builder: &mut Builder,
        pat: PatId,
        mut source: LetSource,
        value_ty: &type_checker::Type,
    ) -> (LetSource, type_checker::Type) {
        let target = self.pattern_type(pat).unwrap_or_else(|| value_ty.clone());
        let mut current = value_ty.clone();
        while current != target {
            let type_checker::Type::Ref(inner, _) = &current else {
                break;
            };
            let reference = match source {
                LetSource::Place(place) => builder.load(place, self.convert_type(&current)),
                LetSource::Value(value) => value,
            };
            current = inner.as_ref().clone();
            source = LetSource::Place(reference);
        }
        (source, current)
    }

    fn project(
        &mut self,
        builder: &mut Builder,
        source: LetSource,
        index: usize,
        field_ty: &type_checker::Type,
    ) -> LetSource {
        let mir_ty = self.convert_type(field_ty);
        match source {
            LetSource::Place(place) => LetSource::Place(builder.field_ptr(place, index, mir_ty)),
            LetSource::Value(value) => {
                LetSource::Value(builder.extract_value(value, index, mir_ty))
            }
        }
    }

    fn adjusted_expr_type(&self, expr: ExprId) -> Option<&type_checker::Type> {
        let body = self.current_body?;
        self.type_result
            .expr_coercions
            .get(&(body, expr))
            .or_else(|| self.type_result.expr_types.get(&(body, expr)))
    }

    fn lower_const_value(
        &mut self,
        builder: &mut Builder,
        const_id: hir::item_tree::ConstId,
    ) -> Value {
        let Some(body_id) = self.hir.const_bodies.get(&const_id).copied() else {
            return builder.unit_const();
        };
        if !self.active_consts.insert(const_id) {
            return builder.unit_const();
        }

        let body = &self.hir.bodies[body_id];
        let old_expr_cache = std::mem::take(&mut self.expr_cache);
        let old_body = self.current_body.replace(body_id);
        let value = self.lower_expr(builder, &[], body, body.root_block);
        self.current_body = old_body;
        self.expr_cache = old_expr_cache;
        self.active_consts.remove(&const_id);
        value
    }

    fn index_len(&self, builder: &mut Builder, base: Value, expr: ExprId) -> Option<Value> {
        let mut ty = self.adjusted_expr_type(expr)?;
        while let type_checker::Type::Ref(inner, _) = ty {
            ty = inner;
        }
        match ty {
            type_checker::Type::Array(_, type_checker::ConstArg::Value(len)) => {
                Some(builder.iconst(*len as u64, IntTy::Usize))
            }
            type_checker::Type::Slice(_) => {
                Some(builder.extract_value(base, 1, Type::Int(IntTy::Usize)))
            }
            _ => None,
        }
    }

    // 类型转换

    fn substitute_tc_type(&self, ty: &type_checker::Type) -> type_checker::Type {
        use type_checker::{ConstArg as TcConstArg, Type as TcType};

        match ty {
            TcType::Param(name) => self
                .generic_tc_subst
                .get(name)
                .map(|ty| self.substitute_tc_type(ty))
                .unwrap_or_else(|| ty.clone()),
            TcType::Ref(inner, mutable) => {
                TcType::Ref(Box::new(self.substitute_tc_type(inner)), *mutable)
            }
            TcType::Ptr { mutable, inner } => TcType::Ptr {
                mutable: *mutable,
                inner: Box::new(self.substitute_tc_type(inner)),
            },
            TcType::Tuple(elements) => TcType::Tuple(
                elements
                    .iter()
                    .map(|element| self.substitute_tc_type(element))
                    .collect(),
            ),
            TcType::Slice(inner) => TcType::Slice(Box::new(self.substitute_tc_type(inner))),
            TcType::Array(inner, len) => {
                let len = match len {
                    TcConstArg::Param(name) => self
                        .generic_tc_subst
                        .get(name)
                        .and_then(|ty| match ty {
                            TcType::Const(value) => Some(value.clone()),
                            _ => None,
                        })
                        .unwrap_or_else(|| len.clone()),
                    _ => len.clone(),
                };
                TcType::Array(Box::new(self.substitute_tc_type(inner)), len)
            }
            TcType::Struct(id, args) => TcType::Struct(
                *id,
                args.iter()
                    .map(|arg| self.substitute_tc_type(arg))
                    .collect(),
            ),
            TcType::Enum(id, args) => TcType::Enum(
                *id,
                args.iter()
                    .map(|arg| self.substitute_tc_type(arg))
                    .collect(),
            ),
            TcType::FunctionItem { function, args } => TcType::FunctionItem {
                function: *function,
                args: args
                    .iter()
                    .map(|arg| self.substitute_tc_type(arg))
                    .collect(),
            },
            TcType::Closure { id, signature } => TcType::Closure {
                id: *id,
                signature: type_checker::CallableSignature {
                    is_unsafe: signature.is_unsafe,
                    kind: signature.kind,
                    params: signature
                        .params
                        .iter()
                        .map(|param| self.substitute_tc_type(param))
                        .collect(),
                    ret: Box::new(self.substitute_tc_type(&signature.ret)),
                },
            },
            TcType::OpaqueCallable { id, signature } => TcType::OpaqueCallable {
                id: *id,
                signature: type_checker::CallableSignature {
                    is_unsafe: signature.is_unsafe,
                    kind: signature.kind,
                    params: signature
                        .params
                        .iter()
                        .map(|param| self.substitute_tc_type(param))
                        .collect(),
                    ret: Box::new(self.substitute_tc_type(&signature.ret)),
                },
            },
            TcType::CallableConstraint(signature) => {
                TcType::CallableConstraint(type_checker::CallableSignature {
                    is_unsafe: signature.is_unsafe,
                    kind: signature.kind,
                    params: signature
                        .params
                        .iter()
                        .map(|param| self.substitute_tc_type(param))
                        .collect(),
                    ret: Box::new(self.substitute_tc_type(&signature.ret)),
                })
            }
            TcType::Const(TcConstArg::Param(name)) => self
                .generic_tc_subst
                .get(name)
                .cloned()
                .unwrap_or_else(|| ty.clone()),
            _ => ty.clone(),
        }
    }

    fn emit_current_drop_scope(&mut self, builder: &mut Builder) {
        if let Some(depth) = self.drop_scopes.len().checked_sub(1) {
            self.emit_drop_scopes_since(builder, depth);
        }
    }

    fn create_drop_slots(
        &self,
        builder: &mut Builder,
        place: Value,
        ty: &type_checker::Type,
        projection: Vec<DropProjection>,
    ) -> Vec<DropSlot> {
        let ty = self.substitute_tc_type(ty);
        if self.type_result.trait_env.type_has_explicit_drop(&ty)
            || matches!(ty, type_checker::Type::Enum(..))
            || matches!(
                ty,
                type_checker::Type::FunctionItem { .. }
                    | type_checker::Type::Closure { .. }
                    | type_checker::Type::OpaqueCallable { .. }
            )
        {
            let flag = builder.alloca(Type::Bool);
            let active = builder.bconst(true);
            builder.store(active, flag);
            return vec![DropSlot {
                place,
                flag,
                ty,
                projection,
            }];
        }

        let mut slots = Vec::new();
        match &ty {
            type_checker::Type::Struct(id, args) => {
                for (index, field_ty) in self.struct_field_types(*id, args).into_iter().enumerate()
                {
                    if self.type_needs_drop(&field_ty, 0) {
                        let field = builder.field_ptr(place, index, self.convert_type(&field_ty));
                        let mut field_projection = projection.clone();
                        field_projection.push(DropProjection::Field(index));
                        slots.extend(self.create_drop_slots(
                            builder,
                            field,
                            &field_ty,
                            field_projection,
                        ));
                    }
                }
            }
            type_checker::Type::Tuple(items) => {
                for (index, item) in items.iter().enumerate() {
                    if self.type_needs_drop(item, 0) {
                        let field = builder.field_ptr(place, index, self.convert_type(item));
                        let mut field_projection = projection.clone();
                        field_projection.push(DropProjection::Field(index));
                        slots.extend(self.create_drop_slots(
                            builder,
                            field,
                            item,
                            field_projection,
                        ));
                    }
                }
            }
            type_checker::Type::Array(item, type_checker::ConstArg::Value(len)) => {
                for index in 0..*len {
                    let index_value = builder.iconst(index as u64, IntTy::Usize);
                    let item_place = builder.index_ptr(place, index_value, self.convert_type(item));
                    let mut item_projection = projection.clone();
                    item_projection.push(DropProjection::Index(index));
                    slots.extend(self.create_drop_slots(
                        builder,
                        item_place,
                        item,
                        item_projection,
                    ));
                }
            }
            _ => {}
        }
        slots
    }

    fn register_drop_slots(&mut self, source: CaptureSource, slots: &[DropSlot]) {
        self.drop_slots.insert(source, slots.to_vec());
    }

    fn emit_drop_scopes_since(&mut self, builder: &mut Builder, depth: usize) {
        let scopes = self.drop_scopes[depth..].to_vec();
        for scope in scopes.into_iter().rev() {
            for slot in scope.into_iter().rev() {
                self.emit_drop_slot(builder, &slot);
            }
        }
    }

    fn emit_drop_slot(&mut self, builder: &mut Builder, slot: &DropSlot) {
        let ty = self.substitute_tc_type(&slot.ty);
        let drop_block = builder.func.new_block_labeled("drop");
        let continue_block = builder.func.new_block_labeled("drop_continue");
        let active = builder.load(slot.flag, Type::Bool);
        builder.set_cond_branch(active, drop_block, continue_block);
        builder.switch_to_block(drop_block);
        self.emit_drop_glue(builder, slot.place, &ty);
        builder.set_branch(continue_block);
        builder.switch_to_block(continue_block);
    }

    fn emit_drop_glue(&mut self, builder: &mut Builder, place: Value, ty: &type_checker::Type) {
        let ty = self.substitute_tc_type(ty);
        if matches!(
            ty,
            type_checker::Type::FunctionItem { .. }
                | type_checker::Type::Closure { .. }
                | type_checker::Type::OpaqueCallable { .. }
        ) {
            let closure = builder.load(place, self.convert_type(&ty));
            let env = builder.extract_value(closure, 1, closure_env_type());
            let drop = builder.extract_value(closure, 2, closure_drop_function_type());
            builder.call_indirect(drop, vec![env], Type::Unit);
            return;
        }
        if let Some(trait_id) = self
            .type_result
            .trait_env
            .lang_items
            .get(type_checker::lang_items::LangItem::Drop)
            && let Some(function) = self.find_trait_impl_method(trait_id, "drop", &ty, None)
        {
            let drop_name = self
                .mono_method_name_for_receiver(function, &ty, None)
                .unwrap_or_else(|| self.function_name(function));
            let mir_ty = self.convert_type(&ty);
            let receiver = builder.unop(UnOp::MutRef, place, Type::Ref(Box::new(mir_ty), true));
            builder.call(FuncRef::Local(drop_name), vec![receiver], Type::Unit);
        }

        let fields = match &ty {
            type_checker::Type::Struct(id, args) => self.struct_field_types(*id, args),
            type_checker::Type::Tuple(items) => items.clone(),
            _ => Vec::new(),
        };
        for (index, field_ty) in fields.into_iter().enumerate() {
            if self.type_needs_drop(&field_ty, 0) {
                let field = builder.field_ptr(place, index, self.convert_type(&field_ty));
                self.emit_drop_glue(builder, field, &field_ty);
            }
        }

        if let type_checker::Type::Array(item, type_checker::ConstArg::Value(len)) = &ty
            && self.type_needs_drop(item, 0)
        {
            for index in 0..*len {
                let index = builder.iconst(index as u64, IntTy::Usize);
                let item_place = builder.index_ptr(place, index, self.convert_type(item));
                self.emit_drop_glue(builder, item_place, item);
            }
        }

        if let type_checker::Type::Enum(id, args) = &ty {
            let variants = self.enum_variant_field_types(*id, args);
            let active = variants
                .iter()
                .enumerate()
                .filter(|(_, fields)| fields.iter().any(|field| self.type_needs_drop(field, 0)))
                .map(|(index, fields)| (index, fields.clone()))
                .collect::<Vec<_>>();
            if !active.is_empty() {
                let tag_place = builder.field_ptr(place, 0, Type::Int(IntTy::U32));
                let tag = builder.load(tag_place, Type::Int(IntTy::U32));
                let done = builder.func.new_block_labeled("enum_drop_done");
                for (position, (variant, fields)) in active.iter().enumerate() {
                    let drop_variant = builder.func.new_block_labeled("enum_drop_variant");
                    let next = if position + 1 == active.len() {
                        done
                    } else {
                        builder.func.new_block_labeled("enum_drop_next")
                    };
                    let expected = builder.iconst(*variant as u64, IntTy::U32);
                    let matches = builder.cmp(CmpOp::Eq, tag, expected);
                    builder.set_cond_branch(matches, drop_variant, next);

                    builder.switch_to_block(drop_variant);
                    let offset = 1 + variants[..*variant].iter().map(Vec::len).sum::<usize>();
                    for (field_index, field_ty) in fields.iter().enumerate() {
                        if self.type_needs_drop(field_ty, 0) {
                            let field = builder.field_ptr(
                                place,
                                offset + field_index,
                                self.convert_type(field_ty),
                            );
                            self.emit_drop_glue(builder, field, field_ty);
                        }
                    }
                    builder.set_branch(done);
                    if next != done {
                        builder.switch_to_block(next);
                    }
                }
                builder.switch_to_block(done);
            }
        }
    }

    fn type_needs_drop(&self, ty: &type_checker::Type, depth: usize) -> bool {
        if depth > 64 {
            return false;
        }
        let ty = self.substitute_tc_type(ty);
        if self.type_result.trait_env.type_has_explicit_drop(&ty)
            || matches!(
                &ty,
                type_checker::Type::FunctionItem { .. }
                    | type_checker::Type::Closure { .. }
                    | type_checker::Type::OpaqueCallable { .. }
            )
        {
            return true;
        }
        match &ty {
            type_checker::Type::Struct(id, args) => self
                .struct_field_types(*id, args)
                .iter()
                .any(|field| self.type_needs_drop(field, depth + 1)),
            type_checker::Type::Enum(id, args) => self
                .enum_variant_field_types(*id, args)
                .iter()
                .flatten()
                .any(|field| self.type_needs_drop(field, depth + 1)),
            type_checker::Type::Tuple(items) => items
                .iter()
                .any(|item| self.type_needs_drop(item, depth + 1)),
            type_checker::Type::Array(item, _) => self.type_needs_drop(item, depth + 1),
            _ => false,
        }
    }

    fn struct_field_types(
        &self,
        id: hir::item_tree::StructId,
        args: &[type_checker::Type],
    ) -> Vec<type_checker::Type> {
        let item = &self.hir.item_tree.structs[id];
        let subst = item
            .generics
            .iter()
            .chain(item.const_generics.iter())
            .zip(args)
            .map(|(name, ty)| (name.0.clone(), ty.clone()))
            .collect();
        item.fields
            .iter()
            .map(|field| self.lower_hir_type_for_pattern(&field.ty, &subst))
            .collect()
    }

    fn enum_variant_field_types(
        &self,
        id: hir::item_tree::EnumId,
        args: &[type_checker::Type],
    ) -> Vec<Vec<type_checker::Type>> {
        let item = &self.hir.item_tree.enums[id];
        let subst = item
            .generics
            .iter()
            .chain(item.const_generics.iter())
            .zip(args)
            .map(|(name, ty)| (name.0.clone(), ty.clone()))
            .collect();
        item.variants
            .iter()
            .map(|variant| match &variant.kind {
                hir::item_tree::HirVariantKind::Unit => Vec::new(),
                hir::item_tree::HirVariantKind::Tuple(fields) => fields
                    .iter()
                    .map(|field| self.lower_hir_type_for_pattern(field, &subst))
                    .collect(),
                hir::item_tree::HirVariantKind::Struct(fields) => fields
                    .iter()
                    .map(|field| self.lower_hir_type_for_pattern(&field.ty, &subst))
                    .collect(),
            })
            .collect()
    }

    fn clear_drop_flags_if_moved(&self, builder: &mut Builder, body: &Body, expr_id: ExprId) {
        let Some(body_id) = self.current_body else {
            return;
        };
        if !self.moved_exprs.contains(&(body_id, expr_id)) {
            return;
        }
        let Some((source, projection)) = self.drop_place_from_expr(body, expr_id) else {
            return;
        };
        let flags = self
            .drop_slots
            .get(&source)
            .into_iter()
            .flatten()
            .filter(|slot| {
                projection.is_empty() || slot.projection.starts_with(projection.as_slice())
            })
            .map(|slot| slot.flag)
            .collect::<HashSet<_>>();
        for flag in flags {
            let inactive = builder.bconst(false);
            builder.store(inactive, flag);
        }
    }

    fn clear_drop_slots_for_source(&self, builder: &mut Builder, source: &CaptureSource) {
        let flags = self
            .drop_slots
            .get(source)
            .into_iter()
            .flatten()
            .map(|slot| slot.flag)
            .collect::<HashSet<_>>();
        for flag in flags {
            let inactive = builder.bconst(false);
            builder.store(inactive, flag);
        }
    }

    fn clear_drop_slots_for_capture(&self, builder: &mut Builder, capture: &CapturePlace) {
        let projection = capture
            .projections
            .iter()
            .filter_map(|projection| match projection {
                Projection::Field(index) => Some(DropProjection::Field(*index)),
                Projection::Index(Some(index)) => Some(DropProjection::Index(*index)),
                Projection::Index(None) => None,
            })
            .collect::<Vec<_>>();
        let flags = self
            .drop_slots
            .get(&capture.source)
            .into_iter()
            .flatten()
            .filter(|slot| {
                slot.projection.starts_with(&projection) || projection.starts_with(&slot.projection)
            })
            .map(|slot| slot.flag)
            .collect::<HashSet<_>>();
        for flag in flags {
            let inactive = builder.bconst(false);
            builder.store(inactive, flag);
        }
    }

    fn drop_place_from_expr(
        &self,
        body: &Body,
        expr_id: ExprId,
    ) -> Option<(CaptureSource, Vec<DropProjection>)> {
        match &body.exprs[expr_id] {
            Expr::Path {
                resolved: Some(ResolvedName::PatternBinding(id)),
                ..
            } => Some((CaptureSource::Pattern(*id), Vec::new())),
            Expr::Path {
                resolved: Some(ResolvedName::Param(index)),
                ..
            } => Some((CaptureSource::Param(*index), Vec::new())),
            Expr::Path {
                resolved: Some(ResolvedName::LambdaParam { lambda, index }),
                ..
            } => Some((
                CaptureSource::LambdaParam {
                    lambda: *lambda,
                    index: *index,
                },
                Vec::new(),
            )),
            Expr::FieldAccess { base, field } => {
                let (source, mut projection) = self.drop_place_from_expr(body, *base)?;
                projection.push(DropProjection::Field(
                    self.resolve_field_index(*base, field),
                ));
                Some((source, projection))
            }
            Expr::IndexAccess { base, index } => {
                let Expr::IntLiteral { value, .. } = body.exprs[*index] else {
                    return None;
                };
                let (source, mut projection) = self.drop_place_from_expr(body, *base)?;
                projection.push(DropProjection::Index(value as usize));
                Some((source, projection))
            }
            _ => None,
        }
    }

    fn clear_dynamic_index_drop_flags_if_moved(
        &self,
        builder: &mut Builder,
        body: &Body,
        expr_id: ExprId,
        base: ExprId,
        index: ExprId,
        index_value: Value,
    ) {
        let Some(body_id) = self.current_body else {
            return;
        };
        if !self.moved_exprs.contains(&(body_id, expr_id))
            || matches!(body.exprs[index], Expr::IntLiteral { .. })
        {
            return;
        }
        let Some((source, projection)) = self.drop_place_from_expr(body, base) else {
            return;
        };
        let mut flags_by_index = BTreeMap::<usize, HashSet<Value>>::new();
        for slot in self.drop_slots.get(&source).into_iter().flatten() {
            if slot.projection.starts_with(projection.as_slice())
                && let Some(DropProjection::Index(item)) = slot.projection.get(projection.len())
            {
                flags_by_index.entry(*item).or_default().insert(slot.flag);
            }
        }
        let index_ty = self
            .type_result
            .expr_types
            .get(&(body_id, index))
            .map(|ty| self.convert_type(ty))
            .and_then(|ty| match ty {
                Type::Int(width) => Some(width),
                _ => None,
            })
            .unwrap_or(IntTy::Usize);
        for (item, flags) in flags_by_index {
            let clear = builder.func.new_block_labeled("move_array_element");
            let next = builder.func.new_block_labeled("move_array_continue");
            let expected = builder.iconst(item as u64, index_ty);
            let matches = builder.cmp(CmpOp::Eq, index_value, expected);
            builder.set_cond_branch(matches, clear, next);
            builder.switch_to_block(clear);
            for flag in flags {
                let inactive = builder.bconst(false);
                builder.store(inactive, flag);
            }
            builder.set_branch(next);
            builder.switch_to_block(next);
        }
    }

    fn clear_indexed_drop_slots(
        &self,
        builder: &mut Builder,
        slots: &[DropSlot],
        index_value: Value,
        index_ty: IntTy,
    ) {
        let mut flags_by_index = BTreeMap::<usize, HashSet<Value>>::new();
        for slot in slots {
            if let Some(index) = slot
                .projection
                .iter()
                .find_map(|projection| match projection {
                    DropProjection::Index(index) => Some(*index),
                    DropProjection::Field(_) => None,
                })
            {
                flags_by_index.entry(index).or_default().insert(slot.flag);
            }
        }
        for (index, flags) in flags_by_index {
            let clear = builder.func.new_block_labeled("move_array_element");
            let next = builder.func.new_block_labeled("move_array_continue");
            let expected = builder.iconst(index as u64, index_ty);
            let matches = builder.cmp(CmpOp::Eq, index_value, expected);
            builder.set_cond_branch(matches, clear, next);
            builder.switch_to_block(clear);
            for flag in flags {
                let inactive = builder.bconst(false);
                builder.store(inactive, flag);
            }
            builder.set_branch(next);
            builder.switch_to_block(next);
        }
    }

    fn convert_type(&self, t: &type_checker::Type) -> Type {
        use type_checker::FloatTy as TcFloat;
        use type_checker::IntTy as TcInt;
        use type_checker::Type as TcType;

        let t = self.substitute_tc_type(t);
        match &t {
            TcType::Int(ity) => Type::Int(match ity {
                TcInt::I8 => IntTy::I8,
                TcInt::I16 => IntTy::I16,
                TcInt::I32 => IntTy::I32,
                TcInt::I64 => IntTy::I64,
                TcInt::Isize => IntTy::Isize,
                TcInt::U8 => IntTy::U8,
                TcInt::U16 => IntTy::U16,
                TcInt::U32 => IntTy::U32,
                TcInt::U64 => IntTy::U64,
                TcInt::Usize => IntTy::Usize,
            }),
            TcType::Float(fty) => Type::Float(match fty {
                TcFloat::F32 => FloatTy::F32,
                TcFloat::F64 => FloatTy::F64,
            }),
            TcType::InferInt => Type::Int(IntTy::I32),
            TcType::InferFloat => Type::Float(FloatTy::F64),
            TcType::Bool => Type::Bool,
            TcType::Str => Type::Str,
            TcType::Char => Type::Char,
            TcType::Unit => Type::Unit,
            TcType::Never => Type::Never,
            TcType::Ref(inner, mutable) => Type::Ref(Box::new(self.convert_type(inner)), *mutable),
            TcType::Ptr { inner, .. } => Type::Ptr(Box::new(self.convert_type(inner))),
            TcType::Tuple(elems) => {
                Type::Tuple(elems.iter().map(|e| self.convert_type(e)).collect())
            }
            TcType::Slice(inner) => Type::Slice(Box::new(self.convert_type(inner))),
            TcType::Array(inner, len) => Type::Array(
                Box::new(self.convert_type(inner)),
                len.as_usize().unwrap_or(0),
            ),
            TcType::Struct(sid, args) => self.convert_struct_type(*sid, args),
            TcType::Enum(eid, args) => self.convert_enum_type(*eid, args),
            TcType::FunctionItem {
                function: fid,
                args,
            } => closure_value_type(self.function_item_signature(*fid, args)),
            TcType::CallableConstraint(signature) => closure_value_type(FnPtrType {
                params: signature
                    .params
                    .iter()
                    .map(|param| self.convert_type(param))
                    .collect(),
                ret: Box::new(self.convert_type(&signature.ret)),
            }),
            TcType::Closure { signature, .. } | TcType::OpaqueCallable { signature, .. } => {
                closure_value_type(FnPtrType {
                    params: signature
                        .params
                        .iter()
                        .map(|param| self.convert_type(param))
                        .collect(),
                    ret: Box::new(self.convert_type(&signature.ret)),
                })
            }
            TcType::InferVar(_) => Type::Unit,
            TcType::Param(name) => self.generic_subst.get(name).cloned().unwrap_or(Type::Unit),
            TcType::Const(_) => Type::Unit,
            TcType::Unknown | TcType::Error => Type::Unit,
        }
    }

    fn function_item_signature(
        &self,
        fid: hir::item_tree::FunctionId,
        args: &[type_checker::Type],
    ) -> FnPtrType {
        let function = &self.hir.item_tree.functions[fid];
        let mut names = Vec::new();
        if let Some(imp) = self.impl_for_method(fid) {
            names.extend(imp.generics.iter().map(|name| name.0.clone()));
        }
        names.extend(function.generics.iter().map(|name| name.0.clone()));
        names.extend(function.implicit_generics.iter().map(|name| name.0.clone()));
        if let Some(imp) = self.impl_for_method(fid) {
            names.extend(imp.const_generics.iter().map(|name| name.0.clone()));
        }
        names.extend(function.const_generics.iter().map(|name| name.0.clone()));

        let mut subst = names
            .into_iter()
            .zip(args.iter().map(|arg| self.substitute_tc_type(arg)))
            .collect::<HashMap<_, _>>();
        if let Some(imp) = self.impl_for_method(fid) {
            let self_ty = self.lower_hir_type_for_pattern(&imp.self_ty, &subst);
            subst.insert("Self".into(), self_ty);
        }

        FnPtrType {
            params: function
                .params
                .iter()
                .map(|param| self.convert_type(&self.lower_hir_type_for_pattern(&param.ty, &subst)))
                .collect(),
            ret: Box::new(
                function
                    .ret_type
                    .as_ref()
                    .map(|ret| self.convert_type(&self.lower_hir_type_for_pattern(ret, &subst)))
                    .unwrap_or(Type::Unit),
            ),
        }
    }

    fn convert_hir_type(&self, t: &hir::item_tree::HirTypeRef) -> Type {
        match t {
            hir::item_tree::HirTypeRef::Never => Type::Never,
            hir::item_tree::HirTypeRef::Named(path) => {
                if let Some(ty) = self.convert_self_associated_type(path) {
                    return ty;
                }
                if is_self_associated_path(path) {
                    return Type::Unit;
                }
                if let Some(name) = path.as_single_name().map(|name| name.0.as_str())
                    && let Some(ty) = self.generic_subst.get(name)
                {
                    return ty.clone();
                }
                match path.segments.last().map(|n| n.0.as_str()) {
                    Some("bool") => Type::Bool,
                    Some("i8") => Type::Int(IntTy::I8),
                    Some("i16") => Type::Int(IntTy::I16),
                    Some("i32") => Type::Int(IntTy::I32),
                    Some("i64") => Type::Int(IntTy::I64),
                    Some("u8") => Type::Int(IntTy::U8),
                    Some("u16") => Type::Int(IntTy::U16),
                    Some("u32") => Type::Int(IntTy::U32),
                    Some("u64") => Type::Int(IntTy::U64),
                    Some("isize") => Type::Int(IntTy::Isize),
                    Some("usize") => Type::Int(IntTy::Usize),
                    Some("f32") => Type::Float(FloatTy::F32),
                    Some("f64") => Type::Float(FloatTy::F64),
                    Some("str") => Type::Str,
                    Some("char") => Type::Char,
                    Some(name) => {
                        if let Some(type_alias) = self.find_associated_type_alias(path)
                            && let Some(ty) = &self.hir.item_tree.type_aliases[type_alias].ty
                        {
                            return self.convert_hir_type(ty);
                        }
                        // Look up user-defined struct by name
                        for (sid, s) in self.hir.item_tree.structs.iter() {
                            if s.name.0 == name {
                                return self
                                    .convert_struct_type_from_hir_args(sid, &path.type_args);
                            }
                        }
                        for (eid, e) in self.hir.item_tree.enums.iter() {
                            if e.name.0 == name {
                                return self.convert_enum_type_from_hir_args(eid, &path.type_args);
                            }
                        }
                        Type::Int(IntTy::I32)
                    }
                    None => Type::Int(IntTy::I32),
                }
            }
            hir::item_tree::HirTypeRef::Ref(inner, mutable) => {
                Type::Ref(Box::new(self.convert_hir_type(inner)), *mutable)
            }
            hir::item_tree::HirTypeRef::Ptr { inner, .. } => {
                Type::Ptr(Box::new(self.convert_hir_type(inner)))
            }
            hir::item_tree::HirTypeRef::Tuple(elems) if elems.is_empty() => Type::Unit,
            hir::item_tree::HirTypeRef::Tuple(elems) => {
                Type::Tuple(elems.iter().map(|e| self.convert_hir_type(e)).collect())
            }
            hir::item_tree::HirTypeRef::Slice(inner) => {
                Type::Slice(Box::new(self.convert_hir_type(inner)))
            }
            hir::item_tree::HirTypeRef::Array(inner, len) => Type::Array(
                Box::new(self.convert_hir_type(inner)),
                self.hir_const_arg_to_usize(len, &HashMap::new()),
            ),
            hir::item_tree::HirTypeRef::Const(_) => Type::Unit,
            hir::item_tree::HirTypeRef::ImplTrait {
                callable, hidden, ..
            } => {
                if let Some(hidden) = hidden
                    && let Some(ty) = self.generic_subst.get(&hidden.0)
                {
                    return ty.clone();
                }
                callable
                    .as_ref()
                    .map(|signature| {
                        closure_value_type(FnPtrType {
                            params: signature
                                .params
                                .iter()
                                .map(|param| self.convert_hir_type(param))
                                .collect(),
                            ret: Box::new(self.convert_hir_type(&signature.ret)),
                        })
                    })
                    .unwrap_or(Type::Unit)
            }
            hir::item_tree::HirTypeRef::Unknown | hir::item_tree::HirTypeRef::Error => Type::Unit,
        }
    }

    fn find_associated_type_alias(
        &self,
        path: &hir::item_tree::HirPath,
    ) -> Option<hir::item_tree::TypeAliasId> {
        if !matches!(path.anchor, hir::item_tree::PathAnchor::Plain) || path.segments.len() != 2 {
            return None;
        }
        let self_ty_name = path.segments[0].0.as_str();
        let alias_name = path.segments[1].0.as_str();

        self.hir.item_tree.impls.iter().find_map(|(_, imp)| {
            let hir::item_tree::HirTypeRef::Named(self_ty_path) = &imp.self_ty else {
                return None;
            };
            if self_ty_path.as_single_name().map(|name| name.0.as_str()) != Some(self_ty_name) {
                return None;
            }
            imp.type_aliases.iter().find_map(|alias_id| {
                (self.hir.item_tree.type_aliases[*alias_id].name.0 == alias_name)
                    .then_some(*alias_id)
            })
        })
    }

    fn convert_self_associated_type(&self, path: &hir::item_tree::HirPath) -> Option<Type> {
        if !is_self_associated_path(path) {
            return None;
        }
        let alias_name = path.segments[1].0.as_str();
        let imp = self.impl_for_method(self.current_function?)?;
        let alias_id = imp
            .type_aliases
            .iter()
            .find(|alias_id| self.hir.item_tree.type_aliases[**alias_id].name.0 == alias_name)?;
        Some(
            self.hir.item_tree.type_aliases[*alias_id]
                .ty
                .as_ref()
                .map(|ty| self.convert_hir_type(ty))
                .unwrap_or(Type::Unit),
        )
    }

    fn convert_struct_type(
        &self,
        sid: hir::item_tree::StructId,
        args: &[type_checker::Type],
    ) -> Type {
        let s = &self.hir.item_tree.structs[sid];
        let type_count = s.generics.len();
        let mir_args = args
            .iter()
            .take(type_count)
            .map(|arg| self.convert_type(arg))
            .collect::<Vec<_>>();
        let const_args = args
            .iter()
            .skip(type_count)
            .filter_map(tc_const_arg_to_usize)
            .collect::<Vec<_>>();
        self.convert_struct_type_from_parts(sid, &mir_args, &const_args)
    }

    fn convert_struct_type_from_hir_args(
        &self,
        sid: hir::item_tree::StructId,
        args: &[hir::item_tree::HirTypeRef],
    ) -> Type {
        let s = &self.hir.item_tree.structs[sid];
        let type_count = s.generics.len();
        let mir_args = args
            .iter()
            .take(type_count)
            .map(|arg| self.convert_hir_type(arg))
            .collect::<Vec<_>>();
        let const_args = args
            .iter()
            .skip(type_count)
            .map(|arg| self.hir_type_ref_const_arg_to_usize(arg, &HashMap::new()))
            .collect::<Vec<_>>();
        self.convert_struct_type_from_parts(sid, &mir_args, &const_args)
    }

    fn convert_struct_type_from_parts(
        &self,
        sid: hir::item_tree::StructId,
        type_args: &[Type],
        const_args: &[usize],
    ) -> Type {
        let s = &self.hir.item_tree.structs[sid];
        let subst = s
            .generics
            .iter()
            .zip(type_args.iter())
            .map(|(name, ty)| (name.0.as_str(), ty))
            .collect::<HashMap<_, _>>();
        let const_subst = s
            .const_generics
            .iter()
            .zip(const_args.iter())
            .map(|(name, value)| (name.0.as_str(), *value))
            .collect::<HashMap<_, _>>();
        let fields = s
            .fields
            .iter()
            .map(|f| {
                (
                    f.name.0.clone(),
                    self.convert_hir_type_with_substs(&f.ty, &subst, &const_subst),
                )
            })
            .collect();
        let name_args = type_args
            .iter()
            .map(mono_type_name)
            .chain(const_args.iter().map(|value| value.to_string()))
            .collect::<Vec<_>>();
        Type::Struct(StructType {
            name: mono_name_from_parts(&s.name.0, &name_args),
            fields,
        })
    }

    fn convert_enum_type(&self, eid: hir::item_tree::EnumId, args: &[type_checker::Type]) -> Type {
        let e = &self.hir.item_tree.enums[eid];
        let type_count = e.generics.len();
        let mir_args = args
            .iter()
            .take(type_count)
            .map(|arg| self.convert_type(arg))
            .collect::<Vec<_>>();
        let const_args = args
            .iter()
            .skip(type_count)
            .filter_map(tc_const_arg_to_usize)
            .collect::<Vec<_>>();
        self.convert_enum_type_from_parts(eid, &mir_args, &const_args)
    }

    fn convert_enum_type_from_hir_args(
        &self,
        eid: hir::item_tree::EnumId,
        args: &[hir::item_tree::HirTypeRef],
    ) -> Type {
        let e = &self.hir.item_tree.enums[eid];
        let type_count = e.generics.len();
        let mir_args = args
            .iter()
            .take(type_count)
            .map(|arg| self.convert_hir_type(arg))
            .collect::<Vec<_>>();
        let const_args = args
            .iter()
            .skip(type_count)
            .map(|arg| self.hir_type_ref_const_arg_to_usize(arg, &HashMap::new()))
            .collect::<Vec<_>>();
        self.convert_enum_type_from_parts(eid, &mir_args, &const_args)
    }

    fn convert_enum_type_from_parts(
        &self,
        eid: hir::item_tree::EnumId,
        type_args: &[Type],
        const_args: &[usize],
    ) -> Type {
        let e = &self.hir.item_tree.enums[eid];
        let subst = e
            .generics
            .iter()
            .zip(type_args.iter())
            .map(|(name, ty)| (name.0.as_str(), ty))
            .collect::<HashMap<_, _>>();
        let const_subst = e
            .const_generics
            .iter()
            .zip(const_args.iter())
            .map(|(name, value)| (name.0.as_str(), *value))
            .collect::<HashMap<_, _>>();
        let mut fields = vec![("tag".to_string(), Type::Int(IntTy::U32))];
        for variant in &e.variants {
            match &variant.kind {
                hir::item_tree::HirVariantKind::Tuple(items) => {
                    for (index, item) in items.iter().enumerate() {
                        fields.push((
                            format!("{}_{}", variant.name.0, index),
                            self.convert_hir_type_with_substs(item, &subst, &const_subst),
                        ));
                    }
                }
                hir::item_tree::HirVariantKind::Struct(items) => {
                    for item in items {
                        fields.push((
                            format!("{}_{}", variant.name.0, item.name.0),
                            self.convert_hir_type_with_substs(&item.ty, &subst, &const_subst),
                        ));
                    }
                }
                hir::item_tree::HirVariantKind::Unit => {}
            }
        }
        let name_args = type_args
            .iter()
            .map(mono_type_name)
            .chain(const_args.iter().map(|value| value.to_string()))
            .collect::<Vec<_>>();
        Type::Struct(StructType {
            name: mono_name_from_parts(&e.name.0, &name_args),
            fields,
        })
    }

    fn mono_method_name(
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

    fn mono_method_name_for_receiver(
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

            let mono_name = format!("{}__{}", self.hir.item_tree.functions[fid].name.0, suffix);
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
        let original_name = self.hir.item_tree.functions[fid].name.0.clone();
        let mono_name = format!("{}__{}", original_name, suffix);
        self.mono_methods.insert(key, mono_name.clone());
        let old_subst = std::mem::replace(&mut self.generic_subst, subst.types);
        let old_tc_subst = std::mem::replace(&mut self.generic_tc_subst, subst.tc_types);
        let old_const_subst = std::mem::replace(&mut self.generic_const_subst, subst.consts);
        let old_expr_cache = std::mem::take(&mut self.expr_cache);
        let old_scope_map = std::mem::take(&mut self.scope_map);
        let old_drop_scopes = std::mem::take(&mut self.drop_scopes);
        let old_drop_slots = std::mem::take(&mut self.drop_slots);
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

    fn mono_function_name(
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

    fn mono_function_name_for_args(
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
        if outer_generics.is_empty()
            && function.generics.is_empty()
            && function.implicit_generics.is_empty()
        {
            return None;
        }
        let tc_args = tc_args
            .iter()
            .map(|arg| self.substitute_tc_type(arg))
            .collect::<Vec<_>>();
        let args = tc_args
            .iter()
            .map(|arg| self.convert_type(arg))
            .collect::<Vec<_>>();
        let mut subst = outer_generics
            .iter()
            .zip(args.iter())
            .map(|(name, ty)| (name.0.clone(), ty.clone()))
            .collect::<HashMap<_, _>>();
        subst.extend(
            function
                .generics
                .iter()
                .chain(function.implicit_generics.iter())
                .zip(args.iter().skip(outer_generics.len()))
                .map(|(name, ty)| (name.0.clone(), ty.clone())),
        );
        let outer_tc_subst = outer_generics
            .iter()
            .zip(tc_args.iter())
            .map(|(name, ty)| (name.0.clone(), ty.clone()))
            .collect::<HashMap<_, _>>();
        let self_tc_ty = imp
            .as_ref()
            .map(|imp| self.lower_hir_type_for_pattern(&imp.self_ty, &outer_tc_subst));
        let self_mir_ty = self_tc_ty.as_ref().map(|ty| self.convert_type(ty));
        if let Some(self_ty) = &self_mir_ty {
            subst.insert("Self".into(), self_ty.clone());
        }
        let suffix = if let Some(self_ty) = &self_mir_ty {
            std::iter::once(mono_type_name(self_ty))
                .chain(args.iter().skip(outer_generics.len()).map(mono_type_name))
                .collect::<Vec<_>>()
                .join("_")
        } else {
            args.iter()
                .map(mono_type_name)
                .collect::<Vec<_>>()
                .join("_")
        };
        let key = (fid, suffix.clone());
        if let Some(name) = self.mono_functions.get(&key) {
            return Some(name.clone());
        }

        let mut tc_subst = outer_tc_subst;
        tc_subst.extend(
            function
                .generics
                .iter()
                .chain(function.implicit_generics.iter())
                .zip(tc_args.iter().skip(outer_generics.len()))
                .map(|(name, ty)| (name.0.clone(), ty.clone())),
        );
        if let Some(self_ty) = self_tc_ty {
            tc_subst.insert("Self".into(), self_ty);
        }
        let mono_name = format!("{}__{}", function.name.0, suffix);
        self.mono_functions.insert(key, mono_name.clone());

        let old_subst = std::mem::replace(&mut self.generic_subst, subst);
        let old_tc_subst = std::mem::replace(&mut self.generic_tc_subst, tc_subst);
        let old_expr_cache = std::mem::take(&mut self.expr_cache);
        let old_scope_map = std::mem::take(&mut self.scope_map);
        let old_drop_scopes = std::mem::take(&mut self.drop_scopes);
        let old_drop_slots = std::mem::take(&mut self.drop_slots);
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
        self.storage_bindings = old_storage_bindings;
        self.parameter_storage = old_parameter_storage;
        self.pattern_bindings = old_pattern_bindings;
        self.capture_access = old_capture_access;
        self.current_lambda = old_current_lambda;
        self.current_body = old_current_body;
        self.generic_subst = old_subst;
        self.generic_tc_subst = old_tc_subst;
        self.module.add_function(func);
        Some(mono_name)
    }

    fn impl_for_method(&self, fid: hir::item_tree::FunctionId) -> Option<&hir::item_tree::HirImpl> {
        self.method_impls
            .get(&fid)
            .map(|impl_id| &self.hir.item_tree.impls[*impl_id])
    }

    fn builtin_operator_for_method(
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

    fn impl_operator_contract(
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

    fn function_name(&self, fid: hir::item_tree::FunctionId) -> String {
        self.static_method_name(fid)
            .unwrap_or_else(|| self.hir.item_tree.functions[fid].name.0.clone())
    }

    fn static_method_name(&self, fid: hir::item_tree::FunctionId) -> Option<String> {
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
        Some(format!(
            "{}__{}",
            self.hir.item_tree.functions[fid].name.0, suffix
        ))
    }

    fn impl_self_mir_type(&self, fid: hir::item_tree::FunctionId) -> Option<Type> {
        self.impl_for_method(fid)
            .map(|imp| self.convert_hir_type(&imp.self_ty))
    }

    fn impl_type_matches(
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

    fn impl_trait_args_match(
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

    fn resolve_trait_ref(
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

    fn impl_mir_subst(
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
            _ => None,
        }
    }

    fn collect_hir_type_subst(
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

    fn convert_hir_type_with_substs(
        &self,
        t: &hir::item_tree::HirTypeRef,
        subst: &HashMap<&str, &Type>,
        const_subst: &HashMap<&str, usize>,
    ) -> Type {
        match t {
            hir::item_tree::HirTypeRef::Never => Type::Never,
            hir::item_tree::HirTypeRef::Named(path) => {
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

    fn convert_struct_type_from_hir_args_with_substs(
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

    fn convert_enum_type_from_hir_args_with_substs(
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

    fn hir_type_ref_const_arg_to_usize(
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

    fn hir_const_arg_to_usize(
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

fn closure_env_type() -> Type {
    Type::Ptr(Box::new(Type::Unit))
}

fn closure_drop_function_type() -> Type {
    Type::FnPtr(FnPtrType {
        params: vec![closure_env_type()],
        ret: Box::new(Type::Unit),
    })
}

fn closure_value_type(signature: FnPtrType) -> Type {
    let mut hasher = DefaultHasher::new();
    signature.hash(&mut hasher);
    let mut call_params = Vec::with_capacity(signature.params.len() + 1);
    call_params.push(closure_env_type());
    call_params.extend(signature.params.clone());
    let call = Type::FnPtr(FnPtrType {
        params: call_params,
        ret: signature.ret,
    });
    Type::Struct(StructType {
        name: format!("riddle_closure_{:016x}", hasher.finish()),
        fields: vec![
            ("call".into(), call),
            ("env".into(), closure_env_type()),
            ("drop".into(), closure_drop_function_type()),
        ],
    })
}

fn closure_call_signature(ty: &Type) -> Option<FnPtrType> {
    let Type::Struct(strukt) = ty else {
        return None;
    };
    match strukt.fields.first().map(|(_, ty)| ty) {
        Some(Type::FnPtr(signature))
            if strukt.fields.get(1).map(|field| &field.1) == Some(&closure_env_type()) =>
        {
            Some(signature.clone())
        }
        _ => None,
    }
}

fn is_self_associated_path(path: &hir::item_tree::HirPath) -> bool {
    matches!(path.anchor, hir::item_tree::PathAnchor::Plain)
        && path.segments.len() == 2
        && path.segments[0].0 == "Self"
}

fn mono_name_from_parts(base: &str, args: &[String]) -> String {
    if args.is_empty() {
        return base.to_string();
    }
    format!("{}_{}", base, args.join("_"))
}

fn tc_const_arg_to_usize(ty: &type_checker::Type) -> Option<usize> {
    match ty {
        type_checker::Type::Const(value) => value.as_usize(),
        _ => None,
    }
}

fn mono_type_name(ty: &Type) -> String {
    match ty {
        Type::Int(i) => format!("{:?}", i).to_ascii_lowercase(),
        Type::Float(f) => format!("{:?}", f).to_ascii_lowercase(),
        Type::Bool => "bool".into(),
        Type::Str => "str".into(),
        Type::Char => "char".into(),
        Type::Unit => "unit".into(),
        Type::Never => "never".into(),
        Type::Ref(inner, mutable) => format!(
            "ref{}_{}",
            if *mutable { "mut" } else { "" },
            mono_type_name(inner)
        ),
        Type::Ptr(inner) => format!("ptr_{}", mono_type_name(inner)),
        Type::Tuple(elems) => format!(
            "tuple_{}",
            elems
                .iter()
                .map(mono_type_name)
                .collect::<Vec<_>>()
                .join("_")
        ),
        Type::Slice(inner) => format!("slice_{}", mono_type_name(inner)),
        Type::Array(inner, len) => format!("arr{len}_{}", mono_type_name(inner)),
        Type::Struct(st) => st.name.clone(),
        Type::Enum(e) => e.name.clone(),
        Type::FnPtr(_) => "fn".into(),
        Type::Void => "void".into(),
    }
}

// 辅助函数

fn primitive_scalar_name(ty: &hir::item_tree::HirTypeRef) -> Option<&str> {
    let hir::item_tree::HirTypeRef::Named(path) = ty else {
        return None;
    };
    path.as_single_name()
        .map(|name| name.0.as_str())
        .filter(|name| {
            matches!(
                *name,
                "bool"
                    | "i8"
                    | "i16"
                    | "i32"
                    | "i64"
                    | "isize"
                    | "u8"
                    | "u16"
                    | "u32"
                    | "u64"
                    | "usize"
                    | "f32"
                    | "f64"
            )
        })
}

fn builtin_operator(lang: &str, method: &str) -> Option<BuiltinOperator> {
    let operator = match (lang, method) {
        ("add", "add") => BuiltinOperator::Binary(BinOp::Add),
        ("sub", "sub") => BuiltinOperator::Binary(BinOp::Sub),
        ("mul", "mul") => BuiltinOperator::Binary(BinOp::Mul),
        ("div", "div") => BuiltinOperator::Binary(BinOp::Div),
        ("rem", "rem") => BuiltinOperator::Binary(BinOp::Mod),
        ("neg", "neg") => BuiltinOperator::Unary(UnOp::Neg),
        ("not", "not") => BuiltinOperator::Unary(UnOp::Not),
        ("bitand", "bitand") => BuiltinOperator::Binary(BinOp::BitAnd),
        ("bitor", "bitor") => BuiltinOperator::Binary(BinOp::BitOr),
        ("bitxor", "bitxor") => BuiltinOperator::Binary(BinOp::BitXor),
        ("shl", "shl") => BuiltinOperator::Binary(BinOp::Shl),
        ("shr", "shr") => BuiltinOperator::Binary(BinOp::Shr),
        ("add_assign", "add_assign") => BuiltinOperator::Assign(BinOp::Add),
        ("sub_assign", "sub_assign") => BuiltinOperator::Assign(BinOp::Sub),
        ("mul_assign", "mul_assign") => BuiltinOperator::Assign(BinOp::Mul),
        ("div_assign", "div_assign") => BuiltinOperator::Assign(BinOp::Div),
        ("rem_assign", "rem_assign") => BuiltinOperator::Assign(BinOp::Mod),
        ("bitand_assign", "bitand_assign") => BuiltinOperator::Assign(BinOp::BitAnd),
        ("bitor_assign", "bitor_assign") => BuiltinOperator::Assign(BinOp::BitOr),
        ("bitxor_assign", "bitxor_assign") => BuiltinOperator::Assign(BinOp::BitXor),
        ("shl_assign", "shl_assign") => BuiltinOperator::Assign(BinOp::Shl),
        ("shr_assign", "shr_assign") => BuiltinOperator::Assign(BinOp::Shr),
        _ => return None,
    };
    Some(operator)
}

fn builtin_operator_supports(op: BuiltinOperator, scalar: &str) -> bool {
    let integer = matches!(
        scalar,
        "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize"
    );
    let float = matches!(scalar, "f32" | "f64");
    let signed = matches!(scalar, "i8" | "i16" | "i32" | "i64" | "isize");
    match op {
        BuiltinOperator::Binary(BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div)
        | BuiltinOperator::Assign(BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div) => {
            integer || float
        }
        BuiltinOperator::Binary(BinOp::Mod) | BuiltinOperator::Assign(BinOp::Mod) => integer,
        BuiltinOperator::Unary(UnOp::Neg) => signed || float,
        BuiltinOperator::Unary(UnOp::Not) => scalar == "bool" || integer,
        BuiltinOperator::Binary(BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor)
        | BuiltinOperator::Assign(BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor) => {
            scalar == "bool" || integer
        }
        BuiltinOperator::Binary(BinOp::Shl | BinOp::Shr)
        | BuiltinOperator::Assign(BinOp::Shl | BinOp::Shr) => integer,
        BuiltinOperator::Unary(UnOp::Ref | UnOp::MutRef | UnOp::Deref) => false,
    }
}

fn operator_params_match(
    function: &hir::item_tree::HirFunction,
    self_ty: &hir::item_tree::HirTypeRef,
    op: BuiltinOperator,
) -> bool {
    match op {
        BuiltinOperator::Binary(_) => {
            function.params.len() == 2
                && type_matches_self(&function.params[0].ty, self_ty)
                && type_matches_self(&function.params[1].ty, self_ty)
        }
        BuiltinOperator::Unary(_) => {
            function.params.len() == 1 && type_matches_self(&function.params[0].ty, self_ty)
        }
        BuiltinOperator::Assign(_) => {
            function.params.len() == 2
                && matches!(
                    &function.params[0].ty,
                    hir::item_tree::HirTypeRef::Ref(inner, true)
                        if type_matches_self(inner, self_ty)
                )
                && type_matches_self(&function.params[1].ty, self_ty)
        }
    }
}

fn type_matches_self(
    ty: &hir::item_tree::HirTypeRef,
    self_ty: &hir::item_tree::HirTypeRef,
) -> bool {
    ty == self_ty
        || matches!(
            ty,
            hir::item_tree::HirTypeRef::Named(path)
                if path.as_single_name().is_some_and(|name| name.0 == "Self")
        )
}

fn trait_operator_contract(
    trait_item: &hir::item_tree::HirTrait,
    method: &str,
    op: BuiltinOperator,
) -> bool {
    let Some(function) = trait_item
        .methods
        .iter()
        .find(|function| function.name.0 == method)
    else {
        return false;
    };
    if !function.generics.is_empty() || !function.const_generics.is_empty() {
        return false;
    }
    let self_ty = hir::item_tree::HirTypeRef::Named(hir::item_tree::HirPath {
        anchor: hir::item_tree::PathAnchor::Plain,
        segments: vec![hir::Name("Self".into())],
        type_args: Vec::new(),
        range: function.name_range,
    });
    if !trait_operator_params_match(trait_item, function, &self_ty, op) {
        return false;
    }
    match op {
        BuiltinOperator::Assign(_) => returns_unit(function),
        BuiltinOperator::Binary(_) | BuiltinOperator::Unary(_) => {
            trait_item
                .type_aliases
                .iter()
                .any(|alias| alias.name.0 == "Output" && alias.ty.is_none())
                && function.ret_type.as_ref().is_some_and(is_self_output)
        }
    }
}

fn trait_operator_params_match(
    trait_item: &hir::item_tree::HirTrait,
    function: &hir::item_tree::HirFunction,
    self_ty: &hir::item_tree::HirTypeRef,
    op: BuiltinOperator,
) -> bool {
    if matches!(op, BuiltinOperator::Unary(_)) {
        return operator_params_match(function, self_ty, op);
    }
    let Some(rhs_name) = trait_item.generics.first() else {
        return operator_params_match(function, self_ty, op);
    };
    if trait_item.generics.len() != 1
        || !trait_item
            .generic_defaults
            .first()
            .and_then(Option::as_ref)
            .is_some_and(|default| type_matches_self(default, self_ty))
    {
        return false;
    }
    let rhs_matches = function.params.get(1).is_some_and(|param| {
        matches!(
            &param.ty,
            hir::item_tree::HirTypeRef::Named(path)
                if path.as_single_name().is_some_and(|name| name == rhs_name)
        )
    });
    match op {
        BuiltinOperator::Binary(_) => {
            function.params.len() == 2
                && type_matches_self(&function.params[0].ty, self_ty)
                && rhs_matches
        }
        BuiltinOperator::Assign(_) => {
            function.params.len() == 2
                && matches!(
                    &function.params[0].ty,
                    hir::item_tree::HirTypeRef::Ref(inner, true)
                        if type_matches_self(inner, self_ty)
                )
                && rhs_matches
        }
        BuiltinOperator::Unary(_) => unreachable!(),
    }
}

fn is_self_output(ty: &hir::item_tree::HirTypeRef) -> bool {
    let hir::item_tree::HirTypeRef::Named(path) = ty else {
        return false;
    };
    is_self_associated_path(path) && path.segments[1].0 == "Output"
}

fn returns_unit(function: &hir::item_tree::HirFunction) -> bool {
    function.ret_type.as_ref().is_none_or(
        |ty| matches!(ty, hir::item_tree::HirTypeRef::Tuple(elements) if elements.is_empty()),
    )
}

fn convert_binop(op: &HirBinOp) -> BinOp {
    match op {
        HirBinOp::Add => BinOp::Add,
        HirBinOp::Sub => BinOp::Sub,
        HirBinOp::Mul => BinOp::Mul,
        HirBinOp::Div => BinOp::Div,
        HirBinOp::Mod => BinOp::Mod,
        HirBinOp::BitAnd => BinOp::BitAnd,
        HirBinOp::BitOr => BinOp::BitOr,
        HirBinOp::BitXor => BinOp::BitXor,
        HirBinOp::Shl => BinOp::Shl,
        HirBinOp::Shr => BinOp::Shr,
        HirBinOp::And => BinOp::BitAnd,
        HirBinOp::Or => BinOp::BitOr,
        // comparison/assign should be handled before reaching here
        HirBinOp::Eq
        | HirBinOp::Neq
        | HirBinOp::Lt
        | HirBinOp::Gt
        | HirBinOp::LtEq
        | HirBinOp::GtEq
        | HirBinOp::Assign
        | HirBinOp::AddAssign
        | HirBinOp::SubAssign
        | HirBinOp::MulAssign
        | HirBinOp::DivAssign
        | HirBinOp::ModAssign
        | HirBinOp::BitAndAssign
        | HirBinOp::BitOrAssign
        | HirBinOp::BitXorAssign
        | HirBinOp::ShlAssign
        | HirBinOp::ShrAssign => unreachable!("cmp/assign handled before convert_binop"),
    }
}

fn convert_cmp_op(op: &HirBinOp) -> CmpOp {
    match op {
        HirBinOp::Eq => CmpOp::Eq,
        HirBinOp::Neq => CmpOp::Neq,
        HirBinOp::Lt => CmpOp::Lt,
        HirBinOp::Gt => CmpOp::Gt,
        HirBinOp::LtEq => CmpOp::LtEq,
        HirBinOp::GtEq => CmpOp::GtEq,
        // Guarded by the caller — these never reach here
        HirBinOp::Assign
        | HirBinOp::Add
        | HirBinOp::Sub
        | HirBinOp::Mul
        | HirBinOp::Div
        | HirBinOp::Mod
        | HirBinOp::BitAnd
        | HirBinOp::BitOr
        | HirBinOp::BitXor
        | HirBinOp::Shl
        | HirBinOp::Shr
        | HirBinOp::And
        | HirBinOp::Or
        | HirBinOp::AddAssign
        | HirBinOp::SubAssign
        | HirBinOp::MulAssign
        | HirBinOp::DivAssign
        | HirBinOp::ModAssign
        | HirBinOp::BitAndAssign
        | HirBinOp::BitOrAssign
        | HirBinOp::BitXorAssign
        | HirBinOp::ShlAssign
        | HirBinOp::ShrAssign => {
            unreachable!("convert_cmp_op called with non-comparison op: {op:?}")
        }
    }
}

fn comparison_trait(op: &HirBinOp) -> Option<(&'static str, &'static str)> {
    Some(match op {
        HirBinOp::Eq => ("partial_eq", "eq"),
        HirBinOp::Neq => ("partial_eq", "ne"),
        HirBinOp::Lt => ("partial_ord", "lt"),
        HirBinOp::Gt => ("partial_ord", "gt"),
        HirBinOp::LtEq => ("partial_ord", "le"),
        HirBinOp::GtEq => ("partial_ord", "ge"),
        _ => return None,
    })
}

fn builtin_comparison_types(
    op: &HirBinOp,
    lhs: &type_checker::Type,
    rhs: &type_checker::Type,
) -> bool {
    let numeric = |ty: &type_checker::Type| {
        matches!(
            ty,
            type_checker::Type::Int(_)
                | type_checker::Type::Float(_)
                | type_checker::Type::InferInt
                | type_checker::Type::InferFloat
        )
    };
    let same_numeric_kind = |lhs: &type_checker::Type, rhs: &type_checker::Type| {
        matches!(
            (lhs, rhs),
            (type_checker::Type::Int(_), type_checker::Type::Int(_))
                | (type_checker::Type::Float(_), type_checker::Type::Float(_))
                | (type_checker::Type::InferInt, type_checker::Type::InferInt)
                | (
                    type_checker::Type::InferFloat,
                    type_checker::Type::InferFloat
                )
        )
    };
    match op {
        HirBinOp::Eq | HirBinOp::Neq => {
            if numeric(lhs) && numeric(rhs) && same_numeric_kind(lhs, rhs) {
                return true;
            }
            matches!(
                (lhs, rhs),
                (type_checker::Type::Bool, type_checker::Type::Bool)
                    | (type_checker::Type::Char, type_checker::Type::Char)
                    | (type_checker::Type::Str, type_checker::Type::Str)
                    | (type_checker::Type::Unit, type_checker::Type::Unit)
            ) || matches!(
                (lhs, rhs),
                (
                    type_checker::Type::Ref(inner_lhs, false),
                    type_checker::Type::Ref(inner_rhs, false)
                ) if matches!(inner_lhs.as_ref(), type_checker::Type::Str)
                    && matches!(inner_rhs.as_ref(), type_checker::Type::Str)
            )
        }
        HirBinOp::Lt | HirBinOp::Gt | HirBinOp::LtEq | HirBinOp::GtEq => {
            (numeric(lhs) && numeric(rhs) && same_numeric_kind(lhs, rhs))
                || matches!(
                    (lhs, rhs),
                    (type_checker::Type::Char, type_checker::Type::Char)
                )
        }
        _ => false,
    }
}

fn convert_unop(op: &HirUnOp) -> UnOp {
    match op {
        HirUnOp::Neg => UnOp::Neg,
        HirUnOp::Not => UnOp::Not,
        HirUnOp::Ref => UnOp::Ref,
        HirUnOp::MutRef => UnOp::MutRef,
        HirUnOp::Deref => UnOp::Deref,
        // Pos is handled as a passthrough before reaching here
        HirUnOp::Pos => unreachable!("Pos should be handled as passthrough"),
    }
}

fn parse_int_suffix(suffix: Option<&str>) -> IntTy {
    match suffix {
        Some("i8") => IntTy::I8,
        Some("i16") => IntTy::I16,
        Some("i32") => IntTy::I32,
        Some("i64") => IntTy::I64,
        Some("isize") => IntTy::Isize,
        Some("u8") => IntTy::U8,
        Some("u16") => IntTy::U16,
        Some("u32") => IntTy::U32,
        Some("u64") => IntTy::U64,
        Some("usize") => IntTy::Usize,
        _ => IntTy::I32, // 默认 i32
    }
}

fn parse_float_suffix(suffix: Option<&str>) -> FloatTy {
    match suffix {
        Some("f32") => FloatTy::F32,
        Some("f64") => FloatTy::F64,
        _ => FloatTy::F64, // 默认 f64
    }
}

/// Extract the function name from a call's callee expression.
fn callee_name(body: &Body, callee: ExprId) -> String {
    match &body.exprs[callee] {
        Expr::Path { path, .. } => {
            // 路径最后一段即为函数名
            path.segments
                .last()
                .map(|s| s.0.clone())
                .unwrap_or_else(|| "unknown".into())
        }
        _ => "unknown".into(),
    }
}

/// Every binding a `let` pattern introduces, paired with its `mut` flag.
fn let_pattern_bindings(body: &Body, pat: PatId) -> Vec<(PatternBindingId, bool)> {
    let mut bindings = Vec::new();
    collect_let_pattern_bindings(body, pat, &mut bindings);
    bindings
}

fn collect_let_pattern_bindings(
    body: &Body,
    pat: PatId,
    bindings: &mut Vec<(PatternBindingId, bool)>,
) {
    match &body.pats[pat] {
        Pattern::Binding { is_mut, .. } => bindings.push((
            PatternBindingId {
                pattern: pat,
                field: None,
            },
            *is_mut,
        )),
        Pattern::Reference { pattern, .. } => {
            collect_let_pattern_bindings(body, *pattern, bindings);
        }
        Pattern::Tuple { elements } | Pattern::TupleStruct { elements, .. } => {
            for element in elements {
                collect_let_pattern_bindings(body, *element, bindings);
            }
        }
        Pattern::Struct { fields, .. } => {
            for (index, field) in fields.iter().enumerate() {
                match field.pat {
                    Some(nested) => collect_let_pattern_bindings(body, nested, bindings),
                    // Shorthand `Point { x, y }` binds immutably.
                    None => bindings.push((
                        PatternBindingId {
                            pattern: pat,
                            field: Some(index),
                        },
                        false,
                    )),
                }
            }
        }
        Pattern::Wildcard | Pattern::Literal(_) | Pattern::Path { .. } => {}
    }
}

/// Resolve a struct field name to its index using type information.
fn resolve_field_index(
    hir: &HirFile,
    type_result: &TypeCheckResult,
    body_id: BodyId,
    base: ExprId,
    field_name: &hir::Name,
) -> usize {
    // Look up the type of the base expression, then find the field index in the struct def.
    let struct_id = type_result
        .expr_types
        .get(&(body_id, base))
        .and_then(|ty| match ty {
            type_checker::Type::Struct(sid, _) => Some(*sid),
            type_checker::Type::Ref(inner, _) => match inner.as_ref() {
                type_checker::Type::Struct(sid, _) => Some(*sid),
                _ => None,
            },
            type_checker::Type::Ptr { inner, .. } => match inner.as_ref() {
                type_checker::Type::Struct(sid, _) => Some(*sid),
                _ => None,
            },
            _ => None,
        });

    if let Some(sid) = struct_id {
        // la_arena uses Index, not .get(); sid should always be valid
        let strukt = &hir.item_tree.structs[sid];
        return strukt
            .fields
            .iter()
            .position(|f| f.name == *field_name)
            .unwrap_or(0);
    }
    0
}

fn determine_cast_op(source: &Type, target: &Type) -> CastOp {
    match (source, target) {
        (Type::Int(_), Type::Int(_)) => CastOp::IntToInt,
        (Type::Int(_), Type::Float(_)) => CastOp::IntToFloat,
        (Type::Float(_), Type::Int(_)) => CastOp::FloatToInt,
        (Type::Float(_), Type::Float(_)) => CastOp::FloatToFloat,
        (Type::Bool, Type::Int(_)) => CastOp::BoolToInt,
        (Type::Char, Type::Int(_)) => CastOp::IntToInt,
        (Type::Int(_), Type::Bool) => CastOp::IntToBool,
        (Type::Int(_), Type::Ptr(_)) => CastOp::IntToPtr,
        (Type::Ptr(_), Type::Ptr(_)) => CastOp::PtrToPtr,
        _ => unreachable!("unsupported cast reached MIR lowering: {source:?} as {target:?}"),
    }
}

fn is_raw_parts_to_slice_cast(source: &Type, target: &Type) -> bool {
    match (source, target) {
        (Type::Tuple(parts), Type::Ref(target, false)) => {
            let Type::Slice(target) = target.as_ref() else {
                return false;
            };
            matches!(
                parts.as_slice(),
                [Type::Ptr(source), Type::Int(IntTy::Usize)] if source.as_ref() == target.as_ref()
            )
        }
        _ => false,
    }
}

/// `&[T] as (*const T, usize)`: decomposing a fat pointer into its parts.
fn is_slice_to_raw_parts_cast(source: &Type, target: &Type) -> bool {
    match (source, target) {
        (Type::Ref(source, _), Type::Tuple(parts)) => {
            let Type::Slice(element) = source.as_ref() else {
                return false;
            };
            matches!(
                parts.as_slice(),
                [Type::Ptr(inner), Type::Int(IntTy::Usize)] if inner.as_ref() == element.as_ref()
            )
        }
        _ => false,
    }
}

fn is_byte_str_layout_cast(source: &Type, target: &Type) -> bool {
    match (source, target) {
        (Type::Ref(source, false), Type::Ref(target, false)) => matches!(
            (source.as_ref(), target.as_ref()),
            (Type::Slice(element), Type::Str)
                | (Type::Str, Type::Slice(element))
                if matches!(element.as_ref(), Type::Int(IntTy::U8))
        ),
        _ => false,
    }
}
