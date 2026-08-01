use ast::{self, support::AstNode};
use frontend::{
    incremental::{IncrementalParser, parse_tokens},
    lexer,
    syntax_kind::{SyntaxKind, SyntaxNode},
    tree_builder::Parse,
};
use rowan::TextRange;
use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    fmt::Write as _,
    ops::Range,
    path::PathBuf,
};
use type_checker::{Diagnostic, LabelStyle, Severity, SourceLabel};

const PROC_MACRO_ERROR: &str = "E0400";
const TOKEN_WIRE_HEADER: &str = "RMT1;";
const MAX_DERIVE_EXPANSION_DEPTH: usize = 32;
const STANDARD_MACRO_PACKAGE: &str = "std";

pub const STANDARD_FUNCTION_MACROS: [&str; 2] = ["print", "println"];
pub const STANDARD_DERIVE_MACROS: [&str; 1] = ["Debug"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcMacroDelimiter {
    Parenthesis,
    Brace,
    Bracket,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcMacroSpacing {
    Alone,
    Joint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcMacroTokenTree {
    Group {
        delimiter: ProcMacroDelimiter,
        stream: ProcMacroTokenStream,
        span: Range<usize>,
    },
    Ident {
        text: String,
        span: Range<usize>,
    },
    Punct {
        value: char,
        spacing: ProcMacroSpacing,
        span: Range<usize>,
    },
    Literal {
        text: String,
        span: Range<usize>,
    },
}

impl ProcMacroTokenTree {
    pub fn span(&self) -> &Range<usize> {
        match self {
            Self::Group { span, .. }
            | Self::Ident { span, .. }
            | Self::Punct { span, .. }
            | Self::Literal { span, .. } => span,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcMacroTokenStream {
    pub trees: Vec<ProcMacroTokenTree>,
}

struct RenderedTokenStream {
    source: String,
    spans: Vec<RenderedTokenSpan>,
}

struct RenderedTokenSpan {
    generated: Range<usize>,
    original: Range<usize>,
}

impl ProcMacroTokenStream {
    pub fn from_source(source: &str, offset: usize) -> Result<Self, String> {
        let tokens = lexer::lex(source);
        let mut index = 0;
        let (stream, closed_at) = parse_lexed_stream(source, &tokens, &mut index, offset, None)?;
        debug_assert!(closed_at.is_none());
        Ok(stream)
    }

    pub fn is_empty(&self) -> bool {
        self.trees.is_empty()
    }

    pub fn extend(&mut self, other: Self) {
        self.trees.extend(other.trees);
    }

    pub fn set_span(&mut self, span: Range<usize>) {
        for tree in &mut self.trees {
            match tree {
                ProcMacroTokenTree::Group {
                    stream,
                    span: tree_span,
                    ..
                } => {
                    *tree_span = span.clone();
                    stream.set_span(span.clone());
                }
                ProcMacroTokenTree::Ident {
                    span: tree_span, ..
                }
                | ProcMacroTokenTree::Punct {
                    span: tree_span, ..
                }
                | ProcMacroTokenTree::Literal {
                    span: tree_span, ..
                } => *tree_span = span.clone(),
            }
        }
    }

    pub fn to_source(&self) -> String {
        self.render().source
    }

    fn render(&self) -> RenderedTokenStream {
        let mut output = String::new();
        let mut spans = Vec::new();
        write_token_stream(self, &mut output, &mut spans);
        RenderedTokenStream {
            source: output,
            spans,
        }
    }

    pub fn encode(&self) -> String {
        let mut output = String::from(TOKEN_WIRE_HEADER);
        encode_stream(self, &mut output);
        output
    }

    pub fn decode(input: &str) -> Result<Self, String> {
        let mut parser = TokenWireParser::new(input);
        parser.expect_bytes(TOKEN_WIRE_HEADER.as_bytes())?;
        let stream = parser.parse_stream()?;
        if parser.index != input.len() {
            return Err("trailing data in proc-macro token stream".into());
        }
        Ok(stream)
    }
}

fn parse_lexed_stream(
    source: &str,
    tokens: &[lexer::Token],
    index: &mut usize,
    offset: usize,
    closing: Option<SyntaxKind>,
) -> Result<(ProcMacroTokenStream, Option<usize>), String> {
    let mut stream = ProcMacroTokenStream::default();
    while let Some(token) = tokens.get(*index) {
        if Some(token.kind) == closing {
            *index += 1;
            return Ok((stream, Some(offset + token.span.end)));
        }
        if is_closing_delimiter(token.kind) {
            return Err(format!(
                "unexpected closing delimiter at byte {}",
                offset + token.span.start
            ));
        }
        if matches!(token.kind, SyntaxKind::Whitespace | SyntaxKind::LineComment) {
            *index += 1;
            continue;
        }
        if token.kind == SyntaxKind::ErrorNode {
            return Err(format!(
                "invalid token at byte {}",
                offset + token.span.start
            ));
        }
        if let Some((delimiter, expected)) = opening_delimiter(token.kind) {
            let start = offset + token.span.start;
            *index += 1;
            let (inner, end) = parse_lexed_stream(source, tokens, index, offset, Some(expected))?;
            let Some(end) = end else {
                return Err(format!("unclosed delimiter at byte {start}"));
            };
            stream.trees.push(ProcMacroTokenTree::Group {
                delimiter,
                stream: inner,
                span: start..end,
            });
            continue;
        }

        let text = token.text(source);
        let span = offset + token.span.start..offset + token.span.end;
        if matches!(
            token.kind,
            SyntaxKind::String | SyntaxKind::Char | SyntaxKind::Float | SyntaxKind::Number
        ) {
            stream.trees.push(ProcMacroTokenTree::Literal {
                text: text.into(),
                span,
            });
            *index += 1;
            continue;
        }
        if text
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        {
            stream.trees.push(ProcMacroTokenTree::Ident {
                text: text.into(),
                span,
            });
            *index += 1;
            continue;
        }

        let next_is_joint = tokens.get(*index + 1).is_some_and(|next| {
            next.span.start == token.span.end && token_is_punctuation(next, source)
        });
        let mut chars = text.char_indices().peekable();
        while let Some((char_offset, value)) = chars.next() {
            if !value.is_ascii() {
                return Err(format!(
                    "non-ASCII punctuation at byte {}",
                    offset + token.span.start + char_offset
                ));
            }
            let spacing = if chars.peek().is_some() || next_is_joint {
                ProcMacroSpacing::Joint
            } else {
                ProcMacroSpacing::Alone
            };
            let start = offset + token.span.start + char_offset;
            stream.trees.push(ProcMacroTokenTree::Punct {
                value,
                spacing,
                span: start..start + value.len_utf8(),
            });
        }
        *index += 1;
    }
    if closing.is_some() {
        Err("unclosed delimiter in proc-macro input".into())
    } else {
        Ok((stream, None))
    }
}

fn opening_delimiter(kind: SyntaxKind) -> Option<(ProcMacroDelimiter, SyntaxKind)> {
    match kind {
        SyntaxKind::LParen => Some((ProcMacroDelimiter::Parenthesis, SyntaxKind::RParen)),
        SyntaxKind::LBrace => Some((ProcMacroDelimiter::Brace, SyntaxKind::RBrace)),
        SyntaxKind::LBracket => Some((ProcMacroDelimiter::Bracket, SyntaxKind::RBracket)),
        _ => None,
    }
}

fn is_closing_delimiter(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::RParen | SyntaxKind::RBrace | SyntaxKind::RBracket
    )
}

fn token_is_punctuation(token: &lexer::Token, source: &str) -> bool {
    !matches!(
        token.kind,
        SyntaxKind::Whitespace
            | SyntaxKind::LineComment
            | SyntaxKind::ErrorNode
            | SyntaxKind::LParen
            | SyntaxKind::RParen
            | SyntaxKind::LBrace
            | SyntaxKind::RBrace
            | SyntaxKind::LBracket
            | SyntaxKind::RBracket
            | SyntaxKind::String
            | SyntaxKind::Char
            | SyntaxKind::Float
            | SyntaxKind::Number
    ) && token
        .text(source)
        .bytes()
        .all(|byte| byte.is_ascii_punctuation())
}

fn write_token_stream(
    stream: &ProcMacroTokenStream,
    output: &mut String,
    spans: &mut Vec<RenderedTokenSpan>,
) {
    let mut first = true;
    let mut joint = false;
    for tree in &stream.trees {
        if !first && !joint {
            output.push(' ');
        }
        write_token_tree(tree, output, spans);
        joint = matches!(
            tree,
            ProcMacroTokenTree::Punct {
                spacing: ProcMacroSpacing::Joint,
                ..
            }
        );
        first = false;
    }
}

fn write_token_tree(
    tree: &ProcMacroTokenTree,
    output: &mut String,
    spans: &mut Vec<RenderedTokenSpan>,
) {
    match tree {
        ProcMacroTokenTree::Group {
            delimiter,
            stream,
            span,
        } => {
            let (opening, closing) = match delimiter {
                ProcMacroDelimiter::Parenthesis => (Some('('), Some(')')),
                ProcMacroDelimiter::Brace => (Some('{'), Some('}')),
                ProcMacroDelimiter::Bracket => (Some('['), Some(']')),
                ProcMacroDelimiter::None => (None, None),
            };
            if let Some(opening) = opening {
                push_mapped_char(output, opening, span, spans);
            }
            write_token_stream(stream, output, spans);
            if let Some(closing) = closing {
                push_mapped_char(output, closing, span, spans);
            }
        }
        ProcMacroTokenTree::Ident { text, span } | ProcMacroTokenTree::Literal { text, span } => {
            let start = output.len();
            output.push_str(text);
            push_rendered_span(start..output.len(), span, spans);
        }
        ProcMacroTokenTree::Punct { value, span, .. } => {
            push_mapped_char(output, *value, span, spans);
        }
    }
}

fn push_mapped_char(
    output: &mut String,
    value: char,
    span: &Range<usize>,
    spans: &mut Vec<RenderedTokenSpan>,
) {
    let start = output.len();
    output.push(value);
    push_rendered_span(start..output.len(), span, spans);
}

fn push_rendered_span(
    generated: Range<usize>,
    original: &Range<usize>,
    spans: &mut Vec<RenderedTokenSpan>,
) {
    if !generated.is_empty() {
        spans.push(RenderedTokenSpan {
            generated,
            original: original.clone(),
        });
    }
}

fn encode_stream(stream: &ProcMacroTokenStream, output: &mut String) {
    write!(output, "S{};", stream.trees.len()).expect("writing to String cannot fail");
    for tree in &stream.trees {
        match tree {
            ProcMacroTokenTree::Group {
                delimiter,
                stream,
                span,
            } => {
                write!(
                    output,
                    "G{};{};{};",
                    encode_delimiter(*delimiter),
                    span.start,
                    span.end
                )
                .expect("writing to String cannot fail");
                encode_stream(stream, output);
            }
            ProcMacroTokenTree::Ident { text, span } => {
                encode_text_token('I', text, span, output);
            }
            ProcMacroTokenTree::Punct {
                value,
                spacing,
                span,
            } => {
                write!(
                    output,
                    "P{};{};{};{};",
                    span.start,
                    span.end,
                    encode_spacing(*spacing),
                    *value as u32
                )
                .expect("writing to String cannot fail");
            }
            ProcMacroTokenTree::Literal { text, span } => {
                encode_text_token('L', text, span, output);
            }
        }
    }
}

fn encode_text_token(kind: char, text: &str, span: &Range<usize>, output: &mut String) {
    write!(output, "{kind}{};{};{};", span.start, span.end, text.len())
        .expect("writing to String cannot fail");
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in text.bytes() {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
}

fn encode_delimiter(delimiter: ProcMacroDelimiter) -> u8 {
    match delimiter {
        ProcMacroDelimiter::Parenthesis => 0,
        ProcMacroDelimiter::Brace => 1,
        ProcMacroDelimiter::Bracket => 2,
        ProcMacroDelimiter::None => 3,
    }
}

fn encode_spacing(spacing: ProcMacroSpacing) -> u8 {
    match spacing {
        ProcMacroSpacing::Alone => 0,
        ProcMacroSpacing::Joint => 1,
    }
}

struct TokenWireParser<'a> {
    input: &'a [u8],
    index: usize,
}

impl<'a> TokenWireParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            index: 0,
        }
    }

    fn parse_stream(&mut self) -> Result<ProcMacroTokenStream, String> {
        self.expect(b'S')?;
        let count = self.read_usize()?;
        if count > self.input.len().saturating_sub(self.index) {
            return Err("invalid proc-macro token count".into());
        }
        let mut trees = Vec::with_capacity(count);
        for _ in 0..count {
            trees.push(self.parse_tree()?);
        }
        Ok(ProcMacroTokenStream { trees })
    }

    fn parse_tree(&mut self) -> Result<ProcMacroTokenTree, String> {
        let kind = self.take()?;
        match kind {
            b'G' => {
                let delimiter = match self.read_usize()? {
                    0 => ProcMacroDelimiter::Parenthesis,
                    1 => ProcMacroDelimiter::Brace,
                    2 => ProcMacroDelimiter::Bracket,
                    3 => ProcMacroDelimiter::None,
                    _ => return Err("invalid proc-macro delimiter".into()),
                };
                let span = self.read_span()?;
                let stream = self.parse_stream()?;
                Ok(ProcMacroTokenTree::Group {
                    delimiter,
                    stream,
                    span,
                })
            }
            b'I' | b'L' => {
                let span = self.read_span()?;
                let text = self.read_text()?;
                if kind == b'I' {
                    Ok(ProcMacroTokenTree::Ident { text, span })
                } else {
                    Ok(ProcMacroTokenTree::Literal { text, span })
                }
            }
            b'P' => {
                let span = self.read_span()?;
                let spacing = match self.read_usize()? {
                    0 => ProcMacroSpacing::Alone,
                    1 => ProcMacroSpacing::Joint,
                    _ => return Err("invalid proc-macro punctuation spacing".into()),
                };
                let value = u32::try_from(self.read_usize()?)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| "invalid proc-macro punctuation".to_string())?;
                Ok(ProcMacroTokenTree::Punct {
                    value,
                    spacing,
                    span,
                })
            }
            _ => Err("invalid proc-macro token tag".into()),
        }
    }

    fn read_span(&mut self) -> Result<Range<usize>, String> {
        let start = self.read_usize()?;
        let end = self.read_usize()?;
        if start > end {
            return Err("invalid proc-macro span".into());
        }
        Ok(start..end)
    }

    fn read_text(&mut self) -> Result<String, String> {
        let len = self.read_usize()?;
        let encoded_len = len
            .checked_mul(2)
            .ok_or_else(|| "proc-macro token text is too large".to_string())?;
        let end = self
            .index
            .checked_add(encoded_len)
            .filter(|end| *end <= self.input.len())
            .ok_or_else(|| "truncated proc-macro token text".to_string())?;
        let mut bytes = Vec::with_capacity(len);
        while self.index < end {
            let high = decode_hex(self.input[self.index])?;
            let low = decode_hex(self.input[self.index + 1])?;
            bytes.push((high << 4) | low);
            self.index += 2;
        }
        String::from_utf8(bytes).map_err(|_| "proc-macro token text is not UTF-8".into())
    }

    fn read_usize(&mut self) -> Result<usize, String> {
        let mut value = 0usize;
        let mut saw_digit = false;
        loop {
            let byte = self.take()?;
            if byte == b';' {
                return saw_digit
                    .then_some(value)
                    .ok_or_else(|| "empty numeric proc-macro field".into());
            }
            if !byte.is_ascii_digit() {
                return Err("invalid numeric proc-macro field".into());
            }
            saw_digit = true;
            value = value
                .checked_mul(10)
                .and_then(|value| value.checked_add((byte - b'0') as usize))
                .ok_or_else(|| "numeric proc-macro field overflow".to_string())?;
        }
    }

    fn expect_bytes(&mut self, expected: &[u8]) -> Result<(), String> {
        for byte in expected {
            self.expect(*byte)?;
        }
        Ok(())
    }

    fn expect(&mut self, expected: u8) -> Result<(), String> {
        if self.take()? == expected {
            Ok(())
        } else {
            Err("invalid proc-macro token stream header".into())
        }
    }

    fn take(&mut self) -> Result<u8, String> {
        let byte = self
            .input
            .get(self.index)
            .copied()
            .ok_or_else(|| "truncated proc-macro token stream".to_string())?;
        self.index += 1;
        Ok(byte)
    }
}

fn decode_hex(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err("invalid hexadecimal proc-macro token text".into()),
    }
}

