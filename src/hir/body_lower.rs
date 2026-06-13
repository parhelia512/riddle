use la_arena::Arena;

use crate::{
    ast::{self, ElseBranch},
    frontend::syntax_kind::{SyntaxKind, SyntaxToken},
};

use super::{
    body::{BinaryOp, Body, Diagnostic, Expr, ExprId, Stmt, StmtId, UnaryOp},
    item_tree::HirTypeRef,
    lower::{Lower, lower_name},
};

pub struct BodyLower {
    exprs: Arena<Expr>,
    stmts: Arena<Stmt>,
    diagnostics: Vec<Diagnostic>,
}

impl BodyLower {
    pub fn lower(block: ast::Block) -> Body {
        let mut lower = BodyLower {
            exprs: Arena::new(),
            stmts: Arena::new(),
            diagnostics: Vec::new(),
        };

        let root_block = lower.lower_block(block);

        Body {
            exprs: lower.exprs,
            stmts: lower.stmts,
            root_block,
            diagnostics: lower.diagnostics,
        }
    }

    fn alloc_expr(&mut self, expr: Expr) -> ExprId {
        self.exprs.alloc(expr)
    }

    fn alloc_stmt(&mut self, stmt: Stmt) -> StmtId {
        self.stmts.alloc(stmt)
    }

    fn diagnostic(&mut self, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic {
            message: message.into(),
        });
    }

    fn missing_expr(&mut self, message: impl Into<String>) -> ExprId {
        self.diagnostic(message);
        self.alloc_expr(Expr::Missing)
    }

    fn lower_optional_expr(&mut self, expr: Option<ast::Expr>) -> Option<ExprId> {
        expr.map(|expr| self.lower_expr(expr))
    }

    fn lower_required_expr(
        &mut self,
        expr: Option<ast::Expr>,
        message: impl Into<String>,
    ) -> ExprId {
        match expr {
            Some(expr) => self.lower_expr(expr),
            None => self.missing_expr(message),
        }
    }

    fn lower_required_block(
        &mut self,
        block: Option<ast::Block>,
        message: impl Into<String>,
    ) -> ExprId {
        match block {
            Some(block) => self.lower_block(block),
            None => self.missing_expr(message),
        }
    }

    fn lower_arg_list(&mut self, arg_list: Option<ast::ArgList>) -> Vec<ExprId> {
        arg_list
            .map(|args| args.args().map(|arg| self.lower_expr(arg)).collect())
            .unwrap_or_default()
    }

    fn lower_optional_type(&mut self, ty: Option<ast::Type>) -> HirTypeRef {
        ty.map(|ty| ty.lower()).unwrap_or(HirTypeRef::Unknown)
    }

    fn lower_block(&mut self, block: ast::Block) -> ExprId {
        let stmts = block
            .stmts()
            .filter_map(|stmt| self.lower_stmt(stmt))
            .collect();

        let tail = self.lower_optional_expr(block.tail_expr());

        self.alloc_expr(Expr::Block { stmts, tail })
    }

    fn lower_stmt(&mut self, stmt: ast::Stmt) -> Option<StmtId> {
        match stmt {
            ast::Stmt::VarDecl(var) => {
                let name = lower_name(var.name());
                let ty = self.lower_optional_type(var.ty());
                let init = self.lower_optional_expr(var.init());

                Some(self.alloc_stmt(Stmt::Let { name, ty, init }))
            }

            ast::Stmt::ReturnStmt(ret) => {
                let value = self.lower_optional_expr(ret.value());

                Some(self.alloc_stmt(Stmt::Return { value }))
            }

            ast::Stmt::ExprStmt(expr_stmt) => {
                let expr =
                    self.lower_required_expr(expr_stmt.expr(), "missing expression statement");

                Some(self.alloc_stmt(Stmt::Expr { expr }))
            }

            ast::Stmt::FuncDecl(_) | ast::Stmt::StructDecl(_) => {
                self.diagnostic(
                    "nested item declarations are not supported in function bodies yet",
                );

                None
            }
        }
    }

    fn lower_expr(&mut self, expr: ast::Expr) -> ExprId {
        match expr {
            ast::Expr::Number(number) => {
                let value = number.value().unwrap_or_else(|| {
                    self.diagnostic("invalid integer literal");
                    0
                });

                self.alloc_expr(Expr::IntLiteral { value })
            }

            ast::Expr::NameRef(name_ref) => {
                let name = lower_name(name_ref.name());

                self.alloc_expr(Expr::NameRef {
                    name,
                    resolved: None,
                })
            }

            ast::Expr::ParenExpr(paren) => {
                self.lower_required_expr(paren.inner(), "missing parenthesized expression")
            }

            ast::Expr::BinaryExpr(binary) => {
                let lhs =
                    self.lower_required_expr(binary.lhs(), "missing lhs of binary expression");

                let rhs =
                    self.lower_required_expr(binary.rhs(), "missing rhs of binary expression");

                let Some(op) = binary.op_token().and_then(lower_binary_op) else {
                    return self.missing_expr("missing binary operator");
                };

                self.alloc_expr(Expr::Binary { lhs, rhs, op })
            }

            ast::Expr::UnaryExpr(unary) => {
                let Some(token) = unary.op_token() else {
                    return self.missing_expr("missing unary operator");
                };

                let operand = self.lower_required_expr(unary.operand(), "missing unary operand");

                if token.kind() == SyntaxKind::AmpAmp {
                    let inner = self.alloc_expr(Expr::Unary {
                        operand,
                        op: UnaryOp::Ref,
                    });

                    return self.alloc_expr(Expr::Unary {
                        operand: inner,
                        op: UnaryOp::Ref,
                    });
                }

                let Some(op) = lower_unary_op(Some(token)) else {
                    return self.missing_expr("unknown unary operator");
                };

                self.alloc_expr(Expr::Unary { operand, op })
            }

            ast::Expr::Block(block) => self.lower_block(block),

            ast::Expr::IfStmt(if_stmt) => {
                let cond = self.lower_required_expr(if_stmt.condition(), "missing if condition");

                let then_branch =
                    self.lower_required_block(if_stmt.then_branch(), "missing if body");

                let else_branch = match if_stmt.else_branch() {
                    Some(ElseBranch::Block(block)) => Some(self.lower_block(block)),

                    Some(ElseBranch::IfStmt(if_stmt)) => {
                        Some(self.lower_expr(ast::Expr::IfStmt(if_stmt)))
                    }

                    None => None,
                };

                self.alloc_expr(Expr::If {
                    cond,
                    then_branch,
                    else_branch,
                })
            }

            ast::Expr::WhileStmt(while_stmt) => {
                let condition =
                    self.lower_required_expr(while_stmt.condition(), "missing while condition");

                let body = self.lower_required_block(while_stmt.body(), "missing while body");

                self.alloc_expr(Expr::While { condition, body })
            }

            ast::Expr::CallExpr(call) => {
                let callee = self.lower_required_expr(call.callee(), "missing call callee");

                let args = self.lower_arg_list(call.arg_list());

                self.alloc_expr(Expr::Call { callee, args })
            }

            ast::Expr::FieldExpr(field_expr) => {
                let base = self.lower_required_expr(field_expr.base(), "missing field base");

                let field = lower_name(field_expr.field_name());

                self.alloc_expr(Expr::FieldAccess { base, field })
            }
        }
    }
}

