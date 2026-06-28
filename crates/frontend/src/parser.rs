use rowan::TextRange;

use super::{lexer::Token, syntax_kind::SyntaxKind};

#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub span: TextRange,
}

#[derive(Debug, Clone)]
pub enum Event {
    StartNode {
        kind: SyntaxKind,
        forward_parent: Option<usize>,
    },
    FinishNode,
    AddToken,
    Placeholder,
}

#[derive(Debug)]
pub struct Marker {
    pos: usize,
    completed: bool,
}

impl Marker {
    pub fn complete(mut self, p: &mut Parser, kind: SyntaxKind) -> CompletedMarker {
        self.completed = true;
        match &mut p.events[self.pos] {
            Event::StartNode { kind: solt, .. } => *solt = kind,
            _ => unreachable!(),
        }
        p.events.push(Event::FinishNode);
        CompletedMarker { pos: self.pos }
    }

    // don't make node
    pub fn abandon(mut self, p: &mut Parser) {
        self.completed = true;
        if self.pos == p.events.len() - 1 {
            p.events.pop();
        } else {
            p.events[self.pos] = Event::Placeholder;
        }
    }
}

impl Drop for Marker {
    fn drop(&mut self) {
        if !self.completed {
            panic!("Marker must be either completed or abandoned");
        }
    }
}

/// It can be traced back using precede().
#[derive(Debug, Clone, Copy)]
pub struct CompletedMarker {
    pos: usize,
}

impl CompletedMarker {
    fn kind(self, p: &Parser) -> SyntaxKind {
        match p.events[self.pos] {
            Event::StartNode { kind, .. } => kind,
            _ => unreachable!(),
        }
    }

