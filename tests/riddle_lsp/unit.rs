use lsp_types::{
    DiagnosticSeverity, InlayHintLabel, Position, Range, SemanticToken, SemanticTokens,
    TextDocumentContentChangeEvent,
};
use riddle_lsp::{
    parse_args,
    test_support::{
        AnalysisSessions, DiagnosticSessions, Document, MOD_DECLARATION, MOD_DEFAULT_LIBRARY,
        MOD_MUTABLE, MOD_STATIC, RequestRevisions, TOKEN_COMMENT, TOKEN_ENUM, TOKEN_FUNCTION,
        TOKEN_INTERFACE, TOKEN_KEYWORD, TOKEN_METHOD, TOKEN_PARAMETER, TOKEN_STRING, TOKEN_STRUCT,
        TOKEN_TYPE, TOKEN_VARIABLE, apply_content_changes, collect_diagnostics,
        collect_document_diagnostics, collect_workspace_diagnostics,
        collect_workspace_diagnostics_cancellable, collect_workspace_diagnostics_with_sessions,
        completion_items_for_document, completion_items_for_source, inlay_hints_for_document,
        inlay_hints_for_source, semantic_token_delta, semantic_tokens_for_document,
        semantic_tokens_for_source, semantic_tokens_for_source_with_options, to_lsp, to_lsp_mapped,
    },
};
use riddlec::pipeline::{CompileOptions, IntoDiagnosticExt};
use rowan::TextRange;
use std::{
    cell::Cell,
    collections::HashMap,
    env, fs,
    path::PathBuf,
    process,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const DOCUMENTED_ERROR_CODES: &[&str] = &[
    "E0001", "E0002", "E0003", "E0004", "E0005", "E0006", "E0007", "E0008", "E0009", "E0010",
    "E0011", "E0012", "E0013", "E0020", "E0022", "E0023", "E0024", "E0025", "E0026", "E0027",
    "E0028", "E0029", "E0030", "E0031", "E0032", "E0033", "E0034", "E0035", "E0036", "E0037",
    "E0038", "E0039", "E0040", "E0041", "E0042", "E0043", "E0044", "E0045", "E0047", "E0048",
    "E0049", "E0050", "E0051", "E0052", "E0053", "E0054", "E0055", "E0056", "E0072", "E0100",
    "E0200", "E0300", "E0301", "E0302", "E0303", "E0304", "E0305", "E0306", "E0307", "E0308",
    "E0391",
];
const SOURCE_UNREACHABLE_CODES: &[&str] = &["E0048", "E0049", "E0200"];

fn temp_root(name: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "riddle-lsp-{name}-{}-{}",
        process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn semantic_tokens(source: &str) -> SemanticTokens {
    semantic_tokens_for_source(source)
}

fn source_label(
    range: TextRange,
    message: &str,
    style: type_checker::LabelStyle,
) -> type_checker::SourceLabel {
    type_checker::SourceLabel {
        range,
        message: message.into(),
        style,
    }
}

fn diagnostic_ext(
    code: &'static str,
    severity: type_checker::Severity,
    labels: Vec<type_checker::SourceLabel>,
) -> riddlec::pipeline::DiagnosticExt {
    riddlec::pipeline::DiagnosticExt {
        code,
        severity,
        message: "message".into(),
        labels,
        help: None,
        notes: Vec::new(),
    }
}

#[test]
fn every_documented_error_code_keeps_rust_style_lsp_fields() {
    let error_docs = include_str!("../../docs/zh-CN/src/errorcode.md");
    let source = "  target  ";
    let uri = lsp_types::Url::parse("file:///diagnostic-codes.rid").unwrap();
    let primary = source_label(
        TextRange::new(0.into(), (source.len() as u32).into()),
        "",
        type_checker::LabelStyle::Primary,
    );

    for &code in DOCUMENTED_ERROR_CODES {
        let diagnostic = to_lsp(
            &uri,
            source,
            diagnostic_ext(code, type_checker::Severity::Error, vec![primary.clone()]),
        )
        .unwrap();

        assert_eq!(
            diagnostic.code,
            Some(lsp_types::NumberOrString::String(code.into()))
        );
        assert_eq!(diagnostic.source.as_deref(), Some("riddle"));
        assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(
            diagnostic.range,
            Range::new(Position::new(0, 2), Position::new(0, 8))
        );
        let code_description = diagnostic.code_description.unwrap();
        let anchor = code.to_ascii_lowercase();
        assert_eq!(code_description.href.fragment(), Some(anchor.as_str()));
        assert!(
            error_docs.contains(&format!("<a id=\"{anchor}\"></a>")),
            "missing documentation anchor for {code}"
        );
    }
}

#[test]
fn diagnostic_conversion_uses_primary_style_utf16_and_related_labels() {
    let source = "😀  primary  secondary";
    let uri = lsp_types::Url::parse("file:///labels.rid").unwrap();
    let primary_start = source.find("primary").unwrap();
    let secondary_start = source.find("secondary").unwrap();
    let mut input = diagnostic_ext(
        "E0001",
        type_checker::Severity::Warning,
        vec![
            source_label(
                TextRange::new(
                    (secondary_start as u32).into(),
                    ((secondary_start + "secondary".len()) as u32).into(),
                ),
                "secondary label",
                type_checker::LabelStyle::Secondary,
            ),
            source_label(
                TextRange::new(
                    ((primary_start - 2) as u32).into(),
                    ((primary_start + "primary".len() + 2) as u32).into(),
                ),
                "primary label",
                type_checker::LabelStyle::Primary,
            ),
        ],
    );
    input.help = Some("fix it".into());
    input.notes.push("context".into());

    let diagnostic = to_lsp(&uri, source, input).unwrap();

    assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::WARNING));
    assert_eq!(
        diagnostic.range,
        Range::new(Position::new(0, 4), Position::new(0, 11))
    );
    assert_eq!(
        diagnostic.message,
        "message\nprimary label\nhelp: fix it\nnote: context"
    );
    let related = diagnostic.related_information.unwrap();
    assert_eq!(related.len(), 1);
    assert_eq!(related[0].location.uri, uri);
    assert_eq!(related[0].message, "secondary label");
    assert_eq!(
        related[0].location.range,
        Range::new(Position::new(0, 13), Position::new(0, 22))
    );
}

#[test]
fn diagnostic_conversion_maps_every_severity() {
    let source = "x";
    let uri = lsp_types::Url::parse("file:///severity.rid").unwrap();
    let label = source_label(
        TextRange::new(0.into(), 1.into()),
        "",
        type_checker::LabelStyle::Primary,
    );
    let cases = [
        (type_checker::Severity::Error, DiagnosticSeverity::ERROR),
        (type_checker::Severity::Warning, DiagnosticSeverity::WARNING),
        (
            type_checker::Severity::Note,
            DiagnosticSeverity::INFORMATION,
        ),
        (type_checker::Severity::Help, DiagnosticSeverity::HINT),
    ];

    for (input, expected) in cases {
        let diagnostic = to_lsp(
            &uri,
            source,
            diagnostic_ext("E0001", input, vec![label.clone()]),
        )
        .unwrap();
        assert_eq!(diagnostic.severity, Some(expected));
    }
}

#[test]
fn diagnostic_conversion_rejects_non_utf8_boundaries() {
    let source = "😀";
    let uri = lsp_types::Url::parse("file:///invalid-range.rid").unwrap();
    let diagnostic = diagnostic_ext(
        "E0001",
        type_checker::Severity::Error,
        vec![source_label(
            TextRange::new(1.into(), 2.into()),
            "",
            type_checker::LabelStyle::Primary,
        )],
    );

    assert!(to_lsp(&uri, source, diagnostic).is_none());
}

#[test]
fn parser_eof_diagnostic_stays_at_user_eof_with_std() {
    let source = "fun main() {";
    let uri = lsp_types::Url::parse("file:///eof.rid").unwrap();
    let result = riddlec::pipeline::compile(source);
    let diagnostics = collect_diagnostics(&uri, source, &result);

    assert!(!diagnostics.is_empty());
    assert!(diagnostics.iter().all(|diagnostic| {
        diagnostic.range.start == Position::new(0, source.len() as u32)
            && diagnostic.range.end == Position::new(0, source.len() as u32)
    }));
}

#[test]
fn recursive_type_alias_reports_diagnostic_without_crashing() {
    let source = "type Result = Result;";
    let uri = lsp_types::Url::parse("file:///recursive-type-alias.rid").unwrap();
    let docs = HashMap::from([(
        uri.clone(),
        Document {
            text: source.into(),
            version: Some(1),
        },
    )]);

    let diagnostics = collect_document_diagnostics(&uri, source, &docs, CompileOptions::default());

    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code == Some(lsp_types::NumberOrString::String("E0391".into()))
                && diagnostic
                    .message
                    .contains("cycle detected when expanding type alias `Result`")
        }),
        "{diagnostics:#?}"
    );
}

