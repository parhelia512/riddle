use frontend::lexer::Lexer;

mod frontend;
mod diagnostic;

fn main() {
    let source = "//:;\n\"hello\"".to_string();
    let mut lexer = Lexer::new(source);
    lexer.scan();
    let tokens = lexer.tokens;

    println!("{:?}", tokens);
}
