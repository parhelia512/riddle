use std::collections::{HashMap, HashSet};

use hir::item_tree::TraitId;

use crate::Type;

/// Tracks which types implement which traits.
///
/// Built during `TypeChecker::build_trait_env()` after all trait and impl
/// declarations have been validated.  Later phases (move detection, method
/// resolution, …) query this environment instead of scanning the item tree
/// repeatedly.
#[derive(Debug, Clone, Default)]
pub struct TraitEnv {
    /// TraitId → set of types that have an explicit `impl Trait for Type`.
    trait_impls: HashMap<TraitId, HashSet<Type>>,

    /// The `Copy` trait, identified by name.  `None` when the user hasn't
    /// declared a trait named `Copy`.
    pub copy_trait_id: Option<TraitId>,
}

impl TraitEnv {
    // ── public queries ──────────────────────────────────────────────

    /// Whether `ty` implements the trait identified by `trait_id`.
    pub fn type_implements(&self, ty: &Type, trait_id: TraitId) -> bool {
        if self.has_builtin_impl(ty, trait_id) {
            return true;
        }
        if let Some(impls) = self.trait_impls.get(&trait_id) {
            if impls.contains(ty) {
                return true;
            }
        }
        self.derive_impl_for_composite(ty, trait_id)
    }

    /// Convenience: `type_implements(ty, copy_trait_id)`.
    pub fn type_is_copy(&self, ty: &Type) -> bool {
        match self.copy_trait_id {
            Some(tid) => self.type_implements(ty, tid),
            None => ty.is_fundamentally_copy(),
        }
    }

    // ── builders ───────────────────────────────────────────────────

    pub(crate) fn insert_impl(&mut self, trait_id: TraitId, self_ty: Type) {
        self.trait_impls
            .entry(trait_id)
            .or_default()
            .insert(self_ty);
    }

    pub(crate) fn set_copy_trait(&mut self, id: TraitId) {
        self.copy_trait_id = Some(id);
    }

    // ── internal ────────────────────────────────────────────────────

    fn has_builtin_impl(&self, ty: &Type, trait_id: TraitId) -> bool {
        if Some(trait_id) == self.copy_trait_id {
            return ty.is_fundamentally_copy();
        }
        false
    }

    fn derive_impl_for_composite(&self, ty: &Type, trait_id: TraitId) -> bool {
        match ty {
            Type::Tuple(elements) => elements
                .iter()
                .all(|elem| self.type_implements(elem, trait_id)),
            Type::Array(inner) => self.type_implements(inner, trait_id),
            _ => false,
        }
    }
}