#[test]
fn full_text_uses_latest_full_sync_change() {
    let old = TextDocumentContentChangeEvent {
        range: None,
        range_length: None,
        text: "old".into(),
    };
    let new = TextDocumentContentChangeEvent {
        range: None,
        range_length: None,
        text: "new".into(),
    };

    let mut text = "initial".to_string();
    assert!(apply_content_changes(&mut text, vec![old, new]));
    assert_eq!(text, "new");
}

#[test]
fn incremental_changes_apply_sequentially_with_utf16_ranges() {
    let mut text = "a😀c\nlast".to_string();
    let changes = vec![
        TextDocumentContentChangeEvent {
            range: Some(Range::new(Position::new(0, 1), Position::new(0, 3))),
            range_length: Some(2),
            text: "x".into(),
        },
        TextDocumentContentChangeEvent {
            range: Some(Range::new(Position::new(1, 0), Position::new(1, 4))),
            range_length: Some(4),
            text: "done".into(),
        },
    ];

    assert!(apply_content_changes(&mut text, changes));
    assert_eq!(text, "axc\ndone");
}

#[test]
fn completion_revisions_are_isolated_per_document_and_removable() {
    let revisions = RequestRevisions::default();
    let first = lsp_types::Url::parse("file:///first.rid").unwrap();
    let second = lsp_types::Url::parse("file:///second.rid").unwrap();

    let first_old = revisions.begin(&first);
    let second_current = revisions.begin(&second);
    let first_current = revisions.begin(&first);

    assert!(!revisions.is_current(&first, first_old));
    assert!(revisions.is_current(&first, first_current));
    assert!(revisions.is_current(&second, second_current));
    revisions.remove(&first);
    assert!(!revisions.is_current(&first, first_current));
}

#[test]
fn semantic_token_delta_replaces_only_the_changed_middle() {
    let token = |delta_start, token_type| SemanticToken {
        delta_line: 0,
        delta_start,
        length: 1,
        token_type,
        token_modifiers_bitset: 0,
    };
    let previous = vec![
        token(0, TOKEN_KEYWORD),
        token(2, TOKEN_VARIABLE),
        token(2, TOKEN_TYPE),
    ];
    let current = vec![
        token(0, TOKEN_KEYWORD),
        token(2, TOKEN_FUNCTION),
        token(2, TOKEN_TYPE),
    ];

    let delta = semantic_token_delta(&previous, &current, "2".into());

    assert_eq!(delta.result_id.as_deref(), Some("2"));
    assert_eq!(delta.edits.len(), 1);
    assert_eq!(delta.edits[0].start, 5);
    assert_eq!(delta.edits[0].delete_count, 5);
    assert_eq!(delta.edits[0].data.as_deref(), Some(&current[1..2]));
}

#[test]
fn completion_filters_keywords_globals_and_locals_by_prefix() {
    let source = "struct Widget {}\nfun helper() {}\nfun main(value: i32) { let local = 1; loc }";
    let local_position = position(source, source.rfind("loc").unwrap() + 3);
    let local =
        completion_items_for_source(source, local_position, CompileOptions { use_std: false });

    assert_eq!(
        local
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>(),
        vec!["local"]
    );

    let helper_start = source.find("helper() {}").unwrap();
    let helper_position = position(source, helper_start + 3);
    let globals =
        completion_items_for_source(source, helper_position, CompileOptions { use_std: false });
    let helper = globals.iter().find(|item| item.label == "helper").unwrap();
    let helper_label = helper.label_details.as_ref().unwrap();
    assert_eq!(helper_label.detail.as_deref(), Some("()"));
    assert_eq!(helper_label.description.as_deref(), Some("()"));

    let keyword_source = "fun main() { ret }";
    let keywords = completion_items_for_source(
        keyword_source,
        position(keyword_source, keyword_source.find("ret").unwrap() + 3),
        CompileOptions { use_std: false },
    );
    assert!(keywords.iter().any(|item| item.label == "return"));
}

#[test]
fn completion_matches_free_functions_case_insensitively() {
    let source = "fun Foo() {} fun main() { f }";
    let items = completion_items_for_source(
        source,
        position(source, source.rfind("f }").unwrap() + 1),
        CompileOptions { use_std: false },
    );

    assert!(items.iter().any(|item| item.label == "Foo"), "{items:#?}");
}

#[test]
fn completion_includes_struct_and_enum_names_in_type_positions() {
    let source = "struct Point {}\nenum State { Ready }\nfun inspect(value: ) {}";
    let items = completion_items_for_source(
        source,
        position(source, source.find(") {}").unwrap()),
        CompileOptions { use_std: false },
    );

    assert!(
        items.iter().any(|item| {
            item.label == "Point" && item.kind == Some(lsp_types::CompletionItemKind::STRUCT)
        }),
        "{items:#?}"
    );
    assert!(
        items.iter().any(|item| {
            item.label == "State" && item.kind == Some(lsp_types::CompletionItemKind::ENUM)
        }),
        "{items:#?}"
    );
    assert!(
        !items
            .iter()
            .any(|item| { item.kind == Some(lsp_types::CompletionItemKind::KEYWORD) }),
        "{items:#?}"
    );
    assert!(
        !items.iter().any(|item| item.label == "inspect"),
        "{items:#?}"
    );
    for unsupported in ["i128", "u128", "f16", "f128"] {
        assert!(
            !items.iter().any(|item| item.label == unsupported),
            "unexpected unsupported type {unsupported}: {items:#?}"
        );
    }
}

#[test]
fn completion_keeps_expression_candidates_after_struct_field_colons() {
    let source =
        "struct Foo { field: i32 }\nfun main() { let value = 1; let item = Foo { field: val }; }";
    let items = completion_items_for_source(
        source,
        position(source, source.find("val }").unwrap() + 3),
        CompileOptions { use_std: false },
    );

    assert!(items.iter().any(|item| item.label == "value"), "{items:#?}");
}

#[test]
fn completion_resolves_fields_and_instance_methods() {
    let source = "struct Point { x: i32, y: i32 }\nimpl Point { fun origin() -> Point { Point { x: 0, y: 0 } } fun magnitude(&self) -> i32 { self.x } fun offset(&self, value: i32) -> i32 { value } }\nfun main() { let point = Point { x: 1, y: 2 }; point. }";
    let items = completion_items_for_source(
        source,
        position(source, source.rfind("point.").unwrap() + "point.".len()),
        CompileOptions { use_std: false },
    );
    let labels = items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();

    assert!(labels.contains(&"x"), "{items:#?}");
    assert!(labels.contains(&"y"), "{items:#?}");
    assert!(labels.contains(&"magnitude"), "{items:#?}");
    assert!(labels.contains(&"offset"), "{items:#?}");
    assert!(!labels.contains(&"origin"), "{items:#?}");

    let magnitude = items.iter().find(|item| item.label == "magnitude").unwrap();
    let magnitude_label = magnitude.label_details.as_ref().unwrap();
    assert_eq!(magnitude_label.detail.as_deref(), Some("(&self)"));
    assert_eq!(magnitude_label.description.as_deref(), Some("i32"));
    assert_eq!(magnitude.insert_text.as_deref(), Some("magnitude"));

    let offset = items.iter().find(|item| item.label == "offset").unwrap();
    let offset_label = offset.label_details.as_ref().unwrap();
    assert_eq!(offset_label.detail.as_deref(), Some("(&self, value: i32)"));
    assert_eq!(offset_label.description.as_deref(), Some("i32"));
}

#[test]
fn completion_filters_member_candidates_after_the_dot() {
    let source = "struct Foo { bar: i32 }\nfun main() { let c = Foo { bar: 1 }; c.b }";
    let items = completion_items_for_source(
        source,
        position(source, source.rfind("c.b").unwrap() + "c.b".len()),
        CompileOptions { use_std: false },
    );

    assert!(items.iter().any(|item| item.label == "bar"), "{items:#?}");
}

#[test]
fn completion_recovers_incomplete_let_member_access() {
    let source = "use std::Vector;\n\nfun main() {\n    let c: Vector<i32> = Vector::new();\n    let d = c.\n}";
    let items = completion_items_for_source(
        source,
        position(source, source.find("c.\n").unwrap() + "c.".len()),
        CompileOptions::default(),
    );

    assert!(items.iter().any(|item| item.label == "len"), "{items:#?}");
}

#[test]
fn completion_resolves_slice_methods() {
    let source = "fun inspect(values: &[i32]) { values. }";
    let items = completion_items_for_source(
        source,
        position(source, source.find("values.").unwrap() + "values.".len()),
        CompileOptions::default(),
    );

    assert!(items.iter().any(|item| item.label == "len"), "{items:#?}");
    assert!(items.iter().any(|item| item.label == "get"), "{items:#?}");
    assert!(items.iter().any(|item| item.label == "iter"), "{items:#?}");
}

