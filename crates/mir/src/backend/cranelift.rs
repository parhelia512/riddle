use std::collections::HashMap;

use cranelift_codegen::ir::{
    types, AbiParam, Function as ClifFunc, InstBuilder, MemFlagsData, Signature, UserFuncName,
};
use cranelift_codegen::isa::{CallConv, OwnedTargetIsa};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{Linkage, Module as ClifModule, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};

use crate::backend::Backend;
use crate::func::Function;
use crate::instr::*;
use crate::module::Module;
use crate::types::*;
use crate::value::{BlockId, FuncRef, Value};

pub struct CraneliftBackend {
    isa: OwnedTargetIsa,
    // Per-function state — reset for each function
    values: HashMap<u32, cranelift_codegen::ir::Value>,
    blocks: HashMap<BlockId, cranelift_codegen::ir::Block>,
    phi_pred_map: HashMap<(BlockId, BlockId), Vec<(Value, Variable)>>,
    phi_var_for_value: HashMap<u32, Variable>,
    struct_layouts: HashMap<String, StructLayout>,
    next_var: usize,
}

#[derive(Clone)]
#[allow(dead_code)]
struct StructLayout {
    fields: Vec<(String, Type)>,
    offsets: Vec<usize>,
    total_size: usize,
}

impl CraneliftBackend {
    pub fn new() -> Result<Self, String> {
        let mut flag_builder = settings::builder();
        // ponytail: speed level, not size
        flag_builder
            .set("opt_level", "speed")
            .map_err(|e| format!("set opt_level: {}", e))?;

        let builder =
            cranelift_native::builder().map_err(|e| format!("host ISA: {}", e))?;
        let flags = settings::Flags::new(flag_builder);
        let isa = builder
            .finish(flags)
            .map_err(|e| format!("finish ISA: {}", e))?;

        Ok(Self {
            isa,
            values: HashMap::new(),
            blocks: HashMap::new(),
            phi_pred_map: HashMap::new(),
            phi_var_for_value: HashMap::new(),
            struct_layouts: HashMap::new(),
            next_var: 0,
        })
    }
}

impl Backend for CraneliftBackend {
    type Error = String;

    fn compile(&mut self, module: &Module) -> Result<String, Self::Error> {
        let isa = self.isa.clone();

        let obj_builder = ObjectBuilder::new(
            isa,
            module.name.clone(),
            default_libcall_names(),
        )
        .map_err(|e| format!("ObjectBuilder: {}", e))?;

        let mut obj_module = ObjectModule::new(obj_builder);

        // Declare malloc for heap allocations
        let mut malloc_sig = Signature::new(CallConv::SystemV);
        malloc_sig.params.push(AbiParam::new(types::I64));
        malloc_sig.returns.push(AbiParam::new(types::I64));
        let malloc_id = obj_module
            .declare_function("malloc", Linkage::Import, &malloc_sig)
            .map_err(|e| format!("declare malloc: {}", e))?;

        // Pre-compute struct layouts
        self.struct_layouts.clear();
        let all_structs = collect_module_structs(module);
        for st in &all_structs {
            self.struct_layouts
                .entry(st.name.clone())
                .or_insert_with(|| compute_struct_layout(st));
        }

        // Declare all functions
        let mut func_ids: HashMap<String, cranelift_module::FuncId> = HashMap::new();
        for &fid in &module.function_order {
            let func = &module.functions[fid];
            let sig = mir_to_clif_sig(func);
            let id = obj_module
                .declare_function(&func.name, Linkage::Export, &sig)
                .map_err(|e| format!("declare {}: {}", func.name, e))?;
            func_ids.insert(func.name.clone(), id);
        }

        // Build and define each function
        let mut fn_builder_ctx = FunctionBuilderContext::new();
        for &fid in &module.function_order {
            let func = &module.functions[fid];
            let sig = mir_to_clif_sig(func);
            let func_name = UserFuncName::user(0, fid.into_raw().into());

            let mut ctx = Context::new();
            ctx.func = ClifFunc::with_name_signature(func_name, sig);

            {
                let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fn_builder_ctx);
                self.compile_function(func, &mut builder, &func_ids, malloc_id, &mut obj_module)?;
                builder.finalize();
            }

            let clif_func_id = func_ids[&func.name];

            // Verify before defining
            if let Err(e) = ctx.verify(&*self.isa) {
                return Err(format!(
                    "verify {}: {}\nIR:\n{}",
                    func.name,
                    e,
                    ctx.func.display()
                ));
            }

            obj_module
                .define_function(clif_func_id, &mut ctx)
                .map_err(|e| format!("define {}: {}", func.name, e))?;
        }