fn lower_binary_op(token: SyntaxToken) -> Option<BinaryOp> {
    match token.kind() {
        SyntaxKind::Plus => Some(BinaryOp::Add),
        SyntaxKind::Minus => Some(BinaryOp::Sub),
        SyntaxKind::Star => Some(BinaryOp::Mul),
        SyntaxKind::Slash => Some(BinaryOp::Div),
        SyntaxKind::Percent => Some(BinaryOp::Mod),

        SyntaxKind::EqEq => Some(BinaryOp::Eq),
        SyntaxKind::BangEq => Some(BinaryOp::Neq),
        SyntaxKind::Less => Some(BinaryOp::Lt),
        SyntaxKind::Greater => Some(BinaryOp::Gt),
        SyntaxKind::LessEq => Some(BinaryOp::LtEq),
        SyntaxKind::GreaterEq => Some(BinaryOp::GtEq),

        SyntaxKind::AmpAmp => Some(BinaryOp::And),
        SyntaxKind::PipePipe => Some(BinaryOp::Or),

        _ => None,
    }
}

fn lower_unary_op(token: Option<SyntaxToken>) -> Option<UnaryOp> {
    match token.map(|t| t.kind()) {
        Some(SyntaxKind::Plus) => Some(UnaryOp::Pos),
        Some(SyntaxKind::Minus) => Some(UnaryOp::Neg),
        Some(SyntaxKind::Amp) => Some(UnaryOp::Ref),
        // Some(SyntaxKind::AmpAmp) => Some(UnaryOp::Ref),
        Some(SyntaxKind::Star) => Some(UnaryOp::Deref),
        Some(SyntaxKind::Bang) => Some(UnaryOp::Not),

        _ => None,
    }
}
