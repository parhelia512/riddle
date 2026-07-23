use la_arena::Arena;

use ast::{
    self, ExternFnDecl, FuncDecl, Param, StructDecl, StructField, StructFieldList, Type,
    support::{AstNode, trimmed_range},
};
use frontend::syntax_kind::{SyntaxKind, SyntaxToken};

use super::{
    Name,
    item_tree::{
        ConstId, EnumId, FunctionId, HirAssocTypeConstraint, HirAttr, HirConst, HirConstArg,
        HirEnum, HirEnumVariant, HirFunction, HirGenericBound, HirParam, HirPath, HirStruct,
        HirStructField, HirTrait, HirTypeAlias, HirTypeRef, HirUseTree, HirUseTreeKind,
        HirVariantKind, PathAnchor, StructId, TraitId, TypeAliasId, Visibility,
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
        .map(|g| {
            g.params()
                .filter(|param| !param.is_const)
                .map(|param| Name(param.name))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub fn lower_const_generic_params(params: Option<ast::GenericParams>) -> Vec<Name> {
    params
        .map(|g| {
            g.params()
                .filter(|param| param.is_const)
                .map(|param| Name(param.name))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub fn lower_generic_defaults(params: Option<ast::GenericParams>) -> Vec<Option<HirTypeRef>> {
    params
        .map(|g| {
            g.params()
                .filter(|param| !param.is_const)
                .map(|param| param.default.map(Lower::lower))
                .collect()
        })
        .unwrap_or_default()
}

pub fn lower_generic_bounds(
    params: Option<ast::GenericParams>,
    where_clause: Option<ast::WhereClause>,
) -> Vec<HirGenericBound> {
    let mut bounds: Vec<HirGenericBound> = params
        .map(|g| {
            g.params()
                .flat_map(|param| {
                    if param.is_const {
                        return Vec::new().into_iter();
                    }
                    let name = Name(param.name);
                    param
                        .bounds
                        .into_iter()
                        .map(move |bound| {
                            let trait_range = trimmed_range(bound.trait_path.syntax());
                            let mut trait_path = bound.trait_path.lower();
                            trait_path.type_args =
                                bound.type_args.into_iter().map(Lower::lower).collect();
                            let assoc_constraints = bound
                                .assoc_constraints
                                .into_iter()
                                .map(|constraint| {
                                    let range = trimmed_range(constraint.ty.syntax());
                                    HirAssocTypeConstraint {
                                        name: Name(constraint.name),
                                        ty: constraint.ty.lower(),
                                        range,
                                    }
                                })
                                .collect();
                            HirGenericBound {
                                param: name.clone(),
                                target_ty: HirTypeRef::Named(HirPath {
                                    anchor: PathAnchor::Plain,
                                    segments: vec![name.clone()],
                                    type_args: Vec::new(),
                                }),
                                target_range: trait_range,
                                trait_ty: HirTypeRef::Named(trait_path),
                                trait_range,
                                assoc_constraints,
                            }
                        })
                        .collect::<Vec<_>>()
                        .into_iter()
                })
                .collect()
        })
        .unwrap_or_default();

    if let Some(where_clause) = where_clause {
        bounds.extend(where_clause.predicates().flat_map(|predicate| {
            let target_range = trimmed_range(predicate.target_ty.syntax());
            let target_ty = predicate.target_ty.lower();
            let param = generic_bound_param_name(&target_ty);
            predicate.bounds.into_iter().map(move |bound| {
                let trait_range = trimmed_range(bound.trait_path.syntax());
                let mut trait_path = bound.trait_path.lower();
                trait_path.type_args = bound.type_args.into_iter().map(Lower::lower).collect();
                let assoc_constraints = bound
                    .assoc_constraints
                    .into_iter()
                    .map(|constraint| {
                        let range = trimmed_range(constraint.ty.syntax());
                        HirAssocTypeConstraint {
                            name: Name(constraint.name),
                            ty: constraint.ty.lower(),
                            range,
                        }
                    })
                    .collect();
                HirGenericBound {
                    param: param.clone(),
                    target_ty: target_ty.clone(),
                    target_range,
                    trait_ty: HirTypeRef::Named(trait_path),
                    trait_range,
                    assoc_constraints,
                }
            })
        }));
    }

    bounds
}

fn generic_bound_param_name(ty: &HirTypeRef) -> Name {
    match ty {
        HirTypeRef::Named(path)
            if matches!(path.anchor, PathAnchor::Plain)
                && path.segments.len() == 1
                && path.type_args.is_empty() =>
        {
            path.segments[0].clone()
        }
        _ => Name("<where>".into()),
    }
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

pub fn lower_visibility(is_pub: bool) -> Visibility {
    if is_pub {
        Visibility::Public
    } else {
        Visibility::Private
    }
}

impl Lower for Param {
    type Output = HirParam;
    fn lower(self) -> Self::Output {
        let range = trimmed_range(self.syntax());
        let name_token = self.name();
        let name_range = name_token
            .as_ref()
            .map(|token| token.text_range())
            .unwrap_or(range);
        let name = lower_name(name_token);
        let ty_ast = self.ty();
        let ty_range = ty_ast
            .as_ref()
            .map(|ty| trimmed_range(ty.syntax()))
            .unwrap_or(range);
        let ty = ty_ast.lower();
        let attrs = lower_attrs(self.syntax());
        HirParam {
            name,
            name_range,
            ty,
            ty_range,
            attrs,
        }
    }
}

impl AstLower for FuncDecl {
    type Id = FunctionId;
    type Item = HirFunction;
    fn lower(self, arena: &mut Arena<Self::Item>) -> Self::Id {
        let range = trimmed_range(self.syntax());
        let name_token = self.name();
        let name_range = name_token
            .as_ref()
            .map(|token| token.text_range())
            .unwrap_or(range);
        let name = lower_name(name_token);
        let generic_params = self.generic_params();
        let generics = lower_generic_params(generic_params.clone());
        let const_generics = lower_const_generic_params(generic_params.clone());
        let generic_bounds = lower_generic_bounds(generic_params, self.where_clause());
        let params = self
            .param_list()
            .map(|pl| pl.params().map(|p| p.lower()).collect())
            .unwrap_or_default();
        let ret_type_ast = self.return_type();
        let ret_type_range = ret_type_ast.as_ref().map(|ty| trimmed_range(ty.syntax()));
        let ret_type = ret_type_ast.map(|ty| ty.lower());
        let has_body = self.body().is_some();
        let attrs = lower_attrs(self.syntax());
        let visibility = lower_visibility(self.is_pub());
        let is_unsafe = self.is_unsafe();
        arena.alloc(HirFunction {
            name,
            name_range,
            visibility,
            is_unsafe,
            generics,
            const_generics,
            generic_bounds,
            params,
            ret_type,
            ret_type_range,
            has_body,
            attrs,
        })
    }
}

impl AstLower for ExternFnDecl {
    type Id = FunctionId;
    type Item = HirFunction;
    fn lower(self, arena: &mut Arena<Self::Item>) -> Self::Id {
        let explicitly_unsafe = self.is_unsafe();
        let func = self
            .func_decl()
            .expect("ExternFnDecl must contain FuncDecl");
        let is_import = func.body().is_none();
        let id = func.lower(arena);
        arena[id].is_unsafe |= explicitly_unsafe || is_import;
        id
    }
}

impl AstLower for StructDecl {
    type Id = StructId;
    type Item = HirStruct;
    fn lower(self, arena: &mut Arena<Self::Item>) -> Self::Id {
        let name = lower_name(self.name());
        let name_range = self
            .name()
            .map(|name| name.text_range())
            .unwrap_or_else(|| self.syntax().text_range());
        let generic_params = self.generic_params();
        let generics = lower_generic_params(generic_params.clone());
        let const_generics = lower_const_generic_params(generic_params.clone());
        let generic_bounds = lower_generic_bounds(generic_params, self.where_clause());
        let fields = self.field_list().lower();
        let attrs = lower_attrs(self.syntax());
        let visibility = lower_visibility(self.is_pub());
        arena.alloc(HirStruct {
            name,
            visibility,
            name_range,
            generics,
            const_generics,
            generic_bounds,
            fields,
            attrs,
        })
    }
}

impl AstLower for ast::EnumDecl {
    type Id = EnumId;
    type Item = HirEnum;
    fn lower(self, arena: &mut Arena<Self::Item>) -> Self::Id {
        let range = trimmed_range(self.syntax());
        let name_token = self.name();
        let name_range = name_token
            .as_ref()
            .map(|token| token.text_range())
            .unwrap_or(range);
        let name = lower_name(name_token);
        let generic_params = self.generic_params();
        let generics = lower_generic_params(generic_params.clone());
        let const_generics = lower_const_generic_params(generic_params.clone());
        let generic_bounds = lower_generic_bounds(generic_params, self.where_clause());
        let variants = self.variants().map(|v| v.lower()).collect();
        let attrs = lower_attrs(self.syntax());
        let visibility = lower_visibility(self.is_pub());
        arena.alloc(HirEnum {
            name,
            name_range,
            visibility,
            generics,
            const_generics,
            generic_bounds,
            variants,
            attrs,
        })
    }
}

impl Lower for ast::EnumVariant {
    type Output = HirEnumVariant;
    fn lower(self) -> Self::Output {
        let range = trimmed_range(self.syntax());
        let name_token = self.name();
        let name_range = name_token
            .as_ref()
            .map(|token| token.text_range())
            .unwrap_or(range);
        let name = lower_name(name_token);
        let (tuple, mut field_ranges): (Vec<HirTypeRef>, Vec<_>) = self
            .tuple_types()
            .map(|ty| {
                let range = trimmed_range(ty.syntax());
                (ty.lower(), range)
            })
            .unzip();
        let kind = if let Some(field_list) = self.field_list() {
            let fields = Some(field_list).lower();
            field_ranges = fields.iter().map(|field| field.ty_range).collect();
            HirVariantKind::Struct(fields)
        } else if !tuple.is_empty() {
            HirVariantKind::Tuple(tuple)
        } else {
            HirVariantKind::Unit
        };
        let attrs = lower_attrs(self.syntax());
        HirEnumVariant {
            name,
            name_range,
            kind,
            field_ranges,
            attrs,
        }
    }
}

impl AstLower for ast::TraitDecl {
    type Id = TraitId;
    type Item = HirTrait;
    fn lower(self, arena: &mut Arena<Self::Item>) -> Self::Id {
        let name = lower_name(self.name());
        let visibility = lower_visibility(self.is_pub());
        let generic_params = self.generic_params();
        let generics = lower_generic_params(generic_params.clone());
        let generic_defaults = lower_generic_defaults(generic_params.clone());
        let generic_bounds = lower_generic_bounds(generic_params, None);
        let supertraits = self
            .supertraits()
            .into_iter()
            .map(|bound| {
                let trait_range = trimmed_range(bound.trait_path.syntax());
                let mut trait_path = bound.trait_path.lower();
                trait_path.type_args = bound.type_args.into_iter().map(Lower::lower).collect();
                HirGenericBound {
                    param: Name("Self".into()),
                    target_ty: HirTypeRef::Named(HirPath {
                        anchor: PathAnchor::Plain,
                        segments: vec![Name("Self".into())],
                        type_args: Vec::new(),
                    }),
                    target_range: trait_range,
                    trait_ty: HirTypeRef::Named(trait_path),
                    trait_range,
                    assoc_constraints: bound
                        .assoc_constraints
                        .into_iter()
                        .map(|constraint| {
                            let range = trimmed_range(constraint.ty.syntax());
                            HirAssocTypeConstraint {
                                name: Name(constraint.name),
                                ty: constraint.ty.lower(),
                                range,
                            }
                        })
                        .collect(),
                }
            })
            .collect();
        let methods = self
            .methods()
            .map(|m| {
                let method_range = trimmed_range(m.syntax());
                let method_name = m.name();
                let method_name_range = method_name
                    .as_ref()
                    .map(|token| token.text_range())
                    .unwrap_or(method_range);
                let mname = lower_name(method_name);
                let params = m
                    .param_list()
                    .map(|pl| {
                        pl.params()
                            .map(|p| {
                                let is_self = p.is_self_receiver();
                                let is_ref = p.is_ref();
                                let is_mut = p.is_mut();
                                let mut param = p.lower();
                                if is_self {
                                    param.ty = self_receiver_type(is_ref, is_mut);
                                }
                                param
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let ret_type_ast = m.return_type();
                let ret_type_range = ret_type_ast.as_ref().map(|ty| trimmed_range(ty.syntax()));
                let ret_type = ret_type_ast.map(|ty| ty.lower());
                let generic_params = m.generic_params();
                HirFunction {
                    name: mname,
                    name_range: method_name_range,
                    visibility: lower_visibility(m.is_pub()),
                    is_unsafe: m.is_unsafe(),
                    generics: lower_generic_params(generic_params.clone()),
                    const_generics: lower_const_generic_params(generic_params.clone()),
                    generic_bounds: lower_generic_bounds(generic_params, m.where_clause()),
                    params,
                    ret_type,
                    ret_type_range,
                    has_body: m.body().is_some(),
                    attrs: lower_attrs(m.syntax()),
                }
            })
            .collect();
        let type_aliases = self
            .type_aliases()
            .map(|t| {
                let range = trimmed_range(t.syntax());
                let name_token = t.name();
                let name_range = name_token
                    .as_ref()
                    .map(|token| token.text_range())
                    .unwrap_or(range);
                let ty_ast = t.ty();
                let ty_range = ty_ast.as_ref().map(|ty| trimmed_range(ty.syntax()));
                HirTypeAlias {
                    name: lower_name(name_token),
                    name_range,
                    visibility: lower_visibility(t.is_pub()),
                    ty: ty_ast.map(|ty| ty.lower()),
                    ty_range,
                    attrs: lower_attrs(t.syntax()),
                }
            })
            .collect();
        let attrs = lower_attrs(self.syntax());
        arena.alloc(HirTrait {
            name,
            visibility,
            generics,
            generic_defaults,
            generic_bounds,
            supertraits,
            methods,
            default_methods: Vec::new(),
            type_aliases,
            attrs,
        })
    }
}

fn self_receiver_type(is_ref: bool, is_mut: bool) -> HirTypeRef {
    let self_ty = HirTypeRef::Named(HirPath {
        anchor: PathAnchor::Plain,
        segments: vec![Name("Self".into())],
        type_args: Vec::new(),
    });
    if is_ref {
        HirTypeRef::Ref(Box::new(self_ty), is_mut)
    } else {
        self_ty
    }
}

impl AstLower for ast::ConstDecl {
    type Id = ConstId;
    type Item = HirConst;
    fn lower(self, arena: &mut Arena<Self::Item>) -> Self::Id {
        let range = trimmed_range(self.syntax());
        let name_token = self.name();
        let name_range = name_token
            .as_ref()
            .map(|token| token.text_range())
            .unwrap_or(range);
        let name = lower_name(name_token);
        let ty_ast = self.ty();
        let ty_range = ty_ast
            .as_ref()
            .map(|ty| trimmed_range(ty.syntax()))
            .unwrap_or(range);
        let ty = ty_ast.lower();
        let has_value = self.value().is_some();
        let attrs = lower_attrs(self.syntax());
        let visibility = lower_visibility(self.is_pub());
        arena.alloc(HirConst {
            name,
            name_range,
            visibility,
            ty,
            ty_range,
            has_value,
            attrs,
        })
    }
}

impl AstLower for ast::TypeAliasDecl {
    type Id = TypeAliasId;
    type Item = HirTypeAlias;
    fn lower(self, arena: &mut Arena<Self::Item>) -> Self::Id {
        let range = trimmed_range(self.syntax());
        let name_token = self.name();
        let name_range = name_token
            .as_ref()
            .map(|token| token.text_range())
            .unwrap_or(range);
        let name = lower_name(name_token);
        let ty_ast = self.ty();
        let ty_range = ty_ast.as_ref().map(|ty| trimmed_range(ty.syntax()));
        let ty = ty_ast.map(|t| t.lower());
        let attrs = lower_attrs(self.syntax());
        let visibility = lower_visibility(self.is_pub());
        arena.alloc(HirTypeAlias {
            name,
            name_range,
            visibility,
            ty,
            ty_range,
            attrs,
        })
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
        let ty_range = self
            .ty()
            .map(|ty| trimmed_range(ty.syntax()))
            .unwrap_or_else(|| trimmed_range(self.syntax()));
        let attrs = lower_attrs(self.syntax());
        HirStructField {
            name,
            ty,
            ty_range,
            attrs,
        }
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
            Type::Array(arr) => {
                let Some(inner) = arr.element() else {
                    return HirTypeRef::Error;
                };
                let Some(len_expr) = arr.len_expr() else {
                    return HirTypeRef::Error;
                };
                HirTypeRef::Array(Box::new(inner.lower()), lower_const_arg(len_expr))
            }
            Type::Const(value) => value
                .value()
                .map(|value| HirTypeRef::Const(HirConstArg::Value(value)))
                .unwrap_or(HirTypeRef::Error),
            Type::Function(function) => HirTypeRef::Function {
                is_unsafe: function.is_unsafe(),
                params: function.param_types().map(Lower::lower).collect(),
                ret: Box::new(
                    function
                        .return_type()
                        .map(Lower::lower)
                        .unwrap_or_else(|| HirTypeRef::Tuple(Vec::new())),
                ),
            },
        }
    }
}

fn lower_const_arg(expr: ast::Expr) -> HirConstArg {
    match expr {
        ast::Expr::Number(n) => n
            .value()
            .map(|value| HirConstArg::Value(value as usize))
            .unwrap_or(HirConstArg::Error),
        ast::Expr::NameRef(name_ref) => name_ref
            .path()
            .and_then(|path| path.segments().next())
            .and_then(|segment| segment.name_token())
            .map(|name| HirConstArg::Param(Name(name.text().to_string())))
            .unwrap_or(HirConstArg::Error),
        _ => HirConstArg::Error,
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
        let range = trimmed_range(self.syntax());
        let prefix = self.path().lower();
        let kind = if self.is_glob() {
            HirUseTreeKind::Glob
        } else if let Some(list) = self.subtree_list() {
            HirUseTreeKind::List(list.trees().map(|t| t.lower()).collect())
        } else {
            let alias = self.alias().map(|t| Name(t.text().to_string()));
            HirUseTreeKind::Simple { alias }
        };
        HirUseTree {
            prefix,
            kind,
            range,
        }
    }
}
