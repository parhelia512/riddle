use body_lower::BodyLower;
use std::collections::HashMap;

use body::{Body, BodyId};
use item_tree::{FunctionId, HirModule, HirUse, ItemTree, ModuleId, TopLevelItem};
use la_arena::Arena;
use lower::{AstLower, Lower};

use ast::{self, Root};

pub mod body;
pub mod body_lower;
pub mod item_tree;
pub mod lower;

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
                let uid = hir.item_tree.uses.alloc(HirUse { tree });
                items.push(TopLevelItem::Use(uid));
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
    let mid = hir.item_tree.modules.alloc(HirModule { name, items: None });

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

    let impl_path = i.path().map(|p| p.lower());
    let for_ty = i.trait_type().map(|t| t.lower());
    let (self_ty, trait_ty) = match for_ty {
        Some(self_ty) => (self_ty, impl_path.map(HirTypeRef::Named)),
        None => (
            impl_path
                .map(HirTypeRef::Named)
                .unwrap_or(HirTypeRef::Error),
            None,
        ),
    };
    let generics = i
        .generic_params()
        .map(|g| g.names().map(|t| Name(t.text().to_string())).collect())
        .unwrap_or_default();

    let mut methods = Vec::new();
    for func in i.methods() {
        let body_ast = func.body();
        let fid = func.lower(&mut hir.item_tree.functions);
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
    })
}
