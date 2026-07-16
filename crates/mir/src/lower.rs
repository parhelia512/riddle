use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};

use escape_analysis::EscapeResult;
use hir::{
    HirFile,
    body::{
        BinaryOp as HirBinOp, Body, BodyId, Expr, ExprId, LiteralPattern, MatchArm, PatId, Pattern,
        ResolvedName, Stmt, StmtId, UnaryOp as HirUnOp,
    },
};
use type_checker::{CaptureMode, CaptureSource, LambdaInfo, TypeCheckResult};

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
    let mut ctx = LowerCtx {
        hir,
        type_result,
        analysis: escape_result,
        module: Module::new("main"),
        method_impls,
        expr_cache: HashMap::new(),
        current_body: None,
        current_function: None,
        scope_map: HashMap::new(),
        storage_bindings: HashSet::new(),
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
    };

    // 遍历所有有函数体的函数
    for (fid, func) in hir.item_tree.functions.iter() {
        if ctx.builtin_operator_for_method(fid).is_some()
            || !func.generics.is_empty()
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
    module: Module,
    method_impls: HashMap<hir::item_tree::FunctionId, hir::item_tree::ImplId>,
    expr_cache: HashMap<ExprId, Value>,
    /// The BodyId currently being lowered, used to look up expr_types.
    current_body: Option<BodyId>,
    current_function: Option<hir::item_tree::FunctionId>,
    /// Maps let-bound StmtId → Value for local variable resolution.
    scope_map: HashMap<StmtId, Value>,
    /// StmtIds backed by storage rather than a direct SSA value.
    storage_bindings: HashSet<StmtId>,
    pattern_bindings: Vec<HashMap<String, Value>>,
    generic_subst: HashMap<String, Type>,
    generic_tc_subst: HashMap<String, type_checker::Type>,
    generic_const_subst: HashMap<String, usize>,
    mono_functions: HashMap<(hir::item_tree::FunctionId, String), String>,
    mono_methods: HashMap<(hir::item_tree::FunctionId, String), String>,
    loop_targets: Vec<LoopTargets>,
    lambda_functions: HashMap<(BodyId, ExprId), String>,
    function_adapters: HashMap<hir::item_tree::FunctionId, String>,
    capture_access: HashMap<CaptureSource, CaptureAccess>,
    current_lambda: Option<ExprId>,
    lambda_counter: u32,
}

#[derive(Clone)]
struct CaptureAccess {
    place: Value,
    ty: Type,
}

#[derive(Clone, Copy)]
struct LoopTargets {
    break_block: BlockId,
    continue_block: BlockId,
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
        self.storage_bindings.clear();
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
        if let Some(self_ty) = self.impl_self_mir_type(fid) {
            self.generic_subst.insert("Self".into(), self_ty);
        }

        let func_item = &self.hir.item_tree.functions[fid];

        let ret_type = func_item
            .ret_type
            .as_ref()
            .map(|rt| self.convert_hir_type(rt))
            .unwrap_or(Type::Unit);

