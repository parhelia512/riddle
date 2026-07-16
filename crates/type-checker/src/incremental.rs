use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
};

use frontend::syntax_kind::SyntaxNode;
use hir::{
    HirFile,
    body::{Body, BodyId, ExprId},
    item_tree::{
        FunctionId, HirAttr, HirConst, HirEnum, HirEnumVariant, HirFunction, HirGenericBound,
        HirImpl, HirModule, HirStruct, HirStructField, HirTrait, HirTypeAlias, HirUse, HirUseTree,
        HirUseTreeKind, HirVariantKind, ItemTree,
    },
};
use rowan::{TextRange, TextSize};

use crate::{
    TypeCheckResult,
    checker::{GenericEdge, TypeChecker},
    result::{
        ClosureKind, Diagnostic, ForLoopInfo, GenericCall, LambdaInfo, OperatorCall,
        TraitMethodCall,
    },
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
    bodies: HashMap<FunctionId, CachedBody>,
    globals: Option<CachedGlobals>,
    last_source: Option<String>,
}

#[derive(Debug, Clone)]
struct CachedGlobals {
    type_context_hash: u64,
    value_diagnostics: Vec<Diagnostic>,
    impl_diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Copy)]
struct TextEdit {
    old_range: TextRange,
    insert_len: TextSize,
}

