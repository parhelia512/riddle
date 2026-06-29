use std::collections::HashMap;

use hir::item_tree::{EnumId, HirPath, HirTypeRef, StructId, TraitId, TypeAliasId};

use crate::{
    checker::TypeChecker,
    types::{FloatTy, IntTy, Type},
};

impl TypeChecker<'_> {
    pub(crate) fn lower_type_alias(&mut self, type_alias: TypeAliasId) -> Type {
        self.hir.item_tree.type_aliases[type_alias]
            .ty
            .as_ref()
            .map(|ty| self.lower_type_ref(ty))
            .unwrap_or(Type::Unknown)
    }

    pub(crate) fn lower_type_ref(&mut self, ty: &HirTypeRef) -> Type {
        self.lower_type_ref_with_params(ty, &HashMap::new())
    }

    pub(crate) fn lower_type_ref_with_params(
        &mut self,
        ty: &HirTypeRef,
        params: &HashMap<String, Type>,
    ) -> Type {
        match ty {
            HirTypeRef::Named(path) => self.lower_named_type(path, params),
            HirTypeRef::Ref(inner, mutable) => Type::Ref(
                Box::new(self.lower_type_ref_with_params(inner, params)),
                *mutable,
            ),
            HirTypeRef::Ptr { mutable, inner } => Type::Ptr {
                mutable: *mutable,
                inner: Box::new(self.lower_type_ref_with_params(inner, params)),
            },
            HirTypeRef::Tuple(elements) => Type::Tuple(
                elements
                    .iter()
                    .map(|ty| self.lower_type_ref_with_params(ty, params))
                    .collect(),
            ),
            HirTypeRef::Array(inner, len) => Type::Array(
                Box::new(self.lower_type_ref_with_params(inner, params)),
                *len,
            ),
            HirTypeRef::Unknown => Type::Unknown,
            HirTypeRef::Error => Type::Error,
        }
    }

    pub(crate) fn display_type_ref(&mut self, ty: &HirTypeRef) -> String {
        let lowered = self.lower_type_ref(ty);
        if !lowered.is_unknown_like() {
            return lowered.display(self.hir);
        }

        match ty {
            HirTypeRef::Named(path) => path.display(),
            HirTypeRef::Ptr { mutable, inner } => {
                let kind = if *mutable { "*mut" } else { "*const" };
                format!("{kind} {}", Self::type_text(inner))
            }
            _ => lowered.display(self.hir),
        }
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
                    .map(|t| Self::type_text(t))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({})", inner)
            }
            HirTypeRef::Array(elem, len) => format!("[{}; {}]", Self::type_text(elem), len),
        }
    }

    pub(crate) fn resolve_trait_ref(&self, ty: &HirTypeRef) -> Option<TraitId> {
        let HirTypeRef::Named(path) = ty else {
            return None;
        };
        let name = path.segments.last()?.0.as_str();
        self.find_trait_by_name(name)
    }

    fn lower_named_type(&mut self, path: &HirPath, params: &HashMap<String, Type>) -> Type {
        if let Some(type_alias) = self.find_associated_type_alias(path) {
            return self.lower_type_alias(type_alias);
        }

        let Some(name) = path.as_single_name().map(|name| name.0.as_str()) else {
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
            "unit" => Type::Unit,
            _ => {
                if let Some(param_ty) = params.get(name) {
                    param_ty.clone()
                } else if let Some(struct_id) = self.find_struct_by_name(name) {
                    let args = self.lower_type_args(&path.type_args, params);
                    self.check_type_arg_count(
                        name,
                        self.hir.item_tree.structs[struct_id].generics.len(),
                        args.len(),
                    );
                    Type::Struct(struct_id, args)
                } else if let Some(enum_id) = self.find_enum_by_name(name) {
                    let args = self.lower_type_args(&path.type_args, params);
                    self.check_type_arg_count(
                        name,
                        self.hir.item_tree.enums[enum_id].generics.len(),
                        args.len(),
                    );
                    Type::Enum(enum_id, args)
                } else {
                    Type::Unknown
                }
            }
        }
    }

    fn lower_type_args(
        &mut self,
        args: &[HirTypeRef],
        params: &HashMap<String, Type>,
    ) -> Vec<Type> {
        args.iter()
            .map(|arg| self.lower_type_ref_with_params(arg, params))
            .collect()
    }

    fn check_type_arg_count(&mut self, name: &str, expected: usize, actual: usize) {
        if expected == actual {
            return;
        }
        self.diagnostic(
            "E0032",
            format!("type `{name}` expects {expected} type argument(s), got {actual}"),
            None,
        );
    }

    pub(crate) fn struct_subst(&self, struct_id: StructId, args: &[Type]) -> HashMap<String, Type> {
        self.hir.item_tree.structs[struct_id]
            .generics
            .iter()
            .zip(args.iter())
            .map(|(name, ty)| (name.0.clone(), ty.clone()))
            .collect()
    }

    pub(crate) fn impl_subst_from_self_ty(
        &mut self,
        imp: &hir::item_tree::HirImpl,
        actual: &Type,
    ) -> Option<HashMap<String, Type>> {
        let params = generic_param_map(imp.generics.iter().map(|name| name.0.as_str()));
        let expected = self.lower_type_ref_with_params(&imp.self_ty, &params);
        let mut subst = HashMap::new();
        if collect_subst(&expected, actual, &mut subst) {
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

    fn find_enum_by_name(&self, name: &str) -> Option<EnumId> {
        self.hir
            .item_tree
            .enums
            .iter()
            .find_map(|(id, e)| (e.name.0 == name).then_some(id))
    }

    fn find_trait_by_name(&self, name: &str) -> Option<TraitId> {
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
}

pub(crate) fn generic_param_map<'a>(names: impl Iterator<Item = &'a str>) -> HashMap<String, Type> {
    names
        .map(|name| (name.to_string(), Type::Param(name.to_string())))
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
                expected_len == actual_len && collect_subst(expected_inner, actual_inner, subst)
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
