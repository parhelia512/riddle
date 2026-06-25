use la_arena::Arena;

use ast::{self, ElseBranch, support::AstNode};
use frontend::syntax_kind::{SyntaxKind, SyntaxToken};
use rowan::ast::SyntaxNodePtr;

use super::{
    HirFile,
    body::{
        BinaryOp, Body, BodyItem, Diagnostic, Expr, ExprId, FieldPat, MatchArm, PatId, Pattern,
        Stmt, StmtId, StructExprField, UnaryOp,
    },
    item_tree::HirTypeRef,
<<<<<<< HEAD
    item_tree::{HirPath, PathAnchor},
=======
>>>>>>> 0d7abe0350871a575608ce4fc1d8aae9223abb1c
    lower::{Lower, lower_name},
};

pub struct BodyLower<'a> {
    hir: &'a mut HirFile,
    exprs: Arena<Expr>,
    stmts: Arena<Stmt>,
    pats: Arena<Pattern>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> BodyLower<'a> {
    pub fn lower(hir: &'a mut HirFile, block: ast::Block) -> Body {
        let root_ptr = SyntaxNodePtr::new(block.syntax());
        let mut lower = BodyLower {
            hir,
            exprs: Arena::new(),
            stmts: Arena::new(),
            pats: Arena::new(),
            diagnostics: Vec::new(),
        };
        let root_block = lower.lower_block(block);
        Body {
            exprs: lower.exprs,
            stmts: lower.stmts,
            pats: lower.pats,
            root_block,
            root_ptr,
            diagnostics: lower.diagnostics,
        }
    }

    fn alloc_expr(&mut self, expr: Expr) -> ExprId {
        self.exprs.alloc(expr)
    }
    fn alloc_stmt(&mut self, stmt: Stmt) -> StmtId {
        self.stmts.alloc(stmt)
    }

    fn alloc_pat(&mut self, pat: Pattern) -> PatId {
        self.pats.alloc(pat)
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

    fn lower_required_expr(&mut self, expr: Option<ast::Expr>, msg: impl Into<String>) -> ExprId {
        match expr {
            Some(e) => self.lower_expr(e),
            None => self.missing_expr(msg),
        }
    }

    fn lower_required_block(
        &mut self,
        block: Option<ast::Block>,
        msg: impl Into<String>,
    ) -> ExprId {
        match block {
            Some(b) => self.lower_block(b),
            None => self.missing_expr(msg),
        }
    }

    fn lower_arg_list(&mut self, arg_list: Option<ast::ArgList>) -> Vec<ExprId> {
        arg_list
            .map(|args| args.args().map(|a| self.lower_expr(a)).collect())
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

            ast::Stmt::ExprStmt(es) => {
                let expr = self.lower_required_expr(es.expr(), "missing expression statement");
                Some(self.alloc_stmt(Stmt::Expr { expr }))
            }

            ast::Stmt::ModDecl(m) => {
                let mid = crate::lower_mod_decl(self.hir, m);
                Some(self.alloc_stmt(Stmt::Item {
                    item: BodyItem::Module(mid),
                }))
            }

            ast::Stmt::UseDecl(u) => {
                let Some(tree_ast) = u.use_tree() else {
                    self.diagnostic("malformed use declaration");
                    return None;
                };
                let tree = tree_ast.lower();
                let uid = self
                    .hir
                    .item_tree
                    .uses
                    .alloc(crate::item_tree::HirUse { tree });
                Some(self.alloc_stmt(Stmt::Item {
                    item: BodyItem::Use(uid),
                }))
            }

            // Top-level declarations inside bodies are allowed and are promoted to the global item tree.
            ast::Stmt::FuncDecl(func) => {
                let body_ast = func.body();
                let fid = {
                    use crate::lower::AstLower;
                    func.lower(&mut self.hir.item_tree.functions)
                };
                if let Some(block) = body_ast {
                    // Lower the nested function body recursively.
                    let nested_body = BodyLower::lower(self.hir, block);
                    let body_id = self.hir.bodies.alloc(nested_body);
                    self.hir.function_bodies.insert(fid, body_id);
                }
                // Function declarations are already registered in the item tree and do not enter
                // the body statement stream. They have no runtime side effects here.
                None
            }

            ast::Stmt::StructDecl(s) => {
                use crate::lower::AstLower;
                let _sid = s.lower(&mut self.hir.item_tree.structs);
                None
            }

            ast::Stmt::EnumDecl(e) => {
                use crate::lower::AstLower;
                let _eid = e.lower(&mut self.hir.item_tree.enums);
                None
            }

            ast::Stmt::TraitDecl(t) => {
                use crate::lower::AstLower;
                let _tid = t.lower(&mut self.hir.item_tree.traits);
                None
            }

            ast::Stmt::ImplDecl(i) => {
                let _iid = crate::lower_impl_decl(self.hir, i);
                None
            }

            ast::Stmt::ConstDecl(c) => {
                use crate::lower::AstLower;
                let _cid = c.lower(&mut self.hir.item_tree.consts);
                None
            }

            ast::Stmt::TypeAliasDecl(t) => {
                use crate::lower::AstLower;
                let _tid = t.lower(&mut self.hir.item_tree.type_aliases);
                None
            }
        }
    }

    fn lower_expr(&mut self, expr: ast::Expr) -> ExprId {
        match expr {
            ast::Expr::Number(n) => {
<<<<<<< HEAD
                let text = n
                    .value_token()
                    .map(|token| token.text().to_string())
                    .unwrap_or_default();
                let (digits, suffix) = split_int_literal(&text);
                let value = digits.parse().unwrap_or_else(|_| {
                    self.diagnostic("invalid integer literal");
                    0
                });
                self.alloc_expr(Expr::IntLiteral { value, suffix })
            }

            ast::Expr::Float(f) => {
                let text = f
                    .value_token()
                    .map(|token| token.text().to_string())
                    .unwrap_or_default();
                let (number, suffix) = split_float_literal(&text);
                let value = number.parse().unwrap_or_else(|_| {
                    self.diagnostic("invalid float literal");
                    0.0
                });
                self.alloc_expr(Expr::FloatLiteral { value, suffix })
=======
                let value = n.value().unwrap_or_else(|| {
                    self.diagnostic("invalid integer literal");
                    0
                });
                self.alloc_expr(Expr::IntLiteral { value })
            }

            ast::Expr::Float(f) => {
                let value = f.value().unwrap_or_else(|| {
                    self.diagnostic("invalid float literal");
                    0.0
                });
                self.alloc_expr(Expr::FloatLiteral { value })
>>>>>>> 0d7abe0350871a575608ce4fc1d8aae9223abb1c
            }

            ast::Expr::StringLit(s) => {
                let text = s
                    .value_token()
                    .map(|t| t.text().to_string())
                    .unwrap_or_default();
                self.alloc_expr(Expr::StringLiteral { value: text })
            }

            ast::Expr::CharLit(c) => {
                let text = c
                    .value_token()
                    .map(|t| t.text().to_string())
                    .unwrap_or_default();
                self.alloc_expr(Expr::CharLiteral { value: text })
            }

            ast::Expr::BoolLit(b) => {
                let value = b.value().unwrap_or(false);
                self.alloc_expr(Expr::BoolLiteral { value })
            }

            ast::Expr::NameRef(name_ref) => {
                let path = name_ref.path().lower();
                self.alloc_expr(Expr::Path {
                    path,
                    resolved: None,
                })
            }

            ast::Expr::ParenExpr(p) => {
                self.lower_required_expr(p.inner(), "missing parenthesized expression")
            }

            ast::Expr::BinaryExpr(b) => {
                let lhs = self.lower_required_expr(b.lhs(), "missing lhs of binary expression");
                let rhs = self.lower_required_expr(b.rhs(), "missing rhs of binary expression");
                let Some(op) = b.op_token().and_then(lower_binary_op) else {
                    return self.missing_expr("missing binary operator");
                };
                self.alloc_expr(Expr::Binary { lhs, rhs, op })
            }

            ast::Expr::UnaryExpr(u) => {
                let Some(token) = u.op_token() else {
                    return self.missing_expr("missing unary operator");
                };
                let operand = self.lower_required_expr(u.operand(), "missing unary operand");
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

            ast::Expr::Block(b) => self.lower_block(b),

            ast::Expr::IfStmt(i) => {
                let cond = self.lower_required_expr(i.condition(), "missing if condition");
                let then_branch = self.lower_required_block(i.then_branch(), "missing if body");
                let else_branch = match i.else_branch() {
                    Some(ElseBranch::Block(b)) => Some(self.lower_block(b)),
                    Some(ElseBranch::IfStmt(i)) => Some(self.lower_expr(ast::Expr::IfStmt(i))),
                    None => None,
                };
                self.alloc_expr(Expr::If {
                    cond,
                    then_branch,
                    else_branch,
                })
            }

            ast::Expr::WhileStmt(w) => {
                let condition = self.lower_required_expr(w.condition(), "missing while condition");
                let body = self.lower_required_block(w.body(), "missing while body");
                self.alloc_expr(Expr::While { condition, body })
            }

            ast::Expr::CallExpr(c) => {
                let callee = self.lower_required_expr(c.callee(), "missing call callee");
                let args = self.lower_arg_list(c.arg_list());
                self.alloc_expr(Expr::Call { callee, args })
            }

            ast::Expr::MatchExpr(m) => {
                let scrutinee = self.lower_required_expr(m.scrutinee(), "missing match scrutinee");
                let arms = m
                    .arms()
                    .map(|arm| {
                        let pat = self.lower_arm_pattern(arm.pattern());
                        let guard = self.lower_optional_expr(arm.guard());
                        let body = self.lower_required_expr(arm.body(), "missing match arm body");
                        MatchArm { pat, guard, body }
                    })
                    .collect();
                self.alloc_expr(Expr::Match { scrutinee, arms })
            }

            ast::Expr::ArrayExpr(a) => {
                let elements = a.elements().map(|e| self.lower_expr(e)).collect();
                self.alloc_expr(Expr::Array { elements })
            }

            ast::Expr::StructExpr(s) => {
                let fields = s
                    .fields()
                    .map(|field| {
                        let name = lower_name(field.name());
<<<<<<< HEAD
                        let value = field
                            .value()
                            .map(|value| self.lower_expr(value))
                            .unwrap_or_else(|| {
                                let path = HirPath {
                                    anchor: PathAnchor::Plain,
                                    segments: vec![name.clone()],
                                };
                                self.alloc_expr(Expr::Path {
                                    path,
                                    resolved: None,
                                })
                            });
=======
                        let value =
                            self.lower_required_expr(field.value(), "missing struct field value");
>>>>>>> 0d7abe0350871a575608ce4fc1d8aae9223abb1c
                        StructExprField { name, value }
                    })
                    .collect();
                let path = s.path().lower();
                self.alloc_expr(Expr::Struct {
                    path,
                    fields,
                    resolved: None,
                })
            }

            ast::Expr::FieldExpr(f) => {
                let base = self.lower_required_expr(f.base(), "missing field base");
                let field = lower_name(f.field_name());
                self.alloc_expr(Expr::FieldAccess { base, field })
            }
        }
    }

    // == pattern lowering ==

    fn lower_arm_pattern(&mut self, ast_pat: Option<ast::Pattern>) -> PatId {
        match ast_pat {
            Some(pat) => self.lower_pattern(pat),
            None => self.alloc_pat(Pattern::Wildcard),
        }
    }

    fn lower_pattern(&mut self, pat: ast::Pattern) -> PatId {
        match pat {
            ast::Pattern::Wildcard(_) => self.alloc_pat(Pattern::Wildcard),
            ast::Pattern::Literal(_) => self.alloc_pat(Pattern::Literal),
            ast::Pattern::Tuple(tp) => {
                let elements = tp.elements().map(|p| self.lower_pattern(p)).collect();
                self.alloc_pat(Pattern::Tuple { elements })
            }
            ast::Pattern::Struct(sp) => {
                // A `StructPattern` in our AST is a single field pattern.
                // For `Variant { a, b: c }`, each field is parsed as a StructPattern.
                let path = sp.path().lower();
                let name = lower_name(sp.name());
                let sub = sp.sub_pattern().map(|p| self.lower_pattern(p));
                self.alloc_pat(Pattern::Struct {
                    path,
                    fields: vec![FieldPat { name, pat: sub }],
                })
            }
            ast::Pattern::Enum(ep) => {
                let path = ep.path().lower();
                let tuple_elems: Vec<PatId> =
                    ep.elements().map(|p| self.lower_pattern(p)).collect();
                if !tuple_elems.is_empty() {
                    self.alloc_pat(Pattern::TupleStruct {
                        path,
                        elements: tuple_elems,
                    })
                } else {
                    let fields: Vec<FieldPat> = ep
                        .fields()
                        .map(|fp| {
                            let name = lower_name(fp.name());
                            let pat = fp.sub_pattern().map(|p| self.lower_pattern(p));
                            FieldPat { name, pat }
                        })
                        .collect();
                    if fields.is_empty() {
                        // A single plain identifier binds a new local; any other path
                        // (multi-segment or anchored) refers to an existing item.
                        match path.as_single_name() {
                            Some(name) => self.alloc_pat(Pattern::Binding { name: name.clone() }),
                            None => self.alloc_pat(Pattern::Path { path }),
                        }
                    } else {
                        self.alloc_pat(Pattern::Struct { path, fields })
                    }
                }
            }
        }
    }
}

fn lower_binary_op(token: SyntaxToken) -> Option<BinaryOp> {
    match token.kind() {
        SyntaxKind::Eq => Some(BinaryOp::Assign),
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
        Some(SyntaxKind::Star) => Some(UnaryOp::Deref),
        Some(SyntaxKind::Bang) => Some(UnaryOp::Not),
        _ => None,
    }
}
<<<<<<< HEAD

fn split_int_literal(text: &str) -> (&str, Option<String>) {
    let suffix_start = text
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(text.len());
    let (digits, suffix) = text.split_at(suffix_start);
    let suffix = (!suffix.is_empty()).then(|| suffix.to_string());
    (digits, suffix)
}

fn split_float_literal(text: &str) -> (&str, Option<String>) {
    for suffix in ["f16", "f32", "f64", "f128"] {
        if let Some(number) = text.strip_suffix(suffix) {
            return (number, Some(suffix.to_string()));
        }
    }
    (text, None)
}
=======
>>>>>>> 0d7abe0350871a575608ce4fc1d8aae9223abb1c
