use la_arena::Arena;

use crate::{
    ast::{self},
    hir::{
        Name,
        item_tree::{Param, TypeRef},
    },
};

use super::item_tree::{Function, FunctionId, ItemTree, Struct, StructField, StructId, TopLevelItem};

pub fn lower_item_tree(root: ast::Root) -> ItemTree {
    let mut tree = ItemTree {
        functions: Arena::new(),
        structs: Arena::new(),
        top_level: vec![],
    };

    for stmt in root.stmts() {
        match stmt {
            ast::Stmt::FuncDecl(func) => {
                let id = lower_function(&mut tree.functions, func);
                tree.top_level.push(TopLevelItem::Function(id));
            }
            ast::Stmt::StructDecl(s) => {
                let id = lower_struct(&mut tree.structs, s);
                tree.top_level.push(TopLevelItem::Struct(id));
            }
            _ => {
                todo!()
            }
        }
    }

    tree
}

fn lower_function(arena: &mut Arena<Function>, func: ast::FuncDecl) -> FunctionId {
    let name = func
        .name()
        .map(|t| Name(t.text().to_string()))
        .unwrap_or(Name("<missing>".into()));

    let params = func
        .param_list()
        .map(|pl| {
            pl.params()
                .map(|p| {
                    let pname = p
                        .name()
                        .map(|t| Name(t.text().to_string()))
                        .unwrap_or(Name("<missing>".into()));
                    let ty = p.ty().map(lower_type_ref).unwrap_or(TypeRef::Error);
                    Param { name: pname, ty }
                })
                .collect()
        })
        .unwrap_or_default();

    let ret_type = func.return_type().map(lower_type_ref);
    let has_body = func.body().is_some();

    arena.alloc(Function {
        name,
        params,
        ret_type,
        has_body,
    })
}

fn lower_struct(arena: &mut Arena<Struct>, s: ast::StructDecl) -> StructId {
    let name = s
        .name()
        .map(|t| Name(t.text().to_string()))
        .unwrap_or(Name("<missing>".into()));

    let fields = s
        .field_list()
        .map(|fl| {
            fl.fields()
                .map(|f| {
                    let fname = f
                        .name()
                        .map(|t| Name(t.text().to_string()))
                        .unwrap_or(Name("<missing>".into()));
                    let ty = f.ty().map(lower_type_ref).unwrap_or(TypeRef::Error);
                    StructField { name: fname, ty }
                })
                .collect()
        })
        .unwrap_or_default();

    arena.alloc(Struct { name, fields })
}

fn lower_type_ref(ty: ast::Type) -> TypeRef {
    match ty {
        ast::Type::Named(node) => {
            let text = node
                .name_token()
                .map(|t| Name(t.text().to_string()))
                .unwrap_or(Name("<missing>".into()));
            TypeRef::Named(text)
        },
        ast::Type::Ref(ref_ty) => match ref_ty.inner() {
            Some(inner) => TypeRef::Ref(Box::new(lower_type_ref(inner))),
            None => TypeRef::Error,
        },
    }
}
