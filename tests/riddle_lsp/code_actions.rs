use super::*;

#[test]
fn unsafe_operation_quick_fix_wraps_the_primary_range() {
    let uri = lsp_types::Url::parse("untitled:unsafe.rid").unwrap();
    let diagnostic = lsp_types::Diagnostic {
        range: Range::new(Position::new(0, 13), Position::new(0, 22)),
        code: Some(lsp_types::NumberOrString::String("E0046".into())),
        source: Some("riddle".into()),
        message: "call to unsafe function requires an unsafe block".into(),
        ..lsp_types::Diagnostic::default()
    };

    let actions = quick_fixes(&uri, Some(7), &[diagnostic]);

    assert_eq!(actions.len(), 1);
    let lsp_types::CodeActionOrCommand::CodeAction(action) = &actions[0] else {
        panic!("expected code action")
    };
    assert_eq!(action.title, "Wrap in `unsafe` block");
    let Some(lsp_types::WorkspaceEdit {
        document_changes: Some(DocumentChanges::Edits(edits)),
        ..
    }) = &action.edit
    else {
        panic!("expected document edits")
    };
    assert_eq!(edits[0].text_document.version, Some(7));
    assert_eq!(edits[0].edits.len(), 2);
    assert_eq!(
        edits[0].edits[0],
        lsp_types::OneOf::Left(lsp_types::TextEdit::new(
            Range::new(Position::new(0, 13), Position::new(0, 13)),
            "unsafe { ".into(),
        ))
    );
    assert_eq!(
        edits[0].edits[1],
        lsp_types::OneOf::Left(lsp_types::TextEdit::new(
            Range::new(Position::new(0, 22), Position::new(0, 22)),
            " }".into(),
        ))
    );
}

#[test]
fn infinite_type_e0046_does_not_offer_an_unsafe_quick_fix() {
    let uri = lsp_types::Url::parse("untitled:infinite-type.rid").unwrap();
    let diagnostic = lsp_types::Diagnostic {
        range: Range::new(Position::new(0, 0), Position::new(0, 6)),
        code: Some(lsp_types::NumberOrString::String("E0046".into())),
        source: Some("riddle".into()),
        message: "cannot construct an infinite type".into(),
        ..lsp_types::Diagnostic::default()
    };

    assert!(quick_fixes(&uri, Some(1), &[diagnostic]).is_empty());
}
