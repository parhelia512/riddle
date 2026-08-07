use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionResponse, DocumentChanges,
    NumberOrString, OneOf, OptionalVersionedTextDocumentIdentifier, Range, TextDocumentEdit,
    TextEdit, WorkspaceEdit,
};

const MUTABLE_CLOSURE_BINDING_MESSAGE: &str =
    "cannot call a mutable closure through an immutable binding\nimmutable closure binding";

#[must_use]
pub fn quick_fixes(
    uri: &lsp_types::Url,
    version: Option<i32>,
    diagnostics: &[lsp_types::Diagnostic],
) -> CodeActionResponse {
    diagnostics
        .iter()
        .filter_map(|diagnostic| {
            if diagnostic.source.as_deref() != Some("riddle") {
                return None;
            }
            let (title, edits) = match &diagnostic.code {
                Some(NumberOrString::String(code))
                    if code == "E0031"
                        && diagnostic
                            .message
                            .starts_with(MUTABLE_CLOSURE_BINDING_MESSAGE) =>
                {
                    let start = diagnostic.range.start;
                    (
                        "Add `mut` to closure binding",
                        vec![OneOf::Left(TextEdit::new(
                            Range::new(start, start),
                            "mut ".into(),
                        ))],
                    )
                }
                Some(NumberOrString::String(code))
                    if code == "E0046"
                        && diagnostic.message.ends_with("requires an unsafe block") =>
                {
                    (
                        "Wrap in `unsafe` block",
                        vec![
                            OneOf::Left(TextEdit::new(
                                Range::new(diagnostic.range.start, diagnostic.range.start),
                                "unsafe { ".into(),
                            )),
                            OneOf::Left(TextEdit::new(
                                Range::new(diagnostic.range.end, diagnostic.range.end),
                                " }".into(),
                            )),
                        ],
                    )
                }
                _ => return None,
            };
            Some(CodeActionOrCommand::CodeAction(CodeAction {
                title: title.into(),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diagnostic.clone()]),
                edit: Some(WorkspaceEdit {
                    document_changes: Some(DocumentChanges::Edits(vec![TextDocumentEdit {
                        text_document: OptionalVersionedTextDocumentIdentifier {
                            uri: uri.clone(),
                            version,
                        },
                        edits,
                    }])),
                    ..WorkspaceEdit::default()
                }),
                is_preferred: Some(true),
                ..CodeAction::default()
            }))
        })
        .collect()
}
