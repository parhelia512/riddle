use std::{
    collections::{HashMap, hash_map::Entry},
    hash::BuildHasher,
};

use crate::{CallableSignature, ConstArg, Type};

pub fn collect_subst<S: BuildHasher>(
    expected: &Type,
    actual: &Type,
    subst: &mut HashMap<String, Type, S>,
) -> bool {
    match expected {
        Type::Param(name) => match subst.entry(name.clone()) {
            Entry::Occupied(entry) => entry.get() == actual,
            Entry::Vacant(entry) => {
                entry.insert(actual.clone());
                true
            }
        },
        Type::Const(expected) => match actual {
            Type::Const(actual) => collect_const_subst(expected, actual, subst),
            _ => expected.is_unknown_like() || actual.is_unknown_like(),
        },
        Type::Ref(expected_inner, expected_mut) => match actual {
            Type::Ref(actual_inner, actual_mut) => {
                expected_mut == actual_mut && collect_subst(expected_inner, actual_inner, subst)
            }
            _ => false,
        },
        Type::DynTrait {
            trait_id: expected_id,
            args: expected_args,
        } => match actual {
            Type::DynTrait {
                trait_id: actual_id,
                args: actual_args,
            } if expected_id == actual_id => collect_args_subst(expected_args, actual_args, subst),
            _ => false,
        },
        Type::Ptr {
            mutable: expected_mut,
            inner: expected_inner,
        } => match actual {
            Type::Ptr {
                mutable: actual_mut,
                inner: actual_inner,
            } => expected_mut == actual_mut && collect_subst(expected_inner, actual_inner, subst),
            _ => false,
        },
        Type::Tuple(expected_elems) => match actual {
            Type::Tuple(actual_elems) => collect_args_subst(expected_elems, actual_elems, subst),
            _ => false,
        },
        Type::Slice(expected_inner) => match actual {
            Type::Slice(actual_inner) | Type::Array(actual_inner, _) => {
                collect_subst(expected_inner, actual_inner, subst)
            }
            _ => false,
        },
        Type::Array(expected_inner, expected_len) => match actual {
            Type::Array(actual_inner, actual_len) => {
                collect_const_subst(expected_len, actual_len, subst)
                    && collect_subst(expected_inner, actual_inner, subst)
            }
            _ => false,
        },
        Type::Struct(expected_id, expected_args) => match actual {
            Type::Struct(actual_id, actual_args) if expected_id == actual_id => {
                collect_args_subst(expected_args, actual_args, subst)
            }
            _ => false,
        },
        Type::Enum(expected_id, expected_args) => match actual {
            Type::Enum(actual_id, actual_args) if expected_id == actual_id => {
                collect_args_subst(expected_args, actual_args, subst)
            }
            _ => false,
        },
        Type::FunctionItem {
            function: expected_function,
            args: expected_args,
        } => match actual {
            Type::FunctionItem {
                function: actual_function,
                args: actual_args,
            } if expected_function == actual_function => {
                collect_args_subst(expected_args, actual_args, subst)
            }
            _ => false,
        },
        Type::Closure {
            id: expected_id,
            signature: expected,
        } => match actual {
            Type::Closure {
                id: actual_id,
                signature: actual,
            } if expected_id == actual_id => collect_signature_subst(expected, actual, subst),
            _ => false,
        },
        Type::OpaqueCallable {
            id: expected_id,
            signature: expected,
        } => match actual {
            Type::OpaqueCallable {
                id: actual_id,
                signature: actual,
            } if expected_id == actual_id => collect_signature_subst(expected, actual, subst),
            _ => false,
        },
        Type::CallableConstraint(expected) => {
            collect_callable_constraint_subst(expected, actual, subst)
        }
        _ => expected.is_unknown_like() || actual.is_unknown_like() || expected == actual,
    }
}

fn collect_args_subst<S: BuildHasher>(
    expected: &[Type],
    actual: &[Type],
    subst: &mut HashMap<String, Type, S>,
) -> bool {
    expected.len() == actual.len()
        && expected
            .iter()
            .zip(actual)
            .all(|(expected, actual)| collect_subst(expected, actual, subst))
}

fn collect_signature_subst<S: BuildHasher>(
    expected: &CallableSignature,
    actual: &CallableSignature,
    subst: &mut HashMap<String, Type, S>,
) -> bool {
    expected.kind == actual.kind
        && expected.is_unsafe == actual.is_unsafe
        && collect_args_subst(&expected.params, &actual.params, subst)
        && collect_subst(&expected.ret, &actual.ret, subst)
}