        // Finish and emit object file
        let product = obj_module.finish();
        let object_data = product.emit().map_err(|e| format!("emit: {}", e))?;

        let obj_path = format!("{}.o", module.name);
        std::fs::write(&obj_path, &object_data[..])
            .map_err(|e| format!("write {}: {}", obj_path, e))?;

        let n_funcs = module.function_order.len();
        let obj_msg = format!("wrote {} ({} functions, {} bytes)", obj_path, n_funcs, object_data.len());

        // Link .o → executable
        let exe_path = if cfg!(windows) {
            format!("{}.exe", module.name)
        } else {
            module.name.clone()
        };

        let link_msg = try_link(&obj_path, &exe_path);
        Ok(format!("{}; {}", obj_msg, link_msg))
    }

    fn name(&self) -> &'static str {
        "cranelift"
    }
}

impl CraneliftBackend {
    fn compile_function(
        &mut self,
        func: &Function,
        builder: &mut FunctionBuilder,
        func_ids: &HashMap<String, cranelift_module::FuncId>,
        malloc_id: cranelift_module::FuncId,
        obj_module: &mut ObjectModule,
    ) -> Result<(), String> {
        self.values.clear();
        self.blocks.clear();
        self.phi_pred_map.clear();
        self.phi_var_for_value.clear();
        self.next_var = 256; // ponytail: start var index above parameter range

        // --- Phase 1: Create CLIF blocks for every MIR block ---
        for (bid, _block) in func.blocks.iter() {
            let clif_block = builder.create_block();
            self.blocks.insert(bid, clif_block);
        }

        // Entry block gets function parameters as block params
        let entry_block = self.blocks[&func.entry];
        for p in &func.params {
            let clif_ty = mir_type_to_clif(&p.ty);
            let param = builder.append_block_param(entry_block, clif_ty);
            self.values.insert(p.value.0, param);
        }

        // --- Phase 2: Pre-scan for phi nodes ---
        for (bid, block) in func.blocks.iter() {
            for (i, inst) in block.insts.iter().enumerate() {
                if let InstKind::Phi(pairs) = &inst.kind {
                    let phi_value = Value(block.start_value + i as u32);
                    // declare_var allocates a new variable with the given type
                    let var_type = mir_type_to_clif(&inst.ty);
                    let var = builder.declare_var(var_type);
                    self.phi_var_for_value.insert(phi_value.0, var);

                    for (pred_val, pred_bid) in pairs {
                        self.phi_pred_map
                            .entry((bid, *pred_bid))
                            .or_default()
                            .push((*pred_val, var));
                    }
                }
            }
        }

        // --- Phase 3: Emit instructions in each block ---
        for (bid, block) in func.blocks.iter() {
            let clif_block = self.blocks[&bid];
            builder.switch_to_block(clif_block);

            for (i, inst) in block.insts.iter().enumerate() {
                let v = Value(block.start_value + i as u32);
                self.emit_inst(builder, inst, v, func_ids, malloc_id, obj_module)?;
            }

            self.emit_terminator(builder, &block.terminator, bid, func)?;
            builder.seal_block(clif_block);
        }

        Ok(())
    }

