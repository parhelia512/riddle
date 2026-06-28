use std::collections::{HashMap, HashSet};

use escape_analysis::EscapeResult;
use hir::{
    HirFile,
    body::{
        BinaryOp as HirBinOp, Body, BodyId, Expr, ExprId, ResolvedName, Stmt, StmtId,
        UnaryOp as HirUnOp,
    },
};
use type_checker::TypeCheckResult;

use crate::builder::Builder;
use crate::func::Function;
use crate::instr::*;
use crate::module::Module;
use crate::types::*;
use crate::value::{BlockId, FuncRef, Value};

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
    let mut ctx = LowerCtx {
        hir,
        type_result,
        analysis: escape_result,
        module: Module::new("main"),
        expr_cache: HashMap::new(),
        current_body: None,
        scope_map: HashMap::new(),
        mut_bindings: HashSet::new(),
    };

    // 遍历所有有函数体的函数
    for (fid, func) in hir.item_tree.functions.iter() {
        if let Some(body_id) = hir.function_bodies.get(&fid).copied() {
            let mir_func = ctx.lower_function(func.name.0.clone(), body_id);
            ctx.module.add_function(mir_func);
        }
    }

    // 注册 extern 函数声明
    let extern_funcs: Vec<_> = hir
        .item_tree
        .extern_function_ids
        .iter()
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
    expr_cache: HashMap<ExprId, Value>,
    /// The BodyId currently being lowered, used to look up expr_types.
    current_body: Option<BodyId>,
    /// Maps let-bound StmtId → Value for local variable resolution.
    scope_map: HashMap<StmtId, Value>,
    /// StmtIds of `mut` bindings — their Value is a storage location (Alloca),
    /// so Path resolution must emit a Load to read the current value.
    mut_bindings: HashSet<StmtId>,
}

impl<'a> LowerCtx<'a> {
    fn lower_function(&mut self, name: String, body_id: BodyId) -> Function {
        let body = &self.hir.bodies[body_id];
        self.expr_cache.clear();
        self.scope_map.clear();
        self.mut_bindings.clear();
        self.current_body = Some(body_id);

        // 查找函数签名（一次扫描）
        let func_item = self
            .hir
            .item_tree
            .functions
            .iter()
            .find(|(_, f)| f.name.0 == name)
            .map(|(_, f)| f);

        let ret_type = func_item
            .and_then(|f| f.ret_type.as_ref())
            .map(|rt| self.convert_hir_type(rt))
            .unwrap_or(Type::Unit);

        let mut func = Function::new(name.clone(), ret_type);
        let mut param_values: Vec<Value> = Vec::new();

        if let Some(fi) = func_item {
            for param in &fi.params {
                let pty = self.convert_hir_type(&param.ty);
                let v = func.add_param(param.name.0.clone(), pty);
                param_values.push(v);
            }
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

            // 设置返回 — only if lower_expr didn't already set one via Stmt::Return
            // ponytail: check if terminator is still the default (Return(None)).
            // If body has an explicit return, it was already set correctly.
            if is_unit_ret {
                builder.set_return(None);
            } else if builder.needs_return() {
                builder.set_return(Some(root_result));
            }
        }

        func
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

            Expr::Path { resolved, .. } => match resolved {
                Some(ResolvedName::Local(stmt)) => {
                    let storage = self.scope_map.get(stmt).copied().unwrap_or_else(|| builder.unit_const());
                    if self.mut_bindings.contains(stmt) {
                        // mut binding: need to Load from storage to get current value
                        builder.load(storage, mir_type.clone())
                    } else {
                        storage
                    }
                }
                Some(ResolvedName::Param(idx)) => {
                    param_values.get(*idx).copied().unwrap_or_else(|| builder.unit_const())
                }
                Some(ResolvedName::Function(_)) => builder.unit_const(),
                Some(ResolvedName::EnumVariant(_, idx)) => {
                    builder.iconst(*idx as i128, IntTy::U32)
                }
                _ => builder.unit_const(),
            },

            Expr::Binary { lhs, rhs, op } => {
                let lv = if matches!(op, HirBinOp::Assign) {
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
                let ov = self.lower_expr(builder, param_values, body, *operand);
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
                }
                match tail {
                    Some(tail_expr) => self.lower_expr(builder, param_values, body,*tail_expr),
                    None => builder.unit_const(),
                }
            }

            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let cv = self.lower_expr(builder, param_values, body,*cond);
                let then_block = builder.func.new_block_labeled("then");
                let else_block = builder.func.new_block_labeled("else");
                let merge_block = builder.func.new_block_labeled("merge");

                builder.set_cond_branch(cv, then_block, else_block);

                // then 分支
                builder.switch_to_block(then_block);
                let tv = self.lower_expr(builder, param_values, body,*then_branch);
                builder.set_branch(merge_block);

                // else 分支
                builder.switch_to_block(else_block);
                let ev = match else_branch {
                    Some(eb) => self.lower_expr(builder, param_values, body,*eb),
                    None => builder.unit_const(),
                };
                builder.set_branch(merge_block);

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
                let cv = self.lower_expr(builder, param_values, body,*condition);
                builder.set_cond_branch(cv, body_block, exit_block);

                // 循环体：执行后跳回条件块
                builder.switch_to_block(body_block);
                self.lower_expr(builder, param_values, body,*while_body);
                builder.set_branch(cond_block);

                // 出口块
                builder.switch_to_block(exit_block);
                builder.unit_const()
            }

