use super::support::{self, AstNode};
use frontend::syntax_kind::{SyntaxKind, SyntaxNode, SyntaxToken};

macro_rules! ast_node {
    ($name:ident, $kind:ident) => {
        #[derive(Debug, Clone)]
        pub struct $name {
            syntax: SyntaxNode,
        }

        impl AstNode for $name {
            fn cast(node: SyntaxNode) -> Option<Self> {
                if node.kind() == SyntaxKind::$kind {
                    Some(Self { syntax: node })
                } else {
                    None
                }
            }

            fn syntax(&self) -> &SyntaxNode {
                &self.syntax
            }
        }
    };
}

ast_node!(Root, Root);
ast_node!(VarDecl, VarDecl);
ast_node!(FuncDecl, FuncDecl);
ast_node!(ParamList, ParamList);
ast_node!(Param, Param);
ast_node!(StructDecl, StructDecl);
ast_node!(StructFieldList, StructFieldList);
ast_node!(StructField, StructField);
ast_node!(IfStmt, IfStmt);
ast_node!(WhileStmt, WhileStmt);
ast_node!(ReturnStmt, ReturnStmt);
ast_node!(Block, Block);
ast_node!(ExprStmt, ExprStmt);
ast_node!(BinaryExpr, BinaryExpr);
ast_node!(UnaryExpr, UnaryExpr);
ast_node!(ParenExpr, ParenExpr);
ast_node!(NumberExpr, NumberLit);
ast_node!(NameRefExpr, NameRef);
ast_node!(NamedType, NamedType);
ast_node!(RefType, RefType);
ast_node!(CallExpr, CallExpr);
ast_node!(ArgList, ArgList);
ast_node!(FieldExpr, FieldExpr);
ast_node!(StructExpr, StructExpr);
ast_node!(StructExprField, StructExprField);
ast_node!(Path, Path);
ast_node!(PathSegment, PathSegment);
ast_node!(ModDecl, ModDecl);
ast_node!(UseDecl, UseDecl);
ast_node!(UseTree, UseTree);
ast_node!(UseTreeList, UseTreeList);
// new items
ast_node!(EnumDecl, EnumDecl);
ast_node!(EnumVariant, EnumVariant);
ast_node!(TraitDecl, TraitDecl);
ast_node!(ImplDecl, ImplDecl);
ast_node!(GenericParams, GenericParams);
ast_node!(ConstDecl, ConstDecl);
ast_node!(TypeAliasDecl, TypeAliasDecl);
// new expressions
ast_node!(FloatLitExpr, FloatLit);
ast_node!(StringLitExpr, StringLit);
ast_node!(CharLitExpr, CharLit);
ast_node!(BoolLitExpr, BoolLit);
ast_node!(MatchExpr, MatchExpr);
ast_node!(MatchArm, MatchArm);
ast_node!(ArrayExpr, ArrayExpr);
// new types
ast_node!(TupleType, TupleType);
ast_node!(ArrayType, ArrayType);
// patterns
ast_node!(WildcardPat, WildcardPattern);
ast_node!(LiteralPat, LiteralPattern);
ast_node!(TuplePat, TuplePattern);
ast_node!(StructPattern, StructPattern);
ast_node!(EnumPattern, EnumPattern);

impl Root {
    pub fn stmts(&self) -> impl Iterator<Item = Stmt> + '_ {
        support::children(&self.syntax)
    }
}

impl VarDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn init(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl FuncDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn param_list(&self) -> Option<ParamList> {
        support::child(&self.syntax)
    }

    pub fn return_type(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn body(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

impl ParamList {
    pub fn params(&self) -> impl Iterator<Item = Param> + '_ {
        support::children(&self.syntax)
    }
}

impl Param {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }
}

impl StructDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn field_list(&self) -> Option<StructFieldList> {
        support::child(&self.syntax)
    }
}

impl StructFieldList {
    pub fn fields(&self) -> impl Iterator<Item = StructField> + '_ {
        support::children(&self.syntax)
    }
}

impl StructField {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }
}