    fn emit_inst(
        &mut self,
        builder: &mut FunctionBuilder,
        inst: &Inst,
        v: Value,
        func_ids: &HashMap<String, cranelift_module::FuncId>,
        malloc_id: cranelift_module::FuncId,
        obj_module: &mut ObjectModule,
    ) -> Result<(), String> {
        match &inst.kind {
            InstKind::Const(c) => self.emit_const(builder, c, &inst.ty, v),
            InstKind::BinOp(op, lhs, rhs) => self.emit_binop(builder, *op, *lhs, *rhs, &inst.ty, v),
            InstKind::UnOp(op, operand) => self.emit_unop(builder, *op, *operand, &inst.ty, v),
            InstKind::Cmp(op, lhs, rhs) => self.emit_cmp(builder, *op, *lhs, *rhs, v),
            InstKind::Cast(op, val, target_ty) => {
                self.emit_cast(builder, *op, *val, target_ty, v)
            }
            InstKind::Alloca(inner_ty) => self.emit_alloca(builder, inner_ty, v),
            InstKind::HeapAlloc(inner_ty) => {
                self.emit_heap_alloc(builder, inner_ty, v, malloc_id, obj_module)
            }
            InstKind::Load(ptr) => self.emit_load(builder, *ptr, &inst.ty, v),
            InstKind::Store(val, ptr) => self.emit_store(builder, *val, *ptr),
            InstKind::FieldPtr(base, index) => {
                self.emit_field_ptr(builder, *base, *index, &inst.ty, v)
            }
            InstKind::IndexPtr(base, index) => {
                self.emit_index_ptr(builder, *base, *index, &inst.ty, v)
            }
            InstKind::ExtractValue(aggregate, index) => {
                self.emit_extract_value(builder, *aggregate, *index, &inst.ty, v)
            }
            InstKind::Call(callee, args) => {
                self.emit_call(builder, callee, args, &inst.ty, v, func_ids, obj_module)?
            }
            InstKind::StructValue(fields) => {
                self.emit_struct_value(builder, fields, &inst.ty, v)
            }
            InstKind::ArrayValue(elements) => {
                self.emit_array_value(builder, elements, &inst.ty, v)
            }
            InstKind::TupleValue(elements) => {
                self.emit_tuple_value(builder, elements, &inst.ty, v)
            }
            InstKind::Phi(_pairs) => {
                if let Some(var) = self.phi_var_for_value.get(&v.0).copied() {
                    let clif_val = builder.use_var(var);
                    self.values.insert(v.0, clif_val);
                }
            }
        }
        Ok(())
    }

    fn emit_const(&mut self, builder: &mut FunctionBuilder, c: &ConstValue, ty: &Type, v: Value) {
        let clif_val = match c {
            ConstValue::Int(val, _width) => {
                let clif_ty = mir_type_to_clif(ty);
                builder.ins().iconst(clif_ty, *val as i64)
            }
            ConstValue::Float(val, _width) => builder.ins().f64const(*val),
            ConstValue::Bool(b) => {
                builder
                    .ins()
                    .iconst(types::I8, if *b { 1i64 } else { 0i64 })
            }
            ConstValue::Char(ch) => builder.ins().iconst(types::I32, *ch as i64),
            ConstValue::String(_s) => {
                // ponytail: string constants not yet supported
                builder.ins().iconst(types::I64, 0)
            }
            ConstValue::Unit => builder.ins().iconst(types::I8, 0),
        };
        self.values.insert(v.0, clif_val);
    }

