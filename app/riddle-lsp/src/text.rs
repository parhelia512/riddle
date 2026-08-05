use lsp_types::{Position, Range, TextDocumentContentChangeEvent};
use rowan::{TextRange, TextSize};
use std::path::PathBuf;

pub fn normalized_path(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or_else(|_| {
        path.parent()
            .and_then(|parent| std::fs::canonicalize(parent).ok())
            .and_then(|parent| path.file_name().map(|name| parent.join(name)))
            .unwrap_or(path)
    })
}

pub struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    pub(crate) fn new(source: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter_map(|(offset, byte)| (byte == b'\n').then_some(offset + 1)),
        );
        Self { starts }
    }

    pub(crate) fn position(&self, source: &str, offset: usize) -> Option<Position> {
        if offset > source.len() || !source.is_char_boundary(offset) {
            return None;
        }
        let line = self.starts.partition_point(|start| *start <= offset) - 1;
        let character = source[self.starts[line]..offset]
            .chars()
            .map(char::len_utf16)
            .sum::<usize>();
        Some(Position::new(
            u32::try_from(line).ok()?,
            u32::try_from(character).ok()?,
        ))
    }

    pub(crate) fn range(&self, source: &str, range: TextRange) -> Option<Range> {
        Some(Range::new(
            self.position(source, usize::from(range.start()))?,
            self.position(source, usize::from(range.end()))?,
        ))
    }
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

pub fn offset_for_position(source: &str, position: Position) -> Option<usize> {
    let mut line_start = 0;
    for _ in 0..position.line {
        line_start += source[line_start..].find('\n')? + 1;
    }
    let line_end = source[line_start..]
        .find('\n')
        .map_or(source.len(), |offset| line_start + offset);
    let line = source[line_start..line_end]
        .strip_suffix('\r')
        .unwrap_or_else(|| &source[line_start..line_end]);
    let mut utf16_column = 0;
    for (byte, ch) in line.char_indices() {
        if utf16_column == position.character {
            return Some(line_start + byte);
        }
        utf16_column +=
            u32::try_from(ch.len_utf16()).expect("a char uses at most two UTF-16 units");
        if utf16_column > position.character {
            return None;
        }
    }
    (utf16_column == position.character).then_some(line_start + line.len())
}

pub fn is_identifier_continue(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

pub fn range_is_in_source(range: TextRange, source_len: usize) -> bool {
    usize::from(range.end()) <= source_len
}

pub fn ranges_overlap(a: TextRange, b: TextRange) -> bool {
    a.start() < b.end() && b.start() < a.end()
}

pub fn text_size(value: usize) -> TextSize {
    TextSize::from(u32::try_from(value).expect("source offset should fit in u32"))
}

pub fn text_range(start: usize, end: usize) -> TextRange {
    TextRange::new(text_size(start), text_size(end))
}