            Expr::Match { scrutinee, arms } => {
                let _sv = self.lower_expr(builder, param_values, body,*scrutinee);
                let merge_block = builder.func.new_block_labeled("match_merge");
                let mut phi_args: Vec<(Value, BlockId)> = Vec::new();

                // 简化处理：每个 arm 生成一个独立块，最后用 phi 合并
                for arm in arms {
                    self.lower_expr(builder, param_values, body,arm.body);
                    let current = builder.current_block;
                    builder.set_branch(merge_block);
                    // 实际需要跟踪 arm body 的值
                    phi_args.push((builder.unit_const(), current));
                }

                builder.switch_to_block(merge_block);
                if phi_args.len() == 1 {
                    phi_args[0].0
                } else {
                    let phi = Inst::new(InstKind::Phi(phi_args), mir_type);
                    builder.func.push_inst(merge_block, phi)
                }
            }

            Expr::Array { elements } => {
                let vals: Vec<Value> = elements
                    .iter()
                    .map(|e| self.lower_expr(builder, param_values, body,*e))
                    .collect();
                builder.array_value(vals, mir_type)
            }

            Expr::Struct { fields, .. } => {
                let vals: Vec<Value> = fields
                    .iter()
                    .map(|f| self.lower_expr(builder, param_values, body,f.value))
                    .collect();
                builder.struct_value(vals, mir_type)
            }

            Expr::Call { callee, args } => {
                // 从 callee 路径提取函数名
                let name = callee_name(body, *callee);
                let arg_vals: Vec<Value> = args
                    .iter()
                    .map(|a| self.lower_expr(builder, param_values, body,*a))
                    .collect();
                // 检查是否是 extern 函数调用
                let is_extern = match &body.exprs[*callee] {
                    Expr::Path { resolved, .. } => resolved
                        .as_ref()
                        .and_then(|r| match r {
                            ResolvedName::Function(fid) => Some(*fid),
                            _ => None,
                        })
                        .map(|fid| self.hir.item_tree.extern_function_ids.contains(&fid))
                        .unwrap_or(false),
                    _ => false,
                };
                let func_ref = if is_extern {
                    FuncRef::Extern(name)
                } else {
                    FuncRef::Local(name)
                };
                builder.call(func_ref, arg_vals, mir_type)
            }