#[test]
fn completion_resolves_std_associated_functions() {
    let source = "fun main() { let value = Vector::ne; }";
    let items = completion_items_for_source(
        source,
        position(source, source.find("ne;").unwrap() + 2),
        CompileOptions::default(),
    );

    assert!(
        items.iter().any(|item| {
            item.label == "new"
                && item.label_details.as_ref().is_some_and(|label| {
                    label.detail.as_deref() == Some("()")
                        && label
                            .description
                            .as_deref()
                            .is_some_and(|description| description.contains("Vector"))
                })
                && item.kind == Some(lsp_types::CompletionItemKind::FUNCTION)
        }),
        "{items:#?}"
    );
}

#[test]
fn completion_includes_std_prelude_imports() {
    let source = "fun main() { Vec }";
    let items = completion_items_for_source(
        source,
        position(source, source.find("Vec").unwrap() + 3),
        CompileOptions::default(),
    );

    assert!(
        items.iter().any(|item| item.label == "Vector"),
        "{items:#?}"
    );
}

#[test]
fn completion_respects_nested_block_scope() {
    let source = "fun main() { if true { let hidden = 1; } hid }";
    let items = completion_items_for_source(
        source,
        position(source, source.rfind("hid").unwrap() + 3),
        CompileOptions { use_std: false },
    );

    assert!(
        !items.iter().any(|item| item.label == "hidden"),
        "{items:#?}"
    );
}

#[test]
fn completion_includes_for_and_match_pattern_bindings() {
    let for_source = "fun main() { for item in [1] { ite } }";
    let for_items = completion_items_for_source(
        for_source,
        position(for_source, for_source.find("ite }").unwrap() + 3),
        CompileOptions { use_std: false },
    );
    assert!(
        for_items.iter().any(|item| item.label == "item"),
        "{for_items:#?}"
    );

    let match_source = "enum Value { Item(i32) } fun main(value: Value) { match value { Value::Item(inner) => { inn } } }";
    let match_items = completion_items_for_source(
        match_source,
        position(match_source, match_source.find("inn }").unwrap() + 3),
        CompileOptions { use_std: false },
    );
    assert!(
        match_items.iter().any(|item| item.label == "inner"),
        "{match_items:#?}"
    );
}

#[test]
fn completion_includes_private_imports() {
    let source =
        "mod models { pub struct Widget {} } use crate::models::Widget; fun main() { Wid }";
    let items = completion_items_for_source(
        source,
        position(source, source.rfind("Wid").unwrap() + 3),
        CompileOptions { use_std: false },
    );

    assert!(
        items.iter().any(|item| item.label == "Widget"),
        "{items:#?}"
    );
}

#[test]
fn completion_resolves_associated_items_through_import_aliases() {
    let source = "mod models { pub struct Widget {} impl Widget { pub fun build() {} } } use crate::models::Widget as Alias; fun main() { Alias::bu }";
    let items = completion_items_for_source(
        source,
        position(source, source.rfind("bu }").unwrap() + 2),
        CompileOptions { use_std: false },
    );

    assert!(items.iter().any(|item| item.label == "build"), "{items:#?}");
}

#[test]
fn completion_loads_unopened_project_modules() {
    let root = temp_root("project-completion");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Clue.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\n",
    )
    .unwrap();
    let main = root.join("src/main.rid");
    let main_text = "mod util;\nfun main() { util::va }\n";
    fs::write(&main, main_text).unwrap();
    fs::write(root.join("src/util.rid"), "pub fun value() {}\n").unwrap();
    let uri = lsp_types::Url::from_file_path(&main).unwrap();
    let docs = HashMap::from([(
        uri.clone(),
        Document {
            text: main_text.into(),
            version: Some(1),
        },
    )]);
    let sessions = AnalysisSessions::default();

    let fallback_sessions = AnalysisSessions::default();
    let items = completion_items_for_document(
        &uri,
        &docs,
        position(main_text, main_text.find("va }").unwrap() + 2),
        CompileOptions::default(),
        &sessions,
        &fallback_sessions,
        || false,
    )
    .unwrap()
    .unwrap();

    assert!(items.iter().any(|item| item.label == "value"), "{items:#?}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn completion_uses_unsaved_project_module_overlays() {
    let root = temp_root("project-completion-overlay");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Clue.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\n",
    )
    .unwrap();
    let main = root.join("src/main.rid");
    let main_text = "mod util;\nfun main() { util::fr }\n";
    fs::write(&main, main_text).unwrap();
    let util = root.join("src/util.rid");
    fs::write(&util, "pub fun stale() {}\n").unwrap();
    let main_uri = lsp_types::Url::from_file_path(&main).unwrap();
    let util_uri = lsp_types::Url::from_file_path(&util).unwrap();
    let docs = HashMap::from([
        (
            main_uri.clone(),
            Document {
                text: main_text.into(),
                version: Some(1),
            },
        ),
        (
            util_uri,
            Document {
                text: "pub fun fresh() {}\n".into(),
                version: Some(2),
            },
        ),
    ]);
    let sessions = AnalysisSessions::default();

    let items = completion_items_for_document(
        &main_uri,
        &docs,
        position(main_text, main_text.find("fr }").unwrap() + 2),
        CompileOptions::default(),
        &sessions,
        &AnalysisSessions::default(),
        || false,
    )
    .unwrap()
    .unwrap();

    assert!(items.iter().any(|item| item.label == "fresh"), "{items:#?}");
    assert!(
        !items.iter().any(|item| item.label == "stale"),
        "{items:#?}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn project_member_completion_uses_active_module_coordinates() {
    let root = temp_root("project-member-completion-coordinates");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Clue.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\n",
    )
    .unwrap();
    fs::write(
        root.join("src/main.rid"),
        "mod padding;\nmod model;\nfun main() {}\n",
    )
    .unwrap();
    fs::write(
        root.join("src/padding.rid"),
        format!("// {}\n", "padding".repeat(80)),
    )
    .unwrap();
    let model = root.join("src/model.rid");
    let model_text = "pub struct Point { field: i32 }\npub fun complete() { let point = Point { field: 1 }; point.fi }\n";
    fs::write(&model, model_text).unwrap();
    let uri = lsp_types::Url::from_file_path(&model).unwrap();
    let docs = HashMap::from([(
        uri.clone(),
        Document {
            text: model_text.into(),
            version: Some(1),
        },
    )]);

    let items = completion_items_for_document(
        &uri,
        &docs,
        position(
            model_text,
            model_text.find("point.fi").unwrap() + "point.fi".len(),
        ),
        CompileOptions { use_std: false },
        &AnalysisSessions::default(),
        &AnalysisSessions::default(),
        || false,
    )
    .unwrap()
    .unwrap();

    assert!(items.iter().any(|item| item.label == "field"), "{items:#?}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn completion_preserves_std_member_items_through_document_sessions() {
    let uri = lsp_types::Url::parse("file:///riddle-lsp-completion.rid").unwrap();
    let text = "use std::Vector; fun main() { let c: Vector<i32> = Vector::new(); let d = c.i }";
    let docs = HashMap::from([(
        uri.clone(),
        Document {
            text: text.into(),
            version: Some(1),
        },
    )]);
    let sessions = AnalysisSessions::default();

    let items = completion_items_for_document(
        &uri,
        &docs,
        position(text, text.find("c.i").unwrap() + 3),
        CompileOptions::default(),
        &sessions,
        &AnalysisSessions::default(),
        || false,
    )
    .unwrap()
    .unwrap();

    assert!(
        items.iter().any(|item| item.label == "is_empty"),
        "{items:#?}"
    );
}

