use crate::func::Function;
use crate::instr::*;
use crate::types::*;
use crate::value::{BlockId, FuncRef, Value};

/// Convenience API for constructing MIR functions.
///
/// Wraps a `Function` and tracks the "current block" so instructions can be
/// emitted sequentially without repeating the block id.
pub struct Builder<'f> {
    pub func: &'f mut Function,
    pub current_block: BlockId,
}

impl<'f> Builder<'f> {
    pub fn new(func: &'f mut Function) -> Self {
        let entry = func.entry;
        Self {
            func,
            current_block: entry,
        }
    }

    pub fn switch_to_block(&mut self, block: BlockId) {
        self.current_block = block;
    }

    // 常量

    pub fn iconst(&mut self, value: i128, ty: IntTy) -> Value {
        let width = int_to_width(ty);
        self.emit(
            InstKind::Const(ConstValue::Int(value, width)),
            Type::Int(ty),
        )
    }

    pub fn fconst(&mut self, value: f64, ty: FloatTy) -> Value {
        let width = float_to_width(ty);
        self.emit(
            InstKind::Const(ConstValue::Float(value, width)),
            Type::Float(ty),
        )
    }

    pub fn bconst(&mut self, value: bool) -> Value {
        self.emit(InstKind::Const(ConstValue::Bool(value)), Type::Bool)
    }

    pub fn sconst(&mut self, value: String) -> Value {
        self.emit(InstKind::Const(ConstValue::String(value)), Type::Str)
    }

    pub fn char_const(&mut self, value: char) -> Value {
        self.emit(InstKind::Const(ConstValue::Char(value)), Type::Char)
    }

    pub fn unit_const(&mut self) -> Value {
        self.emit(InstKind::Const(ConstValue::Unit), Type::Unit)
    }

    // 算术 / 逻辑

    pub fn binop(&mut self, op: BinOp, lhs: Value, rhs: Value, ty: Type) -> Value {
        self.emit(InstKind::BinOp(op, lhs, rhs), ty)
    }

    pub fn unop(&mut self, op: UnOp, operand: Value, ty: Type) -> Value {
        self.emit(InstKind::UnOp(op, operand), ty)
    }

    pub fn cmp(&mut self, op: CmpOp, lhs: Value, rhs: Value) -> Value {
        self.emit(InstKind::Cmp(op, lhs, rhs), Type::Bool)
    }

    pub fn cast(&mut self, op: CastOp, value: Value, ty: Type) -> Value {
        self.emit(InstKind::Cast(op, value, ty.clone()), ty)
    }

    // 内存

    pub fn alloca(&mut self, ty: Type) -> Value {
        let ptr_ty = Type::Ptr(Box::new(ty));
        self.emit(InstKind::Alloca(ptr_ty.clone()), ptr_ty)
    }

    /// Allocate on the GC-managed heap. Returns a `Ptr<type>`.
    /// Used for values that escape analysis determines may escape the stack frame.
    pub fn heap_alloc(&mut self, ty: Type) -> Value {
        let ptr_ty = Type::Ptr(Box::new(ty));
        self.emit(InstKind::HeapAlloc(ptr_ty.clone()), ptr_ty)
    }

    pub fn load(&mut self, ptr: Value, ty: Type) -> Value {
        self.emit(InstKind::Load(ptr), ty)
    }

    pub fn store(&mut self, value: Value, ptr: Value) {
        self.emit_void(InstKind::Store(value, ptr));
    }

    pub fn field_ptr(&mut self, base: Value, field_index: usize, field_ty: Type) -> Value {
        let ptr_ty = Type::Ptr(Box::new(field_ty));
        self.emit(InstKind::FieldPtr(base, field_index), ptr_ty)
    }

    pub fn index_ptr(&mut self, base: Value, index: Value, elem_ty: Type) -> Value {
        let ptr_ty = Type::Ptr(Box::new(elem_ty));
        self.emit(InstKind::IndexPtr(base, index), ptr_ty)
    }

    // 聚合

    pub fn extract_value(&mut self, aggregate: Value, index: usize, ty: Type) -> Value {
        self.emit(InstKind::ExtractValue(aggregate, index), ty)
    }

    pub fn struct_value(&mut self, fields: Vec<Value>, ty: Type) -> Value {
        self.emit(InstKind::StructValue(fields), ty)
    }

    pub fn sparse_struct_value(&mut self, fields: Vec<(usize, Value)>, ty: Type) -> Value {
        self.emit(InstKind::SparseStructValue(fields), ty)
    }

    pub fn array_value(&mut self, elements: Vec<Value>, ty: Type) -> Value {
        self.emit(InstKind::ArrayValue(elements), ty)
    }

    pub fn tuple_value(&mut self, elements: Vec<Value>, ty: Type) -> Value {
        self.emit(InstKind::TupleValue(elements), ty)
    }

    // 调用

    pub fn call(&mut self, callee: FuncRef, args: Vec<Value>, ret_ty: Type) -> Value {
        self.emit(InstKind::Call(callee, args), ret_ty)
    }

    // 终止指令

    pub fn set_branch(&mut self, target: BlockId) {
        self.func
            .set_terminator(self.current_block, Terminator::Branch(target));
    }

    pub fn set_cond_branch(&mut self, cond: Value, then_block: BlockId, else_block: BlockId) {
        self.func.set_terminator(
            self.current_block,
            Terminator::CondBranch(cond, then_block, else_block),
        );
    }

    pub fn set_return(&mut self, value: Option<Value>) {
        self.func
            .set_terminator(self.current_block, Terminator::Return(value));
    }

    pub fn set_unreachable(&mut self) {
        self.func
            .set_terminator(self.current_block, Terminator::Unreachable);
    }

    /// Returns true if the current block has no terminator yet.
    pub fn needs_return(&self) -> bool {
        matches!(
            self.func.blocks[self.current_block].terminator,
            Terminator::Pending
        )
    }

    // 内部方法

    fn emit(&mut self, kind: InstKind, ty: Type) -> Value {
        let inst = Inst::new(kind, ty);
        self.func.push_inst(self.current_block, inst)
    }

    fn emit_void(&mut self, kind: InstKind) {
        let inst = Inst::new(kind, Type::Void);
        self.func.push_inst(self.current_block, inst);
    }
}

fn int_to_width(ty: IntTy) -> IntWidth {
    match ty {
        IntTy::I8 | IntTy::U8 => IntWidth::I8,
        IntTy::I16 | IntTy::U16 => IntWidth::I16,
        IntTy::I32 | IntTy::U32 => IntWidth::I32,
        IntTy::I64 | IntTy::U64 | IntTy::Isize | IntTy::Usize => IntWidth::I64,
        IntTy::I128 | IntTy::U128 => IntWidth::I128,
    }
}

fn float_to_width(ty: FloatTy) -> FloatWidth {
    match ty {
        FloatTy::F16 => FloatWidth::F16,
        FloatTy::F32 => FloatWidth::F32,
        FloatTy::F64 => FloatWidth::F64,
        FloatTy::F128 => FloatWidth::F128,
    }
}
