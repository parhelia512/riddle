use crate::types::Type;
use crate::value::{BlockId, FuncRef, Value};

// 常量值

#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Int(u64, IntWidth),
    NegativeInt(u64, IntWidth),
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatWidth {
    F32,
    F64,
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
    IntToChar,
    IntToFloat,
    FloatToInt,
    FloatToFloat,
    BoolToInt,
    IntToBool,
    IntToPtr,
    PtrToPtr,
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
    #[must_use]
    pub const fn new(kind: InstKind, ty: Type) -> Self {
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

    /// Compute the target-dependent size of a type.
    SizeOf(Type),

    /// Stack allocation: `result = alloca type` (returns `Ptr<type>`)
    Alloca(Type),

    /// GC heap allocation: `result = heap_alloc type` (returns `Ptr<type>`)
    /// Used when escape analysis determines the value may outlive the stack frame.
    HeapAlloc(Type),

    /// Release a compiler-owned heap allocation after its contents are dropped.
    HeapFree(Value),

    /// Load from pointer: `result = load ptr`
    Load(Value),

    /// Store value to pointer: `store value, ptr`
    Store(Value, Value),

    /// Compute address of a struct field: `result = field_ptr(base, index)`
    FieldPtr(Value, usize),

    /// Compute address of an array element: `result = index_ptr(base, index)`
    IndexPtr(Value, Value),

    /// Compute a bounds-checked array or slice element address.
    CheckedIndexPtr(Value, Value, Value),

    /// Extract a field from an aggregate value: `result = extract_value(aggregate, index)`
    ExtractValue(Value, usize),

    /// Function call: `result = call(func, args)`
    Call(FuncRef, Vec<Value>),

    /// Abort with a user-facing panic diagnostic at the source call site.
    Panic(Value, PanicSite),

    /// Obtain a typed function pointer.
    FunctionRef(FuncRef),

    /// Call a function pointer value.
    CallIndirect(Value, Vec<Value>),

    /// Construct a struct value: `result = struct { fields... }`
    StructValue(Vec<Value>),

    /// Construct an aggregate by field index; unspecified fields are zeroed.
    SparseStructValue(Vec<(usize, Value)>),

    /// Construct an array value: `result = [ elements... ]`
    ArrayValue(Vec<Value>),

    /// Construct a tuple value: `result = ( elements... )`
    TupleValue(Vec<Value>),

    /// SSA φ-node: `result = phi [ (val, block) ... ]`
    /// Merges values from multiple predecessor blocks.
    Phi(Vec<(Value, BlockId)>),
}

/// The source location a `panic` instruction reports.
///
/// `offset` points into the combined lowering source, so the C backend can
/// resolve it to the original module file through its source-file segments.
/// `line`/`column` are the precomputed combined-source position, used as the
/// fallback when no segment covers the offset (single-file builds).
#[derive(Debug, Clone, PartialEq)]
pub struct PanicSite {
    pub offset: u32,
    pub line: u32,
    pub column: u32,
}

// 终止指令

/// Block terminator — every basic block must end with exactly one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    /// Temporary builder state; must be replaced before code generation.
    Pending,

    /// Unconditional jump to another block.
    Branch(BlockId),

    /// Conditional branch: if `cond` then `then_block` else `else_block`.
    CondBranch(Value, BlockId, BlockId),

    /// Return from the function.
    Return(Option<Value>),

    /// Mark a control-flow path that cannot be reached by a valid program.
    Unreachable,
}
