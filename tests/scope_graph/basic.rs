use crate::{
    DefKind, build, build_diagnostics, local_binding, param_fn, resolve_paths, resolve_reference,
};

use scope_graph::Node;

#[test]
fn duplicate_top_level_functions_report_both_definitions() {
    let source = "fun hello() -> i32 { 0 }\nfun hello() -> i32 { 1 }\nfun main() -> i32 { 0 }\nfun main() -> i32 { 1 }\n";
    let diagnostics = build_diagnostics(source);
    assert_eq!(diagnostics.len(), 2, "{diagnostics:#?}");

    for (diagnostic, name) in diagnostics.iter().zip(["hello", "main"]) {
        assert_eq!(diagnostic.code, "E0064");
        assert_eq!(
            diagnostic.message,
            format!("the name `{name}` is defined multiple times")
        );
        assert_eq!(diagnostic.labels.len(), 2);
        assert_eq!(diagnostic.labels[0].style, hir::body::LabelStyle::Primary);
        assert_eq!(diagnostic.labels[0].message, "duplicate definition");
        assert_eq!(
            &source[diagnostic.labels[0].range], name,
            "the duplicate definition should be primary"
        );
        assert_eq!(diagnostic.labels[1].style, hir::body::LabelStyle::Secondary);
        assert_eq!(diagnostic.labels[1].message, "first definition");
        assert_eq!(&source[diagnostic.labels[1].range], name);
        assert!(diagnostic.labels[0].range.start() > diagnostic.labels[1].range.start());
    }
}

#[test]
fn duplicate_named_items_are_scoped_and_cross_kind() {
    let source = r"
        struct Shared {}
        enum Shared { Unit }

        mod nested {
            const VALUE: i32 = 1;
            type VALUE = i32;
        }

        mod repeated {}
        mod repeated {}

        mod left { fun same() {} }
        mod right { fun same() {} }
    ";
    let diagnostics = build_diagnostics(source);
    let names = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        names,
        [
            "the name `Shared` is defined multiple times",
            "the name `VALUE` is defined multiple times",
            "the name `repeated` is defined multiple times",
        ],
        "{diagnostics:#?}"
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code == "E0064")
    );
}

#[test]
fn resolves_param_then_local_in_statement_order() {
    let sg = build(
        r"
        fun f(x: int) {
            let y = x;
            y
        }
        ",
    );

    assert_eq!(resolve_paths(&sg, "x"), vec![vec![DefKind::Param]]);
    assert_eq!(resolve_paths(&sg, "y"), vec![vec![DefKind::Local]]);
}

#[test]
fn local_shadows_param() {
    let sg = build(
        r"
        fun f(x: int) {
            let x = 1;
            x
        }
        ",
    );

    assert_eq!(resolve_paths(&sg, "x"), vec![vec![DefKind::Local]]);
}

#[test]
fn let_initializer_does_not_see_its_own_binding() {
    let sg = build(
        r"
        fun f(x: int) {
            let x = x;
        }
        ",
    );

    assert_eq!(resolve_paths(&sg, "x"), vec![vec![DefKind::Param]]);
}

#[test]
fn let_bindings_are_distinct_across_statement_chain() {
    let sg = build(
        r"
        fun f(a: int) {
            let x = a;
            let y = x;
            let x = y;
            x
        }
        ",
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
        .find(|(path, defs)| path == "x" && local_binding(defs).is_some())
        .and_then(|(_, defs)| local_binding(defs))
        .unwrap();

    let tail_x = refs
        .iter()
        .rev()
        .find(|(path, defs)| path == "x" && local_binding(defs).is_some())
        .and_then(|(_, defs)| local_binding(defs))
        .unwrap();

    assert_ne!(y_init_x, tail_x);
    assert!(
        refs.iter()
            .any(|(path, defs)| path == "a" && param_fn(defs).is_some())
    );
    assert!(
        refs.iter()
            .any(|(path, defs)| path == "y" && local_binding(defs).is_some())
    );
}

#[test]
fn local_declared_before_nested_while_body_is_visible() {
    let sg = build(
        r"
        fun f(flag: bool) {
            if flag {
                let mut go: bool = true;
                while go {
                    go = false;
                }
            }
        }
        ",
    );

    assert_eq!(
        resolve_paths(&sg, "go"),
        vec![vec![DefKind::Local], vec![DefKind::Local]]
    );
}

#[test]
fn same_named_locals_in_sibling_blocks_do_not_cross_resolve() {
    let sg = build(
        r"
        fun f(flag: bool) {
            if flag {
                let mut go: bool = true;
                while go { go = false; }
            } else {
                let mut go: bool = true;
                while go { go = false; }
            }
        }
        ",
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
            .then(|| local_binding(&resolve_reference(&sg, nid)).unwrap())
        })
        .collect();

    assert_eq!(go_defs.len(), 4, "expected two refs per block");
    assert_eq!(go_defs[0], go_defs[1]);
    assert_eq!(go_defs[2], go_defs[3]);
    assert_ne!(go_defs[0], go_defs[2]);
}
