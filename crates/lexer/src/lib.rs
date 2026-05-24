use logos::Logos;

#[derive(Logos, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Token {
    // keywords
    #[token("let")]
    Let,
    #[token("fun")]
    Fun,

    // literal
    #[regex("[0-9]+", |lex| lex.slice().parse::<i64>().unwrap())]
    Number(i64),

    #[regex("[a-zA-Z_][a-zA-Z0-9_]*", |lex| lex.slice().to_string())]
    Ident(String),

    // operator
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("=")]
    Equal,
    #[token("==")]
    EqualEqual,
    #[token("&")]
    Amp,

    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token(":")]
    Colon,
    #[token(";")]
    Semi,
    #[token("->")]
    Allow,
    #[token(",")]
    Comma,

    #[regex(r"[ \t\r\n]+", logos::skip)]
    Whitespace,
}

pub fn lexer(src: &str) -> Vec<Token> {
    Token::lexer(src)
        .filter_map(Result::ok)
        .filter(|t| *t != Token::Whitespace)
        .collect()
}
