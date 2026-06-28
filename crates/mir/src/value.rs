use la_arena::Idx;

use crate::block::Block;

/// Handle to a basic block within a function.
pub type BlockId = Idx<Block>;

/// An SSA value (virtual register) within a function body.
///
/// Each instruction that produces a result gets a unique `Value`.
/// `Value(0)` through `Value(N-1)` are reserved for function parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Value(pub u32);

/// Reference to a callable function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FuncRef {
    /// A function defined in the same module.
    Local(String),
    /// A compiler intrinsic (e.g. `print`, `alloc`).
    Intrinsic(String),
    /// An externally-linked function (declared via `extern "C"`).
    Extern(String),
}