#[derive(Debug, Clone)]
struct CachedBody {
    type_context_hash: u64,
    body_hash: u64,
    diagnostics: Vec<Diagnostic>,
    expr_types: Vec<(ExprId, Type)>,
    generic_calls: Vec<(ExprId, GenericCall)>,
    trait_method_calls: Vec<(ExprId, TraitMethodCall)>,
    operator_calls: Vec<(ExprId, OperatorCall)>,
    for_loops: Vec<(ExprId, ForLoopInfo)>,
    lambda_infos: Vec<(ExprId, LambdaInfo)>,
    closure_kinds: Vec<(ExprId, ClosureKind)>,
    generic_edges: Vec<GenericEdge>,
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
        self.globals = None;
        self.last_source = None;
        self.check_inner(
            hir,
            body_fingerprint,
            None,
            type_context_fingerprint_with_ranges(&hir.item_tree),
        )
    }

    pub fn check_with_syntax(
        &mut self,
        hir: &HirFile,
        syntax: &SyntaxNode,
    ) -> IncrementalTypeCheckResult {
        let source = syntax.text().to_string();
        let edit = self
            .last_source
            .as_deref()
            .map(|old_source| text_edit(old_source, &source));
        let result = self.check_inner(
            hir,
            |body| syntax_body_fingerprint(body, syntax),
            edit,
            type_context_fingerprint(&hir.item_tree),
        );
        self.last_source = Some(source);
        result
    }

    fn check_inner(
        &mut self,
        hir: &HirFile,
        body_fingerprint: impl Fn(&Body) -> u64,
        edit: Option<TextEdit>,
        type_context_hash: u64,
    ) -> IncrementalTypeCheckResult {
        let mut checker = TypeChecker::new(hir);
        let mut stats = IncrementalStats::default();
        let mut live_functions = HashSet::new();

        let cached_globals = self
            .globals
            .as_ref()
            .filter(|cached| cached.type_context_hash == type_context_hash)
            .and_then(|cached| shift_cached_globals(cached, edit));
        let value_diagnostics = if let Some(cached) = &cached_globals {
            checker
                .result
                .diagnostics
                .extend(cached.value_diagnostics.iter().cloned());
            cached.value_diagnostics.clone()
        } else {
            let start = checker.result.diagnostics.len();
            checker.check_value_type_declarations();
            checker.result.diagnostics[start..].to_vec()
        };
        checker.check_type_layouts();
        checker.check_traits();
        let impl_diagnostics = if let Some(cached) = &cached_globals {
            checker
                .result
                .diagnostics
                .extend(cached.impl_diagnostics.iter().cloned());
            cached.impl_diagnostics.clone()
        } else {
            let start = checker.result.diagnostics.len();
            checker.check_impls();
            checker.result.diagnostics[start..].to_vec()
        };
        checker.build_trait_env();
        checker.validate_copy_impls();
        self.globals = Some(CachedGlobals {
            type_context_hash,
            value_diagnostics,
            impl_diagnostics,
        });

        for (fid, function) in hir.item_tree.functions.iter() {
            let Some(body_id) = hir.function_bodies.get(&fid).copied() else {
                continue;
            };

            let body = &hir.bodies[body_id];
            let body_hash = body_fingerprint(body);
            live_functions.insert(fid);

            if let Some(cached) = self.bodies.get(&fid).filter(|cached| {
                cached.type_context_hash == type_context_hash && cached.body_hash == body_hash
            }) && replay_cached_body(&mut checker, body_id, cached, edit)
            {
                stats.reused_bodies += 1;
                continue;
            }

            stats.checked_bodies += 1;
            let diagnostic_start = checker.result.diagnostics.len();
            let generic_edge_start = checker.generic_edges.len();
            checker.check_function(
                fid,
                function,
                body_id,
                checker.impl_generic_names(fid),
                checker.impl_const_generic_names(fid),
            );
            let diagnostics = checker.result.diagnostics[diagnostic_start..].to_vec();
            let generic_edges = checker.generic_edges[generic_edge_start..].to_vec();
            let expr_types = checker
                .result
                .expr_types
                .iter()
                .filter_map(|((checked_body, expr), ty)| {
                    if *checked_body == body_id {
                        Some((*expr, ty.clone()))
                    } else {
                        None
                    }
                })
                .collect();
            let generic_calls = checker
                .result
                .generic_calls
                .iter()
                .filter_map(|((checked_body, expr), call)| {
                    if *checked_body == body_id {
                        Some((*expr, call.clone()))
                    } else {
                        None
                    }
                })
                .collect();
            let trait_method_calls = checker
                .result
                .trait_method_calls
                .iter()
                .filter_map(|((checked_body, expr), call)| {
                    if *checked_body == body_id {
                        Some((*expr, call.clone()))
                    } else {
                        None
                    }
                })
                .collect();
            let operator_calls = checker
                .result
                .operator_calls
                .iter()
                .filter_map(|((checked_body, expr), call)| {
                    if *checked_body == body_id {
                        Some((*expr, call.clone()))
                    } else {
                        None
                    }
                })
                .collect();
            let for_loops = checker
                .result
                .for_loops
                .iter()
                .filter_map(|((checked_body, expr), info)| {
                    if *checked_body == body_id {
                        Some((*expr, info.clone()))
                    } else {
                        None
                    }
                })
                .collect();
            let lambda_infos = checker
                .result
                .lambda_infos
                .iter()
                .filter(|((checked_body, _), _)| *checked_body == body_id)
                .map(|((_, expr), info)| (*expr, info.clone()))
                .collect();
            let closure_kinds = checker
                .result
                .closure_kinds
                .iter()
                .filter(|((checked_body, _), _)| *checked_body == body_id)
                .map(|((_, expr), kind)| (*expr, *kind))
                .collect();

            self.bodies.insert(
                fid,
                CachedBody {
                    type_context_hash,
                    body_hash,
                    diagnostics,
                    expr_types,
                    generic_calls,
                    trait_method_calls,
                    operator_calls,
                    for_loops,
                    lambda_infos,
                    closure_kinds,
                    generic_edges,
                },
            );
        }

        self.bodies
            .retain(|function, _| live_functions.contains(function));
        checker.check_generic_recursion();

        IncrementalTypeCheckResult {
            result: checker.result,
            stats,
        }
    }
}