impl Block {
    pub fn stmts(&self) -> impl Iterator<Item = Stmt> + '_ {
        support::children(&self.syntax)
    }

    pub fn tail_expr(&self) -> Option<Expr> {
        support::last_child(&self.syntax)
    }
}

impl IfStmt {
    pub fn condition(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn then_branch(&self) -> Option<Block> {
        support::child(&self.syntax)
    }

    pub fn else_branch(&self) -> Option<ElseBranch> {
        if let Some(else_block) = support::nth_child::<Block>(&self.syntax, 1) {
            return Some(ElseBranch::Block(else_block));
        }
        let if_stmt: Option<IfStmt> = support::child(&self.syntax);
        if_stmt.map(ElseBranch::IfStmt)
    }
}

#[derive(Debug, Clone)]
pub enum ElseBranch {
    Block(Block),
    IfStmt(IfStmt),
}

impl WhileStmt {
    pub fn condition(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn body(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

impl ReturnStmt {
    pub fn value(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl ExprStmt {
    pub fn expr(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl BinaryExpr {
    pub fn lhs(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn rhs(&self) -> Option<Expr> {
        support::nth_child(&self.syntax, 1)
    }

    pub fn op_token(&self) -> Option<SyntaxToken> {
        support::token(&self.syntax, |kind| {
            matches!(
                kind,
                SyntaxKind::Eq
                    | SyntaxKind::Plus
                    | SyntaxKind::Minus
                    | SyntaxKind::Star
                    | SyntaxKind::Slash
                    | SyntaxKind::Percent
                    | SyntaxKind::EqEq
                    | SyntaxKind::BangEq
                    | SyntaxKind::Less
                    | SyntaxKind::Greater
                    | SyntaxKind::LessEq
                    | SyntaxKind::GreaterEq
                    | SyntaxKind::AmpAmp
                    | SyntaxKind::PipePipe
            )
        })
    }
}

impl UnaryExpr {
    pub fn operand(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn op_token(&self) -> Option<SyntaxToken> {
        support::token(&self.syntax, |kind| {
            matches!(
                kind,
                SyntaxKind::Plus
                    | SyntaxKind::Minus
                    | SyntaxKind::Amp
                    | SyntaxKind::AmpAmp
                    | SyntaxKind::Star
                    | SyntaxKind::Bang
            )
        })
    }
}

impl ParenExpr {
    pub fn inner(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl RefType {
    pub fn inner(&self) -> Option<Type> {
        support::child(&self.syntax)
    }
}

impl NumberExpr {
    pub fn value_token(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Number)
    }

    pub fn value(&self) -> Option<i64> {
        let text = self.value_token()?;
        let text = text.text();
        let suffix_start = text
            .find(|ch: char| !ch.is_ascii_digit())
            .unwrap_or(text.len());
        text[..suffix_start].parse().ok()
    }
}

impl NamedType {
    pub fn path(&self) -> Option<Path> {
        support::child(&self.syntax)
    }
}

impl NameRefExpr {
    pub fn path(&self) -> Option<Path> {
        support::child(&self.syntax)
    }
}

impl CallExpr {
    pub fn callee(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn arg_list(&self) -> Option<ArgList> {
        support::child(&self.syntax)
    }
}

impl ArgList {
    pub fn args(&self) -> impl Iterator<Item = Expr> + '_ {
        support::children(&self.syntax)
    }
}

impl FieldExpr {
    pub fn base(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn field_name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }
}

impl StructExpr {
    pub fn path(&self) -> Option<Path> {
        support::child::<NameRefExpr>(&self.syntax)?.path()
    }

    pub fn fields(&self) -> impl Iterator<Item = StructExprField> + '_ {
        support::children(&self.syntax)
    }
}

impl StructExprField {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn value(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl Path {
    pub fn segments(&self) -> impl Iterator<Item = PathSegment> + '_ {
        support::children(&self.syntax)
    }

    /// Returns whether this path starts with `::` and is therefore absolute.
    pub fn is_absolute(&self) -> bool {
        self.syntax
            .children_with_tokens()
            .find(|it| match it {
                rowan::NodeOrToken::Node(_) => true,
                rowan::NodeOrToken::Token(t) => !t.kind().is_trivia(),
            })
            .and_then(|it| it.into_token())
            .map(|t| t.kind() == SyntaxKind::ColonColon)
            .unwrap_or(false)
    }
}

impl PathSegment {
    /// Returns the token for this path segment: identifier, `self`, `super`, or `crate`.
    pub fn name_token(&self) -> Option<SyntaxToken> {
        support::token(&self.syntax, |k| {
            matches!(
                k,
                SyntaxKind::Ident | SyntaxKind::SelfKw | SyntaxKind::SuperKw | SyntaxKind::CrateKw
            )
        })
    }
}

impl ModDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    /// Returns `None` for `mod foo;` and the nested items for `mod foo { ... }`.
    pub fn items(&self) -> Option<impl Iterator<Item = Stmt> + '_> {
        // Use the presence of `{` to decide whether the module has a body.
        let has_brace = self
            .syntax
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|t| t.kind() == SyntaxKind::LBrace);

        if has_brace {
            Some(support::children::<Stmt>(&self.syntax))
        } else {
            None
        }
    }
}

impl UseDecl {
    pub fn use_tree(&self) -> Option<UseTree> {
        support::child(&self.syntax)
    }
}

impl UseTree {
    pub fn path(&self) -> Option<Path> {
        support::child(&self.syntax)
    }

    pub fn alias(&self) -> Option<SyntaxToken> {
        let mut iter = self
            .syntax
            .children_with_tokens()
            .filter_map(|it| it.into_token());
        while let Some(t) = iter.next() {
            if t.kind() == SyntaxKind::As {
                return iter.find(|t| t.kind() == SyntaxKind::Ident);
            }
        }
        None
    }

    pub fn is_glob(&self) -> bool {
        self.syntax
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|t| t.kind() == SyntaxKind::Star)
    }

    pub fn subtree_list(&self) -> Option<UseTreeList> {
        support::child(&self.syntax)
    }
}

impl UseTreeList {
    pub fn trees(&self) -> impl Iterator<Item = UseTree> + '_ {
        support::children(&self.syntax)
    }
}

// == new item accessors ==

impl EnumDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn variants(&self) -> impl Iterator<Item = EnumVariant> + '_ {
        support::children(&self.syntax)
    }
}

impl EnumVariant {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    /// Tuple-variant payload types, e.g. `Foo(A, B)`.
    pub fn tuple_types(&self) -> impl Iterator<Item = Type> + '_ {
        support::children(&self.syntax)
    }

    /// Struct-variant fields, e.g. `Foo { x: T }`.
    pub fn field_list(&self) -> Option<StructFieldList> {
        support::child(&self.syntax)
    }
}

impl TraitDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    /// Trait method signatures (parsed as `FuncDecl` without a body).
    pub fn methods(&self) -> impl Iterator<Item = FuncDecl> + '_ {
        support::children(&self.syntax)
    }

