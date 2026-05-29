use super::{lexer::Token, syntax_kind::SyntaxKind};

#[derive(Debug, Clone)]
pub enum Event {
    StartNode {
        kind: SyntaxKind,
        forward_parent: Option<usize>,
    },
    FinishNode,
    AddToken,
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
            Event::StartNode {
                kind: solt,
                forward_parent: _,
            } => *solt = kind,
            _ => unreachable!(),
        }
        p.events.push(Event::FinishNode);
        CompletedMarker { pos: self.pos }
    }

    // don't make node
    pub fn abandon(mut self, p: &mut Parser) {
        self.completed = true;
        if self.pos == p.events.len() - 1 {
            match p.events.pop() {
                Some(Event::StartNode {
                    kind: SyntaxKind::Tombstone,
                    ..
                }) => {}
                _ => unreachable!(),
            }
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

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize, // include trivia
    pub(crate) events: Vec<Event>,
    pub errors: Vec<String>,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            pos: 0,
            events: vec![],
            errors: vec![],
        }
    }

    // Now non-trivia token.
    fn current(&self) -> SyntaxKind {
        self.nth(0)
    }

    // Look ahead to the nth non-trivia token.
    fn nth(&self, n: usize) -> SyntaxKind {
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

    pub fn parse(mut self) -> (Vec<Event>, Vec<Token>, Vec<String>) {
        let m = self.start();

        while !self.at(SyntaxKind::Eof) {
            self.statement();
        }
        self.eat_trivia();
        m.complete(&mut self, SyntaxKind::Root);

        (self.events, self.tokens, self.errors)
    }

    // == stmt ==

    fn statement(&mut self) {
        match self.current() {
            SyntaxKind::Let => self.var_decl(),
            SyntaxKind::Fun => self.func_decl(),
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

        self.expect(SyntaxKind::Arrow);
        self.ty();

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

    fn block(&mut self) -> CompletedMarker {
        let m = self.start();
        self.bump();

        while !self.at(SyntaxKind::RBrace) && !self.at(SyntaxKind::Eof) {
            self.statement();
        }

        self.expect(SyntaxKind::RBrace);
        m.complete(self, SyntaxKind::Block)
    }

    fn expr_stmt(&mut self) {
        let m = self.start();
        self.expression();
        self.expect(SyntaxKind::Semi);

        m.complete(self, SyntaxKind::ExprStmt);
    }

    // == expr ==

    fn expression(&mut self) {
        self.expr_bp(0);
    }

    fn expr_bp(&mut self, min_bp: u8) -> Option<CompletedMarker> {
        // prefix
        let mut lhs = self.lhs()?;

        // infix loop
        loop {
            let op = self.current();
            let (l_bp, r_bp) = match infix_binding_power(op) {
                Some(bp) => bp,
                None => break,
            };

            if l_bp < min_bp {
                break;
            }

            let m = lhs.precede(self);
            self.bump();
            self.expr_bp(r_bp);
            lhs = m.complete(self, SyntaxKind::BinaryExpr);
        }

        Some(lhs)
    }

    // parse prefix, atom, block
    fn lhs(&mut self) -> Option<CompletedMarker> {
        match self.current() {
            // unary
            SyntaxKind::Plus | SyntaxKind::Minus | SyntaxKind::Amp | SyntaxKind::Star => {
                let m = self.start();
                let op = self.current();
                self.bump(); // operator
                let r_bp = prefix_binding_power(op);
                self.expr_bp(r_bp);
                Some(m.complete(self, SyntaxKind::UnaryExpr))
            }

            SyntaxKind::Number => {
                let m = self.start();
                self.bump();
                Some(m.complete(self, SyntaxKind::Number))
            }

            SyntaxKind::Ident => {
                let m = self.start();
                self.bump();
                Some(m.complete(self, SyntaxKind::Ident))
            }

            SyntaxKind::LBrace => Some(self.block()),

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
            SyntaxKind::Ident => {
                self.bump();
            }
            _ => self.error(format!("expected type, found {:?}", self.current())),
        }
    }
}

// == pratt binding power ==

/// prefix binding power for `rhs`
fn prefix_binding_power(op: SyntaxKind) -> u8 {
    match op {
        SyntaxKind::Plus | SyntaxKind::Minus | SyntaxKind::Amp | SyntaxKind::Star => 7,
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
        SyntaxKind::Plus | SyntaxKind::Minus => Some((1, 2)),
        SyntaxKind::Star | SyntaxKind::Slash => Some((3, 4)),
        _ => None,
    }
}
