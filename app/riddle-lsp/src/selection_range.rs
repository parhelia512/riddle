use ast::support::AstNode;
use lsp_types::{Position, Range, SelectionRange};

use crate::{
    imports::parse_root,
    text::{LineIndex, offset_for_position, text_size},
};

#[cfg(feature = "test")]
#[must_use]
pub fn selection_ranges_for_source(
    source: &str,
    positions: &[Position],
) -> Vec<Option<SelectionRange>> {
    positions
        .iter()
        .map(|position| selection_range_for_position(source, *position))
        .collect()
}

/// Computes nested selection ranges for a document snapshot. Positions that
/// cannot be resolved are skipped, matching the LSP result shape
/// (`SelectionRange[]`).
pub fn selection_ranges_for_text(source: &str, positions: &[Position]) -> Vec<SelectionRange> {
    positions
        .iter()
        .filter_map(|position| selection_range_for_position(source, *position))
        .collect()
}

fn selection_range_for_position(source: &str, position: Position) -> Option<SelectionRange> {
    let offset = offset_for_position(source, position)?;
    let root = parse_root(source)?;
    let syntax = root.syntax();
    let token = match syntax.token_at_offset(text_size(offset)) {
        rowan::TokenAtOffset::None => return None,
        rowan::TokenAtOffset::Single(token) => token,
        rowan::TokenAtOffset::Between(left, right) => {
            // The cursor sits between two tokens; the meaningful one (an
            // identifier, keyword, …) wins over the whitespace/comment.
            if left.kind().is_trivia() { right } else { left }
        }
    };

    let line_index = LineIndex::new(source);
    let lsp_range = |range: rowan::TextRange| line_index.range(source, range);
    // Collapse ancestors that share their child's extent so every level of the
    // returned chain grows the selection.
    let mut ranges: Vec<Range> = Vec::new();
    let mut push = |range: Option<Range>| {
        if let Some(range) = range
            && ranges.last() != Some(&range)
        {
            ranges.push(range);
        }
    };
    push(lsp_range(token.text_range()));
    for ancestor in token.parent_ancestors() {
        push(lsp_range(ancestor.text_range()));
    }

    let mut current: Option<SelectionRange> = None;
    for range in ranges.iter().rev() {
        current = Some(SelectionRange {
            range: *range,
            parent: current.map(Box::new),
        });
    }
    current
}
