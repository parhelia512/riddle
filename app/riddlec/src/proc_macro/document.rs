use super::{scope::valid_span, token_stream::*, *};

#[derive(Debug, Clone)]
pub(super) struct DocumentToken {
    pub(super) kind: SyntaxKind,
    pub(super) text: String,
    pub(super) original: Range<usize>,
    pub(super) depth: usize,
    pub(super) generated: bool,
}

pub(super) struct ParsedDocument {
    pub(super) source: String,
    pub(super) parse: Parse,
    pub(super) ranges: Vec<Range<usize>>,
}

#[derive(Clone)]
pub(super) struct TokenDocument {
    pub(super) tokens: Vec<DocumentToken>,
}

impl TokenDocument {
    pub(super) fn from_source(source: &str) -> Self {
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

    pub(super) fn parse(&self) -> ParsedDocument {
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

    pub(super) fn replace(
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

    pub(super) fn tokens_in(
        &self,
        parsed: &ParsedDocument,
        range: Range<usize>,
    ) -> Vec<DocumentToken> {
        self.tokens
            .iter()
            .zip(&parsed.ranges)
            .filter(|(_, token)| range.start <= token.start && token.end <= range.end)
            .map(|(token, _)| token.clone())
            .collect()
    }

    pub(super) fn tokens_in_without(
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

    pub(super) fn tokens_in_replacing(
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

    pub(super) fn depth_in(&self, parsed: &ParsedDocument, range: Range<usize>) -> usize {
        self.tokens
            .iter()
            .zip(&parsed.ranges)
            .filter(|(_, token)| range.start <= token.start && token.end <= range.end)
            .map(|(token, _)| token.depth)
            .max()
            .unwrap_or(0)
    }

    pub(super) fn finish(self) -> ExpandedDocument {
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

pub(super) struct ExpandedDocument {
    pub(super) source: String,
    pub(super) parse: Parse,
    pub(super) mappings: Vec<ExpandedTokenMapping>,
}

pub(super) fn token_stream_from_document(
    tokens: &[DocumentToken],
) -> Result<ProcMacroTokenStream, String> {
    let mut index = 0usize;
    let (stream, closed) = parse_document_stream(tokens, &mut index, None)?;
    if closed.is_some() || index != tokens.len() {
        return Err("unexpected closing delimiter in process macro input".into());
    }
    Ok(stream)
}

pub(super) fn parse_document_stream(
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

pub(super) fn token_text_is_punctuation(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_punctuation())
}

pub(super) fn document_tokens_from_output(
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

pub(super) fn flatten_output_stream(
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

pub(super) fn checked_output_span(
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

pub(super) fn classify_word(text: &str) -> Option<SyntaxKind> {
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

pub(super) fn classify_literal(text: &str) -> Option<SyntaxKind> {
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
