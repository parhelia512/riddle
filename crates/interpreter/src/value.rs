use std::rc::Rc;

use mir::value::FuncRef;

/// A runtime value held in an interpreter register.
///
/// Integers always carry the two's-complement bit pattern of their static
/// MIR width; signedness is applied when values are compared, printed, or
/// converted. Pointer values carry `(alloc_id << 32) | offset` bits, so
/// pointer equality is bit equality, matching the C backend.
#[derive(Clone)]
pub enum Val {
    /// Integer bits interpreted through the static MIR type.
    Int(u64),
    /// Float value; `f32` results are rounded through `f32` on every
    /// operation to match C `float` arithmetic.
    Float(f64),
    Bool(bool),
    Char(char),
    /// Owned string value in a register (static type `Str`).
    Str(Rc<str>),
    /// Thin pointer or reference: `(alloc_id << 32) | offset`.
    Ptr(u64),
    /// Fat pointer or reference (`&str`, `&[T]`): pointer bits plus a
    /// length in elements (`[T]`) or bytes (`str`).
    Fat(u64, u64),
    /// Function reference; interned into an id when stored to memory.
    FnPtr(FuncRef),
    /// Struct, enum, or tuple aggregate in a register.
    Struct(Vec<Val>),
    /// Fixed-length array value in a register.
    Array(Rc<Vec<Val>>),
    Unit,
}

impl Val {
    /// Pointer bits of a pointer-like value, or `None`.
    #[must_use]
    pub fn as_ptr(&self) -> Option<u64> {
        match self {
            Self::Ptr(bits) => Some(*bits),
            _ => None,
        }
    }

    /// Unsigned integer bits of an integer-like value, or `None`.
    #[must_use]
    pub fn as_int(&self) -> Option<u64> {
        match self {
            Self::Int(bits) => Some(*bits),
            _ => None,
        }
    }

    /// Boolean value, or `None`.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// Owned string content for `Str` registers and fat `&str` values.
    ///
    /// Fat values carry only pointer bits, so the caller supplies the
    /// memory to read the bytes from.
    #[must_use]
    pub fn str_content(&self, mem: &super::mem::Memory) -> Option<String> {
        match self {
            Self::Str(text) => Some(text.to_string()),
            Self::Fat(ptr, len) => mem
                .read_bytes(*ptr, *len as usize)
                .ok()
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
            _ => None,
        }
    }
}

impl std::fmt::Debug for Val {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Int(bits) => write!(f, "Int({bits})"),
            Self::Float(value) => write!(f, "Float({value})"),
            Self::Bool(value) => write!(f, "Bool({value})"),
            Self::Char(value) => write!(f, "Char({value:?})"),
            Self::Str(text) => write!(f, "Str({text:?})"),
            Self::Ptr(bits) => write!(f, "Ptr({bits:#x})"),
            Self::Fat(bits, len) => write!(f, "Fat({bits:#x}, {len})"),
            Self::FnPtr(func) => write!(f, "FnPtr({func:?})"),
            Self::Struct(_) => f.debug_struct("Struct").finish(),
            Self::Array(_) => f.debug_struct("Array").finish(),
            Self::Unit => write!(f, "Unit"),
        }
    }
}
