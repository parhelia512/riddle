use hir::{
    HirFile,
    body::{BodyId, ExprId},
    item_tree::{EnumId, FunctionId, HirStruct, StructId},
};
use rowan::TextRange;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClosureId {
    pub body: BodyId,
    pub expr: ExprId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OpaqueCallableId(pub TextRange);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallableSignature {
    pub is_unsafe: bool,
    pub kind: ClosureKind,
    pub params: Vec<Type>,
    pub ret: Box<Type>,
}

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
    Slice(Box<Type>),
    Array(Box<Type>, ConstArg),
    Struct(StructId, Vec<Type>),
    Enum(EnumId, Vec<Type>),
    Param(String),
    Const(ConstArg),
    FunctionItem {
        function: FunctionId,
        args: Vec<Type>,
    },
    Closure {
        id: ClosureId,
        signature: CallableSignature,
    },
    OpaqueCallable {
        id: OpaqueCallableId,
        signature: CallableSignature,
    },
    CallableConstraint(CallableSignature),
    InferVar(u32),
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
    Isize,
    U8,
    U16,
    U32,
    U64,
    Usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatTy {
    F32,
    F64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClosureKind {
    Fn,
    FnMut,
    FnOnce,
}

impl ClosureKind {
    pub fn accepts(self, actual: Self) -> bool {
        matches!(
            (self, actual),
            (Self::FnOnce, _) | (Self::FnMut, Self::Fn | Self::FnMut) | (Self::Fn, Self::Fn)
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fn => "Fn",
            Self::FnMut => "FnMut",
            Self::FnOnce => "FnOnce",
        }
    }
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
            Type::Slice(inner) => format!("[{}]", inner.display(hir)),
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
            Type::FunctionItem { function: id, .. } => {
                let function = &hir.item_tree.functions[*id];
                let prefix = if function.is_unsafe { "unsafe " } else { "" };
                format!("{prefix}fun {}", function.name.0)
            }
            Type::Closure { signature, .. } => {
                format_callable_signature(signature, "anonymous ", hir)
            }
            Type::OpaqueCallable { signature, .. } => {
                format_callable_signature(signature, "impl ", hir)
            }
            Type::CallableConstraint(signature) => format_callable_signature(signature, "", hir),
            Type::InferVar(_) => "_".to_string(),
            Type::Param(name) => name.clone(),
            Type::Const(value) => value.display(),
            Type::Unknown => "_".to_string(),
            Type::Error => "<error>".to_string(),
        }
    }

    pub fn is_unknown_like(&self) -> bool {
        matches!(self, Type::Unknown | Type::Error | Type::InferVar(_))
    }

    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            Type::Int(_) | Type::Float(_) | Type::InferInt | Type::InferFloat
        )
    }

    pub fn is_integer(&self) -> bool {
        matches!(self, Type::Int(_) | Type::InferInt)
    }

    pub fn is_bitwise_scalar(&self) -> bool {
        self.is_integer() || matches!(self, Type::Bool)
    }

    pub fn is_ordered_scalar(&self) -> bool {
        self.is_numeric() || matches!(self, Type::Char)
    }

    pub fn is_never(&self) -> bool {
        matches!(self, Type::Never)
    }

    /// Returns `true` if this type has a known size at compile time.
    /// Unsized types (`str` and `[T]`) can only exist behind a pointer/reference.
    pub fn is_sized(&self) -> bool {
        match self {
            Type::Str | Type::Slice(_) => false,
            Type::Tuple(elements) => elements.iter().all(Type::is_sized),
            Type::Array(inner, _) => inner.is_sized(),
            Type::Struct(_, args) | Type::Enum(_, args) => args.iter().all(Type::is_sized),
            Type::CallableConstraint(signature)
            | Type::Closure { signature, .. }
            | Type::OpaqueCallable { signature, .. } => {
                signature.params.iter().all(Type::is_sized) && signature.ret.is_sized()
            }
            Type::FunctionItem { args, .. } => args.iter().all(Type::is_sized),
            _ => true,
        }
    }

    pub fn is_valid_value_type(&self) -> bool {
        match self {
            Type::Str | Type::Slice(_) => false,
            Type::Ref(inner, _) | Type::Ptr { inner, .. } => {
                matches!(inner.as_ref(), Type::Str | Type::Slice(_)) || inner.is_valid_value_type()
            }
            Type::Tuple(elements) => elements.iter().all(Type::is_valid_value_type),
            Type::Array(inner, _) => inner.is_valid_value_type(),
            Type::Struct(_, args) | Type::Enum(_, args) => {
                args.iter().all(Type::is_valid_value_type)
            }
            Type::CallableConstraint(signature)
            | Type::Closure { signature, .. }
            | Type::OpaqueCallable { signature, .. } => {
                signature.params.iter().all(Type::is_valid_value_type)
                    && signature.ret.is_valid_value_type()
            }
            Type::FunctionItem { args, .. } => args.iter().all(Type::is_valid_value_type),
            _ => true,
        }
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
                | Type::Unit
                | Type::Never
                | Type::Ref(_, false)
                | Type::Ptr { .. }
                | Type::FunctionItem { .. }
                | Type::InferVar(_)
                | Type::Unknown
                | Type::Error
        )
    }

    pub fn closure_kind(&self) -> Option<ClosureKind> {
        match self {
            Type::CallableConstraint(signature) => Some(signature.kind),
            Type::Closure { signature, .. } | Type::OpaqueCallable { signature, .. } => {
                Some(signature.kind)
            }
            Type::FunctionItem { .. } => Some(ClosureKind::Fn),
            _ => None,
        }
    }

    pub fn callable_signature(&self) -> Option<&CallableSignature> {
        match self {
            Type::CallableConstraint(signature)
            | Type::Closure { signature, .. }
            | Type::OpaqueCallable { signature, .. } => Some(signature),
            _ => None,
        }
    }

    pub fn or(self, fallback: Type) -> Type {
        if self.is_unknown_like() {
            fallback
        } else {
            self
        }
    }
}

