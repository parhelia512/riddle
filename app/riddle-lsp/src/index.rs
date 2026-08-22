use std::{
    collections::{BTreeSet, HashMap},
    hash::BuildHasher,
    path::PathBuf,
};

use lsp_types::{Location, Range, SymbolInformation, SymbolKind};
use riddlec::pipeline::CompileOptions;
use serde::{Deserialize, Serialize};

use crate::{
    analysis::{AnalysisDepth, DocumentAnalysis, analyze_document_cancellable},
    server::Document,
    session::AnalysisSessions,
    text::{LineIndex, normalized_path},
};
use hir::{
    body::{Expr, ResolvedName},
    item_tree::HirTypeRef,
};
use type_checker::Type;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum IndexedSymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    TypeAlias,
    Const,
    Module,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SymbolKey {
    pub project: PathBuf,
    pub source: PathBuf,
    pub start: u32,
    pub end: u32,
    pub kind: IndexedSymbolKind,
}

#[derive(Debug, Clone)]
pub struct IndexedSymbol {
    pub key: SymbolKey,
    pub name: String,
    pub detail: String,
    pub location: Location,
}

#[derive(Debug, Clone)]
pub struct CallEdge {
    pub caller: SymbolKey,
    pub target: SymbolKey,
    pub sites: Vec<Range>,
}

#[derive(Debug, Clone, Default)]
pub struct TypeRelations {
    pub supertypes: HashMap<SymbolKey, Vec<SymbolKey>>,
    pub subtypes: HashMap<SymbolKey, Vec<SymbolKey>>,
}

#[derive(Debug, Clone)]
pub struct ProjectIndex {
    pub project: PathBuf,
    pub revision: u64,
    pub files: BTreeSet<PathBuf>,
    pub symbols: Vec<IndexedSymbol>,
    pub calls: Vec<CallEdge>,
    pub types: TypeRelations,
}

#[allow(deprecated)]
#[must_use]
pub fn workspace_symbols_for_index(index: &ProjectIndex, query: &str) -> Vec<SymbolInformation> {
    let query = query.to_lowercase();
    index
        .symbols
        .iter()
        .filter(|symbol| symbol.name.to_lowercase().contains(&query))
        .map(|symbol| SymbolInformation {
            name: symbol.name.clone(),
            kind: match symbol.key.kind {
                IndexedSymbolKind::Function => SymbolKind::FUNCTION,
                IndexedSymbolKind::Struct => SymbolKind::STRUCT,
                IndexedSymbolKind::Enum => SymbolKind::ENUM,
                IndexedSymbolKind::Trait => SymbolKind::INTERFACE,
                IndexedSymbolKind::TypeAlias => SymbolKind::TYPE_PARAMETER,
                IndexedSymbolKind::Const => SymbolKind::CONSTANT,
                IndexedSymbolKind::Module => SymbolKind::MODULE,
            },
            tags: None,
            deprecated: None,
            location: symbol.location.clone(),
            container_name: None,
        })
        .collect()
}

impl ProjectIndex {
    pub(crate) fn from_analysis(analysis: &DocumentAnalysis) -> Option<Self> {
        let project = analysis.project_root.clone()?;
        let hir = analysis.result.hir.as_ref()?;
        let symbols = collect_symbols(analysis, hir);

        let mut files = analysis
            .files
            .iter()
            .cloned()
            .map(normalized_path)
            .collect::<BTreeSet<_>>();
        files.insert(normalized_path(project.join(clue::CLUE_PROJECT_FILE_NAME)));
        let calls = build_call_edges(analysis, hir, &symbols);
        let types = build_type_relations(analysis, hir, &symbols);
        Some(Self {
            project,
            revision: analysis.project_revision,
            files,
            symbols,
            calls,
            types,
        })
    }

    #[cfg(feature = "test")]
    #[must_use]
    pub fn empty(
        project: PathBuf,
        revision: u64,
        files: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        Self {
            project,
            revision,
            files: files.into_iter().collect(),
            symbols: Vec::new(),
            calls: Vec::new(),
            types: TypeRelations::default(),
        }
    }
}

