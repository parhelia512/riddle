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
    Ref(Box<Self>, bool), // (inner, mutable)
    Ptr(Box<Self>),
    Tuple(Vec<Self>),
    Slice(Box<Self>),
    Array(Box<Self>, usize),
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
    Isize,
    U8,
    U16,
    U32,
    U64,
    Usize,
}

impl IntTy {
    #[must_use]
    pub const fn is_signed(self) -> bool {
        matches!(
            self,
            Self::I8 | Self::I16 | Self::I32 | Self::I64 | Self::Isize
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatTy {
    F32,
    F64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StructType {
    pub name: String,
    pub symbol: String,
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
    #[must_use]
    pub const fn is_scalar(&self) -> bool {
        matches!(
            self,
            Self::Int(_)
                | Self::Float(_)
                | Self::Bool
                | Self::Char
                | Self::Ptr(_)
                | Self::Ref(_, _)
                | Self::FnPtr(_)
        )
    }

    /// Returns `true` if this type has a known size at compile time.
    /// Unsized types (`str` and `[T]`) can only exist behind a pointer/reference.
    #[must_use]
    pub const fn is_sized(&self) -> bool {
        !matches!(self, Self::Str | Self::Slice(_))
    }

    /// Rough size estimate in bytes (used for alloca sizing).
    /// Backends may override this with target-specific layouts.
    #[must_use]
    pub fn size_bytes(&self) -> usize {
        match self {
            Self::Int(ty) => match ty {
                IntTy::I8 | IntTy::U8 => 1,
                IntTy::I16 | IntTy::U16 => 2,
                IntTy::I32 | IntTy::U32 => 4,
                IntTy::I64 | IntTy::U64 => 8,
                IntTy::Isize | IntTy::Usize => std::mem::size_of::<usize>(),
            },
            Self::Float(ty) => match ty {
                FloatTy::F32 => 4,
                FloatTy::F64 => 8,
            },
            Self::Bool => 1,
            Self::Char => 4,
            Self::Ref(inner, _) | Self::Ptr(inner) => {
                if inner.is_sized() {
                    std::mem::size_of::<usize>()
                } else {
                    2 * std::mem::size_of::<usize>()
                }
            }
            Self::FnPtr(_) => 2 * std::mem::size_of::<usize>(),
            Self::Str | Self::Slice(_) => {
                unreachable!("cannot compute the size of an unsized type")
            }
            Self::Unit | Self::Never => 0,
            _ => 8, // 聚合类型：降级为指针大小
        }
    }
}
