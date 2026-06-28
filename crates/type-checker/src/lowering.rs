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
        match ty {
            HirTypeRef::Named(path) => self.lower_named_type(path),
            HirTypeRef::Ref(inner, mutable) => Type::Ref(Box::new(self.lower_type_ref(inner)), *mutable),
            HirTypeRef::Ptr { mutable, inner } => Type::Ptr {
                mutable: *mutable,
                inner: Box::new(self.lower_type_ref(inner)),
            },
            HirTypeRef::Tuple(elements) => {
                Type::Tuple(elements.iter().map(|ty| self.lower_type_ref(ty)).collect())
            }
            HirTypeRef::Array(inner) => Type::Array(Box::new(self.lower_type_ref(inner))),
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
                let inner = elements.iter().map(|t| Self::type_text(t)).collect::<Vec<_>>().join(", ");
                format!("({})", inner)
            }
            HirTypeRef::Array(elem) => format!("[{}]", Self::type_text(elem)),
        }
    }

    pub(crate) fn resolve_trait_ref(&self, ty: &HirTypeRef) -> Option<TraitId> {
        let HirTypeRef::Named(path) = ty else {
            return None;
        };
        let name = path.as_single_name()?.0.as_str();
        self.find_trait_by_name(name)
    }

    fn lower_named_type(&mut self, path: &HirPath) -> Type {
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
                if let Some(struct_id) = self.find_struct_by_name(name) {
                    Type::Struct(struct_id)
                } else if let Some(enum_id) = self.find_enum_by_name(name) {
                    Type::Enum(enum_id)
                } else {
                    Type::Unknown
                }
            }
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
}