fn format_callable_signature(signature: &CallableSignature, prefix: &str, hir: &HirFile) -> String {
    let params = signature
        .params
        .iter()
        .map(|param| param.display(hir))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{prefix}{}({params}) -> {}",
        signature.kind.as_str(),
        signature.ret.display(hir)
    )
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

    pub fn is_unknown_like(&self) -> bool {
        matches!(self, ConstArg::Unknown | ConstArg::Error)
    }
}

impl IntTy {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "i8" => Some(Self::I8),
            "i16" => Some(Self::I16),
            "i32" => Some(Self::I32),
            "i64" => Some(Self::I64),
            "isize" => Some(Self::Isize),
            "u8" => Some(Self::U8),
            "u16" => Some(Self::U16),
            "u32" => Some(Self::U32),
            "u64" => Some(Self::U64),
            "usize" => Some(Self::Usize),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::Isize => "isize",
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::Usize => "usize",
        }
    }

    pub fn contains_u64(self, value: u64) -> bool {
        match self {
            Self::I8 => value <= i8::MAX as u64,
            Self::I16 => value <= i16::MAX as u64,
            Self::I32 => value <= i32::MAX as u64,
            Self::I64 => value <= i64::MAX as u64,
            Self::Isize => value <= isize::MAX as u64,
            Self::U8 => u8::try_from(value).is_ok(),
            Self::U16 => u16::try_from(value).is_ok(),
            Self::U32 => u32::try_from(value).is_ok(),
            Self::U64 => true,
            Self::Usize => usize::try_from(value).is_ok(),
        }
    }

    pub fn contains_negative_magnitude(self, value: u64) -> bool {
        match self {
            Self::I8 => value <= (i8::MAX as u64) + 1,
            Self::I16 => value <= (i16::MAX as u64) + 1,
            Self::I32 => value <= (i32::MAX as u64) + 1,
            Self::I64 => value <= (i64::MAX as u64) + 1,
            Self::Isize => value <= (isize::MAX as u64) + 1,
            Self::U8 | Self::U16 | Self::U32 | Self::U64 | Self::Usize => false,
        }
    }
}

impl FloatTy {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "f32" => Some(Self::F32),
            "f64" => Some(Self::F64),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::F64 => "f64",
        }
    }
}
