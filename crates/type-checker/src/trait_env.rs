use std::collections::HashMap;

use hir::item_tree::TraitId;

use crate::{Type, lowering::substitute_type};

#[derive(Debug, Clone)]
pub struct TraitBound {
    pub ty: Type,
    pub trait_id: TraitId,
    pub trait_args: Vec<Type>,
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
    trait_args: Vec<Type>,
    bounds: Vec<TraitBound>,
    assoc_types: HashMap<String, Type>,
}

#[derive(Debug, Clone, Copy)]
enum CompositeTraitKind {
    Copy,
    PartialEq(bool),
    Eq,
    PartialOrd(bool),
    Ord,
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

    /// Language traits with compiler-provided tuple and array implementations.
    composite_traits: HashMap<TraitId, CompositeTraitKind>,
}

impl TraitEnv {
    /// Whether `ty` implements the trait identified by `trait_id`.
    pub fn type_implements(&self, ty: &Type, trait_id: TraitId) -> bool {
        self.type_implements_inner(ty, trait_id, &[], &[], 0)
    }

    pub fn type_implements_with_args(
        &self,
        ty: &Type,
        trait_id: TraitId,
        trait_args: &[Type],
    ) -> bool {
        self.type_implements_inner(ty, trait_id, trait_args, &[], 0)
    }

    pub(crate) fn type_implements_assuming(
        &self,
        ty: &Type,
        trait_id: TraitId,
        assumptions: &[TraitBound],
    ) -> bool {
        self.type_implements_inner(ty, trait_id, &[], assumptions, 0)
    }

    fn type_implements_inner(
        &self,
        ty: &Type,
        trait_id: TraitId,
        trait_args: &[Type],
        assumptions: &[TraitBound],
        depth: usize,
    ) -> bool {
        if depth > 64 {
            return false;
        }
        if assumptions.iter().any(|bound| {
            bound.trait_id == trait_id
                && bound.ty == *ty
                && (trait_args.is_empty() || bound.trait_args == trait_args)
        }) {
            return true;
        }
        if self.has_builtin_impl(ty, trait_id, trait_args) {
            return true;
        }
        if let Some(impls) = self.trait_impls.get(&trait_id)
            && impls.iter().any(|candidate| {
                self.impl_subst(candidate, ty, trait_args)
                    .map(|subst| {
                        self.bounds_satisfied_inner(
                            &candidate.bounds,
                            &subst,
                            assumptions,
                            depth + 1,
                        )
                    })
                    .unwrap_or(false)
            })
        {
            return true;
        }
        self.derive_impl_for_composite(ty, trait_id, trait_args, assumptions, depth + 1)
    }

    /// Convenience: `type_implements(ty, copy_trait_id)`.
    pub fn type_is_copy(&self, ty: &Type) -> bool {
        match self.copy_trait_id {
            Some(tid) => self.type_implements(ty, tid),
            None => self.builtin_copy_fallback(ty),
        }
    }

    pub fn associated_type(&self, ty: &Type, trait_id: TraitId, name: &str) -> Option<Type> {
        self.associated_type_with_args(ty, trait_id, &[], name)
    }

