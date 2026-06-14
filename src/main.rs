use ast::support::AstNode;
use frontend::incremental::IncrementalParser;
use hir::lower_root;

pub mod ast;
pub mod frontend;
pub mod hir;

fn main() {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(
        r"
    struct Foo{
        x:int
    }
    fun main()->int{
        let a: &&.T.;
        let b = &&.x;
        let c = a. && b.;
        let d = &&x && .y;
        let e = f(x.);
        e.xxxxx;
        1
    }
    ",
    );

    let syntax = parse.syntax();
    let root = ast::Root::cast(syntax).unwrap();
    let hir = lower_root(root);
    for (function_id, body_id) in &hir.function_bodies {
        let function = &hir.item_tree.functions[*function_id];
        let body = &hir.bodies[*body_id];

        println!("function: {:?}", function.name.0);
        println!("body: {}", body.pretty());
    }
}
