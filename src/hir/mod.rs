use body_lower::BodyLower;
use std::collections::HashMap;

use body::{Body, BodyId};
use item_tree::{FunctionId, ItemTree, TopLevelItem};
use la_arena::Arena;
use lower::AstLower;

use crate::ast::{self, Root};

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
            top_level: vec![],
        },
        bodies: Arena::new(),
        function_bodies: HashMap::new(),
    };

    for stmt in root.stmts() {
        match stmt {
            ast::Stmt::FuncDecl(func) => {
                let body = func.body();

                let function_id = func.lower(&mut hir.item_tree.functions);

                hir.item_tree
                    .top_level
                    .push(TopLevelItem::Function(function_id));

                if let Some(block) = body {
                    let lowered_body = BodyLower::lower(block);
                    let body_id = hir.bodies.alloc(lowered_body);

                    hir.function_bodies.insert(function_id, body_id);
                }
            }

            ast::Stmt::StructDecl(s) => {
                let struct_id = s.lower(&mut hir.item_tree.structs);

                hir.item_tree
                    .top_level
                    .push(TopLevelItem::Struct(struct_id));
            }

            ast::Stmt::VarDecl(_) | ast::Stmt::ReturnStmt(_) | ast::Stmt::ExprStmt(_) => {
                // unsupports
            }
        }
    }

    hir
}
