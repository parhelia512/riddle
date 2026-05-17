// [start, end)
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub struct Token {
    pub span: Span,
    pub kind: TokenKind,
    pub lexeme: String,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub enum TokenKind {
    Eof,

    // delimiters
    LeftParen,    // (
    RightParen,   // )
    LeftBracket,  // [
    RightBracket, // ]
    LeftBrace,    // {
    RightBrace,   // }
    Comma,        // ,
    Colon,        // :
    Semi,         // ;
    Dot,          // .

    // operators
    Plus,         // +
    Minus,        // -
    Star,         // *
    Slash,        // /
    Greater,      // >
    GreaterEqual, // >=
    Less,         // <
    LessEqual,    // <=
    Equal,        // =
    EqualEqual,   // ==
    Bang,         // !
    BangEqual,    // !=
    Amp,          // &
    AmpAmp,       // &&
    Pipe,         // |
    PipePipe,     // ||

    // literals
    Identifier,
    Number,
    Str,

    // keywords
    Fun,
    Let,
    Mut,
}
