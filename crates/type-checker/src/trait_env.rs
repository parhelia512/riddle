use std::collections::HashMap;

use hir::item_tree::TraitId;

use crate::{Type, lowering::substitute_type};

#[derive(Debug, Clone)]
pub struct TraitBound {
    pub ty: Type,
    pub trait_id: TraitId,
    pub assoc_constraints: Vec<TraitAssocConstraint>,
}

#[derive(Debug, Clone)]
pub struct TraitAssocConstraint {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone)]
struct TraitImpl {
    self_ty: Type,
    bounds: Vec<TraitBound>,
    assoc_types: HashMap<String, Type>,
}

/// Tracks which types implement which traits.
///
/// Built during `TypeChecker::build_trait_env()` after all trait and impl
/// declarations have been validated.  Later phases (move detection, method
/// resolution, …) query this environment instead of scanning the item tree
/// repeatedly.
#[derive(Debug, Clone, Default)]
pub struct TraitEnv {
    /// TraitId → set of types that have an explicit `impl Trait for Type`.
    trait_impls: HashMap<TraitId, Vec<TraitImpl>>,

    /// The `Copy` trait, identified by name.  `None` when the user hasn't
    /// declared a trait named `Copy`.
    pub copy_trait_id: Option<TraitId>,
}

impl TraitEnv {
    /// Whether `ty` implements the trait identified by `trait_id`.
    pub fn type_implements(&self, ty: &Type, trait_id: TraitId) -> bool {
        if self.has_builtin_impl(ty, trait_id) {
            return true;
        }
        if let Some(impls) = self.trait_impls.get(&trait_id) {
            if impls.iter().any(|candidate| {
                self.impl_subst(candidate, ty)
                    .map(|subst| self.bounds_satisfied(&candidate.bounds, &subst))
                    .unwrap_or(false)
            }) {
                return true;
            }
        }
        self.derive_impl_for_composite(ty, trait_id)
    }

    /// Convenience: `type_implements(ty, copy_trait_id)`.
    pub fn type_is_copy(&self, ty: &Type) -> bool {
        match self.copy_trait_id {
            Some(tid) => self.type_implements(ty, tid),
            None => self.builtin_copy_fallback(ty),
        }
    }

    pub fn associated_type(&self, ty: &Type, trait_id: TraitId, name: &str) -> Option<Type> {
        self.trait_impls.get(&trait_id)?.iter().find_map(|imp| {
            let subst = self.impl_subst(imp, ty)?;
            self.bounds_satisfied(&imp.bounds, &subst).then_some(())?;
            imp.assoc_types
                .get(name)
                .map(|ty| substitute_type(ty, &subst))
        })
    }

    pub(crate) fn insert_impl(
        &mut self,
        trait_id: TraitId,
        self_ty: Type,
        bounds: Vec<TraitBound>,
        assoc_types: HashMap<String, Type>,
    ) {
        self.trait_impls
            .entry(trait_id)
            .or_default()
            .push(TraitImpl {
                self_ty,
                bounds,
                assoc_types,
            });
    }

    pub(crate) fn set_copy_trait(&mut self, id: TraitId) {
        self.copy_trait_id = Some(id);
    }

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
            Type::Array(inner, _) => self.type_implements(inner, trait_id),
            _ => false,
        }
    }

    fn builtin_copy_fallback(&self, ty: &Type) -> bool {
        match ty {
            Type::Tuple(elements) => elements.iter().all(|elem| self.builtin_copy_fallback(elem)),
            Type::Array(inner, _) => self.builtin_copy_fallback(inner),
            _ => ty.is_fundamentally_copy(),
        }
    }

    fn impl_subst(&self, imp: &TraitImpl, actual: &Type) -> Option<HashMap<String, Type>> {
        let mut subst = HashMap::new();
        crate::lowering::collect_subst(&imp.self_ty, actual, &mut subst).then_some(subst)
    }

    fn bounds_satisfied(&self, bounds: &[TraitBound], subst: &HashMap<String, Type>) -> bool {
        bounds.iter().all(|bound| {
            let actual = substitute_type(&bound.ty, subst);
            if !self.type_implements(&actual, bound.trait_id) {
                return false;
            }
            bound.assoc_constraints.iter().all(|constraint| {
                let expected = substitute_type(&constraint.ty, subst);
                self.associated_type(&actual, bound.trait_id, &constraint.name)
                    .map(|actual| {
                        actual.is_unknown_like() || expected.is_unknown_like() || actual == expected
                    })
                    .unwrap_or(false)
            })
        })
    }
}
