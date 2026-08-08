use std::collections::{HashMap, HashSet};

use hir::{
    Name,
    body::{
        BinaryOp, Body, BodyId, Expr, ExprId, LiteralPattern, MatchArm, PatId, Pattern,
        PatternBindingId, ResolvedName, Stmt, StmtId, UnaryOp,
    },
    item_tree::{
        FunctionId, HirAssocTypeConstraint, HirFunction, HirGenericBound, HirStructField,
        HirTypeRef, HirVariantKind, ItemTree, ModuleId, StructId, TopLevelItem, TraitId,
        Visibility,
    },
    place::Projection,
};

use crate::{
    checker::{GenericEdge, PendingGenericCall, TypeChecker},
    context::{BodyCtx, LambdaCtx},
    lang_items::LangItem,
    lowering::{
        builtin_callable_kind, collect_subst, generic_param_map_with_consts, substitute_type,
    },
    result::{
        CaptureMode, CapturePlace, CaptureSource, ForLoopInfo, LabelStyle, LambdaCapture,
        LambdaInfo, OperatorCall, PatternBindingMode, SourceLabel, TraitMethodCall, ValueUse,
    },
    trait_env::TraitBound,
    types::{CallableSignature, ClosureId, ClosureKind, ConstArg, FloatTy, IntTy, Type},
};

#[must_use]
pub fn struct_field_is_visible(
    hir: &hir::HirFile,
    body_id: BodyId,
    struct_id: StructId,
    visibility: &Visibility,
) -> bool {
    if visibility.is_public() {
        return true;
    }
    let owner = body_owner(hir, body_id);
    let Some((owner_range, function_id, const_id)) = owner else {
        return false;
    };

    struct_field_is_visible_for_owner(
        hir,
        owner_range,
        function_id,
        const_id,
        struct_id,
        visibility,
    )
}

#[must_use]
pub fn method_is_visible(
    hir: &hir::HirFile,
    body_id: BodyId,
    function_id: FunctionId,
    visibility: &Visibility,
) -> bool {
    if visibility.is_public() {
        return true;
    }
    let Some((owner_range, owner_function, owner_const)) = body_owner(hir, body_id) else {
        return false;
    };
    method_is_visible_for_owner(
        hir,
        owner_range,
        owner_function,
        owner_const,
        function_id,
        visibility,
    )
}

fn body_owner(
    hir: &hir::HirFile,
    body_id: BodyId,
) -> Option<(
    rowan::TextRange,
    Option<FunctionId>,
    Option<hir::item_tree::ConstId>,
)> {
    hir.function_bodies
        .iter()
        .find_map(|(function_id, candidate)| {
            (*candidate == body_id).then(|| {
                (
                    hir.item_tree.functions[*function_id].name_range,
                    Some(*function_id),
                    None,
                )
            })
        })
        .or_else(|| {
            hir.const_bodies.iter().find_map(|(const_id, candidate)| {
                (*candidate == body_id).then(|| {
                    (
                        hir.item_tree.consts[*const_id].name_range,
                        None,
                        Some(*const_id),
                    )
                })
            })
        })
}

fn struct_field_is_visible_for_owner(
    hir: &hir::HirFile,
    owner_range: rowan::TextRange,
    function_id: Option<FunctionId>,
    const_id: Option<hir::item_tree::ConstId>,
    struct_id: StructId,
    visibility: &Visibility,
) -> bool {
    if visibility.is_public() {
        return true;
    }

    let strukt = &hir.item_tree.structs[struct_id];
    if hir.package_for_range(strukt.name_range) != hir.package_for_range(owner_range) {
        return false;
    }

    let Some(owner) = containing_module(
        &hir.item_tree,
        &hir.item_tree.top_level,
        None,
        &|item| matches!(item, TopLevelItem::Struct(id) if id == struct_id),
    ) else {
        return true;
    };
    let Some(current) =
        containing_module(&hir.item_tree, &hir.item_tree.top_level, None, &|item| {
            item_contains_body_owner(&hir.item_tree, item, function_id, const_id)
        })
    else {
        return false;
    };

    module_contains(&hir.item_tree, owner, current)
}

