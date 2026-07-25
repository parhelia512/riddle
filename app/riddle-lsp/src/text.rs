use lsp_types::{Position, TextDocumentContentChangeEvent};
use rowan::TextRange;
use std::path::PathBuf;

pub(crate) fn normalized_path(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or_else(|_| {
        path.parent()
            .and_then(|parent| std::fs::canonicalize(parent).ok())
            .and_then(|parent| path.file_name().map(|name| parent.join(name)))
            .unwrap_or(path)
    })
}

pub fn apply_content_changes(
    text: &mut String,
    changes: Vec<TextDocumentContentChangeEvent>,
) -> bool {
    let mut updated = text.clone();
    for change in changes {
        let Some(range) = change.range else {
            updated = change.text;
            continue;
        };
        let Some(start) = offset_for_position(&updated, range.start) else {
            return false;
        };
        let Some(end) = offset_for_position(&updated, range.end) else {
            return false;
        };
        if start > end {
            return false;
        }
        updated.replace_range(start..end, &change.text);
    }
    *text = updated;
    true
}

pub(crate) fn offset_for_position(source: &str, position: Position) -> Option<usize> {
    let mut line_start = 0;
    for _ in 0..position.line {
        line_start += source[line_start..].find('\n')? + 1;
    }
    let line_end = source[line_start..]
        .find('\n')
        .map(|offset| line_start + offset)
        .unwrap_or(source.len());
    let line = source[line_start..line_end]
        .strip_suffix('\r')
        .unwrap_or(&source[line_start..line_end]);
    let mut utf16_column = 0;
    for (byte, ch) in line.char_indices() {
        if utf16_column == position.character {
            return Some(line_start + byte);
        }
        utf16_column += ch.len_utf16() as u32;
        if utf16_column > position.character {
            return None;
        }
    }
    (utf16_column == position.character).then_some(line_start + line.len())
}

pub(crate) fn is_identifier_continue(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

pub(crate) fn range_is_in_source(range: TextRange, source_len: usize) -> bool {
    usize::from(range.end()) <= source_len
}

pub(crate) fn ranges_overlap(a: TextRange, b: TextRange) -> bool {
    a.start() < b.end() && b.start() < a.end()
}
