use std::{collections::HashMap, path::PathBuf};

use riddlec::pipeline::{CompileOptions, CompileResult};
use rowan::TextRange;

use crate::{
    server::Document,
    session::AnalysisSessions,
    text::{normalized_path, range_is_in_source},
};

#[derive(Clone, Copy)]
pub(crate) enum AnalysisDepth {
    Resolve,
    Check,
}

pub(crate) struct DocumentAnalysis {
    pub(crate) result: CompileResult,
    pub(crate) source: String,
    pub(crate) source_map: Option<riddlec::pipeline::SourceMap>,
    pub(crate) path: Option<PathBuf>,
}

impl DocumentAnalysis {
    pub(crate) fn local_range(&self, range: TextRange) -> Option<TextRange> {
        let Some(source_map) = &self.source_map else {
            return range_is_in_source(range, self.source.len()).then_some(range);
        };
        let mapped = source_map.map_range(range)?;
        (Some(mapped.path) == self.path.as_deref()).then_some(mapped.range)
    }
}

pub(crate) fn analyze_document(
    uri: &lsp_types::Url,
    docs: &HashMap<lsp_types::Url, Document>,
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
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
            path: Some(normalized_path(path)),
        });
    }

    let session = sessions.standalone(uri);
    let mut session = session
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let result = match depth {
        AnalysisDepth::Resolve => session.resolve_with_options(&document.text, options),
        AnalysisDepth::Check => session.check_with_options(&document.text, options),
    };
    Ok(DocumentAnalysis {
        result,
        source: document.text.clone(),
        source_map: None,
        path: None,
    })
}
