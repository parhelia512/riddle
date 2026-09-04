use std::ops::Range;

/// One original file's slice of the combined lowering source.
///
/// The compiler concatenates every package and module into a single source
/// before lowering, so byte offsets recorded during lowering only point at the
/// combined text. These segments map such offsets back to the file a panic
/// location should report.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// Byte range this file occupies in the combined source.
    pub generated: Range<usize>,
    /// Offset of `generated.start` within the original file text.
    pub original_start: usize,
    /// Generated macro output: every offset inside the region reports the
    /// original call site instead of a linear position within the
    /// replacement text, which is longer than the call it replaced.
    pub synthetic: bool,
    /// Path shown in panic messages.
    pub path: String,
    /// Full original file text used to compute line/column.
    pub text: String,
}

impl SourceFile {
    /// Resolves an offset in the combined source to `(path, line, column)`.
    ///
    /// The smallest containing segment wins so a panic inside generated macro
    /// code resolves to the macro's original file instead of the enclosing
    /// user file.
    #[must_use]
    pub fn resolve(files: &[Self], offset: usize) -> Option<(String, u32, u32)> {
        let segment = files
            .iter()
            .filter(|file| file.generated.start <= offset && offset < file.generated.end)
            .min_by_key(|file| file.generated.len())?;
        let original = if segment.synthetic {
            segment.original_start
        } else {
            segment.original_start + offset - segment.generated.start
        };
        let (line, column) = line_column(&segment.text, original);
        Some((segment.path.clone(), line, column))
    }
}

pub(crate) fn line_column(source: &str, offset: usize) -> (u32, u32) {
    let mut line = 1;
    let mut column = 1;
    for (index, ch) in source.char_indices() {
        if index >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}
