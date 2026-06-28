pub mod block;
pub mod builder;
pub mod func;
pub mod instr;
pub mod lower;
pub mod module;
pub mod types;
pub mod value;

mod display;

pub mod backend;

pub use backend::Backend;
pub use backend::c::CBackend;
pub use func::Function;
pub use lower::lower_hir;
pub use module::Module;
pub use value::{BlockId, FuncRef, Value};
