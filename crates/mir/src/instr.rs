use crate::types::Type;
use crate::value::{BlockId, FuncRef, Value};

// 常量值

#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Int(i128, IntWidth),
    Float(f64, FloatWidth),
    Bool(bool),
    String(String),
    Char(char),
    Unit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntWidth {
    I8,
    I16,
    I32,
    I64,
    I128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatWidth {
    F16,
    F32,
    F64,
    F128,
}

// 运算符

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// Arithmetic negation.
    Neg,
    /// Logical / bitwise not.
    Not,
    /// Take shared reference (&).
    Ref,
    /// Take mutable reference (&mut).
    MutRef,
    /// Dereference a pointer/reference.
    Deref,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Neq,
    Lt,
    Gt,
    LtEq,
    GtEq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastOp {
    IntToInt,
    IntToFloat,
    FloatToInt,
    FloatToFloat,
    BoolToInt,
    IntToBool,
}

// IR 指令

/// A single non-terminator IR instruction.
///
/// Every instruction has a `kind` describing the operation and a `ty`
/// describing the result type. Side-effecting instructions use `Type::Void`.
#[derive(Debug, Clone, PartialEq)]
pub struct Inst {
    pub kind: InstKind,
    /// Result type of this instruction.
    pub ty: Type,
}

impl Inst {
    pub fn new(kind: InstKind, ty: Type) -> Self {
        Self { kind, ty }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum InstKind {
    /// Produce a constant literal.
    Const(ConstValue),

    /// Binary operation: `result = lhs op rhs`
    BinOp(BinOp, Value, Value),

    /// Unary operation: `result = op operand`
    UnOp(UnOp, Value),

    /// Comparison producing a boolean: `result = lhs cmp rhs`
    Cmp(CmpOp, Value, Value),

    /// Type cast: `result = cast(value, target_type)`
    Cast(CastOp, Value, Type),

    /// Stack allocation: `result = alloca type` (returns `Ptr<type>`)
    Alloca(Type),

    /// GC heap allocation: `result = heap_alloc type` (returns `Ptr<type>`)
    /// Used when escape analysis determines the value may outlive the stack frame.
    HeapAlloc(Type),

    /// Load from pointer: `result = load ptr`
    Load(Value),

    /// Store value to pointer: `store value, ptr`
    Store(Value, Value),

    /// Compute address of a struct field: `result = field_ptr(base, index)`
    FieldPtr(Value, usize),

    /// Compute address of an array element: `result = index_ptr(base, index)`
    IndexPtr(Value, Value),

    /// Extract a field from an aggregate value: `result = extract_value(aggregate, index)`
    ExtractValue(Value, usize),

    /// Function call: `result = call(func, args)`
    Call(FuncRef, Vec<Value>),

    /// Construct a struct value: `result = struct { fields... }`
    StructValue(Vec<Value>),

    /// Construct an array value: `result = [ elements... ]`
    ArrayValue(Vec<Value>),

    /// Construct a tuple value: `result = ( elements... )`
    TupleValue(Vec<Value>),

    /// SSA φ-node: `result = phi [ (val, block) ... ]`
    /// Merges values from multiple predecessor blocks.
    Phi(Vec<(Value, BlockId)>),
}

// 终止指令

/// Block terminator — every basic block must end with exactly one.
#[derive(Debug, Clone, PartialEq)]
pub enum Terminator {
    /// Unconditional jump to another block.
    Branch(BlockId),

    /// Conditional branch: if `cond` then `then_block` else `else_block`.
    CondBranch(Value, BlockId, BlockId),

    /// Return from the function.
    Return(Option<Value>),
}
