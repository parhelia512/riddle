use super::*;

fn compile_diagnostics(source: &str) -> Vec<lsp_types::Diagnostic> {
    let uri = lsp_types::Url::parse("untitled:code-actions.rid").unwrap();
    let mut session = riddlec::pipeline::CheckSession::new();
    let result = session.check_with_options(source, CompileOptions::default());
    collect_diagnostics(&uri, source, &result)
}

fn code_actions(source: &str) -> Vec<lsp_types::CodeAction> {
    let diagnostics = compile_diagnostics(source);
    code_actions_for(source, &diagnostics)
}

fn code_actions_for(
    source: &str,
    diagnostics: &[lsp_types::Diagnostic],
) -> Vec<lsp_types::CodeAction> {
    quick_fixes_for_source(source, diagnostics)
        .into_iter()
        .filter_map(|action| match action {
            lsp_types::CodeActionOrCommand::CodeAction(action) => Some(action),
            lsp_types::CodeActionOrCommand::Command(_) => None,
        })
        .collect()
}

fn action_titled<'a>(
    actions: &'a [lsp_types::CodeAction],
    title: &str,
) -> &'a lsp_types::CodeAction {
    actions
        .iter()
        .find(|action| action.title == title)
        .unwrap_or_else(|| {
            panic!(
                "missing action `{title}`; got {:?}",
                actions
                    .iter()
                    .map(|action| &action.title)
                    .collect::<Vec<_>>()
            )
        })
}

fn action_edits(action: &lsp_types::CodeAction) -> Vec<lsp_types::TextEdit> {
    let Some(lsp_types::WorkspaceEdit {
        document_changes: Some(DocumentChanges::Edits(edits)),
        ..
    }) = action.edit.as_ref()
    else {
        panic!("action `{}` has no document edits", action.title)
    };
    edits[0]
        .edits
        .iter()
        .map(|edit| match edit {
            lsp_types::OneOf::Left(edit) => edit.clone(),
            lsp_types::OneOf::Right(_) => panic!("annotated edits are not expected"),
        })
        .collect()
}

fn apply_edits(source: &str, edits: &[lsp_types::TextEdit]) -> String {
    let mut offsets = edits
        .iter()
        .map(|edit| {
            let start = offset_for_position(source, edit.range.start).unwrap();
            let end = offset_for_position(source, edit.range.end).unwrap();
            (start, end, edit.new_text.clone())
        })
        .collect::<Vec<_>>();
    offsets.sort_by_key(|&(start, end, _)| std::cmp::Reverse((start, end)));
    let mut applied = source.to_string();
    for (start, end, text) in offsets {
        applied.replace_range(start..end, &text);
    }
    applied
}

fn apply_titled(source: &str, title: &str) -> String {
    let actions = code_actions(source);
    let action = action_titled(&actions, title);
    let edits = action_edits(action);
    apply_edits(source, &edits)
}

fn organize(source: &str) -> String {
    let edits = organize_imports_for_source(source);
    apply_edits(source, &edits)
}

