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
            pub struct S {}
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
    let (sg, _) = build_scope_graph(&hir, &syntax);
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
fn rust_style_array_repeat_parses_and_lowers() {
    let source = r#"
        fun f() {
            let value = 1;
            let repeated: [i32; 3] = [value; 3];
        }
        "#;

    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(source);
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);

    let syntax = parse.syntax();
    let root = ast::Root::cast(syntax.clone()).unwrap();
    let mut hir = lower_root(root);
    let (sg, _) = build_scope_graph(&hir, &syntax);
    resolve_hir(&mut hir, &sg);

    let body_id = *hir.function_bodies.values().next().unwrap();
    let body = &hir.bodies[body_id];

    assert!(body.exprs.iter().any(|(_, expr)| matches!(
        expr,
        Expr::ArrayRepeat { value, len }
            if matches!(
                body.exprs[*value],
                Expr::Path {
                    resolved: Some(ResolvedName::Local(_)),
                    ..
                }
            ) && matches!(body.exprs[*len], Expr::IntLiteral { value: 3, .. })
    )));
}

#[test]
fn array_type_requires_rust_style_length() {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(
        r#"
        fun f() {
            let xs: [i32] = [];
            let ys: [i32;] = [];
        }
        "#,
    );
    let messages = parse
        .errors
        .iter()
        .map(|error| error.message.as_str())
        .collect::<Vec<_>>();

    assert!(messages.iter().any(|msg| msg.contains("expected Semi")));
    assert!(
        messages
            .iter()
            .any(|msg| msg.contains("expected expression"))
    );
}

#[test]
fn reports_unsupported_explicit_generic_exprs_without_cascading() {
    let source = r#"
        struct Wrap<T> {
            inner: T,
        }

        fun f<T>(x: T) {
            g<Wrap<T>>(x);
            Wrap::<T> { inner: x };
        }
        "#;

    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(source);
    let messages = parse
        .errors
        .iter()
        .map(|error| error.message.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        messages,
        vec![
            "explicit generic function calls are not supported; omit the type arguments",
            "explicit generic struct literals are not supported; omit the type arguments",
        ]
    );
}

#[test]
fn resolves_enum_variant_in_path() {
    let (mut hir, sg) = build_hir_and_graph(
        "enum Foo{\n    A,\n    B\n}\n\nfun main(){\n    let x = Foo::A;\n    x = Foo::B;\n    test(x);\n}\n\nfun test(x: &Foo) -> bool{\n    if x == Foo::A {\n        true\n    }else{\n        false\n    }\n}",
    );

    resolve_hir(&mut hir, &sg);

    let all_resolved: Vec<_> = hir
        .bodies
        .iter()
        .flat_map(|(_, body)| {
            body.exprs.iter().filter_map(|(_, expr)| match expr {
                Expr::Path { path, resolved } => Some((path.display(), resolved.clone())),
                _ => None,
            })
        })
        .collect();

    assert!(
        all_resolved.iter().any(|(path, r)| {
            path == "Foo::A" && matches!(r, Some(ResolvedName::EnumVariant(_, _)))
        }),
        "Expected EnumVariant for Foo::A"
    );
    assert!(
        all_resolved.iter().any(|(path, r)| {
            path == "Foo::B" && matches!(r, Some(ResolvedName::EnumVariant(_, _)))
        }),
        "Expected EnumVariant for Foo::B"
    );

    // No unresolved paths
    let unresolved: Vec<_> = all_resolved
        .iter()
        .filter(|(_, r)| matches!(r, Some(ResolvedName::Unresolved) | None))
        .collect();
    assert!(
        unresolved.is_empty(),
        "Unexpected unresolved paths: {:?}",
        unresolved
    );
}