    /// Insert a new parent node before this node in the hierarchy.
    ///
    /// Used for Pratt parsing: First, the value `1` is parsed. Later,
    /// it is determined that the expression actually represents `1 + 2`.
    /// In other words, the `1` is encapsulated within a BinaryExpr object.
    pub fn precede(self, p: &mut Parser) -> Marker {
        let new_marker = p.start();
        match &mut p.events[self.pos] {
            Event::StartNode { forward_parent, .. } => {
                *forward_parent = Some(new_marker.pos - self.pos)
            }
            _ => unreachable!(),
        }
        new_marker
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReparseEntry {
    Statement,
    Expression,
    Type,
    Block,
    ParamList,
    StructFieldList,
    ArgList,
    UseTree,
    UseTreeList,
    Path,
    Pattern,
    MatchArm,
    EnumVariant,
    FieldPattern,
    TypeList,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExprRestrictions {
    allow_struct_expr: bool,
}

impl ExprRestrictions {
    const NONE: Self = Self {
        allow_struct_expr: true,
    };

    const NO_STRUCT_EXPR: Self = Self {
        allow_struct_expr: false,
    };
}

pub struct Parser<'s> {
    source: &'s str,
    tokens: Vec<Token>,
    pos: usize, // include trivia
    pub(crate) events: Vec<Event>,
    pub errors: Vec<ParseError>,
    // cache
    current_kind: SyntaxKind,
    current_non_trivia_pos: usize,
}

impl<'s> Parser<'s> {
    pub fn new(source: &'s str, tokens: Vec<Token>) -> Self {
        let mut p = Self {
            source,
            tokens,
            pos: 0,
            events: vec![],
            errors: vec![],
            current_kind: SyntaxKind::Eof,
            current_non_trivia_pos: 0,
        };
        p.recompute_current();
        p
    }

    /// Recalculating the current non-trivia token (only called after `pos` changes)
    fn recompute_current(&mut self) {
        let mut i = self.pos;
        while i < self.tokens.len() {
            if !self.tokens[i].kind.is_trivia() {
                self.current_kind = self.tokens[i].kind;
                self.current_non_trivia_pos = i;
                return;
            }
            i += 1;
        }
        self.current_kind = SyntaxKind::Eof;
        self.current_non_trivia_pos = self.tokens.len();
    }

    /// Now non-trivia token.
    #[inline(always)]
    fn current(&self) -> SyntaxKind {
        self.current_kind
    }

    /// Look ahead to the nth non-trivia token.
    #[allow(unused)]
    fn nth(&self, n: usize) -> SyntaxKind {
        if n == 0 {
            return self.current_kind;
        }

        let mut remaining = n;
        let mut i = self.pos;
        while i < self.tokens.len() {
            if !self.tokens[i].kind.is_trivia() {
                if remaining == 0 {
                    return self.tokens[i].kind;
                }
                remaining -= 1;
            }
            i += 1;
        }
        SyntaxKind::Eof
    }

    fn at(&self, kind: SyntaxKind) -> bool {
        self.current() == kind
    }

    /// make a Marker
    fn start(&mut self) -> Marker {
        let pos = self.events.len();
        self.events.push(Event::StartNode {
            kind: SyntaxKind::Tombstone,
            forward_parent: None,
        });
        Marker {
            pos,
            completed: false,
        }
    }

    fn eat_trivia(&mut self) {
        while self.pos < self.tokens.len() && self.tokens[self.pos].kind.is_trivia() {
            self.events.push(Event::AddToken);
            self.pos += 1;
        }
    }

    fn bump(&mut self) {
        self.eat_trivia();
        if self.pos < self.tokens.len() {
            self.events.push(Event::AddToken);
            self.pos += 1;
        }
        self.recompute_current();
    }

    fn expect(&mut self, kind: SyntaxKind) -> bool {
        if self.at(kind) {
            self.bump();
            return true;
        }

        self.error_no_bump(format!("expected {:?}, found {:?}", kind, self.current()));

        if !matches!(
            self.current(),
            SyntaxKind::RParen
                | SyntaxKind::RBrace
                | SyntaxKind::Semi
                | SyntaxKind::Comma
                | SyntaxKind::Eof
        ) {
            let m = self.start();
            self.bump();
            m.complete(self, SyntaxKind::ErrorNode);
        }

        false
    }

    fn current_span(&self) -> TextRange {
        if self.current_non_trivia_pos < self.tokens.len() {
            let span = &self.tokens[self.current_non_trivia_pos].span;
            TextRange::new(
                rowan::TextSize::from(span.start as u32),
                rowan::TextSize::from(span.end as u32),
            )
        } else {
            TextRange::new(0.into(), 0.into())
        }
    }

    fn error(&mut self, msg: String) {
        let span = self.current_span();
        self.errors.push(ParseError { message: msg, span });
        if !self.at(SyntaxKind::Eof) {
            let m = self.start();
            self.bump();
            m.complete(self, SyntaxKind::ErrorNode);
        }
    }

    fn error_no_bump(&mut self, msg: String) {
        let span = self.current_span();
        self.errors.push(ParseError { message: msg, span });
    }

    pub fn parse(mut self) -> (Vec<Event>, Vec<Token>, Vec<ParseError>, &'s str) {
        let m = self.start();

        while !self.at(SyntaxKind::Eof) {
            self.statement();
        }
        self.eat_trivia();
        m.complete(&mut self, SyntaxKind::Root);

        (self.events, self.tokens, self.errors, self.source)
    }

    pub fn reparse(
        mut self,
        entry: ReparseEntry,
    ) -> Option<(Vec<Event>, Vec<Token>, Vec<ParseError>, &'s str)> {
        use ReparseEntry::*;
        match entry {
            Statement => {
                self.statement();
            }
            Expression => {
                self.expression()?;
            }
            Type => {
                self.ty();
            }
            Block => {
                self.block();
            }
            ParamList => {
                self.param_list();
            }
            StructFieldList => {
                self.struct_field_list();
            }
            ArgList => {
                self.arg_list();
            }
            UseTree => {
                self.use_tree();
            }
            UseTreeList => {
                self.use_tree_list();
            }
            Path => {
                self.path();
            }
            Pattern => {
                self.pattern();
            }
            MatchArm => {
                self.match_arm();
            }
            EnumVariant => {
                self.enum_variant();
            }
            FieldPattern => {
                self.field_pattern();
            }
            TypeList => {
                self.type_list();
            }
        }
        if self.current() != SyntaxKind::Eof {
            return None;
        }

        Some((self.events, self.tokens, self.errors, self.source))
    }

    fn at_stmt_start(&self) -> bool {
        matches!(
            self.current(),
            SyntaxKind::Let
                | SyntaxKind::Fun
                | SyntaxKind::Struct
                | SyntaxKind::Mod
                | SyntaxKind::Use
                | SyntaxKind::Return
                | SyntaxKind::Enum
                | SyntaxKind::Trait
                | SyntaxKind::Impl
                | SyntaxKind::Const
                | SyntaxKind::TypeKw
                | SyntaxKind::Extern
        )
    }

    fn at_expr_start(&self) -> bool {
        matches!(
            self.current(),
            SyntaxKind::Number
                | SyntaxKind::Float
                | SyntaxKind::String
                | SyntaxKind::Char
                | SyntaxKind::True
                | SyntaxKind::False
                | SyntaxKind::Ident
                | SyntaxKind::SelfKw
                | SyntaxKind::SuperKw
                | SyntaxKind::CrateKw
                | SyntaxKind::ColonColon
                | SyntaxKind::LParen
                | SyntaxKind::LBrace
                | SyntaxKind::LBracket
                | SyntaxKind::If
                | SyntaxKind::While
                | SyntaxKind::Match
                | SyntaxKind::Unsafe
                | SyntaxKind::Plus
                | SyntaxKind::Minus
                | SyntaxKind::Amp
                | SyntaxKind::AmpAmp
                | SyntaxKind::Star
                | SyntaxKind::Bang
        )
    }

    fn at_expr_with_block_start(&self) -> bool {
        matches!(
            self.current(),
            SyntaxKind::LBrace | SyntaxKind::If | SyntaxKind::While | SyntaxKind::Match | SyntaxKind::Unsafe
        )
    }

    // == stmt ==

    fn statement(&mut self) {
        match self.current() {
            SyntaxKind::Let => self.var_decl(),
            SyntaxKind::Fun => self.func_decl(),
            SyntaxKind::Struct => self.struct_decl(),
            SyntaxKind::Mod => self.mod_decl(),
            SyntaxKind::Use => self.use_decl(),
            SyntaxKind::Enum => self.enum_decl(),
            SyntaxKind::Trait => self.trait_decl(),
            SyntaxKind::Impl => self.impl_decl(),
            SyntaxKind::Const => self.const_decl(),
            SyntaxKind::TypeKw => self.type_alias_decl(),
            SyntaxKind::Return => self.return_stmt(),
            SyntaxKind::Extern => self.extern_decl(),
            SyntaxKind::Eof => return,
            _ => self.expr_stmt(),
        }
    }

    fn mod_decl(&mut self) {
        let m = self.start();

        self.expect(SyntaxKind::Mod);
        self.expect(SyntaxKind::Ident);

        if self.at(SyntaxKind::Semi) {
            self.bump();
        } else if self.at(SyntaxKind::LBrace) {
            self.bump();

            while !self.at(SyntaxKind::RBrace) && !self.at(SyntaxKind::Eof) {
                self.statement();
            }

            self.expect(SyntaxKind::RBrace);
        } else {
            self.error(format!(
                "expected ';' or module body, found {:?}",
                self.current()
            ));
        }

        m.complete(self, SyntaxKind::ModDecl);
    }

    fn use_decl(&mut self) {
        let m = self.start();

        self.expect(SyntaxKind::Use);
        self.use_tree();
        self.expect(SyntaxKind::Semi);

        m.complete(self, SyntaxKind::UseDecl);
    }

    fn use_tree(&mut self) {
        let m = self.start();

        if self.at(SyntaxKind::LBrace) {
            self.use_tree_list();
        } else if self.at_path_start() {
            self.path();

            if self.at(SyntaxKind::As) {
                self.bump();
                self.expect(SyntaxKind::Ident);
            } else if self.at(SyntaxKind::ColonColon) {
                self.bump();

                if self.at(SyntaxKind::Star) {
                    self.bump();
                } else if self.at(SyntaxKind::LBrace) {
                    self.use_tree_list();
                } else {
                    self.error(format!(
                        "expected '*' or '{{' after '::' in use tree, found {:?}",
                        self.current()
                    ));
                }
            }
        } else {
            self.error(format!("expected use tree, found {:?}", self.current()));
        }

        m.complete(self, SyntaxKind::UseTree);
    }

    fn use_tree_list(&mut self) {
        let m = self.start();

        self.expect(SyntaxKind::LBrace);

        if !self.at(SyntaxKind::RBrace) && !self.at(SyntaxKind::Eof) {
            self.use_tree();
            while self.at(SyntaxKind::Comma) {
                self.bump();
                if self.at(SyntaxKind::RBrace) {
                    break;
                }
                self.use_tree();
            }
        }

        self.expect(SyntaxKind::RBrace);
        m.complete(self, SyntaxKind::UseTreeList);
    }

    fn at_path_start(&self) -> bool {
        self.at(SyntaxKind::ColonColon) || Self::is_path_segment_start(self.current())
    }

    fn path(&mut self) -> CompletedMarker {
        let m = self.start();

        if self.at(SyntaxKind::ColonColon) {
            self.bump();
        }

        if Self::is_path_segment_start(self.current()) {
            self.path_segment();
        } else {
            self.error(format!("expected path segment, found {:?}", self.current()));
        }

        while self.at(SyntaxKind::ColonColon) && Self::is_path_segment_start(self.nth(1)) {
            self.bump();
            self.path_segment();
        }

        m.complete(self, SyntaxKind::Path)
    }

    fn path_segment(&mut self) {
        let m = self.start();

        if Self::is_path_segment_start(self.current()) {
            self.bump();
        } else {
            self.error(format!("expected path segment, found {:?}", self.current()));
        }

        m.complete(self, SyntaxKind::PathSegment);
    }

    fn is_path_segment_start(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::Ident | SyntaxKind::SelfKw | SyntaxKind::SuperKw | SyntaxKind::CrateKw
        )
    }

    fn var_decl(&mut self) {
        let m = self.start();
        self.bump(); // 'let'
        if self.at(SyntaxKind::Mut) {
            self.bump(); // 'mut'
        }
        self.expect(SyntaxKind::Ident);

        if self.at(SyntaxKind::Colon) {
            self.bump();
            self.ty();
        }

        if self.at(SyntaxKind::Eq) {
            self.bump();
            self.expression();
        }

        self.expect(SyntaxKind::Semi);
        m.complete(self, SyntaxKind::VarDecl);
    }

    fn func_decl(&mut self) {
        let m = self.start();

        self.bump();
        self.expect(SyntaxKind::Ident);

        self.param_list();

        if self.at(SyntaxKind::Arrow) {
            self.bump();
            self.ty();
        }

        if self.at(SyntaxKind::LBrace) {
            self.block();
        } else {
            self.expect(SyntaxKind::Semi);
        }

        m.complete(self, SyntaxKind::FuncDecl);
    }

    fn param_list(&mut self) {
        let m = self.start();

        self.expect(SyntaxKind::LParen);

        if !self.at(SyntaxKind::RParen) && !self.at(SyntaxKind::Eof) {
            self.param();
            while self.at(SyntaxKind::Comma) {
                self.bump();
                self.param();
            }
        }

        self.expect(SyntaxKind::RParen);
        m.complete(self, SyntaxKind::ParamList);
    }

    fn param(&mut self) {
        let m = self.start();
        self.expect(SyntaxKind::Ident);
        self.expect(SyntaxKind::Colon);
        self.ty();
        m.complete(self, SyntaxKind::Param);
    }

    fn struct_decl(&mut self) {
        let m = self.start();
        self.bump();
        self.expect(SyntaxKind::Ident);
        self.struct_field_list();
        m.complete(self, SyntaxKind::StructDecl);
    }

    fn struct_field_list(&mut self) {
        let m = self.start();
        self.expect(SyntaxKind::LBrace);

        if !self.at(SyntaxKind::RBrace) && !self.at(SyntaxKind::Eof) {
            self.struct_field();
            while self.at(SyntaxKind::Comma) {
                self.bump();
                if self.at(SyntaxKind::RBrace) {
                    break;
                }
                self.struct_field();
            }
        }

        self.expect(SyntaxKind::RBrace);
        m.complete(self, SyntaxKind::StructFieldList);
    }

    fn struct_field(&mut self) {
        let m = self.start();
        self.expect(SyntaxKind::Ident);
        self.expect(SyntaxKind::Colon);
        self.ty();
        m.complete(self, SyntaxKind::StructField);
    }

    fn return_stmt(&mut self) {
        let m = self.start();
        self.bump();

        if !self.at(SyntaxKind::Semi) && !self.at(SyntaxKind::Eof) {
            self.expression();
        }

        self.expect(SyntaxKind::Semi);
        m.complete(self, SyntaxKind::ReturnStmt);
    }

    fn block(&mut self) -> CompletedMarker {
        let m = self.start();
        self.expect(SyntaxKind::LBrace);

        while !self.at(SyntaxKind::RBrace) && !self.at(SyntaxKind::Eof) {
            if self.at_stmt_start() {
                self.statement();
                continue;
            }

            if !self.at_expr_start() {
                self.error(format!(
                    "expected statement or expression, found {:?}",
                    self.current()
                ));
                // Consume the unexpected token to avoid infinite loop
                self.bump();
                continue;
            }

            let starts_with_block = self.at_expr_with_block_start();

            let expr = match self.expression() {
                Some(expr) => expr,
                None => continue,
            };

            if self.at(SyntaxKind::Semi) {
                let stmt = expr.precede(self);
                self.bump();
                stmt.complete(self, SyntaxKind::ExprStmt);
                continue;
            }

            if self.at(SyntaxKind::RBrace) {
                break;
            }

            if starts_with_block {
                let stmt = expr.precede(self);
                stmt.complete(self, SyntaxKind::ExprStmt);
                continue;
            }

            self.error_no_bump(format!(
                "expected ';' or '}}' after expression, found {:?}",
                self.current()
            ));

            let stmt = expr.precede(self);
            stmt.complete(self, SyntaxKind::ExprStmt);

            if !self.at(SyntaxKind::RBrace) && !self.at(SyntaxKind::Eof) {
                let err = self.start();
                self.bump();
                err.complete(self, SyntaxKind::ErrorNode);
            }
        }

        self.expect(SyntaxKind::RBrace);
        m.complete(self, SyntaxKind::Block)
    }

    fn expr_stmt(&mut self) {
        let starts_with_block = self.at_expr_with_block_start();

        let expr = match self.expression() {
            Some(expr) => expr,
            None => {
                if !self.at(SyntaxKind::Eof) {
                    let m = self.start();
                    self.bump();
                    m.complete(self, SyntaxKind::ErrorNode);
                }
                return;
            }
        };

        let m = expr.precede(self);

        if self.at(SyntaxKind::Semi) {
            self.bump();
            m.complete(self, SyntaxKind::ExprStmt);
            return;
        }

        if starts_with_block {
            m.complete(self, SyntaxKind::ExprStmt);
            return;
        }

        self.error(format!(
            "expected ';' after expression, found {:?}",
            self.current()
        ));
        m.complete(self, SyntaxKind::ExprStmt);
    }

    // == expr ==

    fn expression(&mut self) -> Option<CompletedMarker> {
        self.expr_bp(0)
    }

    fn expression_no_struct(&mut self) -> Option<CompletedMarker> {
        self.expr_bp_restricted(0, ExprRestrictions::NO_STRUCT_EXPR)
    }

    fn if_expr(&mut self) -> CompletedMarker {
        let m = self.start();
        self.expect(SyntaxKind::If);
        self.expression_no_struct();

        if self.at(SyntaxKind::LBrace) {
            self.block();
        } else {
            self.error(format!(
                "expected block after if condition, found {:?}",
                self.current()
            ));
        }

        if self.at(SyntaxKind::Else) {
            self.bump();
            if self.at(SyntaxKind::If) {
                self.if_expr();
            } else if self.at(SyntaxKind::LBrace) {
                self.block();
            } else {
                self.error(format!(
                    "expected block or if after else, found {:?}",
                    self.current()
                ));
            }
        }

        m.complete(self, SyntaxKind::IfStmt)
    }

    fn while_expr(&mut self) -> CompletedMarker {
        let m = self.start();
        self.expect(SyntaxKind::While);
        self.expression_no_struct();

        if self.at(SyntaxKind::LBrace) {
            self.block();
        } else {
            self.error(format!(
                "expected block after while condition, found {:?}",
                self.current()
            ));
        }

        m.complete(self, SyntaxKind::WhileStmt)
    }

    fn match_expr(&mut self) -> CompletedMarker {
        let m = self.start();
        self.expect(SyntaxKind::Match);
        self.expression_no_struct();
        self.expect(SyntaxKind::LBrace);

        if !self.at(SyntaxKind::RBrace) && !self.at(SyntaxKind::Eof) {
            self.match_arm();
            while self.at(SyntaxKind::Comma) {
                self.bump();
                if self.at(SyntaxKind::RBrace) {
                    break;
                }
                self.match_arm();
            }
        }

        self.expect(SyntaxKind::RBrace);
        m.complete(self, SyntaxKind::MatchExpr)
    }

    fn unsafe_expr(&mut self) -> CompletedMarker {
        let m = self.start();
        self.expect(SyntaxKind::Unsafe);
        // ponytail: unsafe only supports blocks for now
        self.block();
        m.complete(self, SyntaxKind::UnsafeExpr)
    }

    fn match_arm(&mut self) {
        let m = self.start();

        self.pattern();

        if self.at(SyntaxKind::If) {
            self.bump();
            self.expression();
        }

        self.expect(SyntaxKind::FatArrow);
        self.expression();

        m.complete(self, SyntaxKind::MatchArm);
    }

    fn expr_bp(&mut self, min_bp: u8) -> Option<CompletedMarker> {
        self.expr_bp_restricted(min_bp, ExprRestrictions::NONE)
    }

    fn expr_bp_restricted(
        &mut self,
        min_bp: u8,
        restrictions: ExprRestrictions,
    ) -> Option<CompletedMarker> {
        // prefix
        let mut lhs = self.lhs(restrictions)?;

        loop {
            let op = self.current();
            // postfix
            // call
            if op == SyntaxKind::LParen {
                const CALL_BP: u8 = 15;
                if CALL_BP < min_bp {
                    break;
                }
                let m = lhs.precede(self);
                self.arg_list();
                lhs = m.complete(self, SyntaxKind::CallExpr);
                continue;
            }

            // field access
            if op == SyntaxKind::Dot {
                const FIELD_BP: u8 = 15;
                if FIELD_BP < min_bp {
                    break;
                }
                let m = lhs.precede(self);
                self.bump();
                self.expect(SyntaxKind::Ident);
                lhs = m.complete(self, SyntaxKind::FieldExpr);
                continue;
            }

            // index access
            if op == SyntaxKind::LBracket {
                const INDEX_BP: u8 = 15;
                if INDEX_BP < min_bp {
                    break;
                }
                let m = lhs.precede(self);
                self.bump();
                self.expression();
                self.expect(SyntaxKind::RBracket);
                lhs = m.complete(self, SyntaxKind::IndexExpr);
                continue;
            }

            // struct literal
            if op == SyntaxKind::LBrace
                && restrictions.allow_struct_expr
                && lhs.kind(self) == SyntaxKind::NameRef
            {
                const STRUCT_BP: u8 = 15;
                if STRUCT_BP < min_bp {
                    break;
                }
                let m = lhs.precede(self);
                self.struct_expr_field_list();
                lhs = m.complete(self, SyntaxKind::StructExpr);
                continue;
            }

            // cast
            if op == SyntaxKind::As {
                const CAST_BP: u8 = 13;
                if CAST_BP < min_bp {
                    break;
                }
                let m = lhs.precede(self);
                self.bump(); // 'as'
                self.ty();
                lhs = m.complete(self, SyntaxKind::CastExpr);
                continue;
            }

            // infix
            // binary
            let (l_bp, r_bp) = match infix_binding_power(op) {
                Some(bp) => bp,
                None => break,
            };

            if l_bp < min_bp {
                break;
            }

            let m = lhs.precede(self);
            self.bump(); // operator
            self.expr_bp_restricted(r_bp, restrictions);
            lhs = m.complete(self, SyntaxKind::BinaryExpr);
        }

        Some(lhs)
    }

    fn arg_list(&mut self) {
        let m = self.start();
        self.bump();

        if !self.at(SyntaxKind::RParen) && !self.at(SyntaxKind::Eof) {
            self.expression();
            while self.at(SyntaxKind::Comma) {
                self.bump();
                if self.at(SyntaxKind::RParen) {
                    break;
                }
                self.expression();
            }
        }

        self.expect(SyntaxKind::RParen);
        m.complete(self, SyntaxKind::ArgList);
    }

    // parse prefix, atom, block
    fn lhs(&mut self, restrictions: ExprRestrictions) -> Option<CompletedMarker> {
        match self.current() {
            // unary
            SyntaxKind::Amp => {
                let m = self.start();
                self.bump(); // &
                if self.at(SyntaxKind::Mut) {
                    self.bump(); // mut
                }
                let r_bp = prefix_binding_power(SyntaxKind::Amp);
                self.expr_bp_restricted(r_bp, restrictions);
                Some(m.complete(self, SyntaxKind::UnaryExpr))
            }
            SyntaxKind::Plus
            | SyntaxKind::Minus
            | SyntaxKind::Star
            | SyntaxKind::Bang => {
                let m = self.start();
                let op = self.current();
                self.bump(); // operator
                let r_bp = prefix_binding_power(op);
                self.expr_bp_restricted(r_bp, restrictions);
                Some(m.complete(self, SyntaxKind::UnaryExpr))
            }

            SyntaxKind::Dot => {
                self.error_no_bump("expected expression before field access".to_string());
                let m = self.start();
                Some(m.complete(self, SyntaxKind::ErrorNode))
            }

            SyntaxKind::AmpAmp => {
                let m = self.start();
                let op = self.current();
                self.bump(); // &&
                let r_bp = prefix_binding_power(op);
                self.expr_bp_restricted(r_bp, restrictions);
                Some(m.complete(self, SyntaxKind::UnaryExpr))
            }

            SyntaxKind::Number => {
                let m = self.start();
                self.bump();
                Some(m.complete(self, SyntaxKind::NumberLit))
            }

            SyntaxKind::Float => {
                let m = self.start();
                self.bump();
                Some(m.complete(self, SyntaxKind::FloatLit))
            }

            SyntaxKind::String => {
                let m = self.start();
                self.bump();
                Some(m.complete(self, SyntaxKind::StringLit))
            }

            SyntaxKind::Char => {
                let m = self.start();
                self.bump();
                Some(m.complete(self, SyntaxKind::CharLit))
            }

            SyntaxKind::True | SyntaxKind::False => {
                let m = self.start();
                self.bump();
                Some(m.complete(self, SyntaxKind::BoolLit))
            }

            SyntaxKind::Ident
            | SyntaxKind::SelfKw
            | SyntaxKind::SuperKw
            | SyntaxKind::CrateKw
            | SyntaxKind::ColonColon => {
                let m = self.start();
                self.path();
                Some(m.complete(self, SyntaxKind::NameRef))
            }

            SyntaxKind::LBrace => Some(self.block()),

            SyntaxKind::If => Some(self.if_expr()),

            SyntaxKind::While => Some(self.while_expr()),

            SyntaxKind::Match => Some(self.match_expr()),

            SyntaxKind::Unsafe => Some(self.unsafe_expr()),

            SyntaxKind::LBracket => {
                let m = self.start();
                self.bump();

                if !self.at(SyntaxKind::RBracket) && !self.at(SyntaxKind::Eof) {
                    self.expression();
                    while self.at(SyntaxKind::Comma) {
                        self.bump();
                        if self.at(SyntaxKind::RBracket) {
                            break;
                        }
                        self.expression();
                    }
                }

                self.expect(SyntaxKind::RBracket);
                Some(m.complete(self, SyntaxKind::ArrayExpr))
            }

            SyntaxKind::LParen => {
                let m = self.start();
                self.bump();
                self.expr_bp_restricted(0, restrictions);
                self.expect(SyntaxKind::RParen);
                Some(m.complete(self, SyntaxKind::ParenExpr))
            }

            _ => {
                self.error_no_bump(format!("expected expression, found {:?}", self.current()));
                None
            }
        }
    }

    fn struct_expr_field_list(&mut self) {
        self.expect(SyntaxKind::LBrace);

        if !self.at(SyntaxKind::RBrace) && !self.at(SyntaxKind::Eof) {
            self.struct_expr_field();
            while self.at(SyntaxKind::Comma) {
                self.bump();
                if self.at(SyntaxKind::RBrace) {
                    break;
                }
                self.struct_expr_field();
            }
        }

        self.expect(SyntaxKind::RBrace);
    }

    fn struct_expr_field(&mut self) {
        let m = self.start();

        self.expect(SyntaxKind::Ident);
        if self.at(SyntaxKind::Colon) {
            self.bump();
            self.expression();
        }

        m.complete(self, SyntaxKind::StructExprField);
    }

    // == type ==

    fn ty(&mut self) {
        match self.current() {
            SyntaxKind::Amp => {
                let m = self.start();
                self.bump(); // &
                if self.at(SyntaxKind::Mut) {
                    self.bump(); // mut
                }
                self.ty();
                m.complete(self, SyntaxKind::RefType);
            }
            SyntaxKind::AmpAmp => {
                let outer = self.start();
                let inner = self.start();
                self.bump();
                self.ty();
                inner.complete(self, SyntaxKind::RefType);
                outer.complete(self, SyntaxKind::RefType);
            }
            SyntaxKind::Star => {
                let m = self.start();
                self.bump(); // *
                let is_mut = self.at(SyntaxKind::Mut);
                if is_mut || self.at(SyntaxKind::Const) {
                    self.bump(); // const or mut
                } else {
                    self.error(format!(
                        "expected 'const' or 'mut' after '*' in pointer type, found {:?}",
                        self.current()
                    ));
                }
                self.ty();
                m.complete(self, SyntaxKind::PtrType);
            }
            SyntaxKind::LParen => {
                let m = self.start();
                self.bump();

                if !self.at(SyntaxKind::RParen) && !self.at(SyntaxKind::Eof) {
                    self.ty();
                    while self.at(SyntaxKind::Comma) {
                        self.bump();
                        if self.at(SyntaxKind::RParen) {
                            break;
                        }
                        self.ty();
                    }
                }

                self.expect(SyntaxKind::RParen);
                m.complete(self, SyntaxKind::TupleType);
            }
            SyntaxKind::LBracket => {
                let m = self.start();
                self.bump();
                self.ty();

                if self.at(SyntaxKind::Semi) {
                    self.bump();
                    if !self.at(SyntaxKind::RBracket) && !self.at(SyntaxKind::Eof) {
                        self.expression();
                    }
                }

                self.expect(SyntaxKind::RBracket);
                m.complete(self, SyntaxKind::ArrayType);
            }
            SyntaxKind::Ident
            | SyntaxKind::SelfKw
            | SyntaxKind::SuperKw
            | SyntaxKind::CrateKw
            | SyntaxKind::ColonColon => {
                let m = self.start();
                self.path();
                m.complete(self, SyntaxKind::NamedType);
            }
            _ => self.error(format!("expected type, found {:?}", self.current())),
        }
    }

    // == new items ==

    fn enum_decl(&mut self) {
        let m = self.start();

        self.expect(SyntaxKind::Enum);
        self.expect(SyntaxKind::Ident);
        self.expect(SyntaxKind::LBrace);

        if !self.at(SyntaxKind::RBrace) && !self.at(SyntaxKind::Eof) {
            self.enum_variant();
            while self.at(SyntaxKind::Comma) {
                self.bump();
                if self.at(SyntaxKind::RBrace) {
                    break;
                }
                self.enum_variant();
            }
        }

        self.expect(SyntaxKind::RBrace);
        m.complete(self, SyntaxKind::EnumDecl);
    }

    fn enum_variant(&mut self) {
        let m = self.start();

        self.expect(SyntaxKind::Ident);

        // tuple variant: ident(type, type, ...)
        if self.at(SyntaxKind::LParen) {
            self.bump();
            if !self.at(SyntaxKind::RParen) && !self.at(SyntaxKind::Eof) {
                self.ty();
                while self.at(SyntaxKind::Comma) {
                    self.bump();
                    if self.at(SyntaxKind::RParen) {
                        break;
                    }
                    self.ty();
                }
            }
            self.expect(SyntaxKind::RParen);
        }

        // struct variant: ident { field: ty, ... }
        else if self.at(SyntaxKind::LBrace) {
            self.struct_field_list();
        }

        m.complete(self, SyntaxKind::EnumVariant);
    }

    fn trait_decl(&mut self) {
        let m = self.start();

        self.expect(SyntaxKind::Trait);
        self.expect(SyntaxKind::Ident);
        self.expect(SyntaxKind::LBrace);

        while !self.at(SyntaxKind::RBrace) && !self.at(SyntaxKind::Eof) {
            self.trait_item();
        }

        self.expect(SyntaxKind::RBrace);
        m.complete(self, SyntaxKind::TraitDecl);
    }

    fn trait_item(&mut self) {
        match self.current() {
            SyntaxKind::Fun => self.func_sig(),
            SyntaxKind::TypeKw => self.type_alias_decl(),
            _ => {
                self.error(format!("expected trait item, found {:?}", self.current()));
            }
        }
    }

    fn func_sig(&mut self) {
        let m = self.start();

        self.expect(SyntaxKind::Fun);
        self.expect(SyntaxKind::Ident);
        self.param_list();

        if self.at(SyntaxKind::Arrow) {
            self.bump();
            self.ty();
        }

        self.expect(SyntaxKind::Semi);
        m.complete(self, SyntaxKind::FuncDecl);
    }

    fn impl_decl(&mut self) {
        let m = self.start();

        self.expect(SyntaxKind::Impl);

        // optional generic_params
        if self.at(SyntaxKind::Less) {
            self.generic_params();
        }

        self.path();

        // optional "for" ty
        if self.at(SyntaxKind::For) {
            self.bump();
            self.ty();
        }

        self.expect(SyntaxKind::LBrace);

        while !self.at(SyntaxKind::RBrace) && !self.at(SyntaxKind::Eof) {
            self.impl_item();
        }

        self.expect(SyntaxKind::RBrace);
        m.complete(self, SyntaxKind::ImplDecl);
    }

    fn impl_item(&mut self) {
        match self.current() {
            SyntaxKind::Fun => self.func_decl(),
            SyntaxKind::TypeKw => self.type_alias_decl(),
            SyntaxKind::Const => self.const_decl(),
            _ => {
                self.error(format!("expected impl item, found {:?}", self.current()));
            }
        }
    }

    fn const_decl(&mut self) {
        let m = self.start();

        self.expect(SyntaxKind::Const);
        self.expect(SyntaxKind::Ident);
        self.expect(SyntaxKind::Colon);
        self.ty();

        if self.at(SyntaxKind::Eq) {
            self.bump();
            self.expression();
        }

        self.expect(SyntaxKind::Semi);
        m.complete(self, SyntaxKind::ConstDecl);
    }

    fn type_alias_decl(&mut self) {
        let m = self.start();

        self.expect(SyntaxKind::TypeKw);
        self.expect(SyntaxKind::Ident);

        if self.at(SyntaxKind::Eq) {
            self.bump();
            self.ty();
        }

        self.expect(SyntaxKind::Semi);
        m.complete(self, SyntaxKind::TypeAliasDecl);
    }

    fn generic_params(&mut self) {
        let m = self.start();

        self.expect(SyntaxKind::Less);
        self.expect(SyntaxKind::Ident);
        while self.at(SyntaxKind::Comma) {
            self.bump();
            self.expect(SyntaxKind::Ident);
        }
        self.expect(SyntaxKind::Greater);

        m.complete(self, SyntaxKind::GenericParams);
    }

    fn type_list(&mut self) {
        self.ty();
        while self.at(SyntaxKind::Comma) {
            self.bump();
            if !self.at_type_start() {
                break;
            }
            self.ty();
        }
    }

    fn extern_decl(&mut self) {
        let m = self.start();
        self.expect(SyntaxKind::Extern);
        let _abi = self.expect(SyntaxKind::String); // "C"

        if self.at(SyntaxKind::Fun) {
            // extern "C" fun name(...) -> T { body }
            self.func_decl();
            m.complete(self, SyntaxKind::ExternFnDecl);
        } else if self.at(SyntaxKind::LBrace) {
            // extern "C" { fun ...; fun ...; }
            self.expect(SyntaxKind::LBrace);
            while !self.at(SyntaxKind::RBrace) && !self.at(SyntaxKind::Eof) {
                if self.at(SyntaxKind::Fun) {
                    self.func_sig();
                } else {
                    self.error(format!(
                        "expected 'fun' in extern block, found {:?}",
                        self.current()
                    ));
                    break;
                }
            }
            self.expect(SyntaxKind::RBrace);
            m.complete(self, SyntaxKind::ExternBlock);
        } else {
            self.error(format!(
                "expected 'fun' or '{{' after extern \"C\", found {:?}",
                self.current()
            ));
            m.complete(self, SyntaxKind::ErrorNode);
        }
    }

    fn at_type_start(&self) -> bool {
        matches!(
            self.current(),
            SyntaxKind::Ident
                | SyntaxKind::SelfKw
                | SyntaxKind::SuperKw
                | SyntaxKind::CrateKw
                | SyntaxKind::ColonColon
                | SyntaxKind::Amp
                | SyntaxKind::AmpAmp
                | SyntaxKind::Star
                | SyntaxKind::LParen
                | SyntaxKind::LBracket
        )
    }

    // == patterns ==

    fn pattern(&mut self) {
        self.pattern_inner(false);
    }

    /// When `top_level` is true, we are at the start of a pattern and can see `&`.
    fn pattern_inner(&mut self, _top_level: bool) {
        match self.current() {
            SyntaxKind::Underscore => {
                let m = self.start();
                self.bump();
                m.complete(self, SyntaxKind::WildcardPattern);
            }
            SyntaxKind::Number
            | SyntaxKind::Float
            | SyntaxKind::String
            | SyntaxKind::Char
            | SyntaxKind::True
            | SyntaxKind::False => {
                let m = self.start();
                self.bump();
                m.complete(self, SyntaxKind::LiteralPattern);
            }
            SyntaxKind::LParen => {
                let m = self.start();
                self.bump();

                if !self.at(SyntaxKind::RParen) && !self.at(SyntaxKind::Eof) {
                    self.pattern();
                    while self.at(SyntaxKind::Comma) {
                        self.bump();
                        if self.at(SyntaxKind::RParen) {
                            break;
                        }
                        self.pattern();
                    }
                }

                self.expect(SyntaxKind::RParen);
                m.complete(self, SyntaxKind::TuplePattern);
            }
            SyntaxKind::Ident
            | SyntaxKind::SelfKw
            | SyntaxKind::SuperKw
            | SyntaxKind::CrateKw
            | SyntaxKind::ColonColon => {
                let m = self.start();
                self.path();

                match self.current() {
                    SyntaxKind::LParen => {
                        // enum tuple pattern: Variant(a, b)
                        self.bump();
                        if !self.at(SyntaxKind::RParen) && !self.at(SyntaxKind::Eof) {
                            self.pattern();
                            while self.at(SyntaxKind::Comma) {
                                self.bump();
                                if self.at(SyntaxKind::RParen) {
                                    break;
                                }
                                self.pattern();
                            }
                        }
                        self.expect(SyntaxKind::RParen);
                        m.complete(self, SyntaxKind::EnumPattern);
                    }
                    SyntaxKind::LBrace => {
                        // enum struct pattern: Variant { a, b: c }
                        self.bump();
                        if !self.at(SyntaxKind::RBrace) && !self.at(SyntaxKind::Eof) {
                            self.field_pattern();
                            while self.at(SyntaxKind::Comma) {
                                self.bump();
                                if self.at(SyntaxKind::RBrace) {
                                    break;
                                }
                                self.field_pattern();
                            }
                        }
                        self.expect(SyntaxKind::RBrace);
                        m.complete(self, SyntaxKind::EnumPattern);
                    }
                    _ => {
                        // Plain ident binding or path pattern.
                        m.complete(self, SyntaxKind::EnumPattern);
                    }
                }
            }
            _ => {
                self.error(format!("expected pattern, found {:?}", self.current()));
            }
        }
    }

    fn field_pattern(&mut self) {
        let m = self.start();
        self.expect(SyntaxKind::Ident);

        if self.at(SyntaxKind::Colon) {
            self.bump();
            self.pattern();
        }

        m.complete(self, SyntaxKind::StructPattern);
    }
}

// == pratt binding power ==

/// prefix binding power for `rhs`
fn prefix_binding_power(op: SyntaxKind) -> u8 {
    match op {
        SyntaxKind::Plus
        | SyntaxKind::Minus
        | SyntaxKind::Amp
        | SyntaxKind::AmpAmp
        | SyntaxKind::Star
        | SyntaxKind::Bang => 14,
        _ => 0,
    }
}

/// infix operator (left bp, right bp)
///
/// left < right => left combination
///
/// left > right => right combination

fn infix_binding_power(op: SyntaxKind) -> Option<(u8, u8)> {
    match op {
        SyntaxKind::Eq => Some((1, 1)),
        SyntaxKind::PipePipe => Some((2, 3)),
        SyntaxKind::AmpAmp => Some((4, 5)),
        SyntaxKind::EqEq | SyntaxKind::BangEq => Some((6, 7)),
        SyntaxKind::Less | SyntaxKind::Greater | SyntaxKind::LessEq | SyntaxKind::GreaterEq => {
            Some((8, 9))
        }
        SyntaxKind::Plus | SyntaxKind::Minus => Some((10, 11)),
        SyntaxKind::Star | SyntaxKind::Slash | SyntaxKind::Percent => Some((12, 13)),
        _ => None,
    }
}