    fn emit_binop(
        &mut self,
        builder: &mut FunctionBuilder,
        op: BinOp,
        lhs: Value,
        rhs: Value,
        ty: &Type,
        v: Value,
    ) {
        let l = self.values[&lhs.0];
        let r = self.values[&rhs.0];

        let clif_val = match ty {
            Type::Float(_) => match op {
                BinOp::Add => builder.ins().fadd(l, r),
                BinOp::Sub => builder.ins().fsub(l, r),
                BinOp::Mul => builder.ins().fmul(l, r),
                BinOp::Div => builder.ins().fdiv(l, r),
                BinOp::Mod => builder.ins().iconst(types::I8, 0), // ponytail: no fmod
                _ => builder.ins().iconst(types::I8, 0),
            },
            _ => match op {
                BinOp::Add => builder.ins().iadd(l, r),
                BinOp::Sub => builder.ins().isub(l, r),
                BinOp::Mul => builder.ins().imul(l, r),
                BinOp::Div => builder.ins().udiv(l, r),
                BinOp::Mod => builder.ins().urem(l, r),
                BinOp::BitAnd => builder.ins().band(l, r),
                BinOp::BitOr => builder.ins().bor(l, r),
                BinOp::BitXor => builder.ins().bxor(l, r),
                BinOp::Shl => builder.ins().ishl(l, r),
                BinOp::Shr => builder.ins().ushr(l, r),
            },
        };
        self.values.insert(v.0, clif_val);
    }

    fn emit_unop(
        &mut self,
        builder: &mut FunctionBuilder,
        op: UnOp,
        operand: Value,
        ty: &Type,
        v: Value,
    ) {
        let val = self.values[&operand.0];

        let clif_val = match op {
            UnOp::Neg => match ty {
                Type::Float(_) => builder.ins().fneg(val),
                _ => builder.ins().ineg(val),
            },
            UnOp::Not => {
                let one = builder.ins().iconst(types::I8, 1);
                builder.ins().bxor(val, one)
            }
            UnOp::Ref => {
                // ponytail: MIR Ref already lowers to a pointer value
                val
            }
            UnOp::Deref => {
                let inner_ty = mir_type_to_clif(ty);
                builder.ins().load(inner_ty, MemFlagsData::new(), val, 0)
            }
        };
        self.values.insert(v.0, clif_val);
    }

    fn emit_cmp(
        &mut self,
        builder: &mut FunctionBuilder,
        op: CmpOp,
        lhs: Value,
        rhs: Value,
        v: Value,
    ) {
        use cranelift_codegen::ir::condcodes::IntCC;
        let l = self.values[&lhs.0];
        let r = self.values[&rhs.0];

        let cc = match op {
            CmpOp::Eq => IntCC::Equal,
            CmpOp::Neq => IntCC::NotEqual,
            CmpOp::Lt => IntCC::SignedLessThan,
            CmpOp::Gt => IntCC::SignedGreaterThan,
            CmpOp::LtEq => IntCC::SignedLessThanOrEqual,
            CmpOp::GtEq => IntCC::SignedGreaterThanOrEqual,
        };

        // icmp returns I8 (0 or 1) directly in Cranelift 0.133
        let result = builder.ins().icmp(cc, l, r);
        self.values.insert(v.0, result);
    }

    fn emit_cast(
        &mut self,
        builder: &mut FunctionBuilder,
        op: CastOp,
        val: Value,
        target_ty: &Type,
        v: Value,
    ) {
        let src = self.values[&val.0];
        let dst_ty = mir_type_to_clif(target_ty);

        let clif_val = match op {
            CastOp::IntToInt => {
                let src_ty = builder.func.dfg.value_type(src);
                if src_ty.bits() < dst_ty.bits() {
                    builder.ins().uextend(dst_ty, src)
                } else if src_ty.bits() > dst_ty.bits() {
                    builder.ins().ireduce(dst_ty, src)
                } else {
                    src
                }
            }
            CastOp::IntToFloat => builder.ins().fcvt_from_uint(dst_ty, src),
            CastOp::FloatToInt => builder.ins().fcvt_to_uint(dst_ty, src),
            CastOp::FloatToFloat => {
                let src_ty = builder.func.dfg.value_type(src);
                if src_ty.bits() < dst_ty.bits() {
                    builder.ins().fpromote(dst_ty, src)
                } else {
                    builder.ins().fdemote(dst_ty, src)
                }
            }
            CastOp::BoolToInt => builder.ins().uextend(dst_ty, src),
            CastOp::IntToBool => {
                let src_ty = builder.func.dfg.value_type(src);
                let zero = builder.ins().iconst(src_ty, 0);
                builder
                    .ins()
                    .icmp(cranelift_codegen::ir::condcodes::IntCC::NotEqual, src, zero)
                // icmp already returns I8, which is our bool type
            }
        };
        self.values.insert(v.0, clif_val);
    }

