use ast::{self, support::AstNode};
use frontend::incremental::IncrementalParser;
use hir::lower_root;
use scope_graph::{Node, builder::build_scope_graph, resolve::resolve_reference};

fn main() {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(
        r"
    use crate::foo::{bar as baz, qux::*};

    mod inner {
        struct Foo {
            x: crate::types::int
        }

        trait Bar{
        
        }

        impl Bar for Foo {
            
        }

        fun main(x: &super::Input) -> ::core::int {
            let a: &&crate::types::int;
            let b = &&foo::bar;
            let c = crate::math::add(1, 2);
            c.value
        }
    }
    ",
    );

    let syntax = parse.syntax();
    let root = ast::Root::cast(syntax.clone()).unwrap();
    let hir = lower_root(root);
    let sg = build_scope_graph(&hir, &syntax);

    for (nid, node) in sg.nodes.iter() {
        if let Node::Reference { segments, .. } = node {
            let defs = resolve_reference(&sg, nid);
            let path_str = segments
                .iter()
                .map(|n| n.0.as_str())
                .collect::<Vec<_>>()
                .join("::");
            println!("ref `{}` -> {:?}", path_str, defs);
        }
    }
}