        let mut func = Function::new(name.clone(), ret_type);
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
            let root_result = self.lower_expr(&mut builder, &param_values, body, body.root_block);

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
                    (format!("capture_{}_{}", index, capture.name), field_ty)
                })
                .collect(),
        };

        let env_value = if info.captures.is_empty() {
            self.null_env(builder)
        } else {
            // ponytail: closure environments always use the GC heap; add stack
            // promotion only when escape-analysis data shows it matters.
            let env_ty = Type::Struct(env_struct.clone());
            let env_ptr = builder.heap_alloc(env_ty);
            for (index, (capture, capture_ty)) in
                info.captures.iter().zip(&capture_types).enumerate()
            {
                let field_ty = env_struct.fields[index].1.clone();
                let value = match capture.mode {
                    CaptureMode::Shared | CaptureMode::Mutable => {
                        self.capture_place(builder, outer_params, &capture.source, capture_ty)
                    }
                    CaptureMode::Value => {
                        self.capture_value(builder, outer_params, &capture.source, capture_ty)
                    }
                };
                let field = builder.field_ptr(env_ptr, index, field_ty);
                builder.store(value, field);
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
        }

        let call = builder.function_ref(FuncRef::Local(name), Type::FnPtr(call_signature));
        builder.struct_value(vec![call, env_value], ty.clone())
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
        let old_storage_bindings = std::mem::take(&mut self.storage_bindings);
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
                        capture.source.clone(),
                        CaptureAccess {
                            place,
                            ty: capture_ty.clone(),
                        },
                    );
                }
            }
            let result = self.lower_expr(&mut lambda_builder, &param_values, body, lambda_body);
            if lambda_builder.needs_return() {
                lambda_builder.set_return((!is_unit).then_some(result));
            }
        }
        self.module.add_function(function);

        self.expr_cache = old_expr_cache;
        self.scope_map = old_scope_map;
        self.storage_bindings = old_storage_bindings;
        self.pattern_bindings = old_pattern_bindings;
        self.loop_targets = old_loop_targets;
        self.capture_access = old_capture_access;
        self.current_lambda = old_current_lambda;
        self.current_body = old_current_body;
    }

    fn capture_value(
        &mut self,
        builder: &mut Builder,
        params: &[Value],
        source: &CaptureSource,
        ty: &Type,
    ) -> Value {
        if let Some(access) = self.capture_access.get(source).cloned() {
            return builder.load(access.place, access.ty);
        }
        match source {
            CaptureSource::Local(stmt) => {
                let value = self
                    .scope_map
                    .get(stmt)
                    .copied()
                    .unwrap_or_else(|| builder.unit_const());
                if self.storage_bindings.contains(stmt) {
                    builder.load(value, ty.clone())
                } else {
                    value
                }
            }
            CaptureSource::Param(index) => params
                .get(*index)
                .copied()
                .unwrap_or_else(|| builder.unit_const()),
            CaptureSource::LambdaParam { lambda, index }
                if self.current_lambda == Some(*lambda) =>
            {
                params
                    .get(*index)
                    .copied()
                    .unwrap_or_else(|| builder.unit_const())
            }
            CaptureSource::LambdaParam { .. } => builder.unit_const(),
        }
    }

    fn capture_place(
        &mut self,
        builder: &mut Builder,
        params: &[Value],
        source: &CaptureSource,
        ty: &Type,
    ) -> Value {
        if let Some(access) = self.capture_access.get(source) {
            return access.place;
        }
        if let CaptureSource::Local(stmt) = source
            && self.storage_bindings.contains(stmt)
            && let Some(place) = self.scope_map.get(stmt)
        {
            return *place;
        }
        let value = self.capture_value(builder, params, source, ty);
        let place = builder.heap_alloc(ty.clone());
        builder.store(value, place);
        place
    }

    fn null_env(&self, builder: &mut Builder) -> Value {
        let zero = builder.iconst(0, IntTy::Usize);
        builder.cast(CastOp::IntToPtr, zero, closure_env_type())
    }

    fn lower_function_value(
        &mut self,
        builder: &mut Builder,
        fid: hir::item_tree::FunctionId,
        ty: &Type,
    ) -> Value {
        let Some(signature) = closure_call_signature(ty) else {
            return builder.unit_const();
        };
        let adapter = if let Some(name) = self.function_adapters.get(&fid) {
            name.clone()
        } else {
            let target = self.function_name(fid);
            let name = format!("__riddle_fn_adapter_{}", target);
            self.function_adapters.insert(fid, name.clone());

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
        builder.struct_value(vec![call, env], ty.clone())
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

        let value = match expr {
            Expr::Missing => builder.unit_const(),

            Expr::IntLiteral { value, suffix } => {
                // HIR 中 value 已经是 i64，直接使用
                let ty = parse_int_suffix(suffix.as_deref());
                builder.iconst(*value as i128, ty)
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
                Some(ResolvedName::Local(stmt)) => {
                    if let Some(access) = self
                        .capture_access
                        .get(&CaptureSource::Local(*stmt))
                        .cloned()
                    {
                        return builder.load(access.place, access.ty);
                    }
                    let storage = self
                        .scope_map
                        .get(stmt)
                        .copied()
                        .unwrap_or_else(|| builder.unit_const());
                    if self.storage_bindings.contains(stmt) {
                        // mut binding: need to Load from storage to get current value
                        builder.load(storage, mir_type.clone())
                    } else {
                        storage
                    }
                }
                Some(ResolvedName::Param(idx)) => self
                    .capture_access
                    .get(&CaptureSource::Param(*idx))
                    .cloned()
                    .map(|access| builder.load(access.place, access.ty))
                    .unwrap_or_else(|| {
                        param_values
                            .get(*idx)
                            .copied()
                            .unwrap_or_else(|| builder.unit_const())
                    }),
                Some(ResolvedName::LambdaParam { lambda, index }) => {
                    let source = CaptureSource::LambdaParam {
                        lambda: *lambda,
                        index: *index,
                    };
                    if self.current_lambda == Some(*lambda) {
                        param_values
                            .get(*index)
                            .copied()
                            .unwrap_or_else(|| builder.unit_const())
                    } else if let Some(access) = self.capture_access.get(&source).cloned() {
                        builder.load(access.place, access.ty)
                    } else {
                        builder.unit_const()
                    }
                }
                Some(ResolvedName::Function(fid)) => {
                    self.lower_function_value(builder, *fid, &mir_type)
                }
                Some(ResolvedName::EnumVariant(enum_id, idx)) => {
                    self.lower_enum_variant_value(builder, *enum_id, *idx, Vec::new(), mir_type)
                }
                _ => path
                    .as_single_name()
                    .and_then(|name| {
                        self.generic_const_subst
                            .get(&name.0)
                            .map(|value| builder.iconst(*value as i128, IntTy::Usize))
                            .or_else(|| self.pattern_binding(&name.0))
                    })
                    .unwrap_or_else(|| builder.unit_const()),
            },

            Expr::Binary { lhs, rhs, op } => {
                if let Some(call) = self
                    .current_body
                    .and_then(|bid| self.type_result.operator_calls.get(&(bid, expr_id)))
                    .cloned()
                {
                    let lv = self.lower_expr(builder, param_values, body, *lhs);
                    let rv = self.lower_expr(builder, param_values, body, *rhs);
                    return self.lower_operator_call(
                        builder,
                        *lhs,
                        call.function,
                        vec![lv, rv],
                        mir_type,
                    );
                }

                let lv = if op.is_assignment() {
                    self.lower_lvalue(builder, param_values, body, *lhs)
                } else {
                    self.lower_expr(builder, param_values, body, *lhs)
                };
                let rv = self.lower_expr(builder, param_values, body, *rhs);

                match op {
                    HirBinOp::Assign => {
                        // 赋值 = store rv -> lv 的地址
                        builder.store(rv, lv);
                        rv
                    }
                    _ if let Some(base_op) = op.compound_base() => {
                        let value_ty = self
                            .current_body
                            .and_then(|bid| self.type_result.expr_types.get(&(bid, *lhs)))
                            .map(|t| self.convert_type(t))
                            .unwrap_or(mir_type);
                        let current = builder.load(lv, value_ty.clone());
                        let updated =
                            builder.binop(convert_binop(&base_op), current, rv, value_ty.clone());
                        builder.store(updated, lv);
                        updated
                    }
                    HirBinOp::Eq
                    | HirBinOp::Neq
                    | HirBinOp::Lt
                    | HirBinOp::Gt
                    | HirBinOp::LtEq
                    | HirBinOp::GtEq => {
                        let cmp_op = convert_cmp_op(op);
                        builder.cmp(cmp_op, lv, rv)
                    }
                    _ => {
                        let binop = convert_binop(op);
                        builder.binop(binop, lv, rv, mir_type)
                    }
                }
            }

            Expr::Unary { operand, op } => {
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
                // 块：顺序执行语句，尾表达式返回值
                for &stmt in stmts {
                    self.lower_stmt(builder, param_values, body, stmt);
                    if !builder.needs_return() {
                        break;
                    }
                }
                if !builder.needs_return() {
                    builder.unit_const()
                } else {
                    match tail {
                        Some(tail_expr) => self.lower_expr(builder, param_values, body, *tail_expr),
                        None => builder.unit_const(),
                    }
                }
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
                if builder.needs_return() {
                    builder.set_branch(merge_block);
                }

                // else 分支
                builder.switch_to_block(else_block);
                let ev = match else_branch {
                    Some(eb) => self.lower_expr(builder, param_values, body, *eb),
                    None => builder.unit_const(),
                };
                if builder.needs_return() {
                    builder.set_branch(merge_block);
                }

                // merge 块：用 phi 节点合并两条路径的值
                builder.switch_to_block(merge_block);
                let phi = Inst::new(
                    InstKind::Phi(vec![(tv, then_block), (ev, else_block)]),
                    mir_type.clone(),
                );
                builder.func.push_inst(merge_block, phi)
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

            Expr::Call { callee, args } => {
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
                        self.mono_method_name(fid, base)
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
                let bv = self.lower_expr(builder, param_values, body, *base);
                let field_idx = self.resolve_field_index(*base, field);
                builder.extract_value(bv, field_idx, mir_type)
            }

            Expr::IndexAccess { base, index } => {
                let base_val = self.lower_expr(builder, param_values, body, *base);
                let index_val = self.lower_expr(builder, param_values, body, *index);
                let ptr = builder.index_ptr(base_val, index_val, mir_type.clone());
                builder.load(ptr, mir_type)
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

                // auto-unwrap Ref
                let (base_val, base_mir_ty) = if let Type::Ref(inner, _) = &base_mir_ty {
                    (builder.load(base_val, *inner.clone()), *inner.clone())
                } else {
                    (base_val, base_mir_ty)
                };

                let cast_op = determine_cast_op(&base_mir_ty, &mir_type);
                builder.cast(cast_op, base_val, mir_type)
            }
        };

        self.expr_cache.insert(expr_id, value);
        value
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

        let iterable_value = self.lower_expr(builder, param_values, body, iterable);
        if let Some((item_ty, len)) = self.array_iter_info(iterable) {
            let index_ty = Type::Int(IntTy::I32);
            let zero = builder.iconst(0, IntTy::I32);
            let end = builder.iconst(len as i128, IntTy::I32);
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
            let item_ptr = builder.index_ptr(iterable_value, current, item_ty.clone());
            let item = builder.load(item_ptr, item_ty);
            self.push_pattern_binding(body, pat, item);
            self.loop_targets.push(LoopTargets {
                break_block: exit_block,
                continue_block: step_block,
            });
            self.lower_expr(builder, param_values, body, for_body);
            self.loop_targets.pop();
            self.pattern_bindings.pop();
            if builder.needs_return() {
                builder.set_branch(step_block);
            }

            builder.switch_to_block(step_block);
            let one = builder.iconst(1, IntTy::I32);
            let next = builder.binop(BinOp::Add, current, one, index_ty);
            builder.store(next, cursor);
            builder.set_branch(cond_block);

            builder.switch_to_block(exit_block);
            return builder.unit_const();
        }

        if !self.is_std_range_expr(iterable) {
            panic!("missing type-checker metadata for for loop");
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
        self.push_pattern_binding(body, pat, current);
        self.loop_targets.push(LoopTargets {
            break_block: exit_block,
            continue_block: step_block,
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
            )
            .expect("missing IntoIterator impl method for checked for loop");
        let next_fid = self
            .find_trait_impl_method(info.next.trait_id, &info.next.method, &iter_tc_ty)
            .expect("missing Iterator impl method for checked for loop");
        let into_iter_name = self
            .mono_method_name_for_receiver(into_iter_fid, &iterable_ty)
            .unwrap_or_else(|| self.function_name(into_iter_fid));
        let next_name = self
            .mono_method_name_for_receiver(next_fid, &iter_tc_ty)
            .unwrap_or_else(|| self.function_name(next_fid));

        let iter_value = builder.call(
            FuncRef::Local(into_iter_name),
            vec![iterable_value],
            iter_ty.clone(),
        );
        let iter_slot = builder.alloca(iter_ty.clone());
        builder.store(iter_value, iter_slot);

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
        let some_tag = builder.iconst(info.some_variant as i128, IntTy::U32);
        let has_item = builder.cmp(CmpOp::Eq, tag, some_tag);
        builder.set_cond_branch(has_item, body_block, exit_block);

        builder.switch_to_block(body_block);
        let option_id = match next_tc_ty {
            type_checker::Type::Enum(enum_id, _) => enum_id,
            _ => unreachable!("checked Iterator::next result is not an enum"),
        };
        let payload_index =
            1 + self.enum_payload_offset(&self.hir.item_tree.enums[option_id], info.some_variant);
        let item = builder.extract_value(next_value, payload_index, item_ty);
        self.push_pattern_binding(body, pat, item);
        self.loop_targets.push(LoopTargets {
            break_block: exit_block,
            continue_block: cond_block,
        });
        self.lower_expr(builder, param_values, body, for_body);
        self.loop_targets.pop();
        self.pattern_bindings.pop();
        if builder.needs_return() {
            builder.set_branch(cond_block);
        }

        builder.switch_to_block(exit_block);
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
                &scrutinee_ty,
            );

            if let Some(guard) = arm.guard {
                let guarded_body = builder.func.new_block_labeled("match_guarded_arm");
                let guard_value = self.lower_expr(builder, param_values, body, guard);
                builder.set_cond_branch(guard_value, guarded_body, miss_block);
                builder.switch_to_block(guarded_body);
            }

            let arm_value = self.lower_expr(builder, param_values, body, arm.body);
            self.pattern_bindings.pop();
            let arm_exit = builder.current_block;
            if builder.needs_return() {
                builder.set_branch(merge_block);
                phi_args.push((arm_value, arm_exit));
            }
            next_test = miss_block;
        }

        builder.switch_to_block(next_test);
        builder.set_unreachable();
        builder.switch_to_block(merge_block);
        match phi_args.len() {
            0 => builder.unit_const(),
            1 => phi_args[0].0,
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
        let pattern = body.pats[pat].clone();
        match pattern {
            Pattern::Wildcard => None,
            Pattern::Binding { name } => {
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
        let expected = builder.iconst(variant_index as i128, IntTy::U32);
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
                builder.iconst(*value as i128, ty)
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
        value_ty: &type_checker::Type,
    ) {
        let mut scope = HashMap::new();
        self.collect_match_pattern_bindings(builder, body, pat, value, value_ty, &mut scope);
        self.pattern_bindings.push(scope);
    }

    fn collect_match_pattern_bindings(
        &mut self,
        builder: &mut Builder,
        body: &Body,
        pat: PatId,
        value: Value,
        value_ty: &type_checker::Type,
        scope: &mut HashMap<String, Value>,
    ) {
        match body.pats[pat].clone() {
            Pattern::Binding { name } => {
                if !matches!(
                    self.classify_type_pattern(value_ty, Some(&name.0)),
                    TypePattern::EnumVariant { .. }
                ) {
                    scope.insert(name.0, value);
                }
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
                    self.collect_match_pattern_bindings(
                        builder,
                        body,
                        child,
                        child_value,
                        child_ty,
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
                    self.collect_match_pattern_bindings(
                        builder,
                        body,
                        child,
                        child_value,
                        child_ty,
                        scope,
                    );
                }
            }
            Pattern::Struct { path, fields } => {
                if let type_checker::Type::Struct(struct_id, args) = value_ty {
                    let field_types = self.struct_pattern_field_types(*struct_id, args);
                    for field in fields {
                        let Some((index, (_, child_ty))) = field_types
                            .iter()
                            .enumerate()
                            .find(|(_, (name, _))| *name == field.name.0)
                        else {
                            continue;
                        };
                        let child_value =
                            builder.extract_value(value, index, self.convert_type(child_ty));
                        if let Some(child) = field.pat {
                            self.collect_match_pattern_bindings(
                                builder,
                                body,
                                child,
                                child_value,
                                child_ty,
                                scope,
                            );
                        } else {
                            scope.insert(field.name.0, child_value);
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
                for field in fields {
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
                    if let Some(child) = field.pat {
                        self.collect_match_pattern_bindings(
                            builder,
                            body,
                            child,
                            child_value,
                            child_ty,
                            scope,
                        );
                    } else {
                        scope.insert(field.name.0, child_value);
                    }
                }
            }
            Pattern::Wildcard | Pattern::Literal(_) | Pattern::Path { .. } => {}
        }
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
                    "i128" => type_checker::Type::Int(TcIntTy::I128),
                    "isize" => type_checker::Type::Int(TcIntTy::Isize),
                    "u8" => type_checker::Type::Int(TcIntTy::U8),
                    "u16" => type_checker::Type::Int(TcIntTy::U16),
                    "u32" => type_checker::Type::Int(TcIntTy::U32),
                    "u64" => type_checker::Type::Int(TcIntTy::U64),
                    "u128" => type_checker::Type::Int(TcIntTy::U128),
                    "usize" => type_checker::Type::Int(TcIntTy::Usize),
                    "f16" => type_checker::Type::Float(TcFloatTy::F16),
                    "f32" => type_checker::Type::Float(TcFloatTy::F32),
                    "f64" => type_checker::Type::Float(TcFloatTy::F64),
                    "f128" => type_checker::Type::Float(TcFloatTy::F128),
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
            HirTypeRef::Function { params, ret } => type_checker::Type::Fn(
                params
                    .iter()
                    .map(|param| self.lower_hir_type_for_pattern(param, subst))
                    .collect(),
                Box::new(self.lower_hir_type_for_pattern(ret, subst)),
            ),
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
        let tag = builder.iconst(variant_index as i128, IntTy::U32);
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

    fn pattern_binding(&self, name: &str) -> Option<Value> {
        self.pattern_bindings
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    fn push_pattern_binding(&mut self, body: &Body, pat: PatId, value: Value) {
        let mut scope = HashMap::new();
        if let Pattern::Binding { name } = &body.pats[pat] {
            scope.insert(name.0.clone(), value);
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
            .map(|sid| self.hir.item_tree.structs[sid].name.0 == "Range")
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
                type_checker::Type::Function(fid) => Some(*fid),
                _ => None,
            })
    }

    fn lower_operator_call(
        &mut self,
        builder: &mut Builder,
        lhs: ExprId,
        fid: hir::item_tree::FunctionId,
        args: Vec<Value>,
        ret_ty: Type,
    ) -> Value {
        let name = self
            .mono_method_name(fid, lhs)
            .unwrap_or_else(|| self.function_name(fid));
        builder.call(FuncRef::Local(name), args, ret_ty)
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
                .find_trait_impl_method(call.trait_id, &call.method, receiver_ty)
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
        self.find_trait_impl_method(trait_id, &method_name.0, receiver_ty)
            .unwrap_or(fid)
    }

    fn find_trait_impl_method(
        &self,
        trait_id: hir::item_tree::TraitId,
        method_name: &str,
        receiver_ty: &type_checker::Type,
    ) -> Option<hir::item_tree::FunctionId> {
        let receiver_ty = self.substitute_tc_type(receiver_ty);
        self.hir.item_tree.impls.iter().find_map(|(_, candidate)| {
            let candidate_trait = candidate.trait_ty.as_ref()?;
            (self.resolve_trait_ref(candidate_trait) == Some(trait_id)).then_some(())?;
            self.impl_type_matches(candidate, &receiver_ty)
                .then_some(())?;
            candidate.methods.iter().copied().find(|candidate_fid| {
                self.hir.item_tree.functions[*candidate_fid].name.0 == method_name
            })
        })
    }

    fn lower_receiver_arg(
        &mut self,
        builder: &mut Builder,
        param_values: &[Value],
        body: &Body,
        base: ExprId,
        expected: &hir::item_tree::HirTypeRef,
    ) -> Value {
        let base_val = self.lower_expr(builder, param_values, body, base);
        let expected_ty = self.convert_hir_type(expected);
        let base_ty = self
            .current_body
            .and_then(|bid| self.type_result.expr_types.get(&(bid, base)))
            .map(|t| self.convert_type(t))
            .unwrap_or(Type::Unit);

        match &expected_ty {
            Type::Ref(inner, mutable) if inner.as_ref() == &base_ty => {
                let op = if *mutable {
                    HirUnOp::MutRef
                } else {
                    HirUnOp::Ref
                };
                builder.unop(convert_unop(&op), base_val, expected_ty)
            }
            _ => base_val,
        }
    }

    fn resolve_field_index(&self, base: ExprId, field_name: &hir::Name) -> usize {
        let Some(body_id) = self.current_body else {
            return 0;
        };
        resolve_field_index(self.hir, self.type_result, body_id, base, field_name)
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
        let expr = &body.exprs[expr_id];
        match expr {
            Expr::Path { resolved, .. } => match resolved {
                Some(ResolvedName::Local(stmt)) => self
                    .capture_access
                    .get(&CaptureSource::Local(*stmt))
                    .map(|access| access.place)
                    .or_else(|| self.scope_map.get(stmt).copied())
                    .unwrap_or_else(|| builder.unit_const()),
                Some(ResolvedName::Param(idx)) => self
                    .capture_access
                    .get(&CaptureSource::Param(*idx))
                    .map(|access| access.place)
                    .or_else(|| param_values.get(*idx).copied())
                    .unwrap_or_else(|| builder.unit_const()),
                Some(ResolvedName::LambdaParam { lambda, index }) => {
                    let source = CaptureSource::LambdaParam {
                        lambda: *lambda,
                        index: *index,
                    };
                    self.capture_access
                        .get(&source)
                        .map(|access| access.place)
                        .or_else(|| {
                            (self.current_lambda == Some(*lambda))
                                .then(|| param_values.get(*index).copied())
                                .flatten()
                        })
                        .unwrap_or_else(|| builder.unit_const())
                }
                _ => builder.unit_const(),
            },
            Expr::IndexAccess { base, index } => {
                let base_val = self.lower_expr(builder, param_values, body, *base);
                let index_val = self.lower_expr(builder, param_values, body, *index);
                let mir_type = self
                    .current_body
                    .and_then(|bid| self.type_result.expr_types.get(&(bid, expr_id)))
                    .map(|t| self.convert_type(t))
                    .unwrap_or(Type::Unit);
                builder.index_ptr(base_val, index_val, mir_type)
            }
            Expr::FieldAccess { base, field } => {
                let base_val = self.lower_lvalue(builder, param_values, body, *base);
                let field_idx = self.resolve_field_index(*base, field);
                let field_ty = self
                    .current_body
                    .and_then(|bid| self.type_result.expr_types.get(&(bid, expr_id)))
                    .map(|t| self.convert_type(t))
                    .unwrap_or(Type::Unit);
                builder.field_ptr(base_val, field_idx, field_ty)
            }
            _ => self.lower_expr(builder, param_values, body, expr_id),
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
            Stmt::Let {
                init, ty, is_mut, ..
            } => {
                let escapes = self
                    .current_body
                    .map(|bid| self.analysis.escapes(bid, stmt_id))
                    .unwrap_or(false);

                let val = if escapes {
                    // Use init expr's inferred type (from type checker) for allocation,
                    // falling back to HIR type annotation if no init.
                    let alloc_ty = init
                        .and_then(|init_expr| {
                            self.current_body
                                .and_then(|bid| self.type_result.expr_types.get(&(bid, init_expr)))
                        })
                        .map(|t| self.convert_type(t))
                        .unwrap_or_else(|| self.convert_hir_type(ty));
                    let ptr = builder.heap_alloc(alloc_ty);
                    if let Some(init_expr) = init {
                        let init_val = self.lower_expr(builder, param_values, body, *init_expr);
                        builder.store(init_val, ptr);
                    }
                    self.storage_bindings.insert(stmt_id);
                    ptr
                } else if *is_mut {
                    // mut bindings: always use Alloca for reassignable storage
                    let alloc_ty = init
                        .and_then(|init_expr| {
                            self.current_body
                                .and_then(|bid| self.type_result.expr_types.get(&(bid, init_expr)))
                        })
                        .map(|t| self.convert_type(t))
                        .unwrap_or_else(|| self.convert_hir_type(ty));
                    let ptr = builder.alloca(alloc_ty);
                    if let Some(init_expr) = init {
                        let init_val = self.lower_expr(builder, param_values, body, *init_expr);
                        builder.store(init_val, ptr);
                    }
                    self.storage_bindings.insert(stmt_id);
                    ptr
                } else if let Some(init_expr) = init {
                    self.lower_expr(builder, param_values, body, *init_expr)
                } else {
                    builder.unit_const()
                };
                self.scope_map.insert(stmt_id, val);
            }
            Stmt::Expr { expr } => {
                self.lower_expr(builder, param_values, body, *expr);
            }
            Stmt::Return { value } => {
                let rv = value.map(|v| self.lower_expr(builder, param_values, body, v));
                builder.set_return(rv);
            }
            Stmt::Break => {
                let target = self
                    .loop_targets
                    .last()
                    .expect("break statement outside a checked loop");
                builder.set_branch(target.break_block);
            }
            Stmt::Continue => {
                let target = self
                    .loop_targets
                    .last()
                    .expect("continue statement outside a checked loop");
                builder.set_branch(target.continue_block);
            }
            Stmt::Item { .. } => {}
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
            TcType::Const(TcConstArg::Param(name)) => self
                .generic_tc_subst
                .get(name)
                .cloned()
                .unwrap_or_else(|| ty.clone()),
            _ => ty.clone(),
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
                TcInt::I128 => IntTy::I128,
                TcInt::Isize => IntTy::Isize,
                TcInt::U8 => IntTy::U8,
                TcInt::U16 => IntTy::U16,
                TcInt::U32 => IntTy::U32,
                TcInt::U64 => IntTy::U64,
                TcInt::U128 => IntTy::U128,
                TcInt::Usize => IntTy::Usize,
            }),
            TcType::Float(fty) => Type::Float(match fty {
                TcFloat::F16 => FloatTy::F16,
                TcFloat::F32 => FloatTy::F32,
                TcFloat::F64 => FloatTy::F64,
                TcFloat::F128 => FloatTy::F128,
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
            TcType::Array(inner, len) => Type::Array(
                Box::new(self.convert_type(inner)),
                len.as_usize().unwrap_or(0),
            ),
            TcType::Struct(sid, args) => self.convert_struct_type(*sid, args),
            TcType::Enum(eid, args) => self.convert_enum_type(*eid, args),
            TcType::Function(fid) => {
                let f = &self.hir.item_tree.functions[*fid];
                let params = f
                    .params
                    .iter()
                    .map(|p| self.convert_hir_type(&p.ty))
                    .collect();
                let ret = f
                    .ret_type
                    .as_ref()
                    .map(|rt| self.convert_hir_type(rt))
                    .unwrap_or(Type::Unit);
                closure_value_type(FnPtrType {
                    params,
                    ret: Box::new(ret),
                })
            }
            TcType::Fn(params, ret) => closure_value_type(FnPtrType {
                params: params
                    .iter()
                    .map(|param| self.convert_type(param))
                    .collect(),
                ret: Box::new(self.convert_type(ret)),
            }),
            TcType::InferVar(_) => Type::Unit,
            TcType::Param(name) => self.generic_subst.get(name).cloned().unwrap_or(Type::Unit),
            TcType::Const(_) => Type::Unit,
            TcType::Unknown | TcType::Error => Type::Unit,
        }
    }

    fn convert_hir_type(&self, t: &hir::item_tree::HirTypeRef) -> Type {
        match t {
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
                    Some("i128") => Type::Int(IntTy::I128),
                    Some("u8") => Type::Int(IntTy::U8),
                    Some("u16") => Type::Int(IntTy::U16),
                    Some("u32") => Type::Int(IntTy::U32),
                    Some("u64") => Type::Int(IntTy::U64),
                    Some("u128") => Type::Int(IntTy::U128),
                    Some("isize") => Type::Int(IntTy::Isize),
                    Some("usize") => Type::Int(IntTy::Usize),
                    Some("f16") => Type::Float(FloatTy::F16),
                    Some("f32") => Type::Float(FloatTy::F32),
                    Some("f64") => Type::Float(FloatTy::F64),
                    Some("f128") => Type::Float(FloatTy::F128),
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
            hir::item_tree::HirTypeRef::Array(inner, len) => Type::Array(
                Box::new(self.convert_hir_type(inner)),
                self.hir_const_arg_to_usize(len, &HashMap::new()),
            ),
            hir::item_tree::HirTypeRef::Const(_) => Type::Unit,
            hir::item_tree::HirTypeRef::Function { params, ret } => closure_value_type(FnPtrType {
                params: params
                    .iter()
                    .map(|param| self.convert_hir_type(param))
                    .collect(),
                ret: Box::new(self.convert_hir_type(ret)),
            }),
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
    ) -> Option<String> {
        let body_id = self.current_body?;
        let receiver_ty = self.type_result.expr_types.get(&(body_id, base))?;
        self.mono_method_name_for_receiver(fid, receiver_ty)
    }

    fn mono_method_name_for_receiver(
        &mut self,
        fid: hir::item_tree::FunctionId,
        receiver_ty: &type_checker::Type,
    ) -> Option<String> {
        let receiver_ty = self.substitute_tc_type(receiver_ty);
        let imp = self.impl_for_method(fid)?.clone();
        if imp.generics.is_empty() && imp.const_generics.is_empty() {
            return None;
        }
        let receiver_mir_ty = self.convert_type(&receiver_ty);
        let suffix = mono_type_name(&receiver_mir_ty);
        let key = (fid, suffix.clone());
        if let Some(name) = self.mono_methods.get(&key) {
            return Some(name.clone());
        }

        let subst = self.impl_mir_subst(&imp, &receiver_ty)?;
        let original_name = self.hir.item_tree.functions[fid].name.0.clone();
        let mono_name = format!("{}__{}", original_name, suffix);
        self.mono_methods.insert(key, mono_name.clone());
        let old_subst = std::mem::replace(&mut self.generic_subst, subst.types);
        let old_tc_subst = std::mem::replace(&mut self.generic_tc_subst, subst.tc_types);
        let old_const_subst = std::mem::replace(&mut self.generic_const_subst, subst.consts);
        let old_expr_cache = std::mem::take(&mut self.expr_cache);
        let old_scope_map = std::mem::take(&mut self.scope_map);
        let old_storage_bindings = std::mem::take(&mut self.storage_bindings);
        let old_pattern_bindings = std::mem::take(&mut self.pattern_bindings);
        let old_capture_access = std::mem::take(&mut self.capture_access);
        let old_current_lambda = self.current_lambda;
        let old_current_body = self.current_body;
        let body_id = *self.hir.function_bodies.get(&fid)?;
        let func = self.lower_function(fid, mono_name.clone(), body_id);
        self.expr_cache = old_expr_cache;
        self.scope_map = old_scope_map;
        self.storage_bindings = old_storage_bindings;
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
        let function = &self.hir.item_tree.functions[fid];
        if function.generics.is_empty() {
            return None;
        }
        let body_id = self.current_body?;
        let tc_args = self
            .type_result
            .generic_calls
            .get(&(body_id, callee))?
            .args
            .iter()
            .map(|arg| self.substitute_tc_type(arg))
            .collect::<Vec<_>>();
        let args = tc_args
            .iter()
            .map(|arg| self.convert_type(arg))
            .collect::<Vec<_>>();
        let suffix = args
            .iter()
            .map(mono_type_name)
            .collect::<Vec<_>>()
            .join("_");
        let key = (fid, suffix.clone());
        if let Some(name) = self.mono_functions.get(&key) {
            return Some(name.clone());
        }

        let subst = function
            .generics
            .iter()
            .zip(args.iter())
            .map(|(name, ty)| (name.0.clone(), ty.clone()))
            .collect();
        let tc_subst = function
            .generics
            .iter()
            .zip(tc_args)
            .map(|(name, ty)| (name.0.clone(), ty))
            .collect();
        let mono_name = format!("{}__{}", function.name.0, suffix);
        self.mono_functions.insert(key, mono_name.clone());
        let old_subst = std::mem::replace(&mut self.generic_subst, subst);
        let old_tc_subst = std::mem::replace(&mut self.generic_tc_subst, tc_subst);
        let old_expr_cache = std::mem::take(&mut self.expr_cache);
        let old_scope_map = std::mem::take(&mut self.scope_map);
        let old_storage_bindings = std::mem::take(&mut self.storage_bindings);
        let old_pattern_bindings = std::mem::take(&mut self.pattern_bindings);
        let old_capture_access = std::mem::take(&mut self.capture_access);
        let old_current_lambda = self.current_lambda;
        let old_current_body = self.current_body;
        let body_id = *self.hir.function_bodies.get(&fid)?;
        let func = self.lower_function(fid, mono_name.clone(), body_id);
        self.expr_cache = old_expr_cache;
        self.scope_map = old_scope_map;
        self.storage_bindings = old_storage_bindings;
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
        let lang = trait_item.attrs.iter().find_map(|attr| {
            (attr.name.0 == "lang")
                .then_some(attr.value.as_deref())
                .flatten()
        })?;
        let function = &self.hir.item_tree.functions[fid];
        if !function.generics.is_empty() || !function.const_generics.is_empty() {
            return None;
        }
        let method = function.name.0.as_str();
        let op = builtin_operator(lang, method)?;
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
        Some(format!(
            "{}__{}",
            self.hir.item_tree.functions[fid].name.0,
            mono_type_name(&self_ty)
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
            type_checker::Type::Struct(_, args) => {
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
            hir::item_tree::HirTypeRef::Array(inner, len) => Type::Array(
                Box::new(self.convert_hir_type_with_substs(inner, subst, const_subst)),
                self.hir_const_arg_to_usize(len, const_subst),
            ),
            hir::item_tree::HirTypeRef::Const(_) => Type::Unit,
            hir::item_tree::HirTypeRef::Function { params, ret } => closure_value_type(FnPtrType {
                params: params
                    .iter()
                    .map(|param| self.convert_hir_type_with_substs(param, subst, const_subst))
                    .collect(),
                ret: Box::new(self.convert_hir_type_with_substs(ret, subst, const_subst)),
            }),
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
        fields: vec![("call".into(), call), ("env".into(), closure_env_type())],
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
        Type::Ref(inner, _) | Type::Ptr(inner) => format!("ptr_{}", mono_type_name(inner)),
        Type::Tuple(elems) => format!(
            "tuple_{}",
            elems
                .iter()
                .map(mono_type_name)
                .collect::<Vec<_>>()
                .join("_")
        ),
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
                    | "i128"
                    | "isize"
                    | "u8"
                    | "u16"
                    | "u32"
                    | "u64"
                    | "u128"
                    | "usize"
                    | "f16"
                    | "f32"
                    | "f64"
                    | "f128"
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
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
    );
    let float = matches!(scalar, "f16" | "f32" | "f64" | "f128");
    let signed = matches!(scalar, "i8" | "i16" | "i32" | "i64" | "i128" | "isize");
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
    });
    if !operator_params_match(function, &self_ty, op) {
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
        Some("i128") => IntTy::I128,
        Some("isize") => IntTy::Isize,
        Some("u8") => IntTy::U8,
        Some("u16") => IntTy::U16,
        Some("u32") => IntTy::U32,
        Some("u64") => IntTy::U64,
        Some("u128") => IntTy::U128,
        Some("usize") => IntTy::Usize,
        _ => IntTy::I32, // 默认 i32
    }
}

fn parse_float_suffix(suffix: Option<&str>) -> FloatTy {
    match suffix {
        Some("f16") => FloatTy::F16,
        Some("f32") => FloatTy::F32,
        Some("f64") => FloatTy::F64,
        Some("f128") => FloatTy::F128,
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
        (Type::Int(_), Type::Bool) => CastOp::IntToBool,
        (Type::Int(_), Type::Ptr(_)) => CastOp::IntToPtr,
        (Type::Ptr(_), Type::Ptr(_)) => CastOp::PtrToPtr,
        _ => CastOp::IntToInt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_operator_methods_map_to_mir_operations() {
        let cases = [
            ("add", "add", BuiltinOperator::Binary(BinOp::Add)),
            ("sub", "sub", BuiltinOperator::Binary(BinOp::Sub)),
            ("mul", "mul", BuiltinOperator::Binary(BinOp::Mul)),
            ("div", "div", BuiltinOperator::Binary(BinOp::Div)),
            ("rem", "rem", BuiltinOperator::Binary(BinOp::Mod)),
            ("neg", "neg", BuiltinOperator::Unary(UnOp::Neg)),
            ("not", "not", BuiltinOperator::Unary(UnOp::Not)),
            ("bitand", "bitand", BuiltinOperator::Binary(BinOp::BitAnd)),
            ("bitor", "bitor", BuiltinOperator::Binary(BinOp::BitOr)),
            ("bitxor", "bitxor", BuiltinOperator::Binary(BinOp::BitXor)),
            ("shl", "shl", BuiltinOperator::Binary(BinOp::Shl)),
            ("shr", "shr", BuiltinOperator::Binary(BinOp::Shr)),
            (
                "add_assign",
                "add_assign",
                BuiltinOperator::Assign(BinOp::Add),
            ),
            (
                "sub_assign",
                "sub_assign",
                BuiltinOperator::Assign(BinOp::Sub),
            ),
            (
                "mul_assign",
                "mul_assign",
                BuiltinOperator::Assign(BinOp::Mul),
            ),
            (
                "div_assign",
                "div_assign",
                BuiltinOperator::Assign(BinOp::Div),
            ),
            (
                "rem_assign",
                "rem_assign",
                BuiltinOperator::Assign(BinOp::Mod),
            ),
            (
                "bitand_assign",
                "bitand_assign",
                BuiltinOperator::Assign(BinOp::BitAnd),
            ),
            (
                "bitor_assign",
                "bitor_assign",
                BuiltinOperator::Assign(BinOp::BitOr),
            ),
            (
                "bitxor_assign",
                "bitxor_assign",
                BuiltinOperator::Assign(BinOp::BitXor),
            ),
            (
                "shl_assign",
                "shl_assign",
                BuiltinOperator::Assign(BinOp::Shl),
            ),
            (
                "shr_assign",
                "shr_assign",
                BuiltinOperator::Assign(BinOp::Shr),
            ),
        ];

        for (lang, method, expected) in cases {
            assert_eq!(builtin_operator(lang, method), Some(expected));
        }
        assert_eq!(builtin_operator("add", "sub"), None);
        assert!(!builtin_operator_supports(
            BuiltinOperator::Binary(BinOp::Add),
            "bool"
        ));
        assert!(!builtin_operator_supports(
            BuiltinOperator::Binary(BinOp::BitAnd),
            "f32"
        ));
        assert!(builtin_operator_supports(
            BuiltinOperator::Binary(BinOp::BitAnd),
            "bool"
        ));
    }
}