    fn emit_alloca(&mut self, builder: &mut FunctionBuilder, inner_ty: &Type, v: Value) {
        let size = type_size_bytes(inner_ty);
        let align = size.min(8).max(1) as u8;
        let slot = builder.create_sized_stack_slot(
            cranelift_codegen::ir::StackSlotData::new(
                cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                size as u32,
                align,
            ),
        );
        let ptr = builder.ins().stack_addr(types::I64, slot, 0);
        self.values.insert(v.0, ptr);
    }

    fn emit_heap_alloc(
        &mut self,
        builder: &mut FunctionBuilder,
        inner_ty: &Type,
        v: Value,
        malloc_id: cranelift_module::FuncId,
        obj_module: &mut ObjectModule,
    ) {
        let size = type_size_bytes(inner_ty);
        let size_val = builder.ins().iconst(types::I64, size as i64);

        let malloc_ref = obj_module.declare_func_in_func(malloc_id, &mut builder.func);
        let call_inst = builder.ins().call(malloc_ref, &[size_val]);
        let ptr = builder.func.dfg.first_result(call_inst);
        self.values.insert(v.0, ptr);
    }

    fn emit_load(
        &mut self,
        builder: &mut FunctionBuilder,
        ptr: Value,
        ty: &Type,
        v: Value,
    ) {
        let ptr_val = self.values[&ptr.0];
        let clif_ty = mir_type_to_clif(ty);
        let loaded = builder.ins().load(clif_ty, MemFlagsData::new(), ptr_val, 0);
        self.values.insert(v.0, loaded);
    }

    fn emit_store(&mut self, builder: &mut FunctionBuilder, val: Value, ptr: Value) {
        let val_clif = self.values[&val.0];
        let ptr_clif = self.values[&ptr.0];
        builder.ins().store(MemFlagsData::new(), val_clif, ptr_clif, 0);
    }

    fn emit_field_ptr(
        &mut self,
        builder: &mut FunctionBuilder,
        base: Value,
        index: usize,
        ty: &Type,
        v: Value,
    ) {
        let base_val = self.values[&base.0];
        let offset = self.field_offset(ty, index);
        let offset_val = builder.ins().iconst(types::I64, offset as i64);
        let ptr = builder.ins().iadd(base_val, offset_val);
        self.values.insert(v.0, ptr);
    }

    fn emit_index_ptr(
        &mut self,
        builder: &mut FunctionBuilder,
        base: Value,
        index: Value,
        ty: &Type,
        v: Value,
    ) {
        let base_val = self.values[&base.0];
        let index_val = self.values[&index.0];
        let elem_size = match ty {
            Type::Ptr(inner) | Type::Ref(inner) => type_size_bytes(inner),
            _ => 8,
        };
        let size_val = builder.ins().iconst(types::I64, elem_size as i64);
        let scaled = builder.ins().imul(index_val, size_val);
        let ptr = builder.ins().iadd(base_val, scaled);
        self.values.insert(v.0, ptr);
    }

    fn emit_extract_value(
        &mut self,
        builder: &mut FunctionBuilder,
        aggregate: Value,
        index: usize,
        field_ty: &Type,
        v: Value,
    ) {
        let agg_val = self.values[&aggregate.0];
        let offset = self.field_offset_from_aggregate(aggregate, index);
        let offset_val = builder.ins().iconst(types::I64, offset as i64);
        let field_ptr = builder.ins().iadd(agg_val, offset_val);
        let clif_ty = mir_type_to_clif(field_ty);
        let loaded = builder
            .ins()
            .load(clif_ty, MemFlagsData::new(), field_ptr, 0);
        self.values.insert(v.0, loaded);
    }