fn method_is_visible_for_owner(
    hir: &hir::HirFile,
    owner_range: rowan::TextRange,
    owner_function: Option<FunctionId>,
    owner_const: Option<hir::item_tree::ConstId>,
    function_id: FunctionId,
    visibility: &Visibility,
) -> bool {
    if visibility.is_public() {
        return true;
    }
    let function = &hir.item_tree.functions[function_id];
    if hir.package_for_range(function.name_range) != hir.package_for_range(owner_range) {
        return false;
    }

    let Some(defining_module) =
        containing_module(&hir.item_tree, &hir.item_tree.top_level, None, &|item| {
            item_contains_function(&hir.item_tree, item, function_id)
        })
    else {
        return true;
    };
    let Some(current_module) =
        containing_module(&hir.item_tree, &hir.item_tree.top_level, None, &|item| {
            item_contains_body_owner(&hir.item_tree, item, owner_function, owner_const)
        })
    else {
        return false;
    };

    module_contains(&hir.item_tree, defining_module, current_module)
}

const fn capture_mode(is_move: bool, use_kind: ValueUse) -> CaptureMode {
    if is_move {
        return CaptureMode::Value;
    }
    match use_kind {
        ValueUse::Shared | ValueUse::Copy => CaptureMode::Shared,
        ValueUse::Mutable => CaptureMode::Mutable,
        ValueUse::Move => CaptureMode::Value,
    }
}

mod aggregates;
mod bounds;
mod calls;
mod closures;
mod expressions;
mod methods;
mod operators;
mod patterns;

fn containing_module(
    tree: &ItemTree,
    items: &[TopLevelItem],
    owner: Option<ModuleId>,
    target: &impl Fn(TopLevelItem) -> bool,
) -> Option<ModuleId> {
    for &item in items {
        if target(item) {
            return owner;
        }
        if let TopLevelItem::Module(module) = item
            && let Some(children) = &tree.modules[module].items
            && let Some(owner) = containing_module(tree, children, Some(module), target)
        {
            return Some(owner);
        }
    }
    None
}

fn item_contains_function(tree: &ItemTree, item: TopLevelItem, target: FunctionId) -> bool {
    match item {
        TopLevelItem::Function(function) => function == target,
        TopLevelItem::Impl(imp) => tree.impls[imp].methods.contains(&target),
        TopLevelItem::Trait(trait_id) => tree.traits[trait_id].default_methods.contains(&target),
        _ => false,
    }
}

fn item_contains_body_owner(
    tree: &ItemTree,
    item: TopLevelItem,
    function: Option<FunctionId>,
    konst: Option<hir::item_tree::ConstId>,
) -> bool {
    function.is_some_and(|target| item_contains_function(tree, item, target))
        || konst.is_some_and(|target| match item {
            TopLevelItem::Const(id) => id == target,
            TopLevelItem::Impl(imp) => tree.impls[imp].consts.contains(&target),
            _ => false,
        })
}

fn module_contains(tree: &ItemTree, ancestor: ModuleId, target: ModuleId) -> bool {
    ancestor == target
        || tree.modules[ancestor].items.as_ref().is_some_and(|items| {
            items.iter().any(|item| {
                matches!(item, TopLevelItem::Module(module) if module_contains(tree, *module, target))
            })
        })
}

struct ResolvedMethod {
    fid: FunctionId,
    function: HirFunction,
    subst: HashMap<String, Type>,
    trait_id: Option<TraitId>,
    from_trait_bound: bool,
}

const fn callable_signature_type(signature: CallableSignature) -> Type {
    Type::CallableConstraint(signature)
}

fn expected_has_param(ty: &Type) -> bool {
    type_has_param_where(ty, &|_| true)
}

fn record_generic_arg_spans(
    pattern: &Type,
    params: &HashMap<String, Type>,
    span: Option<rowan::TextRange>,
    spans: &mut HashMap<String, rowan::TextRange>,
) {
    let Some(span) = span else {
        return;
    };
    for name in params.keys() {
        if type_has_param_where(pattern, &|candidate| candidate == name) {
            spans.entry(name.clone()).or_insert(span);
        }
    }
}

