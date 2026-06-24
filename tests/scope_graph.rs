use ast::{self, support::AstNode};
use frontend::{incremental::IncrementalParser, tree_builder::Parse};
use hir::{
    HirFile,
    body::{BinaryOp, Expr, ResolvedName, StmtId},
    item_tree::{FunctionId, ModuleId, StructId, TopLevelItem},
    lower_root,
};
use scope_graph::{DefRef, EdgeKind, Node, NodeId, ScopeGraph, builder::build_scope_graph};

use scope_graph::resolve::{resolve_hir, resolve_reference};

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
    let sg = build_scope_graph(&hir, &syntax);
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

#[test]
fn resolves_param_then_local_in_statement_order() {
    let sg = build(
        r#"
        fun f(x: int) {
            let y = x;
            y
        }
        "#,
    );

    assert_eq!(resolve_paths(&sg, "x"), vec![vec![DefKind::Param]]);
    assert_eq!(resolve_paths(&sg, "y"), vec![vec![DefKind::Local]]);
}

#[test]
fn local_shadows_param() {
    let sg = build(
        r#"
        fun f(x: int) {
            let x = 1;
            x
        }
        "#,
    );

    assert_eq!(resolve_paths(&sg, "x"), vec![vec![DefKind::Local]]);
}

#[test]
fn let_initializer_does_not_see_its_own_binding() {
    let sg = build(
        r#"
        fun f(x: int) {
            let x = x;
        }
        "#,
    );

    assert_eq!(resolve_paths(&sg, "x"), vec![vec![DefKind::Param]]);
}

#[test]
fn use_alias_rewrites_to_target_path() {
    let sg = build(
        r#"
        mod m {
            struct S {}
        }

        use crate::m::S as T;

        fun f() {
            T
        }
        "#,
    );

    assert_eq!(resolve_paths(&sg, "T"), vec![vec![DefKind::Struct]]);
}

#[test]
fn glob_import_resolves_modules_step_by_step() {
    let sg = build(
        r#"
        mod b {
            mod target {
                struct B {}
            }
        }

        mod a {
            mod target {
                struct A {}
            }
        }

        use crate::a::target::*;

        fun f() {
            A;
            B
        }
        "#,
    );

    assert_eq!(resolve_paths(&sg, "A"), vec![vec![DefKind::Struct]]);
    assert_eq!(resolve_paths(&sg, "B"), vec![vec![]]);
}

#[test]
fn unresolved_glob_prefix_does_not_import_parent_module() {
    let sg = build(
        r#"
        mod a {
            struct S {}
        }

        use crate::a::missing::*;

        fun f() {
            S
        }
        "#,
    );

    assert_eq!(resolve_paths(&sg, "S"), vec![vec![]]);
}

#[test]
fn shadowing_stops_before_outer_scope_even_when_remaining_path_fails() {
    let sg = build(
        r#"
        mod x {
            struct S {}
        }

        fun f(x: int) {
            x::S
        }
        "#,
    );

    assert_eq!(resolve_paths(&sg, "x::S"), vec![vec![]]);
}

#[test]
fn plain_paths_climb_out_of_inner_modules() {
    let sg = build(
        r#"
        mod outer {
            struct S {}

            mod inner {
                fun f() {
                    S
                }
            }
        }
        "#,
    );

    assert_eq!(resolve_paths(&sg, "S"), vec![vec![DefKind::Struct]]);
}

#[test]
fn self_super_and_crate_aliases_bind_to_the_right_structs() {
    let (hir, sg) = build_hir_and_graph(
        r#"
        struct S {}

        mod outer {
            struct S {}

            mod inner {
                struct S {}

                use self::S as LocalS;
                use super::S as OuterS;
                use crate::S as CrateS;

                fun f() {
                    LocalS;
                    OuterS;
                    CrateS;
                }
            }
        }
        "#,
    );

    let top_sid = top_level_struct_id(&hir, "S");
    let outer_mid = module_id_by_name(&hir, "outer");
    let inner_mid = child_module_id(&hir, outer_mid, "inner");
    let outer_sid = struct_id_in_module(&hir, outer_mid, "S");
    let inner_sid = struct_id_in_module(&hir, inner_mid, "S");

    assert_eq!(resolved_struct_id(&sg, "LocalS"), inner_sid);
    assert_eq!(resolved_struct_id(&sg, "OuterS"), outer_sid);
    assert_eq!(resolved_struct_id(&sg, "CrateS"), top_sid);
}

#[test]
fn multi_segment_reference_uses_reverse_push_chain() {
    let sg = build(
        r#"
        mod a {
            mod b {
                struct C {}
            }
        }

        fun f() {
            crate::a::b::C
        }
        "#,
    );

    let nid = reference_node(&sg, "a::b::C").unwrap();
    let mut current = nid;
    let mut chain = Vec::new();

    loop {
        let next_edge = sg
            .out_edges
            .get(&current)
            .and_then(|edges| {
                edges
                    .iter()
                    .copied()
                    .find(|eid| sg.edges[*eid].kind == EdgeKind::Lex)
            })
            .expect("reference chain is missing a lexical edge");
        current = sg.edges[next_edge].to;
        match &sg.nodes[current] {
            Node::PushSymbol { name } => chain.push(name.0.clone()),
            Node::Scope(_) => break,
            other => panic!("unexpected node in path chain: {:?}", other),
        }
    }

    assert_eq!(chain, vec!["C", "b", "a"]);
    assert_eq!(current, sg.root);
    assert_eq!(resolve_paths(&sg, "a::b::C"), vec![vec![DefKind::Struct]]);
}

