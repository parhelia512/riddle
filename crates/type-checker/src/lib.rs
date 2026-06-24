mod body;
mod checker;
mod context;
pub mod incremental;
mod lowering;
mod result;
mod traits;
mod types;

pub use checker::{TypeChecker, check_hir};
pub use incremental::{
    IncrementalStats, IncrementalTypeCheckResult, IncrementalTypeChecker, check_hir_incremental,
};
pub use result::{Diagnostic, TypeCheckResult};
pub use types::{FloatTy, IntTy, Type};
