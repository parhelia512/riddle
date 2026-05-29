use logos::Logos;

use super::syntax_kind::SyntaxKind;

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: SyntaxKind,
    pub text: String,
}

pub fn lex(input: &str) -> Vec<Token> {
    let mut tokens = vec![];
    let mut lexer = SyntaxKind::lexer(input);

    while let Some(result) = lexer.next() {
        let kind = result.unwrap_or(SyntaxKind::ErrorNode);
        tokens.push(Token {
            kind,
            text: lexer.slice().to_string(),
        });
    }

    tokens
}
