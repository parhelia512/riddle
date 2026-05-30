use logos::Logos;

use super::syntax_kind::SyntaxKind;

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: SyntaxKind,
    pub span: std::ops::Range<usize>,
}

pub fn lex(input: &str) -> Vec<Token> {
    let mut tokens = vec![];
    let mut lexer = SyntaxKind::lexer(input);

    while let Some(result) = lexer.next() {
        let kind = result.unwrap_or(SyntaxKind::ErrorNode);
        let span = lexer.span();
        tokens.push(Token {
            kind,
            span: span.start..span.end,
        });
    }

    tokens
}

impl Token {
    #[inline]
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.span.start as usize..self.span.end as usize]
    }
}