#[derive(Debug, Clone)]
pub struct ProcMacroDiagnostic {
    pub severity: Severity,
    pub message: String,
    pub span: Range<usize>,
}

#[derive(Debug, Clone)]
pub struct ProcMacroExpansion {
    pub output: ProcMacroTokenStream,
    pub diagnostics: Vec<ProcMacroDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcMacroKind {
    Derive,
    Attribute,
    FunctionLike,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcMacroExport {
    pub name: String,
    pub kind: ProcMacroKind,
    pub helper_attributes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcMacroDefinition {
    pub path: PathBuf,
    pub source: String,
    pub range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcMacroOccurrence {
    pub name: String,
    pub package: String,
    pub macro_name: String,
    pub kind: ProcMacroKind,
    pub range: Range<usize>,
    pub binding: Option<Range<usize>>,
    pub definition: Option<ProcMacroDefinition>,
    pub is_declaration: bool,
}

pub trait ProcMacroProvider {
    fn exports(&self, _package: &str) -> Option<Vec<ProcMacroExport>> {
        None
    }

    fn definition(&self, _package: &str, _macro_name: &str) -> Option<ProcMacroDefinition> {
        None
    }

    fn expand(
        &mut self,
        package: &str,
        macro_name: &str,
        kind: ProcMacroKind,
        input: &ProcMacroTokenStream,
        second_input: Option<&ProcMacroTokenStream>,
        call_site: Range<usize>,
    ) -> Result<ProcMacroExpansion, String>;
}

struct StandardMacroProvider;

impl ProcMacroProvider for StandardMacroProvider {
    fn expand(
        &mut self,
        _package: &str,
        _macro_name: &str,
        _kind: ProcMacroKind,
        _input: &ProcMacroTokenStream,
        _second_input: Option<&ProcMacroTokenStream>,
        _call_site: Range<usize>,
    ) -> Result<ProcMacroExpansion, String> {
        Err("external process macros require a package provider".into())
    }
}

#[derive(Debug, Clone)]
pub struct GeneratedInsertion {
    pub at: usize,
    pub text: String,
    pub call_site: Range<usize>,
    pub(crate) spans: Vec<GeneratedSpanMapping>,
}

#[derive(Debug, Clone)]
pub(crate) struct GeneratedSpanMapping {
    pub generated: Range<usize>,
    pub original: Range<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct ExpandedSource {
    pub source: String,
    pub parse: Option<Parse>,
    pub mappings: Vec<ExpandedTokenMapping>,
    pub insertions: Vec<GeneratedInsertion>,
    pub macro_occurrences: Vec<ProcMacroOccurrence>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone)]
pub struct ExpandedTokenMapping {
    pub generated: Range<usize>,
    pub original: Range<usize>,
    pub synthetic: bool,
}

#[derive(Debug, Clone)]
struct DocumentToken {
    kind: SyntaxKind,
    text: String,
    original: Range<usize>,
    depth: usize,
    generated: bool,
}

struct ParsedDocument {
    source: String,
    parse: Parse,
    ranges: Vec<Range<usize>>,
}

#[derive(Clone)]
struct TokenDocument {
    tokens: Vec<DocumentToken>,
}

impl TokenDocument {
    fn from_source(source: &str) -> Self {
        Self {
            tokens: lexer::lex(source)
                .into_iter()
                .map(|token| DocumentToken {
                    kind: token.kind,
                    text: token.text(source).into(),
                    original: token.span,
                    depth: 0,
                    generated: false,
                })
                .collect(),
        }
    }

    fn parse(&self) -> ParsedDocument {
        let mut source = String::new();
        let mut ranges = Vec::with_capacity(self.tokens.len());
        let mut tokens = Vec::with_capacity(self.tokens.len());
        for token in &self.tokens {
            let start = source.len();
            source.push_str(&token.text);
            let span = start..source.len();
            ranges.push(span.clone());
            tokens.push(lexer::Token {
                kind: token.kind,
                span,
            });
        }
        ParsedDocument {
            parse: parse_tokens(&source, tokens),
            source,
            ranges,
        }
    }

    fn replace(
        &mut self,
        parsed: &ParsedDocument,
        range: Range<usize>,
        replacement: Vec<DocumentToken>,
    ) {
        let start = parsed
            .ranges
            .iter()
            .position(|token| token.end > range.start)
            .unwrap_or(self.tokens.len());
        let end = parsed
            .ranges
            .iter()
            .position(|token| token.start >= range.end)
            .unwrap_or(self.tokens.len());
        self.tokens.splice(start..end, replacement);
    }

    fn tokens_in(&self, parsed: &ParsedDocument, range: Range<usize>) -> Vec<DocumentToken> {
        self.tokens
            .iter()
            .zip(&parsed.ranges)
            .filter(|(_, token)| range.start <= token.start && token.end <= range.end)
            .map(|(token, _)| token.clone())
            .collect()
    }

    fn tokens_in_without(
        &self,
        parsed: &ParsedDocument,
        range: Range<usize>,
        excluded: &[Range<usize>],
    ) -> Vec<DocumentToken> {
        self.tokens
            .iter()
            .zip(&parsed.ranges)
            .filter(|(_, token)| range.start <= token.start && token.end <= range.end)
            .filter(|(_, token)| {
                !excluded
                    .iter()
                    .any(|range| range.start <= token.start && token.end <= range.end)
            })
            .map(|(token, _)| token.clone())
            .collect()
    }

    fn tokens_in_replacing(
        &self,
        parsed: &ParsedDocument,
        range: Range<usize>,
        replaced: Range<usize>,
        replacement: DocumentToken,
    ) -> Vec<DocumentToken> {
        let mut inserted = false;
        let mut tokens = Vec::new();
        for (token, token_range) in self.tokens.iter().zip(&parsed.ranges) {
            if token_range.start < range.start || range.end < token_range.end {
                continue;
            }
            if replaced.start <= token_range.start && token_range.end <= replaced.end {
                if !inserted {
                    tokens.push(replacement.clone());
                    inserted = true;
                }
            } else {
                tokens.push(token.clone());
            }
        }
        if !inserted {
            tokens.push(replacement);
        }
        tokens
    }

    fn depth_in(&self, parsed: &ParsedDocument, range: Range<usize>) -> usize {
        self.tokens
            .iter()
            .zip(&parsed.ranges)
            .filter(|(_, token)| range.start <= token.start && token.end <= range.end)
            .map(|(token, _)| token.depth)
            .max()
            .unwrap_or(0)
    }

    fn finish(self) -> ExpandedDocument {
        let parsed = self.parse();
        let mappings = parsed
            .ranges
            .iter()
            .cloned()
            .zip(&self.tokens)
            .map(|(generated, token)| ExpandedTokenMapping {
                synthetic: token.generated || generated.len() != token.original.len(),
                generated,
                original: token.original.clone(),
            })
            .collect();
        ExpandedDocument {
            source: parsed.source,
            parse: parsed.parse,
            mappings,
        }
    }
}

struct ExpandedDocument {
    source: String,
    parse: Parse,
    mappings: Vec<ExpandedTokenMapping>,
}

fn token_stream_from_document(tokens: &[DocumentToken]) -> Result<ProcMacroTokenStream, String> {
    let mut index = 0usize;
    let (stream, closed) = parse_document_stream(tokens, &mut index, None)?;
    if closed.is_some() || index != tokens.len() {
        return Err("unexpected closing delimiter in process macro input".into());
    }
    Ok(stream)
}

fn parse_document_stream(
    tokens: &[DocumentToken],
    index: &mut usize,
    closing: Option<SyntaxKind>,
) -> Result<(ProcMacroTokenStream, Option<Range<usize>>), String> {
    let mut stream = ProcMacroTokenStream::default();
    while let Some(token) = tokens.get(*index) {
        if Some(token.kind) == closing {
            *index += 1;
            return Ok((stream, Some(token.original.clone())));
        }
        if token.kind.is_trivia() {
            *index += 1;
            continue;
        }
        if is_closing_delimiter(token.kind) {
            return Err("unexpected closing delimiter in process macro input".into());
        }
        if let Some((delimiter, expected)) = opening_delimiter(token.kind) {
            let opening = token.original.clone();
            *index += 1;
            let (inner, closing) = parse_document_stream(tokens, index, Some(expected))?;
            let Some(closing) = closing else {
                return Err("unclosed delimiter in process macro input".into());
            };
            stream.trees.push(ProcMacroTokenTree::Group {
                delimiter,
                stream: inner,
                span: opening.start.min(closing.start)..opening.end.max(closing.end),
            });
            continue;
        }
        if matches!(
            token.kind,
            SyntaxKind::String | SyntaxKind::Char | SyntaxKind::Float | SyntaxKind::Number
        ) {
            stream.trees.push(ProcMacroTokenTree::Literal {
                text: token.text.clone(),
                span: token.original.clone(),
            });
            *index += 1;
            continue;
        }
        if token
            .text
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        {
            stream.trees.push(ProcMacroTokenTree::Ident {
                text: token.text.clone(),
                span: token.original.clone(),
            });
            *index += 1;
            continue;
        }

        let next_is_punct = tokens
            .get(*index + 1)
            .is_some_and(|next| !next.kind.is_trivia() && token_text_is_punctuation(&next.text));
        let chars = token.text.char_indices().collect::<Vec<_>>();
        for (char_index, (offset, value)) in chars.iter().enumerate() {
            if !value.is_ascii() {
                return Err("process macro punctuation must be ASCII".into());
            }
            let end = chars
                .get(char_index + 1)
                .map(|(offset, _)| *offset)
                .unwrap_or(token.text.len());
            let span =
                if token.original.end.saturating_sub(token.original.start) == token.text.len() {
                    token.original.start + *offset..token.original.start + end
                } else {
                    token.original.clone()
                };
            stream.trees.push(ProcMacroTokenTree::Punct {
                value: *value,
                spacing: if char_index + 1 < chars.len() || next_is_punct {
                    ProcMacroSpacing::Joint
                } else {
                    ProcMacroSpacing::Alone
                },
                span,
            });
        }
        *index += 1;
    }
    if closing.is_some() {
        Err("unclosed delimiter in process macro input".into())
    } else {
        Ok((stream, None))
    }
}

fn token_text_is_punctuation(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_punctuation())
}

fn document_tokens_from_output(
    stream: &ProcMacroTokenStream,
    call_site: &Range<usize>,
    source_len: usize,
    depth: usize,
) -> Result<Vec<DocumentToken>, String> {
    let mut output = Vec::new();
    flatten_output_stream(stream, call_site, source_len, depth, &mut output)?;
    if output.last().is_some_and(|token| token.kind.is_trivia()) {
        output.pop();
    }
    Ok(output)
}

fn flatten_output_stream(
    stream: &ProcMacroTokenStream,
    call_site: &Range<usize>,
    source_len: usize,
    depth: usize,
    output: &mut Vec<DocumentToken>,
) -> Result<(), String> {
    let mut index = 0usize;
    while index < stream.trees.len() {
        match &stream.trees[index] {
            ProcMacroTokenTree::Group {
                delimiter,
                stream,
                span,
            } => {
                let span = checked_output_span(span, call_site, source_len);
                let delimiters = match delimiter {
                    ProcMacroDelimiter::Parenthesis => {
                        Some((SyntaxKind::LParen, "(", SyntaxKind::RParen, ")"))
                    }
                    ProcMacroDelimiter::Brace => {
                        Some((SyntaxKind::LBrace, "{", SyntaxKind::RBrace, "}"))
                    }
                    ProcMacroDelimiter::Bracket => {
                        Some((SyntaxKind::LBracket, "[", SyntaxKind::RBracket, "]"))
                    }
                    ProcMacroDelimiter::None => None,
                };
                if let Some((opening_kind, opening, _, _)) = delimiters {
                    output.push(DocumentToken {
                        kind: opening_kind,
                        text: opening.into(),
                        original: span.clone(),
                        depth,
                        generated: true,
                    });
                }
                flatten_output_stream(stream, call_site, source_len, depth, output)?;
                if output.last().is_some_and(|token| token.kind.is_trivia()) {
                    output.pop();
                }
                if let Some((_, _, closing_kind, closing)) = delimiters {
                    output.push(DocumentToken {
                        kind: closing_kind,
                        text: closing.into(),
                        original: span,
                        depth,
                        generated: true,
                    });
                }
            }
            ProcMacroTokenTree::Ident { text, span } => {
                let kind = classify_word(text)
                    .ok_or_else(|| format!("invalid process macro identifier `{text}`"))?;
                output.push(DocumentToken {
                    kind,
                    text: text.clone(),
                    original: checked_output_span(span, call_site, source_len),
                    depth,
                    generated: true,
                });
            }
            ProcMacroTokenTree::Literal { text, span } => {
                let kind = classify_literal(text)
                    .ok_or_else(|| format!("invalid process macro literal `{text}`"))?;
                output.push(DocumentToken {
                    kind,
                    text: text.clone(),
                    original: checked_output_span(span, call_site, source_len),
                    depth,
                    generated: true,
                });
            }
            ProcMacroTokenTree::Punct { .. } => {
                let start = index;
                let mut source = String::new();
                loop {
                    let ProcMacroTokenTree::Punct { value, spacing, .. } = &stream.trees[index]
                    else {
                        unreachable!();
                    };
                    source.push(*value);
                    index += 1;
                    if *spacing == ProcMacroSpacing::Alone
                        || !matches!(
                            stream.trees.get(index),
                            Some(ProcMacroTokenTree::Punct { .. })
                        )
                    {
                        break;
                    }
                }
                let punct_tokens = lexer::lex(&source);
                if punct_tokens.is_empty()
                    || punct_tokens
                        .iter()
                        .any(|token| token.kind == SyntaxKind::ErrorNode)
                {
                    return Err(format!("invalid process macro punctuation `{source}`"));
                }
                let span = checked_output_span(stream.trees[start].span(), call_site, source_len);
                for token in punct_tokens {
                    output.push(DocumentToken {
                        kind: token.kind,
                        text: token.text(&source).into(),
                        original: span.clone(),
                        depth,
                        generated: true,
                    });
                }
                output.push(DocumentToken {
                    kind: SyntaxKind::Whitespace,
                    text: " ".into(),
                    original: call_site.clone(),
                    depth,
                    generated: true,
                });
                continue;
            }
        }
        index += 1;
        output.push(DocumentToken {
            kind: SyntaxKind::Whitespace,
            text: " ".into(),
            original: call_site.clone(),
            depth,
            generated: true,
        });
    }
    Ok(())
}

fn checked_output_span(
    span: &Range<usize>,
    call_site: &Range<usize>,
    source_len: usize,
) -> Range<usize> {
    if valid_span(span, source_len) {
        span.clone()
    } else {
        call_site.clone()
    }
}

fn classify_word(text: &str) -> Option<SyntaxKind> {
    let tokens = lexer::lex(text);
    let token = tokens.as_slice().first()?;
    (tokens.len() == 1
        && token.span == (0..text.len())
        && token.kind != SyntaxKind::ErrorNode
        && !matches!(
            token.kind,
            SyntaxKind::String | SyntaxKind::Char | SyntaxKind::Float | SyntaxKind::Number
        ))
    .then_some(token.kind)
}

fn classify_literal(text: &str) -> Option<SyntaxKind> {
    let tokens = lexer::lex(text);
    let token = tokens.as_slice().first()?;
    (tokens.len() == 1
        && token.span == (0..text.len())
        && matches!(
            token.kind,
            SyntaxKind::String | SyntaxKind::Char | SyntaxKind::Float | SyntaxKind::Number
        ))
    .then_some(token.kind)
}

#[derive(Debug, Clone)]
struct ImportedMacro {
    package: String,
    macro_name: String,
    kind: ProcMacroKind,
    helper_attributes: Vec<String>,
    binding: Option<Range<usize>>,
    definition: Option<ProcMacroDefinition>,
}

type MacroScope = HashMap<String, ImportedMacro>;

#[derive(Debug, Clone)]
struct ScopedStatement {
    statement: ast::Stmt,
    macros: MacroScope,
}

#[derive(Debug, Clone)]
enum DeriveMacroPath {
    Imported(String),
    Qualified { package: String, macro_name: String },
}

#[derive(Debug, Clone)]
struct UseBinding {
    path: Vec<String>,
    alias: Option<String>,
    glob: bool,
    local_range: Option<Range<usize>>,
}

enum ProcMacroUse {
    Ordinary,
    Imports {
        imports: Vec<(String, ImportedMacro)>,
        replacement: Option<String>,
    },
    Invalid {
        message: String,
        replacement: Option<String>,
    },
}

type MacroReexports = HashMap<Vec<String>, MacroScope>;

#[derive(Clone)]
enum ExpansionAction {
    Derive {
        item: SyntaxNode,
        attribute: ast::Attribute,
        macros: MacroScope,
    },
    Attribute {
        item: SyntaxNode,
        attribute: ast::Attribute,
        macros: MacroScope,
    },
    FunctionLike {
        call: ast::MacroCall,
        macros: MacroScope,
    },
}

pub fn expand_source(source: &str, provider: &mut dyn ProcMacroProvider) -> ExpandedSource {
    let mut document = TokenDocument::from_source(source);
    let mut diagnostics = Vec::new();
    let initial = document.parse();
    let macro_occurrences = if initial.parse.errors.is_empty() {
        collect_macro_occurrences(&initial, provider)
    } else {
        Vec::new()
    };

    loop {
        let parsed = document.parse();
        if !parsed.parse.errors.is_empty() {
            let expanded = document.finish();
            return ExpandedSource {
                source: expanded.source,
                parse: Some(expanded.parse),
                mappings: expanded.mappings,
                macro_occurrences,
                diagnostics,
                ..ExpandedSource::default()
            };
        }
        let Some(action) = next_expansion_action(&parsed, provider) else {
            collect_macro_import_diagnostics(&parsed, provider, &mut diagnostics);
            erase_macro_imports(&mut document, &parsed, provider);
            break;
        };
        let action_range = action.range();
        let depth = document.depth_in(&parsed, action_range);
        if depth >= MAX_DERIVE_EXPANSION_DEPTH {
            diagnostics.push(diagnostic(
                action.call_site(&document, &parsed),
                format!(
                    "process macro expansion exceeded the maximum depth of {MAX_DERIVE_EXPANSION_DEPTH}"
                ),
                Severity::Error,
            ));
            if !consume_failed_action(&mut document, &parsed, &action) {
                break;
            }
            continue;
        }

        let progressed = match action {
            ExpansionAction::Derive {
                item,
                attribute,
                macros,
            } => expand_derive_action(
                &mut document,
                &parsed,
                item,
                attribute,
                &macros,
                provider,
                source.len(),
                depth + 1,
                &mut diagnostics,
            ),
            ExpansionAction::Attribute {
                item,
                attribute,
                macros,
            } => expand_attribute_action(
                &mut document,
                &parsed,
                item,
                attribute,
                &macros,
                provider,
                source.len(),
                depth + 1,
                &mut diagnostics,
            ),
            ExpansionAction::FunctionLike { call, macros } => expand_function_action(
                &mut document,
                &parsed,
                call,
                &macros,
                provider,
                source.len(),
                depth + 1,
                &mut diagnostics,
            ),
        };
        if !progressed {
            break;
        }
    }

    let expanded = document.finish();
    ExpandedSource {
        source: expanded.source,
        parse: Some(expanded.parse),
        mappings: expanded.mappings,
        insertions: Vec::new(),
        macro_occurrences,
        diagnostics,
    }
}

pub fn expand_standard_macros(source: &str) -> ExpandedSource {
    expand_source(source, &mut StandardMacroProvider)
}

impl ExpansionAction {
    fn range(&self) -> Range<usize> {
        match self {
            Self::Derive { attribute, .. } | Self::Attribute { attribute, .. } => {
                range(attribute.syntax().text_range())
            }
            Self::FunctionLike { call, .. } => range(call.syntax().text_range()),
        }
    }

    fn call_site(&self, document: &TokenDocument, parsed: &ParsedDocument) -> Range<usize> {
        document.origin_in(parsed, self.range())
    }
}

impl TokenDocument {
    fn origin_in(&self, parsed: &ParsedDocument, range: Range<usize>) -> Range<usize> {
        let mut origins = self
            .tokens
            .iter()
            .zip(&parsed.ranges)
            .filter(|(_, token)| range.start <= token.start && token.end <= range.end)
            .map(|(token, _)| token.original.clone());
        let Some(first) = origins.next() else {
            return range;
        };
        origins.fold(first, |combined, next| {
            combined.start.min(next.start)..combined.end.max(next.end)
        })
    }
}

fn next_expansion_action(
    parsed: &ParsedDocument,
    provider: &dyn ProcMacroProvider,
) -> Option<ExpansionAction> {
    let syntax = parsed.parse.syntax();
    let scopes = macro_scopes(&syntax, provider);
    let mut actions = Vec::new();

    for item in syntax
        .descendants()
        .filter(|node| is_attribute_item(node.kind()))
    {
        let macros = scope_for_range(&scopes, range(item.text_range()));
        for attribute in ast::attrs_for_node(&item) {
            if attribute.name().is_some_and(|name| name.text() == "derive") {
                actions.push(ExpansionAction::Derive {
                    item: item.clone(),
                    attribute,
                    macros: macros.clone(),
                });
                break;
            }
            let Ok((path, _)) = parse_attribute_invocation(&attribute.raw_text()) else {
                continue;
            };
            if is_macro_candidate(&path, ProcMacroKind::Attribute, &macros, provider) {
                actions.push(ExpansionAction::Attribute {
                    item: item.clone(),
                    attribute,
                    macros: macros.clone(),
                });
                break;
            }
        }
    }

    for node in syntax
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::MacroCall)
    {
        let Some(call) = ast::MacroCall::cast(node) else {
            continue;
        };
        let macros = scope_for_range(&scopes, range(call.syntax().text_range()));
        actions.push(ExpansionAction::FunctionLike { call, macros });
    }

    actions
        .into_iter()
        .min_by_key(|action| action.range().start)
}

fn is_attribute_item(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::FuncDecl
            | SyntaxKind::StructDecl
            | SyntaxKind::EnumDecl
            | SyntaxKind::ModDecl
            | SyntaxKind::UseDecl
            | SyntaxKind::TraitDecl
            | SyntaxKind::ImplDecl
            | SyntaxKind::ConstDecl
            | SyntaxKind::TypeAliasDecl
            | SyntaxKind::ExternBlock
            | SyntaxKind::ExternFnDecl
    )
}

fn macro_scopes(syntax: &SyntaxNode, provider: &dyn ProcMacroProvider) -> Vec<ScopedStatement> {
    let Some(root) = ast::Root::cast(syntax.clone()) else {
        return Vec::new();
    };
    let reexports = build_macro_reexports(&root, provider);
    let mut scopes = Vec::new();
    collect_scoped_statements(
        root.stmts().collect(),
        &MacroScope::default(),
        provider,
        Some(&reexports),
        &[],
        &mut scopes,
        &mut Vec::new(),
        &mut Vec::new(),
    );
    scopes
}

fn collect_macro_occurrences(
    parsed: &ParsedDocument,
    provider: &dyn ProcMacroProvider,
) -> Vec<ProcMacroOccurrence> {
    let syntax = parsed.parse.syntax();
    let scopes = macro_scopes(&syntax, provider);
    let mut occurrences = Vec::new();
    let mut declarations = HashSet::new();

    for scoped in &scopes {
        for (name, imported) in &scoped.macros {
            let Some(binding) = imported.binding.clone() else {
                continue;
            };
            if declarations.insert((binding.start, binding.end)) {
                occurrences.push(macro_occurrence(name, imported, binding, true));
            }
        }
    }

    for item in syntax
        .descendants()
        .filter(|node| is_attribute_item(node.kind()))
    {
        let macros = scope_for_range(&scopes, range(item.text_range()));
        for attribute in ast::attrs_for_node(&item) {
            let attribute_start = usize::from(attribute.syntax().text_range().start());
            if attribute.name().is_some_and(|name| name.text() == "derive") {
                let Ok(invocations) = parse_derive_invocations(&attribute.raw_text()) else {
                    continue;
                };
                for (path, name_range) in invocations {
                    let path = match path {
                        DeriveMacroPath::Imported(name) => vec![name],
                        DeriveMacroPath::Qualified {
                            package,
                            macro_name,
                        } => vec![package, macro_name],
                    };
                    let Ok(imported) =
                        resolve_macro(&path, ProcMacroKind::Derive, &macros, provider)
                    else {
                        continue;
                    };
                    let name_range =
                        name_range.start + attribute_start..name_range.end + attribute_start;
                    let name = parsed.source[name_range.clone()].to_string();
                    occurrences.push(macro_occurrence(&name, &imported, name_range, false));
                }
                continue;
            }

            let Ok((path, name_range, _)) =
                parse_attribute_invocation_spanned(&attribute.raw_text())
            else {
                continue;
            };
            if !is_macro_candidate(&path, ProcMacroKind::Attribute, &macros, provider) {
                continue;
            }
            let Ok(imported) = resolve_macro(&path, ProcMacroKind::Attribute, &macros, provider)
            else {
                continue;
            };
            let name_range = name_range.start + attribute_start..name_range.end + attribute_start;
            let name = parsed.source[name_range.clone()].to_string();
            occurrences.push(macro_occurrence(&name, &imported, name_range, false));
        }
    }

    for node in syntax
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::MacroCall)
    {
        let Some(call) = ast::MacroCall::cast(node) else {
            continue;
        };
        let macros = scope_for_range(&scopes, range(call.syntax().text_range()));
        let Some(path) = call.path() else {
            continue;
        };
        let segments = path
            .segments()
            .filter_map(|segment| segment.name_token())
            .collect::<Vec<_>>();
        let names = segments
            .iter()
            .map(|token| token.text().to_string())
            .collect::<Vec<_>>();
        let Some(name) = segments.last() else {
            continue;
        };
        let Ok(imported) = resolve_macro(&names, ProcMacroKind::FunctionLike, &macros, provider)
        else {
            continue;
        };
        occurrences.push(macro_occurrence(
            name.text(),
            &imported,
            range(name.text_range()),
            false,
        ));
    }

