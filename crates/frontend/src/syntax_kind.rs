use logos::Logos;

#[derive(Logos, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum SyntaxKind {
    #[regex(r"[ \t\r\n]+")]
    Whitespace = 0,

    #[regex(r"//[^\n]*", allow_greedy = true)]
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
    #[token("as")]
    As,
    #[token("self")]
    SelfKw,
    #[token("mod")]
    Mod,
    #[token("use")]
    Use,
    #[token("mut")]
    Mut,
    #[token("pub")]
    Pub,
    #[token("super")]
    SuperKw,
    #[token("crate")]
    CrateKw,
    #[token("enum")]
    Enum,
    #[token("trait")]
    Trait,
    #[token("impl")]
    Impl,
    #[token("match")]
    Match,
    #[token("const")]
    Const,
    #[token("type")]
    TypeKw,
    #[token("extern")]
    Extern,
    #[token("unsafe")]
    Unsafe,
    #[token("for")]
    For,
    #[token("in")]
    In,
    #[token("where")]
    Where,
    #[token("true")]
    True,
    #[token("false")]
    False,
    #[regex(r#""([^"\\]|\\.)*""#)]
    // ponytail: raw string r#"..."# needs Logos callback, regex can't count # delimiters
    String,
    #[regex(r#"'([^'\\]|\\.)'"#)]
    Char,
    #[token("_")]
    Underscore,

    #[regex(r"[0-9]+\.[0-9]+(?:[eE][+-]?[0-9]+)?(?:f16|f32|f64|f128)?")]
    #[regex(r"[0-9]+(?:[eE][+-]?[0-9]+)(?:f16|f32|f64|f128)?")]
    #[regex(r"[0-9]+\.[0-9]*(?:f16|f32|f64|f128)?")]
    Float,

    #[regex(r"[a-zA-Z][a-zA-Z0-9_]*")]
    #[regex(r"_[a-zA-Z0-9_]+")]
    Ident,

    #[regex(r"[0-9]+(?:i8|i16|i32|i64|i128|isize|u8|u16|u32|u64|u128|usize)?")]
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
    #[token("=>")]
    FatArrow,
    #[token("+=")]
    PlusEq,
    #[token("-=")]
    MinusEq,
    #[token("*=")]
    StarEq,
    #[token("/=")]
    SlashEq,
    #[token("%=")]
    PercentEq,
    #[token("&=")]
    AmpEq,
    #[token("|=")]
    PipeEq,
    #[token("^=")]
    CaretEq,
    #[token("<<=")]
    ShlEq,
    #[token(">>=")]
    ShrEq,
    #[token("<<")]
    Shl,
    #[token(">>")]
    Shr,

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
    #[token("|")]
    Pipe,
    #[token("^")]
    Caret,
    #[token("<")]
    Less,
    #[token(">")]
    Greater,
    #[token("!")]
    Bang,
    #[token("#")]
    Hash,

    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,

    #[token(".")]
    Dot,
    #[token(":")]
    Colon,
    #[token("::")]
    ColonColon,
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
    ModDecl,
    UseDecl,
    UseTree,
    UseTreeList,
    Path,
    PathSegment,
    IfStmt,
    WhileStmt,
    ForExpr,
    ReturnStmt,
    Block,
    ExprStmt,
    NameRef,
    NumberLit,
    BinaryExpr,
    UnaryExpr,
    ParenExpr,
    CallExpr,
    ArgList,
    FieldExpr,
    IndexExpr,
    StructExpr,
    StructExprField,
    NamedType,
    TypeArgList,
    RefType,
    TupleType,
    ArrayType,
    ConstType,
    ArrayExpr,
    MatchExpr,
    MatchArm,
    EnumDecl,
    EnumVariant,
    TraitDecl,
    ImplDecl,
    GenericParams,
    TypeAliasDecl,
    ConstDecl,
    TuplePattern,
    StructPattern,
    EnumPattern,
    WildcardPattern,
    LiteralPattern,
    FloatLit,
    StringLit,
    CharLit,
    BoolLit,
    ExternBlock,
    ExternFnDecl,
    Attribute,
    UnsafeExpr,
    CastExpr,
    PtrType,
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
        // Safe: #[repr(u16)] with sequential discriminants 0..=Eof are all valid
        if raw.0 <= SyntaxKind::Eof as u16 {
            // Safety: raw.0 is within the valid discriminant range
            unsafe { std::mem::transmute(raw.0) }
        } else {
            SyntaxKind::ErrorNode
        }
    }

    fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind as u16)
    }
}

pub type SyntaxNode = rowan::SyntaxNode<RiddleLang>;
pub type SyntaxToken = rowan::SyntaxToken<RiddleLang>;
pub type SyntaxElement = rowan::NodeOrToken<SyntaxNode, SyntaxToken>;