#[test]
fn unsafe_operation_quick_fix_wraps_the_primary_range() {
    let uri = lsp_types::Url::parse("untitled:unsafe.rid").unwrap();
    let source = "fun main() {\n    risky();\n}\n";
    let diagnostic = lsp_types::Diagnostic {
        range: Range::new(Position::new(1, 4), Position::new(1, 11)),
        code: Some(lsp_types::NumberOrString::String("E0046".into())),
        source: Some("riddle".into()),
        message: "call to unsafe function requires an unsafe block".into(),
        ..lsp_types::Diagnostic::default()
    };

    let actions = quick_fixes(&uri, Some(7), source, &[diagnostic]);

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
            Range::new(Position::new(1, 4), Position::new(1, 4)),
            "unsafe { ".into(),
        ))
    );
    assert_eq!(
        edits[0].edits[1],
        lsp_types::OneOf::Left(lsp_types::TextEdit::new(
            Range::new(Position::new(1, 11), Position::new(1, 11)),
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

    assert!(quick_fixes(&uri, Some(1), "", &[diagnostic]).is_empty());
}

#[test]
fn assignment_to_immutable_binding_offers_add_mut() {
    let source = "fun main() {\n    let x = 1;\n    x = 2;\n}\n";
    assert_eq!(
        apply_titled(source, "Add `mut` to binding"),
        "fun main() {\n    let mut x = 1;\n    x = 2;\n}\n"
    );
}

#[test]
fn immutable_closure_binding_offers_the_closure_fix() {
    let source = "fun main() {\n    let count = 0;\n    let inc = [ -> {\n        count += 1;\n    }];\n    inc();\n}\n";
    let actions = code_actions(source);

    let closure = action_titled(&actions, "Add `mut` to closure binding");
    let edits = action_edits(closure);
    assert_eq!(edits.len(), 1);
    assert!(apply_edits(source, &edits).contains("let mut inc = [ -> {"));

    let binding = action_titled(&actions, "Add `mut` to binding");
    assert!(apply_edits(source, &action_edits(binding)).contains("let mut count = 0;"));
}

#[test]
fn missing_struct_field_fix_fills_single_line_literals() {
    let source = "struct Point {\n    x: i32,\n    y: i32,\n}\n\nfun main() {\n    let p = Point { x: 1 };\n}\n";
    assert_eq!(
        apply_titled(source, "Add field `y`"),
        "struct Point {\n    x: i32,\n    y: i32,\n}\n\nfun main() {\n    let p = Point { x: 1, y: todo!() };\n}\n"
    );
}

#[test]
fn missing_struct_field_fix_keeps_multiline_layout() {
    let source = "struct Point {\n    x: i32,\n    y: i32,\n}\n\nfun main() {\n    let p = Point {\n        x: 1,\n    };\n}\n";
    assert_eq!(
        apply_titled(source, "Add field `y`"),
        "struct Point {\n    x: i32,\n    y: i32,\n}\n\nfun main() {\n    let p = Point {\n        x: 1,\n        y: todo!(),\n    };\n}\n"
    );
}

#[test]
fn non_exhaustive_match_offers_the_missing_enum_arm() {
    let source = "enum Color {\n    Red,\n    Blue,\n}\n\nfun pick(c: Color) -> i32 {\n    let n = match c {\n        Color::Red => 1,\n    };\n    n\n}\n";
    assert_eq!(
        apply_titled(source, "Add `Color::Blue` arm"),
        "enum Color {\n    Red,\n    Blue,\n}\n\nfun pick(c: Color) -> i32 {\n    let n = match c {\n        Color::Red => 1,\n        Color::Blue => todo!(),\n    };\n    n\n}\n"
    );
}

#[test]
fn non_exhaustive_integer_match_offers_a_wildcard_arm() {
    let source = "fun main() -> i32 {\n    let n = match 5 {\n        0 => 1,\n    };\n    n\n}\n";
    assert_eq!(
        apply_titled(source, "Add wildcard arm"),
        "fun main() -> i32 {\n    let n = match 5 {\n        0 => 1,\n        _ => todo!(),\n    };\n    n\n}\n"
    );
}

#[test]
fn empty_use_declaration_fix_removes_the_whole_line() {
    let source = "use ;\n\nfun main() {}\n";
    let diagnostic = lsp_types::Diagnostic {
        range: Range::new(Position::new(0, 4), Position::new(0, 5)),
        code: Some(lsp_types::NumberOrString::String("E0051".into())),
        source: Some("riddle".into()),
        message: "empty use declaration".into(),
        ..lsp_types::Diagnostic::default()
    };
    let actions = code_actions_for(source, &[diagnostic]);
    let action = action_titled(&actions, "Remove empty `use` declaration");
    let edits = action_edits(action);
    assert_eq!(edits.len(), 1);
    assert_eq!(apply_edits(source, &edits), "\nfun main() {}\n");
}

#[test]
fn explicit_destructor_call_is_rewritten_to_drop() {
    let source = "struct Box {\n    v: i32,\n}\n\nimpl Drop for Box {\n    fun drop(&mut self) {}\n}\n\nfun main() {\n    let mut b = Box { v: 1 };\n    b.drop();\n}\n";
    assert!(apply_titled(source, "Replace with `drop(...)`").contains("    drop(b);\n"));
}

#[test]
fn unresolved_name_suggests_a_similar_in_scope_name() {
    let source = "fun main() {\n    let value = 1;\n    let n = valu;\n}\n";
    assert_eq!(
        apply_titled(source, "Did you mean `value`?"),
        "fun main() {\n    let value = 1;\n    let n = value;\n}\n"
    );
}

#[test]
fn unresolved_name_suggests_importing_the_symbol() {
    let source = "mod util {\n    pub fun helper() -> i32 {\n        1\n    }\n}\n\nfun main() {\n    let n = helper();\n}\n";
    let actions = code_actions(source);
    let action = action_titled(&actions, "Import `helper` from `util::helper`");
    let edits = action_edits(action);
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].range.start, Position::new(0, 0));
    assert_eq!(edits[0].new_text, "use util::helper;\n");
}

