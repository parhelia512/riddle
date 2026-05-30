use logos::Logos;

#[derive(Logos, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum SyntaxKind {
    #[regex(r"[ \t\r\n]+")]
    Whitespace = 0,

    #[regex(r"//[^\n]*")]
    LineComment,

    // keywords
    #[token("let")]
    Let,
    #[token("fun")]
    Fun,
    #[token("struct")]
    Struct,
    #[token("if")]
    If,
    #[token("else")]
    Else,
    #[token("while")]
    While,
    #[token("return")]
    Return,

    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*")]
    Ident,

    #[regex(r"[1-9][0-9]*|0")]
    Number,

    #[token("->")]
    Arrow,
    #[token("==")]
    EqEq,
    #[token("!=")]
    BangEq,
    #[token("<=")]
    LessEq,
    #[token(">=")]
    GreaterEq,
    #[token("&&")]
    AmpAmp,
    #[token("||")]
    PipePipe,

    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("%")]
    Percent,
    #[token("&")]
    Amp,
    #[token("<")]
    Less,
    #[token(">")]
    Greater,
    #[token("!")]
    Bang,

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
    #[token(",")]
    Comma,
    #[token("=")]
    Eq,

    // nodes
    // see docs/grammar.md
    Root,
    VarDecl,
    FuncDecl,
    Param, 
    ParamList,
    StructDecl,
    StructField,
    StructFieldList,
    IfStmt,
    WhileStmt,
    ReturnStmt,
    Block,
    ExprStmt,
    NameRef,
    NumberLit,
    BinaryExpr,
    UnaryExpr,
    ParenExpr, 
    NamedType,
    RefType,
    ErrorNode,

    // special
    Tombstone,
    Eof,
}

impl SyntaxKind {
    pub fn is_trivia(self) -> bool {
        matches!(self, SyntaxKind::Whitespace | Self::LineComment)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RiddleLang;

impl rowan::Language for RiddleLang {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> Self::Kind {
        unsafe { std::mem::transmute(raw.0) }
    }

    fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind as u16)
    }
}

pub type SyntaxNode = rowan::SyntaxNode<RiddleLang>;
pub type SyntaxToken = rowan::SyntaxToken<RiddleLang>;
pub type SyntaxElement = rowan::NodeOrToken<SyntaxNode, SyntaxToken>;