    pub fn type_aliases(&self) -> impl Iterator<Item = TypeAliasDecl> + '_ {
        support::children(&self.syntax)
    }
}

impl ImplDecl {
    pub fn generic_params(&self) -> Option<GenericParams> {
        support::child(&self.syntax)
    }

    /// The implementing type's path (the `T` in `impl T` or `impl Trait for T`).
    pub fn path(&self) -> Option<Path> {
        support::child(&self.syntax)
    }

    /// The trait type when this is a trait impl (`impl Trait for T`).
    pub fn trait_type(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn methods(&self) -> impl Iterator<Item = FuncDecl> + '_ {
        support::children(&self.syntax)
    }

    pub fn consts(&self) -> impl Iterator<Item = ConstDecl> + '_ {
        support::children(&self.syntax)
    }

    pub fn type_aliases(&self) -> impl Iterator<Item = TypeAliasDecl> + '_ {
        support::children(&self.syntax)
    }
}

impl GenericParams {
    pub fn names(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        self.syntax
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .filter(|t| t.kind() == SyntaxKind::Ident)
    }
}

impl ConstDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn value(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl TypeAliasDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }
}

// == new expression accessors ==

impl FloatLitExpr {
    pub fn value_token(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Float)
    }

    pub fn value(&self) -> Option<f64> {
        let text = self.value_token()?;
        let text = text.text();
        let trimmed = ["f16", "f32", "f64", "f128"]
            .iter()
            .find_map(|suffix| text.strip_suffix(suffix))
            .unwrap_or(text);
        trimmed.parse().ok()
    }
}