            Expr::FieldAccess { base, field } => {
                let bv = self.lower_expr(builder, param_values, body,*base);
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
                Some(ResolvedName::Local(stmt)) => {
                    *self.scope_map.get(stmt).unwrap_or(&builder.unit_const())
                }
                Some(ResolvedName::Param(idx)) => {
                    param_values.get(*idx).copied().unwrap_or_else(|| builder.unit_const())
                }
                _ => builder.unit_const(),
            },
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
            Stmt::Let { init, ty, is_mut, .. } => {
                let escapes = self
                    .current_body
                    .map(|bid| self.analysis.escapes(bid, stmt_id))
                    .unwrap_or(false);

                let val = if escapes {
                    // Use init expr's inferred type (from type checker) for allocation,
                    // falling back to HIR type annotation if no init.
                    let alloc_ty = init
                        .and_then(|init_expr| {
                            self.current_body.and_then(|bid| {
                                self.type_result.expr_types.get(&(bid, init_expr))
                            })
                        })
                        .map(|t| self.convert_type(t))
                        .unwrap_or_else(|| self.convert_hir_type(ty));
                    let ptr = builder.heap_alloc(alloc_ty);
                    if let Some(init_expr) = init {
                        let init_val = self.lower_expr(builder, param_values, body, *init_expr);
                        builder.store(init_val, ptr);
                    }
                    ptr
                } else if *is_mut {
                    // mut bindings: always use Alloca for reassignable storage
                    let alloc_ty = init
                        .and_then(|init_expr| {
                            self.current_body.and_then(|bid| {
                                self.type_result.expr_types.get(&(bid, init_expr))
                            })
                        })
                        .map(|t| self.convert_type(t))
                        .unwrap_or_else(|| self.convert_hir_type(ty));
                    let ptr = builder.alloca(alloc_ty);
                    if let Some(init_expr) = init {
                        let init_val = self.lower_expr(builder, param_values, body, *init_expr);
                        builder.store(init_val, ptr);
                    }
                    self.mut_bindings.insert(stmt_id);
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
            Stmt::Item { .. } => {}
        }
    }

    // 类型转换

    fn convert_type(&self, t: &type_checker::Type) -> Type {
        use type_checker::FloatTy as TcFloat;
        use type_checker::IntTy as TcInt;
        use type_checker::Type as TcType;

        match t {
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
            TcType::Array(inner) => Type::Array(Box::new(self.convert_type(inner))),
            TcType::Struct(sid) => {
                let s = &self.hir.item_tree.structs[*sid];
                let fields = s
                    .fields
                    .iter()
                    .map(|f| (f.name.0.clone(), self.convert_hir_type(&f.ty)))
                    .collect();
                Type::Struct(StructType {
                    name: s.name.0.clone(),
                    fields,
                })
            }
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
                Type::FnPtr(FnPtrType {
                    params,
                    ret: Box::new(ret),
                })
            }
            TcType::Enum(_) => Type::Int(IntTy::U32),
            TcType::Unknown | TcType::Error => Type::Unit,
        }
    }

    fn convert_hir_type(&self, t: &hir::item_tree::HirTypeRef) -> Type {
        match t {
            hir::item_tree::HirTypeRef::Named(path) => {
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
                        // Look up user-defined struct by name
                        for (_sid, s) in self.hir.item_tree.structs.iter() {
                            if s.name.0 == name {
                                let fields = s
                                    .fields
                                    .iter()
                                    .map(|f| (f.name.0.clone(), self.convert_hir_type(&f.ty)))
                                    .collect();
                                return Type::Struct(StructType {
                                    name: s.name.0.clone(),
                                    fields,
                                });
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
            hir::item_tree::HirTypeRef::Tuple(elems) => {
                Type::Tuple(elems.iter().map(|e| self.convert_hir_type(e)).collect())
            }
            hir::item_tree::HirTypeRef::Array(inner) => {
                Type::Array(Box::new(self.convert_hir_type(inner)))
            }
            hir::item_tree::HirTypeRef::Unknown | hir::item_tree::HirTypeRef::Error => Type::Unit,
        }
    }
}

// 辅助函数

fn convert_binop(op: &HirBinOp) -> BinOp {
    match op {
        HirBinOp::Add => BinOp::Add,
        HirBinOp::Sub => BinOp::Sub,
        HirBinOp::Mul => BinOp::Mul,
        HirBinOp::Div => BinOp::Div,
        HirBinOp::Mod => BinOp::Mod,
        HirBinOp::And => BinOp::BitAnd,
        HirBinOp::Or => BinOp::BitOr,
        // comparison/assign should be handled before reaching here
        HirBinOp::Eq
        | HirBinOp::Neq
        | HirBinOp::Lt
        | HirBinOp::Gt
        | HirBinOp::LtEq
        | HirBinOp::GtEq
        | HirBinOp::Assign => unreachable!("cmp/assign handled before convert_binop"),
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
        | HirBinOp::And
        | HirBinOp::Or => unreachable!("convert_cmp_op called with non-comparison op: {op:?}"),
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
            type_checker::Type::Struct(sid) => Some(*sid),
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
        _ => CastOp::IntToInt,
    }
}
