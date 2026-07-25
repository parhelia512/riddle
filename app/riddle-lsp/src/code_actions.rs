use std::collections::HashMap;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionResponse, NumberOrString, Range,
    TextEdit, WorkspaceEdit,
};

const MUTABLE_CLOSURE_BINDING_MESSAGE: &str =
    "cannot call a mutable closure through an immutable binding\nimmutable closure binding";

pub(crate) fn quick_fixes(
    uri: &lsp_types::Url,
    diagnostics: &[lsp_types::Diagnostic],
) -> CodeActionResponse {
    diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.source.as_deref() == Some("riddle")
                && matches!(
                    &diagnostic.code,
                    Some(NumberOrString::String(code)) if code == "E0031"
                )
                && diagnostic
                    .message
                    .starts_with(MUTABLE_CLOSURE_BINDING_MESSAGE)
        })
        .map(|diagnostic| {
            let start = diagnostic.range.start;
            CodeActionOrCommand::CodeAction(CodeAction {
                title: "Add `mut` to closure binding".into(),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diagnostic.clone()]),
                edit: Some(WorkspaceEdit {
                    changes: Some(HashMap::from([(
                        uri.clone(),
                        vec![TextEdit::new(Range::new(start, start), "mut ".into())],
                    )])),
                    ..WorkspaceEdit::default()
                }),
                is_preferred: Some(true),
                ..CodeAction::default()
            })
        })
        .collect()
}
