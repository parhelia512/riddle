mod substitution;

pub mod lang_items;
pub mod result;
pub mod trait_env;
pub mod types;

pub use result::{
    CaptureMode, CapturePlace, CaptureSource, Diagnostic, ForLoopInfo, GenericCall, LabelStyle,
    LambdaCapture, LambdaInfo, OperatorCall, PatternBindingMode, Severity, SourceLabel,
    TraitMethodCall, TypeCheckResult, ValueUse,
};
pub use substitution::{collect_subst, substitute_type};
pub use trait_env::{TraitAssocConstraint, TraitBound, TraitEnv};
pub use types::{
    CallableSignature, ClosureId, ClosureKind, ConstArg, FloatTy, IntTy, OpaqueCallableId, Type,
};