#[test]
fn let_bindings_are_distinct_across_statement_chain() {
    let sg = build(
        r#"
        fun f(a: int) {
            let x = a;
            let y = x;
            let x = y;
            x
        }
        "#,
    );

    let refs = sg
        .nodes
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
            Some((path_text, resolve_reference(&sg, nid)))
        })
        .collect::<Vec<_>>();

    let y_init_x = refs
        .iter()
        .find(|(path, defs)| path == "x" && local_stmt(defs).is_some())
        .and_then(|(_, defs)| local_stmt(defs))
        .unwrap();

    let tail_x = refs
        .iter()
        .rev()
        .find(|(path, defs)| path == "x" && local_stmt(defs).is_some())
        .and_then(|(_, defs)| local_stmt(defs))
        .unwrap();

    assert_ne!(y_init_x, tail_x);
    assert!(
        refs.iter()
            .any(|(path, defs)| path == "a" && param_fn(defs).is_some())
    );
    assert!(
        refs.iter()
            .any(|(path, defs)| path == "y" && local_stmt(defs).is_some())
    );
}

#[test]
fn resolve_hir_updates_expr_path_resolutions() {
    let (mut hir, sg) = build_hir_and_graph(
        r#"
        mod m {
            struct S {}
        }

        use crate::m::S as T;

        fun f(x: int) {
            let y = x;
            T
        }
        "#,
    );

    resolve_hir(&mut hir, &sg);

    let body_id = *hir.function_bodies.values().next().unwrap();
    let body = &hir.bodies[body_id];
    let resolved_paths = body
        .exprs
        .iter()
        .filter_map(|(_, expr)| match expr {
            Expr::Path { path, resolved } => Some((path.display(), resolved.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(resolved_paths
        .iter()
        .any(|(path, resolved)| path == "x" && matches!(resolved, Some(ResolvedName::Param(0)))));
    assert!(
        resolved_paths.iter().any(
            |(path, resolved)| path == "T" && matches!(resolved, Some(ResolvedName::Struct(_)))
        )
    );
}

#[test]
fn assignment_and_struct_literal_parse_and_lower() {
    let source = r#"
        struct Foo {
            x: int,
            y: int,
        }

        fun f() {
            let t: Foo;
            let x = Foo{x: 1, y: 1};
            t = x;
        }
        "#;

    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(source);
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);

    let syntax = parse.syntax();
    let root = ast::Root::cast(syntax.clone()).unwrap();
    let mut hir = lower_root(root);
    let sg = build_scope_graph(&hir, &syntax);
    resolve_hir(&mut hir, &sg);

    let body_id = *hir.function_bodies.values().next().unwrap();
    let body = &hir.bodies[body_id];

    assert!(body.exprs.iter().any(|(_, expr)| matches!(
        expr,
        Expr::Binary {
            op: BinaryOp::Assign,
            ..
        }
    )));

    assert!(body.exprs.iter().any(|(_, expr)| matches!(
        expr,
        Expr::Struct {
            path,
            fields,
            resolved: Some(ResolvedName::Struct(_)),
        } if path.display() == "Foo" && fields.len() == 2
    )));
}

#[test]
fn impl_method_body_references_are_encoded() {
    let sg = build(
        r#"
        struct S {}

        impl S {
            fun value(arg: i32) {
                arg;
            }
        }
        "#,
    );

    assert_eq!(resolve_paths(&sg, "arg"), vec![vec![DefKind::Param]]);
}

#[test]
fn associated_function_path_resolves_through_impl_scope() {
    let sg = build(
        r#"
        struct Point {}

        impl Point {
            fun new() -> Point {
                Point{}
            }
        }

        fun main() {
            Point::new();
        }
        "#,
    );

    assert_eq!(
        resolve_paths(&sg, "Point::new"),
        vec![vec![DefKind::Function]]
    );
}

#[test]
fn incremental_impl_method_body_rebuilds_resolution() {
    let mut parser = IncrementalParser::new();
    parser.set_source(
        r#"
        struct S {}

        impl S {
            fun value(arg: i32) {
                arg;
            }
        }
        "#,
    );

    let (_, sg) = build_incremental_graph(&parser);
    assert_eq!(resolve_paths(&sg, "arg"), vec![vec![DefKind::Param]]);

    let offset = parser.source().rfind("arg").unwrap();
    parser.apply_edit(offset, "arg".len(), "bad");
    assert!(
        matches!(
            parser.last_reparse_mode(),
            frontend::incremental::ReparseMode::Incremental(_)
        ),
        "expected incremental reparse after replacing body `arg`, got {:?}",
        parser.last_reparse_mode()
    );
    let (_, sg) = build_incremental_graph(&parser);
    assert_eq!(resolve_paths(&sg, "arg"), Vec::<Vec<DefKind>>::new());
    assert_eq!(resolve_paths(&sg, "bad"), vec![vec![]]);
}

#[test]
fn incremental_multi_edit_local_shadowing_rebuilds_statement_chain() {
    let mut parser = IncrementalParser::new();
    parser.set_source(
        r#"
        fun f(value: int) {
            let left = value;
            left;
        }
        "#,
    );

    let (_, sg) = build_incremental_graph(&parser);
    assert_eq!(resolve_paths(&sg, "value"), vec![vec![DefKind::Param]]);
    assert_eq!(resolve_paths(&sg, "left"), vec![vec![DefKind::Local]]);

    replace_once(&mut parser, "left", "right");
    let (_, sg) = build_incremental_graph(&parser);
    assert_eq!(resolve_paths(&sg, "value"), vec![vec![DefKind::Param]]);
    assert_eq!(resolve_paths(&sg, "left"), vec![vec![]]);
    assert_eq!(resolve_paths(&sg, "right"), Vec::<Vec<DefKind>>::new());

    replace_once(&mut parser, "left", "right");
    let (_, sg) = build_incremental_graph(&parser);
    assert_eq!(resolve_paths(&sg, "value"), vec![vec![DefKind::Param]]);
    assert_eq!(resolve_paths(&sg, "right"), vec![vec![DefKind::Local]]);

    replace_once(&mut parser, "right", "left");
    let (_, sg) = build_incremental_graph(&parser);
    assert_eq!(resolve_paths(&sg, "right"), vec![vec![]]);
    assert_eq!(resolve_paths(&sg, "left"), Vec::<Vec<DefKind>>::new());

    replace_once(&mut parser, "right", "left");
    let (_, sg) = build_incremental_graph(&parser);
    assert_eq!(resolve_paths(&sg, "right"), Vec::<Vec<DefKind>>::new());
    assert_eq!(resolve_paths(&sg, "left"), vec![vec![DefKind::Local]]);
}

#[test]
fn incremental_multi_edit_inline_module_definition_updates_path_resolution() {
    let mut parser = IncrementalParser::new();
    parser.set_source(
        r#"
        fun f() {
            mod local {
                struct Hidden {}
            }

            local::Thing;
        }
        "#,
    );

    let (_, sg) = build_incremental_graph(&parser);
    assert_eq!(resolve_paths(&sg, "local::Thing"), vec![vec![]]);

    replace_once(&mut parser, "Hidden", "Thing");
    let (hir, sg) = build_incremental_graph(&parser);
    let local_mid = module_id_by_name(&hir, "local");
    let local_thing = struct_id_in_module(&hir, local_mid, "Thing");
    assert_eq!(resolved_struct_id(&sg, "local::Thing"), local_thing);

    replace_once(&mut parser, "Thing", "Hidden");
    let (_, sg) = build_incremental_graph(&parser);
    assert_eq!(resolve_paths(&sg, "local::Thing"), vec![vec![]]);

    replace_once(&mut parser, "Hidden", "Thing");
    let (hir, sg) = build_incremental_graph(&parser);
    let local_mid = module_id_by_name(&hir, "local");
    let local_thing = struct_id_in_module(&hir, local_mid, "Thing");
    assert_eq!(resolved_struct_id(&sg, "local::Thing"), local_thing);
}

#[test]
fn incremental_multi_edit_use_alias_retargets_resolution() {
    let mut parser = IncrementalParser::new();
    parser.set_source(
        r#"
        mod a {
            struct Left {}
            struct Right {}
        }

        use crate::a::Left as Pick;

        fun f() {
            Pick;
        }
        "#,
    );

    let (hir, sg) = build_incremental_graph(&parser);
    let a_mid = module_id_by_name(&hir, "a");
    let left = struct_id_in_module(&hir, a_mid, "Left");
    assert_eq!(resolved_struct_id(&sg, "Pick"), left);

    replace_once(&mut parser, "Left", "Gone");
    let (_, sg) = build_incremental_graph(&parser);
    assert_eq!(resolve_paths(&sg, "Pick"), vec![vec![]]);

    replace_once(&mut parser, "Right", "Left");
    let (hir, sg) = build_incremental_graph(&parser);
    let a_mid = module_id_by_name(&hir, "a");
    let new_left = struct_id_in_module(&hir, a_mid, "Left");
    assert_eq!(resolved_struct_id(&sg, "Pick"), new_left);

    replace_once(&mut parser, "Pick", "Alias");
    let (_, sg) = build_incremental_graph(&parser);
    assert_eq!(resolve_paths(&sg, "Pick"), vec![vec![]]);
    assert_eq!(resolve_paths(&sg, "Alias"), Vec::<Vec<DefKind>>::new());

    replace_once(&mut parser, "Pick", "Alias");
    let (hir, sg) = build_incremental_graph(&parser);
    let a_mid = module_id_by_name(&hir, "a");
    let left = struct_id_in_module(&hir, a_mid, "Left");
    assert_eq!(resolved_struct_id(&sg, "Alias"), left);
}
