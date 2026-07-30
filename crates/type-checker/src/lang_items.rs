use std::collections::HashMap;

use hir::item_tree::TraitId;

/// Every recognized `#[lang = "..."]` name.
///
/// The variants are intentionally exhaustive: anything not listed is an
/// unknown lang item and produces a diagnostic rather than being silently
/// ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LangItem {
    // Deterministic destruction
    Drop,

    // Copy semantics
    Copy,

    // Clone
    Clone,

    // Comparison
    PartialEq,
    Eq,
    PartialOrd,
    Ord,

    // Arithmetic
    Add,
    Sub,
    Mul,
    Div,
    Rem,

    // Unary
    Neg,
    Not,

    // Bitwise / shift
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,

    // Compound assignment — arithmetic
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    RemAssign,

    // Compound assignment — bitwise / shift
    BitAndAssign,
    BitOrAssign,
    BitXorAssign,
    ShlAssign,
    ShrAssign,

    // Indexing
    Index,
    IndexMut,
}

impl LangItem {
    /// Parses the string value of a `#[lang = "..."]` attribute.
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "drop" => Self::Drop,
            "copy" => Self::Copy,
            "clone" => Self::Clone,
            "partial_eq" => Self::PartialEq,
            "eq" => Self::Eq,
            "partial_ord" => Self::PartialOrd,
            "ord" => Self::Ord,
            "add" => Self::Add,
            "sub" => Self::Sub,
            "mul" => Self::Mul,
            "div" => Self::Div,
            "rem" => Self::Rem,
            "neg" => Self::Neg,
            "not" => Self::Not,
            "bitand" => Self::BitAnd,
            "bitor" => Self::BitOr,
            "bitxor" => Self::BitXor,
            "shl" => Self::Shl,
            "shr" => Self::Shr,
            "add_assign" => Self::AddAssign,
            "sub_assign" => Self::SubAssign,
            "mul_assign" => Self::MulAssign,
            "div_assign" => Self::DivAssign,
            "rem_assign" => Self::RemAssign,
            "bitand_assign" => Self::BitAndAssign,
            "bitor_assign" => Self::BitOrAssign,
            "bitxor_assign" => Self::BitXorAssign,
            "shl_assign" => Self::ShlAssign,
            "shr_assign" => Self::ShrAssign,
            "index" => Self::Index,
            "index_mut" => Self::IndexMut,
            _ => return None,
        })
    }

    /// The canonical string form used in `#[lang = "..."]`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Drop => "drop",
            Self::Copy => "copy",
            Self::Clone => "clone",
            Self::PartialEq => "partial_eq",
            Self::Eq => "eq",
            Self::PartialOrd => "partial_ord",
            Self::Ord => "ord",
            Self::Add => "add",
            Self::Sub => "sub",
            Self::Mul => "mul",
            Self::Div => "div",
            Self::Rem => "rem",
            Self::Neg => "neg",
            Self::Not => "not",
            Self::BitAnd => "bitand",
            Self::BitOr => "bitor",
            Self::BitXor => "bitxor",
            Self::Shl => "shl",
            Self::Shr => "shr",
            Self::AddAssign => "add_assign",
            Self::SubAssign => "sub_assign",
            Self::MulAssign => "mul_assign",
            Self::DivAssign => "div_assign",
            Self::RemAssign => "rem_assign",
            Self::BitAndAssign => "bitand_assign",
            Self::BitOrAssign => "bitor_assign",
            Self::BitXorAssign => "bitxor_assign",
            Self::ShlAssign => "shl_assign",
            Self::ShrAssign => "shr_assign",
            Self::Index => "index",
            Self::IndexMut => "index_mut",
        }
    }

    /// Whether this lang item governs a composite (tuple/array structural)
    /// derivation rule. Returns `(is_binary_rhs)` for the subset that do.
    pub(crate) fn composite_kind(self) -> Option<CompositeKind> {
        Some(match self {
            Self::Copy => CompositeKind::Same,
            Self::Eq => CompositeKind::Same,
            Self::Ord => CompositeKind::Same,
            Self::PartialEq => CompositeKind::Binary,
            Self::PartialOrd => CompositeKind::Binary,
            _ => return None,
        })
    }
}

/// Whether a composite lang trait uses the same type on both sides or allows
/// a distinct RHS type parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompositeKind {
    Same,
    Binary,
}

/// Maps each recognized `LangItem` to its `TraitId`.
///
/// Built once during `TypeChecker::build_trait_env` and then consulted by all
/// later passes.  Downstream consumers (type checker, move checker, MIR
/// lowerer) should query this registry rather than scanning the item tree.
#[derive(Debug, Clone, Default)]
pub struct LangItemRegistry {
    items: HashMap<LangItem, TraitId>,
    /// Reverse map: TraitId → LangItem, populated in lock-step.
    by_trait: HashMap<TraitId, LangItem>,
}

/// Result of a registry insertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterResult {
    /// Successfully registered.
    Ok,
    /// The same `LangItem` was already registered (duplicate item definition).
    DuplicateItem,
    /// The same `TraitId` was already registered for a different `LangItem`
    /// (one trait annotated with two `#[lang]` attributes).
    DuplicateTrait,
}

impl LangItemRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks up the `TraitId` for a lang item.
    pub fn get(&self, item: LangItem) -> Option<TraitId> {
        self.items.get(&item).copied()
    }

    /// Looks up the `LangItem` for a trait id (reverse direction).
    pub fn lang_of(&self, id: TraitId) -> Option<LangItem> {
        self.by_trait.get(&id).copied()
    }

    /// Registers a lang item.
    ///
    /// Returns `DuplicateItem` when the same `LangItem` was already registered
    /// (first definition wins — not overwritten).
    /// Returns `DuplicateTrait` when `id` is already registered for a different
    /// `LangItem` (one trait with two `#[lang]` attributes — not inserted).
    pub fn register(&mut self, item: LangItem, id: TraitId) -> RegisterResult {
        if self.items.contains_key(&item) {
            return RegisterResult::DuplicateItem;
        }
        if self.by_trait.contains_key(&id) {
            return RegisterResult::DuplicateTrait;
        }
        self.items.insert(item, id);
        self.by_trait.insert(id, item);
        RegisterResult::Ok
    }
}
