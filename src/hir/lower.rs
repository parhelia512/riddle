use la_arena::Arena;

use crate::{
    ast::{FuncDecl, Param, StructDecl, StructField, StructFieldList, Type},
    frontend::syntax_kind::SyntaxToken,
};

use super::{
    Name,
    item_tree::{
        FunctionId, HirFunction, HirParam, HirStruct, HirStructField, HirTypeRef, StructId,
    },
};

pub trait AstLower {
    type Id;
    type Item;

    fn lower(self, arena: &mut Arena<Self::Item>) -> Self::Id;
}

pub trait Lower {
    type Output;

    fn lower(self) -> Self::Output;
}

pub fn lower_name(name: Option<SyntaxToken>) -> Name {
    name.map(|t| Name(t.text().to_string()))
        .unwrap_or(Name("<missing>".into()))
}

impl Lower for Param {
    type Output = HirParam;

    fn lower(self) -> Self::Output {
        let name = lower_name(self.name());
        let ty = self.ty().lower();

        HirParam { name, ty }
    }
}

impl AstLower for FuncDecl {
    type Id = FunctionId;
    type Item = HirFunction;

    fn lower(self, arena: &mut Arena<Self::Item>) -> Self::Id {
        let name = lower_name(self.name());

        let params = self
            .param_list()
            .map(|pl| pl.params().map(|p| p.lower()).collect())
            .unwrap_or_default();

        let ret_type = self.return_type().map(|ty| ty.lower());

        let has_body = self.body().is_some();

        arena.alloc(HirFunction {
            name,
            params,
            ret_type,
            has_body,
        })
    }
}

impl AstLower for StructDecl {
    type Id = StructId;

    type Item = HirStruct;

    fn lower(self, arena: &mut Arena<Self::Item>) -> Self::Id {
        let name = lower_name(self.name());
        let fields = self.field_list().lower();
        arena.alloc(HirStruct { name, fields })
    }
}

impl Lower for Option<StructFieldList> {
    type Output = Vec<HirStructField>;

    fn lower(self) -> Self::Output {
        match self {
            Some(list) => list.fields().map(|f| f.lower()).collect(),
            None => Vec::new(),
        }
    }
}


impl Lower for StructField {
    type Output = HirStructField;

    fn lower(self) -> Self::Output {
        let name = lower_name(self.name());
        let ty = self.ty().lower();
        HirStructField { name, ty }
    }
}

impl Lower for Type {
    type Output = HirTypeRef;

    fn lower(self) -> Self::Output {
        match self {
            Type::Named(node) => HirTypeRef::Named(lower_name(node.name())),

            Type::Ref(ref_ty) => match ref_ty.inner() {
                Some(inner) => HirTypeRef::Ref(Box::new(inner.lower())),
                None => HirTypeRef::Error,
            },
        }
    }
}

impl Lower for Option<Type> {
    type Output = HirTypeRef;

    fn lower(self) -> Self::Output {
        match self {
            Some(t) => t.lower(),
            None => return HirTypeRef::Error,
        }
    }
}
