use hir::{
    HirFile,
    item_tree::{FunctionId, HirStruct, StructId},
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
    Ref(Box<Type>),
    Tuple(Vec<Type>),
    Array(Box<Type>),
    Struct(StructId),
    Function(FunctionId),
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
            Type::Ref(inner) => format!("&{}", inner.display(hir)),
            Type::Tuple(elements) => {
                let inner = elements
                    .iter()
                    .map(|ty| ty.display(hir))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({inner})")
            }
            Type::Array(inner) => format!("[{}]", inner.display(hir)),
            Type::Struct(id) => {
                let HirStruct { name, .. } = &hir.item_tree.structs[*id];
                name.0.clone()
            }
            Type::Function(id) => {
                let function = &hir.item_tree.functions[*id];
                format!("fun {}", function.name.0)
            }
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

    pub(crate) fn is_never(&self) -> bool {
        matches!(self, Type::Never)
    }

    pub(crate) fn or(self, fallback: Type) -> Type {
        if self.is_unknown_like() {
            fallback
        } else {
            self
        }
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