fn pattern_has_unresolved_param(ty: &Type, subst: &HashMap<String, Type>) -> bool {
    type_has_param_where(ty, &|name| subst.get(name).is_none_or(generic_arg_unknown))
}

fn type_has_param_where(ty: &Type, predicate: &impl Fn(&str) -> bool) -> bool {
    match ty {
        Type::Param(name) | Type::Const(ConstArg::Param(name)) => predicate(name),
        Type::Ref(inner, _) | Type::Slice(inner) | Type::Ptr { inner, .. } => {
            type_has_param_where(inner, predicate)
        }
        Type::Tuple(elements) => elements
            .iter()
            .any(|element| type_has_param_where(element, predicate)),
        Type::Array(inner, len) => {
            type_has_param_where(inner, predicate)
                || matches!(len, ConstArg::Param(name) if predicate(name))
        }
        Type::Struct(_, args) | Type::Enum(_, args) | Type::FunctionItem { args, .. } => {
            args.iter().any(|arg| type_has_param_where(arg, predicate))
        }
        Type::CallableConstraint(signature)
        | Type::Closure { signature, .. }
        | Type::OpaqueCallable { signature, .. } => {
            signature
                .params
                .iter()
                .any(|arg| type_has_param_where(arg, predicate))
                || type_has_param_where(&signature.ret, predicate)
        }
        _ => false,
    }
}

fn bound_target_param(bound: &HirGenericBound) -> Option<&str> {
    match &bound.target_ty {
        HirTypeRef::Named(path)
            if matches!(path.anchor, hir::item_tree::PathAnchor::Plain)
                && path.segments.len() == 1
                && path.type_args.is_empty() =>
        {
            Some(path.segments[0].0.as_str())
        }
        _ => None,
    }
}

fn type_has_unresolved_inference(ty: &Type) -> bool {
    match ty {
        Type::InferVar(_)
        | Type::Unknown
        | Type::Error
        | Type::Const(ConstArg::Unknown | ConstArg::Error) => true,
        Type::Ref(inner, _) | Type::Slice(inner) | Type::Ptr { inner, .. } => {
            type_has_unresolved_inference(inner)
        }
        Type::Tuple(elements) => elements.iter().any(type_has_unresolved_inference),
        Type::Array(inner, len) => {
            type_has_unresolved_inference(inner)
                || matches!(len, ConstArg::Unknown | ConstArg::Error)
        }
        Type::Struct(_, args) | Type::Enum(_, args) | Type::FunctionItem { args, .. } => {
            args.iter().any(type_has_unresolved_inference)
        }
        Type::CallableConstraint(signature)
        | Type::Closure { signature, .. }
        | Type::OpaqueCallable { signature, .. } => {
            signature.params.iter().any(type_has_unresolved_inference)
                || type_has_unresolved_inference(&signature.ret)
        }
        _ => false,
    }
}

const fn generic_arg_unknown(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Unknown | Type::Error | Type::Const(ConstArg::Unknown | ConstArg::Error)
    )
}

fn is_supported_cast(source: &Type, target: &Type) -> bool {
    if is_unsafe_dst_layout_cast(source, target) || is_str_to_byte_slice_cast(source, target) {
        return true;
    }
    matches!(
        (source, target),
        (
            Type::Int(_) | Type::InferInt,
            Type::Int(_) | Type::Float(_) | Type::Bool | Type::Ptr { .. }
        ) | (
            Type::Float(_) | Type::InferFloat,
            Type::Int(_) | Type::Float(_)
        ) | (Type::Bool | Type::Char, Type::Int(_))
            | (Type::Int(IntTy::U8), Type::Char)
            | (Type::Ptr { .. }, Type::Ptr { .. })
    ) || matches!(
        (source, target),
        (
            Type::Ref(source, source_mutable),
            Type::Ptr {
                mutable: target_mutable,
                inner: target,
            },
        ) if (source == target
            || matches!(
                (source.as_ref(), target.as_ref()),
                (Type::InferInt, Type::Int(IntTy::I32))
                    | (Type::InferFloat, Type::Float(FloatTy::F64))
            ))
            && source.is_sized()
            && (!*target_mutable || *source_mutable)
    )
}