    pub fn associated_type_with_args(
        &self,
        ty: &Type,
        trait_id: TraitId,
        trait_args: &[Type],
        name: &str,
    ) -> Option<Type> {
        self.trait_impls.get(&trait_id)?.iter().find_map(|imp| {
            let subst = self.impl_subst(imp, ty, trait_args)?;
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
        trait_args: Vec<Type>,
        bounds: Vec<TraitBound>,
        assoc_types: HashMap<String, Type>,
    ) {
        self.trait_impls
            .entry(trait_id)
            .or_default()
            .push(TraitImpl {
                self_ty,
                trait_args,
                bounds,
                assoc_types,
            });
    }

    pub(crate) fn set_copy_trait(&mut self, id: TraitId) {
        self.copy_trait_id = Some(id);
    }

    pub(crate) fn set_composite_trait(&mut self, id: TraitId, lang: &str, generic_count: usize) {
        let kind = match lang {
            "copy" => CompositeTraitKind::Copy,
            "partial_eq" => CompositeTraitKind::PartialEq(generic_count != 0),
            "eq" => CompositeTraitKind::Eq,
            "partial_ord" => CompositeTraitKind::PartialOrd(generic_count != 0),
            "ord" => CompositeTraitKind::Ord,
            _ => return,
        };
        self.composite_traits.insert(id, kind);
    }

    fn has_builtin_impl(&self, ty: &Type, trait_id: TraitId, trait_args: &[Type]) -> bool {
        if Some(trait_id) == self.copy_trait_id {
            return ty.is_fundamentally_copy();
        }
        match self.composite_traits.get(&trait_id) {
            Some(CompositeTraitKind::PartialEq(has_rhs)) => {
                ((!has_rhs && trait_args.is_empty()) || (*has_rhs && trait_args.len() <= 1))
                    && builtin_partial_eq(ty, trait_args.first().unwrap_or(ty))
            }
            Some(CompositeTraitKind::PartialOrd(has_rhs)) => {
                ((!has_rhs && trait_args.is_empty()) || (*has_rhs && trait_args.len() <= 1))
                    && builtin_partial_ord(ty, trait_args.first().unwrap_or(ty))
            }
            Some(CompositeTraitKind::Eq) => trait_args.is_empty() && builtin_eq(ty),
            Some(CompositeTraitKind::Ord) => trait_args.is_empty() && builtin_ord(ty),
            Some(CompositeTraitKind::Copy) | None => false,
        }
    }

    fn derive_impl_for_composite(
        &self,
        ty: &Type,
        trait_id: TraitId,
        trait_args: &[Type],
        assumptions: &[TraitBound],
        depth: usize,
    ) -> bool {
        let Some(kind) = self.composite_traits.get(&trait_id) else {
            return false;
        };
        match kind {
            CompositeTraitKind::Copy | CompositeTraitKind::Eq | CompositeTraitKind::Ord => {
                trait_args.is_empty()
                    && self.derive_same_composite(ty, trait_id, assumptions, depth)
            }
            CompositeTraitKind::PartialEq(false) | CompositeTraitKind::PartialOrd(false) => {
                trait_args.is_empty()
                    && self.derive_same_composite(ty, trait_id, assumptions, depth)
            }
            CompositeTraitKind::PartialEq(true) | CompositeTraitKind::PartialOrd(true) => {
                let rhs = trait_args.first().unwrap_or(ty);
                trait_args.len() <= 1
                    && self.derive_binary_composite(ty, rhs, trait_id, assumptions, depth)
            }
        }
    }

    fn derive_same_composite(
        &self,
        ty: &Type,
        trait_id: TraitId,
        assumptions: &[TraitBound],
        depth: usize,
    ) -> bool {
        match ty {
            Type::Tuple(elements) => elements
                .iter()
                .all(|elem| self.type_implements_inner(elem, trait_id, &[], assumptions, depth)),
            Type::Array(inner, _) => {
                self.type_implements_inner(inner, trait_id, &[], assumptions, depth)
            }
            _ => false,
        }
    }

    fn derive_binary_composite(
        &self,
        lhs: &Type,
        rhs: &Type,
        trait_id: TraitId,
        assumptions: &[TraitBound],
        depth: usize,
    ) -> bool {
        match (lhs, rhs) {
            (Type::Tuple(lhs), Type::Tuple(rhs)) if lhs.len() == rhs.len() => {
                lhs.iter().zip(rhs).all(|(lhs, rhs)| {
                    self.type_implements_inner(
                        lhs,
                        trait_id,
                        std::slice::from_ref(rhs),
                        assumptions,
                        depth,
                    )
                })
            }
            (Type::Array(lhs, lhs_len), Type::Array(rhs, rhs_len)) if lhs_len == rhs_len => self
                .type_implements_inner(
                    lhs,
                    trait_id,
                    std::slice::from_ref(rhs.as_ref()),
                    assumptions,
                    depth,
                ),
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

    fn impl_subst(
        &self,
        imp: &TraitImpl,
        actual: &Type,
        trait_args: &[Type],
    ) -> Option<HashMap<String, Type>> {
        let mut subst = HashMap::new();
        if !crate::lowering::collect_subst(&imp.self_ty, actual, &mut subst) {
            return None;
        }
        if !trait_args.is_empty()
            && (imp.trait_args.len() != trait_args.len()
                || !imp
                    .trait_args
                    .iter()
                    .zip(trait_args)
                    .all(|(expected, actual)| {
                        crate::lowering::collect_subst(expected, actual, &mut subst)
                    }))
        {
            return None;
        }
        Some(subst)
    }

    fn bounds_satisfied(&self, bounds: &[TraitBound], subst: &HashMap<String, Type>) -> bool {
        self.bounds_satisfied_inner(bounds, subst, &[], 0)
    }

    fn bounds_satisfied_inner(
        &self,
        bounds: &[TraitBound],
        subst: &HashMap<String, Type>,
        assumptions: &[TraitBound],
        depth: usize,
    ) -> bool {
        bounds.iter().all(|bound| {
            let actual = substitute_type(&bound.ty, subst);
            let trait_args = bound
                .trait_args
                .iter()
                .map(|arg| substitute_type(arg, subst))
                .collect::<Vec<_>>();
            if !self.type_implements_inner(
                &actual,
                bound.trait_id,
                &trait_args,
                assumptions,
                depth + 1,
            ) {
                return false;
            }
            bound.assoc_constraints.iter().all(|constraint| {
                let expected = substitute_type(&constraint.ty, subst);
                self.associated_type_with_args(
                    &actual,
                    bound.trait_id,
                    &trait_args,
                    &constraint.name,
                )
                .map(|actual| {
                    actual.is_unknown_like() || expected.is_unknown_like() || actual == expected
                })
                .unwrap_or(false)
            })
        })
    }
}

fn builtin_partial_eq(lhs: &Type, rhs: &Type) -> bool {
    match (lhs, rhs) {
        (Type::Int(lhs), Type::Int(rhs)) => lhs == rhs,
        (Type::Float(lhs), Type::Float(rhs)) => lhs == rhs,
        (Type::InferInt, Type::InferInt) | (Type::InferFloat, Type::InferFloat) => true,
        (Type::Bool, Type::Bool) | (Type::Char, Type::Char) | (Type::Unit, Type::Unit) => true,
        (Type::Ref(lhs, false), Type::Ref(rhs, false)) => {
            matches!(lhs.as_ref(), Type::Str) && matches!(rhs.as_ref(), Type::Str)
        }
        _ => false,
    }
}

fn builtin_partial_ord(lhs: &Type, rhs: &Type) -> bool {
    match (lhs, rhs) {
        (Type::Int(lhs), Type::Int(rhs)) => lhs == rhs,
        (Type::Float(lhs), Type::Float(rhs)) => lhs == rhs,
        (Type::InferInt, Type::InferInt) | (Type::InferFloat, Type::InferFloat) => true,
        (Type::Char, Type::Char) => true,
        _ => false,
    }
}

fn builtin_eq(ty: &Type) -> bool {
    match ty {
        Type::Int(_) | Type::Bool | Type::Char | Type::Unit => true,
        Type::Ref(inner, false) => matches!(inner.as_ref(), Type::Str),
        _ => false,
    }
}

fn builtin_ord(ty: &Type) -> bool {
    matches!(ty, Type::Int(_) | Type::Bool | Type::Char)
}
