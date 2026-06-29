use body_lower::BodyLower;
use std::collections::HashMap;

use body::{Body, BodyId};
use item_tree::{FunctionId, HirModule, HirUse, ItemTree, ModuleId, TopLevelItem};
use la_arena::Arena;
use lower::{AstLower, Lower};

use ast::{self, Root, support::AstNode};

pub mod body;
pub mod body_lower;
pub mod item_tree;
pub mod lower;
pub mod place;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Name(pub String);

#[derive(Debug)]
pub struct HirFile {
    pub item_tree: ItemTree,
    pub bodies: Arena<Body>,
    pub function_bodies: HashMap<FunctionId, BodyId>,
}

pub fn lower_root(root: Root) -> HirFile {
    let mut hir = HirFile {
        item_tree: ItemTree {
            functions: Arena::new(),
            structs: Arena::new(),
            modules: Arena::new(),
            uses: Arena::new(),
            enums: Arena::new(),
            traits: Arena::new(),
            impls: Arena::new(),
            consts: Arena::new(),
            type_aliases: Arena::new(),
            top_level: vec![],
            extern_function_ids: vec![],
        },
        bodies: Arena::new(),
        function_bodies: HashMap::new(),
    };

    let top = lower_items(&mut hir, root.stmts().collect());
    hir.item_tree.top_level = top;
    hir
}

pub(crate) fn lower_items(hir: &mut HirFile, stmts: Vec<ast::Stmt>) -> Vec<TopLevelItem> {
    let mut items = Vec::new();
    for stmt in stmts {
        match stmt {
            ast::Stmt::FuncDecl(func) => {
                let body_ast = func.body();
                let fid = func.lower(&mut hir.item_tree.functions);
                items.push(TopLevelItem::Function(fid));
                if let Some(block) = body_ast {
                    let body = BodyLower::lower(hir, block);
                    let bid = hir.bodies.alloc(body);
                    hir.function_bodies.insert(fid, bid);
                }
            }

            ast::Stmt::StructDecl(s) => {
                let sid = s.lower(&mut hir.item_tree.structs);
                items.push(TopLevelItem::Struct(sid));
            }

            ast::Stmt::ModDecl(m) => {
                let mid = lower_mod_decl(hir, m);
                items.push(TopLevelItem::Module(mid));
            }

            ast::Stmt::UseDecl(u) => {
                let Some(tree_ast) = u.use_tree() else {
                    continue;
                };
                let tree = tree_ast.lower();
                let attrs = lower::lower_attrs(u.syntax());
                let uid = hir.item_tree.uses.alloc(HirUse { tree, attrs });
                items.push(TopLevelItem::Use(uid));
            }

            ast::Stmt::ExternBlock(block) => {
                for func in block.functions() {
                    let fid = func.lower(&mut hir.item_tree.functions);
                    items.push(TopLevelItem::Function(fid));
                    hir.item_tree.extern_function_ids.push(fid);
                }
            }

            ast::Stmt::ExternFnDecl(decl) => {
                if let Some(func) = decl.func_decl() {
                    let body_ast = func.body();
                    let fid = func.lower(&mut hir.item_tree.functions);
                    items.push(TopLevelItem::Function(fid));
                    hir.item_tree.extern_function_ids.push(fid);
                    if let Some(block) = body_ast {
                        let body = BodyLower::lower(hir, block);
                        let bid = hir.bodies.alloc(body);
                        hir.function_bodies.insert(fid, bid);
                    }
                }
            }

            ast::Stmt::EnumDecl(e) => {
                let eid = e.lower(&mut hir.item_tree.enums);
                items.push(TopLevelItem::Enum(eid));
            }

            ast::Stmt::TraitDecl(t) => {
                let tid = t.lower(&mut hir.item_tree.traits);
                items.push(TopLevelItem::Trait(tid));
            }

            ast::Stmt::ConstDecl(c) => {
                let cid = c.lower(&mut hir.item_tree.consts);
                items.push(TopLevelItem::Const(cid));
            }

            ast::Stmt::TypeAliasDecl(t) => {
                let tid = t.lower(&mut hir.item_tree.type_aliases);
                items.push(TopLevelItem::TypeAlias(tid));
            }

            ast::Stmt::ImplDecl(i) => {
                let iid = lower_impl_decl(hir, i);
                items.push(TopLevelItem::Impl(iid));
            }

            ast::Stmt::VarDecl(_) | ast::Stmt::ReturnStmt(_) | ast::Stmt::ExprStmt(_) => {}
        }
    }
    items
}

