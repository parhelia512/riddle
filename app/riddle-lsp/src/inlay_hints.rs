use std::{collections::HashMap, hash::BuildHasher};

use hir::body::{Expr, Stmt};
use hir::item_tree::HirTypeRef;
use lsp_types::{InlayHint, InlayHintKind, InlayHintLabel, Range};
use riddlec::pipeline::CompileOptions;

use crate::{
    analysis::{AnalysisDepth, DocumentAnalysis, analyze_document_cancellable},
    navigation::call_parameter_names_at,
    server::Document,
    session::AnalysisSessions,
    text::{LineIndex, ranges_overlap},
};
use syntax::SyntaxKind;

#[cfg(feature = "test")]
#[must_use]
pub fn inlay_hints_for_source(source: &str, range: Range) -> Vec<InlayHint> {
    let result = riddlec::pipeline::check_with_options(source, CompileOptions { use_std: false });
    inlay_hints_from_analysis(
        source,
        &DocumentAnalysis {
            result,
            source: source.into(),
            source_map: None,
            macro_occurrences: Vec::new(),
            macro_source_map: None,
            path: None,
            project_root: None,
            project_revision: 0,
            files: Vec::new(),
        },
        range,
    )
}

/// Computes inlay hints for an open document.
///
/// # Errors
///
/// Returns an error when the document is unavailable or project analysis fails.
#[cfg(feature = "test")]
/// Computes inlay hints for an open test document.
///
/// # Errors
///
/// Returns an error when project analysis fails.
///
/// # Panics
///
/// Panics if non-cancellable test analysis is unexpectedly cancelled.
pub fn inlay_hints_for_document<S: BuildHasher>(
    uri: &lsp_types::Url,
    docs: &HashMap<lsp_types::Url, Document, S>,
    range: Range,
    options: CompileOptions,
    sessions: &AnalysisSessions,
) -> std::result::Result<Vec<InlayHint>, String> {
    inlay_hints_for_document_cancellable(uri, docs, range, options, sessions, &|| false)
        .map(|hints| hints.expect("non-cancellable analysis cannot be cancelled"))
}

pub fn inlay_hints_for_document_cancellable<S: BuildHasher>(
    uri: &lsp_types::Url,
    docs: &HashMap<lsp_types::Url, Document, S>,
    range: Range,
    options: CompileOptions,
    sessions: &AnalysisSessions,
    cancelled: &impl Fn() -> bool,
) -> std::result::Result<Option<Vec<InlayHint>>, String> {
    let document = docs
        .get(uri)
        .ok_or_else(|| "document is not open".to_string())?;
    let Some(analysis) = analyze_document_cancellable(
        uri,
        docs,
        options,
        sessions,
        AnalysisDepth::Check,
        cancelled,
    )?
    else {
        return Ok(None);
    };
    Ok(Some(inlay_hints_from_analysis(
        &document.text,
        &analysis,
        range,
    )))
}

pub fn inlay_hints_from_analysis(
    document_source: &str,
    analysis: &DocumentAnalysis,
    range: Range,
) -> Vec<InlayHint> {
    let Some(hir) = analysis.result.hir.as_ref() else {
        return Vec::new();
    };
    let mut hints = type_hints_from_analysis(document_source, analysis, hir);
    hints.extend(parameter_hints_from_analysis(document_source, analysis));

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

fn type_hints_from_analysis(
    document_source: &str,
    analysis: &DocumentAnalysis,
    hir: &hir::HirFile,
) -> Vec<InlayHint> {
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
            let Some(name_range) = analysis.local_range(*name_range) else {
                continue;
            };
            hints.push(InlayHint {
                position: crate::diagnostics::position(
                    document_source,
                    usize::from(name_range.end()),
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
    hints
}

fn parameter_hints_from_analysis(
    document_source: &str,
    analysis: &DocumentAnalysis,
) -> Vec<InlayHint> {
    let line_index = LineIndex::new(document_source);
    let tokens = frontend::lexer::lex(document_source)
        .into_iter()
        .filter(|token| {
            token.kind != SyntaxKind::Whitespace && token.kind != SyntaxKind::LineComment
        })
        .collect::<Vec<_>>();
    let mut hints = Vec::new();
    for index in 0..tokens.len().saturating_sub(1) {
        let token = &tokens[index];
        if token.kind != SyntaxKind::Ident
            || tokens[index + 1].kind != SyntaxKind::LParen
            || index
                .checked_sub(1)
                .and_then(|previous| tokens.get(previous))
                .is_some_and(|previous| previous.kind == SyntaxKind::Fun)
        {
            continue;
        }
        let Some(position) = line_index.position(document_source, token.span.start) else {
            continue;
        };
        let Some(parameters) = call_parameter_names_at(document_source, analysis, position) else {
            continue;
        };
        let arguments = call_arguments(&tokens, index);
        for (parameter, argument) in parameters.iter().zip(arguments) {
            if argument.kind == SyntaxKind::Ident
                && &document_source[argument.span.clone()] == parameter
            {
                continue;
            }
            let Some(position) = line_index.position(document_source, argument.span.start) else {
                continue;
            };
            hints.push(InlayHint {
                position,
                label: InlayHintLabel::String(format!("{parameter}: ")),
                kind: Some(InlayHintKind::PARAMETER),
                text_edits: None,
                tooltip: None,
                padding_left: None,
                padding_right: None,
                data: None,
            });
        }
    }
    hints
}

fn call_arguments(tokens: &[frontend::lexer::Token], index: usize) -> Vec<&frontend::lexer::Token> {
    let mut arguments = Vec::new();
    let mut current = None;
    let mut depth = 0usize;
    for argument in tokens.iter().skip(index + 2) {
        if argument.kind == SyntaxKind::RParen && depth == 0 {
            if let Some(argument) = current.take() {
                arguments.push(argument);
            }
            break;
        }
        if argument.kind == SyntaxKind::Comma && depth == 0 {
            if let Some(argument) = current.take() {
                arguments.push(argument);
            }
            continue;
        }
        if current.is_none() {
            current = Some(argument);
        }
        match argument.kind {
            SyntaxKind::LParen | SyntaxKind::LBracket | SyntaxKind::LBrace => depth += 1,
            SyntaxKind::RParen | SyntaxKind::RBracket | SyntaxKind::RBrace => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    arguments
}