fn collect_callable_constraint_subst<S: BuildHasher>(
    expected: &CallableSignature,
    actual: &Type,
    subst: &mut HashMap<String, Type, S>,
) -> bool {
    let (Type::CallableConstraint(actual)
    | Type::Closure {
        signature: actual, ..
    }
    | Type::OpaqueCallable {
        signature: actual, ..
    }) = actual
    else {
        return false;
    };
    (!actual.is_unsafe || expected.is_unsafe)
        && expected.kind.accepts(actual.kind)
        && collect_args_subst(&expected.params, &actual.params, subst)
        && collect_subst(&expected.ret, &actual.ret, subst)
}

fn collect_const_subst<S: BuildHasher>(
    expected: &ConstArg,
    actual: &ConstArg,
    subst: &mut HashMap<String, Type, S>,
) -> bool {
    match expected {
        ConstArg::Param(name) => match subst.get(name) {
            Some(Type::Const(existing)) => existing == actual,
            Some(_) => false,
            None => {
                subst.insert(name.clone(), Type::Const(actual.clone()));
                true
            }
        },
        _ => expected.is_unknown_like() || actual.is_unknown_like() || expected == actual,
    }
}

#[must_use]
pub fn substitute_type<S: BuildHasher>(ty: &Type, subst: &HashMap<String, Type, S>) -> Type {
    match ty {
        Type::Param(name) => subst.get(name).cloned().unwrap_or_else(|| ty.clone()),
        Type::Const(value) => Type::Const(substitute_const(value, subst)),
        Type::Ref(inner, mutable) => Type::Ref(Box::new(substitute_type(inner, subst)), *mutable),
        Type::DynTrait { trait_id, args } => Type::DynTrait {
            trait_id: *trait_id,
            args: args.iter().map(|ty| substitute_type(ty, subst)).collect(),
        },
        Type::Ptr { mutable, inner } => Type::Ptr {
            mutable: *mutable,
            inner: Box::new(substitute_type(inner, subst)),
        },
        Type::Tuple(elements) => Type::Tuple(
            elements
                .iter()
                .map(|ty| substitute_type(ty, subst))
                .collect(),
        ),
        Type::Slice(inner) => Type::Slice(Box::new(substitute_type(inner, subst))),
        Type::Array(inner, len) => Type::Array(
            Box::new(substitute_type(inner, subst)),
            substitute_const(len, subst),
        ),
        Type::Struct(id, args) => Type::Struct(
            *id,
            args.iter().map(|ty| substitute_type(ty, subst)).collect(),
        ),
        Type::Enum(id, args) => Type::Enum(
            *id,
            args.iter().map(|ty| substitute_type(ty, subst)).collect(),
        ),
        Type::FunctionItem { function, args } => Type::FunctionItem {
            function: *function,
            args: args.iter().map(|ty| substitute_type(ty, subst)).collect(),
        },
        Type::Closure { id, signature } => Type::Closure {
            id: *id,
            signature: CallableSignature {
                is_unsafe: signature.is_unsafe,
                kind: signature.kind,
                params: signature
                    .params
                    .iter()
                    .map(|ty| substitute_type(ty, subst))
                    .collect(),
                ret: Box::new(substitute_type(&signature.ret, subst)),
            },
        },
        Type::OpaqueCallable { id, signature } => Type::OpaqueCallable {
            id: *id,
            signature: CallableSignature {
                is_unsafe: signature.is_unsafe,
                kind: signature.kind,
                params: signature
                    .params
                    .iter()
                    .map(|ty| substitute_type(ty, subst))
                    .collect(),
                ret: Box::new(substitute_type(&signature.ret, subst)),
            },
        },
        Type::CallableConstraint(signature) => Type::CallableConstraint(CallableSignature {
            is_unsafe: signature.is_unsafe,
            kind: signature.kind,
            params: signature
                .params
                .iter()
                .map(|ty| substitute_type(ty, subst))
                .collect(),
            ret: Box::new(substitute_type(&signature.ret, subst)),
        }),
        _ => ty.clone(),
    }
}

fn substitute_const<S: BuildHasher>(
    value: &ConstArg,
    subst: &HashMap<String, Type, S>,
) -> ConstArg {
    match value {
        ConstArg::Param(name) => match subst.get(name) {
            Some(Type::Const(value)) => value.clone(),
            _ => value.clone(),
        },
        _ => value.clone(),
    }
}