fn replay_cached_body(
    checker: &mut TypeChecker<'_>,
    body_id: BodyId,
    cached: &CachedBody,
    edit: Option<TextEdit>,
) -> bool {
    let diagnostics = match edit {
        Some(edit) => match shift_diagnostics(&cached.diagnostics, edit) {
            Some(diagnostics) => diagnostics,
            None => return false,
        },
        None => cached.diagnostics.clone(),
    };
    let generic_edges = match edit {
        Some(edit) => match shift_generic_edges(&cached.generic_edges, edit) {
            Some(edges) => edges,
            None => return false,
        },
        None => cached.generic_edges.clone(),
    };
    checker.result.diagnostics.extend(diagnostics);
    for (expr, ty) in &cached.expr_types {
        checker
            .result
            .expr_types
            .insert((body_id, *expr), ty.clone());
    }
    for (expr, call) in &cached.generic_calls {
        checker
            .result
            .generic_calls
            .insert((body_id, *expr), call.clone());
    }
    for (expr, call) in &cached.trait_method_calls {
        checker
            .result
            .trait_method_calls
            .insert((body_id, *expr), call.clone());
    }
    for (expr, call) in &cached.operator_calls {
        checker
            .result
            .operator_calls
            .insert((body_id, *expr), call.clone());
    }
    for (expr, info) in &cached.for_loops {
        checker
            .result
            .for_loops
            .insert((body_id, *expr), info.clone());
    }
    for (expr, info) in &cached.lambda_infos {
        checker
            .result
            .lambda_infos
            .insert((body_id, *expr), info.clone());
    }
    for (expr, kind) in &cached.closure_kinds {
        checker.result.closure_kinds.insert((body_id, *expr), *kind);
    }
    checker.generic_edges.extend(generic_edges);
    true
}

fn shift_range(range: TextRange, offset: i64) -> TextRange {
    let shift = |position: TextSize| {
        let shifted = i64::from(u32::from(position)) + offset;
        debug_assert!((0..=i64::from(u32::MAX)).contains(&shifted));
        TextSize::from(shifted as u32)
    };
    TextRange::new(shift(range.start()), shift(range.end()))
}

fn shift_cached_globals(cached: &CachedGlobals, edit: Option<TextEdit>) -> Option<CachedGlobals> {
    let Some(edit) = edit else {
        return Some(cached.clone());
    };
    Some(CachedGlobals {
        type_context_hash: cached.type_context_hash,
        value_diagnostics: shift_diagnostics(&cached.value_diagnostics, edit)?,
        impl_diagnostics: shift_diagnostics(&cached.impl_diagnostics, edit)?,
    })
}

fn shift_diagnostics(diagnostics: &[Diagnostic], edit: TextEdit) -> Option<Vec<Diagnostic>> {
    diagnostics
        .iter()
        .cloned()
        .map(|mut diagnostic| {
            for label in &mut diagnostic.labels {
                label.range = shift_range_for_edit(label.range, edit)?;
            }
            Some(diagnostic)
        })
        .collect()
}

fn shift_generic_edges(edges: &[GenericEdge], edit: TextEdit) -> Option<Vec<GenericEdge>> {
    edges
        .iter()
        .cloned()
        .map(|mut edge| {
            edge.span = match edge.span {
                Some(range) => Some(shift_range_for_edit(range, edit)?),
                None => None,
            };
            Some(edge)
        })
        .collect()
}

fn shift_range_for_edit(range: TextRange, edit: TextEdit) -> Option<TextRange> {
    if range.end() <= edit.old_range.start() {
        return Some(range);
    }
    if range.start() < edit.old_range.end() {
        return None;
    }
    let offset = i64::from(u32::from(edit.insert_len)) - i64::from(u32::from(edit.old_range.len()));
    Some(shift_range(range, offset))
}

fn text_edit(old: &str, new: &str) -> TextEdit {
    let mut prefix = 0;
    for (old_char, new_char) in old.chars().zip(new.chars()) {
        if old_char != new_char {
            break;
        }
        prefix += old_char.len_utf8();
    }

    let mut suffix = 0;
    for (old_char, new_char) in old[prefix..].chars().rev().zip(new[prefix..].chars().rev()) {
        if old_char != new_char {
            break;
        }
        suffix += old_char.len_utf8();
    }

    TextEdit {
        old_range: TextRange::new(
            TextSize::from(prefix as u32),
            TextSize::from((old.len() - suffix) as u32),
        ),
        insert_len: TextSize::from((new.len() - prefix - suffix) as u32),
    }
}

fn body_fingerprint(body: &Body) -> u64 {
    let mut hasher = DefaultHasher::new();
    format!("{:?}", body.exprs).hash(&mut hasher);
    format!("{:?}", body.stmts).hash(&mut hasher);
    format!("{:?}", body.pats).hash(&mut hasher);
    body.root_ptr.text_range().hash(&mut hasher);
    for (expr, _) in body.exprs.iter() {
        body.source_map.expr_ranges.get(&expr).hash(&mut hasher);
    }
    for (stmt, _) in body.stmts.iter() {
        body.source_map.stmt_ranges.get(&stmt).hash(&mut hasher);
    }
    for (pat, _) in body.pats.iter() {
        body.source_map.pat_ranges.get(&pat).hash(&mut hasher);
    }
    hasher.finish()
}

