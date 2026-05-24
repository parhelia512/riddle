#[derive(Debug, Clone)]
pub enum Node {
    Number(i64),
    // Expr
    Unary {
        op: String,
        expr: Box<Node>, // expr
    },
    Binary {
        op: String,
        lhs: Box<Node>, // expr
        rhs: Box<Node>, // expr
    },
    // Expr & Type
    Symbol {
        name: String,
    },
    ExprStmt(Box<Node>),
    // Stmt
    VarDecl {
        name: String,
        ty: Option<Box<Node>>,   // type
        init: Option<Box<Node>>, // expr
    },
    FuncDecl {
        name: String,
        params: Vec<(String, Node)>,
        ret: Option<Box<Node>>,  // type
        body: Option<Box<Node>>, // stmt
    },
    Block(Vec<Node>),
}
