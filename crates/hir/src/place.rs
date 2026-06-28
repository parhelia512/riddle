use crate::body::StmtId;

/// A projection from a root local to a sub-location.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Projection {
    /// Struct field by index in the struct definition.
    Field(usize),
    /// Array/tuple element by literal index.
    Index(usize),
}

/// A path to a memory location: `local.field[0].subfield`.
///
/// `Place { local, projections: [] }` — the whole binding.
/// `Place { local, projections: [Field(1)] }` — `local.1`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Place {
    pub local: StmtId,
    pub projections: Vec<Projection>,
}

impl Place {
    pub fn root(local: StmtId) -> Self {
        Self {
            local,
            projections: Vec::new(),
        }
    }

    /// Push a field projection.
    pub fn field(mut self, idx: usize) -> Self {
        self.projections.push(Projection::Field(idx));
        self
    }

    /// Push an index projection.
    pub fn index(mut self, idx: usize) -> Self {
        self.projections.push(Projection::Index(idx));
        self
    }

    /// True when `self` is a prefix of `other` — meaning moving/borrowing
    /// `self` would invalidate `other`.
    ///
    /// `x.0` is a prefix of `x.0.1` → true (x.0.1 is inside x.0).
    /// `x.0` is a prefix of `x.1`   → false (different fields).
    /// `x`   is a prefix of `x.0`   → true (root covers all fields).
    pub fn is_prefix_of(&self, other: &Place) -> bool {
        self.local == other.local
            && self.projections.len() <= other.projections.len()
            && self
                .projections
                .iter()
                .zip(&other.projections)
                .all(|(a, b)| a == b)
    }
}
