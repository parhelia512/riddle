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
        self.syntax.children().filter_map(Stmt::cast)
    }
}

impl VarDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn ty(&self) -> Option<Type> {
        self.syntax.children().find_map(Type::cast)
    }

    pub fn init(&self) -> Option<Expr> {
        self.syntax.children().find_map(Expr::cast)
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
        self.syntax.children().find_map(Type::cast)
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
        self.syntax.children().find_map(Type::cast)
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
        self.syntax.children().find_map(Type::cast)
    }
}

impl Block {
    pub fn stmts(&self) -> impl Iterator<Item = Stmt> + '_ {
        self.syntax.children().filter_map(Stmt::cast)
    }

    pub fn tail_expr(&self) -> Option<Expr> {
        self.syntax.children().filter_map(Expr::cast).last()
    }
}

impl IfStmt {
    pub fn condition(&self) -> Option<Expr> {
        self.syntax.children().find_map(Expr::cast)
    }

    pub fn then_branch(&self) -> Option<Block> {
        support::child(&self.syntax)
    }

    pub fn else_branch(&self) -> Option<ElseBranch> {
        let mut blocks = self.syntax.children().filter_map(Block::cast);
        let _then = blocks.next(); // skip then
        if let Some(else_block) = blocks.next() {
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
        self.syntax.children().find_map(Expr::cast)
    }

    pub fn body(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

impl ReturnStmt {
    pub fn value(&self) -> Option<Expr> {
        self.syntax.children().find_map(Expr::cast)
    }
}

impl ExprStmt {
    pub fn expr(&self) -> Option<Expr> {
        self.syntax.children().find_map(Expr::cast)
    }
}

impl BinaryExpr {
    pub fn lhs(&self) -> Option<Expr> {
        self.syntax.children().find_map(Expr::cast)
    }

    pub fn rhs(&self) -> Option<Expr> {
        let mut exprs = self.syntax.children().filter_map(Expr::cast);
        exprs.next(); // skip lhs
        exprs.next()
    }

    pub fn op_token(&self) -> Option<SyntaxToken> {
        self.syntax
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .find(|t| {
                let kind = t.kind();
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
        self.syntax.children().find_map(Expr::cast)
    }

    pub fn op_token(&self) -> Option<SyntaxToken> {
        self.syntax
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .find(|t| {
                let kind = t.kind();
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
        self.syntax.children().find_map(Expr::cast)
    }
}

impl RefType {
    pub fn inner(&self) -> Option<Type> {
        self.syntax.children().find_map(Type::cast)
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
        self.syntax.children().find_map(Expr::cast)
    }

    pub fn arg_list(&self) -> Option<ArgList> {
        support::child(&self.syntax)
    }
}

impl ArgList {
    pub fn args(&self) -> impl Iterator<Item = Expr> + '_ {
        self.syntax.children().filter_map(Expr::cast)
    }
}

impl FieldExpr {
    pub fn base(&self) -> Option<Expr> {
        self.syntax.children().find_map(Expr::cast)
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

impl Stmt {
    pub fn cast(node: SyntaxNode) -> Option<Self> {
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

impl Expr {
    pub fn cast(node: SyntaxNode) -> Option<Self> {
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
}

#[derive(Debug, Clone)]
pub enum Type {
    Named(NamedType),
    Ref(RefType),
}

impl Type {
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        let kind = node.kind();
        match kind {
            SyntaxKind::RefType => Some(Type::Ref(RefType { syntax: node })),
            SyntaxKind::NamedType => Some(Type::Named(NamedType { syntax: node })),
            _ => None,
        }
    }
}