impl StringLitExpr {
    pub fn value_token(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::String)
    }
}

impl CharLitExpr {
    pub fn value_token(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Char)
    }
}

impl BoolLitExpr {
    pub fn value(&self) -> Option<bool> {
        let t = support::token(&self.syntax, |k| {
            matches!(k, SyntaxKind::True | SyntaxKind::False)
        })?;
        Some(t.kind() == SyntaxKind::True)
    }
}

impl MatchExpr {
    pub fn scrutinee(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn arms(&self) -> impl Iterator<Item = MatchArm> + '_ {
        support::children(&self.syntax)
    }
}

impl MatchArm {
    pub fn pattern(&self) -> Option<Pattern> {
        support::child(&self.syntax)
    }

    pub fn guard(&self) -> Option<Expr> {
        // The guard is the first expression child; the body is the last.
        let mut exprs = support::children::<Expr>(&self.syntax);
        let first = exprs.next();
        // If there are two expressions, the first is the guard.
        if exprs.next().is_some() { first } else { None }
    }

    pub fn body(&self) -> Option<Expr> {
        support::last_child(&self.syntax)
    }
}

impl ArrayExpr {
    pub fn elements(&self) -> impl Iterator<Item = Expr> + '_ {
        support::children(&self.syntax)
    }
}

// == new type accessors ==

impl TupleType {
    pub fn elements(&self) -> impl Iterator<Item = Type> + '_ {
        support::children(&self.syntax)
    }
}

impl ArrayType {
    pub fn element(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn len_expr(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

// == pattern accessors ==

impl LiteralPat {
    pub fn literal_token(&self) -> Option<SyntaxToken> {
        support::token(&self.syntax, |k| {
            matches!(
                k,
                SyntaxKind::Number
                    | SyntaxKind::Float
                    | SyntaxKind::String
                    | SyntaxKind::Char
                    | SyntaxKind::True
                    | SyntaxKind::False
            )
        })
    }
}

impl TuplePat {
    pub fn elements(&self) -> impl Iterator<Item = Pattern> + '_ {
        support::children(&self.syntax)
    }
}

impl StructPattern {
    pub fn path(&self) -> Option<Path> {
        support::child(&self.syntax)
    }

    /// The field name token for `field` or `field: pat`.
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn sub_pattern(&self) -> Option<Pattern> {
        support::child(&self.syntax)
    }
}

impl EnumPattern {
    pub fn path(&self) -> Option<Path> {
        support::child(&self.syntax)
    }

    /// Sub-patterns for tuple-style `Variant(a, b)`.
    pub fn elements(&self) -> impl Iterator<Item = Pattern> + '_ {
        support::children(&self.syntax)
    }

    /// Field sub-patterns for struct-style `Variant { a, b }`.
    pub fn fields(&self) -> impl Iterator<Item = StructPattern> + '_ {
        support::children(&self.syntax)
    }
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Wildcard(WildcardPat),
    Literal(LiteralPat),
    Tuple(TuplePat),
    Struct(StructPattern),
    Enum(EnumPattern),
}

impl AstNode for Pattern {
    fn cast(node: SyntaxNode) -> Option<Self> {
        match node.kind() {
            SyntaxKind::WildcardPattern => Some(Pattern::Wildcard(WildcardPat { syntax: node })),
            SyntaxKind::LiteralPattern => Some(Pattern::Literal(LiteralPat { syntax: node })),
            SyntaxKind::TuplePattern => Some(Pattern::Tuple(TuplePat { syntax: node })),
            SyntaxKind::StructPattern => Some(Pattern::Struct(StructPattern { syntax: node })),
            SyntaxKind::EnumPattern => Some(Pattern::Enum(EnumPattern { syntax: node })),
            _ => None,
        }
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Pattern::Wildcard(it) => it.syntax(),
            Pattern::Literal(it) => it.syntax(),
            Pattern::Tuple(it) => it.syntax(),
            Pattern::Struct(it) => it.syntax(),
            Pattern::Enum(it) => it.syntax(),
        }
    }
}