fn collect_symbols(analysis: &DocumentAnalysis, hir: &hir::HirFile) -> Vec<IndexedSymbol> {
    let mut symbols = Vec::new();
    for (_, item) in hir.item_tree.functions.iter() {
        push_symbol(
            &mut symbols,
            analysis,
            IndexedSymbolKind::Function,
            &item.name.0,
            format!("fun {}", item.name.0),
            item.name_range,
            item.visibility.is_public(),
        );
    }
    for (_, item) in hir.item_tree.structs.iter() {
        push_symbol(
            &mut symbols,
            analysis,
            IndexedSymbolKind::Struct,
            &item.name.0,
            format!("struct {}", item.name.0),
            item.name_range,
            item.visibility.is_public(),
        );
    }
    for (_, item) in hir.item_tree.enums.iter() {
        push_symbol(
            &mut symbols,
            analysis,
            IndexedSymbolKind::Enum,
            &item.name.0,
            format!("enum {}", item.name.0),
            item.name_range,
            item.visibility.is_public(),
        );
    }
    for (_, item) in hir.item_tree.traits.iter() {
        push_symbol(
            &mut symbols,
            analysis,
            IndexedSymbolKind::Trait,
            &item.name.0,
            format!("trait {}", item.name.0),
            item.name_range,
            item.visibility.is_public(),
        );
        for method in &item.methods {
            push_symbol(
                &mut symbols,
                analysis,
                IndexedSymbolKind::Function,
                &method.name.0,
                format!("fun {}", method.name.0),
                method.name_range,
                item.visibility.is_public() && method.visibility.is_public(),
            );
        }
    }
    for (_, item) in hir.item_tree.consts.iter() {
        push_symbol(
            &mut symbols,
            analysis,
            IndexedSymbolKind::Const,
            &item.name.0,
            format!("const {}", item.name.0),
            item.name_range,
            item.visibility.is_public(),
        );
    }
    for (_, item) in hir.item_tree.type_aliases.iter() {
        push_symbol(
            &mut symbols,
            analysis,
            IndexedSymbolKind::TypeAlias,
            &item.name.0,
            format!("type {}", item.name.0),
            item.name_range,
            item.visibility.is_public(),
        );
    }
    for (_, item) in hir.item_tree.modules.iter() {
        push_symbol(
            &mut symbols,
            analysis,
            IndexedSymbolKind::Module,
            &item.name.0,
            format!("mod {}", item.name.0),
            item.name_range,
            item.visibility.is_public(),
        );
    }
    symbols.sort_by(|left, right| left.key.cmp(&right.key));
    symbols.dedup_by(|left, right| left.key == right.key);
    symbols
}

fn build_call_edges(
    analysis: &DocumentAnalysis,
    hir: &hir::HirFile,
    symbols: &[IndexedSymbol],
) -> Vec<CallEdge> {
    let mut grouped = HashMap::<(SymbolKey, SymbolKey), Vec<Range>>::new();
    for (function, body) in &hir.function_bodies {
        let Some(caller) = symbol_key_for_range(
            analysis,
            symbols,
            hir.item_tree.functions[*function].name_range,
            IndexedSymbolKind::Function,
        ) else {
            continue;
        };
        for (_, value) in hir.bodies[*body].exprs.iter() {
            let Expr::Call { callee, .. } = value else {
                continue;
            };
            let target_range = call_target_range(analysis, hir, *body, *callee);
            let Some(target) = target_range.and_then(|range| {
                symbol_key_for_range(analysis, symbols, range, IndexedSymbolKind::Function)
            }) else {
                continue;
            };
            let Some(site) = hir.bodies[*body]
                .source_map
                .expr_ranges
                .get(callee)
                .and_then(|range| mapped_lsp_range(analysis, *range))
            else {
                continue;
            };
            grouped
                .entry((caller.clone(), target))
                .or_default()
                .push(site);
        }
    }
    let mut calls = grouped
        .into_iter()
        .map(|((caller, target), mut sites)| {
            sites.sort_by_key(|range| {
                (
                    range.start.line,
                    range.start.character,
                    range.end.line,
                    range.end.character,
                )
            });
            sites.dedup();
            CallEdge {
                caller,
                target,
                sites,
            }
        })
        .collect::<Vec<_>>();
    calls.sort_by(|left, right| {
        left.caller
            .cmp(&right.caller)
            .then_with(|| left.target.cmp(&right.target))
    });
    calls
}

