use std::collections::HashMap;

use hir::item_tree::{EnumId, HirConstArg, HirPath, HirTypeRef, StructId, TraitId, TypeAliasId};
use rowan::TextRange;

use crate::{
    checker::TypeChecker,
    types::{ConstArg, FloatTy, IntTy, Type},
};

impl TypeChecker<'_> {
    pub(crate) fn lower_type_alias(&mut self, type_alias: TypeAliasId) -> Type {
        let alias = self.hir.item_tree.type_aliases[type_alias].clone();
        alias
            .ty
            .as_ref()
            .map(|ty| {
                self.lower_type_ref_with_params_at(
                    ty,
                    &HashMap::new(),
                    alias.ty_range.or(Some(alias.name_range)),
                )
            })
            .unwrap_or(Type::Unknown)
    }

    pub(crate) fn lower_type_ref_with_params_at(
        &mut self,
        ty: &HirTypeRef,
        params: &HashMap<String, Type>,
        span: Option<TextRange>,
    ) -> Type {
        match ty {
            HirTypeRef::Named(path) => self.lower_named_type(path, params, span),
            HirTypeRef::Ref(inner, mutable) => Type::Ref(
                Box::new(self.lower_type_ref_with_params_at(inner, params, span)),
                *mutable,
            ),
            HirTypeRef::Ptr { mutable, inner } => Type::Ptr {
                mutable: *mutable,
                inner: Box::new(self.lower_type_ref_with_params_at(inner, params, span)),
            },
            HirTypeRef::Tuple(elements) if elements.is_empty() => Type::Unit,
            HirTypeRef::Tuple(elements) => Type::Tuple(
                elements
                    .iter()
                    .map(|ty| self.lower_type_ref_with_params_at(ty, params, span))
                    .collect(),
            ),
            HirTypeRef::Array(inner, len) => {
                if let Some(suggestion) = self.swapped_array_type_suggestion(inner, len, params) {
                    self.diagnostic("E0034", "invalid array type syntax", span);
                    if let Some(diagnostic) = self.result.diagnostics.last_mut() {
                        diagnostic.notes.push(format!(
                            "array types use `[T; N]`; write `{suggestion}` instead"
                        ));
                    }
                    return Type::Unknown;
                }
                Type::Array(
                    Box::new(self.lower_type_ref_with_params_at(inner, params, span)),
                    self.lower_const_arg(len, params),
                )
            }
            HirTypeRef::Const(value) => Type::Const(self.lower_const_arg(value, params)),
            HirTypeRef::Function {
                is_unsafe,
                params: fn_params,
                ret,
            } => Type::Fn {
                is_unsafe: *is_unsafe,
                params: fn_params
                    .iter()
                    .map(|param| self.lower_type_ref_with_params_at(param, params, span))
                    .collect(),
                ret: Box::new(self.lower_type_ref_with_params_at(ret, params, span)),
            },
            HirTypeRef::Unknown => Type::Unknown,
            HirTypeRef::Error => Type::Error,
        }
    }

    pub(crate) fn display_type_ref(&mut self, ty: &HirTypeRef) -> String {
        Self::type_text(ty)
    }

    fn type_text(ty: &HirTypeRef) -> String {
        match ty {
            HirTypeRef::Unknown => "_".to_string(),
            HirTypeRef::Error => "<error>".to_string(),
            HirTypeRef::Named(p) => p.display(),
            HirTypeRef::Ref(inner, mutable) => {
                let kw = if *mutable { "&mut " } else { "&" };
                format!("{}{}", kw, Self::type_text(inner))
            }
            HirTypeRef::Ptr { mutable, inner } => {
                let kind = if *mutable { "*mut" } else { "*const" };
                format!("{kind} {}", Self::type_text(inner))
            }
            HirTypeRef::Tuple(elements) => {
                let inner = elements
                    .iter()
                    .map(Self::type_text)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({})", inner)
            }
            HirTypeRef::Array(elem, len) => {
                format!("[{}; {}]", Self::type_text(elem), len.display())
            }
            HirTypeRef::Const(value) => value.display(),
            HirTypeRef::Function {
                is_unsafe,
                params,
                ret,
            } => {
                let params = params
                    .iter()
                    .map(Self::type_text)
                    .collect::<Vec<_>>()
                    .join(", ");
                let prefix = if *is_unsafe { "unsafe " } else { "" };
                format!("{prefix}fun({params}) -> {}", Self::type_text(ret))
            }
        }
    }

    pub(crate) fn resolve_trait_ref(&self, ty: &HirTypeRef) -> Option<TraitId> {
        let HirTypeRef::Named(path) = ty else {
            return None;
        };
        let name = path.segments.last()?.0.as_str();
        self.find_trait_by_name(name)
    }

    fn lower_named_type(
        &mut self,
        path: &HirPath,
        params: &HashMap<String, Type>,
        span: Option<TextRange>,
    ) -> Type {
        if let Some(ty) = self.lower_self_associated_type(path, params) {
            return ty;
        }
        if is_self_associated_path(path) && params.contains_key("Self") {
            return Type::Unknown;
        }

        if let Some(type_alias) = self.find_associated_type_alias(path) {
            return self.lower_type_alias(type_alias);
        }

        let Some(name) = path.as_single_name().map(|name| name.0.as_str()) else {
            self.diagnostic("E0034", format!("unknown type `{}`", path.display()), span);
            return Type::Unknown;
        };

        match name {
            "i8" => Type::Int(IntTy::I8),
            "i16" => Type::Int(IntTy::I16),
            "i32" => Type::Int(IntTy::I32),
            "i64" => Type::Int(IntTy::I64),
            "i128" => Type::Int(IntTy::I128),
            "isize" => Type::Int(IntTy::Isize),
            "u8" => Type::Int(IntTy::U8),
            "u16" => Type::Int(IntTy::U16),
            "u32" => Type::Int(IntTy::U32),
            "u64" => Type::Int(IntTy::U64),
            "u128" => Type::Int(IntTy::U128),
            "usize" => Type::Int(IntTy::Usize),
            "f16" => Type::Float(FloatTy::F16),
            "f32" => Type::Float(FloatTy::F32),
            "f64" => Type::Float(FloatTy::F64),
            "f128" => Type::Float(FloatTy::F128),
            "bool" => Type::Bool,
            "str" => Type::Str,
            "char" => Type::Char,
            _ => {
                if let Some(param_ty) = params.get(name) {
                    param_ty.clone()
                } else if let Some(struct_id) = self.find_struct_by_name(name) {
                    let args = self.lower_type_args(&path.type_args, params, span);
                    self.check_type_arg_count(
                        name,
                        self.hir.item_tree.structs[struct_id].generics.len()
                            + self.hir.item_tree.structs[struct_id].const_generics.len(),
                        args.len(),
                        span,
                    );
                    Type::Struct(struct_id, args)
                } else if let Some(enum_id) = self.find_enum_by_name(name) {
                    let args = self.lower_type_args(&path.type_args, params, span);
                    self.check_type_arg_count(
                        name,
                        self.hir.item_tree.enums[enum_id].generics.len()
                            + self.hir.item_tree.enums[enum_id].const_generics.len(),
                        args.len(),
                        span,
                    );
                    Type::Enum(enum_id, args)
                } else {
                    self.diagnostic("E0034", format!("unknown type `{name}`"), span);
                    Type::Unknown
                }
            }
        }
    }

    fn lower_type_args(
        &mut self,
        args: &[HirTypeRef],
        params: &HashMap<String, Type>,
        span: Option<TextRange>,
    ) -> Vec<Type> {
        args.iter()
            .map(|arg| self.lower_type_ref_with_params_at(arg, params, span))
            .collect()
    }

    fn lower_const_arg(&self, arg: &HirConstArg, params: &HashMap<String, Type>) -> ConstArg {
        match arg {
            HirConstArg::Value(value) => ConstArg::Value(*value),
            HirConstArg::Param(name) => match params.get(&name.0) {
                Some(Type::Const(value)) => value.clone(),
                _ => ConstArg::Param(name.0.clone()),
            },
            HirConstArg::Unknown => ConstArg::Unknown,
            HirConstArg::Error => ConstArg::Error,
        }
    }

    fn swapped_array_type_suggestion(
        &self,
        inner: &HirTypeRef,
        len: &HirConstArg,
        params: &HashMap<String, Type>,
    ) -> Option<String> {
        let HirTypeRef::Const(HirConstArg::Value(value)) = inner else {
            return None;
        };
        let HirConstArg::Param(name) = len else {
            return None;
        };
        self.is_type_name(&name.0, params)
            .then(|| format!("[{}; {}]", name.0, value))
    }

    fn is_type_name(&self, name: &str, params: &HashMap<String, Type>) -> bool {
        if let Some(ty) = params.get(name) {
            return !matches!(ty, Type::Const(_));
        }
        IntTy::parse(name).is_some()
            || FloatTy::parse(name).is_some()
            || matches!(name, "bool" | "str" | "char")
            || self.find_struct_by_name(name).is_some()
            || self.find_enum_by_name(name).is_some()
    }

    fn check_type_arg_count(
        &mut self,
        name: &str,
        expected: usize,
        actual: usize,
        span: Option<TextRange>,
    ) {
        if expected == actual {
            return;
        }
        self.diagnostic(
            "E0032",
            format!("type `{name}` expects {expected} type argument(s), got {actual}"),
            span,
        );
    }

    pub(crate) fn struct_subst(&self, struct_id: StructId, args: &[Type]) -> HashMap<String, Type> {
        let strukt = &self.hir.item_tree.structs[struct_id];
        strukt
            .generics
            .iter()
            .chain(strukt.const_generics.iter())
            .zip(args.iter())
            .map(|(name, ty)| (name.0.clone(), ty.clone()))
            .collect()
    }

    pub(crate) fn impl_subst_from_self_ty(
        &mut self,
        imp: &hir::item_tree::HirImpl,
        actual: &Type,
    ) -> Option<HashMap<String, Type>> {
        let params = generic_param_map_with_consts(
            imp.generics.iter().map(|name| name.0.as_str()),
            imp.const_generics.iter().map(|name| name.0.as_str()),
        );
        let expected =
            self.lower_type_ref_with_params_at(&imp.self_ty, &params, Some(imp.self_ty_range));
        let mut subst = HashMap::new();
        if collect_subst(&expected, actual, &mut subst) {
            for name in imp.generics.iter().chain(imp.const_generics.iter()) {
                subst.entry(name.0.clone()).or_insert_with(|| {
                    if imp.const_generics.contains(name) {
                        Type::Const(ConstArg::Param(name.0.clone()))
                    } else {
                        Type::Param(name.0.clone())
                    }
                });
            }
            Some(subst)
        } else {
            None
        }
    }

    fn find_struct_by_name(&self, name: &str) -> Option<StructId> {
        self.hir
            .item_tree
            .structs
            .iter()
            .find_map(|(id, strukt)| (strukt.name.0 == name).then_some(id))
    }

    pub(crate) fn find_enum_by_name(&self, name: &str) -> Option<EnumId> {
        self.hir
            .item_tree
            .enums
            .iter()
            .find_map(|(id, e)| (e.name.0 == name).then_some(id))
    }

    pub(crate) fn find_trait_by_name(&self, name: &str) -> Option<TraitId> {
        self.hir
            .item_tree
            .traits
            .iter()
            .find_map(|(id, tr)| (tr.name.0 == name).then_some(id))
    }

    fn find_associated_type_alias(&self, path: &HirPath) -> Option<TypeAliasId> {
        if !matches!(path.anchor, hir::item_tree::PathAnchor::Plain) || path.segments.len() != 2 {
            return None;
        }
        let self_ty_name = path.segments[0].0.as_str();
        let alias_name = path.segments[1].0.as_str();

        self.hir.item_tree.impls.iter().find_map(|(_, imp)| {
            let HirTypeRef::Named(self_ty_path) = &imp.self_ty else {
                return None;
            };
            if self_ty_path.as_single_name().map(|name| name.0.as_str()) != Some(self_ty_name) {
                return None;
            }
            imp.type_aliases.iter().find_map(|alias_id| {
                (self.hir.item_tree.type_aliases[*alias_id].name.0 == alias_name)
                    .then_some(*alias_id)
            })
        })
    }

    fn lower_self_associated_type(
        &mut self,
        path: &HirPath,
        params: &HashMap<String, Type>,
    ) -> Option<Type> {
        if !is_self_associated_path(path) {
            return None;
        }
        let self_ty = params.get("Self")?.clone();
        let alias_name = path.segments[1].0.as_str();
        let impls = self
            .hir
            .item_tree
            .impls
            .iter()
            .map(|(_, imp)| imp.clone())
            .collect::<Vec<_>>();

        for imp in impls {
            let Some(mut subst) = self.impl_subst_from_self_ty(&imp, &self_ty) else {
                continue;
            };
            subst.insert("Self".into(), self_ty.clone());
            let Some(alias_id) = imp
                .type_aliases
                .iter()
                .find(|alias_id| self.hir.item_tree.type_aliases[**alias_id].name.0 == alias_name)
            else {
                continue;
            };
            let alias = self.hir.item_tree.type_aliases[*alias_id].clone();
            return Some(
                alias
                    .ty
                    .map(|ty| {
                        self.lower_type_ref_with_params_at(
                            &ty,
                            &subst,
                            alias.ty_range.or(Some(alias.name_range)),
                        )
                    })
                    .unwrap_or(Type::Unknown),
            );
        }

        None
    }
}

