use crate::{DefKind, build, local_stmt, param_fn, resolve_paths, resolve_reference};

use scope_graph::Node;

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

    let refs: Vec<_> = sg
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
        .collect();

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
fn local_declared_before_nested_while_body_is_visible() {
    let sg = build(
        r#"
        fun f(flag: bool) {
            if flag {
                let mut go: bool = true;
                while go {
                    go = false;
                }
            }
        }
        "#,
    );

    assert_eq!(
        resolve_paths(&sg, "go"),
        vec![vec![DefKind::Local], vec![DefKind::Local]]
    );
}

#[test]
fn same_named_locals_in_sibling_blocks_do_not_cross_resolve() {
    let sg = build(
        r#"
        fun f(flag: bool) {
            if flag {
                let mut go: bool = true;
                while go { go = false; }
            } else {
                let mut go: bool = true;
                while go { go = false; }
            }
        }
        "#,
    );

    let go_defs: Vec<_> = sg
        .nodes
        .iter()
        .filter_map(|(nid, node)| {
            let Node::Reference { segments, .. } = node else {
                return None;
            };
            (segments
                .iter()
                .map(|name| name.0.as_str())
                .collect::<Vec<_>>()
                == ["go"])
            .then(|| local_stmt(&resolve_reference(&sg, nid)).unwrap())
        })
        .collect();

    assert_eq!(go_defs.len(), 4, "expected two refs per block");
    assert_eq!(go_defs[0], go_defs[1]);
    assert_eq!(go_defs[2], go_defs[3]);
    assert_ne!(go_defs[0], go_defs[2]);
}
