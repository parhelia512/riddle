use super::support::{self, AstNode};
use crate::frontend::syntax_kind::{SyntaxKind, SyntaxNode, SyntaxToken};

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
                SyntaxKind::Plus
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
        self.value_token()?.text().parse().ok()
    }
}

impl NamedType {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }
}

impl NameRefExpr {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
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

#[derive(Debug, Clone)]
pub enum Stmt {
    VarDecl(VarDecl),
    FuncDecl(FuncDecl),
    StructDecl(StructDecl),
    ReturnStmt(ReturnStmt),
    ExprStmt(ExprStmt),
}

impl AstNode for Stmt {
    fn cast(node: SyntaxNode) -> Option<Self> {
        let kind = node.kind();
        match kind {
            SyntaxKind::VarDecl => Some(Stmt::VarDecl(VarDecl { syntax: node })),
            SyntaxKind::FuncDecl => Some(Stmt::FuncDecl(FuncDecl { syntax: node })),
            SyntaxKind::StructDecl => Some(Stmt::StructDecl(StructDecl { syntax: node })),
            SyntaxKind::ReturnStmt => Some(Stmt::ReturnStmt(ReturnStmt { syntax: node })),
            SyntaxKind::ExprStmt => Some(Stmt::ExprStmt(ExprStmt { syntax: node })),
            _ => None,
        }
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Stmt::VarDecl(it) => it.syntax(),
            Stmt::FuncDecl(it) => it.syntax(),
            Stmt::StructDecl(it) => it.syntax(),
            Stmt::ReturnStmt(it) => it.syntax(),
            Stmt::ExprStmt(it) => it.syntax(),
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
    Block(Block),
    IfStmt(IfStmt),
    WhileStmt(WhileStmt),
    Number(NumberExpr),
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
            SyntaxKind::Block => Some(Expr::Block(Block { syntax: node })),
            SyntaxKind::IfStmt => Some(Expr::IfStmt(IfStmt { syntax: node })),
            SyntaxKind::WhileStmt => Some(Expr::WhileStmt(WhileStmt { syntax: node })),
            SyntaxKind::NumberLit => Some(Expr::Number(NumberExpr { syntax: node })),
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
            Expr::Block(it) => it.syntax(),
            Expr::IfStmt(it) => it.syntax(),
            Expr::WhileStmt(it) => it.syntax(),
            Expr::Number(it) => it.syntax(),
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
}

impl AstNode for Type {
    fn cast(node: SyntaxNode) -> Option<Self> {
        let kind = node.kind();
        match kind {
            SyntaxKind::RefType => Some(Type::Ref(RefType { syntax: node })),
            SyntaxKind::NamedType => Some(Type::Named(NamedType { syntax: node })),
            _ => None,
        }
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Type::Named(it) => it.syntax(),
            Type::Ref(it) => it.syntax(),
        }
    }
}

impl Type {
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        <Self as AstNode>::cast(node)
    }
}