impl Pattern {
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        <Self as AstNode>::cast(node)
    }
}

#[derive(Debug, Clone)]
pub enum Stmt {
    VarDecl(VarDecl),
    FuncDecl(FuncDecl),
    StructDecl(StructDecl),
    EnumDecl(EnumDecl),
    TraitDecl(TraitDecl),
    ImplDecl(ImplDecl),
    ConstDecl(ConstDecl),
    TypeAliasDecl(TypeAliasDecl),
    ReturnStmt(ReturnStmt),
    ExprStmt(ExprStmt),
    ModDecl(ModDecl),
    UseDecl(UseDecl),
}

impl AstNode for Stmt {
    fn cast(node: SyntaxNode) -> Option<Self> {
        match node.kind() {
            SyntaxKind::VarDecl => Some(Stmt::VarDecl(VarDecl { syntax: node })),
            SyntaxKind::FuncDecl => Some(Stmt::FuncDecl(FuncDecl { syntax: node })),
            SyntaxKind::StructDecl => Some(Stmt::StructDecl(StructDecl { syntax: node })),
            SyntaxKind::EnumDecl => Some(Stmt::EnumDecl(EnumDecl { syntax: node })),
            SyntaxKind::TraitDecl => Some(Stmt::TraitDecl(TraitDecl { syntax: node })),
            SyntaxKind::ImplDecl => Some(Stmt::ImplDecl(ImplDecl { syntax: node })),
            SyntaxKind::ConstDecl => Some(Stmt::ConstDecl(ConstDecl { syntax: node })),
            SyntaxKind::TypeAliasDecl => Some(Stmt::TypeAliasDecl(TypeAliasDecl { syntax: node })),
            SyntaxKind::ReturnStmt => Some(Stmt::ReturnStmt(ReturnStmt { syntax: node })),
            SyntaxKind::ExprStmt => Some(Stmt::ExprStmt(ExprStmt { syntax: node })),
            SyntaxKind::ModDecl => Some(Stmt::ModDecl(ModDecl { syntax: node })),
            SyntaxKind::UseDecl => Some(Stmt::UseDecl(UseDecl { syntax: node })),
            _ => None,
        }
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Stmt::VarDecl(it) => it.syntax(),
            Stmt::FuncDecl(it) => it.syntax(),
            Stmt::StructDecl(it) => it.syntax(),
            Stmt::EnumDecl(it) => it.syntax(),
            Stmt::TraitDecl(it) => it.syntax(),
            Stmt::ImplDecl(it) => it.syntax(),
            Stmt::ConstDecl(it) => it.syntax(),
            Stmt::TypeAliasDecl(it) => it.syntax(),
            Stmt::ReturnStmt(it) => it.syntax(),
            Stmt::ExprStmt(it) => it.syntax(),
            Stmt::ModDecl(it) => it.syntax(),
            Stmt::UseDecl(it) => it.syntax(),
        }
    }
}

impl Stmt {
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        <Self as AstNode>::cast(node)
    }
}

#[derive(Debug, Clone)]
pub enum Expr {
    BinaryExpr(BinaryExpr),
    UnaryExpr(UnaryExpr),
    ParenExpr(ParenExpr),
    CallExpr(CallExpr),
    FieldExpr(FieldExpr),
    StructExpr(StructExpr),
    Block(Block),
    IfStmt(IfStmt),
    WhileStmt(WhileStmt),
    MatchExpr(MatchExpr),
    ArrayExpr(ArrayExpr),
    Number(NumberExpr),
    Float(FloatLitExpr),
    StringLit(StringLitExpr),
    CharLit(CharLitExpr),
    BoolLit(BoolLitExpr),
    NameRef(NameRefExpr),
}