/// Lowers `mod foo { ... }` or `mod foo;` into the item tree.
///
/// Both `body_lower` and `lower_items` use this helper so module children are always promoted into
/// the global item tree.
pub(crate) fn lower_mod_decl(hir: &mut HirFile, m: ast::ModDecl) -> ModuleId {
    let name = lower::lower_name(m.name());

    // Allocate a placeholder first so the module has a stable id while lowering children.
    let attrs = lower::lower_attrs(m.syntax());
    let mid = hir.item_tree.modules.alloc(HirModule {
        name,
        items: None,
        attrs,
    });

    if let Some(items_iter) = m.items() {
        let stmts: Vec<ast::Stmt> = items_iter.collect();
        let children = lower_items(hir, stmts);
        hir.item_tree.modules[mid].items = Some(children);
    }
    mid
}

/// Lowers an `impl` block. Methods/consts/type-aliases are allocated into the global arenas; method
/// bodies are lowered like free functions so their references participate in name resolution.
pub(crate) fn lower_impl_decl(hir: &mut HirFile, i: ast::ImplDecl) -> item_tree::ImplId {
    use item_tree::{HirImpl, HirTypeRef};

    let first_ty = i.self_type().map(|t| t.lower());
    let second_ty = i.trait_type().map(|t| t.lower());
    let (self_ty, trait_ty) = if i.has_for() {
        (second_ty.unwrap_or(HirTypeRef::Error), first_ty)
    } else {
        (first_ty.unwrap_or(HirTypeRef::Error), None)
    };
    let generics = lower::lower_generic_params(i.generic_params());

    let mut methods = Vec::new();
    for func in i.methods() {
        let body_ast = func.body();
        let receivers: Vec<_> = func
            .param_list()
            .map(|pl| {
                pl.params()
                    .map(|p| (p.is_self_receiver(), p.is_ref(), p.is_mut()))
                    .collect()
            })
            .unwrap_or_default();
        let fid = func.lower(&mut hir.item_tree.functions);
        apply_self_receiver_types(&mut hir.item_tree.functions[fid], &receivers, &self_ty);
        methods.push(fid);
        if let Some(block) = body_ast {
            let body = BodyLower::lower(hir, block);
            let bid = hir.bodies.alloc(body);
            hir.function_bodies.insert(fid, bid);
        }
    }

    let consts = i
        .consts()
        .map(|c| c.lower(&mut hir.item_tree.consts))
        .collect();
    let type_aliases = i
        .type_aliases()
        .map(|t| t.lower(&mut hir.item_tree.type_aliases))
        .collect();

    hir.item_tree.impls.alloc(HirImpl {
        self_ty,
        trait_ty,
        generics,
        methods,
        consts,
        type_aliases,
        attrs: lower::lower_attrs(i.syntax()),
    })
}

fn apply_self_receiver_types(
    func: &mut item_tree::HirFunction,
    receivers: &[(bool, bool, bool)],
    self_ty: &item_tree::HirTypeRef,
) {
    for (param, (is_self, is_ref, is_mut)) in func.params.iter_mut().zip(receivers) {
        if !*is_self {
            continue;
        }

        param.ty = if *is_ref {
            item_tree::HirTypeRef::Ref(Box::new(self_ty.clone()), *is_mut)
        } else {
            self_ty.clone()
        };
    }
}
