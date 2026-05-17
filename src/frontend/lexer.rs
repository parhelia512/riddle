use core::panic;

use super::token::{Span, Token, TokenKind};

#[derive(Debug)]
pub struct Lexer {
    source: Vec<char>,
    pub start: usize,
    pub current: usize,
    pub tokens: Vec<Token>,
}

impl Lexer {
    pub fn new(source: String) -> Self {
        Lexer {
            source: source.chars().collect(),
            tokens: vec![],
            start: 0,
            current: 0,
        }
    }

    fn is_end(&self) -> bool {
        self.current >= self.source.len()
    }

    fn advance(&mut self) -> char {
        let r = self.source[self.current];
        self.current += 1;
        r
    }

    fn add_token(&mut self, kind: TokenKind) {
        let text: String = self.source[self.start..self.current].iter().collect();
        self.tokens.push(Token {
            span: Span {
                start: self.start,
                end: self.current,
            },
            kind,
            lexeme: text,
        })
    }

    fn add_token_literal(&mut self, kind: TokenKind, literal: String){
        self.tokens.push(Token {
            span: Span {
                start: self.start,
                end: self.current,
            },
            kind,
            lexeme: literal,
        })
    }

    pub fn scan(&mut self) {
        while !self.is_end() {
            self.start = self.current;
            self.scan_token();
        }

        self.tokens.push(Token {
            span: Span {
                start: self.current,
                end: self.current,
            },
            kind: TokenKind::Eof,
            lexeme: "".to_string(),
        })
    }

    fn eat(&mut self, expected: char) -> bool {
        if self.is_end() {
            return false;
        }
        if self.source[self.current] != expected {
            return false;
        }
        self.current += 1;
        true
    }

    fn peek(&self) -> char {
        if self.is_end() {
            '\0'
        } else {
            self.source[self.current]
        }
    }

    fn peek2(&self)->char{
        if self.current+1 >= self.source.len() {
            '\0'
        }else{
            self.source[self.current+1]
        }
    }

    fn scan_token(&mut self) {
        use TokenKind::*;
        let c = self.advance();
        match c {
            '(' => self.add_token(LeftParen),
            ')' => self.add_token(RightParen),
            '[' => self.add_token(LeftBracket),
            ']' => self.add_token(RightBracket),
            '{' => self.add_token(LeftBrace),
            '}' => self.add_token(RightBrace),
            ',' => self.add_token(Comma),
            '.' => self.add_token(Dot),
            '+' => self.add_token(Plus),
            '-' => self.add_token(Minus),
            '*' => self.add_token(Star),
            '!' => {
                let k = if self.eat('=') { BangEqual } else { Bang };
                self.add_token(k);
            }
            '=' => {
                let kind = if self.eat('=') { EqualEqual } else { Equal };
                self.add_token(kind);
            }
            '>' => {
                let kind = if self.eat('=') { GreaterEqual } else { Greater };
                self.add_token(kind);
            }
            '<' => {
                let kind = if self.eat('=') { LessEqual } else { Less };
                self.add_token(kind);
            }
            '&' => {
                let kind = if self.eat('&') { AmpAmp } else { Amp };
                self.add_token(kind);
            }
            '|' => {
                let kind = if self.eat('|') { PipePipe } else { Pipe };
                self.add_token(kind);
            }
            '/' => {
                if self.eat('/') {
                    while self.peek() != '\n' && !self.is_end() {
                        self.advance();
                    }
                } else {
                    self.add_token(Slash)
                }
            }
            ';' => self.add_token(Semi),
            ':' => self.add_token(Colon),
            '"' => self.string(),
            ' ' | '\r' | '\t' | '\n' => (),
            _ => {
                if c.is_digit(10){
                    self.number();
                } else if c.is_alphabetic() {
                    self.identifier();
                }
                panic!("Unexpected character.")
            }
        }
    }
    
    fn string(&mut self){
        while self.peek() != '"' && !self.is_end(){
            self.advance();
        }
        if self.is_end(){
            panic!("Unterminated string.");
            return;
        }
        self.advance();
        let value:String = self.source[self.start+1..self.current-1].iter().collect();
        self.add_token_literal(TokenKind::Str, value);
    }

    fn number(&mut self){
        while self.peek().is_digit(10){
            self.advance();
        }
        if self.peek() == '.' &&  self.peek2().is_digit(10){
            self.advance();
            while self.peek().is_digit(10){
                self.advance();
            }
        }
        self.add_token(TokenKind::Number)
    }

    fn identifier(&mut self){
        while self.peek().is_alphanumeric(){
            self.advance();
        }
        self.add_token(TokenKind::Identifier);
    }
}