fn is_unsafe_dst_layout_cast(source: &Type, target: &Type) -> bool {
    match (source, target) {
        (Type::Tuple(parts), Type::Ref(target, false)) => {
            let Type::Slice(target) = target.as_ref() else {
                return false;
            };
            matches!(
                parts.as_slice(),
                [Type::Ptr { inner, .. }, Type::Int(IntTy::Usize)] if inner.as_ref() == target.as_ref()
            )
        }
        // `&[T] as (*const T, usize)`: the decomposition direction. A `*mut T`
        // part additionally requires the slice reference itself to be mutable.
        (Type::Ref(source, source_mut), Type::Tuple(parts)) => {
            let Type::Slice(element) = source.as_ref() else {
                return false;
            };
            matches!(
                parts.as_slice(),
                [Type::Ptr { inner, mutable }, Type::Int(IntTy::Usize)]
                    if inner.as_ref() == element.as_ref() && (!mutable || *source_mut)
            )
        }
        (Type::Ref(source, false), Type::Ref(target, false)) => matches!(
            (source.as_ref(), target.as_ref()),
            (Type::Slice(element), Type::Str)
                if matches!(element.as_ref(), Type::Int(IntTy::U8))
        ),
        _ => false,
    }
}

fn is_str_to_byte_slice_cast(source: &Type, target: &Type) -> bool {
    matches!(
        (source, target),
        (Type::Ref(source, false), Type::Ref(target, false))
            if matches!(source.as_ref(), Type::Str)
                && matches!(target.as_ref(), Type::Slice(element)
                    if matches!(element.as_ref(), Type::Int(IntTy::U8)))
    )
}

fn type_contains_unresolved_const_param(ty: &Type, params: &HashMap<String, Type>) -> bool {
    match ty {
        Type::Const(ConstArg::Param(name)) => !matches!(params.get(name), Some(Type::Const(_))),
        Type::Ref(inner, _) | Type::Ptr { inner, .. } | Type::Slice(inner) => {
            type_contains_unresolved_const_param(inner, params)
        }
        Type::Tuple(elements) => elements
            .iter()
            .any(|element| type_contains_unresolved_const_param(element, params)),
        Type::Array(inner, len) => {
            type_contains_unresolved_const_param(inner, params)
                || matches!(len, ConstArg::Param(name) if !matches!(params.get(name), Some(Type::Const(_))))
        }
        Type::Struct(_, args) | Type::Enum(_, args) => args
            .iter()
            .any(|arg| type_contains_unresolved_const_param(arg, params)),
        Type::FunctionItem { args, .. } => args
            .iter()
            .any(|arg| type_contains_unresolved_const_param(arg, params)),
        Type::CallableConstraint(signature)
        | Type::Closure { signature, .. }
        | Type::OpaqueCallable { signature, .. } => {
            signature
                .params
                .iter()
                .any(|arg| type_contains_unresolved_const_param(arg, params))
                || type_contains_unresolved_const_param(&signature.ret, params)
        }
        _ => false,
    }
}

fn type_ref_contains_error(ty: &HirTypeRef) -> bool {
    match ty {
        HirTypeRef::Error => true,
        HirTypeRef::Ref(inner, _) | HirTypeRef::Slice(inner) | HirTypeRef::Ptr { inner, .. } => {
            type_ref_contains_error(inner)
        }
        HirTypeRef::Array(inner, len) => {
            type_ref_contains_error(inner) || matches!(len, hir::item_tree::HirConstArg::Error)
        }
        HirTypeRef::Tuple(elements) => elements.iter().any(type_ref_contains_error),
        HirTypeRef::Named(path) => path.type_args.iter().any(type_ref_contains_error),
        HirTypeRef::Const(value) => matches!(value, hir::item_tree::HirConstArg::Error),
        HirTypeRef::ImplTrait {
            trait_ty, callable, ..
        } => {
            type_ref_contains_error(trait_ty)
                || callable.as_ref().is_some_and(|signature| {
                    signature.params.iter().any(type_ref_contains_error)
                        || type_ref_contains_error(&signature.ret)
                })
        }
        HirTypeRef::Never | HirTypeRef::Unknown => false,
    }
}

