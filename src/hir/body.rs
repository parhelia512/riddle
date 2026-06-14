use la_arena::{Arena, Idx};

use super::{Name, item_tree::HirTypeRef};

pub type ExprId = Idx<Expr>;
pub type StmtId = Idx<Stmt>;
pub type BodyId = Idx<Body>;

/// A function's body
#[derive(Debug)]
pub struct Body {
    pub exprs: Arena<Expr>,
    pub stmts: Arena<Stmt>,
    pub root_block: ExprId,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub message: String,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        name: Name,
        ty: HirTypeRef,
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

    FieldAccess {
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

impl Body {
    pub fn pretty(&self) -> PrettyBody<'_> {
        PrettyBody { body: self }
    }
}

pub struct PrettyBody<'a> {
    body: &'a Body,
}

impl std::fmt::Display for PrettyBody<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let printer = BodyPrinter { body: self.body };
        write!(f, "{}", printer.print_body())
    }
}

struct BodyPrinter<'a> {
    body: &'a Body,
}

impl BodyPrinter<'_> {
    fn print_body(&self) -> String {
        let mut out = self.print_expr(self.body.root_block, 0, 0);

        if !self.body.diagnostics.is_empty() {
            out.push_str("\n\n// diagnostics\n");

            for diagnostic in &self.body.diagnostics {
                out.push_str("// - ");
                out.push_str(&diagnostic.message);
                out.push('\n');
            }
        }

        out
    }

    fn print_stmt(&self, stmt: StmtId, indent: usize) -> String {
        match &self.body.stmts[stmt] {
            Stmt::Let { name, ty, init } => {
                let mut out = String::new();

                out.push_str("let ");
                out.push_str(Self::name_text(name));

                if !Self::is_unknown_type(ty) {
                    out.push_str(": ");
                    out.push_str(&Self::type_text(ty));
                }

                if let Some(init) = init {
                    out.push_str(" = ");
                    out.push_str(&self.print_expr(*init, 0, indent));
                }

                out.push(';');
                out
            }

            Stmt::Return { value } => {
                let mut out = String::new();

                out.push_str("return");

                if let Some(value) = value {
                    out.push(' ');
                    out.push_str(&self.print_expr(*value, 0, indent));
                }

                out.push(';');
                out
            }

            Stmt::Expr { expr } => {
                let mut out = self.print_expr(*expr, 0, indent);
                out.push(';');
                out
            }
        }
    }

    fn print_expr(&self, expr: ExprId, parent_prec: u8, indent: usize) -> String {
        let current_prec = self.expr_prec(expr);

        let out = match &self.body.exprs[expr] {
            Expr::Missing => "<missing>".to_string(),

            Expr::IntLiteral { value } => value.to_string(),

            Expr::NameRef { name, .. } => Self::name_text(name).to_string(),

            Expr::Unary { operand, op } => {
                let operand = self.print_expr(*operand, current_prec, indent);
                format!("({}{})", Self::unary_op_text(op), operand)
            }

            Expr::Binary { lhs, rhs, op } => {
                let lhs = self.print_expr(*lhs, current_prec, indent);
                let rhs = self.print_expr(*rhs, current_prec + 1, indent);

                format!("({} {} {})", lhs, Self::binary_op_text(op), rhs)
            }

            Expr::Call { callee, args } => {
                let callee = self.print_expr(*callee, current_prec, indent);

                let args = args
                    .iter()
                    .map(|arg| self.print_expr(*arg, 0, indent))
                    .collect::<Vec<_>>()
                    .join(", ");

                format!("{}({})", callee, args)
            }

            Expr::FieldAccess { base, field } => {
                let base = self.print_expr(*base, current_prec, indent);
                format!("({}.{})", base, Self::name_text(field))
            }

            Expr::Block { stmts, tail } => self.print_block(stmts, *tail, indent),

            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let mut out = String::new();

                out.push_str("if ");
                out.push_str(&self.print_expr(*cond, 0, indent));
                out.push(' ');
                out.push_str(&self.print_block_like(*then_branch, indent));

                if let Some(else_branch) = else_branch {
                    out.push_str(" else ");

                    match &self.body.exprs[*else_branch] {
                        Expr::If { .. } => {
                            out.push_str(&self.print_expr(*else_branch, 0, indent));
                        }

                        _ => {
                            out.push_str(&self.print_block_like(*else_branch, indent));
                        }
                    }
                }

                out
            }

            Expr::While { condition, body } => {
                let mut out = String::new();

                out.push_str("while ");
                out.push_str(&self.print_expr(*condition, 0, indent));
                out.push(' ');
                out.push_str(&self.print_block_like(*body, indent));

                out
            }
        };

        if current_prec < parent_prec {
            format!("({})", out)
        } else {
            out
        }
    }

    fn print_block_like(&self, expr: ExprId, indent: usize) -> String {
        match &self.body.exprs[expr] {
            Expr::Block { stmts, tail } => self.print_block(stmts, *tail, indent),
            _ => self.print_expr(expr, 0, indent),
        }
    }

    fn print_block(&self, stmts: &[StmtId], tail: Option<ExprId>, indent: usize) -> String {
        let mut out = String::new();

        out.push_str("{\n");

        for stmt in stmts {
            out.push_str(&Self::indent(indent + 1));
            out.push_str(&self.print_stmt(*stmt, indent + 1));
            out.push('\n');
        }

        if let Some(tail) = tail {
            out.push_str(&Self::indent(indent + 1));
            out.push_str(&self.print_expr(tail, 0, indent + 1));
            out.push('\n');
        }

        out.push_str(&Self::indent(indent));
        out.push('}');

        out
    }

    fn expr_prec(&self, expr: ExprId) -> u8 {
        match &self.body.exprs[expr] {
            Expr::Missing
            | Expr::IntLiteral { .. }
            | Expr::NameRef { .. } => 100,

            Expr::Call { .. }
            | Expr::FieldAccess { .. } => 90,

            Expr::Unary { .. } => 80,

            Expr::Binary { op, .. } => Self::binary_prec(op),

            Expr::Block { .. }
            | Expr::If { .. }
            | Expr::While { .. } => 0,
        }
    }

    fn binary_prec(op: &BinaryOp) -> u8 {
        match op {
            BinaryOp::Or => 10,
            BinaryOp::And => 20,

            BinaryOp::Eq
            | BinaryOp::Neq => 30,

            BinaryOp::Lt
            | BinaryOp::Gt
            | BinaryOp::LtEq
            | BinaryOp::GtEq => 40,

            BinaryOp::Add
            | BinaryOp::Sub => 50,

            BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Mod => 60,
        }
    }

    fn binary_op_text(op: &BinaryOp) -> &'static str {
        match op {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Mod => "%",

            BinaryOp::Eq => "==",
            BinaryOp::Neq => "!=",
            BinaryOp::Lt => "<",
            BinaryOp::Gt => ">",
            BinaryOp::LtEq => "<=",
            BinaryOp::GtEq => ">=",

            BinaryOp::And => "&&",
            BinaryOp::Or => "||",
        }
    }

    fn unary_op_text(op: &UnaryOp) -> &'static str {
        match op {
            UnaryOp::Pos => "+",
            UnaryOp::Neg => "-",
            UnaryOp::Ref => "&",
            UnaryOp::Deref => "*",
            UnaryOp::Not => "!",
        }
    }

    fn type_text(ty: &super::item_tree::HirTypeRef) -> String {
        match ty {
            super::item_tree::HirTypeRef::Unknown => "_".to_string(),

            super::item_tree::HirTypeRef::Error => "<error>".to_string(),

            super::item_tree::HirTypeRef::Named(name) => {
                Self::name_text(name).to_string()
            }

            super::item_tree::HirTypeRef::Ref(inner) => {
                format!("&{}", Self::type_text(inner))
            }
        }
    }

    fn is_unknown_type(ty: &super::item_tree::HirTypeRef) -> bool {
        matches!(ty, super::item_tree::HirTypeRef::Unknown)
    }

    fn name_text(name: &super::Name) -> &str {
        &name.0
    }

    fn indent(level: usize) -> String {
        "    ".repeat(level)
    }
}