#[test]
fn project_semantic_tokens_resolve_cross_module_functions() {
    let root = temp_root("project-semantic-tokens");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Clue.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\n",
    )
    .unwrap();
    let main = root.join("src/main.rid");
    let main_text = "mod util;\nfun main() { let callable = util::make; callable; }\n";
    fs::write(&main, main_text).unwrap();
    fs::write(
        root.join("src/util.rid"),
        "pub struct Thing {}\npub fun make() -> Thing { Thing {} }\n",
    )
    .unwrap();
    let uri = lsp_types::Url::from_file_path(&main).unwrap();
    let docs = HashMap::from([(
        uri.clone(),
        Document {
            text: main_text.into(),
            version: Some(1),
        },
    )]);
    let sessions = AnalysisSessions::default();

    let tokens =
        semantic_tokens_for_document(&uri, &docs, CompileOptions::default(), &sessions).unwrap();
    let make = position(main_text, main_text.find("make").unwrap());
    assert!(semantic_token_positions(&tokens).iter().any(|token| {
        token.line == make.line
            && token.start == make.character
            && token.token_type == TOKEN_FUNCTION
    }));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn project_inlay_hints_infer_cross_module_return_types() {
    let root = temp_root("project-inlay-hints");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Clue.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\n",
    )
    .unwrap();
    let main = root.join("src/main.rid");
    let main_text = "mod util;\nfun main() { let value = util::make(); }\n";
    fs::write(&main, main_text).unwrap();
    fs::write(
        root.join("src/util.rid"),
        "pub struct Thing {}\npub fun make() -> Thing { Thing {} }\n",
    )
    .unwrap();
    let uri = lsp_types::Url::from_file_path(&main).unwrap();
    let docs = HashMap::from([(
        uri.clone(),
        Document {
            text: main_text.into(),
            version: Some(1),
        },
    )]);
    let sessions = AnalysisSessions::default();

    let hints = inlay_hints_for_document(
        &uri,
        &docs,
        Range::new(Position::new(0, 0), Position::new(2, 0)),
        CompileOptions::default(),
        &sessions,
    )
    .unwrap();

    assert!(
        hints
            .iter()
            .any(|hint| matches!(&hint.label, InlayHintLabel::String(label) if label == ": Thing"))
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn semantic_tokens_classifies_core_tokens() {
    let tokens = semantic_tokens("fun main() {\n  let mut x = \"hi\"; // ok\n}");
    let types = tokens
        .data
        .iter()
        .map(|token| token.token_type)
        .collect::<Vec<_>>();

    assert!(types.contains(&TOKEN_KEYWORD));
    assert!(types.contains(&TOKEN_FUNCTION));
    assert!(types.contains(&TOKEN_VARIABLE));
    assert!(types.contains(&TOKEN_STRING));
    assert!(types.contains(&TOKEN_COMMENT));
}

#[test]
fn semantic_tokens_classify_every_keyword() {
    let source = "let fun struct if else while break continue return as self mod use mut pub super crate enum trait impl match const type extern unsafe for in where move true false";
    let tokens = semantic_tokens(source);

    assert_eq!(
        tokens
            .data
            .iter()
            .filter(|token| token.token_type == TOKEN_KEYWORD)
            .count(),
        source.split_whitespace().count()
    );
}

#[test]
fn inlay_hints_show_inferred_types() {
    let source =
        "struct Foo{}\n\nfun main(){\n    let a = Foo{};\n    let b = a;\n    let c = a;\n}";
    let hints = inlay_hints_for_source(
        source,
        Range::new(Position::new(0, 0), Position::new(u32::MAX, 0)),
    );
    let type_hints = hints
        .iter()
        .filter(|hint| hint.kind == Some(lsp_types::InlayHintKind::TYPE))
        .collect::<Vec<_>>();

    assert_eq!(hints.len(), 2);
    assert_eq!(type_hints.len(), 2);
    assert!(type_hints.iter().all(|hint| {
        matches!(&hint.label, lsp_types::InlayHintLabel::String(label) if label == ": Foo")
    }));
}

#[test]
fn inlay_hints_skip_invalid_initializers() {
    let source = "fun main(){\n    let a = 1;\n    let b = a as 2;\n    let c = missing;\n}";
    let hints = inlay_hints_for_source(
        source,
        Range::new(Position::new(0, 0), Position::new(u32::MAX, 0)),
    );

    assert_eq!(hints.len(), 1, "{hints:#?}");
    assert!(matches!(
        &hints[0].label,
        lsp_types::InlayHintLabel::String(label) if label == ": i32"
    ));
}

#[test]
fn semantic_tokens_use_utf16_lengths() {
    let tokens = semantic_tokens("let x = '😀';");
    let string = tokens
        .data
        .iter()
        .find(|token| token.token_type == TOKEN_STRING)
        .unwrap();

    assert_eq!(string.length, 4);
}

#[test]
fn semantic_tokens_keep_utf16_positions_across_lines() {
    let tokens = semantic_token_positions(&semantic_tokens("// 😀\nfun main() {}"));
    let function = tokens
        .iter()
        .find(|token| token.token_type == TOKEN_FUNCTION)
        .unwrap();

    assert_eq!(function.line, 1);
    assert_eq!(function.start, 4);
    assert_eq!(function.length, 4);
}

#[test]
fn semantic_tokens_exclude_cr_from_crlf_comment_lengths() {
    let tokens = semantic_token_positions(&semantic_tokens("// ok\r\nfun main() {}"));
    let comment = tokens
        .iter()
        .find(|token| token.token_type == TOKEN_COMMENT)
        .unwrap();
    let function = tokens
        .iter()
        .find(|token| token.token_type == TOKEN_FUNCTION)
        .unwrap();

    assert_eq!((comment.line, comment.start, comment.length), (0, 0, 5));
    assert_eq!((function.line, function.start, function.length), (1, 4, 4));
}

#[test]
fn semantic_tokens_highlight_str_and_self_as_keywords() {
    let source = "struct Foo { text: &str }\nimpl Foo { fun get(&self) -> &str { self.text } }";
    let tokens = semantic_token_positions(&semantic_tokens(source));

    assert!(
        tokens.iter().any(|token| {
            token.line == 0
                && token.start == 20
                && token.length == 3
                && token.token_type == TOKEN_KEYWORD
        }),
        "{tokens:#?}"
    );
    assert!(
        tokens.iter().any(|token| {
            token.line == 1
                && token.start == 20
                && token.length == 4
                && token.token_type == TOKEN_KEYWORD
        }),
        "{tokens:#?}"
    );
    assert!(
        tokens.iter().any(|token| {
            token.line == 1
                && token.start == 36
                && token.length == 4
                && token.token_type == TOKEN_KEYWORD
        }),
        "{tokens:#?}"
    );
}

#[test]
fn semantic_tokens_distinguish_methods_structs_enums_and_traits() {
    let source = r#"struct Point {}
enum State { Ready }
trait Draw { fun draw(&self); }
impl Draw for Point { fun draw(&self) {} }
fun main() {
    let point = Point {};
    point.draw();
    let state = State::Ready;
}"#;
    let tokens = semantic_token_positions(&semantic_tokens(source));
    let symbols = tokens
        .iter()
        .map(|token| {
            let line = source.lines().nth(token.line as usize).unwrap();
            let start = token.start as usize;
            let end = start + token.length as usize;
            (
                &line[start..end],
                token.token_type,
                token.token_modifiers_bitset,
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        symbols
            .iter()
            .filter(|(text, kind, _)| *text == "Point" && *kind == TOKEN_STRUCT)
            .count(),
        3,
        "{symbols:#?}"
    );
    assert_eq!(
        symbols
            .iter()
            .filter(|(text, kind, _)| matches!((*text, *kind), ("State" | "Ready", TOKEN_ENUM)))
            .count(),
        4,
        "{symbols:#?}"
    );
    assert_eq!(
        symbols
            .iter()
            .filter(|(text, kind, _)| *text == "Draw" && *kind == TOKEN_INTERFACE)
            .count(),
        2,
        "{symbols:#?}"
    );
    assert_eq!(
        symbols
            .iter()
            .filter(|(text, kind, _)| *text == "draw" && *kind == TOKEN_METHOD)
            .count(),
        3,
        "{symbols:#?}"
    );
    assert_eq!(
        symbols
            .iter()
            .filter(|(text, kind, modifiers)| {
                *text == "draw" && *kind == TOKEN_METHOD && *modifiers == MOD_DECLARATION
            })
            .count(),
        2,
        "{symbols:#?}"
    );
}

#[test]
fn semantic_tokens_classify_std_structs_and_associated_new() {
    let source = include_str!("../../std/std/vector.rid");
    let tokens = semantic_token_positions(&semantic_tokens_for_source_with_options(
        source,
        CompileOptions::default(),
        true,
    ));
    let symbols = tokens
        .iter()
        .map(|token| {
            let line = source.lines().nth(token.line as usize).unwrap();
            let start = token.start as usize;
            let end = start + token.length as usize;
            (
                &line[start..end],
                token.token_type,
                token.token_modifiers_bitset,
            )
        })
        .collect::<Vec<_>>();

    assert!(
        symbols.iter().any(|(text, kind, modifiers)| {
            *text == "Vector" && *kind == TOKEN_STRUCT && *modifiers & MOD_DEFAULT_LIBRARY != 0
        }),
        "{symbols:#?}"
    );

    let new_tokens = symbols
        .iter()
        .filter(|(text, kind, _)| *text == "new" && *kind == TOKEN_METHOD)
        .collect::<Vec<_>>();
    assert!(!new_tokens.is_empty(), "{symbols:#?}");
    assert!(
        new_tokens.iter().all(|(_, _, modifiers)| {
            *modifiers & MOD_STATIC != 0 && *modifiers & MOD_DEFAULT_LIBRARY != 0
        }),
        "{symbols:#?}"
    );
}

#[test]
fn semantic_tokens_classify_primitives_type_annotations_and_free_functions() {
    let source = r#"fun make(value: i32, size: usize, enabled: bool) -> Vector<i32> {
    Vector::new()
}

fun main() {
    let s: Vector<i32> = make(1i32, 0usize, true);
}"#;
    let tokens = semantic_token_positions(&semantic_tokens_for_source_with_options(
        source,
        CompileOptions::default(),
        false,
    ));
    let symbols = tokens
        .iter()
        .map(|token| {
            let line = source.lines().nth(token.line as usize).unwrap();
            let start = token.start as usize;
            let end = start + token.length as usize;
            (
                &line[start..end],
                token.token_type,
                token.token_modifiers_bitset,
            )
        })
        .collect::<Vec<_>>();

    for primitive in ["i32", "usize", "bool"] {
        assert!(
            symbols
                .iter()
                .any(|(text, kind, _)| { *text == primitive && *kind == TOKEN_KEYWORD }),
            "{primitive}: {symbols:#?}"
        );
    }
    assert_eq!(
        symbols
            .iter()
            .filter(|(text, kind, modifiers)| {
                *text == "Vector" && *kind == TOKEN_STRUCT && *modifiers & MOD_DEFAULT_LIBRARY != 0
            })
            .count(),
        3,
        "{symbols:#?}"
    );
    assert_eq!(
        symbols
            .iter()
            .filter(|(text, kind, _)| *text == "make" && *kind == TOKEN_FUNCTION)
            .count(),
        2,
        "{symbols:#?}"
    );
    assert_eq!(
        symbols
            .iter()
            .filter(|(text, kind, modifiers)| {
                matches!(*text, "make" | "main")
                    && *kind == TOKEN_FUNCTION
                    && *modifiers == MOD_DECLARATION
            })
            .count(),
        2,
        "{symbols:#?}"
    );
}

#[test]
fn semantic_tokens_mark_mutable_locals_like_rust_analyzer() {
    let tokens = semantic_tokens("fun main() { let x = 1; x; let mut y = 2; y; }");
    let variables = tokens
        .data
        .iter()
        .filter(|token| token.token_type == TOKEN_VARIABLE)
        .map(|token| token.token_modifiers_bitset)
        .collect::<Vec<_>>();

    // Immutable locals now appear at use sites with modifier 0;
    // mutable locals carry MOD_MUTABLE at use sites and MOD_DECLARATION|MOD_MUTABLE at their binding.
    assert_eq!(
        variables,
        vec![0, MOD_DECLARATION | MOD_MUTABLE, MOD_MUTABLE]
    );
}

#[test]
fn semantic_tokens_prefer_resolved_variables_over_lexical_types() {
    let tokens = semantic_tokens("fun main() { let mut Foo = 1; Foo; }");
    let types = tokens
        .data
        .iter()
        .map(|token| token.token_type)
        .collect::<Vec<_>>();

    assert_eq!(
        types
            .iter()
            .filter(|token_type| **token_type == TOKEN_VARIABLE)
            .count(),
        2
    );
    assert!(!types.contains(&TOKEN_TYPE));
}

#[test]
fn semantic_tokens_prefer_immutable_locals_over_same_named_structs() {
    let source = "struct Foo {}\nfun main() { let Foo = 1; Foo; }";
    let use_position = position(source, source.rfind("Foo;").unwrap());
    let tokens = semantic_token_positions(&semantic_tokens(source));
    let token = tokens
        .iter()
        .find(|token| token.line == use_position.line && token.start == use_position.character)
        .unwrap();

    assert_eq!(token.token_type, TOKEN_VARIABLE, "{tokens:#?}");
}

#[test]
fn collect_diagnostics_ignores_appended_std_diagnostics() {
    let source = include_str!("../../std/std/array.rid");
    let result = riddlec::pipeline::compile(source);
    let uri = lsp_types::Url::parse("file:///std/std/array.rid").unwrap();
    let diagnostics = collect_diagnostics(&uri, source, &result);

    assert!(
        diagnostics.iter().all(|diagnostic| !diagnostic
            .message
            .contains("expected IntoIter<T, N>, got IntoIter<T, N>")),
        "{diagnostics:#?}"
    );
}

#[test]
fn project_diagnostics_use_unsaved_module_source() {
    let root = temp_root("project-diagnostics");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Clue.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\n",
    )
    .unwrap();
    fs::write(
        root.join("src/main.rid"),
        "mod util;\nfun main() -> i32 { util::value() }\n",
    )
    .unwrap();
    let util = root.join("src/util.rid");
    fs::write(&util, "pub fun value() -> i32 { 1 }\n").unwrap();
    let uri = lsp_types::Url::from_file_path(&util).unwrap();
    let text = "pub fun value() -> i32 { missing }\n".to_string();
    let docs = HashMap::from([(
        uri.clone(),
        Document {
            text: text.clone(),
            version: Some(1),
        },
    )]);

    let diagnostics = collect_document_diagnostics(&uri, &text, &docs, CompileOptions::default());

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("unresolved name: `missing`")),
        "{diagnostics:#?}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn project_diagnostics_include_unopened_modules() {
    let root = temp_root("unopened-module");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Clue.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\n",
    )
    .unwrap();
    let main = root.join("src/main.rid");
    let main_text = "mod util;\nfun main() { util::value(); }\n".to_string();
    fs::write(&main, &main_text).unwrap();
    let util = root.join("src/util.rid");
    let util_text = "pub fun value() { missing; }\n";
    fs::write(&util, util_text).unwrap();
    let main_uri = lsp_types::Url::from_file_path(&main).unwrap();
    let docs = HashMap::from([(
        main_uri,
        Document {
            text: main_text,
            version: Some(1),
        },
    )]);

    let published = collect_workspace_diagnostics(&docs, CompileOptions::default());
    let util_uri = lsp_types::Url::from_file_path(fs::canonicalize(&util).unwrap()).unwrap();
    let util_diagnostics = published.iter().find(|item| item.uri == util_uri).unwrap();
    let unresolved = util_diagnostics
        .diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.code == Some(lsp_types::NumberOrString::String("E0050".into()))
        })
        .unwrap();
    let start = util_text.find("missing").unwrap();

    assert_eq!(util_diagnostics.version, None);
    assert_eq!(
        unresolved.range,
        range(
            util_text,
            TextRange::new(
                (start as u32).into(),
                ((start + "missing".len()) as u32).into(),
            ),
        )
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn project_diagnostics_preserve_the_open_document_uri() {
    let root = temp_root("open-uri-identity");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Clue.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\n",
    )
    .unwrap();
    let main = root.join("src/main.rid");
    fs::write(&main, "fun main() {}\n").unwrap();

    let aliased_path = PathBuf::from(main.as_os_str().to_string_lossy().to_ascii_uppercase());
    let opened_uri = lsp_types::Url::from_file_path(&aliased_path).unwrap();
    let docs = HashMap::from([(
        opened_uri.clone(),
        Document {
            text: "fun main() { missing; }\n".into(),
            version: Some(7),
        },
    )]);

    let published = collect_workspace_diagnostics(&docs, CompileOptions::default());
    let item = published
        .iter()
        .find(|item| {
            item.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == Some(lsp_types::NumberOrString::String("E0050".into()))
            })
        })
        .unwrap();

    assert_eq!(item.uri, opened_uri);
    assert_eq!(item.version, Some(7));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn mapped_diagnostic_keeps_cross_file_related_information() {
    let root = temp_root("cross-file-labels");
    fs::create_dir_all(&root).unwrap();
    let main = root.join("main.rid");
    let main_text = "mod util;\nfun main() { root_error; }\n";
    fs::write(&main, main_text).unwrap();
    let util = root.join("util.rid");
    let util_text = "pub fun value() { related; }\n";
    fs::write(&util, util_text).unwrap();
    let loaded = riddlec::pipeline::load_source_file(&main).unwrap();
    let primary_start = loaded.source.find("root_error").unwrap();
    let secondary_start = loaded.source.find("related").unwrap();
    let input = diagnostic_ext(
        "E0001",
        type_checker::Severity::Error,
        vec![
            source_label(
                TextRange::new(
                    (secondary_start as u32).into(),
                    ((secondary_start + "related".len()) as u32).into(),
                ),
                "declared here",
                type_checker::LabelStyle::Secondary,
            ),
            source_label(
                TextRange::new(
                    (primary_start as u32).into(),
                    ((primary_start + "root_error".len()) as u32).into(),
                ),
                "",
                type_checker::LabelStyle::Primary,
            ),
        ],
    );

    let (uri, diagnostic) = to_lsp_mapped(&loaded.source_map, input).unwrap();
    let related = diagnostic.related_information.unwrap();

    assert_eq!(
        uri,
        lsp_types::Url::from_file_path(fs::canonicalize(&main).unwrap()).unwrap()
    );
    assert_eq!(related.len(), 1);
    assert_eq!(
        related[0].location.uri,
        lsp_types::Url::from_file_path(fs::canonicalize(&util).unwrap()).unwrap()
    );
    let related_start = util_text.find("related").unwrap();
    assert_eq!(
        related[0].location.range,
        range(
            util_text,
            TextRange::new(
                (related_start as u32).into(),
                ((related_start + "related".len()) as u32).into(),
            ),
        )
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn reachable_diagnostic_producers_have_exact_primary_and_lsp_spans() {
    let cases = [
        (
            "E0001",
            "let initializer",
            "fun main() { let emoji = \"😀\"; let value: bool = 1; }",
            "let value: bool = 1;",
            "let value: bool = 1;",
        ),
        (
            "E0002",
            "if branches",
            "fun main() { let value = if true { 1 } else { false }; }",
            "if true { 1 } else { false }",
            "if true { 1 } else { false }",
        ),
        (
            "E0003",
            "remainder requires integer operands",
            "fun main() { let value = true % false; }",
            "true % false",
            "true % false",
        ),
        (
            "E0004",
            "cannot call value",
            "fun main() { let value = 1; value(); }",
            "value",
            "; value",
        ),
        (
            "E0005",
            "expects 1 argument",
            "fun takes(value: i32) {} fun main() { takes(); }",
            "takes()",
            "takes()",
        ),
        (
            "E0006",
            "unknown field",
            "struct Point { x: i32 } fun main() { let value = Point { y: 1 }; }",
            "Point { y: 1 }",
            "Point { y: 1 }",
        ),
        (
            "E0007",
            "missing field",
            "struct Point { x: i32, y: i32 } fun main() { let value = Point { x: 1 }; }",
            "Point { x: 1 }",
            "Point { x: 1 }",
        ),
        (
            "E0008",
            "cannot dereference",
            "fun main() { let value = *1; }",
            "1",
            "*1",
        ),
        (
            "E0009",
            "struct literal does not resolve",
            "fun main() { let value = Missing { x: 1 }; }",
            "Missing { x: 1 }",
            "Missing { x: 1 }",
        ),
        (
            "E0010",
            "tuple pattern expects",
            "fun check(value: (i32,)) { match value { (left, right) => {} } }",
            "(left, right)",
            "(left, right)",
        ),
        (
            "E0011",
            "out of range for `u8`",
            "fun check(value: u8) { match value { 256 => {}, _ => {} } }",
            "256",
            "256",
        ),
        (
            "E0012",
            "cannot cast `bool` to `f64`",
            "fun main() { let value = true as f64; }",
            "true as f64",
            "true as f64",
        ),
        (
            "E0013",
            "unknown method",
            "fun main() { let value = 1; value.missing(); }",
            "value.missing()",
            "value.missing()",
        ),
        (
            "E0020",
            "duplicate method",
            "trait Foo { fun bar(); fun bar(); }",
            "bar",
            "fun bar(); fun bar",
        ),
        (
            "E0022",
            "duplicate associated type",
            "trait Foo { type Item; type Item; }",
            "Item",
            "type Item; type Item",
        ),
        (
            "E0023",
            "unknown trait",
            "struct Point {}\nimpl Missing for Point {}",
            "Missing",
            "impl Missing",
        ),
        (
            "E0024",
            "duplicate method",
            "struct Point {}\nimpl Point { fun bar() {} fun bar() {} }",
            "bar",
            "fun bar() {} fun bar",
        ),
        (
            "E0025",
            "duplicate associated type",
            "struct Point {}\nimpl Point { type Item = i32; type Item = bool; }",
            "Item",
            "type Item = i32; type Item",
        ),
        (
            "E0026",
            "missing method",
            "trait Foo { fun bar(); }\nstruct Point {}\nimpl Foo for Point {}",
            "Point",
            "impl Foo for Point",
        ),
        (
            "E0027",
            "missing associated type",
            "trait Foo { type Item; }\nstruct Point {}\nimpl Foo for Point {}",
            "Point",
            "impl Foo for Point",
        ),
        (
            "E0028",
            "parameter count mismatch",
            "trait Foo { fun bar(value: i32); }\nstruct Point {}\nimpl Foo for Point { fun bar() {} }",
            "bar",
            "impl Foo for Point { fun bar",
        ),
        (
            "E0029",
            "parameter 1 type mismatch",
            "trait Foo { fun bar(value: i32); }\nstruct Point {}\nimpl Foo for Point { fun bar(value: bool) {} }",
            "bool",
            "value: bool",
        ),
        (
            "E0030",
            "return type mismatch",
            "trait Foo { fun bar() -> i32; }\nstruct Point {}\nimpl Foo for Point { fun bar() -> bool { true } }",
            "bool",
            "-> bool",
        ),
        (
            "E0031",
            "not declared as mutable",
            "fun main() { let value = 1; value = 2; }",
            "value = 2",
            "; value = 2",
        ),
        (
            "E0032",
            "expects 1 type argument",
            "struct Box<T> { value: T }\nfun main() { let value: Box<i32, bool>; }",
            "Box<i32, bool>",
            "Box<i32, bool>",
        ),
        (
            "E0033",
            "calling `g`",
            "struct Wrap<T> { inner: T } fun f<T>(x: T) -> T { return g(Wrap { inner: x }); } fun g<T>(x: T) -> T { return f(Wrap { inner: x }); }",
            "g(Wrap { inner: x })",
            "g(Wrap { inner: x })",
        ),
        (
            "E0034",
            "unknown type `Missing`",
            "fun main() { let value: Missing; }",
            "Missing",
            "Missing",
        ),
        (
            "E0035",
            "missing `IntoIterator` trait",
            "fun main() { for item in 1 {} }",
            "for item in 1 {}",
            "for item in 1 {}",
        ),
        (
            "E0036",
            "requires `PartialEq`",
            "#[lang = \"partial_eq\"] trait PartialEq {}\n#[lang = \"eq\"] trait Eq: PartialEq {}\nstruct Point {}\nimpl Eq for Point {}",
            "Point",
            "impl Eq for Point",
        ),
        (
            "E0037",
            "not strictly smaller",
            "trait Foo {}\nstruct Vec<T> { value: T }\nimpl<T> Foo for T where Vec<T>: Foo {}",
            "Foo",
            "Vec<T>: Foo",
        ),
        (
            "E0038",
            "requires a payload pattern",
            "enum State { Ready, Done(i32) }\nfun check(value: State) { match value { State::Done => {} } }",
            "State::Done",
            "State::Done",
        ),
        (
            "E0039",
            "non-exhaustive match",
            "fun check(value: bool) { match value { true => {} } }",
            "match value { true => {} }",
            "match value { true => {} }",
        ),
        (
            "E0040",
            "invalid integer literal",
            "fun main() { let value = 18446744073709551616; }",
            "18446744073709551616",
            "18446744073709551616",
        ),
        (
            "E0041",
            "non-Copy field",
            "#[lang = \"copy\"] trait Copy {}\nstruct Token { value: i32 }\nstruct Wrapper { value: Token }\nimpl Copy for Wrapper {}",
            "Wrapper",
            "impl Copy for Wrapper",
        ),
        (
            "E0042",
            "`break` outside",
            "fun main() { break; }",
            "break;",
            "break;",
        ),
        (
            "E0043",
            "contains unsized `str`",
            "fun main() { let value: str; }",
            "let value: str;",
            "let value: str;",
        ),
        (
            "E0044",
            "unknown supertrait `Missing`",
            "trait Child: Missing {}",
            "Missing",
            "Missing",
        ),
        (
            "E0045",
            "parameter `x`",
            "fun main() { let identity = fun(x) { x }; }",
            "x",
            "fun(x",
        ),
        (
            "E0047",
            "conflicting implementations",
            "trait Foo {}\nstruct Point {}\nimpl Foo for Point {}\nimpl Foo for Point {}",
            "Point",
            "impl Foo for Point {}\nimpl Foo for Point",
        ),
        (
            "E0050",
            "unresolved name",
            "fun main() { missing; }",
            "missing",
            "missing",
        ),
        (
            "E0053",
            "unknown lang item",
            "#[lang = \"unknown_item\"] trait Foo {}",
            "Foo",
            "#[lang = \"unknown_item\"] trait Foo",
        ),
        (
            "E0054",
            "field `value` of struct `Secret` is private",
            "mod model { pub struct Secret { value: i32 } pub fun make() -> Secret { Secret { value: 1 } } } fun main() { model::make().value; }",
            "model::make().value",
            "model::make().value",
        ),
        (
            "E0055",
            "`Copy` cannot be implemented",
            "#[lang = \"copy\"] trait Copy {} #[lang = \"drop\"] trait Drop { fun drop(&mut self); } struct Guard {} impl Drop for Guard { fun drop(&mut self) {} } impl Copy for Guard {}",
            "Guard",
            "impl Copy for Guard",
        ),
        (
            "E0056",
            "explicit destructor calls are not allowed",
            "#[lang = \"drop\"] trait Drop { fun drop(&mut self); } struct Guard {} impl Drop for Guard { fun drop(&mut self) {} } fun main() { let mut guard = Guard {}; guard.drop(); }",
            "guard.drop()",
            "guard.drop()",
        ),
        (
            "E0051",
            "empty use declaration",
            "use crate;\nfun main() {}",
            "crate",
            "crate",
        ),
        (
            "E0052",
            "glob import target not found",
            "use missing::*;\nfun main() {}",
            "missing::*",
            "missing::*",
        ),
        (
            "E0072",
            "recursive type",
            "enum Loop { Next(Loop) }",
            "Loop",
            "enum Loop",
        ),
        (
            "E0100",
            "use of moved value",
            "struct Point { x: i32 } fun main() { let original = Point { x: 1 }; let moved = original; let second = original; }",
            "original",
            "let second = original",
        ),
        (
            "E0300",
            "borrow `point` as mutable",
            "struct Point { x: i32 } fun main() { let mut point = Point { x: 1 }; let shared = &point; let mutable = &mut point; }",
            "&mut point",
            "&mut point",
        ),
        (
            "E0301",
            "borrow `point` as immutable",
            "struct Point { x: i32 } fun main() { let mut point = Point { x: 1 }; let mutable = &mut point; let shared = &point; }",
            "&point",
            "&point",
        ),
        (
            "E0302",
            "borrow `point` as mutable more than once",
            "struct Point { x: i32 } fun main() { let mut point = Point { x: 1 }; let first = &mut point; let second = &mut point; }",
            "&mut point",
            "let second = &mut point",
        ),
        (
            "E0303",
            "assign to `point` while borrowed",
            "struct Point { x: i32 } fun main() { let mut point = Point { x: 1 }; let shared = &point; point = Point { x: 2 }; }",
            "point = Point { x: 2 }",
            "point = Point { x: 2 }",
        ),
        (
            "E0304",
            "cannot move `point` while borrowed",
            "struct Point { x: i32 } fun main() { let point = Point { x: 1 }; let shared = &point; let moved = point; }",
            "point",
            "let moved = point",
        ),
        (
            "E0305",
            "cannot move out of a field",
            "#[lang = \"drop\"] trait Drop { fun drop(&mut self); } struct Token {} struct Guard { token: Token } impl Drop for Guard { fun drop(&mut self) {} } fun consume(value: Token) {} fun main() { let guard = Guard { token: Token {} }; consume(guard.token); }",
            "guard.token",
            "guard.token",
        ),
        (
            "E0306",
            "cannot outlive its owner",
            "#[lang = \"drop\"] trait Drop { fun drop(&mut self); } struct Guard {} impl Drop for Guard { fun drop(&mut self) {} } fun leak() -> &Guard { let guard = Guard {}; return &guard; }",
            "&guard",
            "return &guard",
        ),
        (
            "E0307",
            "cannot move pattern binding `token` in a match guard",
            "struct Token {} enum MaybeToken { Some(Token), None } fun consume(value: Token) -> bool { true } fun main(value: MaybeToken) { match value { MaybeToken::Some(token) if consume(token) => {}, MaybeToken::Some(token) => {}, MaybeToken::None => {} } }",
            "token",
            "consume(token",
        ),
        (
            "E0308",
            "cannot move out of dereference",
            "struct Token {} fun main() { let mut token = Token {}; let reference = &mut token; let moved = *reference; }",
            "*reference",
            "let moved = *reference",
        ),
        (
            "E0391",
            "cycle detected when expanding type alias",
            "type Result = Result;",
            "Result",
            "type Result",
        ),
    ];
    let uri = lsp_types::Url::parse("file:///producer-spans.rid").unwrap();

    for &code in DOCUMENTED_ERROR_CODES {
        let expected_count = usize::from(!SOURCE_UNREACHABLE_CODES.contains(&code));
        assert_eq!(
            cases.iter().filter(|case| case.0 == code).count(),
            expected_count,
            "unexpected producer fixture count for {code}"
        );
    }

    for (code, message, source, expected, marker) in cases {
        let result =
            riddlec::pipeline::compile_with_options(source, CompileOptions { use_std: false });
        assert!(
            result.parse_errors.is_empty(),
            "{}: {:#?}",
            code,
            result.parse_errors
        );
        let diagnostic = result
            .hir_diagnostics
            .iter()
            .chain(result.type_result.diagnostics.iter())
            .chain(result.analysis_diagnostics.iter())
            .find(|diagnostic| {
                diagnostic.code == code && diagnostic.message.contains(message)
            })
            .unwrap_or_else(|| {
                panic!(
                    "missing {code} containing {message:?}; HIR: {:#?}; type: {:#?}; analysis: {:#?}",
                    result.hir_diagnostics,
                    result.type_result.diagnostics,
                    result.analysis_diagnostics,
                )
            });
        let primary = diagnostic
            .labels
            .iter()
            .find(|label| label.style == type_checker::LabelStyle::Primary)
            .unwrap();
        let actual = &source[usize::from(primary.range.start())..usize::from(primary.range.end())];
        let marker_start = source
            .find(marker)
            .unwrap_or_else(|| panic!("{code}: missing marker {marker:?}"));
        assert!(
            marker.ends_with(expected),
            "{code}: invalid marker {marker:?}"
        );
        let expected_start = marker_start + marker.len() - expected.len();
        let expected_end = expected_start + expected.len();

        assert_eq!(actual, expected, "{code}: {diagnostic:#?}");
        assert_eq!(
            usize::from(primary.range.start()),
            expected_start,
            "{code}: {diagnostic:#?}"
        );
        let lsp = to_lsp(&uri, source, diagnostic.to_ext()).unwrap();
        assert_eq!(
            lsp.range,
            Range::new(
                position(source, expected_start),
                position(source, expected_end),
            ),
            "{code}: {diagnostic:#?}"
        );
    }
}

#[test]
fn closure_diagnostic_spans_point_at_the_relevant_source() {
    let cases = [
        (
            "E0031",
            "mutable closure",
            "fun main() { let mut total = 0; let add = fun() { total += 1; }; add(); }",
            "add",
            false,
        ),
        (
            "E0100",
            "use of moved value: `once`",
            "struct Token { value: i32 } fun take(value: Token) {} fun main() { let token = Token { value: 1 }; let once = fun() { take(token); }; once(); once(); }",
            "once",
            true,
        ),
        (
            "E0303",
            "assign to `base` while borrowed",
            "fun main() { let mut base = 1; let read = fun() { base }; base = 2; read(); }",
            "base = 2",
            true,
        ),
        (
            "E0046",
            "infinite type",
            "fun main() { let id = fun(value) { value }; id(id); }",
            "id(id)",
            true,
        ),
        (
            "E0031",
            "immutable parameter",
            "fun run(callback: impl FnMut() -> i32) -> i32 { callback() }",
            "callback",
            false,
        ),
        (
            "E0047",
            "only impl Fn",
            "trait Display {} fun show(value: impl Display) {}",
            "impl Display",
            false,
        ),
        (
            "E0001",
            "unsafe function",
            "fun apply(f: impl Fn(i32) -> i32, value: i32) -> i32 { f(value) } unsafe fun dangerous(value: i32) -> i32 { value } fun main() -> i32 { apply(dangerous, 1) }",
            "dangerous",
            true,
        ),
    ];
    let uri = lsp_types::Url::parse("file:///closure-spans.rid").unwrap();

    for (code, message, source, expected, use_last) in cases {
        let result =
            riddlec::pipeline::compile_with_options(source, CompileOptions { use_std: false });
        let diagnostic = result
            .type_result
            .diagnostics
            .iter()
            .chain(result.analysis_diagnostics.iter())
            .find(|diagnostic| diagnostic.code == code && diagnostic.message.contains(message))
            .unwrap_or_else(|| panic!("missing {code} containing {message:?}"));
        let primary = diagnostic
            .labels
            .iter()
            .find(|label| label.style == type_checker::LabelStyle::Primary)
            .unwrap();
        let start = if use_last {
            source.rfind(expected)
        } else {
            source.find(expected)
        }
        .unwrap();
        let end = start + expected.len();

        assert_eq!(
            &source[usize::from(primary.range.start())..usize::from(primary.range.end())],
            expected,
            "{code}: {diagnostic:#?}"
        );
        let lsp = to_lsp(&uri, source, diagnostic.to_ext()).unwrap();
        assert_eq!(
            lsp.range,
            Range::new(position(source, start), position(source, end),),
            "{code}: {diagnostic:#?}"
        );
    }

    let source =
        "fun main() { let value = if true { fun(x: i32) { x } } else { fun(x: i32) { x } }; }";
    let result = riddlec::pipeline::compile_with_options(source, CompileOptions { use_std: false });
    let diagnostic = result
        .type_result
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "E0002")
        .expect("missing distinct anonymous function type diagnostic");
    assert_eq!(
        diagnostic
            .labels
            .iter()
            .filter(|label| label.style == type_checker::LabelStyle::Primary)
            .count(),
        1
    );
    for label in &diagnostic.labels {
        assert!(!source[label.range].trim().is_empty());
    }
}

fn position(source: &str, offset: usize) -> Position {
    let prefix = &source[..offset];
    let line_start = prefix.rfind('\n').map_or(0, |newline| newline + 1);
    Position::new(
        prefix.bytes().filter(|byte| *byte == b'\n').count() as u32,
        source[line_start..offset].encode_utf16().count() as u32,
    )
}

fn range(source: &str, range: TextRange) -> Range {
    Range::new(
        position(source, range.start().into()),
        position(source, range.end().into()),
    )
}

#[test]
fn project_diagnostics_follow_peer_overlay_removal() {
    let root = temp_root("peer-overlay-removal");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Clue.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\n",
    )
    .unwrap();
    let main = root.join("src/main.rid");
    let main_text = "mod util;\nfun main() -> i32 { util::value() }\n".to_string();
    fs::write(&main, &main_text).unwrap();
    let util = root.join("src/util.rid");
    fs::write(&util, "pub fun value() -> i32 { 1 }\n").unwrap();
    let main_uri = lsp_types::Url::from_file_path(&main).unwrap();
    let util_uri = lsp_types::Url::from_file_path(&util).unwrap();
    let mut docs = HashMap::from([
        (
            main_uri.clone(),
            Document {
                text: main_text.clone(),
                version: Some(1),
            },
        ),
        (
            util_uri.clone(),
            Document {
                text: "pub fun other() -> i32 { 1 }\n".into(),
                version: Some(1),
            },
        ),
    ]);

    let mut sessions = DiagnosticSessions::default();
    let stale = collect_workspace_diagnostics_with_sessions(
        &docs,
        CompileOptions::default(),
        &mut sessions,
    )
    .into_iter()
    .find(|published| published.uri == main_uri)
    .unwrap()
    .diagnostics;
    assert!(
        stale
            .iter()
            .any(|diagnostic| diagnostic.message.contains("unresolved")),
        "{stale:#?}"
    );

    docs.remove(&util_uri);
    let refreshed = collect_workspace_diagnostics_with_sessions(
        &docs,
        CompileOptions::default(),
        &mut sessions,
    )
    .into_iter()
    .find(|published| published.uri == main_uri)
    .unwrap()
    .diagnostics;
    assert!(refreshed.is_empty(), "{refreshed:#?}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn parse_args_accepts_no_std() {
    let args = vec!["riddle-lsp".into(), "--no-std".into()];
    let opts = parse_args(&args).unwrap();

    assert!(!opts.compile_options.use_std);
}

#[test]
fn parse_args_accepts_completion_delay_ms() {
    let args = vec![
        "riddle-lsp".into(),
        "--completion-delay-ms".into(),
        "25".into(),
    ];
    let opts = parse_args(&args).unwrap();

    assert_eq!(opts.completion_delay, Duration::from_millis(25));
}

#[test]
fn workspace_analysis_can_be_cancelled_between_documents() {
    let docs = HashMap::from([
        (
            lsp_types::Url::parse("untitled:first.rid").unwrap(),
            Document {
                text: "fun first() {}".into(),
                version: Some(1),
            },
        ),
        (
            lsp_types::Url::parse("untitled:second.rid").unwrap(),
            Document {
                text: "fun second() {}".into(),
                version: Some(1),
            },
        ),
    ]);
    let polls = Cell::new(0);
    let result = collect_workspace_diagnostics_cancellable(
        &docs,
        CompileOptions::default(),
        &mut DiagnosticSessions::default(),
        || {
            let next = polls.get() + 1;
            polls.set(next);
            next > 1
        },
    );

    assert!(result.is_none());
    assert_eq!(polls.get(), 2);
}

#[test]
fn workspace_sessions_observe_project_disk_edits() {
    let root = temp_root("project-session-reuse");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Clue.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\n",
    )
    .unwrap();
    let main_path = root.join("src/main.rid");
    let main_source = "mod util;\nfun main() { util::value(); }\n";
    fs::write(&main_path, main_source).unwrap();
    let util_path = root.join("src/util.rid");
    fs::write(&util_path, "pub fun value() {}\n").unwrap();
    let main_uri = lsp_types::Url::from_file_path(fs::canonicalize(&main_path).unwrap()).unwrap();
    let docs = HashMap::from([(
        main_uri,
        Document {
            text: main_source.into(),
            version: Some(1),
        },
    )]);
    let mut sessions = DiagnosticSessions::default();

    collect_workspace_diagnostics_with_sessions(&docs, CompileOptions::default(), &mut sessions);

    fs::write(&util_path, "pub fun value() { missing; }\n").unwrap();
    let published = collect_workspace_diagnostics_with_sessions(
        &docs,
        CompileOptions::default(),
        &mut sessions,
    );
    let util_uri = lsp_types::Url::from_file_path(fs::canonicalize(&util_path).unwrap()).unwrap();
    assert!(published.iter().any(|item| {
        item.uri == util_uri
            && item
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("unresolved name: `missing`"))
    }));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn semantic_tokens_place_local_declaration_and_use_on_identifier() {
    let source = "fun main() {\n  let mut foo_bar = 1; foo_bar;\n}";
    let tokens = semantic_token_positions(&semantic_tokens(source));
    let variables = tokens
        .iter()
        .filter(|token| token.token_type == TOKEN_VARIABLE)
        .collect::<Vec<_>>();

    assert_eq!(
        variables,
        vec![
            &SemanticTokenPosition {
                line: 1,
                start: 10,
                length: 7,
                token_type: TOKEN_VARIABLE,
                token_modifiers_bitset: MOD_DECLARATION | MOD_MUTABLE,
            },
            &SemanticTokenPosition {
                line: 1,
                start: 23,
                length: 7,
                token_type: TOKEN_VARIABLE,
                token_modifiers_bitset: MOD_MUTABLE,
            },
        ]
    );
}