    fn emit_call(
        &mut self,
        builder: &mut FunctionBuilder,
        callee: &FuncRef,
        args: &[Value],
        _ret_ty: &Type,
        v: Value,
        func_ids: &HashMap<String, cranelift_module::FuncId>,
        obj_module: &mut ObjectModule,
    ) -> Result<(), String> {
        let name = match callee {
            FuncRef::Local(n) => n,
            FuncRef::Intrinsic(n) => n,
        };

        let clif_func_id = match func_ids.get(name.as_str()) {
            Some(id) => *id,
            None => {
                return Err(format!("undefined function: {}", name));
            }
        };

        let func_ref = obj_module.declare_func_in_func(clif_func_id, &mut builder.func);
        let clif_args: Vec<cranelift_codegen::ir::Value> =
            args.iter().map(|a| self.values[&a.0]).collect();

        let call_inst = builder.ins().call(func_ref, &clif_args);
        let results = builder.func.dfg.inst_results(call_inst);
        if !results.is_empty() {
            self.values.insert(v.0, results[0]);
        }
        Ok(())
    }

    fn emit_struct_value(
        &mut self,
        builder: &mut FunctionBuilder,
        fields: &[Value],
        ty: &Type,
        v: Value,
    ) {
        let total_size = type_size_bytes(ty);
        let align = total_size.min(8).max(1) as u8;
        let slot = builder.create_sized_stack_slot(
            cranelift_codegen::ir::StackSlotData::new(
                cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                total_size as u32,
                align,
            ),
        );
        let base_ptr = builder.ins().stack_addr(types::I64, slot, 0);

        let layout = self.struct_layout_for_type(ty);
        for (i, field_val) in fields.iter().enumerate() {
            let offset = layout
                .as_ref()
                .map(|l| l.offsets.get(i).copied().unwrap_or(i * 8))
                .unwrap_or(i * 8) as i64;
            let field_clif = self.values[&field_val.0];
            if offset == 0 {
                builder
                    .ins()
                    .store(MemFlagsData::new(), field_clif, base_ptr, 0);
            } else {
                let offset_val = builder.ins().iconst(types::I64, offset);
                let field_ptr = builder.ins().iadd(base_ptr, offset_val);
                builder
                    .ins()
                    .store(MemFlagsData::new(), field_clif, field_ptr, 0);
            }
        }

        self.values.insert(v.0, base_ptr);
    }

    fn emit_array_value(
        &mut self,
        builder: &mut FunctionBuilder,
        elements: &[Value],
        ty: &Type,
        v: Value,
    ) {
        let elem_size = match ty {
            Type::Array(inner) => type_size_bytes(inner),
            _ => 8,
        };
        let total_size = elem_size * elements.len();
        let align = elem_size.min(8).max(1) as u8;
        let slot = builder.create_sized_stack_slot(
            cranelift_codegen::ir::StackSlotData::new(
                cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                total_size as u32,
                align,
            ),
        );
        let base_ptr = builder.ins().stack_addr(types::I64, slot, 0);

        for (i, elem_val) in elements.iter().enumerate() {
            let elem_clif = self.values[&elem_val.0];
            if i == 0 {
                builder
                    .ins()
                    .store(MemFlagsData::new(), elem_clif, base_ptr, 0);
            } else {
                let offset_val = builder
                    .ins()
                    .iconst(types::I64, (i * elem_size) as i64);
                let elem_ptr = builder.ins().iadd(base_ptr, offset_val);
                builder
                    .ins()
                    .store(MemFlagsData::new(), elem_clif, elem_ptr, 0);
            }
        }

        self.values.insert(v.0, base_ptr);
    }

    fn emit_tuple_value(
        &mut self,
        builder: &mut FunctionBuilder,
        elements: &[Value],
        ty: &Type,
        v: Value,
    ) {
        // ponytail: tuples handled like arrays
        self.emit_array_value(builder, elements, ty, v)
    }

