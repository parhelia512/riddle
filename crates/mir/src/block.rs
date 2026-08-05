use crate::{
    instr::{Inst, Terminator},
    value::Value,
};

/// A basic block — a linear sequence of instructions ending with a terminator.
#[derive(Debug, Clone)]
pub struct Block {
    /// Optional label for debugging and backend code generation.
    pub label: Option<String>,

    /// Instructions in this block.
    pub insts: Vec<Inst>,

    /// Block terminator (branch, conditional branch, or return).
    pub terminator: Terminator,

    /// The Value number assigned to the first instruction in this block.
    /// Subsequent instructions get consecutive values.
    pub start_value: u32,
}

impl Block {
    #[must_use]
    pub const fn new(start_value: u32) -> Self {
        Self {
            label: None,
            insts: Vec::new(),
            terminator: Terminator::Pending,
            start_value,
        }
    }

    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The next available Value number in this block.
    #[must_use]
    pub fn next_value(&self) -> u32 {
        self.value_at(self.insts.len()).0
    }

    /// Returns the value assigned to the instruction at `index`.
    ///
    /// # Panics
    ///
    /// Panics when the instruction index or resulting value number exceeds `u32`.
    #[must_use]
    pub fn value_at(&self, index: usize) -> Value {
        let index = u32::try_from(index).expect("block instruction index should fit in u32");
        Value(
            self.start_value
                .checked_add(index)
                .expect("MIR value number should fit in u32"),
        )
    }
}
