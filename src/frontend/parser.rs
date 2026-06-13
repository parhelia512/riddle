use super::{lexer::Token, syntax_kind::SyntaxKind};

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

// It can be traced back using precede().
#[derive(Debug, Clone, Copy)]
pub struct CompletedMarker {
    pos: usize,
}

impl CompletedMarker {
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

pub struct Parser<'s> {
    source: &'s str,
    tokens: Vec<Token>,
    pos: usize, // include trivia
    pub(crate) events: Vec<Event>,
    pub errors: Vec<String>,
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

    // Now non-trivia token.
    #[inline(always)]
    fn current(&self) -> SyntaxKind {
        self.current_kind
    }

    // Look ahead to the nth non-trivia token.
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

    #[allow(unused)]
    fn at_any(&self, kinds: &[SyntaxKind]) -> bool {
        kinds.contains(&self.current())
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
            true
        } else {
            self.error(format!("expected {:?}, found {:?}", kind, self.current()));
            false
        }
    }

    fn error(&mut self, msg: String) {
        self.errors.push(msg);
        if !self.at(SyntaxKind::Eof) {
            let m = self.start();
            self.bump();
            m.complete(self, SyntaxKind::ErrorNode);
        }
    }

    fn error_no_bump(&mut self, msg: String) {
        self.errors.push(msg);
    }

    pub fn parse(mut self) -> (Vec<Event>, Vec<Token>, Vec<String>, &'s str) {
        let m = self.start();

        while !self.at(SyntaxKind::Eof) {
            self.statement();
        }
        self.eat_trivia();
        m.complete(&mut self, SyntaxKind::Root);

        (self.events, self.tokens, self.errors, self.source)
    }

    fn at_stmt_start(&self) -> bool {
        matches!(
            self.current(),
            SyntaxKind::Let | SyntaxKind::Fun | SyntaxKind::Struct | SyntaxKind::Return
        )
    }

    fn at_expr_start(&self) -> bool {
        matches!(
            self.current(),
            SyntaxKind::Number
                | SyntaxKind::Ident
                | SyntaxKind::LParen
                | SyntaxKind::LBrace
                | SyntaxKind::If
                | SyntaxKind::While
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
            SyntaxKind::LBrace | SyntaxKind::If | SyntaxKind::While
        )
    }

    // == stmt ==

    fn statement(&mut self) {
        match self.current() {
            SyntaxKind::Let => self.var_decl(),
            SyntaxKind::Fun => self.func_decl(),
            SyntaxKind::Struct => self.struct_decl(),
            SyntaxKind::Return => self.return_stmt(),
            SyntaxKind::LBrace => {
                self.block();
            }
            SyntaxKind::Eof => return,
            _ => self.expr_stmt(),
        }
    }

    fn var_decl(&mut self) {
        let m = self.start();
        self.bump();
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
            None => return,
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

    fn if_expr(&mut self) -> CompletedMarker {
        let m = self.start();
        self.expect(SyntaxKind::If);
        self.expression();

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
        self.expression();

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

    fn expr_bp(&mut self, min_bp: u8) -> Option<CompletedMarker> {
        // prefix
        let mut lhs = self.lhs()?;

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
            self.expr_bp(r_bp);
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
    fn lhs(&mut self) -> Option<CompletedMarker> {
        match self.current() {
            // unary
            SyntaxKind::Plus
            | SyntaxKind::Minus
            | SyntaxKind::Amp
            | SyntaxKind::Star
            | SyntaxKind::Bang => {
                let m = self.start();
                let op = self.current();
                self.bump(); // operator
                let r_bp = prefix_binding_power(op);
                self.expr_bp(r_bp);
                Some(m.complete(self, SyntaxKind::UnaryExpr))
            }

            SyntaxKind::AmpAmp => {
                let m = self.start();
                let op = self.current();
                self.bump(); // &&
                let r_bp = prefix_binding_power(op);
                self.expr_bp(r_bp);
                Some(m.complete(self, SyntaxKind::UnaryExpr))
            }
            

            SyntaxKind::Number => {
                let m = self.start();
                self.bump();
                Some(m.complete(self, SyntaxKind::NumberLit))
            }

            SyntaxKind::Ident => {
                let m = self.start();
                self.bump();
                Some(m.complete(self, SyntaxKind::NameRef))
            }

            SyntaxKind::LBrace => Some(self.block()),

            SyntaxKind::If => Some(self.if_expr()),

            SyntaxKind::While => Some(self.while_expr()),

            SyntaxKind::LParen => {
                let m = self.start();
                self.bump();
                self.expr_bp(0);
                self.expect(SyntaxKind::RParen);
                Some(m.complete(self, SyntaxKind::ParenExpr))
            }

            _ => {
                self.error_no_bump(format!("expected expression, found {:?}", self.current()));
                None
            }
        }
    }

    // == type ==

    fn ty(&mut self) {
        match self.current() {
            SyntaxKind::Amp => {
                let m = self.start();
                self.bump();
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
            SyntaxKind::Ident => {
                let m = self.start();
                self.bump();
                m.complete(self, SyntaxKind::NamedType);
            }
            _ => self.error(format!("expected type, found {:?}", self.current())),
        }
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
        | SyntaxKind::Bang => 13,
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
        SyntaxKind::PipePipe => Some((1, 2)),
        SyntaxKind::AmpAmp => Some((3, 4)),
        SyntaxKind::EqEq | SyntaxKind::BangEq => Some((5, 6)),
        SyntaxKind::Less | SyntaxKind::Greater | SyntaxKind::LessEq | SyntaxKind::GreaterEq => {
            Some((7, 8))
        }
        SyntaxKind::Plus | SyntaxKind::Minus => Some((9, 10)),
        SyntaxKind::Star | SyntaxKind::Slash | SyntaxKind::Percent => Some((11, 12)),
        _ => None,
    }
}