    occurrences.sort_by_key(|occurrence| {
        (
            occurrence.range.start,
            occurrence.range.end,
            !occurrence.is_declaration,
        )
    });
    occurrences.dedup_by(|left, right| {
        left.range == right.range
            && left.kind == right.kind
            && left.is_declaration == right.is_declaration
    });
    occurrences
}

fn macro_occurrence(
    name: &str,
    imported: &ImportedMacro,
    range: Range<usize>,
    is_declaration: bool,
) -> ProcMacroOccurrence {
    ProcMacroOccurrence {
        name: name.into(),
        package: imported.package.clone(),
        macro_name: imported.macro_name.clone(),
        kind: imported.kind,
        range,
        binding: imported.binding.clone(),
        definition: imported.definition.clone(),
        is_declaration,
    }
}

fn scope_for_range(scopes: &[ScopedStatement], target: Range<usize>) -> MacroScope {
    scopes
        .iter()
        .filter(|scoped| {
            let item = range(scoped.statement.syntax().text_range());
            item.start <= target.start && target.end <= item.end
        })
        .min_by_key(|scoped| {
            let item = range(scoped.statement.syntax().text_range());
            item.end - item.start
        })
        .map(|scoped| scoped.macros.clone())
        .unwrap_or_default()
}

fn is_macro_candidate(
    path: &[String],
    kind: ProcMacroKind,
    macros: &MacroScope,
    provider: &dyn ProcMacroProvider,
) -> bool {
    match path {
        [name] => macros
            .get(name)
            .is_some_and(|imported| imported.kind == kind),
        [package, name] => provider.exports(package).is_some_and(|exports| {
            exports
                .iter()
                .any(|export| export.name == *name && export.kind == kind)
        }),
        _ => false,
    }
}

