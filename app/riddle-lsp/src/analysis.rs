use std::{
    collections::HashMap,
    hash::BuildHasher,
    path::{Path, PathBuf},
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
    Check,
}

pub struct DocumentAnalysis {
    pub(crate) result: CompileResult,
    pub(crate) source: String,
    pub(crate) source_map: Option<riddlec::pipeline::SourceMap>,
    pub(crate) macro_occurrences: Vec<riddlec::proc_macro::ProcMacroOccurrence>,
    pub(crate) macro_source_map: Option<riddlec::pipeline::SourceMap>,
    pub(crate) path: Option<PathBuf>,
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

    pub(crate) fn local_macro_range(&self, range: &std::ops::Range<usize>) -> Option<TextRange> {
        let source_map = self.macro_source_map.as_ref()?;
        let mapped = source_map.map_range(text_range(range.start, range.end))?;
        self.path
            .as_deref()
            .is_none_or(|path| mapped.path == path)
            .then_some(mapped.range)
    }
}

pub fn analyze_standalone_source(
    source: &str,
    options: CompileOptions,
    session: &mut CheckSession,
    depth: AnalysisDepth,
    path: Option<PathBuf>,
) -> DocumentAnalysis {
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
    let package_range = 0..loaded.source.len();
    let package_ranges = std::slice::from_ref(&package_range);
    let mut result = match (depth, parse.as_ref()) {
        (AnalysisDepth::Resolve, Some(parse)) => session
            .resolve_parsed_package_with_options_cancellable(
                &loaded.source,
                parse,
                package_ranges,
                options,
                || false,
            )
            .expect("non-cancellable pipeline cannot be cancelled"),
        (AnalysisDepth::Resolve, None) => {
            session.resolve_package_with_options(&loaded.source, package_ranges, options)
        }
        (AnalysisDepth::Check, Some(parse)) => session
            .check_parsed_package_with_options_cancellable(
                &loaded.source,
                parse,
                package_ranges,
                options,
                || false,
            )
            .expect("non-cancellable pipeline cannot be cancelled"),
        (AnalysisDepth::Check, None) => {
            session.check_package_with_options(&loaded.source, package_ranges, options)
        }
    };
    result.macro_diagnostics = diagnostics;
    DocumentAnalysis {
        result,
        source: loaded.source,
        source_map: Some(loaded.source_map),
        macro_occurrences,
        macro_source_map: Some(macro_source_map),
        path,
    }
}

pub fn analyze_document<S: BuildHasher>(
    uri: &lsp_types::Url,
    docs: &HashMap<lsp_types::Url, Document, S>,
    options: CompileOptions,
    sessions: &AnalysisSessions,
    depth: AnalysisDepth,
) -> std::result::Result<DocumentAnalysis, String> {
    let document = docs
        .get(uri)
        .ok_or_else(|| "document is not open".to_string())?;
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
            AnalysisDepth::Resolve => {
                clue::resolve_project_with_session(&root, &overlays, options, &mut session)
            }
            AnalysisDepth::Check => {
                clue::check_project_with_session(&root, &overlays, options, &mut session)
            }
        }
        .map_err(|error| error.to_string())?;
        return Ok(DocumentAnalysis {
            result: analysis.result,
            source: analysis.source.source,
            source_map: Some(analysis.source.source_map),
            macro_occurrences: analysis.macro_occurrences,
            macro_source_map: Some(analysis.macro_source_map),
            path: Some(normalized_path(path)),
        });
    }

    let session = sessions.standalone(uri);
    let mut session = session
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = uri.to_file_path().ok().map(normalized_path);
    Ok(analyze_standalone_source(
        &document.text,
        options,
        &mut session,
        depth,
        path,
    ))
}
