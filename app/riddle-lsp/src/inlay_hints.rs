use std::collections::HashMap;

use hir::body::{Expr, Stmt};
use hir::item_tree::HirTypeRef;
use lsp_types::{InlayHint, InlayHintKind, InlayHintLabel, Range};
use riddlec::pipeline::CompileOptions;

use crate::{
    analysis::{AnalysisDepth, DocumentAnalysis, analyze_document},
    server::Document,
    session::AnalysisSessions,
    text::ranges_overlap,
};

#[cfg(feature = "test-support")]
pub fn inlay_hints_for_source(source: &str, range: Range) -> Vec<InlayHint> {
    let result = riddlec::pipeline::check_with_options(source, CompileOptions { use_std: false });
    inlay_hints_from_analysis(
        source,
        &DocumentAnalysis {
            result,
            source: source.into(),
            source_map: None,
            path: None,
        },
        range,
    )
}

pub fn inlay_hints_for_document(
    uri: &lsp_types::Url,
    docs: &HashMap<lsp_types::Url, Document>,
    range: Range,
    options: CompileOptions,
    sessions: &AnalysisSessions,
) -> std::result::Result<Vec<InlayHint>, String> {
    let document = docs
        .get(uri)
        .ok_or_else(|| "document is not open".to_string())?;
    let analysis = analyze_document(uri, docs, options, sessions, AnalysisDepth::Check)?;
    Ok(inlay_hints_from_analysis(&document.text, &analysis, range))
}

pub(crate) fn inlay_hints_from_analysis(
    document_source: &str,
    analysis: &DocumentAnalysis,
    range: Range,
) -> Vec<InlayHint> {
    let Some(hir) = analysis.result.hir.as_ref() else {
        return Vec::new();
    };
    let type_result = &analysis.result.type_result;
    let mut hints = Vec::new();

    for (body_id, body) in hir.bodies.iter() {
        for (_, statement) in body.stmts.iter() {
            let Stmt::Let {
                pat,
                ty: HirTypeRef::Unknown,
                init: Some(init),
                ..
            } = statement
            else {
                continue;
            };
            // The hint sits after the whole pattern, so `let (a, b) = pair`
            // reads `let (a, b): (i32, i32) = pair`.
            let Some(name_range) = body.source_map.pat_ranges.get(pat) else {
                continue;
            };
            if matches!(body.exprs[*init], Expr::Struct { .. }) {
                continue;
            }
            let Some(init_range) = body.source_map.expr_ranges.get(init).copied() else {
                continue;
            };
            if analysis
                .result
                .hir_diagnostics
                .iter()
                .chain(type_result.diagnostics.iter())
                .filter(|diagnostic| diagnostic.severity == type_checker::Severity::Error)
                .flat_map(|diagnostic| &diagnostic.labels)
                .any(|label| ranges_overlap(label.range, init_range))
            {
                continue;
            }
            let Some(ty) = type_result.expr_types.get(&(body_id, *init)) else {
                continue;
            };
            if matches!(
                ty,
                type_checker::Type::Unknown
                    | type_checker::Type::Error
                    | type_checker::Type::InferVar(_)
                    | type_checker::Type::Never
            ) {
                continue;
            }
            hints.push(InlayHint {
                position: crate::diagnostics::position(
                    document_source,
                    usize::from(match analysis.local_range(*name_range) {
                        Some(range) => range.end(),
                        None => continue,
                    }),
                ),
                label: InlayHintLabel::String(format!(": {}", ty.display(hir))),
                kind: Some(InlayHintKind::TYPE),
                text_edits: None,
                tooltip: None,
                padding_left: None,
                padding_right: None,
                data: None,
            });
        }
    }

    // LSP ranges have an exclusive end: a hint on range.end.line is only
    // inside the range when its character is strictly before range.end.character.
    hints.retain(|hint| {
        let line = hint.position.line;
        if line < range.start.line || line > range.end.line {
            return false;
        }
        if line == range.start.line && hint.position.character < range.start.character {
            return false;
        }
        if line == range.end.line && hint.position.character >= range.end.character {
            return false;
        }
        true
    });
    hints.sort_by_key(|hint| (hint.position.line, hint.position.character));
    hints
}