fn resolve_macro(
    path: &[String],
    expected: ProcMacroKind,
    macros: &MacroScope,
    provider: &dyn ProcMacroProvider,
) -> Result<ImportedMacro, String> {
    let imported = match path {
        [name] => macros
            .get(name)
            .cloned()
            .or_else(|| standard_macro(name, expected))
            .ok_or_else(|| {
                if expected == ProcMacroKind::Derive {
                    format!(
                        "cannot find derive macro `{name}` in this scope; import it with `use package::{name};`"
                    )
                } else {
                    format!("cannot find macro `{name}` in this scope")
                }
            })?,
        [package, name] if package == STANDARD_MACRO_PACKAGE => standard_macro(name, expected)
            .ok_or_else(|| format!("unknown standard macro `{package}::{name}`"))?,
        [package, name] => {
            if let Some(exports) = provider.exports(package) {
                let export = exports
                    .into_iter()
                    .find(|export| export.name == *name)
                    .ok_or_else(|| match expected {
                        ProcMacroKind::Derive => {
                            format!("unknown proc-macro derive `{package}::{name}`")
                        }
                        ProcMacroKind::Attribute => {
                            format!("unknown proc-macro attribute `{package}::{name}`")
                        }
                        ProcMacroKind::FunctionLike => {
                            format!("unknown function-like proc macro `{package}::{name}`")
                        }
                    })?;
                ImportedMacro {
                    package: package.clone(),
                    macro_name: name.clone(),
                    kind: export.kind,
                    helper_attributes: export.helper_attributes,
                    binding: None,
                    definition: provider.definition(package, name),
                }
            } else {
                ImportedMacro {
                    package: package.clone(),
                    macro_name: name.clone(),
                    kind: expected,
                    helper_attributes: Vec::new(),
                    binding: None,
                    definition: provider.definition(package, name),
                }
            }
        }
        _ => return Err("process macro paths must contain one or two segments".into()),
    };
    if imported.kind != expected {
        return Err(format!(
            "{} is a {:?} macro, not a {:?} macro",
            path.join("::"),
            imported.kind,
            expected
        ));
    }
    Ok(imported)
}

fn standard_macro(name: &str, kind: ProcMacroKind) -> Option<ImportedMacro> {
    ((kind == ProcMacroKind::FunctionLike && STANDARD_FUNCTION_MACROS.contains(&name))
        || (kind == ProcMacroKind::Derive && STANDARD_DERIVE_MACROS.contains(&name)))
    .then(|| ImportedMacro {
        package: STANDARD_MACRO_PACKAGE.into(),
        macro_name: name.into(),
        kind,
        helper_attributes: Vec::new(),
        binding: None,
        definition: None,
    })
}

fn macro_call_input(tokens: &[DocumentToken]) -> Result<ProcMacroTokenStream, String> {
    let stream = token_stream_from_document(tokens)?;
    stream
        .trees
        .into_iter()
        .rev()
        .find_map(|tree| match tree {
            ProcMacroTokenTree::Group { stream, .. } => Some(stream),
            _ => None,
        })
        .ok_or_else(|| "function-like macro call has no delimited input".into())
}

fn parse_attribute_invocation(raw: &str) -> Result<(Vec<String>, ProcMacroTokenStream), String> {
    parse_attribute_invocation_spanned(raw).map(|(path, _, args)| (path, args))
}

fn parse_attribute_invocation_spanned(
    raw: &str,
) -> Result<(Vec<String>, Range<usize>, ProcMacroTokenStream), String> {
    let stream = ProcMacroTokenStream::from_source(raw, 0)?;
    let [
        ProcMacroTokenTree::Punct { value: '#', .. },
        ProcMacroTokenTree::Group {
            delimiter: ProcMacroDelimiter::Bracket,
            stream: attribute,
            ..
        },
    ] = stream.trees.as_slice()
    else {
        return Err("invalid attribute syntax".into());
    };
    let mut path = Vec::new();
    let mut name_range = None;
    let mut index = 0usize;
    while let Some(ProcMacroTokenTree::Ident { text, span }) = attribute.trees.get(index) {
        path.push(text.clone());
        name_range = Some(span.clone());
        index += 1;
        if !matches!(
            attribute.trees.get(index..index + 2),
            Some([
                ProcMacroTokenTree::Punct { value: ':', .. },
                ProcMacroTokenTree::Punct { value: ':', .. }
            ])
        ) {
            break;
        }
        index += 2;
    }
    if path.is_empty() {
        return Err("attribute has no name".into());
    }
    let args = match attribute.trees.get(index) {
        Some(ProcMacroTokenTree::Group {
            delimiter: ProcMacroDelimiter::Parenthesis,
            stream: arguments,
            ..
        }) if index + 1 == attribute.trees.len() => arguments.clone(),
        _ => ProcMacroTokenStream {
            trees: attribute.trees[index..].to_vec(),
        },
    };
    Ok((path, name_range.unwrap(), args))
}

fn item_range_with_attrs(item: &SyntaxNode, attrs: &[ast::Attribute]) -> Range<usize> {
    let item = range(item.text_range());
    attrs
        .first()
        .map(|attr| range(attr.syntax().text_range()).start..item.end)
        .unwrap_or(item)
}

fn append_macro_diagnostics(
    diagnostics: &mut Vec<Diagnostic>,
    incoming: Vec<ProcMacroDiagnostic>,
    call_site: &Range<usize>,
    source_len: usize,
) -> bool {
    let has_error = incoming
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error);
    diagnostics.extend(incoming.into_iter().map(|incoming| {
        let span = checked_output_span(&incoming.span, call_site, source_len);
        diagnostic(span, incoming.message, incoming.severity)
    }));
    has_error
}

fn replace_if_parses(
    document: &mut TokenDocument,
    parsed: &ParsedDocument,
    range: Range<usize>,
    replacement: Vec<DocumentToken>,
    call_site: &Range<usize>,
    message: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let mut candidate = document.clone();
    candidate.replace(parsed, range, replacement);
    let parse = candidate.parse();
    if parse.parse.errors.is_empty() {
        *document = candidate;
        true
    } else {
        let detail = parse
            .parse
            .errors
            .first()
            .map(|error| format!(": {}", error.message))
            .unwrap_or_default();
        diagnostics.push(diagnostic(
            call_site.clone(),
            format!("{message}{detail}"),
            Severity::Error,
        ));
        false
    }
}

