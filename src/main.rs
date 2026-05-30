use frontend::incremental::IncrementalParser;

pub mod frontend;

fn main() {
    let mut parser = IncrementalParser::new();

    let source = "let x = &&&&a&&&&&b";
    let parse = parser.set_source(source);
    println!("{}", parse.debug_tree());
}
