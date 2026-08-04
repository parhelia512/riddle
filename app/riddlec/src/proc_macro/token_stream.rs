use super::*;

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

pub(super) struct RenderedTokenStream {
    pub(super) source: String,
    pub(super) spans: Vec<RenderedTokenSpan>,
}

pub(super) struct RenderedTokenSpan {
    pub(super) generated: Range<usize>,
    pub(super) original: Range<usize>,
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

    pub(super) fn render(&self) -> RenderedTokenStream {
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

pub(super) fn opening_delimiter(kind: SyntaxKind) -> Option<(ProcMacroDelimiter, SyntaxKind)> {
    match kind {
        SyntaxKind::LParen => Some((ProcMacroDelimiter::Parenthesis, SyntaxKind::RParen)),
        SyntaxKind::LBrace => Some((ProcMacroDelimiter::Brace, SyntaxKind::RBrace)),
        SyntaxKind::LBracket => Some((ProcMacroDelimiter::Bracket, SyntaxKind::RBracket)),
        _ => None,
    }
}

pub(super) fn is_closing_delimiter(kind: SyntaxKind) -> bool {
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