fn validate_item_output(tokens: &[DocumentToken], message: &str) -> Result<(), String> {
    let parsed = TokenDocument {
        tokens: tokens.to_vec(),
    }
    .parse();
    if let Some(error) = parsed.parse.errors.first() {
        return Err(format!("{message}: {}", error.message));
    }
    let root = ast::Root::cast(parsed.parse.syntax()).expect("parsed root should be a root node");
    if root.stmts().all(|statement| {
        matches!(
            statement,
            ast::Stmt::FuncDecl(_)
                | ast::Stmt::StructDecl(_)
                | ast::Stmt::EnumDecl(_)
                | ast::Stmt::TraitDecl(_)
                | ast::Stmt::ImplDecl(_)
                | ast::Stmt::ConstDecl(_)
                | ast::Stmt::TypeAliasDecl(_)
                | ast::Stmt::ModDecl(_)
                | ast::Stmt::UseDecl(_)
                | ast::Stmt::ExternBlock(_)
                | ast::Stmt::ExternFnDecl(_)
        ) || matches!(statement, ast::Stmt::ExprStmt(ref expression)
            if expression.syntax().descendants().any(|node| node.kind() == SyntaxKind::MacroCall))
    }) {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_derive_helper_attributes(
    document: &TokenDocument,
    parsed: &ParsedDocument,
    item: &SyntaxNode,
    item_attrs: &[ast::Attribute],
    helpers: &HashSet<String>,
    macros: &MacroScope,
    provider: &dyn ProcMacroProvider,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let mut valid = true;
    for attribute in item_attrs.iter().cloned().chain(
        item.descendants()
            .filter(|node| node.kind() == SyntaxKind::Attribute)
            .filter_map(ast::Attribute::cast),
    ) {
        let Some(name) = attribute.name().map(|name| name.text().to_string()) else {
            continue;
        };
        if matches!(
            name.as_str(),
            "derive"
                | "lang"
                | "fundamental"
                | "builtin"
                | "c_export"
                | "proc_macro"
                | "proc_macro_attribute"
                | "proc_macro_derive"
        ) || helpers.contains(&name)
        {
            continue;
        }
        if parse_attribute_invocation(&attribute.raw_text())
            .ok()
            .and_then(|(path, _)| {
                resolve_macro(&path, ProcMacroKind::Attribute, macros, provider).ok()
            })
            .is_some()
        {
            continue;
        }
        let attribute_range = range(attribute.syntax().text_range());
        diagnostics.push(diagnostic(
            document.origin_in(parsed, attribute_range),
            format!(
                "cannot find attribute `{name}` in this scope; derive helper attributes must be declared with `attributes({name})`"
            ),
            Severity::Error,
        ));
        valid = false;
    }
    valid
}

fn space_token(original: Range<usize>, depth: usize) -> DocumentToken {
    DocumentToken {
        kind: SyntaxKind::Whitespace,
        text: "\n".into(),
        original,
        depth,
        generated: true,
    }
}

fn erased_token(
    document: &TokenDocument,
    parsed: &ParsedDocument,
    range: Range<usize>,
    depth: usize,
) -> DocumentToken {
    let text = parsed.source[range.clone()]
        .bytes()
        .map(|byte| match byte {
            b'\n' | b'\r' | b'\t' => byte,
            _ => b' ',
        })
        .collect::<Vec<_>>();
    DocumentToken {
        kind: SyntaxKind::Whitespace,
        text: String::from_utf8(text).expect("erased macro syntax is ASCII whitespace"),
        original: document.origin_in(parsed, range),
        depth,
        generated: true,
    }
}

fn erase_range(
    document: &mut TokenDocument,
    parsed: &ParsedDocument,
    range: Range<usize>,
    depth: usize,
) {
    let erased = erased_token(document, parsed, range.clone(), depth);
    document.replace(parsed, range, vec![erased]);
}

fn consume_failed_action(
    document: &mut TokenDocument,
    parsed: &ParsedDocument,
    action: &ExpansionAction,
) -> bool {
    match action {
        ExpansionAction::Derive { attribute, .. }
        | ExpansionAction::Attribute { attribute, .. } => {
            let range = range(attribute.syntax().text_range());
            let erased = erased_token(document, parsed, range.clone(), 0);
            document.replace(parsed, range, vec![erased]);
            true
        }
        ExpansionAction::FunctionLike { .. } => false,
    }
}

fn collect_macro_import_diagnostics(
    parsed: &ParsedDocument,
    provider: &dyn ProcMacroProvider,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(root) = ast::Root::cast(parsed.parse.syntax()) else {
        return;
    };
    let reexports = build_macro_reexports(&root, provider);
    collect_scoped_statements(
        root.stmts().collect(),
        &MacroScope::default(),
        provider,
        Some(&reexports),
        &[],
        &mut Vec::new(),
        &mut Vec::new(),
        diagnostics,
    );
}

fn erase_macro_imports(
    document: &mut TokenDocument,
    parsed: &ParsedDocument,
    provider: &dyn ProcMacroProvider,
) {
    let syntax = parsed.parse.syntax();
    let Some(root) = ast::Root::cast(syntax.clone()) else {
        return;
    };
    let reexports = build_macro_reexports(&root, provider);
    let mut edits = syntax
        .descendants()
        .filter_map(ast::UseDecl::cast)
        .filter_map(|use_decl| {
            let module_path = module_path_for_node(use_decl.syntax());
            let replacement = match parse_proc_macro_use_resolved(
                &use_decl,
                provider,
                &reexports,
                &module_path,
            ) {
                ProcMacroUse::Ordinary => return None,
                ProcMacroUse::Imports { replacement, .. }
                | ProcMacroUse::Invalid { replacement, .. } => replacement,
            };
            Some((range(use_decl.syntax().text_range()), replacement))
        })
        .collect::<Vec<_>>();
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    for (range, replacement) in edits {
        let tokens = replacement.map_or_else(
            || vec![erased_token(document, parsed, range.clone(), 0)],
            |replacement| replacement_tokens(document, parsed, range.clone(), &replacement),
        );
        document.replace(parsed, range, tokens);
    }
}

fn module_path_for_node(node: &SyntaxNode) -> Vec<String> {
    let mut path = node
        .ancestors()
        .filter_map(ast::ModDecl::cast)
        .filter_map(|module| module.name().map(|name| name.text().to_string()))
        .collect::<Vec<_>>();
    path.reverse();
    path
}

fn replacement_tokens(
    document: &TokenDocument,
    parsed: &ParsedDocument,
    range: Range<usize>,
    replacement: &str,
) -> Vec<DocumentToken> {
    let original = document.origin_in(parsed, range);
    lexer::lex(replacement)
        .into_iter()
        .map(|token| DocumentToken {
            kind: token.kind,
            text: token.text(replacement).into(),
            original: original.clone(),
            depth: 0,
            generated: true,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn expand_derive_action(
    document: &mut TokenDocument,
    parsed: &ParsedDocument,
    item: SyntaxNode,
    attribute: ast::Attribute,
    macros: &MacroScope,
    provider: &mut dyn ProcMacroProvider,
    source_len: usize,
    output_depth: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let call_range = range(attribute.syntax().text_range());
    let call_site = document.origin_in(parsed, call_range.clone());
    if !matches!(item.kind(), SyntaxKind::StructDecl | SyntaxKind::EnumDecl) {
        diagnostics.push(diagnostic(
            call_site,
            "derive macros may only be applied to structs or enums".into(),
            Severity::Error,
        ));
        erase_range(document, parsed, call_range, output_depth);
        return true;
    }

    let attrs = ast::attrs_for_node(&item);
    let full_range = item_range_with_attrs(&item, &attrs);
    let derive_ranges = attrs
        .iter()
        .filter(|attr| attr.name().is_some_and(|name| name.text() == "derive"))
        .map(|attr| range(attr.syntax().text_range()))
        .collect::<Vec<_>>();
    let input_tokens = document.tokens_in_without(parsed, full_range.clone(), &derive_ranges);
    let input = match token_stream_from_document(&input_tokens) {
        Ok(input) => input,
        Err(message) => {
            diagnostics.push(diagnostic(call_site, message, Severity::Error));
            erase_range(document, parsed, call_range, output_depth);
            return true;
        }
    };
    let paths = match parse_derive_paths(&attribute.raw_text()) {
        Ok(paths) => paths,
        Err(message) => {
            diagnostics.push(diagnostic(call_site, message, Severity::Error));
            erase_range(document, parsed, call_range, output_depth);
            return true;
        }
    };

    let imported = paths
        .into_iter()
        .filter_map(|path| {
            let path = match path {
                DeriveMacroPath::Imported(name) => vec![name],
                DeriveMacroPath::Qualified {
                    package,
                    macro_name,
                } => vec![package, macro_name],
            };
            let imported = match resolve_macro(&path, ProcMacroKind::Derive, macros, provider) {
                Ok(imported) => imported,
                Err(message) => {
                    diagnostics.push(diagnostic(call_site.clone(), message, Severity::Error));
                    return None;
                }
            };
            Some(imported)
        })
        .collect::<Vec<_>>();
    let helpers = imported
        .iter()
        .flat_map(|imported| imported.helper_attributes.iter().cloned())
        .collect::<HashSet<_>>();
    if !validate_derive_helper_attributes(
        document,
        parsed,
        &item,
        &attrs,
        &helpers,
        macros,
        provider,
        diagnostics,
    ) {
        erase_range(document, parsed, call_range, output_depth);
        return true;
    }

    let mut generated = Vec::new();
    for imported in imported {
        let expansion = if imported.package == STANDARD_MACRO_PACKAGE {
            match expand_standard_derive_macro(&imported.macro_name, &item, &call_site) {
                Ok(output) => Ok(ProcMacroExpansion {
                    output,
                    diagnostics: Vec::new(),
                }),
                Err(message) => Err(message),
            }
        } else {
            provider.expand(
                &imported.package,
                &imported.macro_name,
                ProcMacroKind::Derive,
                &input,
                None,
                call_site.clone(),
            )
        };
        match expansion {
            Ok(expansion) => {
                let has_error = append_macro_diagnostics(
                    diagnostics,
                    expansion.diagnostics,
                    &call_site,
                    source_len,
                );
                if !has_error {
                    match document_tokens_from_output(
                        &expansion.output,
                        &call_site,
                        source_len,
                        output_depth,
                    ) {
                        Ok(mut output) => {
                            if let Err(message) = validate_item_output(
                                &output,
                                "derive macro output must contain only top-level items",
                            ) {
                                diagnostics.push(diagnostic(
                                    call_site.clone(),
                                    message,
                                    Severity::Error,
                                ));
                                continue;
                            }
                            if !generated.is_empty() && !output.is_empty() {
                                generated.push(space_token(call_site.clone(), output_depth));
                            }
                            generated.append(&mut output);
                        }
                        Err(message) => diagnostics.push(diagnostic(
                            call_site.clone(),
                            message,
                            Severity::Error,
                        )),
                    }
                }
            }
            Err(message) => diagnostics.push(diagnostic(
                call_site.clone(),
                format!(
                    "failed to expand {}::{}: {message}",
                    imported.package, imported.macro_name
                ),
                Severity::Error,
            )),
        }
    }

    let mut replacement = document.tokens_in_replacing(
        parsed,
        full_range.clone(),
        call_range.clone(),
        erased_token(document, parsed, call_range, output_depth),
    );
    if !generated.is_empty() {
        replacement.push(space_token(call_site.clone(), output_depth));
        replacement.extend(generated);
    }
    replace_if_parses(
        document,
        parsed,
        full_range,
        replacement,
        &call_site,
        "derive macro output is not valid Riddle",
        diagnostics,
    )
}

#[allow(clippy::too_many_arguments)]
fn expand_attribute_action(
    document: &mut TokenDocument,
    parsed: &ParsedDocument,
    item: SyntaxNode,
    attribute: ast::Attribute,
    macros: &MacroScope,
    provider: &mut dyn ProcMacroProvider,
    source_len: usize,
    output_depth: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let attribute_range = range(attribute.syntax().text_range());
    let call_site = document.origin_in(parsed, attribute_range.clone());
    let (path, args) = match parse_attribute_invocation(&attribute.raw_text()) {
        Ok(parsed) => parsed,
        Err(message) => {
            diagnostics.push(diagnostic(call_site, message, Severity::Error));
            erase_range(document, parsed, attribute_range, output_depth);
            return true;
        }
    };
    let imported = match resolve_macro(&path, ProcMacroKind::Attribute, macros, provider) {
        Ok(imported) => imported,
        Err(message) => {
            diagnostics.push(diagnostic(call_site, message, Severity::Error));
            erase_range(document, parsed, attribute_range, output_depth);
            return true;
        }
    };
    let attrs = ast::attrs_for_node(&item);
    let full_range = item_range_with_attrs(&item, &attrs);
    let item_tokens = document.tokens_in_without(
        parsed,
        full_range.clone(),
        std::slice::from_ref(&attribute_range),
    );
    let item_input = match token_stream_from_document(&item_tokens) {
        Ok(input) => input,
        Err(message) => {
            diagnostics.push(diagnostic(call_site, message, Severity::Error));
            return false;
        }
    };
    let expansion = match provider.expand(
        &imported.package,
        &imported.macro_name,
        ProcMacroKind::Attribute,
        &args,
        Some(&item_input),
        call_site.clone(),
    ) {
        Ok(expansion) => expansion,
        Err(message) => {
            diagnostics.push(diagnostic(
                call_site,
                format!(
                    "failed to expand {}::{}: {message}",
                    imported.package, imported.macro_name
                ),
                Severity::Error,
            ));
            return false;
        }
    };
    let has_error =
        append_macro_diagnostics(diagnostics, expansion.diagnostics, &call_site, source_len);
    let mut replacement = if has_error {
        item_tokens
    } else {
        match document_tokens_from_output(&expansion.output, &call_site, source_len, output_depth) {
            Ok(output) => {
                if let Err(message) = validate_item_output(
                    &output,
                    "attribute macro output must contain only top-level items",
                ) {
                    diagnostics.push(diagnostic(call_site, message, Severity::Error));
                    return false;
                }
                output
            }
            Err(message) => {
                diagnostics.push(diagnostic(call_site, message, Severity::Error));
                return false;
            }
        }
    };
    replacement.insert(
        0,
        erased_token(document, parsed, attribute_range, output_depth),
    );
    replace_if_parses(
        document,
        parsed,
        full_range,
        replacement,
        &call_site,
        "attribute macro output is not valid Riddle",
        diagnostics,
    )
}

#[allow(clippy::too_many_arguments)]
fn expand_function_action(
    document: &mut TokenDocument,
    parsed: &ParsedDocument,
    call: ast::MacroCall,
    macros: &MacroScope,
    provider: &mut dyn ProcMacroProvider,
    source_len: usize,
    output_depth: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let call_range = range(call.syntax().text_range());
    let call_site = document.origin_in(parsed, call_range.clone());
    let path = call
        .path()
        .map(|path| {
            path.segments()
                .filter_map(|segment| segment.name_token().map(|token| token.text().to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let imported = match resolve_macro(&path, ProcMacroKind::FunctionLike, macros, provider) {
        Ok(imported) => imported,
        Err(message) => {
            diagnostics.push(diagnostic(call_site, message, Severity::Error));
            return false;
        }
    };
    let call_tokens = document.tokens_in(parsed, call_range.clone());
    let input = match macro_call_input(&call_tokens) {
        Ok(input) => input,
        Err(message) => {
            diagnostics.push(diagnostic(call_site, message, Severity::Error));
            return false;
        }
    };
    let output = if imported.package == STANDARD_MACRO_PACKAGE {
        match expand_standard_print_macro(&imported.macro_name, &input, &call_site) {
            Ok(output) => output,
            Err((span, message)) => {
                diagnostics.push(diagnostic(span, message, Severity::Error));
                return false;
            }
        }
    } else {
        let expansion = match provider.expand(
            &imported.package,
            &imported.macro_name,
            ProcMacroKind::FunctionLike,
            &input,
            None,
            call_site.clone(),
        ) {
            Ok(expansion) => expansion,
            Err(message) => {
                diagnostics.push(diagnostic(
                    call_site,
                    format!(
                        "failed to expand {}::{}: {message}",
                        imported.package, imported.macro_name
                    ),
                    Severity::Error,
                ));
                return false;
            }
        };
        if append_macro_diagnostics(diagnostics, expansion.diagnostics, &call_site, source_len) {
            return false;
        }
        expansion.output
    };
    let mut replacement =
        match document_tokens_from_output(&output, &call_site, source_len, output_depth) {
            Ok(output) => output,
            Err(message) => {
                diagnostics.push(diagnostic(call_site, message, Severity::Error));
                return false;
            }
        };
    replacement.insert(
        0,
        erased_token(document, parsed, call_range.clone(), output_depth),
    );

    let mut candidate = document.clone();
    candidate.replace(parsed, call_range.clone(), replacement.clone());
    if candidate.parse().parse.errors.is_empty() {
        *document = candidate;
        return true;
    }
    if let Some(statement) = call
        .syntax()
        .ancestors()
        .find(|node| node.kind() == SyntaxKind::ExprStmt)
    {
        let mut candidate = document.clone();
        candidate.replace(parsed, range(statement.text_range()), replacement);
        if candidate.parse().parse.errors.is_empty() {
            *document = candidate;
            return true;
        }
    }
    diagnostics.push(diagnostic(
        call_site,
        "function-like macro output is not valid in this position".into(),
        Severity::Error,
    ));
    false
}

fn expand_standard_derive_macro(
    name: &str,
    item: &SyntaxNode,
    call_site: &Range<usize>,
) -> Result<ProcMacroTokenStream, String> {
    if name != "Debug" {
        return Err(format!("unknown standard derive macro `{name}`"));
    }

    let source = if let Some(item) = ast::StructDecl::cast(item.clone()) {
        expand_standard_debug_struct(&item)?
    } else if let Some(item) = ast::EnumDecl::cast(item.clone()) {
        expand_standard_debug_enum(&item)?
    } else {
        return Err("Debug can only be derived for structs and enums".into());
    };
    let mut output = ProcMacroTokenStream::from_source(&source, 0)
        .map_err(|message| format!("failed to build Debug implementation: {message}"))?;
    output.set_span(call_site.clone());
    Ok(output)
}

fn expand_standard_debug_struct(item: &ast::StructDecl) -> Result<String, String> {
    let name = item
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| "Debug derive input is missing a struct name".to_string())?;
    let mut output = debug_impl_header(&name, item.generic_params(), item.where_clause());
    let fields = item
        .field_list()
        .map(|fields| {
            fields
                .fields()
                .filter_map(|field| field.name().map(|name| name.text().to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if fields.is_empty() {
        let _ = write!(output, "formatter.write_str({name:?})");
    } else {
        let _ = write!(output, "formatter.write_str({:?})?;", format!("{name} {{ "));
        for (index, field) in fields.iter().enumerate() {
            if index > 0 {
                output.push_str("formatter.write_str(\", \")?;");
            }
            let _ = write!(output, "formatter.write_str({:?})?;", format!("{field}: "));
            let _ = write!(
                output,
                "crate::std::fmt::write_debug(&self.{field}, &mut *formatter)?;"
            );
        }
        output.push_str("formatter.write_str(\" }\")");
    }
    output.push_str(" } }");
    Ok(output)
}

fn expand_standard_debug_enum(item: &ast::EnumDecl) -> Result<String, String> {
    let name = item
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| "Debug derive input is missing an enum name".to_string())?;
    let mut output = debug_impl_header(&name, item.generic_params(), item.where_clause());
    output.push_str("match self {");
    for variant in item.variants() {
        let variant_name = variant
            .name()
            .map(|name| name.text().to_string())
            .ok_or_else(|| "Debug derive input contains an unnamed enum variant".to_string())?;
        let fields = variant
            .field_list()
            .map(|fields| {
                fields
                    .fields()
                    .filter_map(|field| field.name().map(|name| name.text().to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let tuple_fields = variant.tuple_types().count();
        let has_tuple_fields = variant
            .syntax()
            .children_with_tokens()
            .any(|element| element.kind() == SyntaxKind::LParen);

        if !fields.is_empty() {
            let _ = write!(output, "{name}::{variant_name} {{");
            for (index, field) in fields.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                let _ = write!(output, "{field}:__riddle_debug_{index}");
            }
            let _ = write!(
                output,
                "}}=>{{formatter.write_str({:?})?;",
                format!("{variant_name} {{ ")
            );
            for (index, field) in fields.iter().enumerate() {
                if index > 0 {
                    output.push_str("formatter.write_str(\", \")?;");
                }
                let _ = write!(output, "formatter.write_str({:?})?;", format!("{field}: "));
                let _ = write!(
                    output,
                    "crate::std::fmt::write_debug(__riddle_debug_{index}, &mut *formatter)?;"
                );
            }
            output.push_str("formatter.write_str(\" }\")},");
        } else if has_tuple_fields && tuple_fields > 0 {
            let _ = write!(output, "{name}::{variant_name}(");
            for index in 0..tuple_fields {
                if index > 0 {
                    output.push(',');
                }
                let _ = write!(output, "__riddle_debug_{index}");
            }
            let _ = write!(
                output,
                ")=>{{formatter.write_str({:?})?;",
                format!("{variant_name}(")
            );
            for index in 0..tuple_fields {
                if index > 0 {
                    output.push_str("formatter.write_str(\", \")?;");
                }
                let _ = write!(
                    output,
                    "crate::std::fmt::write_debug(__riddle_debug_{index}, &mut *formatter)?;"
                );
            }
            output.push_str("formatter.write_str(\")\")},");
        } else {
            let braces = variant.field_list().is_some();
            let tuple = has_tuple_fields;
            let _ = write!(output, "{name}::{variant_name}");
            if braces {
                output.push_str(" {}");
            } else if tuple {
                output.push_str("()");
            }
            let _ = write!(output, "=>formatter.write_str({variant_name:?}),");
        }
    }
    output.push_str("} } }");
    Ok(output)
}

fn debug_impl_header(
    name: &str,
    generic_params: Option<ast::GenericParams>,
    where_clause: Option<ast::WhereClause>,
) -> String {
    let declaration = generic_params
        .as_ref()
        .map(|params| params.syntax().text().to_string())
        .unwrap_or_default();
    let params = generic_params
        .as_ref()
        .map(|params| params.params().collect::<Vec<_>>())
        .unwrap_or_default();
    let type_arguments = if params.is_empty() {
        String::new()
    } else {
        format!(
            "<{}>",
            params
                .iter()
                .map(|param| param.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let debug_bounds = params
        .iter()
        .filter(|param| !param.is_const)
        .map(|param| format!("{}: crate::std::fmt::Debug", param.name))
        .collect::<Vec<_>>();
    let mut where_clause = where_clause
        .map(|clause| clause.syntax().text().to_string())
        .unwrap_or_default();
    if !debug_bounds.is_empty() {
        if where_clause.is_empty() {
            where_clause.push_str("where ");
        } else if where_clause.trim_end().ends_with(',') {
            where_clause.push(' ');
        } else {
            where_clause.push_str(", ");
        }
        where_clause.push_str(&debug_bounds.join(", "));
    }

    format!(
        "impl{declaration} crate::std::fmt::Debug for {name}{type_arguments} {where_clause} {{ fun fmt(&self, formatter: &mut crate::std::fmt::Formatter) -> crate::std::fmt::Result {{"
    )
}

fn expand_standard_print_macro(
    name: &str,
    input: &ProcMacroTokenStream,
    call_site: &Range<usize>,
) -> Result<ProcMacroTokenStream, (Range<usize>, String)> {
    let arguments = split_macro_arguments(input).map_err(|message| (call_site.clone(), message))?;
    let newline = name == "println";
    let mut body = ProcMacroTokenStream::default();
    if arguments.is_empty() {
        if newline {
            emit_print_call(&mut body, string_token_stream("\n", call_site), call_site);
        }
        return Ok(grouped_stream(ProcMacroDelimiter::Brace, body, call_site));
    }

    let format = &arguments[0];
    let format_span = token_stream_span(format).unwrap_or_else(|| call_site.clone());
    let [ProcMacroTokenTree::Literal { text, .. }] = format.trees.as_slice() else {
        return Err((
            format_span,
            "format argument must be a string literal".into(),
        ));
    };
    if classify_literal(text) != Some(SyntaxKind::String) {
        return Err((
            format_span,
            "format argument must be a string literal".into(),
        ));
    }
    let format = parse_format_literal(text, &format_span)
        .map_err(|message| (format_span.clone(), message))?;
    let values = &arguments[1..];
    let placeholders = format.arguments.len();
    if placeholders != values.len() {
        return Err((
            format_span,
            format!(
                "format string contains {placeholders} placeholder(s), but {} argument(s) were supplied",
                values.len()
            ),
        ));
    }

    for (index, segment) in format.segments.iter().enumerate() {
        if !segment.is_empty() {
            emit_print_call(
                &mut body,
                string_token_stream(segment, call_site),
                call_site,
            );
        }
        if let Some(value) = values.get(index) {
            let span = token_stream_span(value).unwrap_or_else(|| call_site.clone());
            let argument = &format.arguments[index];
            match argument.trait_kind {
                StandardFormatTrait::Display => {
                    emit_io_call(&mut body, "print", value.clone(), &argument.span, &span);
                }
                StandardFormatTrait::Debug => {
                    emit_io_call(
                        &mut body,
                        "print_debug",
                        value.clone(),
                        &argument.span,
                        &span,
                    );
                }
            }
        }
    }
    if newline {
        emit_print_call(&mut body, string_token_stream("\n", call_site), call_site);
    }
    Ok(grouped_stream(ProcMacroDelimiter::Brace, body, call_site))
}

fn split_macro_arguments(
    input: &ProcMacroTokenStream,
) -> Result<Vec<ProcMacroTokenStream>, String> {
    let mut arguments = Vec::new();
    let mut current = ProcMacroTokenStream::default();
    for tree in &input.trees {
        if matches!(tree, ProcMacroTokenTree::Punct { value: ',', .. }) {
            if current.is_empty() {
                return Err("expected an argument before `,`".into());
            }
            arguments.push(current);
            current = ProcMacroTokenStream::default();
        } else {
            current.trees.push(tree.clone());
        }
    }
    if !current.is_empty() {
        arguments.push(current);
    }
    Ok(arguments)
}

#[derive(Clone, Copy)]
enum StandardFormatTrait {
    Display,
    Debug,
}

struct StandardFormatString {
    segments: Vec<String>,
    arguments: Vec<StandardFormatArgument>,
}

struct StandardFormatArgument {
    trait_kind: StandardFormatTrait,
    span: Range<usize>,
}

struct DecodedLiteralChar {
    value: char,
    source: Range<usize>,
}

fn parse_format_literal(
    text: &str,
    literal_span: &Range<usize>,
) -> Result<StandardFormatString, String> {
    let chars = decode_string_literal(text).ok_or("invalid format string literal")?;
    let mut segments = vec![String::new()];
    let mut arguments = Vec::new();
    let mut index = 0;
    while let Some(character) = chars.get(index) {
        let next = |offset: usize| chars.get(index + offset).map(|character| character.value);
        match character {
            DecodedLiteralChar { value: '{', .. } if next(1) == Some('{') => {
                segments.last_mut().unwrap().push('{');
                index += 2;
            }
            DecodedLiteralChar { value: '{', source } if next(1) == Some('}') => {
                arguments.push(StandardFormatArgument {
                    trait_kind: StandardFormatTrait::Display,
                    span: literal_source_range(
                        text,
                        literal_span,
                        source.start..chars[index + 1].source.end,
                    ),
                });
                segments.push(String::new());
                index += 2;
            }
            DecodedLiteralChar { value: '{', source } if next(1) == Some(':') => {
                if next(2) != Some('?') || next(3) != Some('}') {
                    return Err("only `{}` and `{:?}` format placeholders are supported".into());
                }
                arguments.push(StandardFormatArgument {
                    trait_kind: StandardFormatTrait::Debug,
                    span: literal_source_range(
                        text,
                        literal_span,
                        source.start..chars[index + 3].source.end,
                    ),
                });
                segments.push(String::new());
                index += 4;
            }
            DecodedLiteralChar { value: '{', .. } => {
                return Err("only `{}` and `{:?}` format placeholders are supported".into());
            }
            DecodedLiteralChar { value: '}', .. } if next(1) == Some('}') => {
                segments.last_mut().unwrap().push('}');
                index += 2;
            }
            DecodedLiteralChar { value: '}', .. } => {
                return Err("unmatched `}` in format string".into());
            }
            character => {
                segments.last_mut().unwrap().push(character.value);
                index += 1;
            }
        }
    }
    Ok(StandardFormatString {
        segments,
        arguments,
    })
}

fn literal_source_range(
    text: &str,
    literal_span: &Range<usize>,
    relative: Range<usize>,
) -> Range<usize> {
    if literal_span.end.saturating_sub(literal_span.start) == text.len() {
        literal_span.start + relative.start..literal_span.start + relative.end
    } else {
        literal_span.clone()
    }
}

fn decode_string_literal(text: &str) -> Option<Vec<DecodedLiteralChar>> {
    let (body, raw) = raw_string_body_range(text)
        .map(|body| (body, true))
        .or_else(|| {
            (text.starts_with('"') && text.ends_with('"') && text.len() >= 2)
                .then_some((1..text.len() - 1, false))
        })?;
    let mut output = Vec::new();
    let mut chars = text[body.clone()].char_indices();
    while let Some((offset, character)) = chars.next() {
        let start = body.start + offset;
        if raw || character != '\\' {
            output.push(DecodedLiteralChar {
                value: character,
                source: start..start + character.len_utf8(),
            });
            continue;
        }
        let (end, value) = match chars.next() {
            Some((offset, 'n')) => (body.start + offset + 1, '\n'),
            Some((offset, 'r')) => (body.start + offset + 1, '\r'),
            Some((offset, 't')) => (body.start + offset + 1, '\t'),
            Some((offset, '0')) => (body.start + offset + 1, '\0'),
            Some((offset, '\\')) => (body.start + offset + 1, '\\'),
            Some((offset, '\'')) => (body.start + offset + 1, '\''),
            Some((offset, '"')) => (body.start + offset + 1, '"'),
            Some((offset, character)) => (body.start + offset + character.len_utf8(), character),
            None => (start + 1, '\\'),
        };
        output.push(DecodedLiteralChar {
            value,
            source: start..end,
        });
    }
    Some(output)
}

fn raw_string_body_range(text: &str) -> Option<Range<usize>> {
    let rest = text.strip_prefix('r')?;
    let hashes = rest.bytes().take_while(|&byte| byte == b'#').count();
    let opening_quote = 1 + hashes;
    if text.as_bytes().get(opening_quote) != Some(&b'"') {
        return None;
    }
    let suffix_start = text.len().checked_sub(1 + hashes)?;
    if suffix_start <= opening_quote || text.as_bytes().get(suffix_start) != Some(&b'"') {
        return None;
    }
    text.as_bytes()[suffix_start + 1..]
        .iter()
        .all(|&byte| byte == b'#')
        .then_some(opening_quote + 1..suffix_start)
}

fn string_token_stream(value: &str, span: &Range<usize>) -> ProcMacroTokenStream {
    let mut text = String::from("\"");
    for character in value.chars() {
        match character {
            '\0' => text.push_str("\\0"),
            '\n' => text.push_str("\\n"),
            '\r' => text.push_str("\\r"),
            '\t' => text.push_str("\\t"),
            '\\' => text.push_str("\\\\"),
            '"' => text.push_str("\\\""),
            character => text.push(character),
        }
    }
    text.push('"');
    ProcMacroTokenStream {
        trees: vec![ProcMacroTokenTree::Literal {
            text,
            span: span.clone(),
        }],
    }
}

fn token_stream_span(stream: &ProcMacroTokenStream) -> Option<Range<usize>> {
    Some(stream.trees.first()?.span().start..stream.trees.last()?.span().end)
}

fn grouped_stream(
    delimiter: ProcMacroDelimiter,
    stream: ProcMacroTokenStream,
    span: &Range<usize>,
) -> ProcMacroTokenStream {
    ProcMacroTokenStream {
        trees: vec![ProcMacroTokenTree::Group {
            delimiter,
            stream,
            span: span.clone(),
        }],
    }
}

fn emit_print_call(
    output: &mut ProcMacroTokenStream,
    value: ProcMacroTokenStream,
    span: &Range<usize>,
) {
    emit_io_call(output, "print", value, span, span);
}

fn emit_io_call(
    output: &mut ProcMacroTokenStream,
    function: &str,
    value: ProcMacroTokenStream,
    callee_span: &Range<usize>,
    value_span: &Range<usize>,
) {
    push_path(output, &["crate", "std", "io", function], callee_span);
    let mut arguments = ProcMacroTokenStream::default();
    arguments.trees.push(ProcMacroTokenTree::Punct {
        value: '&',
        spacing: ProcMacroSpacing::Alone,
        span: value_span.clone(),
    });
    arguments.trees.push(ProcMacroTokenTree::Group {
        delimiter: ProcMacroDelimiter::Parenthesis,
        stream: value,
        span: value_span.clone(),
    });
    output.trees.push(ProcMacroTokenTree::Group {
        delimiter: ProcMacroDelimiter::Parenthesis,
        stream: arguments,
        span: value_span.clone(),
    });
    output.trees.push(ProcMacroTokenTree::Punct {
        value: ';',
        spacing: ProcMacroSpacing::Alone,
        span: callee_span.clone(),
    });
}

fn push_path(output: &mut ProcMacroTokenStream, path: &[&str], span: &Range<usize>) {
    for (index, segment) in path.iter().enumerate() {
        if index > 0 {
            output.trees.push(ProcMacroTokenTree::Punct {
                value: ':',
                spacing: ProcMacroSpacing::Joint,
                span: span.clone(),
            });
            output.trees.push(ProcMacroTokenTree::Punct {
                value: ':',
                spacing: ProcMacroSpacing::Alone,
                span: span.clone(),
            });
        }
        output.trees.push(ProcMacroTokenTree::Ident {
            text: (*segment).into(),
            span: span.clone(),
        });
    }
}

#[allow(dead_code)]
fn expand_source_legacy(source: &str, provider: &mut dyn ProcMacroProvider) -> ExpandedSource {
    expand_source_at_depth(source, provider, 0, &MacroScope::default())
}

fn expand_source_at_depth(
    source: &str,
    provider: &mut dyn ProcMacroProvider,
    depth: usize,
    inherited_macros: &MacroScope,
) -> ExpandedSource {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(source);
    if !parse.errors.is_empty() {
        return ExpandedSource {
            source: source.into(),
            ..ExpandedSource::default()
        };
    }

    let root = ast::Root::cast(parse.syntax()).expect("parsed root should be a root node");
    let mut insertions = Vec::new();
    let mut diagnostics = Vec::new();

    let mut statements = Vec::new();
    let mut erased_imports = Vec::new();
    collect_scoped_statements(
        root.stmts().collect(),
        inherited_macros,
        provider,
        None,
        &[],
        &mut statements,
        &mut erased_imports,
        &mut diagnostics,
    );
    for ScopedStatement {
        statement: stmt,
        macros,
    } in statements
    {
        let attrs = ast::attrs_for_node(stmt.syntax());
        let derive_attrs = attrs
            .iter()
            .filter(|attr| attr.name().is_some_and(|name| name.text() == "derive"))
            .collect::<Vec<_>>();
        if derive_attrs.is_empty() {
            continue;
        }
        let insertion_call_site = range(derive_attrs[0].syntax().text_range());
        if !matches!(&stmt, ast::Stmt::StructDecl(_) | ast::Stmt::EnumDecl(_)) {
            diagnostics.push(diagnostic(
                insertion_call_site,
                "derive macros may only be applied to structs or enums".into(),
                Severity::Error,
            ));
            continue;
        }

        let mut input = ProcMacroTokenStream::default();
        let mut input_error = None;
        for attr in &attrs {
            if attr.name().is_some_and(|name| name.text() != "derive") {
                let attr_range = range(attr.syntax().text_range());
                match ProcMacroTokenStream::from_source(
                    &source[attr_range.clone()],
                    attr_range.start,
                ) {
                    Ok(tokens) => input.extend(tokens),
                    Err(message) => input_error = Some(message),
                }
            }
        }
        let item_range = range(stmt.syntax().text_range());
        match ProcMacroTokenStream::from_source(&source[item_range.clone()], item_range.start) {
            Ok(tokens) => input.extend(tokens),
            Err(message) => input_error = Some(message),
        }
        if let Some(message) = input_error {
            diagnostics.push(diagnostic(
                insertion_call_site,
                format!("failed to tokenize derive input: {message}"),
                Severity::Error,
            ));
            continue;
        }

        let mut generated = String::new();
        let mut generated_spans = Vec::new();
        for attr in derive_attrs {
            let call_site = range(attr.syntax().text_range());
            let paths = match parse_derive_paths(&attr.raw_text()) {
                Ok(paths) => paths,
                Err(message) => {
                    diagnostics.push(diagnostic(call_site.clone(), message, Severity::Error));
                    continue;
                }
            };
            for path in paths {
                let (package, macro_name) = match path {
                    DeriveMacroPath::Qualified {
                        package,
                        macro_name,
                    } => (package, macro_name),
                    DeriveMacroPath::Imported(local_name) => {
                        let Some(imported) = macros.get(&local_name) else {
                            diagnostics.push(diagnostic(
                                call_site.clone(),
                                format!(
                                    "cannot find derive macro `{local_name}` in this scope; import it with `use package::{local_name};`"
                                ),
                                Severity::Error,
                            ));
                            continue;
                        };
                        if imported.kind != ProcMacroKind::Derive {
                            diagnostics.push(diagnostic(
                                call_site.clone(),
                                format!("`{local_name}` is not a derive macro"),
                                Severity::Error,
                            ));
                            continue;
                        }
                        (imported.package.clone(), imported.macro_name.clone())
                    }
                };
                if depth >= MAX_DERIVE_EXPANSION_DEPTH {
                    diagnostics.push(diagnostic(
                        call_site.clone(),
                        format!(
                            "derive expansion exceeded the maximum depth of {MAX_DERIVE_EXPANSION_DEPTH}"
                        ),
                        Severity::Error,
                    ));
                    continue;
                }
                match provider.expand(
                    &package,
                    &macro_name,
                    ProcMacroKind::Derive,
                    &input,
                    None,
                    call_site.clone(),
                ) {
                    Ok(expansion) => {
                        let has_error = expansion
                            .diagnostics
                            .iter()
                            .any(|diagnostic| diagnostic.severity == Severity::Error);
                        diagnostics.extend(expansion.diagnostics.into_iter().map(|d| {
                            let span = if valid_span(&d.span, source.len()) {
                                d.span
                            } else {
                                call_site.clone()
                            };
                            diagnostic(span, d.message, d.severity)
                        }));
                        if !expansion.output.is_empty() && !has_error {
                            let mut rendered = expansion.output.render();
                            if let Some(message) = validate_output(&rendered.source) {
                                diagnostics.push(diagnostic(
                                    call_site.clone(),
                                    message,
                                    Severity::Error,
                                ));
                            } else {
                                let nested = expand_source_at_depth(
                                    &rendered.source,
                                    provider,
                                    depth + 1,
                                    &macros,
                                );
                                let nested_has_error = nested
                                    .diagnostics
                                    .iter()
                                    .any(|diagnostic| diagnostic.severity == Severity::Error);
                                diagnostics.extend(nested.diagnostics.into_iter().map(|inner| {
                                    let span = rendered_span_for_diagnostic(&rendered, &inner)
                                        .unwrap_or_else(|| call_site.clone());
                                    diagnostic(span, inner.message, inner.severity)
                                }));
                                if nested_has_error {
                                    continue;
                                }
                                shift_rendered_spans(&mut rendered.spans, &nested.insertions);
                                rendered.source = nested.source;
                                if !generated.is_empty() {
                                    generated.push('\n');
                                }
                                let output_start = generated.len();
                                generated_spans.extend(rendered.spans.into_iter().map(|span| {
                                    GeneratedSpanMapping {
                                        generated: span.generated.start + output_start
                                            ..span.generated.end + output_start,
                                        original: span.original,
                                    }
                                }));
                                generated.push_str(&rendered.source);
                            }
                        }
                    }
                    Err(message) => diagnostics.push(diagnostic(
                        call_site.clone(),
                        format!("failed to expand `{package}::{macro_name}`: {message}"),
                        Severity::Error,
                    )),
                }
            }
        }

        if !generated.is_empty() {
            for span in &mut generated_spans {
                span.generated.start += 1;
                span.generated.end += 1;
            }
            insertions.push(GeneratedInsertion {
                at: usize::from(stmt.syntax().text_range().end()),
                text: format!("\n{generated}\n"),
                call_site: insertion_call_site,
                spans: generated_spans,
            });
        }
    }

    insertions.sort_by_key(|insertion| insertion.at);
    let source_without_macro_imports = erase_imports(source, &erased_imports);
    let mut expanded = String::with_capacity(source.len());
    let mut cursor = 0usize;
    for insertion in &insertions {
        expanded.push_str(&source_without_macro_imports[cursor..insertion.at]);
        expanded.push_str(&insertion.text);
        cursor = insertion.at;
    }
    expanded.push_str(&source_without_macro_imports[cursor..]);

    ExpandedSource {
        source: expanded,
        parse: None,
        mappings: Vec::new(),
        insertions,
        macro_occurrences: Vec::new(),
        diagnostics,
    }
}

fn rendered_span_for_diagnostic(
    rendered: &RenderedTokenStream,
    diagnostic: &Diagnostic,
) -> Option<Range<usize>> {
    let label = diagnostic.labels.first()?;
    let start = usize::from(label.range.start());
    let end = usize::from(label.range.end());
    rendered
        .spans
        .iter()
        .filter(|span| span.generated.start <= start && end <= span.generated.end)
        .min_by_key(|span| span.generated.len())
        .map(|span| span.original.clone())
}

fn shift_rendered_spans(spans: &mut [RenderedTokenSpan], insertions: &[GeneratedInsertion]) {
    for span in spans {
        let start_shift = insertions
            .iter()
            .filter(|insertion| insertion.at <= span.generated.start)
            .map(|insertion| insertion.text.len())
            .sum::<usize>();
        let end_shift = insertions
            .iter()
            .filter(|insertion| insertion.at < span.generated.end)
            .map(|insertion| insertion.text.len())
            .sum::<usize>();
        span.generated.start += start_shift;
        span.generated.end += end_shift;
    }
}

fn build_macro_reexports(root: &ast::Root, provider: &dyn ProcMacroProvider) -> MacroReexports {
    let mut modules = Vec::new();
    collect_module_uses(root.stmts().collect(), &[], &mut modules);
    let mut reexports = modules
        .iter()
        .map(|(path, _)| (path.clone(), MacroScope::default()))
        .collect::<MacroReexports>();

    loop {
        let snapshot = reexports.clone();
        let mut changed = false;
        for (path, uses) in &modules {
            for use_decl in uses.iter().filter(|use_decl| use_decl.is_pub()) {
                let ProcMacroUse::Imports { imports, .. } =
                    parse_proc_macro_use_resolved(use_decl, provider, &snapshot, path)
                else {
                    continue;
                };
                let exported = reexports.entry(path.clone()).or_default();
                for (name, imported) in imports {
                    if exported.get(&name).is_none_or(|existing| {
                        existing.package != imported.package
                            || existing.macro_name != imported.macro_name
                            || existing.kind != imported.kind
                    }) {
                        exported.insert(name, imported);
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    reexports
}

fn collect_module_uses(
    statements: Vec<ast::Stmt>,
    path: &[String],
    output: &mut Vec<(Vec<String>, Vec<ast::UseDecl>)>,
) {
    output.push((
        path.to_vec(),
        statements
            .iter()
            .filter_map(|statement| match statement {
                ast::Stmt::UseDecl(use_decl) => Some(use_decl.clone()),
                _ => None,
            })
            .collect(),
    ));
    for statement in statements {
        let ast::Stmt::ModDecl(module) = statement else {
            continue;
        };
        let Some(items) = module.items() else {
            continue;
        };
        let Some(name) = module.name().map(|name| name.text().to_string()) else {
            continue;
        };
        let mut child_path = path.to_vec();
        child_path.push(name);
        collect_module_uses(items.collect(), &child_path, output);
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_scoped_statements(
    statements: Vec<ast::Stmt>,
    inherited_macros: &MacroScope,
    provider: &dyn ProcMacroProvider,
    reexports: Option<&MacroReexports>,
    module_path: &[String],
    output: &mut Vec<ScopedStatement>,
    erased_imports: &mut Vec<Range<usize>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut macros = inherited_macros.clone();
    for statement in &statements {
        let ast::Stmt::UseDecl(use_decl) = statement else {
            continue;
        };
        let use_range = range(use_decl.syntax().text_range());
        match reexports.map_or_else(
            || parse_proc_macro_use(use_decl, provider),
            |reexports| parse_proc_macro_use_resolved(use_decl, provider, reexports, module_path),
        ) {
            ProcMacroUse::Ordinary => {}
            ProcMacroUse::Imports {
                imports,
                replacement,
            } => {
                if replacement.is_none() {
                    erased_imports.push(use_range.clone());
                }
                for (local_name, imported) in imports {
                    match macros.entry(local_name) {
                        Entry::Occupied(entry) => diagnostics.push(diagnostic(
                            use_range.clone(),
                            format!("macro `{}` is imported more than once", entry.key()),
                            Severity::Error,
                        )),
                        Entry::Vacant(entry) => {
                            entry.insert(imported);
                        }
                    }
                }
            }
            ProcMacroUse::Invalid {
                message,
                replacement,
            } => {
                if replacement.is_none() {
                    erased_imports.push(use_range.clone());
                }
                diagnostics.push(diagnostic(use_range, message, Severity::Error));
            }
        }
    }

    for statement in statements {
        let (children, child_path) = match &statement {
            ast::Stmt::ModDecl(module) => {
                let children = module
                    .items()
                    .map(|items| items.collect::<Vec<_>>())
                    .unwrap_or_default();
                let mut child_path = module_path.to_vec();
                if let Some(name) = module.name() {
                    child_path.push(name.text().to_string());
                }
                (children, child_path)
            }
            _ => (Vec::new(), module_path.to_vec()),
        };
        output.push(ScopedStatement {
            statement,
            macros: macros.clone(),
        });
        if !children.is_empty() {
            collect_scoped_statements(
                children,
                &MacroScope::default(),
                provider,
                reexports,
                &child_path,
                output,
                erased_imports,
                diagnostics,
            );
        }
    }
}

fn parse_proc_macro_use(use_decl: &ast::UseDecl, provider: &dyn ProcMacroProvider) -> ProcMacroUse {
    parse_proc_macro_use_resolved(use_decl, provider, &MacroReexports::default(), &[])
}

fn parse_proc_macro_use_resolved(
    use_decl: &ast::UseDecl,
    provider: &dyn ProcMacroProvider,
    reexports: &MacroReexports,
    module_path: &[String],
) -> ProcMacroUse {
    let Some(tree) = use_decl.use_tree() else {
        return ProcMacroUse::Ordinary;
    };
    let mut bindings = Vec::new();
    flatten_use_tree(&tree, &[], &mut bindings);

    let mut imports = Vec::new();
    let mut ordinary = Vec::new();
    let mut errors = Vec::new();
    for binding in bindings {
        let Some(package) = binding.path.first() else {
            ordinary.push(binding);
            continue;
        };

        if let Some(exports) = provider.exports(package) {
            if binding.glob {
                if binding.path.len() != 1 {
                    errors.push(format!(
                        "process macros are exported from the root of proc-macro package `{package}`"
                    ));
                    continue;
                }
                imports.extend(exports.into_iter().map(|export| {
                    (
                        export.name.clone(),
                        ImportedMacro {
                            package: package.clone(),
                            definition: provider.definition(package, &export.name),
                            macro_name: export.name,
                            kind: export.kind,
                            helper_attributes: export.helper_attributes,
                            binding: None,
                        },
                    )
                }));
                continue;
            }
            if binding.path.len() != 2 {
                errors.push(format!(
                    "process macros are exported from the root of proc-macro package `{package}`"
                ));
                continue;
            }
            let macro_name = &binding.path[1];
            let Some(export) = exports.iter().find(|export| export.name == *macro_name) else {
                errors.push(format!(
                    "proc-macro package `{package}` does not export macro `{macro_name}`"
                ));
                continue;
            };
            imports.push((
                binding.alias.clone().unwrap_or_else(|| macro_name.clone()),
                ImportedMacro {
                    package: package.clone(),
                    macro_name: macro_name.clone(),
                    kind: export.kind,
                    helper_attributes: export.helper_attributes.clone(),
                    binding: binding.local_range.clone(),
                    definition: provider.definition(package, macro_name),
                },
            ));
            continue;
        }

        if binding.glob {
            if let Some(exported) = find_reexport_scope(reexports, module_path, &binding.path) {
                imports.extend(exported.iter().map(|(name, imported)| {
                    let mut imported = imported.clone();
                    imported.binding = None;
                    (name.clone(), imported)
                }));
                ordinary.push(binding);
            } else {
                ordinary.push(binding);
            }
            continue;
        }

        let Some((name, export_path)) = binding.path.split_last() else {
            ordinary.push(binding);
            continue;
        };
        let Some(imported) = find_reexport_scope(reexports, module_path, export_path)
            .and_then(|exports| exports.get(name))
            .cloned()
        else {
            ordinary.push(binding);
            continue;
        };
        let mut imported = imported;
        imported.binding = binding.local_range.clone();
        imports.push((
            binding.alias.clone().unwrap_or_else(|| name.clone()),
            imported,
        ));
    }

    if imports.is_empty() && errors.is_empty() {
        return ProcMacroUse::Ordinary;
    }
    let replacement = render_use_bindings(&ordinary, use_decl.is_pub());
    if !errors.is_empty() {
        return ProcMacroUse::Invalid {
            message: errors.join("; "),
            replacement,
        };
    }
    ProcMacroUse::Imports {
        imports,
        replacement,
    }
}

fn find_reexport_scope<'a>(
    reexports: &'a MacroReexports,
    module_path: &[String],
    path: &[String],
) -> Option<&'a MacroScope> {
    let mut resolved = module_path.to_vec();
    let mut rest = path;
    if path.first().is_some_and(|segment| segment == "crate") {
        resolved.clear();
        rest = &path[1..];
    } else if path.first().is_some_and(|segment| segment == "self") {
        rest = &path[1..];
    } else {
        while rest.first().is_some_and(|segment| segment == "super") {
            resolved.pop();
            rest = &rest[1..];
        }
        if rest.len() == path.len()
            && let Some(exports) = reexports.get(path)
        {
            return Some(exports);
        }
    }
    resolved.extend_from_slice(rest);
    reexports.get(&resolved)
}

fn render_use_bindings(bindings: &[UseBinding], public: bool) -> Option<String> {
    if bindings.is_empty() {
        return None;
    }
    let mut output = String::new();
    for binding in bindings {
        if public {
            output.push_str("pub ");
        }
        output.push_str("use ");
        output.push_str(&binding.path.join("::"));
        if binding.glob {
            if !binding.path.is_empty() {
                output.push_str("::");
            }
            output.push('*');
        }
        if let Some(alias) = &binding.alias {
            output.push_str(" as ");
            output.push_str(alias);
        }
        output.push_str(";\n");
    }
    Some(output)
}

fn flatten_use_tree(tree: &ast::UseTree, prefix: &[String], output: &mut Vec<UseBinding>) {
    let mut path = prefix.to_vec();
    if let Some(tree_path) = tree.path() {
        path.extend(
            tree_path
                .segments()
                .filter_map(|segment| segment.name_token().map(|token| token.text().to_string())),
        );
    }
    if let Some(list) = tree.subtree_list() {
        for child in list.trees() {
            flatten_use_tree(&child, &path, output);
        }
        return;
    }
    let alias = tree.alias();
    let local_range = alias
        .as_ref()
        .map(|token| range(token.text_range()))
        .or_else(|| {
            (!tree.is_glob())
                .then(|| tree.path()?.segments().last()?.name_token())
                .flatten()
                .map(|token| range(token.text_range()))
        });
    output.push(UseBinding {
        path,
        alias: alias.map(|alias| alias.text().to_string()),
        glob: tree.is_glob(),
        local_range,
    });
}

fn erase_imports(source: &str, ranges: &[Range<usize>]) -> String {
    if ranges.is_empty() {
        return source.into();
    }
    let mut bytes = source.as_bytes().to_vec();
    for range in ranges {
        for byte in &mut bytes[range.clone()] {
            if !matches!(*byte, b'\n' | b'\r') {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(bytes).expect("blanking imports should preserve UTF-8")
}

fn parse_derive_paths(raw: &str) -> Result<Vec<DeriveMacroPath>, String> {
    parse_derive_invocations(raw)
        .map(|invocations| invocations.into_iter().map(|(path, _)| path).collect())
}

fn parse_derive_invocations(raw: &str) -> Result<Vec<(DeriveMacroPath, Range<usize>)>, String> {
    let attribute = ProcMacroTokenStream::from_source(raw, 0)
        .map_err(|_| "malformed derive attribute".to_string())?;
    let [
        ProcMacroTokenTree::Punct { value: '#', .. },
        ProcMacroTokenTree::Group {
            delimiter: ProcMacroDelimiter::Bracket,
            stream: body,
            ..
        },
    ] = attribute.trees.as_slice()
    else {
        return Err("malformed derive attribute".into());
    };
    let [
        ProcMacroTokenTree::Ident { text: name, .. },
        ProcMacroTokenTree::Group {
            delimiter: ProcMacroDelimiter::Parenthesis,
            stream: arguments,
            ..
        },
    ] = body.trees.as_slice()
    else {
        return Err("malformed derive attribute".into());
    };
    if name != "derive" {
        return Err("malformed derive attribute".into());
    }

    let mut paths = Vec::new();
    let trees = &arguments.trees;
    let mut index = 0;
    while index < trees.len() {
        let Some(name) = token_ident(trees.get(index)) else {
            return Err("expected a derive macro name".into());
        };
        if token_punct(trees.get(index + 1)) == Some(':')
            && token_punct(trees.get(index + 2)) == Some(':')
            && let Some(macro_name) = token_ident(trees.get(index + 3))
        {
            paths.push((
                DeriveMacroPath::Qualified {
                    package: name.into(),
                    macro_name: macro_name.into(),
                },
                trees[index + 3].span().clone(),
            ));
            index += 4;
        } else {
            paths.push((
                DeriveMacroPath::Imported(name.into()),
                trees[index].span().clone(),
            ));
            index += 1;
        }
        if index < trees.len() {
            if token_punct(trees.get(index)) != Some(',') {
                return Err("expected comma between derive macros".into());
            }
            index += 1;
        }
    }
    if paths.is_empty() {
        return Err("derive attribute must contain at least one macro".into());
    }
    Ok(paths)
}

fn token_ident(tree: Option<&ProcMacroTokenTree>) -> Option<&str> {
    match tree? {
        ProcMacroTokenTree::Ident { text, .. } => Some(text),
        _ => None,
    }
}

fn token_punct(tree: Option<&ProcMacroTokenTree>) -> Option<char> {
    match tree? {
        ProcMacroTokenTree::Punct { value, .. } => Some(*value),
        _ => None,
    }
}

fn validate_output(output: &str) -> Option<String> {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(output);
    if let Some(error) = parse.errors.first() {
        return Some(format!(
            "derive macro output is not valid Riddle: {}",
            error.message
        ));
    }
    let root = ast::Root::cast(parse.syntax()).expect("parsed output should have a root");
    if root.stmts().any(|stmt| !is_item(&stmt)) {
        return Some("derive macro output must contain only top-level items".into());
    }
    None
}

fn is_item(stmt: &ast::Stmt) -> bool {
    matches!(
        stmt,
        ast::Stmt::FuncDecl(_)
            | ast::Stmt::StructDecl(_)
            | ast::Stmt::EnumDecl(_)
            | ast::Stmt::TraitDecl(_)
            | ast::Stmt::ImplDecl(_)
            | ast::Stmt::ConstDecl(_)
            | ast::Stmt::TypeAliasDecl(_)
            | ast::Stmt::ModDecl(_)
            | ast::Stmt::UseDecl(_)
            | ast::Stmt::ExternBlock(_)
            | ast::Stmt::ExternFnDecl(_)
    )
}

fn diagnostic(range: Range<usize>, message: String, severity: Severity) -> Diagnostic {
    Diagnostic {
        code: PROC_MACRO_ERROR,
        severity,
        message,
        labels: vec![SourceLabel {
            range: text_range(&range),
            message: String::new(),
            style: LabelStyle::Primary,
        }],
        help: None,
        notes: Vec::new(),
    }
}

fn text_range(range: &Range<usize>) -> TextRange {
    TextRange::new((range.start as u32).into(), (range.end as u32).into())
}

fn valid_span(span: &Range<usize>, source_len: usize) -> bool {
    span.start <= span.end && span.end <= source_len
}

fn range(range: TextRange) -> Range<usize> {
    usize::from(range.start())..usize::from(range.end())
}