fn call_target_range(
    analysis: &DocumentAnalysis,
    hir: &hir::HirFile,
    body: hir::body::BodyId,
    callee: hir::body::ExprId,
) -> Option<rowan::TextRange> {
    analysis
        .result
        .type_result
        .trait_method_calls
        .get(&(body, callee))
        .map_or_else(
            || match analysis.result.type_result.expr_types.get(&(body, callee)) {
                Some(Type::FunctionItem { function, .. }) => {
                    Some(hir.item_tree.functions[*function].name_range)
                }
                _ => None,
            },
            |call| {
                hir.item_tree.traits[call.trait_id]
                    .methods
                    .iter()
                    .find(|method| method.name.0 == call.method)
                    .map(|method| method.name_range)
            },
        )
}

fn build_type_relations(
    analysis: &DocumentAnalysis,
    hir: &hir::HirFile,
    symbols: &[IndexedSymbol],
) -> TypeRelations {
    let mut supertypes = HashMap::<SymbolKey, BTreeSet<SymbolKey>>::new();
    let mut subtypes = HashMap::<SymbolKey, BTreeSet<SymbolKey>>::new();
    let mut connect = |subtype: SymbolKey, supertype: SymbolKey| {
        supertypes
            .entry(subtype.clone())
            .or_default()
            .insert(supertype.clone());
        subtypes.entry(supertype).or_default().insert(subtype);
    };

    for (_, tr) in hir.item_tree.traits.iter() {
        let Some(child) =
            symbol_key_for_range(analysis, symbols, tr.name_range, IndexedSymbolKind::Trait)
        else {
            continue;
        };
        for bound in &tr.supertraits {
            let Some(parent_range) = resolved_trait_range(hir, &bound.trait_ty) else {
                continue;
            };
            if let Some(parent) =
                symbol_key_for_range(analysis, symbols, parent_range, IndexedSymbolKind::Trait)
            {
                connect(child.clone(), parent);
            }
        }
    }
    for (_, implementation) in hir.item_tree.impls.iter() {
        let Some(trait_range) = implementation
            .trait_ty
            .as_ref()
            .and_then(|ty| resolved_trait_range(hir, ty))
        else {
            continue;
        };
        let Some(supertype) =
            symbol_key_for_range(analysis, symbols, trait_range, IndexedSymbolKind::Trait)
        else {
            continue;
        };
        let Some((kind, range)) = resolved_nominal_range(hir, &implementation.self_ty) else {
            continue;
        };
        if let Some(subtype) = symbol_key_for_range(analysis, symbols, range, kind) {
            connect(subtype, supertype);
        }
    }

    TypeRelations {
        supertypes: supertypes
            .into_iter()
            .map(|(key, values)| (key, values.into_iter().collect()))
            .collect(),
        subtypes: subtypes
            .into_iter()
            .map(|(key, values)| (key, values.into_iter().collect()))
            .collect(),
    }
}

fn resolved_trait_range(hir: &hir::HirFile, ty: &HirTypeRef) -> Option<rowan::TextRange> {
    let HirTypeRef::Named(path) = ty else {
        return None;
    };
    match hir.type_resolutions.get(&path.range) {
        Some(ResolvedName::Trait(id)) => Some(hir.item_tree.traits[*id].name_range),
        _ => None,
    }
}

fn resolved_nominal_range(
    hir: &hir::HirFile,
    ty: &HirTypeRef,
) -> Option<(IndexedSymbolKind, rowan::TextRange)> {
    let HirTypeRef::Named(path) = ty else {
        return None;
    };
    match hir.type_resolutions.get(&path.range) {
        Some(ResolvedName::Struct(id)) => Some((
            IndexedSymbolKind::Struct,
            hir.item_tree.structs[*id].name_range,
        )),
        Some(ResolvedName::Enum(id)) => {
            Some((IndexedSymbolKind::Enum, hir.item_tree.enums[*id].name_range))
        }
        _ => None,
    }
}

fn symbol_key_for_range(
    analysis: &DocumentAnalysis,
    symbols: &[IndexedSymbol],
    range: rowan::TextRange,
    kind: IndexedSymbolKind,
) -> Option<SymbolKey> {
    let mapped = analysis.source_map.as_ref()?.map_range(range)?;
    let source = normalized_path(mapped.path.to_path_buf());
    symbols
        .iter()
        .find(|symbol| {
            symbol.key.source == source
                && symbol.key.start == u32::from(mapped.range.start())
                && symbol.key.end == u32::from(mapped.range.end())
                && symbol.key.kind == kind
        })
        .map(|symbol| symbol.key.clone())
}

fn mapped_lsp_range(analysis: &DocumentAnalysis, range: rowan::TextRange) -> Option<Range> {
    let mapped = analysis.source_map.as_ref()?.map_range(range)?;
    LineIndex::new(mapped.source).range(mapped.source, mapped.range)
}

