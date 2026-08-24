pub mod diagnostics;
pub mod fmt;
pub mod pipeline;
pub mod proc_macro;
pub mod target;

pub const GIT_HASH: &str = env!("GIT_HASH");

pub(crate) fn text_size(value: usize) -> rowan::TextSize {
    rowan::TextSize::from(u32::try_from(value).expect("source offset should fit in u32"))
}

pub(crate) fn text_range(start: usize, end: usize) -> rowan::TextRange {
    rowan::TextRange::new(text_size(start), text_size(end))
}
