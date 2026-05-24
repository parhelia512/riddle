use lexer::lexer;
use parser::parse;


fn main() {
    let src = r#"
        let x = 1 + 2 * 3;
        let y = (x + 4) * 2;
    "#;
    let tokens = lexer(src);

    let ret = parse(tokens);

    println!("{:?}", ret);
}
