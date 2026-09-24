//! Direct `ast` and `hir` crate tests.
//!
//! These exercise the typed AST accessors, HIR body lowering, and the
//! `Place` algebra on their own — independent of type checking and the move
//! checker, which have their own suites layered on top.

use ast::{Root, support::AstNode};
use frontend::{incremental::IncrementalParser, tree_builder::Parse};
use hir::{
    HirFile,
    body::{Expr, ResolvedName, Stmt},
    lower_root,
    place::{Place, PlaceRoot, Projection},
};
use scope_graph::{builder::build_scope_graph, resolve::resolve_hir};

fn parse(source: &str) -> Parse {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(source);
    assert!(parse.errors.is_empty(), "parse errors: {:?}", parse.errors);
    parse.clone()
}

fn lower_and_resolve(source: &str) -> HirFile {
    let parse = parse(source);
    let syntax = parse.syntax();
    let root = Root::cast(syntax.clone()).expect("root casts");
    let mut hir = lower_root(&root);
    let (graph, _) = build_scope_graph(&hir, &syntax);
    resolve_hir(&mut hir, &graph);
    hir
}

// ═══════════════════════════════════════════════════════════
// ast: typed accessors over the rowan tree
// ═══════════════════════════════════════════════════════════

#[test]
fn ast_fn_decl_exposes_signature_pieces() {
    let parse = parse("pub fun add(a: i32, b: i32) -> i32 { a + b }");
    let _ = Root::cast(parse.syntax()).unwrap();
    let fn_nodes: Vec<ast::FuncDecl> = parse
        .syntax()
        .descendants()
        .filter_map(ast::FuncDecl::cast)
        .collect();
    assert_eq!(fn_nodes.len(), 1);
    let function = &fn_nodes[0];
    assert!(function.is_pub());
    assert_eq!(function.name().unwrap().text(), "add");
    let params = function.param_list().unwrap().params().count();
    assert_eq!(params, 2);
    let ret = function.return_type().expect("return type present");
    assert!(ret.syntax().text().to_string().contains("i32"));
    assert!(function.body().is_some());
}

#[test]
fn ast_let_stmt_exposes_pattern_init_and_else() {
    let parse = parse("fun main() { let x = 1; let Some(y) = opt else { return; }; }");
    let lets: Vec<ast::VarDecl> = parse
        .syntax()
        .descendants()
        .filter_map(ast::VarDecl::cast)
        .collect();
    assert_eq!(lets.len(), 2);
    assert!(lets[0].pattern().is_some());
    assert!(lets[0].init().is_some());
    assert!(lets[0].else_block().is_none());
    assert!(lets[1].else_block().is_some(), "let-else block present");
}

