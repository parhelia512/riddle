use rowan::{NodeOrToken, TextRange, TextSize};

use super::{
    lexer,
    parser::{ParseError, Parser, ReparseEntry},
    syntax_kind::{SyntaxKind, SyntaxNode, SyntaxToken},
    tree_builder::{self, Parse},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReparseMode {
    Full,
    Incremental(SyntaxKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    OutOfBounds {
        offset: usize,
        delete_len: usize,
        source_len: usize,
    },
    NotCharBoundary {
        offset: usize,
    },
    SourceTooLarge,
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::OutOfBounds {
                offset,
                delete_len,
                source_len,
            } => write!(
                f,
                "edit [{offset}..{}) is outside source of length {source_len}",
                offset.saturating_add(*delete_len)
            ),
            EditError::NotCharBoundary { offset } => {
                write!(f, "edit offset {offset} is not a UTF-8 character boundary")
            }
            EditError::SourceTooLarge => write!(
                f,
                "source is too large for rowan TextSize (maximum is u32::MAX bytes)"
            ),
        }
    }
}

impl std::error::Error for EditError {}

pub struct IncrementalParser {
    current: Option<Parse>,
    source: String,
    last_reparse: ReparseMode,
}

impl IncrementalParser {
    pub fn new() -> Self {
        Self {
            current: None,
            source: String::new(),
            last_reparse: ReparseMode::Full,
        }
    }

    /// Full Parsing (First or Backward)
    pub fn set_source(&mut self, source: &str) -> &Parse {
        self.source = source.to_string();
        self.current = Some(parse_full(&self.source));
        self.last_reparse = ReparseMode::Full;
        self.current.as_ref().expect("parse was just initialized")
    }

    /// Apply editing and redirection
    pub fn apply_edit(&mut self, offset: usize, delete_len: usize, insert: &str) -> &Parse {
        self.try_apply_edit(offset, delete_len, insert)
            .unwrap_or_else(|err| panic!("invalid source edit: {err}"))
    }

    pub fn try_apply_edit(
        &mut self,
        offset: usize,
        delete_len: usize,
        insert: &str,
    ) -> Result<&Parse, EditError> {
        self.validate_edit(offset, delete_len)?;

        let old_end = offset
            .checked_add(delete_len)
            .ok_or(EditError::OutOfBounds {
                offset,
                delete_len,
                source_len: self.source.len(),
            })?;

        let mut new_source = String::with_capacity(
            self.source
                .len()
                .checked_sub(delete_len)
                .and_then(|len| len.checked_add(insert.len()))
                .ok_or(EditError::SourceTooLarge)?,
        );
        new_source.push_str(&self.source[..offset]);
        new_source.push_str(insert);
        new_source.push_str(&self.source[old_end..]);

        if new_source.len() > u32::MAX as usize {
            return Err(EditError::SourceTooLarge);
        }

        let incremental = self.current.as_ref().and_then(|old_parse| {
            try_incremental_reparse(
                old_parse,
                &self.source,
                &new_source,
                offset,
                delete_len,
                insert,
            )
        });

        match incremental {
            Some((parse, kind)) => {
                self.current = Some(parse);
                self.last_reparse = ReparseMode::Incremental(kind);
            }
            None => {
                self.current = Some(parse_full(&new_source));
                self.last_reparse = ReparseMode::Full;
            }
        }

        self.source = new_source;
        Ok(self.current.as_ref().expect("parse was just updated"))
    }

