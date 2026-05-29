use frontend::{incremental::IncrementalParser, lexer, parser, syntax_kind::{SyntaxElement, SyntaxNode}, tree_builder};

pub mod frontend;

fn parse(input: &str) -> tree_builder::Parse {
    let tokens = lexer::lex(input);
    let parser = parser::Parser::new(tokens);
    let (events, tokens, errors) = parser.parse();
    tree_builder::build_tree(events, tokens, errors)
}

fn print_tree(node: &SyntaxNode, depth: usize) {
    let indent = "  ".repeat(depth);
    println!(
        "{}{:?} @ {:?}",
        indent,
        node.kind(),
        node.text_range()
    );
    for child in node.children_with_tokens() {
        match child {
            SyntaxElement::Node(n) => print_tree(&n, depth + 1),
            SyntaxElement::Token(t) => {
                let indent = "  ".repeat(depth + 1);
                println!("{}{:?} {:?}", indent, t.kind(), t.text());
            }
        }
    }
}

fn main() {
    let mut parser = IncrementalParser::new();

    let source = "let x = 10;\nlet y = x + 1;\n";
    let parse = parser.set_source(source);
    println!("{}", parse.debug_tree());

    let parse = parser.apply_edit(8, 2, "20");
    println!("{}", parse.debug_tree());

    assert_eq!(parser.source(), "let x = 20;\nlet y = x + 1;\n");
}
