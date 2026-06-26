use crate::{DefKind, build, reference_node, resolve_paths};

use scope_graph::{EdgeKind, Node};

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