    fn emit_terminator(
        &mut self,
        builder: &mut FunctionBuilder,
        term: &Terminator,
        current_bid: BlockId,
        _func: &Function,
    ) -> Result<(), String> {
        match term {
            Terminator::Branch(target) => {
                self.def_phi_vars(builder, current_bid, *target);
                let target_block = self.blocks[target];
                builder.ins().jump(target_block, &[]);
            }
            Terminator::CondBranch(cond, then_block, else_block) => {
                self.def_phi_vars(builder, current_bid, *then_block);
                self.def_phi_vars(builder, current_bid, *else_block);

                let cond_val = self.values[&cond.0];
                let then_clif = self.blocks[then_block];
                let else_clif = self.blocks[else_block];
                // brif: branch if cond is non-zero
                builder
                    .ins()
                    .brif(cond_val, then_clif, &[], else_clif, &[]);
            }
            Terminator::Return(val) => {
                let inst_results: Vec<cranelift_codegen::ir::Value> = match val {
                    Some(v) => vec![self.values[&v.0]],
                    None => vec![],
                };
                builder.ins().return_(&inst_results);
            }
        }
        Ok(())
    }

    fn def_phi_vars(
        &mut self,
        builder: &mut FunctionBuilder,
        current_bid: BlockId,
        target: BlockId,
    ) {
        if let Some(phi_vals) = self.phi_pred_map.get(&(target, current_bid)) {
            for (val, var) in phi_vals {
                let clif_val = self.values[&val.0];
                builder.def_var(*var, clif_val);
            }
        }
    }

    fn field_offset(&self, ptr_ty: &Type, index: usize) -> usize {
        match ptr_ty {
            Type::Ptr(inner) | Type::Ref(inner) => match inner.as_ref() {
                Type::Struct(st) => self
                    .struct_layouts
                    .get(&st.name)
                    .and_then(|l| l.offsets.get(index).copied())
                    .unwrap_or(index * 8),
                _ => index * 8,
            },
            _ => index * 8,
        }
    }

    fn field_offset_from_aggregate(&self, _aggregate: Value, index: usize) -> usize {
        // ponytail: assume 8-byte aligned fields
        index * 8
    }

    fn struct_layout_for_type(&self, ty: &Type) -> Option<&StructLayout> {
        match ty {
            Type::Struct(st) => self.struct_layouts.get(&st.name),
            _ => None,
        }
    }
}

// ---- Type mapping ----

fn mir_type_to_clif(ty: &Type) -> types::Type {
    match ty {
        Type::Int(i) => match i {
            IntTy::I8 | IntTy::U8 => types::I8,
            IntTy::I16 | IntTy::U16 => types::I16,
            IntTy::I32 | IntTy::U32 => types::I32,
            IntTy::I64 | IntTy::U64 => types::I64,
            IntTy::I128 | IntTy::U128 => types::I128,
            IntTy::Isize | IntTy::Usize => types::I64,
        },
        Type::Float(f) => match f {
            FloatTy::F16 => types::I16,
            FloatTy::F32 => types::F32,
            FloatTy::F64 => types::F64,
            FloatTy::F128 => types::I128,
        },
        Type::Bool => types::I8,
        Type::Char => types::I32,
        Type::Str => types::I64,
        Type::Ptr(_) | Type::Ref(_) | Type::FnPtr(_) => types::I64,
        Type::Array(_) | Type::Struct(_) | Type::Tuple(_) | Type::Enum(_) => types::I64,
        Type::Unit | Type::Never | Type::Void => types::I8,
    }
}

