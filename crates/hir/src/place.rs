use crate::body::{ExprId, PatternBindingId};

/// A projection from a root local to a sub-location.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Projection {
    /// Struct field by index in the struct definition.
    Field(usize),
    /// Array/tuple element; `None` is a runtime index and overlaps every element.
    Index(Option<usize>),
}

/// The local a [`Place`] is rooted in: a pattern binding, a function
/// parameter, or a lambda parameter. Parameters are not pattern bindings,
/// but they own their value just the same and field moves out of them
/// must be tracked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlaceRoot {
    Pattern(PatternBindingId),
    Param(usize),
    LambdaParam { lambda: ExprId, index: usize },
}

/// A path to a memory location: `local.field[0].subfield`.
///
/// `Place { root, projections: [] }` — the whole binding.
/// `Place { root, projections: [Field(1)] }` — `local.1`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Place {
    pub root: PlaceRoot,
    pub projections: Vec<Projection>,
}

impl Place {
    #[must_use]
    pub const fn root(local: PatternBindingId) -> Self {
        Self {
            root: PlaceRoot::Pattern(local),
            projections: Vec::new(),
        }
    }

    /// A place rooted in function parameter `index`.
    #[must_use]
    pub const fn param(index: usize) -> Self {
        Self {
            root: PlaceRoot::Param(index),
            projections: Vec::new(),
        }
    }

    /// A place rooted in parameter `index` of lambda `lambda`.
    #[must_use]
    pub const fn lambda_param(lambda: ExprId, index: usize) -> Self {
        Self {
            root: PlaceRoot::LambdaParam { lambda, index },
            projections: Vec::new(),
        }
    }

    /// Push a field projection.
    #[must_use]
    pub fn field(mut self, idx: usize) -> Self {
        self.projections.push(Projection::Field(idx));
        self
    }

    /// Push an index projection.
    #[must_use]
    pub fn index(mut self, idx: Option<usize>) -> Self {
        self.projections.push(Projection::Index(idx));
        self
    }

    /// True when `self` is a prefix of `other` — meaning moving/borrowing
    /// `self` would invalidate `other`.
    ///
    /// `x.0` is a prefix of `x.0.1` → true (x.0.1 is inside x.0).
    /// `x.0` is a prefix of `x.1`   → false (different fields).
    /// `x`   is a prefix of `x.0`   → true (root covers all fields).
    #[must_use]
    pub fn is_prefix_of(&self, other: &Self) -> bool {
        self.root == other.root
            && self.projections.len() <= other.projections.len()
            && self
                .projections
                .iter()
                .zip(&other.projections)
                .all(|(a, b)| match (a, b) {
                    (Projection::Index(None), Projection::Index(_))
                    | (Projection::Index(_), Projection::Index(None)) => true,
                    _ => a == b,
                })
    }
}
