use ast::support::AstNode;
use frontend::incremental::IncrementalParser;

pub mod frontend;
pub mod ast;

fn main() {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source("let x: i32 = 1 + 2;struct Foo{x:int}fun main()->int{return 1;}");

    let syntax = parse.syntax();
    let root = ast::Root::cast(syntax).unwrap();
}
