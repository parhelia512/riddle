mod basic;
mod hir_lowering;
mod impls;
mod imports;
mod incremental;
mod modules;

use ast::{self, support::AstNode};
use frontend::{incremental::IncrementalParser, tree_builder::Parse};
use hir::{
    HirFile,
    body::StmtId,
    item_tree::{FunctionId, ModuleId, StructId, TopLevelItem},
    lower_root,
};
use scope_graph::resolve::resolve_reference;
use scope_graph::{DefRef, Node, NodeId, ScopeGraph, builder::build_scope_graph};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefKind {
    Function,
    Struct,
    Module,
    Local,
    Param,
    UseAlias,
}

fn build(source: &str) -> ScopeGraph {
    build_hir_and_graph(source).1
}

fn build_hir_and_graph(source: &str) -> (HirFile, ScopeGraph) {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(source);
    build_hir_and_graph_from_parse(parse)
}

fn build_hir_and_graph_from_parse(parse: &Parse) -> (HirFile, ScopeGraph) {
    let syntax = parse.syntax();
    let root = ast::Root::cast(syntax.clone()).unwrap();
    let hir = lower_root(root);
    let (sg, _) = build_scope_graph(&hir, &syntax);
    (hir, sg)
}

fn build_incremental_graph(parser: &IncrementalParser) -> (HirFile, ScopeGraph) {
    let parse = parser.current_parse().unwrap();
    build_hir_and_graph_from_parse(parse)
}

fn replace_once(parser: &mut IncrementalParser, needle: &str, replacement: &str) {
    let offset = parser
        .source()
        .find(needle)
        .unwrap_or_else(|| panic!("missing edit target `{needle}` in:\n{}", parser.source()));
    parser.apply_edit(offset, needle.len(), replacement);
    assert!(
        matches!(
            parser.last_reparse_mode(),
            frontend::incremental::ReparseMode::Incremental(_)
        ),
        "expected incremental reparse after replacing `{needle}` with `{replacement}`, got {:?}",
        parser.last_reparse_mode()
    );
}

fn resolve_paths(sg: &ScopeGraph, path: &str) -> Vec<Vec<DefKind>> {
    sg.nodes
        .iter()
        .filter_map(|(nid, node)| {
            let Node::Reference { segments, .. } = node else {
                return None;
            };
            let path_text = segments
                .iter()
                .map(|name| name.0.as_str())
                .collect::<Vec<_>>()
                .join("::");
            if path_text != path {
                return None;
            }

            Some(
                resolve_reference(sg, nid)
                    .iter()
                    .map(def_kind)
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

fn reference_node(sg: &ScopeGraph, path: &str) -> Option<NodeId> {
    sg.nodes.iter().find_map(|(nid, node)| {
        let Node::Reference { segments, .. } = node else {
            return None;
        };
        let path_text = segments
            .iter()
            .map(|name| name.0.as_str())
            .collect::<Vec<_>>()
            .join("::");
        (path_text == path).then_some(nid)
    })
}

fn resolved_struct_id(sg: &ScopeGraph, path: &str) -> StructId {
    let nid = reference_node(sg, path).unwrap();
    resolve_reference(sg, nid)
        .into_iter()
        .find_map(|def| match def {
            DefRef::Struct(id) => Some(id),
            _ => None,
        })
        .unwrap()
}

fn top_level_struct_id(hir: &HirFile, name: &str) -> StructId {
    hir.item_tree
        .top_level
        .iter()
        .find_map(|item| match item {
            TopLevelItem::Struct(sid) if hir.item_tree.structs[*sid].name.0 == name => Some(*sid),
            _ => None,
        })
        .unwrap()
}

fn module_id_by_name(hir: &HirFile, name: &str) -> ModuleId {
    hir.item_tree
        .modules
        .iter()
        .find_map(|(mid, module)| (module.name.0 == name).then_some(mid))
        .unwrap()
}

fn child_module_id(hir: &HirFile, parent: ModuleId, name: &str) -> ModuleId {
    hir.item_tree.modules[parent]
        .items
        .as_ref()
        .and_then(|items| {
            items.iter().find_map(|item| match item {
                TopLevelItem::Module(mid) if hir.item_tree.modules[*mid].name.0 == name => {
                    Some(*mid)
                }
                _ => None,
            })
        })
        .unwrap()
}

fn struct_id_in_module(hir: &HirFile, module: ModuleId, name: &str) -> StructId {
    hir.item_tree.modules[module]
        .items
        .as_ref()
        .and_then(|items| {
            items.iter().find_map(|item| match item {
                TopLevelItem::Struct(sid) if hir.item_tree.structs[*sid].name.0 == name => {
                    Some(*sid)
                }
                _ => None,
            })
        })
        .unwrap()
}

fn def_kind(def: &DefRef) -> DefKind {
    match def {
        DefRef::Function(_) => DefKind::Function,
        DefRef::Struct(_) => DefKind::Struct,
        DefRef::Enum(_) => DefKind::Struct, // reuse for simplicity
        DefRef::Trait(_) => DefKind::Struct,
        DefRef::Const(_) => DefKind::Struct,
        DefRef::TypeAlias(_) => DefKind::Struct,
        DefRef::Module { .. } => DefKind::Module,
        DefRef::Local { .. } => DefKind::Local,
        DefRef::Param { .. } => DefKind::Param,
        DefRef::PatternBinding { .. } => DefKind::Local,
        DefRef::UseAlias { .. } => DefKind::UseAlias,
        DefRef::EnumVariant { .. } => DefKind::Struct,
    }
}

fn local_stmt(defs: &[DefRef]) -> Option<StmtId> {
    defs.iter().find_map(|def| match def {
        DefRef::Local { stmt } => Some(*stmt),
        _ => None,
    })
}

fn param_fn(defs: &[DefRef]) -> Option<FunctionId> {
    defs.iter().find_map(|def| match def {
        DefRef::Param { fn_id, .. } => Some(*fn_id),
        _ => None,
    })
}
