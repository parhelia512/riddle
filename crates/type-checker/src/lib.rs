mod body;
mod checker;
mod context;
mod coverage;
pub mod incremental;
pub mod lang_items;
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
    CaptureMode, CapturePlace, CaptureSource, Diagnostic, ForLoopInfo, LabelStyle, LambdaCapture,
    LambdaInfo, OperatorCall, PatternBindingMode, Severity, SourceLabel, TraitMethodCall,
    TypeCheckResult, ValueUse,
};
pub use trait_env::TraitEnv;
pub use types::{
    CallableSignature, ClosureId, ClosureKind, ConstArg, FloatTy, IntTy, OpaqueCallableId, Type,
};