    pub fn current_parse(&self) -> Option<&Parse> {
        self.current.as_ref()
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn last_reparse_mode(&self) -> ReparseMode {
        self.last_reparse
    }

    fn validate_edit(&self, offset: usize, delete_len: usize) -> Result<(), EditError> {
        let end = offset
            .checked_add(delete_len)
            .ok_or(EditError::OutOfBounds {
                offset,
                delete_len,
                source_len: self.source.len(),
            })?;

        if offset > self.source.len() || end > self.source.len() {
            return Err(EditError::OutOfBounds {
                offset,
                delete_len,
                source_len: self.source.len(),
            });
        }

        if !self.source.is_char_boundary(offset) {
            return Err(EditError::NotCharBoundary { offset });
        }
        if !self.source.is_char_boundary(end) {
            return Err(EditError::NotCharBoundary { offset: end });
        }

        Ok(())
    }
}

impl Default for IncrementalParser {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_full(source: &str) -> Parse {
    let tokens = lexer::lex(source);
    let mut lex_errors = lexer_error_diagnostics(source, &tokens);
    let parser = Parser::new(source, tokens);
    let (events, tokens, mut errors, source) = parser.parse();
    lex_errors.append(&mut errors);
    tree_builder::build_tree(events, tokens, source, lex_errors)
}

fn parse_fragment(source: &str, entry: ReparseEntry) -> Option<Parse> {
    let tokens = lexer::lex(source);
    let mut lex_errors = lexer_error_diagnostics(source, &tokens);
    let parser = Parser::new(source, tokens);
    let (event, tokens, mut errors, source) = parser.reparse(entry)?;
    lex_errors.append(&mut errors);
    if !lex_errors.is_empty() {
        return None;
    }
    Some(tree_builder::build_tree(event, tokens, source, lex_errors))
}

/// Emit diagnostics for tokens the lexer couldn't recognise.
fn lexer_error_diagnostics(source: &str, tokens: &[lexer::Token]) -> Vec<ParseError> {
    use crate::syntax_kind::SyntaxKind;
    tokens
        .iter()
        .filter(|t| t.kind == SyntaxKind::ErrorNode)
        .map(|t| {
            let text = &source[t.span.start..t.span.end];
            let msg = if text.is_empty() {
                "unrecognized token".into()
            } else {
                format!("unrecognized character: `{}`", text)
            };
            ParseError {
                message: msg,
                span: TextRange::new(
                    TextSize::from(t.span.start as u32),
                    TextSize::from(t.span.end as u32),
                ),
            }
        })
        .collect()
}

fn try_incremental_reparse(
    old_parse: &Parse,
    old_source: &str,
    new_source: &str,
    offset: usize,
    delete_len: usize,
    insert: &str,
) -> Option<(Parse, SyntaxKind)> {
    // Fall back to full reparse when the previous parse has errors —
    // avoids compounding stale diagnostic spans across incremental edits.
    if !old_parse.errors.is_empty() {
        return None;
    }

    // `//...` is the only token in the current lexer whose right boundary can
    // escape an arbitrary syntax node. Newlines also change that boundary.
    // Be conservative around either character.
    if lexical_state_may_escape(old_source, new_source, offset, delete_len, insert.len()) {
        return None;
    }

    let edit_end = offset + delete_len;
    let rowan_range = TextRange::new(to_text_size(offset)?, to_text_size(edit_end)?);
    let root = old_parse.syntax();
    let covering = root.covering_element(rowan_range);

    let mut node = match covering {
        NodeOrToken::Node(node) => Some(node),
        NodeOrToken::Token(token) => token.parent(),
    };

    while let Some(candidate) = node {
        let parent = candidate.parent();

        if let Some(entry) = reparse_entry(candidate.kind())
            && edit_is_inside_stable_boundaries(&candidate, offset, edit_end)
            && let Some((fragment, kind)) = reparse_candidate(
                &candidate,
                entry,
                new_source,
                offset,
                delete_len,
                insert.len(),
            )
        {
            return Some((fragment, kind));
        }

        node = parent;
    }

    None
}

fn reparse_candidate(
    old_node: &SyntaxNode,
    entry: ReparseEntry,
    new_source: &str,
    edit_offset: usize,
    delete_len: usize,
    insert_len: usize,
) -> Option<(Parse, SyntaxKind)> {
    let old_range = old_node.text_range();
    let old_start = text_size_to_usize(old_range.start());
    let old_end = text_size_to_usize(old_range.end());

    let edit_end = edit_offset.checked_add(delete_len)?;
    if edit_offset < old_start || edit_end > old_end {
        return None;
    }

    let new_end = if insert_len >= delete_len {
        old_end.checked_add(insert_len - delete_len)?
    } else {
        old_end.checked_sub(delete_len - insert_len)?
    };

    if new_end > new_source.len()
        || !new_source.is_char_boundary(old_start)
        || !new_source.is_char_boundary(new_end)
    {
        return None;
    }

    let fragment_source = &new_source[old_start..new_end];
    let fragment_parse = parse_fragment(fragment_source, entry)?;

    // A fragment parser must produce exactly one replacement root whose text
    // is byte-for-byte equal to the edited source slice.
    let fragment_root = fragment_parse.syntax();
    if fragment_root.text() != fragment_source {
        return None;
    }

    let new_kind = fragment_root.kind();
    let new_green = old_node.replace_with(fragment_parse.green);

    Some((
        Parse {
            green: new_green,
            errors: fragment_parse.errors,
        },
        new_kind,
    ))
}

fn reparse_entry(kind: SyntaxKind) -> Option<ReparseEntry> {
    match kind {
        SyntaxKind::VarDecl
        | SyntaxKind::FuncDecl
        | SyntaxKind::StructDecl
        | SyntaxKind::BreakStmt
        | SyntaxKind::ContinueStmt
        | SyntaxKind::ReturnStmt
        | SyntaxKind::ExprStmt
        | SyntaxKind::EnumDecl
        | SyntaxKind::TraitDecl
        | SyntaxKind::ImplDecl
        | SyntaxKind::ConstDecl
        | SyntaxKind::TypeAliasDecl => Some(ReparseEntry::Statement),

        SyntaxKind::Block => Some(ReparseEntry::Block),
        SyntaxKind::ParamList => Some(ReparseEntry::ParamList),
        SyntaxKind::StructFieldList => Some(ReparseEntry::StructFieldList),
        SyntaxKind::ArgList => Some(ReparseEntry::ArgList),

        SyntaxKind::IfStmt
        | SyntaxKind::WhileStmt
        | SyntaxKind::ForExpr
        | SyntaxKind::NameRef
        | SyntaxKind::NumberLit
        | SyntaxKind::FloatLit
        | SyntaxKind::StringLit
        | SyntaxKind::CharLit
        | SyntaxKind::BoolLit
        | SyntaxKind::BinaryExpr
        | SyntaxKind::UnaryExpr
        | SyntaxKind::ParenExpr
        | SyntaxKind::CallExpr
        | SyntaxKind::LambdaExpr
        | SyntaxKind::FieldExpr
        | SyntaxKind::StructExpr
        | SyntaxKind::StructExprField
        | SyntaxKind::MatchExpr
        | SyntaxKind::ArrayExpr => Some(ReparseEntry::Expression),

        SyntaxKind::NamedType
        | SyntaxKind::RefType
        | SyntaxKind::TupleType
        | SyntaxKind::ArrayType => Some(ReparseEntry::Type),
        SyntaxKind::FnType => Some(ReparseEntry::Type),

        SyntaxKind::UseDecl | SyntaxKind::ModDecl => Some(ReparseEntry::Statement),

        SyntaxKind::UseTree => Some(ReparseEntry::UseTree),
        SyntaxKind::UseTreeList => Some(ReparseEntry::UseTreeList),
        SyntaxKind::Path => Some(ReparseEntry::Path),

        SyntaxKind::WildcardPattern
        | SyntaxKind::LiteralPattern
        | SyntaxKind::TuplePattern
        | SyntaxKind::StructPattern
        | SyntaxKind::EnumPattern => Some(ReparseEntry::Pattern),

        SyntaxKind::MatchArm => Some(ReparseEntry::MatchArm),
        SyntaxKind::EnumVariant => Some(ReparseEntry::EnumVariant),
        SyntaxKind::GenericParams => Some(ReparseEntry::TypeList),

        _ => None,
    }
}

/// The first and last non-trivia tokens act as unchanged lexical anchors.
/// Reparse only when the edit lies between them; otherwise move to a wider
/// ancestor. This prevents a fragment lexer from missing token merging across
/// the old node boundary.
fn edit_is_inside_stable_boundaries(node: &SyntaxNode, edit_start: usize, edit_end: usize) -> bool {
    let Some(first) = first_non_trivia_token(node) else {
        return false;
    };
    let Some(last) = last_non_trivia_token(node) else {
        return false;
    };

    let interior_start = text_size_to_usize(first.text_range().end());
    let interior_end = text_size_to_usize(last.text_range().start());

    edit_start >= interior_start && edit_end <= interior_end
}

fn first_non_trivia_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    let node_range = node.text_range();
    let mut token = node.first_token()?;