#[test]
fn ast_string_literals_carry_raw_and_decoded_text() {
    let parse = parse(r##"fun main() { let a = "x\ny"; let b = r#"raw"#; }"##);
    let strings: Vec<ast::StringLitExpr> = parse
        .syntax()
        .descendants()
        .filter_map(ast::StringLitExpr::cast)
        .collect();
    assert_eq!(strings.len(), 2);
    assert!(strings[0].value_token().is_some());
    assert!(strings[1].value_token().is_some());
}

#[test]
fn ast_use_tree_shape_is_queryable() {
    let parse = parse("use std::collections::{HashMap, HashSet as HS};");
    let use_decl = parse
        .syntax()
        .descendants()
        .find_map(ast::UseDecl::cast)
        .expect("use decl");
    assert!(!use_decl.is_pub());
    let tree = use_decl.use_tree().expect("use tree");
    let list = tree.subtree_list().expect("nested list");
    let trees: Vec<_> = list.trees().collect();
    assert_eq!(trees.len(), 2);
    assert!(trees[0].alias().is_none());
    assert!(trees[1].alias().is_some(), "renamed import has alias");
}

#[test]
fn ast_attributes_and_doc_comments_are_discoverable() {
    let parse = parse("#[lang = \"drop\"]\n/// docs\nfun drop(&mut self) {}");
    let fn_node = parse
        .syntax()
        .descendants()
        .find_map(ast::FuncDecl::cast)
        .expect("fn with attributes");
    let attrs = ast::attrs_for_node(fn_node.syntax());
    assert_eq!(attrs.len(), 1);
    assert_eq!(attrs[0].name().unwrap().text(), "lang");
    assert_eq!(attrs[0].string_value().as_deref(), Some("drop"));
    let docs = ast::doc_comments_for_node(fn_node.syntax());
    assert_eq!(docs.len(), 1, "doc comment attached to the fn");
}

// ═══════════════════════════════════════════════════════════
// hir: body lowering shape and name resolution
// ═══════════════════════════════════════════════════════════

#[test]
fn hir_lowering_builds_block_stmts_and_tail() {
    let hir = lower_and_resolve("fun main() { let x = 1; x + 2 }");
    let (body_id, body) = hir.bodies.iter().next().expect("one body");
    assert!(
        body.diagnostics.is_empty(),
        "lowering diagnostics: {:?}",
        body.diagnostics
    );
    let Expr::Block { stmts, tail } = &body.exprs[body.root_block] else {
        panic!("root is a block");
    };
    assert_eq!(stmts.len(), 1);
    assert!(tail.is_some(), "block tail expression kept");
    let Stmt::Let {
        init: Some(init), ..
    } = &body.stmts[stmts[0]]
    else {
        panic!("stmt is a let with init");
    };
    assert!(matches!(
        body.exprs[*init],
        Expr::IntLiteral { value: 1, .. }
    ));
    let _ = body_id;
}

#[test]
fn hir_resolves_local_paths_to_pattern_bindings() {
    let hir = lower_and_resolve("fun main() { let value = 1; let copy = value; }");
    let (_, body) = hir.bodies.iter().next().unwrap();
    let paths: Vec<&Expr> = body
        .exprs
        .iter()
        .filter(|(_, expr)| matches!(expr, Expr::Path { .. }))
        .map(|(_, expr)| expr)
        .collect();
    // `let value` lowers to a Pattern, so the only Path expression is the
    // use — and it must resolve back to that binding.
    assert_eq!(paths.len(), 1, "unexpected paths: {paths:#?}");
    let Expr::Path { resolved, .. } = paths[0] else {
        unreachable!();
    };
    assert!(
        matches!(resolved, Some(ResolvedName::PatternBinding(_))),
        "use of `value` resolves to its binding, got {resolved:?}"
    );
}

#[test]
fn hir_functions_carry_params_and_return_types() {
    let hir = lower_and_resolve("fun add(a: i32, b: i32) -> i32 { a + b }");
    let (fid, function) = hir.item_tree.functions.iter().next().unwrap();
    assert_eq!(function.name.0.as_str(), "add");
    assert_eq!(function.params.len(), 2);
    assert!(function.ret_type.is_some());
    assert!(hir.function_bodies.contains_key(&fid));
}

// ═══════════════════════════════════════════════════════════
// hir: Place algebra (pure)
// ═══════════════════════════════════════════════════════════

#[test]
fn place_projections_and_prefixes() {
    let root = Place::root(PatternBindingIdFixture::at(0));
    let field = root.clone().field(1).index(Some(4));
    let wildcard = root.clone().index(None);

    // A place is a prefix of its own extensions, not the reverse.
    assert!(root.is_prefix_of(&field));
    assert!(!field.is_prefix_of(&root));
    assert!(field.is_prefix_of(&field));

    // Distinct fields and concrete/wildcard indices overlap the base but are
    // not prefixes of each other.
    let other_field = root.clone().field(2);
    assert!(!field.is_prefix_of(&other_field));
    assert!(!wildcard.is_prefix_of(&field));

    // Prefix algebra walks projections in order.
    match &field.projections[..] {
        [Projection::Field(1), Projection::Index(Some(4))] => {}
        other => panic!("unexpected projections {other:?}"),
    }
    let _ = PlaceRoot::Pattern(PatternBindingIdFixture::at(7));
}

/// Minimal stand-in mirroring `hir::body::PatternBindingId`'s shape without
/// reaching into body internals for a place-only test.
struct PatternBindingIdFixture;

impl PatternBindingIdFixture {
    fn at(id: usize) -> hir::body::PatternBindingId {
        hir::body::PatternBindingId {
            pattern: la_arena::Idx::from_raw(la_arena::RawIdx::from_u32(id as u32)),
            field: None,
        }
    }
}
