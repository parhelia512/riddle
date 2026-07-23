use std::collections::HashMap;

use la_arena::Arena;

use ast::{
    self, ElseBranch,
    support::{AstNode, trimmed_range},
};
use frontend::syntax_kind::{SyntaxKind, SyntaxToken};
use rowan::{TextRange, ast::SyntaxNodePtr};

use super::{
    HirFile,
    body::{
        BinaryOp, Body, BodyItem, Diagnostic, Expr, ExprId, FieldPat, LabelStyle, LambdaParam,
        LiteralPattern, MatchArm, PatId, Pattern, Severity, SourceLabel, SourceMap, Stmt, StmtId,
        StructExprField, UnaryOp,
    },
    item_tree::HirTypeRef,
    item_tree::{HirPath, PathAnchor},
    lower::{Lower, lower_name},
};

pub struct BodyLower<'a> {
    hir: &'a mut HirFile,
    exprs: Arena<Expr>,
    stmts: Arena<Stmt>,
    pats: Arena<Pattern>,
    diagnostics: Vec<Diagnostic>,
    /// Source ranges collected during lowering, moved into the Body on finish.
    expr_ranges: HashMap<ExprId, TextRange>,
    stmt_ranges: HashMap<StmtId, TextRange>,
    pat_ranges: HashMap<PatId, TextRange>,
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
            expr_ranges: HashMap::new(),
            stmt_ranges: HashMap::new(),
            pat_ranges: HashMap::new(),
        };
        let root_block = lower.lower_block(block);
        Body {
            exprs: lower.exprs,
            stmts: lower.stmts,
            pats: lower.pats,
            root_block,
            root_ptr,
            diagnostics: lower.diagnostics,
            source_map: SourceMap {
                expr_ranges: lower.expr_ranges,
                stmt_ranges: lower.stmt_ranges,
                pat_ranges: lower.pat_ranges,
            },
        }
    }

    fn alloc_expr(&mut self, expr: Expr, range: TextRange) -> ExprId {
        let id = self.exprs.alloc(expr);
        self.expr_ranges.insert(id, range);
        id
    }
    fn alloc_stmt(&mut self, stmt: Stmt, range: TextRange) -> StmtId {
        let id = self.stmts.alloc(stmt);
        self.stmt_ranges.insert(id, range);
        id
    }

    fn alloc_pat(&mut self, pat: Pattern, range: TextRange) -> PatId {
        let id = self.pats.alloc(pat);
        self.pat_ranges.insert(id, range);
        id
    }

    fn diagnostic(&mut self, message: impl Into<String>, span: TextRange) {
        self.diagnostics.push(Diagnostic {
            code: "E0040",
            severity: Severity::Error,
            message: message.into(),
            labels: vec![SourceLabel {
                range: span,
                message: String::new(),
                style: LabelStyle::Primary,
            }],
            help: None,
            notes: vec![
                "the source code could not be lowered; check for syntax or structural errors"
                    .into(),
            ],
        });
    }

    fn missing_expr(&mut self, message: impl Into<String>, range: TextRange) -> ExprId {
        let msg = message.into();
        self.diagnostic(msg, range);
        self.alloc_expr(Expr::Missing, range)
    }

    fn lower_optional_expr(&mut self, expr: Option<ast::Expr>) -> Option<ExprId> {
        expr.map(|expr| self.lower_expr(expr))
    }

    fn lower_required_expr(
        &mut self,
        expr: Option<ast::Expr>,
        msg: impl Into<String>,
        fallback: TextRange,
    ) -> ExprId {
        match expr {
            Some(e) => self.lower_expr(e),
            None => self.missing_expr(msg, fallback),
        }
    }

    fn lower_required_block(
        &mut self,
        block: Option<ast::Block>,
        msg: impl Into<String>,
        fallback: TextRange,
    ) -> ExprId {
        match block {
            Some(b) => self.lower_block(b),
            None => self.missing_expr(msg, fallback),
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
        let range = trimmed_range(block.syntax());
        let stmts = block
            .stmts()
            .filter_map(|stmt| self.lower_stmt(stmt))
            .collect();
        let tail = self.lower_optional_expr(block.tail_expr());
        self.alloc_expr(Expr::Block { stmts, tail }, range)
    }

    fn lower_stmt(&mut self, stmt: ast::Stmt) -> Option<StmtId> {
        let range = trimmed_range(stmt.syntax());
        match stmt {
            ast::Stmt::VarDecl(var) => {
                let name_token = var.name();
                let name_range = name_token.as_ref().map(|token| token.text_range());
                let name = lower_name(name_token);
                let ty_ast = var.ty();
                let ty_range = ty_ast.as_ref().map(|ty| trimmed_range(ty.syntax()));
                let ty = self.lower_optional_type(ty_ast);
                let init = self.lower_optional_expr(var.init());
                let is_mut = var.is_mut();
                Some(self.alloc_stmt(
                    Stmt::Let {
                        name,
                        name_range,
                        ty,
                        ty_range,
                        init,
                        is_mut,
                    },
                    range,
                ))
            }

            ast::Stmt::ReturnStmt(ret) => {
                let value = self.lower_optional_expr(ret.value());
                Some(self.alloc_stmt(Stmt::Return { value }, range))
            }

            ast::Stmt::BreakStmt(_) => Some(self.alloc_stmt(Stmt::Break, range)),

            ast::Stmt::ContinueStmt(_) => Some(self.alloc_stmt(Stmt::Continue, range)),

            ast::Stmt::ExprStmt(es) => {
                let expr =
                    self.lower_required_expr(es.expr(), "missing expression statement", range);
                Some(self.alloc_stmt(Stmt::Expr { expr }, range))
            }

            ast::Stmt::ModDecl(m) => {
                let mid = crate::lower_mod_decl(self.hir, m);
                Some(self.alloc_stmt(
                    Stmt::Item {
                        item: BodyItem::Module(mid),
                    },
                    range,
                ))
            }

            ast::Stmt::UseDecl(u) => {
                let Some(tree_ast) = u.use_tree() else {
                    self.diagnostic("malformed use declaration", range);
                    return None;
                };
                let tree = tree_ast.lower();
                let attrs = crate::lower::lower_attrs(u.syntax());
                let uid = self.hir.item_tree.uses.alloc(crate::item_tree::HirUse {
                    tree,
                    visibility: crate::item_tree::Visibility::Private,
                    attrs,
                });
                Some(self.alloc_stmt(
                    Stmt::Item {
                        item: BodyItem::Use(uid),
                    },
                    range,
                ))
            }

            // Top-level declarations inside bodies are allowed and are promoted to the global item tree.
            ast::Stmt::FuncDecl(func) => {
                let body_ast = func.body();
                let fid = {
                    use crate::lower::AstLower;
                    func.lower(&mut self.hir.item_tree.functions)
                };
                if let Some(block) = body_ast {
                    let nested_body = BodyLower::lower(self.hir, block);
                    let body_id = self.hir.bodies.alloc(nested_body);
                    self.hir.function_bodies.insert(fid, body_id);
                }
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
                let _tid = crate::lower_trait_decl(self.hir, t);
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

            ast::Stmt::ExternBlock(block) => {
                for func in block.functions() {
                    use crate::lower::AstLower;
                    let fid = func.lower(&mut self.hir.item_tree.functions);
                    self.hir.item_tree.extern_function_ids.push(fid);
                }
                None
            }

            ast::Stmt::ExternFnDecl(decl) => {
                if let Some(func) = decl.func_decl() {
                    use crate::lower::AstLower;
                    let body_ast = func.body();
                    let fid = func.lower(&mut self.hir.item_tree.functions);
                    self.hir.item_tree.extern_function_ids.push(fid);
                    if let Some(block) = body_ast {
                        let nested_body = BodyLower::lower(self.hir, block);
                        let body_id = self.hir.bodies.alloc(nested_body);
                        self.hir.function_bodies.insert(fid, body_id);
                    }
                }
                None
            }
        }
    }

    fn lower_expr(&mut self, expr: ast::Expr) -> ExprId {
        let range = trimmed_range(expr.syntax());
        match expr {
            ast::Expr::Number(n) => {
                let text = n
                    .value_token()
                    .map(|token| token.text().to_string())
                    .unwrap_or_default();
                let (digits, radix, suffix) = split_int_literal(&text);
                let value = i64::from_str_radix(&digits, radix).unwrap_or_else(|_| {
                    self.diagnostic("invalid integer literal", range);
                    0
                });
                self.alloc_expr(Expr::IntLiteral { value, suffix }, range)
            }

            ast::Expr::Float(f) => {
                let text = f
                    .value_token()
                    .map(|token| token.text().to_string())
                    .unwrap_or_default();
                let (number, suffix) = split_float_literal(&text);
                let value = number.parse().unwrap_or_else(|_| {
                    self.diagnostic("invalid float literal", range);
                    0.0
                });
                self.alloc_expr(Expr::FloatLiteral { value, suffix }, range)
            }

            ast::Expr::StringLit(s) => {
                let text = s
                    .value_token()
                    .map(|t| t.text().to_string())
                    .unwrap_or_default();
                self.alloc_expr(Expr::StringLiteral { value: text }, range)
            }

            ast::Expr::CharLit(c) => {
                let text = c
                    .value_token()
                    .map(|t| t.text().to_string())
                    .unwrap_or_default();
                self.alloc_expr(
                    Expr::CharLiteral {
                        value: lower_char_literal(&text),
                    },
                    range,
                )
            }

            ast::Expr::BoolLit(b) => {
                let value = b.value().unwrap_or(false);
                self.alloc_expr(Expr::BoolLiteral { value }, range)
            }

            ast::Expr::NameRef(name_ref) => {
                let path = name_ref.path().lower();
                self.alloc_expr(
                    Expr::Path {
                        path,
                        resolved: None,
                    },
                    range,
                )
            }

            ast::Expr::ParenExpr(p) if p.is_tuple() => {
                let elements = p.elements().map(|expr| self.lower_expr(expr)).collect();
                self.alloc_expr(Expr::Tuple { elements }, range)
            }
            ast::Expr::ParenExpr(p) => match p.inner() {
                Some(inner) => self.lower_expr(inner),
                None => self.alloc_expr(
                    Expr::Block {
                        stmts: Vec::new(),
                        tail: None,
                    },
                    range,
                ),
            },

            ast::Expr::BinaryExpr(b) => {
                let lhs =
                    self.lower_required_expr(b.lhs(), "missing lhs of binary expression", range);
                let rhs =
                    self.lower_required_expr(b.rhs(), "missing rhs of binary expression", range);
                let Some(op) = b.op_token().and_then(lower_binary_op) else {
                    return self.missing_expr("missing binary operator", range);
                };
                self.alloc_expr(Expr::Binary { lhs, rhs, op }, range)
            }

            ast::Expr::UnaryExpr(u) => {
                let Some(token) = u.op_token() else {
                    return self.missing_expr("missing unary operator", range);
                };
                let operand = self.lower_required_expr(u.operand(), "missing unary operand", range);
                let is_mut = u.is_mut();
                if token.kind() == SyntaxKind::AmpAmp {
                    let inner_op = if is_mut {
                        UnaryOp::MutRef
                    } else {
                        UnaryOp::Ref
                    };
                    let inner = self.alloc_expr(
                        Expr::Unary {
                            operand,
                            op: inner_op,
                        },
                        range,
                    );
                    return self.alloc_expr(
                        Expr::Unary {
                            operand: inner,
                            op: UnaryOp::Ref,
                        },
                        range,
                    );
                }
                let Some(base_op) = lower_unary_op(Some(token)) else {
                    return self.missing_expr("unknown unary operator", range);
                };
                let op = if is_mut && base_op == UnaryOp::Ref {
                    UnaryOp::MutRef
                } else {
                    base_op
                };
                self.alloc_expr(Expr::Unary { operand, op }, range)
            }

            ast::Expr::Block(b) => self.lower_block(b),

            ast::Expr::UnsafeExpr(u) => {
                let body = u
                    .body()
                    .map(|b| self.lower_block(b))
                    .unwrap_or_else(|| self.missing_expr("missing unsafe block body", range));
                self.alloc_expr(Expr::Unsafe { body }, range)
            }

            ast::Expr::CastExpr(c) => {
                let base = self.lower_required_expr(c.base(), "missing cast operand", range);
                let target = self.lower_optional_type(c.ty());
                self.alloc_expr(Expr::Cast { base, target }, range)
            }

            ast::Expr::IfStmt(i) => {
                let cond = self.lower_required_expr(i.condition(), "missing if condition", range);
                let then_branch =
                    self.lower_required_block(i.then_branch(), "missing if body", range);
                let else_branch = match i.else_branch() {
                    Some(ElseBranch::Block(b)) => Some(self.lower_block(b)),
                    Some(ElseBranch::IfStmt(i)) => Some(self.lower_expr(ast::Expr::IfStmt(i))),
                    None => None,
                };
                self.alloc_expr(
                    Expr::If {
                        cond,
                        then_branch,
                        else_branch,
                    },
                    range,
                )
            }

            ast::Expr::WhileStmt(w) => {
                let condition =
                    self.lower_required_expr(w.condition(), "missing while condition", range);
                let body = self.lower_required_block(w.body(), "missing while body", range);
                self.alloc_expr(Expr::While { condition, body }, range)
            }

            ast::Expr::ForExpr(f) => {
                let name_token = f.name();
                let pat_range = name_token
                    .as_ref()
                    .map(|token| token.text_range())
                    .unwrap_or(range);
                let name = lower_name(name_token);
                let pat = self.alloc_pat(Pattern::Binding { name }, pat_range);
                let iterable =
                    self.lower_required_expr(f.iterable(), "missing for iterable", range);
                let body = self.lower_required_block(f.body(), "missing for body", range);
                self.alloc_expr(
                    Expr::For {
                        pat,
                        iterable,
                        body,
                    },
                    range,
                )
            }

            ast::Expr::CallExpr(c) => {
                let callee = self.lower_required_expr(c.callee(), "missing call callee", range);
                let args = self.lower_arg_list(c.arg_list());
                self.alloc_expr(Expr::Call { callee, args }, range)
            }

            ast::Expr::LambdaExpr(lambda) => {
                let params = lambda
                    .param_list()
                    .map(|list| {
                        list.params()
                            .map(|param| {
                                let name_token = param.name();
                                let ty = param.ty();
                                LambdaParam {
                                    name: lower_name(name_token.clone()),
                                    name_range: name_token.map(|token| token.text_range()),
                                    ty_range: ty.as_ref().map(|ty| trimmed_range(ty.syntax())),
                                    ty: self.lower_optional_type(ty),
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let ret_type = lambda.return_type();
                let ret_type_range = ret_type.as_ref().map(|ty| trimmed_range(ty.syntax()));
                let ret_type = self.lower_optional_type(ret_type);
                let body = self.lower_required_block(
                    lambda.body(),
                    "missing anonymous function body",
                    range,
                );
                self.alloc_expr(
                    Expr::Lambda {
                        params,
                        ret_type,
                        ret_type_range,
                        body,
                    },
                    range,
                )
            }

            ast::Expr::MatchExpr(m) => {
                let scrutinee =
                    self.lower_required_expr(m.scrutinee(), "missing match scrutinee", range);
                let arms = m
                    .arms()
                    .map(|arm| {
                        let pat = self.lower_arm_pattern(arm.pattern());
                        let guard = self.lower_optional_expr(arm.guard());
                        let arm_range = trimmed_range(arm.syntax());
                        let body = self.lower_required_expr(
                            arm.body(),
                            "missing match arm body",
                            arm_range,
                        );
                        MatchArm { pat, guard, body }
                    })
                    .collect();
                self.alloc_expr(Expr::Match { scrutinee, arms }, range)
            }

            ast::Expr::ArrayExpr(a) => {
                if a.is_repeat() {
                    let value =
                        self.lower_required_expr(a.repeat_value(), "missing array value", range);
                    let len =
                        self.lower_required_expr(a.repeat_len(), "missing array length", range);
                    self.alloc_expr(Expr::ArrayRepeat { value, len }, range)
                } else {
                    let elements = a.elements().map(|e| self.lower_expr(e)).collect();
                    self.alloc_expr(Expr::Array { elements }, range)
                }
            }

            ast::Expr::StructExpr(s) => {
                let fields = s
                    .fields()
                    .map(|field| {
                        let name = lower_name(field.name());
                        let value = field
                            .value()
                            .map(|value| self.lower_expr(value))
                            .unwrap_or_else(|| {
                                let path = HirPath {
                                    anchor: PathAnchor::Plain,
                                    segments: vec![name.clone()],
                                    type_args: Vec::new(),
                                };
                                let r = field.name().map(|t| t.text_range()).unwrap_or(range);
                                self.alloc_expr(
                                    Expr::Path {
                                        path,
                                        resolved: None,
                                    },
                                    r,
                                )
                            });
                        StructExprField { name, value }
                    })
                    .collect();
                let path = s.path().lower();
                self.alloc_expr(
                    Expr::Struct {
                        path,
                        fields,
                        resolved: None,
                    },
                    range,
                )
            }

            ast::Expr::FieldExpr(f) => {
                let base = self.lower_required_expr(f.base(), "missing field base", range);
                let field = lower_name(f.field_name());
                self.alloc_expr(Expr::FieldAccess { base, field }, range)
            }

            ast::Expr::IndexExpr(idx) => {
                let base = self.lower_required_expr(idx.base(), "missing index base", range);
                let index =
                    self.lower_required_expr(idx.index(), "missing index expression", range);
                self.alloc_expr(Expr::IndexAccess { base, index }, range)
            }
        }
    }

    // == pattern lowering ==

    fn lower_arm_pattern(&mut self, ast_pat: Option<ast::Pattern>) -> PatId {
        match ast_pat {
            Some(pat) => self.lower_pattern(pat),
            None => self.alloc_pat(Pattern::Wildcard, TextRange::empty(0u32.into())),
        }
    }

    fn lower_pattern(&mut self, pat: ast::Pattern) -> PatId {
        let range = trimmed_range(pat.syntax());
        match pat {
            ast::Pattern::Wildcard(_) => self.alloc_pat(Pattern::Wildcard, range),
            ast::Pattern::Literal(literal) => {
                let token = literal.literal_token();
                let text = token
                    .as_ref()
                    .map(|token| token.text().to_string())
                    .unwrap_or_default();
                let literal = match token.map(|token| token.kind()) {
                    Some(SyntaxKind::Number) => {
                        let (digits, radix, suffix) = split_int_literal(&text);
                        let (value, valid) = match i64::from_str_radix(&digits, radix) {
                            Ok(value) => (value, true),
                            Err(_) => {
                                self.diagnostic("invalid integer literal pattern", range);
                                (0, false)
                            }
                        };
                        LiteralPattern::Int {
                            value,
                            suffix,
                            valid,
                        }
                    }
                    Some(SyntaxKind::Float) => {
                        let (number, suffix) = split_float_literal(&text);
                        let (value, valid) = match number.parse() {
                            Ok(value) => (value, true),
                            Err(_) => {
                                self.diagnostic("invalid float literal pattern", range);
                                (0.0, false)
                            }
                        };
                        LiteralPattern::Float {
                            value,
                            suffix,
                            valid,
                        }
                    }
                    Some(SyntaxKind::String) => LiteralPattern::String(text),
                    Some(SyntaxKind::Char) => LiteralPattern::Char(lower_char_literal(&text)),
                    Some(SyntaxKind::True) => LiteralPattern::Bool(true),
                    Some(SyntaxKind::False) => LiteralPattern::Bool(false),
                    _ => LiteralPattern::Bool(false),
                };
                self.alloc_pat(Pattern::Literal(literal), range)
            }
            ast::Pattern::Tuple(tp) => {
                let elements = tp.elements().map(|p| self.lower_pattern(p)).collect();
                self.alloc_pat(Pattern::Tuple { elements }, range)
            }
            ast::Pattern::Struct(sp) => {
                let path = sp.path().lower();
                let name = lower_name(sp.name());
                let sub = sp.sub_pattern().map(|p| self.lower_pattern(p));
                self.alloc_pat(
                    Pattern::Struct {
                        path,
                        fields: vec![FieldPat { name, pat: sub }],
                    },
                    range,
                )
            }
            ast::Pattern::Enum(ep) => {
                let path = ep.path().lower();
                if ep.is_tuple() {
                    let elements = ep.elements().map(|p| self.lower_pattern(p)).collect();
                    self.alloc_pat(Pattern::TupleStruct { path, elements }, range)
                } else if ep.is_struct() {
                    let fields: Vec<FieldPat> = ep
                        .fields()
                        .map(|fp| {
                            let name = lower_name(fp.name());
                            let pat = fp.sub_pattern().map(|p| self.lower_pattern(p));
                            FieldPat { name, pat }
                        })
                        .collect();
                    self.alloc_pat(Pattern::Struct { path, fields }, range)
                } else {
                    match path.as_single_name() {
                        Some(name) => {
                            self.alloc_pat(Pattern::Binding { name: name.clone() }, range)
                        }
                        None => self.alloc_pat(Pattern::Path { path }, range),
                    }
                }
            }
        }
    }
}

fn lower_binary_op(token: SyntaxToken) -> Option<BinaryOp> {
    match token.kind() {
        SyntaxKind::Eq => Some(BinaryOp::Assign),
        SyntaxKind::PlusEq => Some(BinaryOp::AddAssign),
        SyntaxKind::MinusEq => Some(BinaryOp::SubAssign),
        SyntaxKind::StarEq => Some(BinaryOp::MulAssign),
        SyntaxKind::SlashEq => Some(BinaryOp::DivAssign),
        SyntaxKind::PercentEq => Some(BinaryOp::ModAssign),
        SyntaxKind::AmpEq => Some(BinaryOp::BitAndAssign),
        SyntaxKind::PipeEq => Some(BinaryOp::BitOrAssign),
        SyntaxKind::CaretEq => Some(BinaryOp::BitXorAssign),
        SyntaxKind::ShlEq => Some(BinaryOp::ShlAssign),
        SyntaxKind::ShrEq => Some(BinaryOp::ShrAssign),
        SyntaxKind::Plus => Some(BinaryOp::Add),
        SyntaxKind::Minus => Some(BinaryOp::Sub),
        SyntaxKind::Star => Some(BinaryOp::Mul),
        SyntaxKind::Slash => Some(BinaryOp::Div),
        SyntaxKind::Percent => Some(BinaryOp::Mod),
        SyntaxKind::Amp => Some(BinaryOp::BitAnd),
        SyntaxKind::Pipe => Some(BinaryOp::BitOr),
        SyntaxKind::Caret => Some(BinaryOp::BitXor),
        SyntaxKind::Shl => Some(BinaryOp::Shl),
        SyntaxKind::Shr => Some(BinaryOp::Shr),
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

fn lower_char_literal(text: &str) -> String {
    let inner = text
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(text);
    let ch = match inner.strip_prefix('\\') {
        Some("n") => '\n',
        Some("r") => '\r',
        Some("t") => '\t',
        Some("0") => '\0',
        Some("\\") => '\\',
        Some("'") => '\'',
        Some("\"") => '"',
        Some(rest) => rest.chars().next().unwrap_or('\0'),
        None => inner.chars().next().unwrap_or('\0'),
    };
    ch.to_string()
}

fn split_int_literal(text: &str) -> (String, u32, Option<String>) {
    // Strip underscores
    let filtered: String = text.chars().filter(|&c| c != '_').collect();
    // Determine radix
    let (radix, digits) = if let Some(rest) = filtered.strip_prefix("0x") {
        (16, rest)
    } else if let Some(rest) = filtered.strip_prefix("0o") {
        (8, rest)
    } else if let Some(rest) = filtered.strip_prefix("0b") {
        (2, rest)
    } else {
        (10, filtered.as_str())
    };
    let is_digit = |ch: char| match radix {
        16 => ch.is_ascii_hexdigit(),
        _ => ch.is_ascii_digit(),
    };
    let suffix_start = digits
        .find(|ch: char| !is_digit(ch))
        .unwrap_or(digits.len());
    let (digits, suffix) = digits.split_at(suffix_start);
    let suffix = (!suffix.is_empty()).then(|| suffix.to_string());
    (digits.to_string(), radix, suffix)
}

fn split_float_literal(text: &str) -> (String, Option<String>) {
    // Strip underscores
    let filtered: String = text.chars().filter(|&c| c != '_').collect();
    for suffix in ["f16", "f32", "f64", "f128"] {
        if let Some(number) = filtered.strip_suffix(suffix) {
            return (number.to_string(), Some(suffix.to_string()));
        }
    }
    (filtered, None)
}
