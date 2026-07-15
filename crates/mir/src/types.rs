/// MIR type system — a flattened representation of Riddle types,
/// oriented toward code generation rather than type checking.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    // 基本标量类型
    Int(IntTy),
    Float(FloatTy),
    Bool,
    Str,
    Char,
    Unit,
    Never,

    // 复合类型
    Ref(Box<Type>, bool), // (inner, mutable)
    Ptr(Box<Type>),
    Tuple(Vec<Type>),
    Array(Box<Type>, usize),
    Struct(StructType),
    Enum(EnumType),

    // 函数指针
    FnPtr(FnPtrType),

    /// No type (used for instructions that don't produce a value).
    Void,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    I128,
    Isize,
    U8,
    U16,
    U32,
    U64,
    U128,
    Usize,
}

impl IntTy {
    pub fn is_signed(self) -> bool {
        matches!(
            self,
            IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::I128 | IntTy::Isize
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatTy {
    F16,
    F32,
    F64,
    F128,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StructType {
    pub name: String,
    pub fields: Vec<(String, Type)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnumType {
    pub name: String,
    pub variants: Vec<EnumVariant>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EnumVariantKind {
    Unit,
    Tuple(Vec<Type>),
    Struct(Vec<(String, Type)>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnumVariant {
    pub name: String,
    pub discriminant: u32,
    pub kind: EnumVariantKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FnPtrType {
    pub params: Vec<Type>,
    pub ret: Box<Type>,
}

impl Type {
    /// Returns true if the type fits in a machine register.
    pub fn is_scalar(&self) -> bool {
        matches!(
            self,
            Type::Int(_)
                | Type::Float(_)
                | Type::Bool
                | Type::Char
                | Type::Ptr(_)
                | Type::Ref(_, _)
                | Type::FnPtr(_)
        )
    }

    /// Returns `true` if this type has a known size at compile time.
    /// Unsized types (like `str`) can only exist behind a pointer/reference.
    pub fn is_sized(&self) -> bool {
        !matches!(self, Type::Str)
        // ponytail: only str is unsized for now; [T] slices would also be unsized
    }

    /// Rough size estimate in bytes (used for alloca sizing).
    /// Backends may override this with target-specific layouts.
    pub fn size_bytes(&self) -> usize {
        match self {
            Type::Int(ty) => match ty {
                IntTy::I8 | IntTy::U8 => 1,
                IntTy::I16 | IntTy::U16 => 2,
                IntTy::I32 | IntTy::U32 => 4,
                IntTy::I64 | IntTy::U64 => 8,
                IntTy::I128 | IntTy::U128 => 16,
                IntTy::Isize | IntTy::Usize => 8,
            },
            Type::Float(ty) => match ty {
                FloatTy::F16 => 2,
                FloatTy::F32 => 4,
                FloatTy::F64 => 8,
                FloatTy::F128 => 16,
            },
            Type::Bool => 1,
            Type::Char => 4,
            Type::Ref(inner, _) | Type::Ptr(inner) => {
                if !inner.is_sized() {
                    16
                } else {
                    8
                }
            }
            Type::FnPtr(_) => 8,
            Type::Str => panic!("cannot compute the size of unsized `str`"),
            Type::Unit => 0,
            Type::Never => 0,
            _ => 8, // 聚合类型：降级为指针大小
        }
    }
}