fn type_size_bytes(ty: &Type) -> usize {
    match ty {
        Type::Int(i) => match i {
            IntTy::I8 | IntTy::U8 => 1,
            IntTy::I16 | IntTy::U16 => 2,
            IntTy::I32 | IntTy::U32 => 4,
            IntTy::I64 | IntTy::U64 => 8,
            IntTy::I128 | IntTy::U128 => 16,
            IntTy::Isize | IntTy::Usize => 8,
        },
        Type::Float(f) => match f {
            FloatTy::F16 => 2,
            FloatTy::F32 => 4,
            FloatTy::F64 => 8,
            FloatTy::F128 => 16,
        },
        Type::Bool => 1,
        Type::Char => 4,
        Type::Ref(_) | Type::Ptr(_) | Type::FnPtr(_) => 8,
        Type::Str => 16,
        Type::Unit => 0,
        Type::Never => 0,
        _ => 8,
    }
}

fn mir_to_clif_sig(func: &Function) -> Signature {
    let mut sig = Signature::new(CallConv::SystemV);

    for p in &func.params {
        let clif_ty = mir_type_to_clif(&p.ty);
        sig.params.push(AbiParam::new(clif_ty));
    }

    if !matches!(func.ret_type, Type::Unit | Type::Never | Type::Void) {
        let ret_ty = mir_type_to_clif(&func.ret_type);
        sig.returns.push(AbiParam::new(ret_ty));
    }

    sig
}

// ---- Struct layout computation ----

fn compute_struct_layout(st: &StructType) -> StructLayout {
    let mut offset = 0usize;
    let mut offsets = Vec::new();

    for (_name, ty) in &st.fields {
        let size = type_size_bytes(ty);
        let align = size.min(8).max(1);
        offset = (offset + align - 1) & !(align - 1);
        offsets.push(offset);
        offset += size;
    }

    let total_size = if offsets.is_empty() {
        0
    } else {
        let last_field = st.fields.last().unwrap();
        *offsets.last().unwrap() + type_size_bytes(&last_field.1)
    };

    StructLayout {
        fields: st.fields.clone(),
        offsets,
        total_size,
    }
}

fn collect_module_structs(module: &Module) -> Vec<StructType> {
    let mut seen: std::collections::BTreeMap<String, StructType> = std::collections::BTreeMap::new();

    for &fid in &module.function_order {
        let func = &module.functions[fid];
        for (_bid, block) in func.blocks.iter() {
            for inst in &block.insts {
                collect_types(&inst.ty, &mut seen);
            }
        }
        for p in &func.params {
            collect_types(&p.ty, &mut seen);
        }
        collect_types(&func.ret_type, &mut seen);
    }

    seen.into_values().collect()
}

fn collect_types(ty: &Type, seen: &mut std::collections::BTreeMap<String, StructType>) {
    match ty {
        Type::Struct(st) => {
            if seen.contains_key(&st.name) {
                return;
            }
            seen.insert(st.name.clone(), st.clone());
            for (_name, field_ty) in &st.fields {
                collect_types(field_ty, seen);
            }
        }
        Type::Ptr(inner) | Type::Ref(inner) => collect_types(inner, seen),
        Type::Array(inner) => collect_types(inner, seen),
        Type::Tuple(types) => {
            for t in types {
                collect_types(t, seen);
            }
        }
        _ => {}
    }
}

// ---- Link .o → executable ----

/// Try to link the object file into an executable using the system C compiler.
/// Falls back gracefully if no compiler is available.
fn try_link(obj_path: &str, exe_path: &str) -> String {
    // ponytail: try gcc first, then cc, then clang
    let linkers = ["gcc", "cc", "clang"];

    for linker in &linkers {
        let mut cmd = std::process::Command::new(linker);
        cmd.args(["-o", exe_path, obj_path]);
        // -mconsole: use console subsystem on Windows (expects main, not WinMain)
        // -no-pie: avoid position-independent executable issues with static linking
        if cfg!(windows) {
            cmd.arg("-mconsole");
        }

        match cmd.output() {
            Ok(output) if output.status.success() => {
                return format!("linked {}", exe_path);
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if linker == linkers.last().unwrap() {
                    return format!("link failed: {}", stderr.trim());
                }
            }
            Err(_) => {
                continue;
            }
        }
    }

    "link skipped (no linker found)".into()
}
