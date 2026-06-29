use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    fmt::Debug,
    hash::{Hash, Hasher},
};

use frontend::syntax_kind::RiddleLang;
use hir::{
    HirFile,
    body::{Body, BodyId, ExprId},
};
use rowan::ast::SyntaxNodePtr;

use crate::{
    TypeCheckResult,
    checker::TypeChecker,
    result::{Diagnostic, GenericCall},
    types::Type,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IncrementalStats {
    pub checked_bodies: usize,
    pub reused_bodies: usize,
}

#[derive(Debug, Clone)]
pub struct IncrementalTypeCheckResult {
    pub result: TypeCheckResult,
    pub stats: IncrementalStats,
}

#[derive(Debug, Default)]
pub struct IncrementalTypeChecker {
    bodies: HashMap<SyntaxNodePtr<RiddleLang>, CachedBody>,
}

#[derive(Debug, Clone)]
struct CachedBody {
    type_context_hash: u64,
    body_hash: u64,
    diagnostics: Vec<Diagnostic>,
    expr_types: Vec<(ExprId, Type)>,
    generic_calls: Vec<(ExprId, GenericCall)>,
}

pub fn check_hir_incremental(
    hir: &HirFile,
    checker: &mut IncrementalTypeChecker,
) -> IncrementalTypeCheckResult {
    checker.check(hir)
}

impl IncrementalTypeChecker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn check(&mut self, hir: &HirFile) -> IncrementalTypeCheckResult {
        let type_context_hash = fingerprint(&hir.item_tree);
        let mut checker = TypeChecker::new(hir);
        let mut stats = IncrementalStats::default();
        let mut live_ptrs = HashSet::new();

        checker.check_traits();
        checker.check_impls();
        checker.build_trait_env();

        for (fid, function) in hir.item_tree.functions.iter() {
            let Some(body_id) = hir.function_bodies.get(&fid).copied() else {
                continue;
            };

            let body = &hir.bodies[body_id];
            let ptr = body.root_ptr.clone();
            let body_hash = body_fingerprint(body);
            live_ptrs.insert(ptr.clone());

            if let Some(cached) = self.bodies.get(&ptr).filter(|cached| {
                cached.type_context_hash == type_context_hash && cached.body_hash == body_hash
            }) {
                stats.reused_bodies += 1;
                replay_cached_body(&mut checker.result, body_id, cached);
                continue;
            }

            stats.checked_bodies += 1;
            let diagnostic_start = checker.result.diagnostics.len();
            checker.check_function(fid, function, body_id);
            let diagnostics = checker.result.diagnostics[diagnostic_start..].to_vec();
            let expr_types = checker
                .result
                .expr_types
                .iter()
                .filter_map(|((checked_body, expr), ty)| {
                    (*checked_body == body_id).then(|| (*expr, ty.clone()))
                })
                .collect();
            let generic_calls = checker
                .result
                .generic_calls
                .iter()
                .filter_map(|((checked_body, expr), call)| {
                    (*checked_body == body_id).then(|| (*expr, call.clone()))
                })
                .collect();

            self.bodies.insert(
                ptr,
                CachedBody {
                    type_context_hash,
                    body_hash,
                    diagnostics,
                    expr_types,
                    generic_calls,
                },
            );
        }

        self.bodies.retain(|ptr, _| live_ptrs.contains(ptr));

        IncrementalTypeCheckResult {
            result: checker.result,
            stats,
        }
    }
}

fn replay_cached_body(result: &mut TypeCheckResult, body_id: BodyId, cached: &CachedBody) {
    result
        .diagnostics
        .extend(cached.diagnostics.iter().cloned());
    for (expr, ty) in &cached.expr_types {
        result.expr_types.insert((body_id, *expr), ty.clone());
    }
    for (expr, call) in &cached.generic_calls {
        result.generic_calls.insert((body_id, *expr), call.clone());
    }
}

fn body_fingerprint(body: &Body) -> u64 {
    let mut hasher = DefaultHasher::new();
    format!("{:?}", body.exprs).hash(&mut hasher);
    format!("{:?}", body.stmts).hash(&mut hasher);
    format!("{:?}", body.pats).hash(&mut hasher);
    hasher.finish()
}

fn fingerprint(value: &impl Debug) -> u64 {
    let mut hasher = DefaultHasher::new();
    format!("{value:?}").hash(&mut hasher);
    hasher.finish()
}
