use ast::{self, support::AstNode};
use frontend::incremental::IncrementalParser;
use hir::{
    body::{BinaryOp, Expr, ResolvedName},
    lower_root,
};
use scope_graph::builder::build_scope_graph;
use scope_graph::resolve::resolve_hir;

use crate::build_hir_and_graph;

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
    let resolved_paths: Vec<_> = body
        .exprs
        .iter()
        .filter_map(|(_, expr)| match expr {
            Expr::Path { path, resolved } => Some((path.display(), resolved.clone())),
            _ => None,
        })
        .collect();

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