#[test]
fn semantic_tokens_separate_parameters_from_immutable_locals() {
    let source = r#"unsafe extern "C" { fun putchar(c: i32) -> i32; }

fun print_digit(n: i32){
    putchar(n + 48);
    putchar(10);
}

fun main(){
    let t = fun(x) {
        x+1
    };
    let v = t(1);
    print_digit(v);
}"#;
    let symbols = semantic_token_positions(&semantic_tokens(source))
        .into_iter()
        .filter(|token| matches!(token.token_type, TOKEN_PARAMETER | TOKEN_VARIABLE))
        .collect::<Vec<_>>();

    assert_eq!(
        symbols,
        vec![
            SemanticTokenPosition {
                line: 0,
                start: 32,
                length: 1,
                token_type: TOKEN_PARAMETER,
                token_modifiers_bitset: MOD_DECLARATION,
            },
            SemanticTokenPosition {
                line: 2,
                start: 16,
                length: 1,
                token_type: TOKEN_PARAMETER,
                token_modifiers_bitset: MOD_DECLARATION,
            },
            SemanticTokenPosition {
                line: 3,
                start: 12,
                length: 1,
                token_type: TOKEN_PARAMETER,
                token_modifiers_bitset: 0,
            },
            SemanticTokenPosition {
                line: 8,
                start: 16,
                length: 1,
                token_type: TOKEN_PARAMETER,
                token_modifiers_bitset: MOD_DECLARATION,
            },
            SemanticTokenPosition {
                line: 9,
                start: 8,
                length: 1,
                token_type: TOKEN_PARAMETER,
                token_modifiers_bitset: 0,
            },
            // Immutable local closure `t` at its call site in `t(1)`.
            SemanticTokenPosition {
                line: 11,
                start: 12,
                length: 1,
                token_type: TOKEN_VARIABLE,
                token_modifiers_bitset: 0,
            },
            // Immutable local `v` at its use site in `print_digit(v)`.
            SemanticTokenPosition {
                line: 12,
                start: 16,
                length: 1,
                token_type: TOKEN_VARIABLE,
                token_modifiers_bitset: 0,
            },
        ]
    );
}

#[derive(Debug, PartialEq, Eq)]
struct SemanticTokenPosition {
    line: u32,
    start: u32,
    length: u32,
    token_type: u32,
    token_modifiers_bitset: u32,
}

fn semantic_token_positions(tokens: &SemanticTokens) -> Vec<SemanticTokenPosition> {
    let mut line = 0;
    let mut start = 0;
    tokens
        .data
        .iter()
        .map(|token| {
            line += token.delta_line;
            if token.delta_line == 0 {
                start += token.delta_start;
            } else {
                start = token.delta_start;
            }
            SemanticTokenPosition {
                line,
                start,
                length: token.length,
                token_type: token.token_type,
                token_modifiers_bitset: token.token_modifiers_bitset,
            }
        })
        .collect()
}