fn syntax_body_fingerprint(body: &Body, syntax: &SyntaxNode) -> u64 {
    let mut hasher = DefaultHasher::new();
    body.root_ptr
        .to_node(syntax)
        .green()
        .into_owned()
        .hash(&mut hasher);
    hasher.finish()
}

fn type_context_fingerprint(tree: &ItemTree) -> u64 {
    let mut hasher = DefaultHasher::new();
    tree.functions.len().hash(&mut hasher);
    for (_, function) in tree.functions.iter() {
        hash_function(function, &mut hasher);
    }
    tree.structs.len().hash(&mut hasher);
    for (_, strukt) in tree.structs.iter() {
        hash_struct(strukt, &mut hasher);
    }
    tree.modules.len().hash(&mut hasher);
    for (_, module) in tree.modules.iter() {
        hash_module(module, &mut hasher);
    }
    tree.uses.len().hash(&mut hasher);
    for (_, use_item) in tree.uses.iter() {
        hash_use(use_item, &mut hasher);
    }
    tree.enums.len().hash(&mut hasher);
    for (_, enumeration) in tree.enums.iter() {
        hash_enum(enumeration, &mut hasher);
    }
    tree.traits.len().hash(&mut hasher);
    for (_, trait_item) in tree.traits.iter() {
        hash_trait(trait_item, &mut hasher);
    }
    tree.impls.len().hash(&mut hasher);
    for (_, impl_item) in tree.impls.iter() {
        hash_impl(impl_item, &mut hasher);
    }
    tree.consts.len().hash(&mut hasher);
    for (_, const_item) in tree.consts.iter() {
        hash_const(const_item, &mut hasher);
    }
    tree.type_aliases.len().hash(&mut hasher);
    for (_, alias) in tree.type_aliases.iter() {
        hash_type_alias(alias, &mut hasher);
    }
    format!("{:?}", tree.top_level).hash(&mut hasher);
    format!("{:?}", tree.extern_function_ids).hash(&mut hasher);
    hasher.finish()
}

fn type_context_fingerprint_with_ranges(tree: &ItemTree) -> u64 {
    let mut hasher = DefaultHasher::new();
    type_context_fingerprint(tree).hash(&mut hasher);
    format!("{tree:?}").hash(&mut hasher);
    hasher.finish()
}

fn hash_function(function: &HirFunction, hasher: &mut impl Hasher) {
    function.name.hash(hasher);
    function.visibility.is_public().hash(hasher);
    function.generics.hash(hasher);
    function.const_generics.hash(hasher);
    hash_bounds(&function.generic_bounds, hasher);
    function.params.len().hash(hasher);
    for param in &function.params {
        param.name.hash(hasher);
        param.ty.hash(hasher);
        hash_attrs(&param.attrs, hasher);
    }
    function.ret_type.hash(hasher);
    function.has_body.hash(hasher);
    hash_attrs(&function.attrs, hasher);
}

fn hash_struct(strukt: &HirStruct, hasher: &mut impl Hasher) {
    strukt.name.hash(hasher);
    strukt.visibility.is_public().hash(hasher);
    strukt.generics.hash(hasher);
    strukt.const_generics.hash(hasher);
    hash_bounds(&strukt.generic_bounds, hasher);
    hash_fields(&strukt.fields, hasher);
    hash_attrs(&strukt.attrs, hasher);
}

fn hash_fields(fields: &[HirStructField], hasher: &mut impl Hasher) {
    fields.len().hash(hasher);
    for field in fields {
        field.name.hash(hasher);
        field.ty.hash(hasher);
        hash_attrs(&field.attrs, hasher);
    }
}

fn hash_module(module: &HirModule, hasher: &mut impl Hasher) {
    module.name.hash(hasher);
    module.visibility.is_public().hash(hasher);
    format!("{:?}", module.items).hash(hasher);
    hash_attrs(&module.attrs, hasher);
}

