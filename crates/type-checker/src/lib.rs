mod body;
mod checker;
mod context;
mod coverage;
pub mod incremental;
mod lowering;
mod result;
mod trait_env;
mod traits;
mod types;

pub use checker::{TypeChecker, check_hir};
pub use incremental::{
    IncrementalStats, IncrementalTypeCheckResult, IncrementalTypeChecker, check_hir_incremental,
};
pub use result::{
    CaptureMode, CaptureSource, Diagnostic, ForLoopInfo, LabelStyle, LambdaCapture, LambdaInfo,
    Severity, SourceLabel, TraitMethodCall, TypeCheckResult,
};
pub use trait_env::TraitEnv;
pub use types::{ClosureKind, ConstArg, FloatTy, IntTy, Type};
