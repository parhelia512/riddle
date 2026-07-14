use crate::instr::{Inst, Terminator};

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
    pub fn new(start_value: u32) -> Self {
        Self {
            label: None,
            insts: Vec::new(),
            terminator: Terminator::Pending,
            start_value,
        }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The next available Value number in this block.
    pub fn next_value(&self) -> u32 {
        self.start_value + self.insts.len() as u32
    }
}