fn hash_use(use_item: &HirUse, hasher: &mut impl Hasher) {
    hash_use_tree(&use_item.tree, hasher);
    use_item.visibility.is_public().hash(hasher);
    hash_attrs(&use_item.attrs, hasher);
}

fn hash_use_tree(tree: &HirUseTree, hasher: &mut impl Hasher) {
    tree.prefix.hash(hasher);
    match &tree.kind {
        HirUseTreeKind::Simple { alias } => {
            0u8.hash(hasher);
            alias.hash(hasher);
        }
        HirUseTreeKind::Glob => 1u8.hash(hasher),
        HirUseTreeKind::List(children) => {
            2u8.hash(hasher);
            children.len().hash(hasher);
            for child in children {
                hash_use_tree(child, hasher);
            }
        }
    }
}

fn hash_enum(enumeration: &HirEnum, hasher: &mut impl Hasher) {
    enumeration.name.hash(hasher);
    enumeration.visibility.is_public().hash(hasher);
    enumeration.generics.hash(hasher);
    enumeration.const_generics.hash(hasher);
    hash_bounds(&enumeration.generic_bounds, hasher);
    enumeration.variants.len().hash(hasher);
    for variant in &enumeration.variants {
        hash_variant(variant, hasher);
    }
    hash_attrs(&enumeration.attrs, hasher);
}

fn hash_variant(variant: &HirEnumVariant, hasher: &mut impl Hasher) {
    variant.name.hash(hasher);
    match &variant.kind {
        HirVariantKind::Unit => 0u8.hash(hasher),
        HirVariantKind::Tuple(types) => {
            1u8.hash(hasher);
            types.hash(hasher);
        }
        HirVariantKind::Struct(fields) => {
            2u8.hash(hasher);
            hash_fields(fields, hasher);
        }
    }
    hash_attrs(&variant.attrs, hasher);
}

fn hash_trait(trait_item: &HirTrait, hasher: &mut impl Hasher) {
    trait_item.name.hash(hasher);
    trait_item.visibility.is_public().hash(hasher);
    trait_item.methods.len().hash(hasher);
    for method in &trait_item.methods {
        hash_function(method, hasher);
    }
    trait_item.type_aliases.len().hash(hasher);
    for alias in &trait_item.type_aliases {
        hash_type_alias(alias, hasher);
    }
    hash_attrs(&trait_item.attrs, hasher);
}

fn hash_impl(impl_item: &HirImpl, hasher: &mut impl Hasher) {
    impl_item.self_ty.hash(hasher);
    impl_item.trait_ty.hash(hasher);
    impl_item.generics.hash(hasher);
    impl_item.const_generics.hash(hasher);
    hash_bounds(&impl_item.generic_bounds, hasher);
    format!("{:?}", impl_item.methods).hash(hasher);
    format!("{:?}", impl_item.consts).hash(hasher);
    format!("{:?}", impl_item.type_aliases).hash(hasher);
    hash_attrs(&impl_item.attrs, hasher);
}

fn hash_const(const_item: &HirConst, hasher: &mut impl Hasher) {
    const_item.name.hash(hasher);
    const_item.visibility.is_public().hash(hasher);
    const_item.ty.hash(hasher);
    const_item.has_value.hash(hasher);
    hash_attrs(&const_item.attrs, hasher);
}

fn hash_type_alias(alias: &HirTypeAlias, hasher: &mut impl Hasher) {
    alias.name.hash(hasher);
    alias.visibility.is_public().hash(hasher);
    alias.ty.hash(hasher);
    hash_attrs(&alias.attrs, hasher);
}

fn hash_bounds(bounds: &[HirGenericBound], hasher: &mut impl Hasher) {
    bounds.len().hash(hasher);
    for bound in bounds {
        bound.param.hash(hasher);
        bound.target_ty.hash(hasher);
        bound.trait_ty.hash(hasher);
        bound.assoc_constraints.len().hash(hasher);
        for constraint in &bound.assoc_constraints {
            constraint.name.hash(hasher);
            constraint.ty.hash(hasher);
        }
    }
}

fn hash_attrs(attrs: &[HirAttr], hasher: &mut impl Hasher) {
    attrs.len().hash(hasher);
    for attr in attrs {
        attr.name.hash(hasher);
        attr.value.hash(hasher);
        attr.raw.hash(hasher);
    }
}
