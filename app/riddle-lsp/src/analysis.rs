use std::{
    collections::HashMap,
    hash::BuildHasher,
    path::{Path, PathBuf},
    sync::Arc,
};

use riddlec::pipeline::{CheckSession, CompileOptions, CompileResult, LoadedSource};
use rowan::TextRange;

use crate::{
    server::Document,
    session::AnalysisSessions,
    text::{normalized_path, range_is_in_source, text_range},
};

#[derive(Clone, Copy)]
pub enum AnalysisDepth {
    Resolve,
    Infer,
}

pub struct DocumentAnalysis {
    pub(crate) result: Arc<CompileResult>,
    pub(crate) source: String,
    pub(crate) source_map: Option<riddlec::pipeline::SourceMap>,
    pub(crate) macro_occurrences: Vec<riddlec::proc_macro::ProcMacroOccurrence>,
    pub(crate) macro_source_map: Option<riddlec::pipeline::SourceMap>,
    pub(crate) path: Option<PathBuf>,
    pub(crate) project_root: Option<PathBuf>,
    pub(crate) project_revision: u64,
    pub(crate) files: Vec<PathBuf>,
}

impl DocumentAnalysis {
    pub(crate) fn local_range(&self, range: TextRange) -> Option<TextRange> {
        let Some(source_map) = &self.source_map else {
            return range_is_in_source(range, self.source.len()).then_some(range);
        };
        let mapped = source_map.map_range(range)?;
        self.path
            .as_deref()
            .is_none_or(|path| mapped.path == path)
            .then_some(mapped.range)
    }

    pub(crate) fn local_authored_range(&self, range: TextRange) -> Option<TextRange> {
        let Some(source_map) = &self.source_map else {
            return range_is_in_source(range, self.source.len()).then_some(range);
        };
        let mapped = source_map.map_range(range)?;
        (!mapped.synthetic && self.path.as_deref().is_none_or(|path| mapped.path == path))
            .then_some(mapped.range)
    }

    pub(crate) fn local_macro_range(&self, range: &std::ops::Range<usize>) -> Option<TextRange> {
        let source_map = self.macro_source_map.as_ref()?;
        let mapped = source_map.map_range(text_range(range.start, range.end))?;
        self.path
            .as_deref()
            .is_none_or(|path| mapped.path == path)
            .then_some(mapped.range)
    }
}

#[cfg(feature = "test")]
pub fn analyze_standalone_source(
    source: &str,
    options: CompileOptions,
    session: &mut CheckSession,
    depth: AnalysisDepth,
    path: Option<PathBuf>,
) -> DocumentAnalysis {
    analyze_standalone_source_cancellable(source, options, session, depth, path, &|| false)
        .expect("non-cancellable analysis cannot be cancelled")
}

pub fn analyze_standalone_source_cancellable(
    source: &str,
    options: CompileOptions,
    session: &mut CheckSession,
    depth: AnalysisDepth,
    path: Option<PathBuf>,
    cancelled: &impl Fn() -> bool,
) -> Option<DocumentAnalysis> {
    if cancelled() {
        return None;
    }
    let mut loaded =
        LoadedSource::single_file(path.as_deref().unwrap_or_else(|| Path::new("")), source);
    let macro_source_map = loaded.source_map.clone();
    let riddlec::proc_macro::ExpandedSource {
        source,
        parse,
        mappings,
        macro_occurrences,
        diagnostics,
        ..
    } = riddlec::proc_macro::expand_standard_macros(source);
    loaded.apply_expansion(source, &mappings);
    if cancelled() {
        return None;
    }
    let package_range = 0..loaded.source.len();
    let package_ranges = std::slice::from_ref(&package_range);
    let mut result = match (depth, parse.as_ref()) {
        (AnalysisDepth::Resolve, Some(parse)) => session
            .resolve_parsed_package_with_options_cancellable(
                &loaded.source,
                parse,
                package_ranges,
                options,
                cancelled,
            )?,
        (AnalysisDepth::Resolve, None) => session.resolve_package_with_options_cancellable(
            &loaded.source,
            package_ranges,
            options,
            cancelled,
        )?,
        (AnalysisDepth::Infer, Some(parse)) => session
            .infer_parsed_package_with_options_and_gc_cancellable(
                &loaded.source,
                parse,
                package_ranges,
                options,
                true,
                cancelled,
            )?,
        (AnalysisDepth::Infer, None) => session.infer_package_with_options_and_gc_cancellable(
            &loaded.source,
            package_ranges,
            options,
            true,
            cancelled,
        )?,
    };
    result.macro_diagnostics = diagnostics;
    Some(DocumentAnalysis {
        result: Arc::new(result),
        source: loaded.source,
        source_map: Some(loaded.source_map),
        macro_occurrences,
        macro_source_map: Some(macro_source_map),
        path,
        project_root: None,
        project_revision: 0,
        files: Vec::new(),
    })
}

pub fn analyze_document_cancellable<S: BuildHasher>(
    uri: &lsp_types::Url,
    docs: &HashMap<lsp_types::Url, Document, S>,
    options: CompileOptions,
    sessions: &AnalysisSessions,
    depth: AnalysisDepth,
    cancelled: &impl Fn() -> bool,
) -> std::result::Result<Option<DocumentAnalysis>, String> {
    let document = docs
        .get(uri)
        .ok_or_else(|| "document is not open".to_string())?;
    if cancelled() {
        return Ok(None);
    }
    if let Ok(path) = uri.to_file_path()
        && let Some(root) = clue::find_project_root(&path)
    {
        let overlays = docs
            .iter()
            .filter_map(|(uri, document)| {
                uri.to_file_path()
                    .ok()
                    .map(|path| (path, document.text.clone()))
            })
            .collect::<HashMap<_, _>>();
        let session = sessions.project(&root);
        let mut session = session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let analysis = match depth {
            AnalysisDepth::Resolve => clue::resolve_project_with_session_cancellable(
                &root,
                &overlays,
                options,
                &mut session,
                cancelled,
            ),
            AnalysisDepth::Infer => clue::infer_project_with_session_cancellable(
                &root,
                &overlays,
                options,
                &mut session,
                cancelled,
            ),
        }
        .map_err(|error| error.to_string())?;
        let Some(analysis) = analysis else {
            return Ok(None);
        };
        let project_revision = session.revision();
        drop(session);
        let files = analysis.source.files.clone();
        return Ok(Some(DocumentAnalysis {
            result: Arc::clone(&analysis.result),
            source: analysis.source.source.clone(),
            source_map: Some(analysis.source.source_map.clone()),
            macro_occurrences: analysis.macro_occurrences.clone(),
            macro_source_map: Some(analysis.macro_source_map.clone()),
            path: Some(normalized_path(path)),
            project_root: Some(root),
            project_revision,
            files,
        }));
    }

    let session = sessions.standalone(uri);
    let mut session = session
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = uri.to_file_path().ok().map(normalized_path);
    let mut analysis = analyze_standalone_source_cancellable(
        &document.text,
        options,
        &mut session,
        depth,
        path,
        cancelled,
    );
    drop(session);
    if let Some(analysis) = &mut analysis
        && let Some(path) = &analysis.path
    {
        analysis.files.push(path.clone());
    }
    Ok(analysis)
}
