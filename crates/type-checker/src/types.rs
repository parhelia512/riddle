use hir::{
    HirFile,
    item_tree::{EnumId, FunctionId, HirStruct, StructId},
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Int(IntTy),
    Float(FloatTy),
    InferInt,
    InferFloat,
    Bool,
    Str,
    Char,
    Unit,
    Never,
    Ref(Box<Type>, bool), // (inner, mutable)
    /// Raw pointer type: `*const T` or `*mut T`.
    Ptr {
        mutable: bool,
        inner: Box<Type>,
    },
    Tuple(Vec<Type>),
    Array(Box<Type>, ConstArg),
    Struct(StructId, Vec<Type>),
    Enum(EnumId, Vec<Type>),
    Param(String),
    Const(ConstArg),
    Function(FunctionId),
    Unknown,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConstArg {
    Value(usize),
    Param(String),
    Unknown,
    Error,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatTy {
    F16,
    F32,
    F64,
    F128,
}

impl Type {
    pub fn display(&self, hir: &HirFile) -> String {
        match self {
            Type::Int(ty) => ty.as_str().to_string(),
            Type::Float(ty) => ty.as_str().to_string(),
            Type::InferInt => "i32".to_string(),
            Type::InferFloat => "f64".to_string(),
            Type::Bool => "bool".to_string(),
            Type::Str => "str".to_string(),
            Type::Char => "char".to_string(),
            Type::Unit => "()".to_string(),
            Type::Never => "!".to_string(),
            Type::Ref(inner, mutable) => {
                let kw = if *mutable { "&mut " } else { "&" };
                format!("{}{}", kw, inner.display(hir))
            }
            Type::Ptr { mutable, inner } => {
                let kind = if *mutable { "*mut" } else { "*const" };
                format!("{kind} {}", inner.display(hir))
            }
            Type::Tuple(elements) => {
                let inner = elements
                    .iter()
                    .map(|ty| ty.display(hir))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({inner})")
            }
            Type::Array(inner, len) => format!("[{}; {}]", inner.display(hir), len.display()),
            Type::Struct(id, args) => {
                let HirStruct { name, .. } = &hir.item_tree.structs[*id];
                if args.is_empty() {
                    name.0.clone()
                } else {
                    let args = args
                        .iter()
                        .map(|arg| arg.display(hir))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{}<{}>", name.0, args)
                }
            }
            Type::Enum(id, args) => {
                let enum_data = &hir.item_tree.enums[*id];
                if args.is_empty() {
                    enum_data.name.0.clone()
                } else {
                    let args = args
                        .iter()
                        .map(|arg| arg.display(hir))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{}<{}>", enum_data.name.0, args)
                }
            }
            Type::Function(id) => {
                let function = &hir.item_tree.functions[*id];
                format!("fun {}", function.name.0)
            }
            Type::Param(name) => name.clone(),
            Type::Const(value) => value.display(),
            Type::Unknown => "_".to_string(),
            Type::Error => "<error>".to_string(),
        }
    }

    pub(crate) fn is_unknown_like(&self) -> bool {
        matches!(self, Type::Unknown | Type::Error)
    }

    pub(crate) fn is_numeric(&self) -> bool {
        matches!(
            self,
            Type::Int(_) | Type::Float(_) | Type::InferInt | Type::InferFloat
        )
    }

    pub(crate) fn is_integer(&self) -> bool {
        matches!(self, Type::Int(_) | Type::InferInt)
    }

    pub(crate) fn is_bitwise_scalar(&self) -> bool {
        self.is_integer() || matches!(self, Type::Bool)
    }

    pub(crate) fn is_ordered_scalar(&self) -> bool {
        self.is_numeric() || matches!(self, Type::Char)
    }

    pub(crate) fn is_never(&self) -> bool {
        matches!(self, Type::Never)
    }

    /// Returns `true` if this type has a known size at compile time.
    /// Unsized types (like `str`) can only exist behind a pointer/reference.
    pub fn is_sized(&self) -> bool {
        !matches!(self, Type::Str)
        // ponytail: only str is unsized for now; [T] slices would also be unsized
    }

    /// Compiler-intrinsic `Copy` candidates – types that are `Copy`
    /// regardless of whether a `Copy` trait is defined.
    pub fn is_fundamentally_copy(&self) -> bool {
        matches!(
            self,
            Type::Int(_)
                | Type::Float(_)
                | Type::InferInt
                | Type::InferFloat
                | Type::Bool
                | Type::Char
                | Type::Str // fat pointer {ptr, len} — plain data, trivially Copy
                | Type::Unit
                | Type::Never
                | Type::Ref(_, false)
                | Type::Ptr { .. }
                | Type::Function(_)
                | Type::Unknown
                | Type::Error
        )
    }

    pub(crate) fn or(self, fallback: Type) -> Type {
        if self.is_unknown_like() {
            fallback
        } else {
            self
        }
    }
}

impl ConstArg {
    pub fn display(&self) -> String {
        match self {
            ConstArg::Value(value) => value.to_string(),
            ConstArg::Param(name) => name.clone(),
            ConstArg::Unknown => "_".to_string(),
            ConstArg::Error => "<error>".to_string(),
        }
    }

    pub fn as_usize(&self) -> Option<usize> {
        match self {
            ConstArg::Value(value) => Some(*value),
            _ => None,
        }
    }

    pub(crate) fn is_unknown_like(&self) -> bool {
        matches!(self, ConstArg::Unknown | ConstArg::Error)
    }
}

impl IntTy {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "i8" => Some(Self::I8),
            "i16" => Some(Self::I16),
            "i32" => Some(Self::I32),
            "i64" => Some(Self::I64),
            "i128" => Some(Self::I128),
            "isize" => Some(Self::Isize),
            "u8" => Some(Self::U8),
            "u16" => Some(Self::U16),
            "u32" => Some(Self::U32),
            "u64" => Some(Self::U64),
            "u128" => Some(Self::U128),
            "usize" => Some(Self::Usize),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::I128 => "i128",
            Self::Isize => "isize",
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::U128 => "u128",
            Self::Usize => "usize",
        }
    }
}

impl FloatTy {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "f16" => Some(Self::F16),
            "f32" => Some(Self::F32),
            "f64" => Some(Self::F64),
            "f128" => Some(Self::F128),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::F16 => "f16",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::F128 => "f128",
        }
    }
}