fn grows_generic_arg(ty: &Type, params: &HashMap<String, Type>) -> bool {
    match ty {
        Type::Param(name) | Type::Const(ConstArg::Param(name)) => !params.contains_key(name),
        Type::Ref(inner, _) | Type::Ptr { inner, .. } | Type::Slice(inner) => {
            contains_current_param(inner, params)
        }
        Type::Array(inner, len) => {
            contains_current_param(inner, params) || const_contains_current_param(len, params)
        }
        Type::Tuple(elements) => elements
            .iter()
            .any(|element| contains_current_param(element, params)),
        Type::Struct(_, args) | Type::Enum(_, args) => {
            args.iter().any(|arg| contains_current_param(arg, params))
        }
        _ => false,
    }
}

fn contains_current_param(ty: &Type, params: &HashMap<String, Type>) -> bool {
    match ty {
        Type::Param(name) | Type::Const(ConstArg::Param(name)) => params.contains_key(name),
        Type::Ref(inner, _) | Type::Ptr { inner, .. } | Type::Slice(inner) => {
            contains_current_param(inner, params)
        }
        Type::Array(inner, len) => {
            contains_current_param(inner, params) || const_contains_current_param(len, params)
        }
        Type::Tuple(elements) => elements
            .iter()
            .any(|element| contains_current_param(element, params)),
        Type::Struct(_, args) | Type::Enum(_, args) => {
            args.iter().any(|arg| contains_current_param(arg, params))
        }
        _ => false,
    }
}

fn const_contains_current_param(value: &ConstArg, params: &HashMap<String, Type>) -> bool {
    matches!(value, ConstArg::Param(name) if params.contains_key(name))
}

const fn binary_operator_trait(op: BinaryOp) -> Option<(LangItem, &'static str)> {
    Some(match op {
        BinaryOp::Add => (LangItem::Add, "add"),
        BinaryOp::Sub => (LangItem::Sub, "sub"),
        BinaryOp::Mul => (LangItem::Mul, "mul"),
        BinaryOp::Div => (LangItem::Div, "div"),
        BinaryOp::Mod => (LangItem::Rem, "rem"),
        BinaryOp::BitAnd => (LangItem::BitAnd, "bitand"),
        BinaryOp::BitOr => (LangItem::BitOr, "bitor"),
        BinaryOp::BitXor => (LangItem::BitXor, "bitxor"),
        BinaryOp::Shl => (LangItem::Shl, "shl"),
        BinaryOp::Shr => (LangItem::Shr, "shr"),
        BinaryOp::Eq => (LangItem::PartialEq, "eq"),
        BinaryOp::Neq => (LangItem::PartialEq, "ne"),
        BinaryOp::Lt => (LangItem::PartialOrd, "lt"),
        BinaryOp::Gt => (LangItem::PartialOrd, "gt"),
        BinaryOp::LtEq => (LangItem::PartialOrd, "le"),
        BinaryOp::GtEq => (LangItem::PartialOrd, "ge"),
        _ => return None,
    })
}

const fn unary_operator_trait(op: UnaryOp) -> Option<(LangItem, &'static str)> {
    Some(match op {
        UnaryOp::Neg => (LangItem::Neg, "neg"),
        UnaryOp::Not => (LangItem::Not, "not"),
        _ => return None,
    })
}

const fn assign_operator_trait(op: BinaryOp) -> Option<(LangItem, &'static str)> {
    Some(match op {
        BinaryOp::Add => (LangItem::AddAssign, "add_assign"),
        BinaryOp::Sub => (LangItem::SubAssign, "sub_assign"),
        BinaryOp::Mul => (LangItem::MulAssign, "mul_assign"),
        BinaryOp::Div => (LangItem::DivAssign, "div_assign"),
        BinaryOp::Mod => (LangItem::RemAssign, "rem_assign"),
        BinaryOp::BitAnd => (LangItem::BitAndAssign, "bitand_assign"),
        BinaryOp::BitOr => (LangItem::BitOrAssign, "bitor_assign"),
        BinaryOp::BitXor => (LangItem::BitXorAssign, "bitxor_assign"),
        BinaryOp::Shl => (LangItem::ShlAssign, "shl_assign"),
        BinaryOp::Shr => (LangItem::ShrAssign, "shr_assign"),
        _ => return None,
    })
}
