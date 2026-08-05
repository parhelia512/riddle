use logos::Logos;

use syntax::SyntaxKind;

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: SyntaxKind,
    pub span: std::ops::Range<usize>,
}

#[must_use]
pub fn lex(input: &str) -> Vec<Token> {
    let mut tokens = vec![];
    let mut lexer = SyntaxKind::lexer(input);

    while let Some(result) = lexer.next() {
        let kind = result.unwrap_or(SyntaxKind::ErrorNode);
        let span = lexer.span();
        tokens.push(Token { kind, span });
    }

    tokens
}

impl Token {
    #[inline]
    #[must_use]
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.span.clone()]
    }
}
