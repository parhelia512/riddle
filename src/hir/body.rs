use la_arena::{Arena, Idx};

use super::{Name, item_tree::TypeRef};

pub type ExprId = Idx<Expr>;
pub type StmtId = Idx<Stmt>;

/// A function's body
#[derive(Debug)]
pub struct Body {
    pub exprs: Arena<Expr>,
    pub stmts: Arena<Stmt>,
    pub root_block: ExprId,
    pub diagnostics: Vec<Diagnostics>,
}

#[derive(Debug, Clone)]
pub struct Diagnostics {
    pub message: String,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        name: Name,
        ty: Option<TypeRef>,
        init: Option<ExprId>,
    },
    Expr {
        expr: ExprId,
    },
    Return {
        value: Option<ExprId>,
    },
}

#[derive(Debug, Clone)]
pub enum Expr {
    Missing,

    IntLiteral {
        value: i64,
    },

    NameRef {
        name: Name,
        resolved: Option<ResolvedName>,
    },

    Binary {
        lhs: ExprId,
        rhs: ExprId,
        op: BinaryOp,
    },

    Unary {
        operand: ExprId,
        op: UnaryOp,
    },

    Block {
        stmts: Vec<StmtId>,
        tail: Option<ExprId>,
    },

    If {
        cond: ExprId,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
    },

    While {
        condition: ExprId,
        body: ExprId,
    },

    Call {
        callee: ExprId,
        args: Vec<ExprId>,
    },

    Field {
        base: ExprId,
        field: Name,
    },
}

#[derive(Debug, Clone)]
pub enum ResolvedName {
    Local(StmtId),
    Param(usize),
    Function(super::item_tree::FunctionId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Neq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Neg,
    Pos,
    Ref,
    Deref,
    Not,
}