pub(crate) fn generic_param_map_with_consts<'a>(
    type_names: impl Iterator<Item = &'a str>,
    const_names: impl Iterator<Item = &'a str>,
) -> HashMap<String, Type> {
    type_names
        .map(|name| (name.to_string(), Type::Param(name.to_string())))
        .chain(const_names.map(|name| {
            (
                name.to_string(),
                Type::Const(ConstArg::Param(name.to_string())),
            )
        }))
        .collect()
}

pub(crate) fn collect_subst(
    expected: &Type,
    actual: &Type,
    subst: &mut HashMap<String, Type>,
) -> bool {
    match expected {
        Type::Param(name) => match subst.get(name) {
            Some(existing) => existing == actual,
            None => {
                subst.insert(name.clone(), actual.clone());
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
            Type::Tuple(actual_elems) if expected_elems.len() == actual_elems.len() => {
                expected_elems
                    .iter()
                    .zip(actual_elems)
                    .all(|(expected, actual)| collect_subst(expected, actual, subst))
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
            Type::Struct(actual_id, actual_args)
                if expected_id == actual_id && expected_args.len() == actual_args.len() =>
            {
                expected_args
                    .iter()
                    .zip(actual_args)
                    .all(|(expected, actual)| collect_subst(expected, actual, subst))
            }
            _ => false,
        },
        Type::Enum(expected_id, expected_args) => match actual {
            Type::Enum(actual_id, actual_args)
                if expected_id == actual_id && expected_args.len() == actual_args.len() =>
            {
                expected_args
                    .iter()
                    .zip(actual_args)
                    .all(|(expected, actual)| collect_subst(expected, actual, subst))
            }
            _ => false,
        },
        _ => expected.is_unknown_like() || actual.is_unknown_like() || expected == actual,
    }
}

fn collect_const_subst(
    expected: &ConstArg,
    actual: &ConstArg,
    subst: &mut HashMap<String, Type>,
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

pub(crate) fn substitute_type(ty: &Type, subst: &HashMap<String, Type>) -> Type {
    match ty {
        Type::Param(name) => subst.get(name).cloned().unwrap_or_else(|| ty.clone()),
        Type::Const(value) => Type::Const(substitute_const(value, subst)),
        Type::Ref(inner, mutable) => Type::Ref(Box::new(substitute_type(inner, subst)), *mutable),
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
        _ => ty.clone(),
    }
}

fn substitute_const(value: &ConstArg, subst: &HashMap<String, Type>) -> ConstArg {
    match value {
        ConstArg::Param(name) => match subst.get(name) {
            Some(Type::Const(value)) => value.clone(),
            _ => value.clone(),
        },
        _ => value.clone(),
    }
}

fn is_self_associated_path(path: &HirPath) -> bool {
    matches!(path.anchor, hir::item_tree::PathAnchor::Plain)
        && path.segments.len() == 2
        && path.segments[0].0 == "Self"
}
