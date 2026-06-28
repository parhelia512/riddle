use la_arena::Arena;

use crate::block::Block;
use crate::instr::{Inst, Terminator};
use crate::types::Type;
use crate::value::{BlockId, Value};

/// An MIR function — a control-flow graph made of basic blocks.
#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub params: Vec<Param>,
    pub ret_type: Type,
    pub blocks: Arena<Block>,
    pub entry: BlockId,

    /// Global counter for the next unreserved Value number.
    pub next_value: u32,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    pub value: Value,
}

impl Function {
    pub fn new(name: String, ret_type: Type) -> Self {
        let mut blocks = Arena::new();
        let entry_block = Block::new(0);
        let entry = blocks.alloc(entry_block);

        Self {
            name,
            params: Vec::new(),
            ret_type,
            blocks,
            entry,
            next_value: 0,
        }
    }

    /// Add a parameter and return its Value.
    pub fn add_param(&mut self, name: String, ty: Type) -> Value {
        let v = self.alloc_value();
        self.params.push(Param { name, ty, value: v });
        v
    }

    /// Reserve the next Value number.
    pub fn alloc_value(&mut self) -> Value {
        let v = Value(self.next_value);
        self.next_value += 1;
        v
    }

    /// Create a new empty basic block.
    pub fn new_block(&mut self) -> BlockId {
        let start = self.next_value;
        let block = Block::new(start);
        self.blocks.alloc(block)
    }

    /// Create a new basic block with a label.
    pub fn new_block_labeled(&mut self, label: impl Into<String>) -> BlockId {
        let start = self.next_value;
        let block = Block::new(start).with_label(label);
        self.blocks.alloc(block)
    }

    /// Append an instruction to a block and return its Value.
    pub fn push_inst(&mut self, block: BlockId, inst: Inst) -> Value {
        let b = &mut self.blocks[block];
        let v = Value(b.next_value());
        b.insts.push(inst);
        if v.0 >= self.next_value {
            self.next_value = v.0 + 1;
        }
        v
    }

    /// Set the terminator of a block.
    pub fn set_terminator(&mut self, block: BlockId, term: Terminator) {
        self.blocks[block].terminator = term;
    }
}
