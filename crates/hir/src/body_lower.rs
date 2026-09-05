use std::collections::HashMap;

use la_arena::Arena;

use ast::{
    self, ElseBranch,
    support::{AstNode, trimmed_range},
};
use rowan::{TextRange, ast::SyntaxNodePtr};
use syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

use super::{
    HirFile, Name,
    body::{
        BinaryOp, Body, BodyItem, Diagnostic, Expr, ExprId, FieldPat, LabelStyle, LambdaParam,
        LiteralPattern, MatchArm, PatId, Pattern, Severity, SourceLabel, SourceMap, Stmt, StmtId,
        StructExprField, UnaryOp,
    },
    item_tree::HirTypeRef,
    item_tree::{HirPath, PathAnchor},
    lower::{AstLower, Lower, lower_name},
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
    pub fn lower(hir: &'a mut HirFile, block: &ast::Block) -> Body {
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

    pub fn lower_const(hir: &'a mut HirFile, expr: ast::Expr) -> Body {
        let root_ptr = SyntaxNodePtr::new(expr.syntax());
        let range = trimmed_range(expr.syntax());
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
        let tail = lower.lower_expr(expr);
        let root_block = lower.alloc_expr(
            Expr::Block {
                stmts: Vec::new(),
                tail: Some(tail),
            },
            range,
        );
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

    /// Lowers an optional expression slot owned by `owner`. A slot holding an
    /// unexpanded macro call — the proc-macro layer failed to expand it and
    /// already reported the error at the same span — lowers to `Expr::Missing`
    /// without a diagnostic, so one expansion failure does not cascade into
    /// misleading missing-expression or uninitialized-binding errors.
    fn lower_expr_slot(&mut self, expr: Option<ast::Expr>, owner: &SyntaxNode) -> Option<ExprId> {
        if let Some(expr) = expr {
            return Some(self.lower_expr(expr));
        }
        if owner
            .children()
            .any(|child| child.kind() == SyntaxKind::MacroCall)
        {
            let range = trimmed_range(owner);
            return Some(self.alloc_expr(Expr::Missing, range));
        }
        None
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
            Some(b) => self.lower_block(&b),
            None => self.missing_expr(msg, fallback),
        }
    }

    fn lower_arg_list(&mut self, arg_list: Option<ast::ArgList>) -> Vec<ExprId> {
        arg_list
            .map(|args| args.args().map(|a| self.lower_expr(a)).collect())
            .unwrap_or_default()
    }

    fn lower_optional_type(ty: Option<ast::Type>) -> HirTypeRef {
        ty.map_or(HirTypeRef::Unknown, super::lower::Lower::lower)
    }

    fn lower_block(&mut self, block: &ast::Block) -> ExprId {
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
                let pat = match var.pattern() {
                    Some(pattern) => self.lower_pattern(pattern),
                    None => self.alloc_pat(Pattern::Wildcard, range),
                };
                let ty_ast = var.ty();
                let ty_range = ty_ast.as_ref().map(|ty| trimmed_range(ty.syntax()));
                let ty = Self::lower_optional_type(ty_ast);
                let init = self.lower_expr_slot(var.init(), var.syntax());
                let else_ = var.else_block().map(|block| self.lower_block(&block));
                Some(self.alloc_stmt(
                    Stmt::Let {
                        pat,
                        ty,
                        ty_range,
                        init,
                        else_,
                    },
                    range,
                ))
            }

            ast::Stmt::ReturnStmt(ret) => {
                let value = self.lower_optional_expr(ret.value());
                Some(self.alloc_stmt(Stmt::Return { value }, range))
            }

            ast::Stmt::BreakStmt(b) => {
                let value = self.lower_optional_expr(b.value());
                Some(self.alloc_stmt(Stmt::Break { value }, range))
            }

            ast::Stmt::ContinueStmt(_) => Some(self.alloc_stmt(Stmt::Continue, range)),

            ast::Stmt::ExprStmt(es) => {
                let expr = self
                    .lower_expr_slot(es.expr(), es.syntax())
                    .unwrap_or_else(|| self.missing_expr("missing expression statement", range));
                Some(self.alloc_stmt(Stmt::Expr { expr }, range))
            }

            ast::Stmt::ModDecl(m) => {
                let mid = crate::lower_mod_decl(self.hir, &m);
                Some(self.alloc_stmt(
                    Stmt::Item {
                        item: BodyItem::Module(mid),
                    },
                    range,
                ))
            }

            ast::Stmt::UseDecl(u) => self.lower_use_stmt(&u, range),

            // Top-level declarations inside bodies are allowed and are promoted to the global item tree.
            ast::Stmt::FuncDecl(func) => {
                self.lower_nested_function(func);
                None
            }

            ast::Stmt::StructDecl(s) => {
                let _sid = s.lower(&mut self.hir.item_tree.structs);
                None
            }

            ast::Stmt::EnumDecl(e) => {
                let _eid = e.lower(&mut self.hir.item_tree.enums);
                None
            }

            ast::Stmt::TraitDecl(t) => {
                let _tid = crate::lower_trait_decl(self.hir, t);
                None
            }

            ast::Stmt::ImplDecl(i) => {
                let _iid = crate::lower_impl_decl(self.hir, &i);
                None
            }

            ast::Stmt::ConstDecl(c) => {
                let value = c.value();
                let cid = c.lower(&mut self.hir.item_tree.consts);
                if let Some(value) = value {
                    let body = BodyLower::lower_const(self.hir, value);
                    let body_id = self.hir.bodies.alloc(body);
                    self.hir.const_bodies.insert(cid, body_id);
                }
                None
            }

            ast::Stmt::TypeAliasDecl(t) => {
                let _tid = t.lower(&mut self.hir.item_tree.type_aliases);
                None
            }

            ast::Stmt::ExternBlock(block) => {
                for func in block.functions() {
                    let fid = func.lower(&mut self.hir.item_tree.functions);
                    self.hir.item_tree.extern_function_ids.push(fid);
                }
                None
            }

            ast::Stmt::ExternFnDecl(decl) => {
                self.lower_extern_function(&decl);
                None
            }
        }
    }

    fn lower_use_stmt(&mut self, use_decl: &ast::UseDecl, range: TextRange) -> Option<StmtId> {
        let Some(tree_ast) = use_decl.use_tree() else {
            self.diagnostic("malformed use declaration", range);
            return None;
        };
        let tree = tree_ast.lower();
        let attrs = crate::lower::lower_attrs(use_decl.syntax());
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

    fn lower_nested_function(&mut self, function: ast::FuncDecl) {
        let body = function.body();
        let fid = function.lower(&mut self.hir.item_tree.functions);
        if let Some(block) = body {
            let nested_body = BodyLower::lower(self.hir, &block);
            let body_id = self.hir.bodies.alloc(nested_body);
            self.hir.function_bodies.insert(fid, body_id);
        }
    }

    fn lower_extern_function(&mut self, declaration: &ast::ExternFnDecl) {
        let Some(function) = declaration.func_decl() else {
            return;
        };
        let Some(body) = function.body() else {
            return;
        };
        let fid = function.lower(&mut self.hir.item_tree.functions);
        self.hir.item_tree.extern_function_ids.push(fid);
        let nested_body = BodyLower::lower(self.hir, &body);
        let body_id = self.hir.bodies.alloc(nested_body);
        self.hir.function_bodies.insert(fid, body_id);
    }

    fn lower_expr(&mut self, expr: ast::Expr) -> ExprId {
        let range = trimmed_range(expr.syntax());
        match expr {
            ast::Expr::Number(number) => self.lower_number_expr(&number, range),

            ast::Expr::Float(float) => self.lower_float_expr(&float, range),

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

            ast::Expr::ParenExpr(paren) => self.lower_paren_expr(&paren, range),

            ast::Expr::BinaryExpr(binary) => self.lower_binary_expr(&binary, range),

            ast::Expr::UnaryExpr(unary) => self.lower_unary_expr(&unary, range),

            ast::Expr::Block(b) => self.lower_block(&b),

            ast::Expr::UnsafeExpr(u) => {
                let body = if let Some(block) = u.body() {
                    self.lower_block(&block)
                } else {
                    self.missing_expr("missing unsafe block body", range)
                };
                self.alloc_expr(Expr::Unsafe { body }, range)
            }

            ast::Expr::CastExpr(c) => {
                let base = self.lower_required_expr(c.base(), "missing cast operand", range);
                let target = Self::lower_optional_type(c.ty());
                self.alloc_expr(Expr::Cast { base, target }, range)
            }

            ast::Expr::TryExpr(t) => {
                let operand = self.lower_required_expr(t.operand(), "missing try operand", range);
                self.alloc_expr(Expr::Try { operand }, range)
            }

            ast::Expr::IfStmt(if_stmt) => self.lower_if_expr(&if_stmt, range),

            ast::Expr::WhileStmt(while_stmt) => self.lower_while_expr(&while_stmt, range),

            ast::Expr::LoopExpr(loop_expr) => self.lower_loop_expr(&loop_expr, range),

            ast::Expr::ForExpr(for_expr) => self.lower_for_expr(&for_expr, range),

            ast::Expr::CallExpr(c) => {
                let callee = self.lower_required_expr(c.callee(), "missing call callee", range);
                let args = self.lower_arg_list(c.arg_list());
                let type_args = c
                    .type_args()
                    .into_iter()
                    .map(super::lower::Lower::lower)
                    .collect();
                self.alloc_expr(
                    Expr::Call {
                        callee,
                        args,
                        type_args,
                    },
                    range,
                )
            }

            ast::Expr::BracketLambdaExpr(lambda) => self.lower_bracket_lambda_expr(&lambda, range),

            ast::Expr::MatchExpr(match_expr) => self.lower_match_expr(&match_expr, range),

            ast::Expr::ArrayExpr(array) => self.lower_array_expr(&array, range),

            ast::Expr::StructExpr(struct_expr) => self.lower_struct_expr(&struct_expr, range),

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

            ast::Expr::RangeExpr(range_expr) => self.lower_range_expr(&range_expr, range),
        }
    }

    /// Desugars `start..end` / `start..=end` into a call to
    /// `crate::std::ops::range` / `range_inclusive`, whose results implement
    /// `IntoIterator`, so the rest of the pipeline sees an ordinary call.
    fn lower_range_expr(&mut self, range_expr: &ast::RangeExpr, range: TextRange) -> ExprId {
        let start = self.lower_required_expr(range_expr.start(), "missing range start", range);
        let end = self.lower_required_expr(range_expr.end(), "missing range end", range);
        let function = if range_expr.inclusive() {
            "range_inclusive"
        } else {
            "range"
        };
        self.lower_std_call(&["std", "ops", function], vec![start, end], range)
    }

    /// Builds a call to a `crate::std::...` free function from a desugared
    /// expression; the synthesized path resolves like a user-written one.
    fn lower_std_call(&mut self, segments: &[&str], args: Vec<ExprId>, range: TextRange) -> ExprId {
        let path = HirPath {
            anchor: PathAnchor::Crate,
            segments: segments
                .iter()
                .map(|segment| Name(segment.to_string()))
                .collect(),
            segment_type_args: Vec::new(),
            type_args: Vec::new(),
            range,
        };
        let callee = self.alloc_expr(
            Expr::Path {
                path,
                resolved: None,
            },
            range,
        );
        self.alloc_expr(
            Expr::Call {
                callee,
                args,
                type_args: Vec::new(),
            },
            range,
        )
    }

    fn lower_number_expr(&mut self, number: &ast::NumberExpr, range: TextRange) -> ExprId {
        let text = number
            .value_token()
            .map(|token| token.text().to_string())
            .unwrap_or_default();
        let (digits, radix, suffix) = split_int_literal(&text);
        let value = u64::from_str_radix(&digits, radix).unwrap_or_else(|_| {
            self.diagnostic("invalid integer literal", range);
            0
        });
        self.alloc_expr(Expr::IntLiteral { value, suffix }, range)
    }

    fn lower_float_expr(&mut self, float: &ast::FloatLitExpr, range: TextRange) -> ExprId {
        let text = float
            .value_token()
            .map(|token| token.text().to_string())
            .unwrap_or_default();
        let (number, suffix) = split_float_literal(&text);
        let value = parse_float_literal(&number, suffix.as_deref()).unwrap_or_else(|error| {
            self.diagnostic(error, range);
            0.0
        });
        self.alloc_expr(Expr::FloatLiteral { value, suffix }, range)
    }

    fn lower_paren_expr(&mut self, paren: &ast::ParenExpr, range: TextRange) -> ExprId {
        if paren.is_tuple() {
            let elements = paren.elements().map(|expr| self.lower_expr(expr)).collect();
            return self.alloc_expr(Expr::Tuple { elements }, range);
        }
        let Some(inner) = paren.inner() else {
            return self.alloc_expr(
                Expr::Block {
                    stmts: Vec::new(),
                    tail: None,
                },
                range,
            );
        };
        self.lower_expr(inner)
    }

    fn lower_binary_expr(&mut self, binary: &ast::BinaryExpr, range: TextRange) -> ExprId {
        let lhs = self.lower_required_expr(binary.lhs(), "missing lhs of binary expression", range);
        let rhs = self.lower_required_expr(binary.rhs(), "missing rhs of binary expression", range);
        let Some(op) = binary.op_token().and_then(|token| lower_binary_op(&token)) else {
            return self.missing_expr("missing binary operator", range);
        };
        self.alloc_expr(Expr::Binary { lhs, rhs, op }, range)
    }

    fn lower_unary_expr(&mut self, unary: &ast::UnaryExpr, range: TextRange) -> ExprId {
        let Some(token) = unary.op_token() else {
            return self.missing_expr("missing unary operator", range);
        };
        let operand = self.lower_required_expr(unary.operand(), "missing unary operand", range);
        let is_mut = unary.is_mut();
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
        let Some(base_op) = lower_unary_op(Some(&token)) else {
            return self.missing_expr("unknown unary operator", range);
        };
        let op = if is_mut && base_op == UnaryOp::Ref {
            UnaryOp::MutRef
        } else {
            base_op
        };
        self.alloc_expr(Expr::Unary { operand, op }, range)
    }

    fn lower_if_expr(&mut self, if_stmt: &ast::IfStmt, range: TextRange) -> ExprId {
        if let Some(let_condition) = if_stmt.let_condition() {
            return self.lower_if_let_expr(if_stmt, &let_condition, range);
        }
        let cond = self.lower_required_expr(if_stmt.condition(), "missing if condition", range);
        let then_branch =
            self.lower_required_block(if_stmt.then_branch(), "missing if body", range);
        let else_branch = match if_stmt.else_branch() {
            Some(ElseBranch::Block(block)) => Some(self.lower_block(&block)),
            Some(ElseBranch::IfStmt(if_stmt)) => Some(self.lower_expr(ast::Expr::IfStmt(if_stmt))),
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

    // `if let pat = scrutinee { then } else { else }` lowers to a match whose
    // wildcard arm holds the else branch (or an empty block).
    fn lower_if_let_expr(
        &mut self,
        if_stmt: &ast::IfStmt,
        condition: &ast::LetCondition,
        range: TextRange,
    ) -> ExprId {
        let scrutinee =
            self.lower_required_expr(condition.expr(), "missing if-let scrutinee", range);
        let pat = self.lower_arm_pattern(condition.pattern());
        let then_branch =
            self.lower_required_block(if_stmt.then_branch(), "missing if body", range);
        let else_branch = match if_stmt.else_branch() {
            Some(ElseBranch::Block(block)) => self.lower_block(&block),
            Some(ElseBranch::IfStmt(if_stmt)) => self.lower_expr(ast::Expr::IfStmt(if_stmt)),
            None => self.alloc_expr(
                Expr::Block {
                    stmts: Vec::new(),
                    tail: None,
                },
                range,
            ),
        };
        let wildcard = self.alloc_pat(Pattern::Wildcard, range);
        self.alloc_expr(
            Expr::Match {
                scrutinee,
                arms: vec![
                    MatchArm {
                        pat,
                        guard: None,
                        body: then_branch,
                    },
                    MatchArm {
                        pat: wildcard,
                        guard: None,
                        body: else_branch,
                    },
                ],
            },
            range,
        )
    }

    fn lower_while_expr(&mut self, while_stmt: &ast::WhileStmt, range: TextRange) -> ExprId {
        if let Some(let_condition) = while_stmt.let_condition() {
            return self.lower_while_let_expr(while_stmt, &let_condition, range);
        }
        let condition =
            self.lower_required_expr(while_stmt.condition(), "missing while condition", range);
        let body = self.lower_required_block(while_stmt.body(), "missing while body", range);
        self.alloc_expr(Expr::While { condition, body }, range)
    }

    // `while let pat = scrutinee { body }` lowers to a loop whose body matches
    // on the re-evaluated scrutinee; the loop body itself is the pattern arm
    // body so bindings stay in scope, and the wildcard arm breaks.
    fn lower_while_let_expr(
        &mut self,
        while_stmt: &ast::WhileStmt,
        condition: &ast::LetCondition,
        range: TextRange,
    ) -> ExprId {
        let scrutinee =
            self.lower_required_expr(condition.expr(), "missing while-let scrutinee", range);
        let pat = self.lower_arm_pattern(condition.pattern());
        let body = self.lower_required_block(while_stmt.body(), "missing while body", range);
        let break_stmt = self.alloc_stmt(Stmt::Break { value: None }, range);
        let break_block = self.alloc_expr(
            Expr::Block {
                stmts: vec![break_stmt],
                tail: None,
            },
            range,
        );
        let wildcard = self.alloc_pat(Pattern::Wildcard, range);
        let match_expr = self.alloc_expr(
            Expr::Match {
                scrutinee,
                arms: vec![
                    MatchArm {
                        pat,
                        guard: None,
                        body,
                    },
                    MatchArm {
                        pat: wildcard,
                        guard: None,
                        body: break_block,
                    },
                ],
            },
            range,
        );
        let loop_body = self.alloc_expr(
            Expr::Block {
                stmts: Vec::new(),
                tail: Some(match_expr),
            },
            range,
        );
        self.alloc_expr(Expr::Loop { body: loop_body }, range)
    }

    fn lower_loop_expr(&mut self, loop_expr: &ast::LoopExpr, range: TextRange) -> ExprId {
        let body = self.lower_required_block(loop_expr.body(), "missing loop body", range);
        self.alloc_expr(Expr::Loop { body }, range)
    }

    fn lower_for_expr(&mut self, for_expr: &ast::ForExpr, range: TextRange) -> ExprId {
        let pat = self.lower_arm_pattern(for_expr.pattern());
        let iterable = self.lower_required_expr(for_expr.iterable(), "missing for iterable", range);
        let body = self.lower_required_block(for_expr.body(), "missing for body", range);
        self.alloc_expr(
            Expr::For {
                pat,
                iterable,
                body,
            },
            range,
        )
    }

    fn lower_bracket_lambda_expr(
        &mut self,
        lambda: &ast::BracketLambdaExpr,
        range: TextRange,
    ) -> ExprId {
        let params = self.lower_lambda_params(lambda.param_list());
        let body = match self.lower_optional_expr(lambda.body()) {
            Some(expr) => self.alloc_expr(
                Expr::Block {
                    stmts: Vec::new(),
                    tail: Some(expr),
                },
                range,
            ),
            None => self.missing_expr("missing lambda body", range),
        };
        self.alloc_expr(
            Expr::Lambda {
                is_move: lambda.is_move(),
                generics: Vec::new(),
                generic_bounds: Vec::new(),
                params,
                ret_type: HirTypeRef::Unknown,
                ret_type_range: None,
                body,
            },
            range,
        )
    }

    fn lower_lambda_params(&mut self, param_list: Option<ast::ParamList>) -> Vec<LambdaParam> {
        param_list
            .map(|list| {
                list.params()
                    .enumerate()
                    .map(|(index, param)| {
                        let ast_pattern = param.pattern();
                        let binding = match ast_pattern.as_ref() {
                            Some(ast::Pattern::Binding(binding)) => {
                                binding.name().map(|name| (name, binding.is_mut()))
                            }
                            Some(ast::Pattern::Enum(pattern))
                                if !pattern.is_tuple() && !pattern.is_struct() =>
                            {
                                pattern.path().and_then(|path| {
                                    if path.is_absolute() {
                                        return None;
                                    }
                                    let mut segments = path.segments();
                                    let segment = segments.next()?;
                                    if segments.next().is_some() || !segment.type_args().is_empty()
                                    {
                                        return None;
                                    }
                                    segment
                                        .name_token()
                                        .filter(|name| name.kind() == syntax::SyntaxKind::Ident)
                                        .map(|name| (name, false))
                                })
                            }
                            _ => None,
                        };
                        let pat = binding
                            .is_none()
                            .then_some(ast_pattern)
                            .flatten()
                            .map(|pattern| self.lower_pattern(pattern));
                        let (name_token, is_mut) =
                            binding.map_or((None, false), |(name, is_mut)| (Some(name), is_mut));
                        let ty = param.ty();
                        LambdaParam {
                            name: name_token.clone().map_or_else(
                                || Name(format!("#arg{index}")),
                                |name| Name(name.text().into()),
                            ),
                            name_range: name_token.map(|token| token.text_range()),
                            is_mut,
                            pat,
                            ty_range: ty.as_ref().map(|ty| trimmed_range(ty.syntax())),
                            ty: Self::lower_optional_type(ty),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn lower_match_expr(&mut self, match_expr: &ast::MatchExpr, range: TextRange) -> ExprId {
        let scrutinee =
            self.lower_required_expr(match_expr.scrutinee(), "missing match scrutinee", range);
        let arms = match_expr
            .arms()
            .map(|arm| {
                let pat = self.lower_arm_pattern(arm.pattern());
                let guard = self.lower_optional_expr(arm.guard());
                let arm_range = trimmed_range(arm.syntax());
                let body =
                    self.lower_required_expr(arm.body(), "missing match arm body", arm_range);
                MatchArm { pat, guard, body }
            })
            .collect();
        self.alloc_expr(Expr::Match { scrutinee, arms }, range)
    }

    fn lower_array_expr(&mut self, array: &ast::ArrayExpr, range: TextRange) -> ExprId {
        if array.is_repeat() {
            let value =
                self.lower_required_expr(array.repeat_value(), "missing array value", range);
            let len = self.lower_required_expr(array.repeat_len(), "missing array length", range);
            self.alloc_expr(Expr::ArrayRepeat { value, len }, range)
        } else {
            let elements = array
                .elements()
                .map(|element| self.lower_expr(element))
                .collect();
            self.alloc_expr(Expr::Array { elements }, range)
        }
    }

    fn lower_struct_expr(&mut self, struct_expr: &ast::StructExpr, range: TextRange) -> ExprId {
        let fields = struct_expr
            .fields()
            .map(|field| {
                let name = lower_name(field.name());
                let Some(value) = field.value() else {
                    let field_range = field.name().map_or(range, |token| token.text_range());
                    let path = HirPath {
                        anchor: PathAnchor::Plain,
                        segments: vec![name.clone()],
                        segment_type_args: Vec::new(),
                        type_args: Vec::new(),
                        range: field_range,
                    };
                    let value = self.alloc_expr(
                        Expr::Path {
                            path,
                            resolved: None,
                        },
                        field_range,
                    );
                    return StructExprField { name, value };
                };
                let value = self.lower_expr(value);
                StructExprField { name, value }
            })
            .collect();
        let mut path = struct_expr.path().lower();
        let explicit_type_args: Vec<HirTypeRef> = struct_expr
            .type_args()
            .into_iter()
            .map(super::lower::Lower::lower)
            .collect();
        if !explicit_type_args.is_empty() {
            path.type_args = explicit_type_args;
        }
        self.alloc_expr(
            Expr::Struct {
                path,
                fields,
                resolved: None,
            },
            range,
        )
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
            ast::Pattern::Binding(binding) => {
                let name = lower_name(binding.name());
                let is_mut = binding.is_mut();
                self.alloc_pat(Pattern::Binding { name, is_mut }, range)
            }
            ast::Pattern::Reference(reference) => {
                let mutable = reference.is_mut();
                let pattern = if let Some(pattern) = reference.pattern() {
                    self.lower_pattern(pattern)
                } else {
                    self.alloc_pat(Pattern::Wildcard, range)
                };
                self.alloc_pat(Pattern::Reference { mutable, pattern }, range)
            }
            ast::Pattern::Literal(literal) => self.lower_literal_pattern(&literal, range),
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
                        Some(name) => self.alloc_pat(
                            Pattern::Binding {
                                name: name.clone(),
                                is_mut: false,
                            },
                            range,
                        ),
                        None => self.alloc_pat(Pattern::Path { path }, range),
                    }
                }
            }
        }
    }

    fn lower_literal_pattern(&mut self, literal: &ast::LiteralPat, range: TextRange) -> PatId {
        let token = literal.literal_token();
        let text = token
            .as_ref()
            .map(|token| token.text().to_string())
            .unwrap_or_default();
        let literal = match token.map(|token| token.kind()) {
            Some(SyntaxKind::Number) => {
                let (digits, radix, suffix) = split_int_literal(&text);
                let (value, valid) = u64::from_str_radix(&digits, radix).map_or_else(
                    |_| {
                        self.diagnostic("invalid integer literal pattern", range);
                        (0, false)
                    },
                    |value| (value, true),
                );
                LiteralPattern::Int {
                    value,
                    suffix,
                    valid,
                }
            }
            Some(SyntaxKind::Float) => {
                let (number, suffix) = split_float_literal(&text);
                let (value, valid) = match parse_float_literal(&number, suffix.as_deref()) {
                    Ok(value) => (value, true),
                    Err(error) => {
                        self.diagnostic(error, range);
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
            Some(kind @ (SyntaxKind::True | SyntaxKind::False)) => {
                LiteralPattern::Bool(kind == SyntaxKind::True)
            }
            _ => LiteralPattern::Bool(false),
        };
        self.alloc_pat(Pattern::Literal(literal), range)
    }
}

fn lower_binary_op(token: &SyntaxToken) -> Option<BinaryOp> {
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

fn lower_unary_op(token: Option<&SyntaxToken>) -> Option<UnaryOp> {
    match token.map(SyntaxToken::kind) {
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
    let (radix, digits) = match filtered.as_bytes() {
        [b'0', b'x', ..] => (16, &filtered[2..]),
        [b'0', b'o', ..] => (8, &filtered[2..]),
        [b'0', b'b', ..] => (2, &filtered[2..]),
        _ => (10, filtered.as_str()),
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

fn parse_float_literal(number: &str, suffix: Option<&str>) -> Result<f64, &'static str> {
    let value = number.parse::<f64>().map_err(|_| "invalid float literal")?;
    if !value.is_finite() || (suffix == Some("f32") && value.abs() > f64::from(f32::MAX)) {
        return Err("non-finite float literal");
    }
    Ok(value)
}