impl AstNode for Expr {
    fn cast(node: SyntaxNode) -> Option<Self> {
        let kind = node.kind();
        match kind {
            SyntaxKind::BinaryExpr => Some(Expr::BinaryExpr(BinaryExpr { syntax: node })),
            SyntaxKind::UnaryExpr => Some(Expr::UnaryExpr(UnaryExpr { syntax: node })),
            SyntaxKind::ParenExpr => Some(Expr::ParenExpr(ParenExpr { syntax: node })),
            SyntaxKind::CallExpr => Some(Expr::CallExpr(CallExpr { syntax: node })),
            SyntaxKind::FieldExpr => Some(Expr::FieldExpr(FieldExpr { syntax: node })),
            SyntaxKind::StructExpr => Some(Expr::StructExpr(StructExpr { syntax: node })),
            SyntaxKind::Block => Some(Expr::Block(Block { syntax: node })),
            SyntaxKind::IfStmt => Some(Expr::IfStmt(IfStmt { syntax: node })),
            SyntaxKind::WhileStmt => Some(Expr::WhileStmt(WhileStmt { syntax: node })),
            SyntaxKind::MatchExpr => Some(Expr::MatchExpr(MatchExpr { syntax: node })),
            SyntaxKind::ArrayExpr => Some(Expr::ArrayExpr(ArrayExpr { syntax: node })),
            SyntaxKind::NumberLit => Some(Expr::Number(NumberExpr { syntax: node })),
            SyntaxKind::FloatLit => Some(Expr::Float(FloatLitExpr { syntax: node })),
            SyntaxKind::StringLit => Some(Expr::StringLit(StringLitExpr { syntax: node })),
            SyntaxKind::CharLit => Some(Expr::CharLit(CharLitExpr { syntax: node })),
            SyntaxKind::BoolLit => Some(Expr::BoolLit(BoolLitExpr { syntax: node })),
            SyntaxKind::NameRef => Some(Expr::NameRef(NameRefExpr { syntax: node })),
            _ => None,
        }
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Expr::BinaryExpr(it) => it.syntax(),
            Expr::UnaryExpr(it) => it.syntax(),
            Expr::ParenExpr(it) => it.syntax(),
            Expr::CallExpr(it) => it.syntax(),
            Expr::FieldExpr(it) => it.syntax(),
            Expr::StructExpr(it) => it.syntax(),
            Expr::Block(it) => it.syntax(),
            Expr::IfStmt(it) => it.syntax(),
            Expr::WhileStmt(it) => it.syntax(),
            Expr::MatchExpr(it) => it.syntax(),
            Expr::ArrayExpr(it) => it.syntax(),
            Expr::Number(it) => it.syntax(),
            Expr::Float(it) => it.syntax(),
            Expr::StringLit(it) => it.syntax(),
            Expr::CharLit(it) => it.syntax(),
            Expr::BoolLit(it) => it.syntax(),
            Expr::NameRef(it) => it.syntax(),
        }
    }
}

impl Expr {
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        <Self as AstNode>::cast(node)
    }
}

#[derive(Debug, Clone)]
pub enum Type {
    Named(NamedType),
    Ref(RefType),
    Tuple(TupleType),
    Array(ArrayType),
}

impl AstNode for Type {
    fn cast(node: SyntaxNode) -> Option<Self> {
        let kind = node.kind();
        match kind {
            SyntaxKind::RefType => Some(Type::Ref(RefType { syntax: node })),
            SyntaxKind::NamedType => Some(Type::Named(NamedType { syntax: node })),
            SyntaxKind::TupleType => Some(Type::Tuple(TupleType { syntax: node })),
            SyntaxKind::ArrayType => Some(Type::Array(ArrayType { syntax: node })),
            _ => None,
        }
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Type::Named(it) => it.syntax(),
            Type::Ref(it) => it.syntax(),
            Type::Tuple(it) => it.syntax(),
            Type::Array(it) => it.syntax(),
        }
    }
}

impl Type {
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        <Self as AstNode>::cast(node)
    }
}