    loop {
        if !token.kind().is_trivia() {
            return Some(token);
        }
        let next = token.next_token()?;
        if !range_contains(node_range, next.text_range()) {
            return None;
        }
        token = next;
    }
}

fn last_non_trivia_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    let node_range = node.text_range();
    let mut token = node.last_token()?;

    loop {
        if !token.kind().is_trivia() {
            return Some(token);
        }
        let previous = token.prev_token()?;
        if !range_contains(node_range, previous.text_range()) {
            return None;
        }
        token = previous;
    }
}

fn range_contains(outer: TextRange, inner: TextRange) -> bool {
    outer.start() <= inner.start() && inner.end() <= outer.end()
}

fn lexical_state_may_escape(
    old_source: &str,
    new_source: &str,
    offset: usize,
    delete_len: usize,
    insert_len: usize,
) -> bool {
    fn sensitive_byte(byte: u8) -> bool {
        matches!(
            byte,
            b'/' | b'\n'
                | b'\r'
                | b':'
                | b'-'
                | b'='
                | b'!'
                | b'<'
                | b'>'
                | b'&'
                | b'|'
                | b'#'
                | b'['
                | b']'
        )
    }

    // Include one byte on each side so deleting the middle of `/x/`, for
    // example, cannot silently create a `//` token spanning past the fragment.
    let old_begin = offset.saturating_sub(1);
    let old_end = offset
        .saturating_add(delete_len)
        .saturating_add(1)
        .min(old_source.len());

    let new_begin = offset.saturating_sub(1);
    let new_end = offset
        .saturating_add(insert_len)
        .saturating_add(1)
        .min(new_source.len());

    old_source.as_bytes()[old_begin..old_end]
        .iter()
        .chain(&new_source.as_bytes()[new_begin..new_end])
        .copied()
        .any(sensitive_byte)
}

fn to_text_size(value: usize) -> Option<TextSize> {
    Some(TextSize::from(u32::try_from(value).ok()?))
}

fn text_size_to_usize(value: TextSize) -> usize {
    u32::from(value) as usize
}