#[test]
fn organize_imports_sorts_and_dedupes() {
    assert_eq!(
        organize("use b;\npub use a;\nuse b;\n"),
        "use b;\npub use a;\n"
    );
}

#[test]
fn organize_imports_moves_reexports_after_plain_imports() {
    assert_eq!(organize("pub use b;\nuse a;\n"), "use a;\npub use b;\n");
}

#[test]
fn organize_imports_dedupes_without_reordering_commented_blocks() {
    let source = "// lead\nuse b;\n// keep\nuse a;\nuse a;\n";
    assert_eq!(organize(source), "// lead\nuse b;\n// keep\nuse a;\n");
}

#[test]
fn organize_imports_is_noop_when_already_sorted() {
    assert_eq!(organize("use a;\nuse b;\n"), "use a;\nuse b;\n");
    assert!(organize_imports_for_source("use a;\n").is_empty());
}

#[test]
fn organize_imports_leaves_commented_blocks_unordered() {
    let source = "// a\nuse b;\n// keep\nuse a;\n";
    assert_eq!(organize(source), source);
}

#[test]
fn organize_imports_skips_interleaved_items() {
    let source = "use b;\nfun main() {}\nuse a;\n";
    assert_eq!(organize(source), source);
}

#[test]
fn missing_trait_method_offers_stub_implementation() {
    let source = "trait Show { fun show(self) -> i32; }\nstruct Boxed { value: i32 }\nimpl Show for Boxed { fun ignored(self) -> i32 { self.value } }\n";

    let actions = code_actions(source);
    let action = action_titled(&actions, "Implement `show` from `Show`");
    let fixed = apply_edits(source, &action_edits(action));

    assert_eq!(
        fixed,
        "trait Show { fun show(self) -> i32; }\nstruct Boxed { value: i32 }\nimpl Show for Boxed { fun ignored(self) -> i32 { self.value }\n    fun show(self) -> i32 {\n        todo!()\n    }\n}\n"
    );
}

#[test]
fn missing_trait_method_fix_renders_receiver_and_return_types() {
    let source = "trait Visit { fun visit(&mut self, value: i32) -> bool; }\nstruct Counter { total: i32 }\nimpl Visit for Counter { }\n";

    let actions = code_actions(source);
    let action = action_titled(&actions, "Implement `visit` from `Visit`");
    let fixed = apply_edits(source, &action_edits(action));

    assert!(fixed.contains("fun visit(&mut self, value: i32) -> bool {"));
}

#[test]
fn unknown_method_offers_did_you_mean() {
    let source = "struct Counter { value: i32 } impl Counter { fun inc(self) -> Counter { Counter { value: self.value + 1 } } } fun main() { let c = Counter { value: 0 }; let d = c.inc(); let e = d.icn(); }";

    let actions = code_actions(source);
    let action = action_titled(&actions, "Did you mean `inc`?");
    let fixed = apply_edits(source, &action_edits(action));

    assert!(fixed.contains("let e = d.inc();"));
}

#[test]
fn unknown_method_without_close_candidate_has_no_fix() {
    let source = "struct P { x: i32 } fun main() { let p = P { x: 1 }; p.qqqqzzzz(); }";

    let actions = code_actions(source);

    assert!(actions.is_empty());
}