fn push_symbol(
    symbols: &mut Vec<IndexedSymbol>,
    analysis: &DocumentAnalysis,
    kind: IndexedSymbolKind,
    name: &str,
    detail: String,
    range: rowan::TextRange,
    _is_public: bool,
) {
    let Some(source_map) = &analysis.source_map else {
        return;
    };
    let Some(mapped) = source_map.map_range(range) else {
        return;
    };
    let source = normalized_path(mapped.path.to_path_buf());
    let Ok(uri) = lsp_types::Url::from_file_path(&source) else {
        return;
    };
    let Some(range) = LineIndex::new(mapped.source).range(mapped.source, mapped.range) else {
        return;
    };
    symbols.push(IndexedSymbol {
        key: SymbolKey {
            project: analysis
                .project_root
                .clone()
                .expect("project symbols require a project root"),
            source,
            start: mapped.range.start().into(),
            end: mapped.range.end().into(),
            kind,
        },
        name: name.into(),
        detail,
        location: Location { uri, range },
    });
}

pub fn project_index_for_document_cancellable<S: BuildHasher>(
    uri: &lsp_types::Url,
    docs: &HashMap<lsp_types::Url, Document, S>,
    options: CompileOptions,
    sessions: &AnalysisSessions,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<ProjectIndex>, String> {
    let Some(analysis) = analyze_document_cancellable(
        uri,
        docs,
        options,
        sessions,
        AnalysisDepth::Infer,
        cancelled,
    )?
    else {
        return Ok(None);
    };
    Ok(ProjectIndex::from_analysis(&analysis))
}

#[cfg(feature = "test")]
/// Builds a project index for an open test document.
///
/// # Errors
///
/// Returns an error when project analysis fails.
pub fn project_index_for_document<S: BuildHasher>(
    uri: &lsp_types::Url,
    docs: &HashMap<lsp_types::Url, Document, S>,
    options: CompileOptions,
    sessions: &AnalysisSessions,
) -> Result<Option<ProjectIndex>, String> {
    project_index_for_document_cancellable(uri, docs, options, sessions, &|| false)
}

pub fn project_index_for_root_cancellable<S: BuildHasher>(
    root: &std::path::Path,
    overlays: &HashMap<PathBuf, String, S>,
    options: CompileOptions,
    sessions: &AnalysisSessions,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<ProjectIndex>, String> {
    let root = normalized_path(root.to_path_buf());
    let session = sessions.project(&root);
    let mut session = session
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let analysis = clue::infer_project_with_session_cancellable(
        &root,
        overlays,
        options,
        &mut session,
        cancelled,
    )
    .map_err(|error| error.to_string())?;
    let Some(analysis) = analysis else {
        return Ok(None);
    };
    let project_revision = session.revision();
    drop(session);
    let files = analysis.source.files.clone();
    let path = Some(normalized_path(analysis.entry.clone()));
    let document_analysis = DocumentAnalysis {
        result: std::sync::Arc::clone(&analysis.result),
        source: analysis.source.source.clone(),
        source_map: Some(analysis.source.source_map.clone()),
        macro_occurrences: analysis.macro_occurrences.clone(),
        macro_source_map: Some(analysis.macro_source_map.clone()),
        path,
        project_root: Some(root),
        project_revision,
        files,
    };
    Ok(ProjectIndex::from_analysis(&document_analysis))
}

#[cfg(feature = "test")]
/// Builds a project index from a test project root.
///
/// # Errors
///
/// Returns an error when the project cannot be loaded or checked.
pub fn project_index_for_root<S: BuildHasher>(
    root: &std::path::Path,
    overlays: &HashMap<PathBuf, String, S>,
    options: CompileOptions,
    sessions: &AnalysisSessions,
) -> Result<Option<ProjectIndex>, String> {
    project_index_for_root_cancellable(root, overlays, options, sessions, &|| false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_keys_round_trip_through_protocol_data() {
        let key = SymbolKey {
            project: PathBuf::from("project"),
            source: PathBuf::from("project/src/main.rid"),
            start: 7,
            end: 12,
            kind: IndexedSymbolKind::Function,
        };

        let encoded = serde_json::to_value(&key).unwrap();
        let decoded: SymbolKey = serde_json::from_value(encoded).unwrap();

        assert_eq!(decoded, key);
    }
}
