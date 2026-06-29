use la_arena::Arena;

use ast::{
    self, ExternFnDecl, FuncDecl, Param, StructDecl, StructField, StructFieldList, Type,
    support::AstNode,
};
use frontend::syntax_kind::{SyntaxKind, SyntaxToken};

use super::{
    Name,
    item_tree::{
        ConstId, EnumId, FunctionId, HirAttr, HirConst, HirEnum, HirEnumVariant, HirFunction,
        HirParam, HirPath, HirStruct, HirStructField, HirTrait, HirTypeAlias, HirTypeRef,
        HirUseTree, HirUseTreeKind, HirVariantKind, PathAnchor, StructId, TraitId, TypeAliasId,
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

pub fn lower_generic_params(params: Option<ast::GenericParams>) -> Vec<Name> {
    params
        .map(|g| g.names().map(|t| Name(t.text().to_string())).collect())
        .unwrap_or_default()
}

pub fn lower_attrs(node: &frontend::syntax_kind::SyntaxNode) -> Vec<HirAttr> {
    ast::attrs_for_node(node)
        .into_iter()
        .map(|attr| HirAttr {
            name: lower_name(attr.name()),
            value: attr.string_value(),
            raw: attr.raw_text(),
        })
        .collect()
}

impl Lower for Param {
    type Output = HirParam;
    fn lower(self) -> Self::Output {
        let name = lower_name(self.name());
        let ty = self.ty().lower();
        let attrs = lower_attrs(self.syntax());
        HirParam { name, ty, attrs }
    }
}

impl AstLower for FuncDecl {
    type Id = FunctionId;
    type Item = HirFunction;
    fn lower(self, arena: &mut Arena<Self::Item>) -> Self::Id {
        let name = lower_name(self.name());
        let generics = lower_generic_params(self.generic_params());
        let params = self
            .param_list()
            .map(|pl| pl.params().map(|p| p.lower()).collect())
            .unwrap_or_default();
        let ret_type = self.return_type().map(|ty| ty.lower());
        let has_body = self.body().is_some();
        let attrs = lower_attrs(self.syntax());
        arena.alloc(HirFunction {
            name,
            generics,
            params,
            ret_type,
            has_body,
            attrs,
        })
    }
}

impl AstLower for ExternFnDecl {
    type Id = FunctionId;
    type Item = HirFunction;
    fn lower(self, arena: &mut Arena<Self::Item>) -> Self::Id {
        self.func_decl()
            .expect("ExternFnDecl must contain FuncDecl")
            .lower(arena)
    }
}

impl AstLower for StructDecl {
    type Id = StructId;
    type Item = HirStruct;
    fn lower(self, arena: &mut Arena<Self::Item>) -> Self::Id {
        let name = lower_name(self.name());
        let generics = lower_generic_params(self.generic_params());
        let fields = self.field_list().lower();
        let attrs = lower_attrs(self.syntax());
        arena.alloc(HirStruct {
            name,
            generics,
            fields,
            attrs,
        })
    }
}

impl AstLower for ast::EnumDecl {
    type Id = EnumId;
    type Item = HirEnum;
    fn lower(self, arena: &mut Arena<Self::Item>) -> Self::Id {
        let name = lower_name(self.name());
        let generics = lower_generic_params(self.generic_params());
        let variants = self.variants().map(|v| v.lower()).collect();
        let attrs = lower_attrs(self.syntax());
        arena.alloc(HirEnum {
            name,
            generics,
            variants,
            attrs,
        })
    }
}

impl Lower for ast::EnumVariant {
    type Output = HirEnumVariant;
    fn lower(self) -> Self::Output {
        let name = lower_name(self.name());
        let tuple: Vec<HirTypeRef> = self.tuple_types().map(|t| t.lower()).collect();
        let kind = if let Some(field_list) = self.field_list() {
            HirVariantKind::Struct(Some(field_list).lower())
        } else if !tuple.is_empty() {
            HirVariantKind::Tuple(tuple)
        } else {
            HirVariantKind::Unit
        };
        let attrs = lower_attrs(self.syntax());
        HirEnumVariant { name, kind, attrs }
    }
}

impl AstLower for ast::TraitDecl {
    type Id = TraitId;
    type Item = HirTrait;
    fn lower(self, arena: &mut Arena<Self::Item>) -> Self::Id {
        let name = lower_name(self.name());
        let methods = self
            .methods()
            .map(|m| {
                let mname = lower_name(m.name());
                let params = m
                    .param_list()
                    .map(|pl| pl.params().map(|p| p.lower()).collect())
                    .unwrap_or_default();
                let ret_type = m.return_type().map(|ty| ty.lower());
                HirFunction {
                    name: mname,
                    generics: lower_generic_params(m.generic_params()),
                    params,
                    ret_type,
                    has_body: m.body().is_some(),
                    attrs: lower_attrs(m.syntax()),
                }
            })
            .collect();
        let type_aliases = self
            .type_aliases()
            .map(|t| HirTypeAlias {
                name: lower_name(t.name()),
                ty: t.ty().map(|ty| ty.lower()),
                attrs: lower_attrs(t.syntax()),
            })
            .collect();
        let attrs = lower_attrs(self.syntax());
        arena.alloc(HirTrait {
            name,
            methods,
            type_aliases,
            attrs,
        })
    }
}

impl AstLower for ast::ConstDecl {
    type Id = ConstId;
    type Item = HirConst;
    fn lower(self, arena: &mut Arena<Self::Item>) -> Self::Id {
        let name = lower_name(self.name());
        let ty = self.ty().lower();
        let has_value = self.value().is_some();
        let attrs = lower_attrs(self.syntax());
        arena.alloc(HirConst {
            name,
            ty,
            has_value,
            attrs,
        })
    }
}

impl AstLower for ast::TypeAliasDecl {
    type Id = TypeAliasId;
    type Item = HirTypeAlias;
    fn lower(self, arena: &mut Arena<Self::Item>) -> Self::Id {
        let name = lower_name(self.name());
        let ty = self.ty().map(|t| t.lower());
        let attrs = lower_attrs(self.syntax());
        arena.alloc(HirTypeAlias { name, ty, attrs })
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
        let attrs = lower_attrs(self.syntax());
        HirStructField { name, ty, attrs }
    }
}

impl Lower for Type {
    type Output = HirTypeRef;
    fn lower(self) -> Self::Output {
        match self {
            Type::Named(node) => {
                let mut path = node.path().lower();
                path.type_args = node.type_args().into_iter().map(|ty| ty.lower()).collect();
                HirTypeRef::Named(path)
            }
            Type::Ref(ref_ty) => match ref_ty.inner() {
                Some(inner) => HirTypeRef::Ref(Box::new(inner.lower()), ref_ty.is_mut()),
                None => HirTypeRef::Error,
            },
            Type::Ptr(ptr_ty) => match ptr_ty.inner() {
                Some(inner) => HirTypeRef::Ptr {
                    mutable: ptr_ty.is_mut(),
                    inner: Box::new(inner.lower()),
                },
                None => HirTypeRef::Error,
            },
            Type::Tuple(tuple) => HirTypeRef::Tuple(tuple.elements().map(|t| t.lower()).collect()),
            Type::Array(arr) => match arr.element() {
                Some(inner) => {
                    let len = arr
                        .len_expr()
                        .and_then(|e| match e {
                            ast::Expr::Number(n) => n.value().map(|v| v as usize),
                            _ => None,
                        })
                        .unwrap_or(0);
                    HirTypeRef::Array(Box::new(inner.lower()), len)
                }
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
            None => HirTypeRef::Error,
        }
    }
}

impl Lower for ast::Path {
    type Output = HirPath;
    fn lower(self) -> Self::Output {
        let absolute = self.is_absolute();
        let mut segs: Vec<(SyntaxKind, String)> = self
            .segments()
            .filter_map(|seg| {
                let t = seg.name_token()?;
                Some((t.kind(), t.text().to_string()))
            })
            .collect();

        let anchor = if absolute {
            PathAnchor::Absolute
        } else {
            match segs.first().map(|(k, text)| (*k, text.as_str())) {
                Some((SyntaxKind::CrateKw, _)) | Some((_, "crate")) => {
                    segs.remove(0);
                    PathAnchor::Crate
                }
                Some((SyntaxKind::SuperKw, _)) | Some((_, "super")) => {
                    segs.remove(0);
                    PathAnchor::Super
                }
                Some((SyntaxKind::SelfKw, _)) | Some((_, "self")) if segs.len() > 1 => {
                    segs.remove(0);
                    PathAnchor::SelfMod
                }
                _ => PathAnchor::Plain,
            }
        };

        let segments = segs.into_iter().map(|(_, t)| Name(t)).collect();
        HirPath {
            anchor,
            segments,
            type_args: Vec::new(),
        }
    }
}

impl Lower for Option<ast::Path> {
    type Output = HirPath;
    fn lower(self) -> Self::Output {
        self.map(|p| p.lower()).unwrap_or(HirPath {
            anchor: PathAnchor::Plain,
            segments: vec![Name("<missing>".into())],
            type_args: Vec::new(),
        })
    }
}

impl Lower for ast::UseTree {
    type Output = HirUseTree;
    fn lower(self) -> Self::Output {
        let prefix = self.path().lower();
        let kind = if self.is_glob() {
            HirUseTreeKind::Glob
        } else if let Some(list) = self.subtree_list() {
            HirUseTreeKind::List(list.trees().map(|t| t.lower()).collect())
        } else {
            let alias = self.alias().map(|t| Name(t.text().to_string()));
            HirUseTreeKind::Simple { alias }
        };
        HirUseTree { prefix, kind }
    }
}
